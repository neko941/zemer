//! Core Audio Format (CAF) reader and writer for Zemer.

mod decode;
mod encode;
mod info;
mod rewrite;

pub use decode::{CafCodec, CafDecoder};
pub use encode::CafEncoder;
pub use rewrite::rewrite_metadata;
