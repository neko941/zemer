//! Shared PCM sample representations and exact sample-format conversion.

mod buffer;
mod f32;
mod f64;
mod i16;
mod i24;
mod i32;
mod sample;
mod sample_conversion;
mod samples;
mod spec;
mod u8;

pub use crate::buffer::PcmBuffer;
pub use crate::sample::SampleFormat;
pub use crate::sample_conversion::convert_sample_format;
pub use crate::samples::Samples;
pub use crate::spec::AudioSpec;

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmError(pub &'static str);

impl fmt::Display for PcmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for PcmError {}
