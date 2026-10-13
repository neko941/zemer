//! Clean-room MPEG-4 DST frame encoding and decoding.
//!
//! DST is a frame codec used by DSDIFF. Frames decode independently to
//! channel-major, MSB-first DSD octets; the encoder uses a first-order model
//! and callers can fall back to standard mode-0 frames when it is not useful.

mod bitstream;
mod decode;
mod encode;

pub use decode::decode_frame;
pub use encode::encode_frame;
