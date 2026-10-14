use std::sync::OnceLock;

use zemer_core::AudioError;

use crate::{VorbisCodebook, VorbisFloor1, bitstream::BitReader};

const FLOOR1_RANGES: [u32; 4] = [256, 128, 86, 64];
const MAX_SPECTRUM_BINS: usize = 4096;

pub(crate) enum FloorResult {
    Unused,
    Truncated,
    Curve(Vec<f32>),
}

/// Decodes one floor-0 channel curve from the packet and setup codebooks.
pub(crate) fn decode_floor0(
    floor: &crate::VorbisFloor0,
    codebooks: &[VorbisCodebook],
    bits: &mut BitReader<'_>,
    spectrum_bins: usize,
) -> Result<FloorResult, AudioError> {
    if spectrum_bins > MAX_SPECTRUM_BINS {
        return Err(AudioError::InvalidData("Vorbis spectrum exceeds 4096 bins"));
    }
    if floor.order == 0 || floor.rate == 0 || floor.bark_map_size == 0 || floor.books.is_empty() {
        return Err(AudioError::InvalidData("invalid Vorbis floor-0 setup"));
    }
    match decode_floor0_inner(floor, codebooks, bits, spectrum_bins) {
        Err(AudioError::InvalidFormat("truncated Vorbis packet")) => Ok(FloorResult::Truncated),
        result => result,
    }
}

fn decode_floor0_inner(
    floor: &crate::VorbisFloor0,
    codebooks: &[VorbisCodebook],
    bits: &mut BitReader<'_>,
    spectrum_bins: usize,
) -> Result<FloorResult, AudioError> {
    let amplitude = bits.read_bits(u32::from(floor.amplitude_bits))?;
    if amplitude == 0 {
        return Ok(FloorResult::Unused);
    }

    let book_bits = integer_log(floor.books.len() as u32);
    let book_number = bits.read_bits(book_bits)? as usize;
    let book_index = *floor
        .books
        .get(book_number)
        .ok_or(AudioError::InvalidFormat(
            "invalid Vorbis floor-0 book number",
        ))?;
    let book = codebooks
        .get(usize::from(book_index))
        .ok_or(AudioError::InvalidFormat("invalid Vorbis floor-0 codebook"))?;
    let mut coefficients = Vec::new();
    coefficients
        .try_reserve_exact(usize::from(floor.order))
        .map_err(|_| AudioError::InvalidData("Vorbis floor-0 allocation failed"))?;
    let mut last = 0.0_f64;
    while coefficients.len() < usize::from(floor.order) {
        let entry = book.huffman.decode_entry(bits)?;
        let count =
            (usize::from(floor.order) - coefficients.len()).min(usize::from(book.dimensions));
        let mut vector = Vec::new();
        vector
            .try_reserve_exact(count)
            .map_err(|_| AudioError::InvalidData("Vorbis floor-0 allocation failed"))?;
        vector.resize(count, 0.0);
        book.lookup_vector_prefix(entry, &mut vector)?;
        for value in &mut vector {
            *value += last;
        }
        if let Some(&value) = vector.last() {
            last = value;
        }
        coefficients.extend(vector);
    }

    synthesize_floor0(floor, amplitude, &coefficients, spectrum_bins).map(FloorResult::Curve)
}

