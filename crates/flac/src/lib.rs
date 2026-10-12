mod bit_reader;
mod decode;
mod frame;
mod md5;
mod metadata;
mod rewrite;

pub use decode::FlacDecoder;
pub use metadata::{
    FlacApplication, FlacCueIndex, FlacCueSheet, FlacCueTrack, FlacMetadataBlock, FlacPicture,
    FlacSeekPoint, FlacSeekTable, FlacStreamInfo,
};
pub use rewrite::rewrite_metadata;

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zemer_core::{AudioDecoder, AudioError, SampleFormat, Samples};

    use crate::FlacDecoder;

    fn rfc_9639_appendix_d1() -> [u8; 57] {
        [
            0x66, 0x4c, 0x61, 0x43, 0x80, 0x00, 0x00, 0x22, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00,
            0x0f, 0x00, 0x00, 0x0f, 0x0a, 0xc4, 0x42, 0xf0, 0x00, 0x00, 0x00, 0x01, 0x3e, 0x84,
            0xb4, 0x18, 0x07, 0xdc, 0x69, 0x03, 0x07, 0x58, 0x6a, 0x3d, 0xad, 0x1a, 0x2e, 0x0f,
            0xff, 0xf8, 0x69, 0x18, 0x00, 0x00, 0xbf, 0x03, 0x58, 0xfd, 0x03, 0x12, 0x8b, 0xaa,
            0x9a,
        ]
    }

    #[test]
    fn decodes_rfc_9639_appendix_d_example_one() {
        let bytes = rfc_9639_appendix_d1();
        let mut decoder = FlacDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.spec().sample_rate(), 44_100);
        assert_eq!(decoder.spec().channels(), 2);
        assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);
        assert_eq!(decoder.metadata_blocks().len(), 1);
        assert_eq!(decoder.metadata_blocks()[0].kind(), 0);
        assert_eq!(decoder.metadata_blocks()[0].data().len(), 34);
        let block = decoder.read_frames(1).unwrap().unwrap();
        assert_eq!(block.samples(), &Samples::I16(vec![25_588, 10_416]));
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn rejects_a_corrupted_frame_crc() {
        let mut bytes = rfc_9639_appendix_d1();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        let mut decoder = FlacDecoder::new(Cursor::new(bytes)).unwrap();
        assert!(matches!(
            decoder.read_frames(1),
            Err(AudioError::InvalidData("FLAC frame CRC-16 mismatch"))
        ));
    }

    #[test]
    fn verifies_streaminfo_md5_after_the_complete_stream_is_read() {
        let mut bytes = rfc_9639_appendix_d1();
        let mut decoder = FlacDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.read_frames(1).unwrap().unwrap().frames(), 1);
        assert!(decoder.read_frames(1).unwrap().is_none());

        bytes[26] ^= 1;
        let mut decoder = FlacDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.read_frames(1).unwrap().unwrap().frames(), 1);
        assert!(matches!(
            decoder.read_frames(1),
            Err(AudioError::InvalidData(
                "FLAC decoded audio MD5 differs from STREAMINFO"
            ))
        ));
    }

    #[test]
    fn skips_streaminfo_md5_validation_when_the_checksum_is_unknown() {
        let mut bytes = rfc_9639_appendix_d1();
        bytes[26..42].fill(0);
        let mut decoder = FlacDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.read_frames(1).unwrap().unwrap().frames(), 1);
        assert!(decoder.read_frames(1).unwrap().is_none());
    }
}
