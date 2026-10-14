//! Clean-room Ogg Vorbis I decoder and experimental encoder built from Xiph's
//! bitstream specification.
//!
//! The decoder returns interleaved Float32 PCM and handles sequential Ogg
//! logical streams. The encoder offers a fixed scalar-residue profile and an
//! experimental nominal-bitrate profile. The latter budgets each audio packet
//! and greedily allocates bits by unweighted spectral squared-error reduction;
//! neither profile is a psychoacoustic quality encoder. Vorbis remains
//! experimental pending broader conformance coverage and perceptual validation.

mod bitstream;
mod codebook;
mod coupling;
mod decoder;
mod encoder;
mod floor;
mod headers;
mod mdct;
mod rate_control;
mod residue;
mod rewrite;
mod setup;
mod stream;

pub use decoder::VorbisDecoder;
pub use encoder::VorbisEncoder;
pub use headers::{VorbisCommentHeader, VorbisIdentificationHeader};
pub use rewrite::rewrite_metadata;
pub use setup::{
    VorbisCodebook, VorbisCouplingStep, VorbisFloor, VorbisFloor0, VorbisFloor1, VorbisFloor1Class,
    VorbisLookup, VorbisMapping, VorbisMode, VorbisResidue, VorbisSetupHeader, VorbisSubmap,
};
pub use stream::VorbisStreamReader;
