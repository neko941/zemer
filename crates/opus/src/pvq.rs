use crate::range::{RangeDecoder, RangeError};

const MAX_PVQ_DIMENSIONS: usize = 176;
const MAX_PVQ_PSEUDO_PULSES: u16 = 255;
const MAX_SPLIT_ANGLE_RESOLUTION: usize = 256;
const SPLIT_ANGLE_EXP2_Q14: [u16; 8] = [
    16_384, 17_866, 19_483, 21_247, 23_170, 25_267, 27_554, 30_048,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PulseAllocation {
    pub(crate) pulses: u32,
    pub(crate) codebook_eighth_bits: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SplitAngle {
    pub(crate) resolution: u16,
    pub(crate) index: u16,
    pub(crate) angle_q14: u16,
    pub(crate) left_gain: f32,
    pub(crate) right_gain: f32,
}

/// A split decision after its angle symbol has been decoded.
///
/// `remaining_eighth_bits` is the shape budget left after the angle's actual
/// range-coded cost has been removed. The caller owns that bitstream-specific
/// accounting; this value is then partitioned between equal-width children.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RecursivePvqSplit {
    pub(crate) angle: SplitAngle,
    pub(crate) remaining_eighth_bits: u32,
}

/// Decodes a PVQ shape, recursively applying caller-decoded split angles.
///
/// The callback runs at each node before its children, matching the recursive
/// codeword order. It returns `None` for a leaf, or the decoded angle and the
/// remaining budget for a split node. Angle precision and its coded-bit cost
/// remain the caller's responsibility because those depend on CELT allocation
/// state. The maximum split depth is fixed by the CELT
/// frame duration as LM + 1.
pub(crate) fn decode_recursive_pvq_shape(
    range: &mut RangeDecoder<'_>,
    frame_samples_48k: u16,
    dimensions: usize,
    budget_eighth_bits: u32,
    decode_split: &mut impl FnMut(
        &mut RangeDecoder<'_>,
        usize,
        u32,
        u8,
    ) -> Result<Option<RecursivePvqSplit>, RangeError>,
) -> Result<(Vec<f32>, u32), RangeError> {
    let max_split_levels = pvq_split_level_limit(frame_samples_48k)?;
    decode_recursive_pvq_shape_at(
        range,
        dimensions,
        budget_eighth_bits,
        max_split_levels,
        0,
        decode_split,
    )
}

fn pvq_split_level_limit(frame_samples_48k: u16) -> Result<u8, RangeError> {
    match frame_samples_48k {
        120 => Ok(1),
        240 => Ok(2),
        480 => Ok(3),
        960 => Ok(4),
        _ => Err(RangeError::InvalidCeltEnergyLayout),
    }
}

fn decode_recursive_pvq_shape_at(
    range: &mut RangeDecoder<'_>,
    dimensions: usize,
    budget_eighth_bits: u32,
    max_split_levels: u8,
    depth: u8,
    decode_split: &mut impl FnMut(
        &mut RangeDecoder<'_>,
        usize,
        u32,
        u8,
    ) -> Result<Option<RecursivePvqSplit>, RangeError>,
) -> Result<(Vec<f32>, u32), RangeError> {
    if dimensions == 0 || dimensions > MAX_PVQ_DIMENSIONS {
        return Err(RangeError::InvalidPvqLayout);
    }

    let Some(split) = decode_split(range, dimensions, budget_eighth_bits, depth)? else {
        let allocation = pulses_for_allocation(dimensions, budget_eighth_bits)?;
        if dimensions == 1 {
            let sign = if budget_eighth_bits >= 8 {
                range.read_raw_bits(1)?
            } else {
                0
            };
            return Ok((vec![if sign == 0 { 1.0 } else { -1.0 }], 1));
        }

        let pulses = if allocation.pulses == 0 {
            vec![0; dimensions]
        } else {
            decode_codeword(range, dimensions, allocation.pulses as usize)?
        };
        return Ok((normalize_pulses(&pulses)?, allocation.pulses));
    };

    if depth >= max_split_levels || dimensions < 2 || dimensions % 2 != 0 {
        return Err(RangeError::InvalidPvqLayout);
    }
    if split.remaining_eighth_bits > budget_eighth_bits {
        return Err(RangeError::InvalidPvqLayout);
    }
    validate_split_angle(split.angle)?;
    let child_dimensions = dimensions / 2;
    let (left_budget, right_budget) = split_shape_budget(
        split.remaining_eighth_bits,
        child_dimensions,
        split.angle.angle_q14,
    )?;
    let (left, left_pulses) = decode_recursive_pvq_shape_at(
        range,
        child_dimensions,
        left_budget,
        max_split_levels,
        depth + 1,
        decode_split,
    )?;
    let (right, right_pulses) = decode_recursive_pvq_shape_at(
        range,
        child_dimensions,
        right_budget,
        max_split_levels,
        depth + 1,
        decode_split,
    )?;
    let pulses = left_pulses
        .checked_add(right_pulses)
        .ok_or(RangeError::InvalidPvqLayout)?;
    Ok((combine_split_shapes(&left, &right, split.angle)?, pulses))
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
    let (left_gain, right_gain) = split_gains(angle_q14)?;
    Ok(SplitAngle {
        resolution,
        index,
        angle_q14,
        left_gain,
        right_gain,
    })
}

fn split_gains(angle_q14: u16) -> Result<(f32, f32), RangeError> {
    if angle_q14 > 16_384 {
        return Err(RangeError::InvalidPvqLayout);
    }
    if angle_q14 == 0 {
        return Ok((1.0, 0.0));
    }
    if angle_q14 == 16_384 {
        return Ok((0.0, 1.0));
    }
    let angle = f64::from(angle_q14) * (std::f64::consts::FRAC_PI_2 / 16_384.0);
    Ok((angle.cos() as f32, angle.sin() as f32))
}

pub(crate) fn combine_split_shapes(
    left: &[f32],
    right: &[f32],
    split: SplitAngle,
) -> Result<Vec<f32>, RangeError> {
    if left.is_empty()
        || left.len() != right.len()
        || left.len() + right.len() > MAX_PVQ_DIMENSIONS
        || left.iter().chain(right).any(|value| !value.is_finite())
    {
        return Err(RangeError::InvalidPvqLayout);
    }
    validate_split_angle(split)?;

    let mut shape = Vec::with_capacity(left.len() + right.len());
    shape.extend(left.iter().map(|&value| value * split.left_gain));
    shape.extend(right.iter().map(|&value| value * split.right_gain));
    Ok(shape)
}

fn validate_split_angle(split: SplitAngle) -> Result<(), RangeError> {
    if split.angle_q14 > 16_384
        || !split.left_gain.is_finite()
        || !split.right_gain.is_finite()
        || !(0.0..=1.0).contains(&split.left_gain)
        || !(0.0..=1.0).contains(&split.right_gain)
    {
        return Err(RangeError::InvalidPvqLayout);
    }
    Ok(())
}

/// Reconstructs normalized left and right vectors from normalized mid/side vectors.
///
/// Quantization can make the decoded mid and side vectors non-orthogonal, so
/// each reconstructed channel is normalized independently after rotation.
pub(crate) fn combine_mid_side_shapes(
    mid: &[f32],
    side: &[f32],
    angle: SplitAngle,
) -> Result<(Vec<f32>, Vec<f32>), RangeError> {
    if mid.is_empty()
        || mid.len() != side.len()
        || mid.len() > MAX_PVQ_DIMENSIONS
        || mid.iter().chain(side).any(|value| !value.is_finite())
    {
        return Err(RangeError::InvalidPvqLayout);
    }

    let mut left = Vec::with_capacity(mid.len());
    let mut right = Vec::with_capacity(mid.len());
    for (&mid_value, &side_value) in mid.iter().zip(side) {
        left.push(mid_value * angle.left_gain + side_value * angle.right_gain);
        right.push(mid_value * angle.left_gain - side_value * angle.right_gain);
    }
    normalize_shape(&mut left)?;
    normalize_shape(&mut right)?;
    Ok((left, right))
}

fn normalize_shape(shape: &mut [f32]) -> Result<(), RangeError> {
    let norm_squared = shape
        .iter()
        .map(|&value| f64::from(value) * f64::from(value))
        .sum::<f64>();
    if !norm_squared.is_finite() {
        return Err(RangeError::InvalidPvqLayout);
    }
    if norm_squared == 0.0 {
        return Ok(());
    }
    let inverse_norm = norm_squared.sqrt().recip() as f32;
    for value in shape {
        *value *= inverse_norm;
    }
    Ok(())
}

/// Splits a remaining shape budget between equal-dimension PVQ children.
///
/// The budget is measured after coding the split angle, in 1/8-bit units.
/// `child_dimensions` is the dimension of either child. CELT uses the same
/// energy-angle allocation as mid/side stereo: the left child's share is
/// `(budget - (N - 1) * log2(tan(theta))) / 2`.
/// The result is rounded to the nearest eighth-bit, with ties assigned left.
pub(crate) fn split_shape_budget(
    remaining_eighth_bits: u32,
    child_dimensions: usize,
    angle_q14: u16,
) -> Result<(u32, u32), RangeError> {
    if !(1..=MAX_PVQ_DIMENSIONS / 2).contains(&child_dimensions) || angle_q14 > 16_384 {
        return Err(RangeError::InvalidPvqLayout);
    }
    split_energy_angle_budget(remaining_eighth_bits, child_dimensions, angle_q14)
}

/// Splits a stereo band budget between mid and side vectors of equal width.
pub(crate) fn split_stereo_shape_budget(
    remaining_eighth_bits: u32,
    dimensions: usize,
    angle_q14: u16,
) -> Result<(u32, u32), RangeError> {
    if !(1..=MAX_PVQ_DIMENSIONS).contains(&dimensions) || angle_q14 > 16_384 {
        return Err(RangeError::InvalidPvqLayout);
    }
    split_energy_angle_budget(remaining_eighth_bits, dimensions, angle_q14)
}

fn split_energy_angle_budget(
    remaining_eighth_bits: u32,
    dimensions: usize,
    angle_q14: u16,
) -> Result<(u32, u32), RangeError> {
    if angle_q14 == 0 {
        return Ok((remaining_eighth_bits, 0));
    }
    if angle_q14 == 16_384 {
        return Ok((0, remaining_eighth_bits));
    }

    let theta = f64::from(angle_q14) * (std::f64::consts::FRAC_PI_2 / 16_384.0);
    let log2_tangent = theta.tan().ln() / std::f64::consts::LN_2;
    let dimension_cost = (dimensions - 1) as f64 * 8.0 * log2_tangent;
    let left_exact = (f64::from(remaining_eighth_bits) - dimension_cost) * 0.5;
    let left = (left_exact + 0.5)
        .floor()
        .clamp(0.0, f64::from(remaining_eighth_bits)) as u32;
    Ok((left, remaining_eighth_bits - left))
}

pub(crate) fn codebook_size(dimensions: usize, pulses: usize) -> Result<u32, RangeError> {
    if dimensions == 0 || dimensions > MAX_PVQ_DIMENSIONS {
        return Err(RangeError::InvalidPvqLayout);
    }
    if pulses > max_codebook_pulses(dimensions) {
        return Err(RangeError::PvqCodebookTooLarge);
    }
    if dimensions == 1 {
        return Ok(if pulses == 0 { 1 } else { 2 });
    }
    if dimensions == 2 {
        return if pulses == 0 {
            Ok(1)
        } else {
            u32::try_from(pulses)
                .ok()
                .and_then(|pulses| pulses.checked_mul(4))
                .ok_or(RangeError::PvqCodebookTooLarge)
        };
    }
    let counts = codebook_counts(dimensions, pulses)?;
    Ok(counts[dimensions][pulses])
}

pub(crate) fn unrank_codeword(
    dimensions: usize,
    pulses: usize,
    index: u32,
) -> Result<Vec<i32>, RangeError> {
    let size = codebook_size(dimensions, pulses)?;
    if index >= size {
        return Err(RangeError::CorruptFrame);
    }
    if dimensions == 1 {
        return Ok(vec![if index == 0 {
            pulses as i32
        } else {
            -(pulses as i32)
        }]);
    }
    if dimensions == 2 {
        return unrank_two_dimensional(pulses, index);
    }
    let counts = codebook_counts(dimensions, pulses)?;
    unrank_from_counts(&counts, dimensions, pulses, index)
}

fn unrank_two_dimensional(pulses: usize, index: u32) -> Result<Vec<i32>, RangeError> {
    if pulses == 0 {
        return if index == 0 {
            Ok(vec![0, 0])
        } else {
            Err(RangeError::CorruptFrame)
        };
    }

    let positive_count = 2 * pulses as u64 + 1;
    let (sign, local_index) = if u64::from(index) < positive_count {
        (1_i32, u64::from(index))
    } else {
        (-1_i32, u64::from(index) - positive_count)
    };
    let remaining = if local_index == 0 {
        0
    } else {
        local_index.div_ceil(2) as usize
    };
    if remaining > pulses || (sign < 0 && remaining == pulses) {
        return Err(RangeError::CorruptFrame);
    }
    let first = sign * (pulses - remaining) as i32;
    let second = if remaining == 0 {
        0
    } else if local_index % 2 == 1 {
        remaining as i32
    } else {
        -(remaining as i32)
    };
    Ok(vec![first, second])
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
    let total = codebook_size(dimensions, pulses)?;
    let index = range.decode_uint(total)?;
    unrank_codeword(dimensions, pulses, index)
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
    pulse_count: u32,
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
    let gain = dimensions as f32 / (dimensions as f32 + spread_factor * pulse_count as f32);
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
    let mut high = max_codebook_pulses(dimensions);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        match codebook_size(dimensions, middle) {
            Ok(_) => low = middle,
            Err(RangeError::PvqCodebookTooLarge) => high = middle - 1,
            Err(error) => return Err(error),
        }
    }
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
        let size = codebook_size(dimensions, pulses)?;
        let cost = u32::from(conservative_log2_eighths(size));
        if cost > available_eighth_bits {
            continue;
        }
        let distance = available_eighth_bits - cost;
        if distance < best_distance {
            best = PulseAllocation {
                pulses: pulses as u32,
                codebook_eighth_bits: cost as u16,
            };
            best_distance = distance;
        }
    }
    Ok(best)
}

