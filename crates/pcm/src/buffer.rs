use crate::{AudioSpec, PcmError, Samples};

#[derive(Clone, Debug, PartialEq)]
pub struct PcmBuffer {
    spec: AudioSpec,
    samples: Samples,
}

impl PcmBuffer {
    pub fn new(spec: AudioSpec, samples: Samples) -> Result<Self, PcmError> {
        if !samples.matches(spec.sample_format()) {
            return Err(PcmError(
                "sample storage does not match the declared sample format",
            ));
        }
        if samples.len() % usize::from(spec.channels()) != 0 {
            return Err(PcmError("sample count is not a whole number of frames"));
        }
        if !samples.valid() {
            return Err(PcmError(
                "24-bit PCM sample is outside the signed 24-bit range",
            ));
        }
        Ok(Self { spec, samples })
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.spec.channels())
    }

    pub fn spec(&self) -> AudioSpec {
        self.spec
    }
    pub fn samples(&self) -> &Samples {
        &self.samples
    }

    pub fn truncate_frames(&mut self, frames: usize) -> Result<(), PcmError> {
        if frames > self.frames() {
            return Err(PcmError("cannot extend a PCM buffer by truncating it"));
        }
        let sample_count = frames
            .checked_mul(usize::from(self.spec.channels()))
            .ok_or(PcmError("PCM sample count overflow"))?;
        match &mut self.samples {
            Samples::U8(values) => values.truncate(sample_count),
            Samples::I16(values) => values.truncate(sample_count),
            Samples::I24(values) | Samples::I32(values) => values.truncate(sample_count),
            Samples::F32(values) => values.truncate(sample_count),
            Samples::F64(values) => values.truncate(sample_count),
        }
        Ok(())
    }

    pub fn take_prefix_frames(&mut self, frames: usize) -> Result<Self, PcmError> {
        if frames > self.frames() {
            return Err(PcmError(
                "cannot take more frames than the PCM buffer contains",
            ));
        }
        let sample_count = frames
            .checked_mul(usize::from(self.spec.channels()))
            .ok_or(PcmError("PCM sample count overflow"))?;
        let prefix = match &mut self.samples {
            Samples::U8(values) => Samples::U8(take_prefix(values, sample_count)),
            Samples::I16(values) => Samples::I16(take_prefix(values, sample_count)),
            Samples::I24(values) => Samples::I24(take_prefix(values, sample_count)),
            Samples::I32(values) => Samples::I32(take_prefix(values, sample_count)),
            Samples::F32(values) => Samples::F32(take_prefix(values, sample_count)),
            Samples::F64(values) => Samples::F64(take_prefix(values, sample_count)),
        };
        Ok(Self {
            spec: self.spec,
            samples: prefix,
        })
    }
}

fn take_prefix<T>(values: &mut Vec<T>, count: usize) -> Vec<T> {
    let remainder = values.split_off(count);
    std::mem::replace(values, remainder)
}

#[cfg(test)]
mod tests {
    use crate::{AudioSpec, SampleFormat, Samples};

    use super::PcmBuffer;

    #[test]
    fn truncates_whole_interleaved_frames_and_rejects_extension() {
        let spec = AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap();
        let mut buffer = PcmBuffer::new(spec, Samples::I16(vec![1, 2, 3, 4, 5, 6])).unwrap();
        buffer.truncate_frames(2).unwrap();
        assert_eq!(buffer.frames(), 2);
        assert_eq!(buffer.samples(), &Samples::I16(vec![1, 2, 3, 4]));
        assert!(buffer.truncate_frames(3).is_err());
    }

    #[test]
    fn takes_prefix_frames_and_keeps_the_remainder_interleaved() {
        let spec = AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap();
        let mut buffer = PcmBuffer::new(spec, Samples::I16(vec![1, 10, 2, 20, 3, 30])).unwrap();
        let prefix = buffer.take_prefix_frames(2).unwrap();
        assert_eq!(prefix.samples(), &Samples::I16(vec![1, 10, 2, 20]));
        assert_eq!(buffer.samples(), &Samples::I16(vec![3, 30]));
        assert!(buffer.take_prefix_frames(2).is_err());
    }
}
