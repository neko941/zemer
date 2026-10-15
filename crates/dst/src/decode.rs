use std::collections::VecDeque;

use zemer_core::AudioError;

use crate::bitstream::BitReader;

const MAX_CHANNELS: usize = 64;
const MAX_FILTERS: usize = 64;
const MAX_PTABLES: usize = 64;
const MAX_FILTER_SEGMENTS: usize = 4;
const MAX_PTABLE_SEGMENTS: usize = 8;
type Segmentations = (Vec<Vec<usize>>, Vec<Vec<usize>>);

/// Decode one complete MPEG-4 DST audio frame to channel-major DSD bytes.
///
/// `frame_bytes_per_channel` is 4,704, 9,408, or 18,816 at DSD64, DSD128, or
/// DSD256 respectively. Frame storage is bounded to the format's maximum of
/// 64 channels at DSD256.
pub fn decode_frame(
    frame: &[u8],
    channels: usize,
    frame_bytes_per_channel: usize,
) -> Result<Vec<u8>, AudioError> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(AudioError::Unsupported("unsupported DST channel count"));
    }
    if !matches!(frame_bytes_per_channel, 4_704 | 9_408 | 18_816) {
        return Err(AudioError::Unsupported("unsupported DST frame length"));
    }
    let output_len = channels
        .checked_mul(frame_bytes_per_channel)
        .ok_or(AudioError::InvalidData("DST frame output size overflow"))?;
    let maximum_frame_size = output_len
        .checked_add(1)
        .ok_or(AudioError::InvalidData("DST frame size overflow"))?;
    if frame.len() > maximum_frame_size {
        return Err(AudioError::InvalidFormat(
            "DST frame exceeds its maximum size",
        ));
    }
    let mut bits = BitReader::new(frame);
    match bits.read(1)? {
        0 => decode_uncompressed(&mut bits, channels, frame_bytes_per_channel, output_len),
        1 => decode_compressed(&mut bits, channels, frame_bytes_per_channel, output_len),
        _ => unreachable!(),
    }
}

fn decode_uncompressed(
    bits: &mut BitReader<'_>,
    channels: usize,
    frame_bytes_per_channel: usize,
    output_len: usize,
) -> Result<Vec<u8>, AudioError> {
    let _dst_x_bit = bits.read(1)?;
    if bits.read(6)? != 0 {
        return Err(AudioError::InvalidFormat("nonzero DST reserved bits"));
    }
    let mut output = new_frame_buffer(output_len)?;
    for byte_index in 0..frame_bytes_per_channel {
        for channel in 0..channels {
            output[channel * frame_bytes_per_channel + byte_index] = bits.read(8)? as u8;
        }
    }
    check_zero_padding(bits)?;
    Ok(output)
}

