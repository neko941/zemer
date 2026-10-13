//! Clean-room Vorbis I support built from Xiph's bitstream specification.
//!
//! This crate is under development and is not advertised as a supported codec
//! until packet decoding, PCM synthesis, and interoperable output are complete.

mod bitstream;
mod codebook;
mod headers;
mod setup;

pub use headers::{VorbisCommentHeader, VorbisIdentificationHeader};
pub use setup::{
    VorbisCodebook, VorbisCouplingStep, VorbisFloor, VorbisFloor0, VorbisFloor1, VorbisFloor1Class,
    VorbisLookup, VorbisMapping, VorbisMode, VorbisResidue, VorbisSetupHeader, VorbisSubmap,
};
