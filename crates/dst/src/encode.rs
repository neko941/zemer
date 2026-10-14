use zemer_core::AudioError;

use crate::decode::decode_frame;

const MAX_CHANNELS: usize = 64;
const MAX_FRAME_BYTES_PER_CHANNEL: usize = 18_816;

/// Encode one channel-major, MSB-first DSD frame as a DST compressed frame.
///
/// This clean-room encoder uses a first-order predictor and one shared
/// probability table. It returns an error when prediction is not DST-valid or
/// does not reduce the frame size; callers can then write a mode-0 bypass frame.
pub fn encode_frame(
    dsd: &[u8],
    channels: usize,
    frame_bytes_per_channel: usize,
) -> Result<Vec<u8>, AudioError> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(AudioError::Unsupported("unsupported DST channel count"));
    }
    if !matches!(
        frame_bytes_per_channel,
        4_704 | 9_408 | MAX_FRAME_BYTES_PER_CHANNEL
    ) {
        return Err(AudioError::Unsupported("unsupported DST frame length"));
    }
    let sample_bits = frame_bytes_per_channel
        .checked_mul(8)
        .ok_or(AudioError::InvalidData("DST frame size overflow"))?;
    let expected_len = channels
        .checked_mul(frame_bytes_per_channel)
        .ok_or(AudioError::InvalidData("DST frame size overflow"))?;
    if dsd.len() != expected_len {
        return Err(AudioError::InvalidData(
            "DST input does not match the channel and frame sizes",
        ));
    }

    let mut best_model = (1_i16, usize::MAX, 0_usize);
    for coefficient in [1_i16, -1] {
        let mut previous = vec![false; channels];
        let mut errors = 0_usize;
        let mut coded_errors = 0_usize;
        for bit_index in 0..sample_bits {
            for (channel, previous_bit) in previous.iter_mut().enumerate() {
                let bit = dsd_bit(dsd, frame_bytes_per_channel, channel, bit_index);
                let predicted = *previous_bit ^ (coefficient < 0);
                let error = bit ^ predicted;
                errors += usize::from(error);
                if bit_index != 0 {
                    coded_errors += usize::from(error);
                }
                *previous_bit = bit;
            }
        }
        if errors < best_model.1 {
            best_model = (coefficient, errors, coded_errors);
        }
    }
    let (coefficient, errors, coded_errors) = best_model;
    if errors > channels * sample_bits / 2 {
        return Err(AudioError::Unsupported(
            "DST first-order predictor exceeds the permitted error count",
        ));
    }
    let coded_bits = channels * (sample_bits - 1);
    let probability = (coded_errors * 256 + coded_bits / 2)
        .checked_div(coded_bits)
        .map_or(1, |value| value.clamp(1, 128) as u8);

    let mut trace = Vec::new();
    let symbol_count = channels
        .checked_mul(sample_bits)
        .and_then(|count| count.checked_add(1))
        .ok_or(AudioError::InvalidData("DST symbol count overflow"))?;
    trace
        .try_reserve_exact(symbol_count)
        .map_err(|_| AudioError::InvalidData("DST encoder workspace is too large"))?;
    let dst_x_probability = probability_from_coefficient(coefficient);
    trace.push((dst_x_probability, false));
    let mut previous = vec![false; channels];
    for bit_index in 0..sample_bits {
        for (channel, previous_bit) in previous.iter_mut().enumerate() {
            let bit = dsd_bit(dsd, frame_bytes_per_channel, channel, bit_index);
            trace.push((
                if bit_index == 0 { 128 } else { probability },
                bit ^ *previous_bit ^ (coefficient < 0),
            ));
            *previous_bit = bit;
        }
    }

    let mut arithmetic = encode_arithmetic(&trace)?;
    let mut bits = BitWriter::default();
    bits.write(1, 1); // Processing_Mode.
    bits.write(1, 1); // Filter_And_Ptable_Segmentation.
    bits.write(1, 1); // Same_Segm_For_All_Channels.
    bits.write(1, 1); // End_Of_Channel_Segm.
    bits.write(1, 1); // Same_Mapping.
    bits.write(1, 1); // Same_Maps_For_All_Channels; the sole map entry is implicit zero.
    for _ in 0..channels {
        bits.write(1, 1); // Half_Probability for the order-1 warm-up bit.
    }
    bits.write(0, 7); // Prediction order = 1.
    bits.write(0, 1); // Store the coefficient directly.
    bits.write(u32::from(coefficient as u16 & 0x01ff), 9);
    bits.write(1, 6); // Probability table length = 2.
    bits.write(0, 1); // Store probability values directly.
    bits.write(u32::from(probability - 1), 7);
    bits.write(127, 7); // The order-1 predictor only uses Ptable entry zero.
    bits.append(&mut arithmetic);
    let frame = bits.bytes();

    if frame.len() >= dsd.len() || frame.len() > dsd.len() + 1 {
        return Err(AudioError::Unsupported(
            "DST compressed frame does not reduce the input size",
        ));
    }
    if decode_frame(&frame, channels, frame_bytes_per_channel)? != dsd {
        return Err(AudioError::InvalidData(
            "DST encoder output failed its decoder round trip",
        ));
    }
    Ok(frame)
}

fn dsd_bit(bytes: &[u8], frame_bytes_per_channel: usize, channel: usize, bit: usize) -> bool {
    let byte = bytes[channel * frame_bytes_per_channel + bit / 8];
    byte & (0x80 >> (bit % 8)) != 0
}

