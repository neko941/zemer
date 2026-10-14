use std::cmp::Ordering;
use std::collections::BinaryHeap;

use zemer_core::AudioError;

pub(crate) const QUANTIZER_BITS: [u8; 4] = [2, 4, 8, 12];
pub(crate) const PARTITION_SIZE: usize = 16;

pub(crate) type DistortionLevels = [f64; QUANTIZER_BITS.len() + 1];

#[derive(Clone, Copy, Debug)]
struct Upgrade {
    ratio: f64,
    partition: usize,
    from_level: u8,
    level: u8,
    cost: usize,
}

impl PartialEq for Upgrade {
    fn eq(&self, other: &Self) -> bool {
        self.ratio.total_cmp(&other.ratio) == Ordering::Equal
            && self.partition == other.partition
            && self.from_level == other.from_level
            && self.level == other.level
            && self.cost == other.cost
    }
}

impl Eq for Upgrade {}

impl PartialOrd for Upgrade {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Upgrade {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ratio
            .total_cmp(&other.ratio)
            .then_with(|| other.partition.cmp(&self.partition))
            .then_with(|| other.level.cmp(&self.level))
    }
}

/// Allocates increasing scalar precision to the partitions with the greatest
/// squared-error reduction per additional coded bit.
pub(crate) fn allocate_levels(
    distortions: &[DistortionLevels],
    residue_bit_budget: usize,
) -> Result<Vec<u8>, AudioError> {
    let mut levels = Vec::new();
    levels
        .try_reserve_exact(distortions.len())
        .map_err(|_| AudioError::InvalidData("Vorbis allocation result allocation failed"))?;
    levels.resize(distortions.len(), 0);

    let mut upgrades = BinaryHeap::new();
    let quantizer_count = QUANTIZER_BITS.len();
    let upgrades_per_partition = quantizer_count
        .checked_mul(quantizer_count + 1)
        .and_then(|count| count.checked_div(2))
        .ok_or(AudioError::InvalidData(
            "Vorbis rate allocator size overflow",
        ))?;
    upgrades
        .try_reserve_exact(
            distortions
                .len()
                .checked_mul(upgrades_per_partition)
                .ok_or(AudioError::InvalidData(
                    "Vorbis rate allocator size overflow",
                ))?,
        )
        .map_err(|_| AudioError::InvalidData("Vorbis rate allocator allocation failed"))?;
    for (partition, distortion) in distortions.iter().enumerate() {
        push_upgrades(partition, 0, distortion, &mut upgrades);
    }

    let mut remaining = residue_bit_budget;
    while let Some(upgrade) = upgrades.pop() {
        if levels[upgrade.partition] != upgrade.from_level {
            continue;
        }
        if upgrade.cost > remaining {
            continue;
        }
        levels[upgrade.partition] = upgrade.level;
        remaining -= upgrade.cost;
        push_upgrades(
            upgrade.partition,
            upgrade.level,
            &distortions[upgrade.partition],
            &mut upgrades,
        );
    }
    Ok(levels)
}

fn push_upgrades(
    partition: usize,
    from_level: u8,
    distortion: &DistortionLevels,
    upgrades: &mut BinaryHeap<Upgrade>,
) {
    let previous_bits = if from_level == 0 {
        0
    } else {
        QUANTIZER_BITS[usize::from(from_level - 1)]
    };
    for level in from_level + 1..=QUANTIZER_BITS.len() as u8 {
        let added_bits = QUANTIZER_BITS[usize::from(level - 1)] - previous_bits;
        let improvement = distortion[usize::from(from_level)] - distortion[usize::from(level)];
        if improvement.is_finite() && improvement > 0.0 {
            upgrades.push(Upgrade {
                ratio: improvement / (f64::from(added_bits) * PARTITION_SIZE as f64),
                partition,
                from_level,
                level,
                cost: usize::from(added_bits) * PARTITION_SIZE,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::allocate_levels;

    #[test]
    fn allocates_the_available_bits_to_the_largest_distortion_reduction() {
        let distortions = [[10.0, 1.0, 0.7, 0.6, 0.5], [1.0, 0.9, 0.8, 0.7, 0.6]];
        let levels = allocate_levels(&distortions, 32).unwrap();
        assert_eq!(levels, [1, 0]);
    }

    #[test]
    fn upgrades_follow_each_partition_precision_sequence() {
        let distortions = [[100.0, 90.0, 0.0, 0.0, 0.0]];
        let levels = allocate_levels(&distortions, 64).unwrap();
        assert_eq!(levels, [2]);
    }

    #[test]
    fn does_not_spend_bits_when_a_quantizer_cannot_reduce_error() {
        let distortions = [[1.0, 1.0, 1.0, 1.0, 1.0]];
        let levels = allocate_levels(&distortions, 10_000).unwrap();
        assert_eq!(levels, [0]);
    }

    #[test]
    fn can_choose_a_finer_quantizer_when_coarse_levels_do_not_help() {
        let distortions = [[10.0, 10.0, 10.0, 0.0, 0.0]];
        let levels = allocate_levels(&distortions, 128).unwrap();
        assert_eq!(levels, [3]);
    }

    #[test]
    fn can_allocate_twelve_bit_precision_when_the_budget_allows_it() {
        let distortions = [[10.0, 10.0, 10.0, 10.0, 0.0]];
        let levels = allocate_levels(&distortions, 192).unwrap();
        assert_eq!(levels, [4]);
    }
}
