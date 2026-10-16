use std::f64::consts::PI;

use zemer_core::AudioError;

/// Applies the Vorbis window, including short-window transitions on long blocks.
pub(crate) fn apply_window(
    samples: &mut [f64],
    short_block_size: usize,
    previous_window_long: bool,
    next_window_long: bool,
) -> Result<(), AudioError> {
    let block_size = samples.len();
    if !(64..=8192).contains(&block_size)
        || !block_size.is_power_of_two()
        || !(64..=block_size).contains(&short_block_size)
        || !short_block_size.is_power_of_two()
        || block_size % 4 != 0
        || short_block_size % 4 != 0
    {
        return Err(AudioError::InvalidData("invalid Vorbis window block size"));
    }
    let long_block = block_size != short_block_size;
    let left_transition = long_block && !previous_window_long;
    let right_transition = long_block && !next_window_long;

    let left_start = if left_transition {
        block_size / 4 - short_block_size / 4
    } else {
        0
    };
    let left_end = if left_transition {
        block_size / 4 + short_block_size / 4
    } else {
        block_size / 2
    };
    let left_length = left_end - left_start;

    let right_start = if right_transition {
        block_size * 3 / 4 - short_block_size / 4
    } else {
        block_size / 2
    };
    let right_end = if right_transition {
        block_size * 3 / 4 + short_block_size / 4
    } else {
        block_size
    };
    let right_length = right_end - right_start;

    for (index, sample) in samples.iter_mut().enumerate() {
        let window = if index < left_start || index >= right_end {
            0.0
        } else if index < left_end {
            vorbis_slope(index - left_start, left_length, false)
        } else if index < right_start {
            1.0
        } else {
            vorbis_slope(index - right_start, right_length, true)
        };
        *sample *= window;
        if !sample.is_finite() {
            return Err(AudioError::InvalidData("non-finite Vorbis window output"));
        }
    }
    Ok(())
}

fn vorbis_slope(index: usize, length: usize, falling: bool) -> f64 {
    let position = (index as f64 + 0.5) / length as f64 * (PI / 2.0);
    let inner = if falling {
        position + PI / 2.0
    } else {
        position
    };
    (PI / 2.0 * inner.sin().powi(2)).sin()
}

#[cfg(test)]
mod tests {
    use super::apply_window;
    use zemer_core::transform::{forward_mdct, inverse_mdct};

    #[test]
    fn analysis_and_synthesis_windows_reconstruct_one_hop() {
        for bins in [32, 4096] {
            let input = |frame: isize| {
                (0.4 * (frame as f64 * 0.017).sin()) + (0.2 * (frame as f64 * 0.031).cos())
            };
            let mut blocks = Vec::new();
            for center in [0, bins as isize] {
                let mut block: Vec<f64> = (0..bins * 2)
                    .map(|index| input(center - bins as isize + index as isize))
                    .collect();
                apply_window(&mut block, bins * 2, true, true).unwrap();
                let mut synthesized = inverse_mdct(&forward_mdct(&block).unwrap()).unwrap();
                apply_window(&mut synthesized, bins * 2, true, true).unwrap();
                blocks.push(synthesized);
            }
            for frame in 0..bins {
                let reconstructed = blocks[0][bins + frame] + blocks[1][frame];
                assert!(
                    (reconstructed - input(frame as isize)).abs() < 1e-9,
                    "block bins {bins}, frame {frame}: got {reconstructed}, expected {}",
                    input(frame as isize)
                );
            }
        }
    }

    #[test]
    fn fft_forward_mdct_matches_direct_cosine_sum() {
        for bins in [32, 64, 128] {
            let samples: Vec<f64> = (0..bins * 2)
                .map(|index| ((index * 29 % 83) as f64 - 41.0) / 23.0)
                .collect();
            let actual = forward_mdct(&samples).unwrap();
            let expected: Vec<f64> = (0..bins)
                .map(|coefficient| {
                    let coefficient = coefficient as f64 + 0.5;
                    2.0 / bins as f64
                        * samples
                            .iter()
                            .enumerate()
                            .map(|(index, &sample)| {
                                let coordinate = index as f64 + 0.5 + bins as f64 / 2.0;
                                sample
                                    * (std::f64::consts::PI / bins as f64
                                        * coordinate
                                        * coefficient)
                                        .cos()
                            })
                            .sum::<f64>()
                })
                .collect();
            for (actual, expected) in actual.iter().zip(expected) {
                assert!((actual - expected).abs() < 1e-10);
            }
        }
    }

    #[test]
    fn forward_mdct_rejects_odd_and_oversized_input() {
        assert!(forward_mdct(&[0.0; 63]).is_err());
        assert!(forward_mdct(&vec![0.0; 32_768]).is_err());
    }

    #[test]
    fn fft_inverse_mdct_matches_direct_cosine_sum() {
        for bins in [32, 64, 128] {
            let spectrum: Vec<f64> = (0..bins)
                .map(|index| ((index * 37 % 101) as f64 - 50.0) / 17.0)
                .collect();
            let actual = inverse_mdct(&spectrum).unwrap();
            let expected: Vec<f64> = (0..bins * 2)
                .map(|sample| {
                    let coordinate = sample as f64 + 0.5 + bins as f64 / 2.0;
                    spectrum
                        .iter()
                        .enumerate()
                        .map(|(index, &value)| {
                            value
                                * (std::f64::consts::PI / bins as f64
                                    * coordinate
                                    * (index as f64 + 0.5))
                                    .cos()
                        })
                        .sum::<f64>()
                })
                .collect();
            for (actual, expected) in actual.iter().zip(expected) {
                assert!((actual - expected).abs() < 1e-10);
            }
        }
    }

    #[test]
    fn windows_use_short_lobes_at_long_block_transitions() {
        let mut short = vec![1.0; 256];
        apply_window(&mut short, 256, false, false).unwrap();
        assert!(short[0] > 0.0 && short[0] < short[64]);
        assert!(short[127] > 0.99 && short[128] > 0.99);
        assert!(short[255] > 0.0 && short[255] < short[192]);

        let mut transition = vec![1.0; 2048];
        apply_window(&mut transition, 256, false, false).unwrap();
        assert_eq!(&transition[..448], vec![0.0; 448]);
        assert_eq!(&transition[1600..], vec![0.0; 448]);
        assert!(transition[448] > 0.0 && transition[448] < 1.0);
        assert!(transition[576..1472].iter().all(|&value| value == 1.0));
    }
}
