use crate::range::{RangeDecoder, RangeError};

const MAX_PVQ_DIMENSIONS: usize = 176;
const MAX_PVQ_PULSES: usize = 1024;
const MAX_PVQ_PSEUDO_PULSES: u16 = 255;
const MAX_SPLIT_ANGLE_RESOLUTION: usize = 256;
const SPLIT_ANGLE_EXP2_Q14: [u16; 8] = [
    16_384, 17_866, 19_483, 21_247, 23_170, 25_267, 27_554, 30_048,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PulseAllocation {
    pub(crate) pulses: u16,
    pub(crate) codebook_eighth_bits: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SplitAngle {
    pub(crate) resolution: u16,
    pub(crate) index: u16,
    pub(crate) angle_q14: u16,
}

pub(crate) fn decode_split_angle(
    range: &mut RangeDecoder<'_>,
    dimensions: usize,
    allocation_eighth_bits: u32,
    pulse_cap_eighth_bits: u32,
    offset_eighth_bits: i32,
    time_split: bool,
) -> Result<SplitAngle, RangeError> {
    if !(2..=MAX_PVQ_DIMENSIONS).contains(&dimensions) {
        return Err(RangeError::InvalidPvqLayout);
    }

    let split_dimensions = (2 * dimensions - 1) as i64;
    let allocation = i64::from(allocation_eighth_bits);
    let pulse_cap = i64::from(pulse_cap_eighth_bits);
    let offset = i64::from(offset_eighth_bits);
    let resolution_bits = (allocation - pulse_cap - 32)
        .min((allocation + split_dimensions * offset) / split_dimensions)
        .min(64);
    let resolution = if resolution_bits < 4 {
        1
    } else {
        let exponent = (resolution_bits / 8) as u32;
        let fraction = (resolution_bits % 8) as usize;
        let approximate = u32::from(SPLIT_ANGLE_EXP2_Q14[fraction]) >> (14 - exponent);
        ((approximate + 1) >> 1 << 1) as u16
    };
    let index = if resolution == 1 {
        0
    } else if time_split {
        range.decode_uint(u32::from(resolution) + 1)? as u16
    } else {
        let resolution = usize::from(resolution);
        let half = resolution / 2;
        let mut frequencies = [0_u16; MAX_SPLIT_ANGLE_RESOLUTION + 1];
        for (value, frequency) in frequencies[..=resolution].iter_mut().enumerate() {
            *frequency = (value.min(resolution - value) + 1) as u16;
        }
        debug_assert_eq!(
            frequencies[..=resolution]
                .iter()
                .map(|&value| usize::from(value))
                .sum::<usize>(),
            (half + 1) * (half + 1)
        );
        range.decode_symbol(&frequencies[..=resolution])? as u16
    };
    let angle_q14 = (u32::from(index) * 16_384 / u32::from(resolution)) as u16;
    Ok(SplitAngle {
        resolution,
        index,
        angle_q14,
    })
}

pub(crate) fn codebook_size(dimensions: usize, pulses: usize) -> Result<u32, RangeError> {
    let counts = codebook_counts(dimensions, pulses)?;
    Ok(counts[dimensions][pulses])
}

pub(crate) fn unrank_codeword(
    dimensions: usize,
    pulses: usize,
    index: u32,
) -> Result<Vec<i32>, RangeError> {
    let counts = codebook_counts(dimensions, pulses)?;
    unrank_from_counts(&counts, dimensions, pulses, index)
}

fn unrank_from_counts(
    counts: &[Vec<u32>],
    dimensions: usize,
    pulses: usize,
    mut index: u32,
) -> Result<Vec<i32>, RangeError> {
    if index >= counts[dimensions][pulses] {
        return Err(RangeError::CorruptFrame);
    }

    let mut vector = Vec::with_capacity(dimensions);
    let mut remaining_pulses = pulses;
    for position in 0..dimensions {
        let remaining_dimensions = dimensions - position;
        let positive_count = (u64::from(counts[remaining_dimensions - 1][remaining_pulses])
            + u64::from(counts[remaining_dimensions][remaining_pulses]))
            / 2;
        let sign = if u64::from(index) < positive_count {
            1_i32
        } else {
            index -= positive_count as u32;
            -1_i32
        };

        let original_pulses = remaining_pulses;
        let mut branch_start = positive_count
            .checked_sub(u64::from(
                counts[remaining_dimensions - 1][remaining_pulses],
            ))
            .ok_or(RangeError::CorruptFrame)?;
        while branch_start > u64::from(index) {
            if remaining_pulses == 0 {
                return Err(RangeError::CorruptFrame);
            }
            remaining_pulses -= 1;
            branch_start = branch_start
                .checked_sub(u64::from(
                    counts[remaining_dimensions - 1][remaining_pulses],
                ))
                .ok_or(RangeError::CorruptFrame)?;
        }

        vector.push(sign * (original_pulses - remaining_pulses) as i32);
        index -= branch_start as u32;
    }

    if remaining_pulses != 0 || index != 0 {
        return Err(RangeError::CorruptFrame);
    }
    Ok(vector)
}

pub(crate) fn decode_codeword(
    range: &mut RangeDecoder<'_>,
    dimensions: usize,
    pulses: usize,
) -> Result<Vec<i32>, RangeError> {
    let counts = codebook_counts(dimensions, pulses)?;
    let total = counts[dimensions][pulses];
    let index = range.decode_uint(total)?;
    unrank_from_counts(&counts, dimensions, pulses, index)
}

pub(crate) fn normalize_pulses(pulses: &[i32]) -> Result<Vec<f32>, RangeError> {
    if pulses.is_empty() || pulses.len() > MAX_PVQ_DIMENSIONS {
        return Err(RangeError::InvalidPvqLayout);
    }
    let norm_squared = pulses
        .iter()
        .map(|&pulse| f64::from(pulse) * f64::from(pulse))
        .sum::<f64>();
    if norm_squared == 0.0 {
        return Ok(vec![0.0; pulses.len()]);
    }
    let inverse_norm = norm_squared.sqrt().recip();
    Ok(pulses
        .iter()
        .map(|&pulse| (f64::from(pulse) * inverse_norm) as f32)
        .collect())
}

pub(crate) fn spread_shape(
    shape: &mut [f32],
    pulse_count: u16,
    spread: u8,
    blocks: u8,
) -> Result<(), RangeError> {
    if shape.is_empty() || shape.len() > MAX_PVQ_DIMENSIONS || blocks == 0 {
        return Err(RangeError::InvalidPvqLayout);
    }
    if spread > 3 {
        return Err(RangeError::InvalidPvqSpread);
    }
    let block_count = usize::from(blocks);
    if shape.len() % block_count != 0 {
        return Err(RangeError::InvalidPvqBlockCount);
    }
    if spread == 0 || shape.len() < 2 {
        return Ok(());
    }

    let dimensions = shape.len();
    let block_width = dimensions / block_count;
    let spread_factor = [0.0_f32, 15.0, 10.0, 5.0][usize::from(spread)];
    let gain = dimensions as f32 / (dimensions as f32 + spread_factor * f32::from(pulse_count));
    let theta = std::f32::consts::FRAC_PI_4 * gain * gain;
    let (main_sine, main_cosine) = theta.sin_cos();

    if block_count > 1 && block_width >= 8 {
        let stride = rounded_sqrt(block_width);
        let (pre_sine, pre_cosine) = (std::f32::consts::FRAC_PI_2 - theta).sin_cos();
        for offset in 0..stride {
            let indices: Vec<usize> = (offset..dimensions).step_by(stride).collect();
            rotate_index_chain(shape, &indices, pre_sine, pre_cosine);
        }
    }

    for block in 0..block_count {
        let start = block * block_width;
        rotate_contiguous_chain(
            &mut shape[start..start + block_width],
            main_sine,
            main_cosine,
        );
    }
    Ok(())
}

fn rotate_contiguous_chain(values: &mut [f32], sine: f32, cosine: f32) {
    for index in 0..values.len().saturating_sub(1) {
        rotate_pair(values, index, index + 1, sine, cosine);
    }
    for index in (0..values.len().saturating_sub(2)).rev() {
        rotate_pair(values, index, index + 1, sine, cosine);
    }
}

fn rotate_index_chain(values: &mut [f32], indices: &[usize], sine: f32, cosine: f32) {
    for index in 0..indices.len().saturating_sub(1) {
        rotate_pair(values, indices[index], indices[index + 1], sine, cosine);
    }
    for index in (0..indices.len().saturating_sub(2)).rev() {
        rotate_pair(values, indices[index], indices[index + 1], sine, cosine);
    }
}

fn rotate_pair(values: &mut [f32], first: usize, second: usize, sine: f32, cosine: f32) {
    let x = values[first];
    let y = values[second];
    values[first] = cosine * x + sine * y;
    values[second] = -sine * x + cosine * y;
}

fn rounded_sqrt(value: usize) -> usize {
    let floor = integer_sqrt(value);
    if 4 * (value - floor * floor) > 4 * floor + 1 {
        floor + 1
    } else {
        floor
    }
}

fn integer_sqrt(value: usize) -> usize {
    let mut low = 0_usize;
    let mut high = value.min(usize::BITS as usize).max(1);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if middle <= value / middle {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

pub(crate) fn pulses_for_allocation(
    dimensions: usize,
    available_eighth_bits: u32,
) -> Result<PulseAllocation, RangeError> {
    if dimensions == 0 || dimensions > MAX_PVQ_DIMENSIONS {
        return Err(RangeError::InvalidPvqLayout);
    }

    let mut low = 0_usize;
    let mut high = MAX_PVQ_PULSES;
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        match codebook_size(dimensions, middle) {
            Ok(_) => low = middle,
            Err(RangeError::PvqCodebookTooLarge) => high = middle - 1,
            Err(error) => return Err(error),
        }
    }
    let counts = codebook_counts(dimensions, low)?;

    let mut best = PulseAllocation {
        pulses: 0,
        codebook_eighth_bits: 0,
    };
    let mut best_distance = available_eighth_bits;
    for pseudo in 1..=MAX_PVQ_PSEUDO_PULSES {
        let Some(pulses) = pseudo_pulse_count(pseudo) else {
            break;
        };
        if pulses > low {
            break;
        }
        let cost = u32::from(conservative_log2_eighths(counts[dimensions][pulses]));
        if cost > available_eighth_bits {
            continue;
        }
        let distance = available_eighth_bits - cost;
        if distance < best_distance {
            best = PulseAllocation {
                pulses: pulses as u16,
                codebook_eighth_bits: cost as u16,
            };
            best_distance = distance;
        }
    }
    Ok(best)
}

fn pseudo_pulse_count(index: u16) -> Option<usize> {
    if index < 8 {
        return Some(usize::from(index));
    }
    let shift = u32::from(index / 8 - 1);
    usize::from(8 + (index & 7)).checked_shl(shift)
}

fn conservative_log2_eighths(value: u32) -> u16 {
    if value <= 1 {
        return 0;
    }

    // ceil(log2(value) * 8) is ceil(log2(value^8)). A codebook size is
    // bounded to u32, so its eighth power fits in four u64 limbs.
    let mut power = [1_u64, 0, 0, 0];
    for _ in 0..8 {
        let mut carry = 0_u128;
        for limb in &mut power {
            let product = u128::from(*limb) * u128::from(value) + carry;
            *limb = product as u64;
            carry = product >> u64::BITS;
        }
        debug_assert_eq!(carry, 0);
    }

    let highest_limb = power.iter().rposition(|&limb| limb != 0).unwrap_or(0);
    let bit_length = highest_limb as u16 * u64::BITS as u16
        + (u64::BITS - power[highest_limb].leading_zeros()) as u16;
    let is_power_of_two = power.iter().map(|limb| limb.count_ones()).sum::<u32>() == 1;
    if is_power_of_two {
        bit_length - 1
    } else {
        bit_length
    }
}

fn codebook_counts(dimensions: usize, pulses: usize) -> Result<Vec<Vec<u32>>, RangeError> {
    if dimensions == 0 || dimensions > MAX_PVQ_DIMENSIONS || pulses > MAX_PVQ_PULSES {
        return Err(RangeError::InvalidPvqLayout);
    }

    let mut counts = vec![vec![0_u32; pulses + 1]; dimensions + 1];
    counts[0][0] = 1;
    for dimension in 1..=dimensions {
        counts[dimension][0] = 1;
        for pulse in 1..=pulses {
            counts[dimension][pulse] = counts[dimension - 1][pulse]
                .checked_add(counts[dimension][pulse - 1])
                .and_then(|sum| sum.checked_add(counts[dimension - 1][pulse - 1]))
                .ok_or(RangeError::PvqCodebookTooLarge)?;
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        codebook_size, decode_codeword, decode_split_angle, normalize_pulses,
        pulses_for_allocation, spread_shape, unrank_codeword,
    };
    use crate::range::{RangeDecoder, RangeError};

    #[test]
    fn codebook_sizes_match_small_signed_pulse_spheres() {
        assert_eq!(codebook_size(1, 0), Ok(1));
        assert_eq!(codebook_size(1, 3), Ok(2));
        assert_eq!(codebook_size(2, 1), Ok(4));
        assert_eq!(codebook_size(2, 2), Ok(8));
        assert_eq!(codebook_size(3, 1), Ok(6));
        assert_eq!(codebook_size(3, 2), Ok(18));
        assert_eq!(codebook_size(3, 3), Ok(38));
    }

    #[test]
    fn unranking_visits_each_small_codeword_once() {
        let mut words = HashSet::new();
        for index in 0..codebook_size(3, 2).unwrap() {
            let vector = unrank_codeword(3, 2, index).unwrap();
            assert_eq!(
                vector.iter().map(|value| value.unsigned_abs()).sum::<u32>(),
                2
            );
            words.insert(vector);
        }
        assert_eq!(words.len(), 18);
    }

    #[test]
    fn decoder_uses_the_uniform_range_index() {
        let mut low = RangeDecoder::new(&[0x00]);
        let mut high = RangeDecoder::new(&[0xff]);
        let low_vector = decode_codeword(&mut low, 2, 2).unwrap();
        let high_vector = decode_codeword(&mut high, 2, 2).unwrap();
        assert_eq!(low_vector.iter().map(|value| value.abs()).sum::<i32>(), 2);
        assert_eq!(high_vector.iter().map(|value| value.abs()).sum::<i32>(), 2);
        assert_ne!(low_vector, high_vector);
    }

    #[test]
    fn rejects_invalid_layouts_overlarge_codebooks_and_indexes() {
        assert_eq!(codebook_size(0, 1), Err(RangeError::InvalidPvqLayout));
        assert_eq!(codebook_size(177, 1), Err(RangeError::InvalidPvqLayout));
        assert_eq!(codebook_size(2, 1025), Err(RangeError::InvalidPvqLayout));
        assert_eq!(
            codebook_size(176, 100),
            Err(RangeError::PvqCodebookTooLarge)
        );
        assert_eq!(unrank_codeword(2, 2, 8), Err(RangeError::CorruptFrame));
    }

    #[test]
    fn pulse_normalization_produces_a_unit_vector_and_keeps_zero_vectors_zero() {
        let normalized = normalize_pulses(&[3, 4, 0]).unwrap();
        let norm_squared = normalized
            .iter()
            .map(|&value| f64::from(value) * f64::from(value))
            .sum::<f64>();
        assert!((norm_squared - 1.0).abs() < 1e-7);
        assert_eq!(normalize_pulses(&[0, 0]), Ok(vec![0.0, 0.0]));
        assert_eq!(normalize_pulses(&[]), Err(RangeError::InvalidPvqLayout));
    }

    #[test]
    fn allocation_selects_the_nearest_fitting_pseudo_pulse_count() {
        assert_eq!(
            pulses_for_allocation(2, 20),
            Ok(super::PulseAllocation {
                pulses: 1,
                codebook_eighth_bits: 16,
            })
        );
        assert_eq!(
            pulses_for_allocation(2, 28),
            Ok(super::PulseAllocation {
                pulses: 2,
                codebook_eighth_bits: 24,
            })
        );
        assert_eq!(
            pulses_for_allocation(2, 32),
            Ok(super::PulseAllocation {
                pulses: 4,
                codebook_eighth_bits: 32,
            })
        );
        assert_eq!(
            pulses_for_allocation(1, 8),
            Ok(super::PulseAllocation {
                pulses: 1,
                codebook_eighth_bits: 8,
            })
        );
    }

    #[test]
    fn split_angle_resolution_tracks_available_eighth_bits() {
        for (resolution_bits, expected_resolution) in [
            (3, 1),
            (4, 2),
            (8, 2),
            (16, 4),
            (32, 16),
            (63, 234),
            (64, 256),
        ] {
            let allocation = 48 + resolution_bits;
            let mut range = RangeDecoder::new(&[0; 32]);
            let angle = decode_split_angle(&mut range, 4, allocation, 16, 64, true).unwrap();
            assert_eq!(angle.resolution, expected_resolution, "{resolution_bits}");
            assert_eq!(angle.index, 0);
            assert_eq!(angle.angle_q14, 0);
        }

        let mut previous_resolution = 1;
        for resolution_bits in 4..=64 {
            let allocation = 48 + resolution_bits;
            let mut range = RangeDecoder::new(&[0; 32]);
            let angle = decode_split_angle(&mut range, 4, allocation, 16, 64, false).unwrap();
            assert!(angle.resolution >= previous_resolution);
            assert_eq!(angle.resolution % 2, 0);
            previous_resolution = angle.resolution;
        }
    }

    #[test]
    fn split_angle_uses_uniform_time_and_triangular_frequency_models() {
        let bytes = [0x5a; 32];
        let mut time_range = RangeDecoder::new(&bytes);
        let time = decode_split_angle(&mut time_range, 4, 80, 16, 64, true).unwrap();
        let mut expected_time_range = RangeDecoder::new(&bytes);
        let expected_time = expected_time_range.decode_uint(17).unwrap() as u16;
        assert_eq!(time.resolution, 16);
        assert_eq!(time.index, expected_time);
        assert_eq!(
            time.angle_q14,
            (u32::from(expected_time) * 16_384 / 16) as u16
        );
        assert_eq!(
            time_range.decode_uint(19),
            expected_time_range.decode_uint(19)
        );

        let mut frequency_range = RangeDecoder::new(&bytes);
        let frequency = decode_split_angle(&mut frequency_range, 4, 80, 16, 64, false).unwrap();
        let frequencies: Vec<u16> = (0..=16)
            .map(|value| (value.min(16 - value) + 1) as u16)
            .collect();
        let mut expected_frequency_range = RangeDecoder::new(&bytes);
        let expected_frequency = expected_frequency_range
            .decode_symbol(&frequencies)
            .unwrap() as u16;
        assert_eq!(frequency.resolution, 16);
        assert_eq!(frequency.index, expected_frequency);
        assert_eq!(
            frequency_range.decode_uint(19),
            expected_frequency_range.decode_uint(19)
        );
    }

    #[test]
    fn split_angle_rejects_dimensions_outside_pvq_bounds() {
        let mut range = RangeDecoder::new(&[0]);
        assert_eq!(
            decode_split_angle(&mut range, 1, 64, 16, 0, false),
            Err(RangeError::InvalidPvqLayout)
        );
        assert_eq!(
            decode_split_angle(&mut range, 177, 64, 16, 0, false),
            Err(RangeError::InvalidPvqLayout)
        );
    }

    #[test]
    fn codebook_cost_rounds_log2_up_without_float_math() {
        for (size, expected) in [(1, 0), (2, 8), (3, 13), (4, 16), (8, 24), (9, 26), (16, 32)] {
            assert_eq!(super::conservative_log2_eighths(size), expected);
        }
        assert_eq!(super::conservative_log2_eighths(u32::MAX), 256);
        for size in 1..=10_000 {
            let expected = (f64::from(size).log2() * 8.0).ceil() as u16;
            assert_eq!(super::conservative_log2_eighths(size), expected, "{size}");
        }
    }

    #[test]
    fn spreading_is_identity_for_none_and_preserves_norm_for_each_mode() {
        let original = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut unchanged = original;
        spread_shape(&mut unchanged, 1, 0, 1).unwrap();
        assert_eq!(unchanged, original);

        for spread in 1..=3 {
            let mut shape = original;
            spread_shape(&mut shape, 1, spread, 1).unwrap();
            let norm_squared = shape.iter().map(|value| value * value).sum::<f32>();
            assert!((norm_squared - 1.0).abs() < 1e-6);
            assert_ne!(shape, original);
        }
    }

    #[test]
    fn spreading_handles_blockwise_and_interleaved_prerotation() {
        let mut shape = [1.0; 16];
        spread_shape(&mut shape, 3, 2, 2).unwrap();
        let norm_squared = shape.iter().map(|value| value * value).sum::<f32>();
        assert!((norm_squared - 16.0).abs() < 1e-5);
        assert_ne!(&shape[..8], &shape[8..]);
        assert_eq!(super::rounded_sqrt(8), 3);
        assert_eq!(super::rounded_sqrt(15), 4);
        assert_eq!(super::rounded_sqrt(16), 4);
    }

    #[test]
    fn spreading_rejects_invalid_parameters_and_block_geometry() {
        let mut shape = [0.0; 8];
        assert_eq!(
            spread_shape(&mut shape, 1, 4, 1),
            Err(RangeError::InvalidPvqSpread)
        );
        assert_eq!(
            spread_shape(&mut shape, 1, 1, 3),
            Err(RangeError::InvalidPvqBlockCount)
        );
        assert_eq!(
            spread_shape(&mut [], 1, 1, 1),
            Err(RangeError::InvalidPvqLayout)
        );
    }
}
