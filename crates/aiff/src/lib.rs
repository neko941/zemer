mod comment;
mod decode;
mod encode;
mod ima4;
mod rewrite;

pub use comment::AiffComment;
pub use decode::AiffDecoder;
pub use encode::{AiffEncoder, AiffKind};
pub use rewrite::rewrite_metadata;
