//! Clean-room Opus packet framing and decoder for Ogg Opus.
//!
//! The decoder is under development. No reference implementation code is
//! copied, translated, linked, or vendored.

#[allow(dead_code)]
mod celt;
mod ogg;
mod packet;
#[allow(dead_code)]
mod postfilter;
#[allow(dead_code)]
mod pvq;
// These primitives are staged for the decoder frame pipeline.
#[allow(dead_code)]
mod range;
mod stream;

pub use ogg::{OpusHead, OpusHeadError, OpusTags, OpusTagsError};
pub use packet::{Bandwidth, Mode, OpusPacket, PacketError, Toc, parse_multistream_packet};
pub use stream::{OpusAudioPacket, OpusStreamReader};