fn probability_from_coefficient(coefficient: i16) -> u8 {
    let low = coefficient as u16 & 0x7f;
    (low.reverse_bits().wrapping_shr(9) as u8) + 1
}

fn encode_arithmetic(symbols: &[(u8, bool)]) -> Result<BitWriter, AudioError> {
    let mut range = 4095_u16;
    let (mut low, mut high) = (0_u16, 4095_u16);
    let mut trace = Vec::new();
    trace
        .try_reserve_exact(symbols.len())
        .map_err(|_| AudioError::InvalidData("DST encoder workspace is too large"))?;

    for &(probability, symbol) in symbols {
        if probability == 0 || probability > 128 {
            return Err(AudioError::InvalidData("invalid DST encoder probability"));
        }
        let k = (range >> 8) | ((range >> 7) & 1);
        let q = k * u16::from(probability);
        let split = range - q;
        let boundary = split;
        if symbol {
            low = low.max(split) - split;
            high -= split;
            range = q;
        } else {
            high = high.min(split - 1);
            range = split;
        }
        if low > high {
            return Err(AudioError::Unsupported(
                "DST arithmetic interval is empty for the selected model",
            ));
        }

        let mut shift = 0_u8;
        while range < 2048 {
            range <<= 1;
            shift += 1;
        }
        let mask = (1_u16 << shift) - 1;
        low <<= shift;
        high = (high << shift) | mask;
        trace.push(ArithmeticStep {
            split: boundary,
            shift,
            symbol,
        });
    }

    let mut appended_bits = Vec::new();
    appended_bits
        .try_reserve_exact(trace.len())
        .map_err(|_| AudioError::InvalidData("DST encoder workspace is too large"))?;
    let mut code = low;
    for step in trace.iter().rev() {
        let mask = (1_u16 << step.shift) - 1;
        let appended = code & mask;
        code >>= step.shift;
        if step.symbol {
            code = code.checked_add(step.split).ok_or(AudioError::InvalidData(
                "DST arithmetic code reconstruction overflow",
            ))?;
        }
        appended_bits.push((appended, step.shift));
    }
    let mut output = BitWriter::default();
    output.write(0, 1); // The arithmetic decoder's required leading bit.
    output.write(u32::from(code), 12);
    for chunk in appended_bits.into_iter().rev() {
        output.push(chunk);
    }
    Ok(output)
}

struct ArithmeticStep {
    split: u16,
    shift: u8,
    symbol: bool,
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    pending: u8,
    used: u8,
}

impl BitWriter {
    fn write(&mut self, value: u32, count: usize) {
        for shift in (0..count).rev() {
            self.push_bit((value >> shift) & 1 != 0);
        }
    }

    fn push(&mut self, (value, count): (u16, u8)) {
        self.write(u32::from(value), usize::from(count));
    }

    fn push_bit(&mut self, bit: bool) {
        self.pending = (self.pending << 1) | u8::from(bit);
        self.used += 1;
        if self.used == 8 {
            self.bytes.push(self.pending);
            self.pending = 0;
            self.used = 0;
        }
    }

    fn append(&mut self, other: &mut Self) {
        for byte in other.bytes.drain(..) {
            self.write(u32::from(byte), 8);
        }
        if other.used != 0 {
            self.write(u32::from(other.pending), usize::from(other.used));
        }
    }

    fn bytes(mut self) -> Vec<u8> {
        if self.used != 0 {
            self.pending <<= 8 - self.used;
            self.bytes.push(self.pending);
        }
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::encode_frame;
    use crate::decode_frame;

    #[test]
    fn encodes_compressible_single_and_multichannel_frames() {
        for frame_len in [4_704, 9_408, 18_816] {
            for channels in [1, 2, 6] {
                let mut dsd = vec![0; channels * frame_len];
                for channel in 0..channels {
                    for byte in 0..frame_len {
                        dsd[channel * frame_len + byte] = if (byte / 8 + channel) % 2 == 0 {
                            0xff
                        } else {
                            0x00
                        };
                    }
                }
                let encoded = encode_frame(&dsd, channels, frame_len).unwrap();
                assert!(encoded.len() < dsd.len());
                assert_eq!(decode_frame(&encoded, channels, frame_len).unwrap(), dsd);
            }
        }
    }

    #[test]
    fn encodes_alternating_bits_with_the_negative_first_order_coefficient() {
        for frame_len in [4_704, 9_408, 18_816] {
            for channels in [1, 2, 6] {
                let mut dsd = Vec::with_capacity(channels * frame_len);
                for _ in 0..frame_len {
                    for channel in 0..channels {
                        dsd.push(if channel % 2 == 0 { 0x55 } else { 0xaa });
                    }
                }
                let encoded = encode_frame(&dsd, channels, frame_len).unwrap();
                assert!(encoded.len() < dsd.len());
                assert_eq!(decode_frame(&encoded, channels, frame_len).unwrap(), dsd);
            }
        }
    }

    #[test]
    fn encodes_and_decodes_the_64_channel_dst_profiles() {
        let channels = 64;
        for frame_bytes in [4_704, 9_408, 18_816] {
            let dsd = vec![0xff; channels * frame_bytes];
            let encoded = encode_frame(&dsd, channels, frame_bytes).unwrap();
            assert!(encoded.len() < dsd.len());
            assert_eq!(decode_frame(&encoded, channels, frame_bytes).unwrap(), dsd);
        }
    }
}
