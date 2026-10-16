use std::f64::consts::PI;

use crate::AudioError;

const MAX_MDCT_BINS: usize = 8192;

/// Computes a forward MDCT over a window containing twice as many samples as bins.
/// Bin counts must be even and no greater than 8192.
pub fn forward_mdct(samples: &[f64]) -> Result<Vec<f64>, AudioError> {
    let bins = samples.len() / 2;
    if samples.len() != bins * 2 || !(2..=MAX_MDCT_BINS).contains(&bins) || bins % 2 != 0 {
        return Err(AudioError::InvalidData("invalid forward MDCT input length"));
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(AudioError::InvalidData("non-finite forward MDCT input"));
    }

    let transform_len = bins
        .checked_mul(4)
        .and_then(usize::checked_next_power_of_two)
        .ok_or(AudioError::InvalidData("forward MDCT size overflow"))?;
    let mut left = zeroed_complex(transform_len)?;
    let mut right = zeroed_complex(transform_len)?;
    let mut folded = zeroed_f64(bins)?;
    let mut rotated = zeroed_f64(samples.len())?;
    let phase = PI / bins as f64;

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
        .map_err(|_| AudioError::InvalidData("forward MDCT allocation failed"))?;
    for index in 0..bins {
        let index_float = index as f64;
        let angle = phase * (index_float * index_float + index_float) / 2.0 + phase / 4.0;
        let convolved = left[index + bins - 1];
        let value = scale * (convolved.0 * angle.cos() - convolved.1 * angle.sin());
        if !value.is_finite() {
            return Err(AudioError::InvalidData("non-finite forward MDCT output"));
        }
        output.push(value);
    }
    Ok(output)
}

/// Computes the inverse MDCT into a window twice the spectrum length.
/// Spectrum lengths must be even and no greater than 8192.
pub fn inverse_mdct(spectrum: &[f64]) -> Result<Vec<f64>, AudioError> {
    let bins = spectrum.len();
    if !(2..=MAX_MDCT_BINS).contains(&bins) || bins % 2 != 0 {
        return Err(AudioError::InvalidData(
            "invalid inverse MDCT spectrum length",
        ));
    }
    if spectrum.iter().any(|sample| !sample.is_finite()) {
        return Err(AudioError::InvalidData("non-finite inverse MDCT input"));
    }

    let transform_len = bins
        .checked_mul(4)
        .and_then(usize::checked_next_power_of_two)
        .ok_or(AudioError::InvalidData("inverse MDCT size overflow"))?;
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
        .map_err(|_| AudioError::InvalidData("inverse MDCT allocation failed"))?;
    for sample in 0..bins * 2 {
        let shifted = sample as f64 + offset as f64 + 0.5;
        let angle = phase * shifted * shifted;
        let convolved = left[sample + bins];
        let value = convolved.0 * angle.cos() - convolved.1 * angle.sin();
        if !value.is_finite() {
            return Err(AudioError::InvalidData("non-finite inverse MDCT output"));
        }
        output.push(value);
    }
    Ok(output)
}

fn zeroed_complex(length: usize) -> Result<Vec<(f64, f64)>, AudioError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| AudioError::InvalidData("MDCT allocation failed"))?;
    values.resize(length, (0.0, 0.0));
    Ok(values)
}

fn zeroed_f64(length: usize) -> Result<Vec<f64>, AudioError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| AudioError::InvalidData("MDCT allocation failed"))?;
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
    use super::{forward_mdct, inverse_mdct};

    #[test]
    fn forward_mdct_matches_direct_cosine_sum_for_arbitrary_lengths() {
        for bins in [32, 48, 120, 240, 480, 960] {
            let samples: Vec<f64> = (0..bins * 2)
                .map(|index| ((index * 29 % 83) as f64 - 41.0) / 23.0)
                .collect();
            let actual = forward_mdct(&samples).unwrap();
            for (coefficient, &value) in actual.iter().enumerate() {
                let coefficient = coefficient as f64 + 0.5;
                let expected = 2.0 / bins as f64
                    * samples
                        .iter()
                        .enumerate()
                        .map(|(index, &sample)| {
                            let coordinate = index as f64 + 0.5 + bins as f64 / 2.0;
                            sample
                                * (std::f64::consts::PI / bins as f64 * coordinate * coefficient)
                                    .cos()
                        })
                        .sum::<f64>();
                assert!((value - expected).abs() < 1e-10);
            }
        }
    }

    #[test]
    fn inverse_mdct_matches_direct_cosine_sum_for_arbitrary_lengths() {
        for bins in [32, 48, 120, 240, 480, 960] {
            let spectrum: Vec<f64> = (0..bins)
                .map(|index| ((index * 37 % 101) as f64 - 50.0) / 17.0)
                .collect();
            let actual = inverse_mdct(&spectrum).unwrap();
            for (sample, &value) in actual.iter().enumerate() {
                let coordinate = sample as f64 + 0.5 + bins as f64 / 2.0;
                let expected = spectrum
                    .iter()
                    .enumerate()
                    .map(|(index, &coefficient)| {
                        coefficient
                            * (std::f64::consts::PI / bins as f64
                                * coordinate
                                * (index as f64 + 0.5))
                                .cos()
                    })
                    .sum::<f64>();
                assert!(
                    (value - expected).abs() < 1e-9,
                    "bins={bins}, sample={sample}, actual={value}, expected={expected}, error={}",
                    (value - expected).abs()
                );
            }
        }
    }

    #[test]
    fn mdct_rejects_non_finite_and_oversized_inputs() {
        assert!(forward_mdct(&[0.0; 3]).is_err());
        assert!(forward_mdct(&[f64::NAN; 64]).is_err());
        assert!(forward_mdct(&vec![0.0; 32_768]).is_err());
        assert!(inverse_mdct(&[f64::INFINITY; 32]).is_err());
        assert!(inverse_mdct(&vec![0.0; 8193]).is_err());
    }
}
