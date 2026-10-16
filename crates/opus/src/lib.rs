//! Clean-room Opus packet framing and decoder for Ogg Opus.
//!
//! The decoder is under development. No reference implementation code is
//! copied, translated, linked, or vendored.

mod packet;

pub use packet::{OpusPacket, PacketError, Toc};
