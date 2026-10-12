//! Shared native DSD byte-stream types.

mod buffer;
mod spec;

pub use buffer::DsdBuffer;
pub use spec::DsdSpec;

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DsdError(pub &'static str);

impl fmt::Display for DsdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for DsdError {}

#[cfg(test)]
mod tests {
    use super::{DsdBuffer, DsdSpec};

    #[test]
    fn dsd_buffer_counts_interleaved_channel_octets() {
        let spec = DsdSpec::new(2_822_400, 2).unwrap();
        let audio = DsdBuffer::new(spec, vec![0x80, 0x7f, 0x55, 0xaa]).unwrap();
        assert_eq!(audio.frames(), 2);
        assert_eq!(audio.bytes(), [0x80, 0x7f, 0x55, 0xaa]);
    }

    #[test]
    fn rejects_partial_channel_octet_frames() {
        let spec = DsdSpec::new(2_822_400, 2).unwrap();
        assert!(DsdBuffer::new(spec, vec![0x80]).is_err());
    }
}
