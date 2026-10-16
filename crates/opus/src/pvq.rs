use crate::range::{RangeDecoder, RangeError};

const MAX_PVQ_DIMENSIONS: usize = 176;
const MAX_PVQ_PULSES: usize = 1024;

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

    use super::{codebook_size, decode_codeword, normalize_pulses, unrank_codeword};
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
}