fn synthesize_floor0(
    floor: &crate::VorbisFloor0,
    amplitude: u64,
    coefficients: &[f64],
    spectrum_bins: usize,
) -> Result<Vec<f32>, AudioError> {
    if coefficients.len() < usize::from(floor.order) {
        return Err(AudioError::InvalidData(
            "insufficient Vorbis floor-0 coefficients",
        ));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(spectrum_bins)
        .map_err(|_| AudioError::InvalidData("Vorbis floor allocation failed"))?;

    let rate = f64::from(floor.rate);
    let bark_size = f64::from(floor.bark_map_size);
    let bark_nyquist = bark(rate / 2.0);
    if bark_nyquist <= 0.0 || !bark_nyquist.is_finite() {
        return Err(AudioError::InvalidData("invalid Vorbis floor-0 Bark map"));
    }
    let mut previous_map = None;
    let mut floor_value = 0.0_f64;
    for index in 0..spectrum_bins {
        let bark_index = (bark(rate * index as f64 / (2.0 * spectrum_bins as f64)) * bark_size
            / bark_nyquist)
            .floor()
            .clamp(0.0, bark_size - 1.0) as u16;
        if previous_map != Some(bark_index) {
            floor_value = floor0_value(floor, amplitude, coefficients, bark_index)?;
            previous_map = Some(bark_index);
        }
        let value = floor_value as f32;
        if !value.is_finite() {
            return Err(AudioError::InvalidData(
                "non-finite Vorbis floor-0 curve value",
            ));
        }
        output.push(value);
    }
    Ok(output)
}

fn floor0_value(
    floor: &crate::VorbisFloor0,
    amplitude: u64,
    coefficients: &[f64],
    bark_index: u16,
) -> Result<f64, AudioError> {
    let omega = std::f64::consts::PI * f64::from(bark_index) / f64::from(floor.bark_map_size);
    let cos_omega = omega.cos();
    let order = usize::from(floor.order);
    let mut p;
    let mut q;
    if order % 2 == 1 {
        p = 1.0 - cos_omega * cos_omega;
        for index in (1..order - 1).step_by(2) {
            p *= 4.0 * (coefficients[index].cos() - cos_omega).powi(2);
        }
        q = 0.25;
        for index in (0..order).step_by(2) {
            q *= 4.0 * (coefficients[index].cos() - cos_omega).powi(2);
        }
    } else {
        p = (1.0 - cos_omega) / 2.0;
        q = (1.0 + cos_omega) / 2.0;
        for index in (1..order).step_by(2) {
            p *= 4.0 * (coefficients[index].cos() - cos_omega).powi(2);
        }
        for index in (0..order).step_by(2) {
            q *= 4.0 * (coefficients[index].cos() - cos_omega).powi(2);
        }
    }
    let denominator = (p + q).sqrt();
    if denominator == 0.0 || !denominator.is_finite() {
        return Err(AudioError::InvalidData("invalid Vorbis floor-0 LSP curve"));
    }
    let amplitude_scale = amplitude as f64 * f64::from(floor.amplitude_offset)
        / (((1_u64 << floor.amplitude_bits) - 1) as f64 * denominator);
    let value = (0.11512925 * (amplitude_scale - f64::from(floor.amplitude_offset))).exp();
    if !value.is_finite() {
        return Err(AudioError::InvalidData(
            "invalid Vorbis floor-0 curve value",
        ));
    }
    Ok(value)
}

fn bark(frequency: f64) -> f64 {
    13.1 * (0.00074 * frequency).atan()
        + 2.24 * (0.0000000185 * frequency * frequency).atan()
        + 0.0001 * frequency
}

/// Decodes one floor-1 channel curve. A missing curve marks a channel as silent.
pub(crate) fn decode_floor1(
    floor: &VorbisFloor1,
    codebooks: &[VorbisCodebook],
    bits: &mut BitReader<'_>,
    spectrum_bins: usize,
) -> Result<FloorResult, AudioError> {
    if spectrum_bins > MAX_SPECTRUM_BINS {
        return Err(AudioError::InvalidData("Vorbis spectrum exceeds 4096 bins"));
    }
    if floor.x_list.len() < 2 || floor.x_list.len() > 65 {
        return Err(AudioError::InvalidData(
            "invalid Vorbis floor-1 point count",
        ));
    }
    match decode_floor1_inner(floor, codebooks, bits, spectrum_bins) {
        Err(AudioError::InvalidFormat("truncated Vorbis packet")) => Ok(FloorResult::Truncated),
        result => result,
    }
}

fn decode_floor1_inner(
    floor: &VorbisFloor1,
    codebooks: &[VorbisCodebook],
    bits: &mut BitReader<'_>,
    spectrum_bins: usize,
) -> Result<FloorResult, AudioError> {
    if !bits.read_bit()? {
        return Ok(FloorResult::Unused);
    }

    let range = *FLOOR1_RANGES
        .get(usize::from(floor.multiplier.saturating_sub(1)))
        .ok_or(AudioError::InvalidData("invalid Vorbis floor-1 multiplier"))?;
    let value_bits = integer_log(range - 1);
    let point_count = floor.x_list.len();
    let mut value_storage = [0_u32; 65];
    let values = &mut value_storage[..point_count];
    values[0] = bits.read_bits(value_bits)? as u32;
    values[1] = bits.read_bits(value_bits)? as u32;

    let mut offset = 2;
    for &class_number in &floor.partition_classes {
        let class = floor
            .classes
            .get(usize::from(class_number))
            .ok_or(AudioError::InvalidData("invalid Vorbis floor-1 class"))?;
        let subclass_bits = u32::from(class.subclasses);
        let subclass_mask = (1_u32 << subclass_bits).wrapping_sub(1);
        let mut subclass_value = match class.masterbook {
            Some(book) if subclass_bits != 0 => codebooks
                .get(usize::from(book))
                .ok_or(AudioError::InvalidData("invalid Vorbis floor-1 masterbook"))?
                .huffman
                .decode_entry(bits)?,
            _ => 0,
        };
        for _ in 0..class.dimensions {
            let book_index = class
                .subclass_books
                .get((subclass_value & subclass_mask) as usize)
                .ok_or(AudioError::InvalidData(
                    "invalid Vorbis floor-1 subclass book",
                ))?;
            if let Some(book) = book_index {
                values[offset] = codebooks
                    .get(usize::from(*book))
                    .ok_or(AudioError::InvalidData("invalid Vorbis floor-1 codebook"))?
                    .huffman
                    .decode_entry(bits)?;
            }
            offset += 1;
            subclass_value >>= subclass_bits;
        }
    }

    let mut active_storage = [false; 65];
    let active = &mut active_storage[..point_count];
    active[0] = true;
    active[1] = true;
    for index in 2..values.len() {
        let low = low_neighbor(&floor.x_list, index)?;
        let high = high_neighbor(&floor.x_list, index)?;
        let predicted = render_point(
            floor.x_list[low],
            values[low],
            floor.x_list[high],
            values[high],
            floor.x_list[index],
        );
        let high_room = range - predicted;
        let low_room = predicted;
        let room = high_room.min(low_room) * 2;
        let value = values[index];
        if value != 0 {
            active[low] = true;
            active[high] = true;
            active[index] = true;
            let final_value = if value >= room {
                if high_room > low_room {
                    i64::from(value - low_room + predicted)
                } else {
                    i64::from(predicted) - i64::from(value) + i64::from(high_room) - 1
                }
            } else if value & 1 != 0 {
                i64::from(predicted) - i64::from(value.div_ceil(2))
            } else {
                i64::from(predicted) + i64::from(value / 2)
            };
            values[index] = final_value.clamp(0, i64::from(range - 1)) as u32;
        } else {
            active[index] = false;
            values[index] = predicted;
        }
    }
    for value in values.iter_mut() {
        *value = (*value).min(range - 1);
    }

    let mut order_storage: [usize; 65] = std::array::from_fn(|index| index);
    let order = &mut order_storage[..point_count];
    order.sort_unstable_by_key(|&index| floor.x_list[index]);

    let mut curve = Vec::new();
    curve
        .try_reserve_exact(spectrum_bins)
        .map_err(|_| AudioError::InvalidData("Vorbis floor allocation failed"))?;
    curve.resize(spectrum_bins, 0.0_f32);
    let mut left_x = 0_u32;
    let mut left_y = values[0] * u32::from(floor.multiplier);
    let mut right_x = 0_u32;
    let mut right_y = 0_u32;
    for &index in order.iter().skip(1) {
        if active[index] {
            right_x = u32::from(floor.x_list[index]);
            right_y = values[index] * u32::from(floor.multiplier);
            render_line(left_x, left_y, right_x, right_y, &mut curve)?;
            left_x = right_x;
            left_y = right_y;
        }
    }
    if right_x < spectrum_bins as u32 {
        curve[right_x as usize..].fill(inverse_db(right_y)?);
    }
    Ok(FloorResult::Curve(curve))
}

fn low_neighbor(x_list: &[u16], index: usize) -> Result<usize, AudioError> {
    (0..index)
        .filter(|&candidate| x_list[candidate] < x_list[index])
        .max_by_key(|&candidate| x_list[candidate])
        .ok_or(AudioError::InvalidData(
            "Vorbis floor-1 low neighbor is missing",
        ))
}

fn high_neighbor(x_list: &[u16], index: usize) -> Result<usize, AudioError> {
    (0..index)
        .filter(|&candidate| x_list[candidate] > x_list[index])
        .min_by_key(|&candidate| x_list[candidate])
        .ok_or(AudioError::InvalidData(
            "Vorbis floor-1 high neighbor is missing",
        ))
}

pub(crate) fn render_point(x0: u16, y0: u32, x1: u16, y1: u32, x: u16) -> u32 {
    let (dy, adx) = (i64::from(y1) - i64::from(y0), i64::from(x1) - i64::from(x0));
    let offset = (dy.unsigned_abs() as i64 * (i64::from(x) - i64::from(x0))) / adx;
    if dy < 0 {
        (i64::from(y0) - offset) as u32
    } else {
        (i64::from(y0) + offset) as u32
    }
}

fn render_line(x0: u32, y0: u32, x1: u32, y1: u32, output: &mut [f32]) -> Result<(), AudioError> {
    if x1 <= x0 {
        return Err(AudioError::InvalidData("invalid Vorbis floor-1 line"));
    }
    let end = x1.min(output.len() as u32);
    if x0 >= end {
        return Ok(());
    }
    let dx = x1 - x0;
    let dy = i64::from(y1) - i64::from(y0);
    let base = dy / i64::from(dx);
    let step = if dy < 0 { base - 1 } else { base + 1 };
    let ady = dy.unsigned_abs() as i64 - base.unsigned_abs() as i64 * i64::from(dx);
    let mut error = 0_i64;
    let mut y = i64::from(y0);
    for x in x0..end {
        output[x as usize] =
            inverse_db(u32::try_from(y).map_err(|_| {
                AudioError::InvalidData("Vorbis floor-1 amplitude is out of range")
            })?)?;
        error += ady;
        if error >= i64::from(dx) {
            error -= i64::from(dx);
            y += step;
        } else {
            y += base;
        }
    }
    Ok(())
}

pub(crate) fn inverse_db(value: u32) -> Result<f32, AudioError> {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        std::array::from_fn(|index| 10.0_f64.powf((index as f64 - 255.0) * (7.0 / 256.0)) as f32)
    });
    table
        .get(value as usize)
        .copied()
        .ok_or(AudioError::InvalidData(
            "Vorbis floor-1 amplitude is out of range",
        ))
}

