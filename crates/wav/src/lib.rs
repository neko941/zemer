mod decode;
mod encode;

pub use decode::WavDecoder;
pub use encode::{G711Encoding, WavContainer, WavWriter, write_wav, write_wav_with_metadata};
