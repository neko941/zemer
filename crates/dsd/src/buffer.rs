use crate::{DsdError, DsdSpec};

/// Packed native DSD bytes in channel-interleaved order, most-significant bit
/// first within each byte. One frame contains one octet from every channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DsdBuffer {
    spec: DsdSpec,
    bytes: Vec<u8>,
}

impl DsdBuffer {
    pub fn new(spec: DsdSpec, bytes: Vec<u8>) -> Result<Self, DsdError> {
        if bytes.len() % usize::from(spec.channels()) != 0 {
            return Err(DsdError("DSD byte count is not a whole number of frames"));
        }
        Ok(Self { spec, bytes })
    }

    pub fn spec(&self) -> DsdSpec {
        self.spec
    }

    /// Number of packed-octet frames; each frame represents eight DSD bit samples.
    pub fn frames(&self) -> usize {
        self.bytes.len() / usize::from(self.spec.channels())
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}
