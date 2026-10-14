use std::f64::consts::PI;

use zemer_core::AudioError;

/// Computes the forward MDCT with an in-house Bluestein convolution.
pub(crate) fn forward_mdct(samples: &[f64]) -> Result<Vec<f64>, AudioError> {
    let bins = samples.len() / 2;
    if samples.len() != bins * 2 || !(32..=4096).contains(&bins) || !bins.is_power_of_two() {
        return Err(AudioError::InvalidData(
            "invalid Vorbis forward MDCT input length",
        ));
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(AudioError::InvalidData(
            "non-finite Vorbis forward MDCT input",
        ));
    }

    let transform_len = bins
        .checked_mul(4)
        .ok_or(AudioError::InvalidData("Vorbis forward MDCT size overflow"))?;
    let convolution_len = transform_len;
    let mut left = zeroed_complex(convolution_len)?;
    let mut right = zeroed_complex(convolution_len)?;
    let mut folded = zeroed_f64(bins)?;
    let mut rotated = zeroed_f64(samples.len())?;
    let phase = std::f64::consts::PI / bins as f64;

    for (index, &sample) in samples.iter().enumerate() {
        let shifted = index + bins / 2;
        let wrapped = shifted >= bins * 2;
        let destination = shifted % (bins * 2);
        rotated[destination] += if wrapped { -sample } else { sample };
    }
    for index in 0..bins {
        folded[index] = rotated[index] - rotated[bins * 2 - 1 - index];
    }

    for (index, &sample) in folded.iter().enumerate() {
        let index = index as f64;
        let angle = phase * (index * index + index) / 2.0;
        left[index as usize] = (sample * angle.cos(), sample * angle.sin());
    }
    for (offset, kernel) in right.iter_mut().take(bins * 2 - 1).enumerate() {
        let difference = offset as isize - (bins as isize - 1);
        let difference_float = difference as f64;
        let angle = -phase * difference_float * difference_float / 2.0;
        *kernel = (angle.cos(), angle.sin());
    }

    fft(&mut left, false);
    fft(&mut right, false);
    for (left, right) in left.iter_mut().zip(right) {
        *left = complex_mul(*left, right);
    }
    fft(&mut left, true);

    let scale = 2.0 / bins as f64;
    let mut output = Vec::new();
    output
        .try_reserve_exact(bins)
        .map_err(|_| AudioError::InvalidData("Vorbis forward MDCT allocation failed"))?;
    for index in 0..bins {
        let index_float = index as f64;
        let angle = phase * (index_float * index_float + index_float) / 2.0 + phase / 4.0;
        let convolved = left[index + bins - 1];
        let value = scale * (convolved.0 * angle.cos() - convolved.1 * angle.sin());
        if !value.is_finite() {
            return Err(AudioError::InvalidData(
                "non-finite Vorbis forward MDCT output",
            ));
        }
        output.push(value);
    }
    Ok(output)
}

/// Computes the inverse MDCT with a Bluestein convolution over a radix-2 FFT.
pub(crate) fn inverse_mdct(spectrum: &[f64]) -> Result<Vec<f64>, AudioError> {
    let bins = spectrum.len();
    if bins < 32 || !bins.is_power_of_two() {
        return Err(AudioError::InvalidData(
            "invalid Vorbis inverse MDCT spectrum length",
        ));
    }
    if spectrum.iter().any(|sample| !sample.is_finite()) {
        return Err(AudioError::InvalidData(
            "non-finite Vorbis inverse MDCT input",
        ));
    }

    let transform_len = bins
        .checked_mul(4)
        .ok_or(AudioError::InvalidData("Vorbis inverse MDCT size overflow"))?;
    let mut left = zeroed_complex(transform_len)?;
    let mut right = zeroed_complex(transform_len)?;
    let phase = PI / (2.0 * bins as f64);

    for (index, &value) in spectrum.iter().enumerate() {
        let half_index = index as f64 + 0.5;
        let angle = phase * half_index * half_index;
        left[index] = (value * angle.cos(), value * angle.sin());
    }

    let offset = bins / 2;
    for (index, value) in right.iter_mut().take(bins * 3).enumerate() {
        let difference = index as isize - offset as isize;
        let angle = -phase * (difference as f64).powi(2);
        *value = (angle.cos(), angle.sin());
    }

    fft(&mut left, false);
    fft(&mut right, false);
    for (left, right) in left.iter_mut().zip(right) {
        *left = complex_mul(*left, right);
    }
    fft(&mut left, true);

    let mut output = Vec::new();
    output
        .try_reserve_exact(bins * 2)
        .map_err(|_| AudioError::InvalidData("Vorbis inverse MDCT allocation failed"))?;
    for sample in 0..bins * 2 {
        let shifted = sample as f64 + offset as f64 + 0.5;
        let angle = phase * shifted * shifted;
        let convolved = left[sample + bins];
        let value = convolved.0 * angle.cos() - convolved.1 * angle.sin();
        if !value.is_finite() {
            return Err(AudioError::InvalidData(
                "non-finite Vorbis inverse MDCT output",
            ));
        }
        output.push(value);
    }
    Ok(output)
}

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

fn zeroed_complex(length: usize) -> Result<Vec<(f64, f64)>, AudioError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| AudioError::InvalidData("Vorbis inverse MDCT allocation failed"))?;
    values.resize(length, (0.0, 0.0));
    Ok(values)
}

fn zeroed_f64(length: usize) -> Result<Vec<f64>, AudioError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| AudioError::InvalidData("Vorbis MDCT allocation failed"))?;
    values.resize(length, 0.0);
    Ok(values)
}

fn complex_mul(left: (f64, f64), right: (f64, f64)) -> (f64, f64) {
    (
        left.0 * right.0 - left.1 * right.1,
        left.0 * right.1 + left.1 * right.0,
    )
}

fn fft(values: &mut [(f64, f64)], inverse: bool) {
    let length = values.len();
    let mut reversed = 0;
    for index in 1..length {
        let mut bit = length >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if index < reversed {
            values.swap(index, reversed);
        }
    }

    let direction = if inverse { 1.0 } else { -1.0 };
    let mut width = 2;
    while width <= length {
        let angle = direction * 2.0 * PI / width as f64;
        let root = (angle.cos(), angle.sin());
        for start in (0..length).step_by(width) {
            let (mut twiddle_real, mut twiddle_imaginary) = (1.0, 0.0);
            let half = width / 2;
            for offset in 0..half {
                let even = values[start + offset];
                let odd = complex_mul(
                    values[start + offset + half],
                    (twiddle_real, twiddle_imaginary),
                );
                values[start + offset] = (even.0 + odd.0, even.1 + odd.1);
                values[start + offset + half] = (even.0 - odd.0, even.1 - odd.1);
                (twiddle_real, twiddle_imaginary) =
                    complex_mul((twiddle_real, twiddle_imaginary), root);
            }
        }
        width *= 2;
    }

    if inverse {
        let scale = 1.0 / length as f64;
        for value in values {
            value.0 *= scale;
            value.1 *= scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_window, forward_mdct, inverse_mdct};

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
    fn forward_mdct_rejects_non_vorbis_block_sizes() {
        assert!(forward_mdct(&[0.0; 63]).is_err());
        assert!(forward_mdct(&vec![0.0; 16_384]).is_err());
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
