mod decode;
mod encode;
mod rewrite;

pub use decode::AuDecoder;
pub use encode::{AuEncoder, G711Encoding};
pub use rewrite::rewrite_metadata;

pub(crate) const MAX_ANNOTATION_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const UNKNOWN_DATA_SIZE: u32 = u32::MAX;
