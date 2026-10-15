mod comment;
mod decode;
mod encode;
mod ima4;
mod instrument;
mod marker;
mod rewrite;

pub use comment::AiffComment;
pub use decode::AiffDecoder;
pub use encode::{AiffEncoder, AiffKind};
pub use g726::G726Rate;
pub use instrument::{AiffInstrument, AiffLoop, AiffLoopMode};
pub use marker::AiffMarker;
pub use rewrite::rewrite_metadata;
