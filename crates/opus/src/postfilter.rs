use crate::celt::celt_low_overlap_window;
use crate::range::RangeError;

const HISTORY_SAMPLES: usize = 1_024;
const TRANSITION_SAMPLES: usize = 120;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PostFilter {
    pub(crate) period: u16,
    pub(crate) gain: u8,
    pub(crate) tapset: u8,
}

/// Stateful CELT pitch post-filter over channel-major frame samples.
#[derive(Default)]
pub(crate) struct CeltPostFilter {
    previous: Option<PostFilter>,
    initialized: bool,
    history: Vec<Vec<f32>>,
}

impl CeltPostFilter {
    pub(crate) fn reset(&mut self) {
        self.previous = None;
        self.initialized = false;
        self.history.clear();
    }

    /// Filters one channel-major PCM block and retains the prior filtered samples.
    pub(crate) fn process(
        &mut self,
        channels: &mut [Vec<f32>],
        current: Option<PostFilter>,
    ) -> Result<(), RangeError> {
        validate_parameters(current)?;
        let Some(frame_len) = channels.first().map(Vec::len) else {
            return Err(RangeError::InvalidCeltEnergyLayout);
        };
        if frame_len == 0
            || channels
                .iter()
                .any(|channel| channel.len() != frame_len || channel.iter().any(|x| !x.is_finite()))
            || (self.initialized && self.history.len() != channels.len())
        {
            return Err(RangeError::InvalidCeltEnergyLayout);
        }

        let old = if self.initialized {
            self.previous
        } else {
            current
        };
        let transitioning = old != current;
        if transitioning && frame_len < TRANSITION_SAMPLES {
            return Err(RangeError::InvalidCeltEnergyLayout);
        }
        let window = if transitioning {
            Some(celt_low_overlap_window(TRANSITION_SAMPLES)?)
        } else {
            None
        };
        let mut output = Vec::new();
        output
            .try_reserve_exact(channels.len())
            .map_err(|_| RangeError::CorruptFrame)?;
        let mut next_history = Vec::new();
        next_history
            .try_reserve_exact(channels.len())
            .map_err(|_| RangeError::CorruptFrame)?;

        for (channel_index, input) in channels.iter().enumerate() {
            let prior = self
                .history
                .get(channel_index)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let mut filtered = Vec::new();
            filtered
                .try_reserve_exact(prior.len().saturating_add(frame_len))
                .map_err(|_| RangeError::CorruptFrame)?;
            filtered.extend_from_slice(prior);
            let current_start = filtered.len();
            filtered.extend_from_slice(input);

            for index in 0..frame_len {
                let absolute_index = current_start + index;
                let sample = f64::from(filtered[absolute_index]);
                let value =
                    if let Some(window) = window.as_ref().filter(|_| index < TRANSITION_SAMPLES) {
                        let previous = filter_sample(&filtered, absolute_index, sample, old)?;
                        let next = filter_sample(&filtered, absolute_index, sample, current)?;
                        let weight = window[index] * window[index];
                        previous + (next - previous) * weight
                    } else {
                        filter_sample(&filtered, absolute_index, sample, current)?
                    };
                let value = value as f32;
                if !value.is_finite() {
                    return Err(RangeError::CorruptFrame);
                }
                filtered[absolute_index] = value;
            }

            let mut block = Vec::new();
            block
                .try_reserve_exact(frame_len)
                .map_err(|_| RangeError::CorruptFrame)?;
            block.extend_from_slice(&filtered[current_start..]);
            let history_start = filtered.len().saturating_sub(HISTORY_SAMPLES);
            let mut channel_history = Vec::new();
            channel_history
                .try_reserve_exact(filtered.len() - history_start)
                .map_err(|_| RangeError::CorruptFrame)?;
            channel_history.extend_from_slice(&filtered[history_start..]);
            next_history.push(channel_history);
            output.push(block);
        }

        for (destination, source) in channels.iter_mut().zip(output) {
            destination.copy_from_slice(&source);
        }
        self.history = next_history;
        self.previous = current;
        self.initialized = true;
        Ok(())
    }
}