fn integer_log(value: u32) -> u32 {
    u32::BITS - value.leading_zeros()
}

#[cfg(test)]
mod tests {
    use crate::codebook::{AllocationBudget, HuffmanCodebook};
    use crate::{VorbisFloor0, VorbisFloor1Class, VorbisLookup};

    use super::{FloorResult, decode_floor0, decode_floor1, inverse_db};
    use crate::{VorbisCodebook, VorbisFloor1, bitstream::BitReader};

    struct BitWriter(Vec<bool>);

    impl BitWriter {
        fn new() -> Self {
            Self(Vec::new())
        }

        fn write(&mut self, value: u32, width: usize) {
            self.0.extend((0..width).map(|bit| value & (1 << bit) != 0));
        }

        fn finish(self) -> Vec<u8> {
            let mut bytes = vec![0; self.0.len().div_ceil(8)];
            for (offset, bit) in self.0.into_iter().enumerate() {
                if bit {
                    bytes[offset / 8] |= 1 << (offset % 8);
                }
            }
            bytes
        }
    }

    fn codebook() -> VorbisCodebook {
        let lengths = [1, 1];
        VorbisCodebook {
            dimensions: 1,
            entries: 2,
            codeword_lengths: lengths.to_vec(),
            lookup: VorbisLookup {
                kind: 0,
                minimum: 0,
                delta: 0,
                value_bits: 0,
                sequence: false,
                values: 0,
                multiplicands: Vec::new(),
            },
            huffman: HuffmanCodebook::new(&lengths, &mut AllocationBudget::new()).unwrap(),
        }
    }

