use zemer_core::AudioError;

use crate::VorbisCouplingStep;

/// Converts decoded magnitude/angle residue vectors back to channel vectors.
pub(crate) fn inverse_couple(
    vectors: &mut [Vec<f64>],
    steps: &[VorbisCouplingStep],
) -> Result<(), AudioError> {
    if vectors
        .first()
        .is_some_and(|first| vectors.iter().any(|vector| vector.len() != first.len()))
    {
        return Err(AudioError::InvalidData(
            "Vorbis coupling vectors have different lengths",
        ));
    }
    for step in steps.iter().rev() {
        let magnitude_index = usize::from(step.magnitude);
        let angle_index = usize::from(step.angle);
        if magnitude_index == angle_index {
            return Err(AudioError::InvalidFormat("invalid Vorbis coupling pair"));
        }
        let (magnitude, angle) = if magnitude_index < angle_index {
            let (before_angle, from_angle) = vectors.split_at_mut(angle_index);
            (
                before_angle
                    .get_mut(magnitude_index)
                    .ok_or(AudioError::InvalidFormat(
                        "invalid Vorbis magnitude channel",
                    ))?,
                from_angle
                    .first_mut()
                    .ok_or(AudioError::InvalidFormat("invalid Vorbis angle channel"))?,
            )
        } else {
            let (before_magnitude, from_magnitude) = vectors.split_at_mut(magnitude_index);
            (
                from_magnitude.first_mut().ok_or(AudioError::InvalidFormat(
                    "invalid Vorbis magnitude channel",
                ))?,
                before_magnitude
                    .get_mut(angle_index)
                    .ok_or(AudioError::InvalidFormat("invalid Vorbis angle channel"))?,
            )
        };
        for (m, a) in magnitude.iter_mut().zip(angle) {
            let (new_m, new_a) = match (*m > 0.0, *a > 0.0) {
                (true, true) => (*m, *m - *a),
                (true, false) => (*m + *a, *m),
                (false, true) => (*m, *m + *a),
                (false, false) => (*m - *a, *m),
            };
            if !new_m.is_finite() || !new_a.is_finite() {
                return Err(AudioError::InvalidData(
                    "non-finite Vorbis inverse coupling value",
                ));
            }
            *m = new_m;
            *a = new_a;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::inverse_couple;
    use crate::VorbisCouplingStep;

    #[test]
    fn decouples_all_sign_quadrants_in_reverse_step_order() {
        let mut vectors = vec![vec![3.0, 3.0, -3.0, -3.0], vec![1.0, -1.0, 1.0, -1.0]];
        inverse_couple(
            &mut vectors,
            &[VorbisCouplingStep {
                magnitude: 0,
                angle: 1,
            }],
        )
        .unwrap();
        assert_eq!(vectors, [[3.0, 2.0, -3.0, -2.0], [2.0, 3.0, -2.0, -3.0]]);

        let mut chained = vec![vec![10.0], vec![4.0], vec![1.0]];
        inverse_couple(
            &mut chained,
            &[
                VorbisCouplingStep {
                    magnitude: 0,
                    angle: 1,
                },
                VorbisCouplingStep {
                    magnitude: 1,
                    angle: 2,
                },
            ],
        )
        .unwrap();
        assert_eq!(chained, [[10.0], [6.0], [3.0]]);
    }
}
