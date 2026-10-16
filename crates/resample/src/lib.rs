//! Streaming PCM sample-rate conversion using a windowed-sinc low-pass filter.

use std::collections::VecDeque;

use zemer_pcm::{AudioSpec, PcmBuffer, SampleFormat, Samples, convert_sample_format};

const RADIUS: i64 = 16;
const MAX_RATE_RATIO: u64 = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResampleError(pub &'static str);

impl std::fmt::Display for ResampleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for ResampleError {}

/// Converts a PCM stream while keeping only the filter's working window in memory.
/// Output is normalized Float64 PCM so callers choose their final sample format.
pub struct StreamingResampler {
    input_spec: AudioSpec,
    output_spec: AudioSpec,
    channels: usize,
    input_rate: u64,
    output_rate: u64,
    samples: VecDeque<f64>,
    buffer_start: u64,
    input_frames: u64,
    next_output: u64,
    finished: bool,
}

impl StreamingResampler {
    pub fn new(input_spec: AudioSpec, output_rate: u32) -> Result<Self, ResampleError> {
        if output_rate == 0 {
            return Err(ResampleError(
                "output sample rate must be greater than zero",
            ));
        }
        let input_rate = u64::from(input_spec.sample_rate());
        let output_rate_u64 = u64::from(output_rate);
        if input_rate > output_rate_u64.saturating_mul(MAX_RATE_RATIO)
            || output_rate_u64 > input_rate.saturating_mul(MAX_RATE_RATIO)
        {
            return Err(ResampleError(
                "sample-rate ratio exceeds the supported 64:1 bound",
            ));
        }
        let output_spec = AudioSpec::new_with_speaker_mask(
            output_rate,
            input_spec.channels(),
            SampleFormat::Float64,
            input_spec.speaker_mask(),
        )
        .map_err(|_| ResampleError("invalid output audio specification"))?;
        Ok(Self {
            input_spec,
            output_spec,
            channels: usize::from(input_spec.channels()),
            input_rate,
            output_rate: output_rate_u64,
            samples: VecDeque::new(),
            buffer_start: 0,
            input_frames: 0,
            next_output: 0,
            finished: false,
        })
    }

    pub fn output_spec(&self) -> AudioSpec {
        self.output_spec
    }

    /// Adds input frames and returns output frames ready without end-of-stream padding.
    pub fn push(&mut self, input: &PcmBuffer) -> Result<PcmBuffer, ResampleError> {
        if self.finished {
            return Err(ResampleError("cannot add input after resampler finish"));
        }
        if input.spec() != self.input_spec {
            return Err(ResampleError("input audio specification changed"));
        }
        let converted = convert_sample_format(input, SampleFormat::Float64)
            .map_err(|_| ResampleError("could not convert input samples to Float64"))?;
        let Samples::F64(values) = converted.samples() else {
            unreachable!("Float64 conversion returns Float64 samples")
        };
        if values.iter().any(|sample| !sample.is_finite()) {
            return Err(ResampleError("input contains a non-finite sample"));
        }
        let input_frames = self
            .input_frames
            .checked_add(input.frames() as u64)
            .ok_or(ResampleError("input frame count overflow"))?;
        self.input_frames = input_frames;
        if self.input_rate == self.output_rate {
            self.next_output = input_frames;
            return Ok(converted);
        }
        self.samples.extend(values.iter().copied());
        self.produce(false)
    }

    /// Flushes the filter using edge extension and returns the final output frames.
    pub fn finish(&mut self) -> Result<PcmBuffer, ResampleError> {
        if self.finished {
            return Err(ResampleError("resampler is already finished"));
        }
        self.finished = true;
        self.produce(true)
    }

    fn produce(&mut self, flush: bool) -> Result<PcmBuffer, ResampleError> {
        let target_frames = if flush {
            let numerator = u128::from(self.input_frames) * u128::from(self.output_rate);
            u64::try_from(
                (numerator + u128::from(self.input_rate / 2)) / u128::from(self.input_rate),
            )
            .map_err(|_| ResampleError("output frame count overflow"))?
        } else {
            u64::MAX
        };
        let cutoff = (self.output_rate as f64 / self.input_rate as f64).min(1.0) * 0.94;
        let mut output = Vec::new();
        while self.next_output < target_frames {
            let numerator = u128::from(self.next_output) * u128::from(self.input_rate);
            let position = numerator as f64 / self.output_rate as f64;
            let center = (numerator / u128::from(self.output_rate)) as u64;
            if !flush && center.saturating_add(RADIUS as u64) >= self.input_frames {
                break;
            }
            let mut tap_frames = [0_i64; (RADIUS * 2) as usize];
            let mut weights = [0.0; (RADIUS * 2) as usize];
            let mut weight_sum = 0.0;
            for (tap, frame) in (center as i64 - RADIUS + 1..=center as i64 + RADIUS).enumerate() {
                let distance = position - frame as f64;
                let normalized_distance = distance / RADIUS as f64;
                let window = if normalized_distance.abs() >= 1.0 {
                    0.0
                } else {
                    0.42 + 0.5 * (std::f64::consts::PI * normalized_distance).cos()
                        + 0.08 * (2.0 * std::f64::consts::PI * normalized_distance).cos()
                };
                let argument = distance * cutoff;
                let sinc = if argument.abs() < f64::EPSILON {
                    1.0
                } else {
                    (std::f64::consts::PI * argument).sin() / (std::f64::consts::PI * argument)
                };
                let weight = cutoff * sinc * window;
                tap_frames[tap] = frame;
                weights[tap] = weight;
                weight_sum += weight;
            }
            if !weight_sum.is_finite() || weight_sum.abs() < f64::EPSILON {
                return Err(ResampleError("invalid resampling filter weights"));
            }
            for channel in 0..self.channels {
                let mut weighted = 0.0;
                for tap in 0..weights.len() {
                    weighted += self.sample(tap_frames[tap], channel)? * weights[tap];
                }
                output.push(weighted / weight_sum);
            }
            self.next_output = self
                .next_output
                .checked_add(1)
                .ok_or(ResampleError("output frame count overflow"))?;
        }
        self.discard_old_samples();
        PcmBuffer::new(self.output_spec, Samples::F64(output))
            .map_err(|_| ResampleError("invalid resampler output block"))
    }

    fn sample(&self, frame: i64, channel: usize) -> Result<f64, ResampleError> {
        let frame = if frame < 0 {
            0
        } else if frame as u64 >= self.input_frames {
            self.input_frames.saturating_sub(1)
        } else {
            frame as u64
        };
        let offset = frame
            .checked_sub(self.buffer_start)
            .ok_or(ResampleError("resampler discarded a required input frame"))?;
        let index = usize::try_from(offset)
            .ok()
            .and_then(|offset| offset.checked_mul(self.channels))
            .and_then(|offset| offset.checked_add(channel))
            .ok_or(ResampleError("resampler buffer index overflow"))?;
        self.samples
            .get(index)
            .copied()
            .ok_or(ResampleError("resampler input window is incomplete"))
    }

    fn discard_old_samples(&mut self) {
        let numerator = u128::from(self.next_output) * u128::from(self.input_rate);
        let center = (numerator / u128::from(self.output_rate)) as u64;
        let keep_from = center.saturating_sub((RADIUS - 1) as u64);
        let discard_frames = keep_from.saturating_sub(self.buffer_start);
        let discard_samples = usize::try_from(discard_frames)
            .ok()
            .and_then(|frames| frames.checked_mul(self.channels))
            .unwrap_or(self.samples.len())
            .min(self.samples.len());
        self.samples.drain(..discard_samples);
        self.buffer_start += (discard_samples / self.channels) as u64;
    }
}

#[cfg(test)]
mod tests {
    use zemer_pcm::{AudioSpec, PcmBuffer, SampleFormat, Samples};

    use super::StreamingResampler;

    fn audio(rate: u32, channels: u16, samples: Vec<i16>) -> PcmBuffer {
        PcmBuffer::new(
            AudioSpec::new(rate, channels, SampleFormat::PcmI16).unwrap(),
            Samples::I16(samples),
        )
        .unwrap()
    }

    fn collect(
        rate: u32,
        channels: u16,
        data: &[i16],
        block_frames: usize,
        output_rate: u32,
    ) -> Vec<f64> {
        let input = audio(rate, channels, data.to_vec());
        let Samples::I16(source) = input.samples() else {
            unreachable!()
        };
        let mut resampler = StreamingResampler::new(input.spec(), output_rate).unwrap();
        let mut output = Vec::new();
        let mut offset = 0;
        while offset < input.frames() {
            let count = block_frames.min(input.frames() - offset);
            let samples = source
                [offset * usize::from(channels)..(offset + count) * usize::from(channels)]
                .to_vec();
            let block = audio(rate, channels, samples);
            let converted = resampler.push(&block).unwrap();
            let Samples::F64(values) = converted.samples() else {
                unreachable!()
            };
            output.extend_from_slice(values);
            offset += count;
        }
        let converted = resampler.finish().unwrap();
        let Samples::F64(values) = converted.samples() else {
            unreachable!()
        };
        output.extend_from_slice(values);
        output
    }

    #[test]
    fn keeps_duration_and_channel_layout_for_up_and_down_sampling() {
        let input: Vec<_> = (0..441)
            .flat_map(|frame| [frame as i16, -(frame as i16)])
            .collect();
        let output = collect(44_100, 2, &input, 37, 48_000);
        assert_eq!(output.len(), 480 * 2);
        for frame in output.chunks_exact(2) {
            assert!((frame[0] + frame[1]).abs() < 1e-12);
        }
        assert_eq!(collect(48_000, 1, &vec![0; 480], 41, 44_100).len(), 441);
    }

    #[test]
    fn output_does_not_depend_on_input_block_boundaries() {
        let input: Vec<_> = (0..1_003)
            .map(|frame| (frame * 29 % 30_000) as i16 - 15_000)
            .collect();
        assert_eq!(
            collect(44_100, 1, &input, 1_003, 32_000),
            collect(44_100, 1, &input, 37, 32_000)
        );
    }

    #[test]
    fn preserves_constant_signal_and_rejects_spec_changes() {
        let source = audio(48_000, 2, vec![12_000; 400]);
        let mut resampler = StreamingResampler::new(source.spec(), 96_000).unwrap();
        let output = resampler.push(&source).unwrap();
        let tail = resampler.finish().unwrap();
        for block in [output, tail] {
            let Samples::F64(samples) = block.samples() else {
                unreachable!()
            };
            assert!(
                samples
                    .iter()
                    .all(|sample| (*sample - 12_000.0 / 32_768.0).abs() < 1e-12)
            );
        }
        assert!(resampler.push(&source).is_err());
    }

    #[test]
    fn output_spec_preserves_channels_and_speaker_mask() {
        let spec = AudioSpec::new_with_speaker_mask(44_100, 2, SampleFormat::PcmI16, Some(0x0003))
            .unwrap();
        let resampler = StreamingResampler::new(spec, 48_000).unwrap();
        assert_eq!(resampler.output_spec().sample_rate(), 48_000);
        assert_eq!(resampler.output_spec().channels(), 2);
        assert_eq!(resampler.output_spec().speaker_mask(), Some(0x0003));
        assert_eq!(
            resampler.output_spec().sample_format(),
            SampleFormat::Float64
        );
    }

    #[test]
    fn equal_rate_conversion_preserves_samples_without_filtering() {
        let input = audio(48_000, 1, vec![i16::MIN, -1, 0, i16::MAX]);
        let mut resampler = StreamingResampler::new(input.spec(), 48_000).unwrap();
        let output = resampler.push(&input).unwrap();
        let Samples::F64(samples) = output.samples() else {
            unreachable!()
        };
        assert_eq!(
            *samples,
            vec![-1.0, -1.0 / 32_768.0, 0.0, 32_767.0 / 32_768.0]
        );
        assert_eq!(resampler.finish().unwrap().frames(), 0);
    }

    #[test]
    fn rejects_non_finite_float_samples() {
        let spec = AudioSpec::new(48_000, 1, SampleFormat::Float64).unwrap();
        let source = PcmBuffer::new(spec, Samples::F64(vec![f64::NAN])).unwrap();
        let mut resampler = StreamingResampler::new(spec, 44_100).unwrap();
        assert!(resampler.push(&source).is_err());
    }

    #[test]
    fn low_pass_filter_rejects_frequencies_above_output_nyquist() {
        let input: Vec<_> = (0..4_800)
            .map(|frame| {
                ((std::f64::consts::TAU * 12_000.0 * frame as f64 / 48_000.0).sin() * 30_000.0)
                    as i16
            })
            .collect();
        let output = collect(48_000, 1, &input, 257, 24_000);
        let rms =
            (output.iter().map(|sample| sample * sample).sum::<f64>() / output.len() as f64).sqrt();
        assert!(rms < 0.01, "aliased RMS was {rms}");
    }

    #[test]
    fn passband_tone_keeps_its_amplitude() {
        let input: Vec<_> = (0..4_800)
            .map(|frame| {
                ((std::f64::consts::TAU * 1_000.0 * frame as f64 / 48_000.0).sin() * 20_000.0)
                    as i16
            })
            .collect();
        let output = collect(48_000, 1, &input, 257, 24_000);
        let stable = &output[32..output.len() - 32];
        let rms =
            (stable.iter().map(|sample| sample * sample).sum::<f64>() / stable.len() as f64).sqrt();
        let expected = (20_000.0 / 32_768.0) / 2.0_f64.sqrt();
        assert!((rms - expected).abs() < 0.002, "passband RMS was {rms}");
    }

    #[test]
    fn rejects_extreme_sample_rate_ratios() {
        let spec = AudioSpec::new(1, 1, SampleFormat::PcmI16).unwrap();
        assert!(StreamingResampler::new(spec, 65).is_err());
    }
}
