mod decode;
mod encode;
mod rewrite;

pub use decode::AuDecoder;
pub use encode::{AuEncoder, G711Encoding};
pub use rewrite::rewrite_metadata;

pub(crate) fn g726_packing_group(rate: g726::G726Rate) -> (u64, u64) {
    match rate {
        g726::G726Rate::Kbps16 => unreachable!("AU does not assign the 2-bit G.726 profile"),
        g726::G726Rate::Kbps24 => (3, 8),
        g726::G726Rate::Kbps32 => (1, 2),
        g726::G726Rate::Kbps40 => (5, 8),
    }
}

pub(crate) const MAX_ANNOTATION_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MINIMUM_DATA_OFFSET: u64 = 28;
pub(crate) const UNKNOWN_DATA_SIZE: u32 = u32::MAX;