    fn floor0_codebook() -> VorbisCodebook {
        let lengths = [1, 1];
        VorbisCodebook {
            dimensions: 2,
            entries: 2,
            codeword_lengths: lengths.to_vec(),
            lookup: VorbisLookup {
                kind: 2,
                minimum: 0,
                delta: (788 << 21) | 1,
                value_bits: 2,
                sequence: false,
                values: 4,
                multiplicands: vec![1, 2, 3, 4],
            },
            huffman: HuffmanCodebook::new(&lengths, &mut AllocationBudget::new()).unwrap(),
        }
    }

    #[test]
    fn decodes_floor_one_empty_and_active_curves() {
        let floor = VorbisFloor1 {
            partition_classes: Vec::new(),
            classes: Vec::new(),
            multiplier: 1,
            range_bits: 0,
            x_list: vec![0, 1],
        };
        let mut bits = BitReader::new(&[0]);
        assert!(matches!(
            decode_floor1(&floor, &[], &mut bits, 16).unwrap(),
            FloorResult::Unused
        ));

        let mut packet = BitWriter::new();
        packet.write(1, 1);
        packet.write(80, 8);
        packet.write(80, 8);
        let packet = packet.finish();
        let FloorResult::Curve(curve) =
            decode_floor1(&floor, &[], &mut BitReader::new(&packet), 16).unwrap()
        else {
            panic!("active floor was not decoded")
        };
        assert_eq!(curve.len(), 16);
        assert!(
            curve
                .iter()
                .all(|&value| (value - inverse_db(80).unwrap()).abs() < 1e-10)
        );
    }