fn decode_compressed(
    bits: &mut BitReader<'_>,
    channels: usize,
    frame_bytes_per_channel: usize,
    output_len: usize,
) -> Result<Vec<u8>, AudioError> {
    let (filter_segments, ptable_segments) =
        parse_segmentations(bits, channels, frame_bytes_per_channel)?;
    let (filter_map, ptable_map) = parse_mappings(bits, &filter_segments, &ptable_segments)?;
    let mut half_probability = Vec::with_capacity(channels);
    for _ in 0..channels {
        half_probability.push(bits.read(1)? != 0);
    }
    let filter_count = filter_map
        .iter()
        .flatten()
        .copied()
        .max()
        .map_or(0, |value| value + 1);
    let ptable_count = ptable_map
        .iter()
        .flatten()
        .copied()
        .max()
        .map_or(0, |value| value + 1);
    if filter_count == 0
        || filter_count > MAX_FILTERS
        || ptable_count == 0
        || ptable_count > MAX_PTABLES
    {
        return Err(AudioError::InvalidFormat("invalid DST model count"));
    }
    let filters = read_filters(bits, filter_count)?;
    let ptables = read_ptables(bits, ptable_count)?;
    validate_ptable_warmup(&filter_segments, &ptable_segments, &filter_map, &filters)?;
    let mut output = new_frame_buffer(output_len)?;
    let mut arithmetic = ArithmeticDecoder::new(bits)?;
    let first_probability = dst_x_probability(filters[0].coefficients[0]);
    let _dst_x_bit = arithmetic.decode(first_probability)?;
    let mut histories = Vec::with_capacity(channels);
    for _ in 0..channels {
        histories.push(SourceHistory::new());
    }
    let mut error_count = 0_usize;
    let sample_bits = frame_bytes_per_channel * 8;
    for bit_index in 0..sample_bits {
        for channel in 0..channels {
            let filter_segment = segment_for_bit(&filter_segments[channel], bit_index);
            let ptable_segment = segment_for_bit(&ptable_segments[channel], bit_index);
            let filter = filter_map[channel][filter_segment];
            let ptable = ptable_map[channel][ptable_segment];
            let model = &filters[filter];
            let prediction = histories[channel].prediction(&model.coefficients);
            let predicted_bit = u8::from(prediction >= 0);
            let probability = if half_probability[channel]
                && bit_index < filters[filter_map[channel][0]].coefficients.len()
            {
                128
            } else {
                let index =
                    ((prediction.unsigned_abs() >> 3) as usize).min(ptables[ptable].len() - 1);
                ptables[ptable][index]
            };
            let error = arithmetic.decode(probability)? as u8;
            error_count += usize::from(error != 0);
            let dsd_bit = error ^ predicted_bit;
            histories[channel].push(if dsd_bit != 0 { 1 } else { -1 });
            if dsd_bit != 0 {
                output[channel * frame_bytes_per_channel + bit_index / 8] |=
                    0x80 >> (bit_index % 8);
            }
        }
    }
    if error_count > channels * sample_bits / 2 {
        return Err(AudioError::InvalidFormat(
            "DST frame exceeds the permitted prediction-error count",
        ));
    }
    arithmetic.finish()?;
    Ok(output)
}

fn new_frame_buffer(length: usize) -> Result<Vec<u8>, AudioError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|_| AudioError::InvalidData("DST frame output is too large"))?;
    output.resize(length, 0);
    Ok(output)
}

fn check_zero_padding(bits: &mut BitReader<'_>) -> Result<(), AudioError> {
    if bits.remaining() >= 8 {
        return Err(AudioError::InvalidFormat("trailing bytes in DST frame"));
    }
    while bits.remaining() != 0 {
        if bits.read(1)? != 0 {
            return Err(AudioError::InvalidFormat("nonzero DST padding bits"));
        }
    }
    Ok(())
}

fn parse_segmentations(
    bits: &mut BitReader<'_>,
    channels: usize,
    frame_len: usize,
) -> Result<Segmentations, AudioError> {
    if bits.read(1)? != 0 {
        let shared = read_segment_allocation(bits, channels, frame_len, 128, MAX_FILTER_SEGMENTS)?;
        return Ok((shared.clone(), shared));
    }
    let filters = read_segment_allocation(bits, channels, frame_len, 128, MAX_FILTER_SEGMENTS)?;
    let ptables = read_segment_allocation(bits, channels, frame_len, 4, MAX_PTABLE_SEGMENTS)?;
    Ok((filters, ptables))
}

fn read_segment_allocation(
    bits: &mut BitReader<'_>,
    channels: usize,
    frame_len: usize,
    minimum: usize,
    maximum_segments: usize,
) -> Result<Vec<Vec<usize>>, AudioError> {
    let same_for_all = bits.read(1)? != 0;
    let mut resolution = None;
    let first = read_channel_segments(bits, frame_len, minimum, maximum_segments, &mut resolution)?;
    let mut result = Vec::with_capacity(channels);
    result.push(first.clone());
    if same_for_all {
        result.resize_with(channels, || first.clone());
    } else {
        for _ in 1..channels {
            result.push(read_channel_segments(
                bits,
                frame_len,
                minimum,
                maximum_segments,
                &mut resolution,
            )?);
        }
    }
    Ok(result)
}