fn max_codebook_pulses(dimensions: usize) -> usize {
    match dimensions {
        1 => 1_024,
        2 => (u32::MAX / 4) as usize,
        3 => 65_536,
        4 => 2_048,
        _ => 1_024,
    }
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
    if dimensions <= 2 || dimensions > MAX_PVQ_DIMENSIONS {
        return Err(RangeError::InvalidPvqLayout);
    }
    if pulses > max_codebook_pulses(dimensions) {
        return Err(RangeError::PvqCodebookTooLarge);
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
        RecursivePvqSplit, SplitAngle, codebook_size, combine_mid_side_shapes,
        combine_split_shapes, decode_codeword, decode_recursive_pvq_shape, decode_split_angle,
        normalize_pulses, pulses_for_allocation, split_shape_budget, split_stereo_shape_budget,
        spread_shape, unrank_codeword,
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
    fn two_dimensional_unranking_matches_the_published_signed_pulse_order() {
        for pulses in 1..=8 {
            let mut expected = vec![vec![pulses as i32, 0]];
            for remaining in 1..=pulses {
                let first = (pulses - remaining) as i32;
                expected.push(vec![first, remaining as i32]);
                expected.push(vec![first, -(remaining as i32)]);
            }
            expected.push(vec![-(pulses as i32), 0]);
            for remaining in 1..pulses {
                let first = -((pulses - remaining) as i32);
                expected.push(vec![first, remaining as i32]);
                expected.push(vec![first, -(remaining as i32)]);
            }

            let actual: Vec<_> = (0..codebook_size(2, pulses).unwrap())
                .map(|index| unrank_codeword(2, pulses, index).unwrap())
                .collect();
            assert_eq!(actual, expected, "pulses={pulses}");
            assert_eq!(actual.iter().collect::<HashSet<_>>().len(), 4 * pulses);
        }
    }

    #[test]
    fn two_dimensional_codebooks_support_high_pulse_counts_without_large_tables() {
        assert_eq!(codebook_size(2, 262_144), Ok(1_048_576));
        assert_eq!(unrank_codeword(2, 262_144, 0), Ok(vec![262_144, 0]));
        let allocation = pulses_for_allocation(2, 160).unwrap();
        assert!(allocation.pulses > 100_000);
        assert_eq!(allocation.codebook_eighth_bits, 160);
        assert_eq!(codebook_size(2, (u32::MAX / 4) as usize), Ok(u32::MAX - 3));
        assert_eq!(
            codebook_size(2, (u32::MAX / 4) as usize + 1),
            Err(RangeError::PvqCodebookTooLarge)
        );
        let mut range = RangeDecoder::new(&[0xa5; 8]);
        let decoded = decode_codeword(&mut range, 2, (u32::MAX / 4) as usize).unwrap();
        assert_eq!(
            decoded
                .iter()
                .map(|value| value.unsigned_abs())
                .sum::<u32>(),
            u32::MAX / 4
        );
    }

    #[test]
    fn recursive_pvq_decodes_parent_split_before_equal_width_children() {
        let angle = SplitAngle {
            resolution: 2,
            index: 1,
            angle_q14: 8_192,
            left_gain: std::f32::consts::FRAC_1_SQRT_2,
            right_gain: std::f32::consts::FRAC_1_SQRT_2,
        };
        let mut range = RangeDecoder::new(&[0x5a; 32]);
        let mut calls = Vec::new();
        let (shape, pulses) = decode_recursive_pvq_shape(
            &mut range,
            240,
            8,
            160,
            &mut |_, dimensions, budget, depth| {
                calls.push((dimensions, budget, depth));
                Ok((depth == 0).then_some(RecursivePvqSplit {
                    angle,
                    remaining_eighth_bits: budget,
                }))
            },
        )
        .unwrap();

        assert_eq!(calls, [(8, 160, 0), (4, 80, 1), (4, 80, 1)]);
        assert!(pulses > 0);
        assert_eq!(shape.len(), 8);
        let norm_squared = shape
            .iter()
            .map(|&value| f64::from(value) * f64::from(value))
            .sum::<f64>();
        assert!((norm_squared - 1.0).abs() < 1e-6);

        let mut expected_range = RangeDecoder::new(&[0x5a; 32]);
        for _ in 0..2 {
            let allocation = pulses_for_allocation(4, 80).unwrap();
            decode_codeword(&mut expected_range, 4, allocation.pulses as usize).unwrap();
        }
        assert_eq!(range.tell_frac(), expected_range.tell_frac());
    }

    #[test]
    fn recursive_pvq_rejects_split_depth_and_uneven_children() {
        let angle = SplitAngle {
            resolution: 2,
            index: 1,
            angle_q14: 8_192,
            left_gain: std::f32::consts::FRAC_1_SQRT_2,
            right_gain: std::f32::consts::FRAC_1_SQRT_2,
        };
        let mut depth_range = RangeDecoder::new(&[0; 8]);
        assert_eq!(
            decode_recursive_pvq_shape(&mut depth_range, 120, 4, 80, &mut |_, _, budget, _| {
                Ok(Some(RecursivePvqSplit {
                    angle,
                    remaining_eighth_bits: budget,
                }))
            },),
            Err(RangeError::InvalidPvqLayout)
        );

        let mut odd_range = RangeDecoder::new(&[0; 8]);
        assert_eq!(
            decode_recursive_pvq_shape(&mut odd_range, 240, 3, 80, &mut |_, _, budget, _| {
                Ok(Some(RecursivePvqSplit {
                    angle,
                    remaining_eighth_bits: budget,
                }))
            },),
            Err(RangeError::InvalidPvqLayout)
        );
    }

    #[test]
    fn recursive_pvq_split_depth_follows_celt_frame_lm() {
        for (frame_samples_48k, expected) in [(120, 1), (240, 2), (480, 3), (960, 4)] {
            assert_eq!(
                super::pvq_split_level_limit(frame_samples_48k),
                Ok(expected),
                "frame_samples_48k={frame_samples_48k}"
            );
        }
        assert_eq!(
            super::pvq_split_level_limit(600),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
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
        assert_eq!(codebook_size(0, 1025), Err(RangeError::InvalidPvqLayout));
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
    fn split_gains_cover_endpoints_and_preserve_unit_energy() {
        assert_eq!(super::split_gains(0), Ok((1.0, 0.0)));
        assert_eq!(super::split_gains(16_384), Ok((0.0, 1.0)));
        assert!(super::split_gains(16_385).is_err());
        for angle_q14 in (0..=16_384).step_by(257) {
            let (left, right) = super::split_gains(angle_q14).unwrap();
            let energy = left * left + right * right;
            assert!((energy - 1.0).abs() < 2e-7, "angle={angle_q14}: {energy}");
            assert!((0.0..=1.0).contains(&left));
            assert!((0.0..=1.0).contains(&right));
        }
    }

    #[test]
    fn split_shape_combines_unit_children_with_quantized_gains() {
        let (left_gain, right_gain) = super::split_gains(8_192).unwrap();
        let split = super::SplitAngle {
            resolution: 2,
            index: 1,
            angle_q14: 8_192,
            left_gain,
            right_gain,
        };
        let left = [0.6, 0.8];
        let right = [0.0, 1.0];
        let combined = super::combine_split_shapes(&left, &right, split).unwrap();
        let left_energy = combined[..2]
            .iter()
            .map(|&value| value * value)
            .sum::<f32>();
        let right_energy = combined[2..]
            .iter()
            .map(|&value| value * value)
            .sum::<f32>();
        assert!((left_energy - 0.5).abs() < 2e-7);
        assert!((right_energy - 0.5).abs() < 2e-7);
        assert!((left_energy + right_energy - 1.0).abs() < 2e-7);
        assert_eq!(combined.len(), 4);
        assert!(super::combine_split_shapes(&left[..1], &right, split).is_err());
        assert!(super::combine_split_shapes(&left, &[f32::NAN, 1.0], split).is_err());
    }

    #[test]
    fn split_shape_rejects_invalid_angle_gains() {
        let child = [1.0, 0.0];
        let mut invalid = SplitAngle {
            resolution: 2,
            index: 1,
            angle_q14: 8_192,
            left_gain: f32::NAN,
            right_gain: 0.5,
        };
        assert_eq!(
            combine_split_shapes(&child, &child, invalid),
            Err(RangeError::InvalidPvqLayout)
        );
        invalid.left_gain = 0.5;
        invalid.right_gain = 1.1;
        assert_eq!(
            combine_split_shapes(&child, &child, invalid),
            Err(RangeError::InvalidPvqLayout)
        );
        invalid.right_gain = 0.5;
        invalid.angle_q14 = 16_385;
        assert_eq!(
            combine_split_shapes(&child, &child, invalid),
            Err(RangeError::InvalidPvqLayout)
        );
    }

    #[test]
    fn mid_side_reconstruction_rotates_and_renormalizes_channels() {
        let angle = super::SplitAngle {
            resolution: 2,
            index: 1,
            angle_q14: 8_192,
            left_gain: std::f32::consts::FRAC_1_SQRT_2,
            right_gain: std::f32::consts::FRAC_1_SQRT_2,
        };
        let mid = [1.0, 0.0];
        let side = [0.0, 1.0];
        let (left, right) = combine_mid_side_shapes(&mid, &side, angle).unwrap();
        let expected = std::f32::consts::FRAC_1_SQRT_2;
        for (actual, wanted) in left.iter().zip([expected, expected]) {
            assert!((actual - wanted).abs() < 1e-7);
        }
        for (actual, wanted) in right.iter().zip([expected, -expected]) {
            assert!((actual - wanted).abs() < 1e-7);
        }

        let non_orthogonal_side = [0.5, 0.866_025_4];
        let (left, right) = combine_mid_side_shapes(&mid, &non_orthogonal_side, angle).unwrap();
        for channel in [left, right] {
            let norm_squared = channel
                .iter()
                .map(|&value| f64::from(value) * f64::from(value))
                .sum::<f64>();
            assert!((norm_squared - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn mid_side_reconstruction_rejects_invalid_vectors() {
        let angle = super::SplitAngle {
            resolution: 2,
            index: 1,
            angle_q14: 8_192,
            left_gain: std::f32::consts::FRAC_1_SQRT_2,
            right_gain: std::f32::consts::FRAC_1_SQRT_2,
        };
        assert!(combine_mid_side_shapes(&[], &[], angle).is_err());
        assert!(combine_mid_side_shapes(&[1.0], &[1.0, 0.0], angle).is_err());
        assert!(combine_mid_side_shapes(&[f32::INFINITY], &[1.0], angle).is_err());
        assert_eq!(
            combine_mid_side_shapes(&[0.0], &[0.0], angle),
            Ok((vec![0.0], vec![0.0]))
        );
    }

    #[test]
    fn split_shape_budget_tracks_energy_angle_and_preserves_total() {
        assert_eq!(split_shape_budget(160, 4, 8_192), Ok((80, 80)));
        assert_eq!(split_shape_budget(160, 4, 4_096), Ok((95, 65)));
        assert_eq!(split_shape_budget(160, 4, 12_288), Ok((65, 95)));
        assert_eq!(split_shape_budget(101, 4, 8_192), Ok((51, 50)));
    }

    #[test]
    fn split_shape_budget_handles_zero_energy_endpoints() {
        assert_eq!(split_shape_budget(101, 4, 0), Ok((101, 0)));
        assert_eq!(split_shape_budget(101, 4, 16_384), Ok((0, 101)));
    }

    #[test]
    fn split_shape_budget_rejects_invalid_dimensions_and_angles() {
        assert_eq!(split_shape_budget(101, 1, 8_192), Ok((51, 50)));
        assert_eq!(split_shape_budget(101, 88, 8_192), Ok((51, 50)));
        assert_eq!(
            split_shape_budget(101, 0, 8_192),
            Err(RangeError::InvalidPvqLayout)
        );
        assert_eq!(
            split_shape_budget(101, 89, 8_192),
            Err(RangeError::InvalidPvqLayout)
        );
        assert_eq!(
            split_shape_budget(101, 178, 8_192),
            Err(RangeError::InvalidPvqLayout)
        );
        assert_eq!(
            split_shape_budget(101, 4, 16_385),
            Err(RangeError::InvalidPvqLayout)
        );
    }

    #[test]
    fn stereo_shape_budget_accepts_full_celt_band_widths() {
        assert_eq!(split_stereo_shape_budget(160, 176, 8_192), Ok((80, 80)));
        assert_eq!(
            split_stereo_shape_budget(160, 0, 8_192),
            Err(RangeError::InvalidPvqLayout)
        );
        assert_eq!(
            split_stereo_shape_budget(160, 177, 8_192),
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
