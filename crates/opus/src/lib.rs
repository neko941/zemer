//! Clean-room Opus packet framing and decoder for Ogg Opus.
//!
//! The decoder is under development. No reference implementation code is
//! copied, translated, linked, or vendored.

#[allow(dead_code)]
mod celt;
mod ogg;
mod packet;
#[allow(dead_code)]
mod pvq;
// These primitives are staged for the decoder frame pipeline.
#[allow(dead_code)]
mod range;

pub use ogg::{OpusHead, OpusHeadError};
pub use packet::{Bandwidth, Mode, OpusPacket, PacketError, Toc};