fn read_channel_segments(
    bits: &mut BitReader<'_>,
    frame_len: usize,
    minimum: usize,
    maximum_segments: usize,
    resolution: &mut Option<usize>,
) -> Result<Vec<usize>, AudioError> {
    let mut lengths = Vec::new();
    let mut start = 0;
    loop {
        if bits.read(1)? != 0 {
            if start >= frame_len {
                return Err(AudioError::InvalidFormat("invalid DST segment boundary"));
            }
            lengths.push(frame_len - start);
            break;
        }
        if lengths.len() + 1 >= maximum_segments {
            return Err(AudioError::InvalidFormat("too many DST segments"));
        }
        let remaining = frame_len
            .checked_sub(start)
            .filter(|&value| value > minimum)
            .ok_or(AudioError::InvalidFormat("invalid DST segment boundary"))?;
        let resolution_value = match resolution {
            Some(value) => *value,
            None => {
                let value = bits.read(bit_width(frame_len - minimum))? as usize;
                if value == 0 || value > frame_len - minimum {
                    return Err(AudioError::InvalidFormat("invalid DST segment resolution"));
                }
                *resolution = Some(value);
                value
            }
        };
        let range = (remaining - minimum) / resolution_value;
        if range == 0 {
            return Err(AudioError::InvalidFormat(
                "invalid DST scaled segment range",
            ));
        }
        let width = bit_width(range);
        let scaled = bits.read(width)? as usize;
        if scaled == 0 || scaled > range {
            return Err(AudioError::InvalidFormat(
                "invalid DST scaled segment length",
            ));
        }
        let length = resolution_value
            .checked_mul(scaled)
            .filter(|&length| length >= minimum && length <= remaining - minimum)
            .ok_or(AudioError::InvalidFormat("invalid DST segment length"))?;
        lengths.push(length);
        start = start
            .checked_add(length)
            .ok_or(AudioError::InvalidFormat("DST segment position overflow"))?;
    }
    Ok(lengths)
}

fn bit_width(value: usize) -> usize {
    usize::BITS as usize - value.leading_zeros() as usize
}

fn parse_mappings(
    bits: &mut BitReader<'_>,
    filter_segments: &[Vec<usize>],
    ptable_segments: &[Vec<usize>],
) -> Result<Segmentations, AudioError> {
    let same_mapping = bits.read(1)? != 0;
    if same_mapping {
        if filter_segments
            .iter()
            .zip(ptable_segments)
            .any(|(filters, ptables)| filters.len() != ptables.len())
        {
            return Err(AudioError::InvalidFormat(
                "DST shared mapping requires shared segmentation",
            ));
        }
        let mapping = read_maps(bits, filter_segments)?;
        Ok((mapping.clone(), mapping))
    } else {
        Ok((
            read_maps(bits, filter_segments)?,
            read_maps(bits, ptable_segments)?,
        ))
    }
}

fn read_maps(
    bits: &mut BitReader<'_>,
    segments: &[Vec<usize>],
) -> Result<Vec<Vec<usize>>, AudioError> {
    let channels = segments.len();
    let maximum_elements = 2 * channels;
    let same_for_all = bits.read(1)? != 0;
    if same_for_all && segments.iter().any(|row| row.len() != segments[0].len()) {
        return Err(AudioError::InvalidFormat(
            "DST shared channel mapping has different segment counts",
        ));
    }
    let mut element_count = 0;
    let mut maps = Vec::with_capacity(channels);
    let first = read_channel_map(
        bits,
        segments[0].len(),
        maximum_elements,
        &mut element_count,
    )?;
    maps.push(first.clone());
    if same_for_all {
        maps.resize_with(channels, || first.clone());
    } else {
        for row in segments.iter().skip(1) {
            maps.push(read_channel_map(
                bits,
                row.len(),
                maximum_elements,
                &mut element_count,
            )?);
        }
    }
    if element_count == 0 {
        return Err(AudioError::InvalidFormat("empty DST model mapping"));
    }
    Ok(maps)
}