fn validate_parameters(parameters: Option<PostFilter>) -> Result<(), RangeError> {
    if parameters.is_some_and(|value| {
        !(15..=1_022).contains(&value.period) || value.gain > 7 || value.tapset > 2
    }) {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    Ok(())
}

fn filter_sample(
    samples: &[f32],
    index: usize,
    input: f64,
    parameters: Option<PostFilter>,
) -> Result<f64, RangeError> {
    let Some(parameters) = parameters else {
        return Ok(input);
    };
    let taps = match parameters.tapset {
        0 => [0.306_640_625, 0.217_041_015_6, 0.129_638_671_9],
        1 => [0.463_867_187_5, 0.268_066_406_2, 0.0],
        2 => [0.799_804_687_5, 0.100_097_656_2, 0.0],
        _ => return Err(RangeError::InvalidCeltEnergyLayout),
    };
    let period = usize::from(parameters.period);
    let mut response = 0.0;
    for (coefficient, offset) in taps.into_iter().zip([0_usize, 1, 2]) {
        if coefficient == 0.0 {
            continue;
        }
        let delays = if offset == 0 {
            [Some(period), None]
        } else {
            [Some(period.saturating_sub(offset)), Some(period + offset)]
        };
        for delay in delays.into_iter().flatten() {
            let delayed = index
                .checked_sub(delay)
                .and_then(|position| samples.get(position))
                .copied()
                .unwrap_or(0.0);
            response += coefficient * f64::from(delayed);
        }
    }
    let gain = 3.0 * f64::from(parameters.gain + 1) / 32.0;
    Ok(input + gain * response)
}

#[cfg(test)]
mod tests {
    use super::{CeltPostFilter, PostFilter};
    use crate::range::RangeError;

    fn params(period: u16, gain: u8, tapset: u8) -> PostFilter {
        PostFilter {
            period,
            gain,
            tapset,
        }
    }

    #[test]
    fn disabled_filter_preserves_samples_and_tracks_channel_history() {
        let mut state = CeltPostFilter::default();
        let mut samples = vec![vec![0.25, -0.5, 1.0]];
        state.process(&mut samples, None).unwrap();
        assert_eq!(samples, [vec![0.25, -0.5, 1.0]]);
        assert_eq!(state.history, [vec![0.25, -0.5, 1.0]]);
    }

    #[test]
    fn applies_the_five_tap_pitch_response_from_prior_filtered_samples() {
        let mut state = CeltPostFilter::default();
        let params = params(15, 7, 0);
        let mut impulse = vec![vec![0.0; 120]];
        impulse[0][0] = 1.0;
        state.process(&mut impulse, Some(params)).unwrap();
        let gain = 3.0 * 8.0 / 32.0;
        assert!((impulse[0][15] - gain * 0.306_640_63).abs() < 1e-7);
        assert!((impulse[0][14] - gain * 0.217_041_02).abs() < 1e-7);
        assert!((impulse[0][16] - gain * 0.217_041_02).abs() < 1e-7);
    }

    #[test]
    fn parameter_changes_crossfade_for_one_window_then_use_new_filter() {
        let mut state = CeltPostFilter::default();
        let old = params(15, 7, 0);
        let new = None;
        let mut first = vec![vec![0.0; 120]];
        state.process(&mut first, Some(old)).unwrap();
        let mut next = vec![vec![0.0; 240]];
        next[0][0] = 1.0;
        state.process(&mut next, new).unwrap();

        assert_eq!(next[0][0], 1.0);
        assert!(next[0][14] > 0.0);
        assert!(next[0][119].is_finite());
        assert_eq!(next[0][120], 0.0);
    }

    #[test]
    fn invalid_input_does_not_consume_filter_state_or_change_samples() {
        let mut state = CeltPostFilter::default();
        let params = params(15, 7, 0);
        let mut first = vec![vec![0.5; 120]];
        state.process(&mut first, Some(params)).unwrap();
        let history = state.history.clone();
        let mut invalid = vec![vec![0.25; 119]];
        assert_eq!(
            state.process(&mut invalid, None),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(invalid, [vec![0.25; 119]]);
        assert_eq!(state.history, history);
        assert_eq!(state.previous, Some(params));
    }

    #[test]
    fn rejects_invalid_parameters_and_non_finite_audio() {
        let mut state = CeltPostFilter::default();
        let mut samples = vec![vec![f32::NAN; 120]];
        assert_eq!(
            state.process(&mut samples, Some(params(14, 0, 0))),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            state.process(&mut samples, None),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert!(samples[0].iter().all(|sample| sample.is_nan()));
    }
}
