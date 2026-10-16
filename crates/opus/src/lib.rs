//! Clean-room Opus packet framing and decoder for Ogg Opus.
//!
//! The decoder is under development. No reference implementation code is
//! copied, translated, linked, or vendored.

mod packet;
// These primitives are staged for the decoder frame pipeline.
#[allow(dead_code)]
mod range;

pub use packet::{OpusPacket, PacketError, Toc};