fn read_channel_map(
    bits: &mut BitReader<'_>,
    segment_count: usize,
    maximum_elements: usize,
    element_count: &mut usize,
) -> Result<Vec<usize>, AudioError> {
    let mut mapping = Vec::with_capacity(segment_count);
    for _ in 0..segment_count {
        let width = bit_width(*element_count);
        let element = if width == 0 {
            0
        } else {
            bits.read(width)? as usize
        };
        if element > *element_count
            || (element == *element_count && *element_count >= maximum_elements)
        {
            return Err(AudioError::InvalidFormat("invalid DST mapping element"));
        }
        if element == *element_count {
            *element_count += 1;
        }
        mapping.push(element);
    }
    Ok(mapping)
}

fn read_filters(bits: &mut BitReader<'_>, count: usize) -> Result<Vec<Filter>, AudioError> {
    let mut filters = Vec::with_capacity(count);
    for _ in 0..count {
        let order = bits.read(7)? as usize + 1;
        let mut coefficients = Vec::with_capacity(order);
        if bits.read(1)? == 0 {
            for _ in 0..order {
                coefficients.push(bits.read_signed(9)? as i16);
            }
        } else {
            let method = bits.read(2)? as usize;
            let prediction = match method {
                0 => &[-1, 0, 0][..1],
                1 => &[-2, 1, 0][..2],
                2 => &[-9, -5, 6][..3],
                _ => return Err(AudioError::InvalidFormat("reserved DST coefficient method")),
            };
            if prediction.len() >= order {
                return Err(AudioError::InvalidFormat(
                    "invalid DST coefficient prediction order",
                ));
            }
            for _ in prediction {
                coefficients.push(bits.read_signed(9)? as i16);
            }
            let rice = bits.read(3)? as usize;
            if rice > 6 {
                return Err(AudioError::InvalidFormat(
                    "invalid DST coefficient Rice parameter",
                ));
            }
            for _ in prediction.len()..order {
                let delta = read_rice_delta(bits, rice)?;
                let predicted = filter_prediction(method, &coefficients);
                let value = delta + predicted;
                if !(-256..=255).contains(&value) {
                    return Err(AudioError::InvalidFormat("DST coefficient is out of range"));
                }
                coefficients.push(value as i16);
            }
        }
        filters.push(Filter { coefficients });
    }
    Ok(filters)
}

fn read_rice_delta(bits: &mut BitReader<'_>, parameter: usize) -> Result<i32, AudioError> {
    let mut run = 0_u32;
    loop {
        if bits.read(1)? != 0 {
            break;
        }
        run = run
            .checked_add(1)
            .filter(|&value| value <= 511)
            .ok_or(AudioError::InvalidFormat("DST Rice code is too long"))?;
    }
    let low = bits.read(parameter)?;
    let magnitude = (run << parameter) | low;
    if magnitude == 0 {
        Ok(0)
    } else if bits.read(1)? == 0 {
        Ok(magnitude as i32)
    } else {
        Ok(-(magnitude as i32))
    }
}

fn filter_prediction(method: usize, coefficients: &[i16]) -> i32 {
    let last = coefficients.len();
    match method {
        0 => i32::from(coefficients[last - 1]),
        1 => 2 * i32::from(coefficients[last - 1]) - i32::from(coefficients[last - 2]),
        2 => {
            let delta8 = -9 * i32::from(coefficients[last - 1])
                - 5 * i32::from(coefficients[last - 2])
                + 6 * i32::from(coefficients[last - 3]);
            if delta8 >= 0 {
                -((delta8 + 4) / 8)
            } else {
                (-delta8 + 3) / 8
            }
        }
        _ => unreachable!(),
    }
}

