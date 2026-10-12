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
}