    #[test]
    fn inverse_db_values_match_the_xiph_table_endpoints() {
        assert!((inverse_db(0).unwrap() - 1.0649863e-7).abs() < 1e-13);
        assert!((inverse_db(1).unwrap() - 1.1341951e-7).abs() < 1e-13);
        assert_eq!(inverse_db(255).unwrap(), 1.0);
    }

    #[test]
    fn decodes_floor_zero_lsp_curve_and_treats_packet_truncation_as_unused() {
        let floor = VorbisFloor0 {
            order: 3,
            rate: 44_100,
            bark_map_size: 64,
            amplitude_bits: 4,
            amplitude_offset: 20,
            books: vec![0],
        };
        let mut packet = BitWriter::new();
        packet.write(15, 4); // Nonzero amplitude.
        packet.write(0, 1); // Select the only floor codebook.
        packet.write(1, 1); // Entry one has LSP coefficient 3 radians.
        packet.write(1, 1); // Reuse the selected book for the next coefficient vector.
        let packet = packet.finish();
        let FloorResult::Curve(curve) = decode_floor0(
            &floor,
            &[floor0_codebook()],
            &mut BitReader::new(&packet),
            32,
        )
        .unwrap() else {
            panic!("active floor was not decoded")
        };
        let denominator =
            (0.25 * (4.0 * (3.0_f64.cos() - 1.0).powi(2)) * (4.0 * (7.0_f64.cos() - 1.0).powi(2)))
                .sqrt();
        let expected_first = (0.11512925 * (20.0 / denominator - 20.0)).exp();
        assert!(
            (f64::from(curve[0]) - expected_first).abs() < 1e-6,
            "actual {}, expected {expected_first}",
            curve[0]
        );
        assert!(curve.iter().all(|value| value.is_finite() && *value > 0.0));

        let two_books = VorbisFloor0 {
            books: vec![0, 0],
            ..floor.clone()
        };
        let mut packet = BitWriter::new();
        packet.write(15, 4);
        packet.write(1, 2); // The spec-width selector can also choose book one.
        packet.write(1, 1); // Entry one has LSP coefficient 3 radians.
        packet.write(1, 1);
        assert!(matches!(
            decode_floor0(
                &two_books,
                &[floor0_codebook()],
                &mut BitReader::new(&packet.finish()),
                32,
            )
            .unwrap(),
            FloorResult::Curve(_)
        ));

        assert!(matches!(
            decode_floor0(&floor, &[], &mut BitReader::new(&[0]), 32).unwrap(),
            FloorResult::Unused
        ));
        assert!(matches!(
            decode_floor0(&floor, &[floor0_codebook()], &mut BitReader::new(&[]), 32).unwrap(),
            FloorResult::Truncated
        ));
    }

    #[test]
    fn decodes_floor_one_partition_books_and_handles_truncation_as_unused() {
        let floor = VorbisFloor1 {
            partition_classes: vec![0],
            classes: vec![VorbisFloor1Class {
                dimensions: 1,
                subclasses: 1,
                masterbook: Some(0),
                subclass_books: vec![Some(0), None],
            }],
            multiplier: 1,
            range_bits: 4,
            x_list: vec![0, 16, 8],
        };
        let books = [codebook()];
        let mut packet = BitWriter::new();
        packet.write(1, 1); // Floor present.
        packet.write(80, 8); // Endpoint 0.
        packet.write(80, 8); // Endpoint 1.
        packet.write(0, 1); // Masterbook selects subclass book 0.
        packet.write(1, 1); // Subclass book emits a nonzero difference.
        let packet = packet.finish();
        let FloorResult::Curve(curve) =
            decode_floor1(&floor, &books, &mut BitReader::new(&packet), 16).unwrap()
        else {
            panic!("active floor was not decoded")
        };
        assert!(curve[8] < curve[0]);

        assert!(matches!(
            decode_floor1(&floor, &books, &mut BitReader::new(&[1]), 16).unwrap(),
            FloorResult::Truncated
        ));
    }
}