fn ptable_prediction(method: usize, values: &[i32]) -> i32 {
    let last = values.len();
    match method {
        0 => values[last - 1],
        1 => 2 * values[last - 1] - values[last - 2],
        2 => 3 * values[last - 1] - 3 * values[last - 2] + values[last - 3],
        _ => unreachable!(),
    }
}

fn read_ptables(bits: &mut BitReader<'_>, count: usize) -> Result<Vec<Vec<u8>>, AudioError> {
    let mut tables = Vec::with_capacity(count);
    for _ in 0..count {
        let length = bits.read(6)? as usize + 1;
        let mut values = Vec::with_capacity(length);
        if length == 1 {
            values.push(128);
        } else if bits.read(1)? == 0 {
            for _ in 0..length {
                values.push(bits.read(7)? as i32 + 1);
            }
        } else {
            let method = bits.read(2)? as usize;
            let prediction = match method {
                0 => &[-1, 0, 0][..1],
                1 => &[-2, 1, 0][..2],
                2 => &[-3, 3, -1][..3],
                _ => return Err(AudioError::InvalidFormat("reserved DST probability method")),
            };
            if prediction.len() >= length {
                return Err(AudioError::InvalidFormat(
                    "invalid DST probability prediction order",
                ));
            }
            for _ in prediction {
                values.push(bits.read(7)? as i32 + 1);
            }
            let rice = bits.read(3)? as usize;
            if rice > 4 {
                return Err(AudioError::InvalidFormat(
                    "invalid DST probability Rice parameter",
                ));
            }
            for _ in prediction.len()..length {
                let delta = read_rice_delta(bits, rice)?;
                let predicted = ptable_prediction(method, &values);
                values.push(delta + predicted);
            }
        }
        if values.iter().any(|value| !(1..=128).contains(value)) {
            return Err(AudioError::InvalidFormat("DST probability is out of range"));
        }
        tables.push(values.into_iter().map(|value| value as u8).collect());
    }
    Ok(tables)
}

fn validate_ptable_warmup(
    filter_segments: &[Vec<usize>],
    ptable_segments: &[Vec<usize>],
    filter_map: &[Vec<usize>],
    filters: &[Filter],
) -> Result<(), AudioError> {
    for channel in 0..filter_segments.len() {
        let first_filter = filter_map[channel][0];
        let warmup_bytes = filters[first_filter].coefficients.len().div_ceil(8);
        if ptable_segments[channel][0] < warmup_bytes
            || filter_segments[channel].iter().sum::<usize>()
                != ptable_segments[channel].iter().sum::<usize>()
        {
            return Err(AudioError::InvalidFormat(
                "invalid DST probability warm-up segment",
            ));
        }
    }
    Ok(())
}

fn segment_for_bit(lengths: &[usize], bit_index: usize) -> usize {
    let byte_index = bit_index / 8;
    let mut end = 0;
    for (segment, &length) in lengths.iter().enumerate() {
        end += length;
        if byte_index < end {
            return segment;
        }
    }
    lengths.len() - 1
}

fn dst_x_probability(coefficient: i16) -> u8 {
    let low = coefficient as u16 & 0x7f;
    low.reverse_bits().wrapping_shr(9) as u8 + 1
}

struct Filter {
    coefficients: Vec<i16>,
}

struct SourceHistory {
    values: VecDeque<i8>,
}

impl SourceHistory {
    fn new() -> Self {
        let mut values = VecDeque::with_capacity(128);
        for index in 0..128 {
            values.push_back(if index % 2 == 0 { 1 } else { -1 });
        }
        Self { values }
    }

    fn prediction(&self, coefficients: &[i16]) -> i32 {
        coefficients
            .iter()
            .zip(self.values.iter().rev())
            .map(|(&coefficient, &sample)| i32::from(coefficient) * i32::from(sample))
            .sum()
    }

    fn push(&mut self, sample: i8) {
        self.values.pop_front();
        self.values.push_back(sample);
    }
}

struct ArithmeticDecoder<'a, 'b> {
    bits: &'a mut BitReader<'b>,
    range: u16,
    code: u16,
}

impl<'a, 'b> ArithmeticDecoder<'a, 'b> {
    fn new(bits: &'a mut BitReader<'b>) -> Result<Self, AudioError> {
        let first_bit = bits.read_arithmetic_bit();
        if first_bit != 0 {
            return Err(AudioError::InvalidFormat(
                "DST arithmetic data must start with zero",
            ));
        }
        let mut code = 0_u16;
        for _ in 0..12 {
            code = (code << 1) | bits.read_arithmetic_bit();
        }
        Ok(Self {
            bits,
            range: 4095,
            code,
        })
    }

    fn decode(&mut self, probability: u8) -> Result<bool, AudioError> {
        if probability == 0 || probability > 128 {
            return Err(AudioError::InvalidFormat(
                "invalid DST arithmetic probability",
            ));
        }
        let k = (self.range >> 8) | ((self.range >> 7) & 1);
        let q = k * u16::from(probability);
        let split = self.range - q;
        let output = self.code >= split;
        if output {
            self.code -= split;
            self.range = q;
        } else {
            self.range = split;
        }
        while self.range < 0x800 {
            self.range <<= 1;
            self.code = ((self.code << 1) | self.bits.read_arithmetic_bit()) & 0x0fff;
        }
        Ok(output)
    }

    fn finish(self) -> Result<(), AudioError> {
        check_zero_padding(self.bits)
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_frame, filter_prediction, ptable_prediction};

    #[derive(Default)]
    struct BitWriter {
        bits: Vec<bool>,
    }

    impl BitWriter {
        fn write(&mut self, value: u32, width: usize) {
            for shift in (0..width).rev() {
                self.bits.push((value >> shift) & 1 != 0);
            }
        }

        fn bytes(mut self) -> Vec<u8> {
            while self.bits.len() % 8 != 0 {
                self.bits.push(false);
            }
            self.bits
                .chunks_exact(8)
                .map(|chunk| {
                    chunk
                        .iter()
                        .fold(0, |value, bit| (value << 1) | u8::from(*bit))
                })
                .collect()
        }
    }

    #[test]
    fn decodes_uncompressed_multichannel_frame_and_deinterleaves_bytes() {
        let channels = 2;
        let frame_len = 4_704;
        let mut frame = vec![0; 1 + frame_len * channels];
        frame[1..].chunks_exact_mut(2).for_each(|pair| {
            pair[0] = 0x12;
            pair[1] = 0x34;
        });
        let output = decode_frame(&frame, channels, frame_len).unwrap();
        assert!(output[..frame_len].iter().all(|&byte| byte == 0x12));
        assert!(output[frame_len..].iter().all(|&byte| byte == 0x34));
    }

    #[test]
    fn rejects_truncated_reserved_and_trailing_uncompressed_frame_data() {
        assert!(decode_frame(&[0], 1, 4_704).is_err());
        let mut reserved = vec![0; 1 + 4_704];
        reserved[0] = 1;
        assert!(decode_frame(&reserved, 1, 4_704).is_err());
        let trailing = vec![0; 1 + 4_704 + 1];
        assert!(decode_frame(&trailing, 1, 4_704).is_err());
    }

    #[test]
    fn decodes_compressed_single_channel_constant_prediction_frame() {
        let mut bits = BitWriter::default();
        bits.write(1, 1); // Processing_Mode.
        bits.write(1, 1); // Same_Segmentation.
        bits.write(1, 1); // Same_Segm_For_All_Channels.
        bits.write(1, 1); // End_Of_Channel_Segm.
        bits.write(1, 1); // Same_Mapping.
        bits.write(1, 1); // Same_Maps_For_All_Channels; Element[0][0] is implicit 0.
        bits.write(0, 1); // Half_Probability.
        bits.write(0, 7); // Prediction order = 1.
        bits.write(0, 1); // Direct coefficient.
        bits.write(0, 9); // Coef[0][0] = 0, DST-X probability = 1/256.
        bits.write(0, 6); // One-entry Ptable, implicitly P=128.
        bits.write(0, 8); // Arithmetic code starts at zero; missing tail bits are implicit zeroes.
        let decoded = decode_frame(&bits.bytes(), 1, 4_704).unwrap();
        assert!(decoded.iter().all(|&byte| byte == 0xff));
    }

    #[test]
    fn compressed_frame_mutations_and_truncations_never_panic() {
        let frame_len = 4_704;
        let mut dsd = vec![0; frame_len];
        for bit in (15..frame_len * 8).step_by(16) {
            dsd[bit / 8] |= 0x80 >> (bit % 8);
        }
        let frame = crate::encode::encode_frame(&dsd, 1, frame_len).unwrap();
        assert_eq!(frame[0] >> 7, 1, "test input must use compressed mode");
        assert!(frame.len() >= 128, "test frame must exercise deep mutations");

        for sample in 0..128 {
            let byte_index = sample * frame.len() / 128;
            let mut mutated = frame.clone();
            let bit = (byte_index * 5 + 3) % 8;
            mutated[byte_index] ^= 1 << bit;
            assert!(
                std::panic::catch_unwind(|| decode_frame(&mutated, 1, frame_len)).is_ok(),
                "decoder panicked after mutating byte {byte_index}"
            );
        }

        for removed in 1..=frame.len().min(32) {
            let truncated = &frame[..frame.len() - removed];
            assert!(
                std::panic::catch_unwind(|| decode_frame(truncated, 1, frame_len)).is_ok(),
                "decoder panicked after truncating {removed} bytes"
            );
        }
    }

    #[test]
    fn decodes_segmented_constant_prediction_frames_at_all_rates() {
        for (frame_len, resolution) in [(4_704, 128), (9_408, 256), (18_816, 512)] {
            let mut bits = BitWriter::default();
            bits.write(1, 1); // Processing_Mode.
            bits.write(1, 1); // Same_Segmentation.
            bits.write(1, 1); // Same_Segm_For_All_Channels.
            bits.write(0, 1); // Another segment follows.
            bits.write(resolution, super::bit_width(frame_len - 128));
            let range = (frame_len - 128) / resolution as usize;
            bits.write(1, super::bit_width(range)); // First segment is one Resolution long.
            bits.write(1, 1); // End_Of_Channel_Segm.
            bits.write(1, 1); // Same_Mapping.
            bits.write(1, 1); // Same_Maps_For_All_Channels.
            bits.write(0, 1); // The second segment reuses element 0.
            bits.write(0, 1); // Half_Probability.
            bits.write(0, 7); // Prediction order = 1.
            bits.write(0, 1); // Direct coefficient.
            bits.write(0, 9); // Coef[0][0] = 0.
            bits.write(0, 6); // One-entry Ptable, implicitly P=128.
            bits.write(0, 8); // Arithmetic code starts at zero; tail bits are implicit zeroes.
            let decoded = decode_frame(&bits.bytes(), 1, frame_len).unwrap();
            assert!(decoded.iter().all(|&byte| byte == 0xff));
        }
    }

    #[test]
    fn predicts_filter_and_probability_values_with_specified_signs() {
        assert_eq!(filter_prediction(0, &[7]), 7);
        assert_eq!(filter_prediction(1, &[3, 5]), 7);
        assert_eq!(filter_prediction(2, &[1, 4, 8]), 11);
        assert_eq!(ptable_prediction(0, &[7]), 7);
        assert_eq!(ptable_prediction(1, &[3, 5]), 7);
        assert_eq!(ptable_prediction(2, &[1, 3, 5]), 7);
    }
}
