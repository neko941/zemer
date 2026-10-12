//! DSDIFF container support for uncompressed, raw one-bit DSD.

mod decode;
mod encode;

pub use decode::DsdiffDecoder;
pub use encode::DsdiffEncoder;

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zemer_core::{DsdBuffer, DsdDecoder, DsdEncoder, DsdSpec, Metadata};

    use crate::{DsdiffDecoder, DsdiffEncoder};

    #[test]
    fn raw_dsd_round_trip_preserves_channels_and_text_metadata() {
        let spec = DsdSpec::new(2_822_400, 2).unwrap();
        let audio = DsdBuffer::new(spec, vec![0x80, 0x01, 0x55, 0xaa, 0x7f, 0x20]).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "Title");
        metadata.insert("ARTIST", "Composer");
        metadata.insert("DSDIFF_CHANNEL_ID", "C123");
        metadata.insert("DSDIFF_CHANNEL_ID", "C124");
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.set_metadata(&metadata).unwrap();
        encoder.write_dsd_octets(&audio).unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();

        let mut decoder = DsdiffDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.dsd_spec(), spec);
        assert_eq!(decoder.sample_count(), Some(24));
        assert_eq!(decoder.metadata().values("TITLE"), ["Title"]);
        assert_eq!(decoder.metadata().values("ARTIST"), ["Composer"]);
        assert_eq!(
            decoder.metadata().values("DSDIFF_CHANNEL_ID"),
            ["C123", "C124"]
        );
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_dsd_octets(1).unwrap() {
            decoded.extend_from_slice(block.bytes());
        }
        assert_eq!(decoded, audio.bytes());
    }

    #[test]
    fn mono_odd_length_data_uses_an_unplayed_zero_pad_byte() {
        let spec = DsdSpec::new(2_822_400, 1).unwrap();
        let audio = DsdBuffer::new(spec, vec![0xa5]).unwrap();
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_dsd_octets(&audio).unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();
        assert_eq!(bytes.last(), Some(&0));
        let mut decoder = DsdiffDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.read_dsd_octets(8).unwrap().unwrap().bytes(), [0xa5]);
        assert!(decoder.read_dsd_octets(8).unwrap().is_none());
    }

    #[test]
    fn rejects_dst_and_bad_form_boundaries() {
        let spec = DsdSpec::new(2_822_400, 1).unwrap();
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55]).unwrap())
            .unwrap();
        encoder.finish().unwrap();
        let mut bytes = encoder.into_inner().into_inner();
        let compression_id = bytes
            .windows(4)
            .position(|window| window == b"CMPR")
            .unwrap();
        let compression_id = compression_id + 12;
        bytes[compression_id..compression_id + 4].copy_from_slice(b"DST ");
        assert!(matches!(
            DsdiffDecoder::new(Cursor::new(bytes.clone())),
            Err(zemer_core::AudioError::Unsupported(_))
        ));

        bytes[4..12].copy_from_slice(&0_u64.to_be_bytes());
        assert!(DsdiffDecoder::new(Cursor::new(bytes)).is_err());
    }

    #[test]
    fn rejects_mismatched_text_count_and_duplicate_titles() {
        let spec = DsdSpec::new(2_822_400, 1).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "Title");
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.set_metadata(&metadata).unwrap();
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55]).unwrap())
            .unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();

        let mut bad_count = bytes.clone();
        let title = bad_count
            .windows(4)
            .position(|window| window == b"DITI")
            .unwrap();
        bad_count[title + 12..title + 16].copy_from_slice(&99_u32.to_be_bytes());
        assert!(DsdiffDecoder::new(Cursor::new(bad_count)).is_err());

        let mut duplicate = bytes;
        let title_size = u64::from_be_bytes(duplicate[title + 4..title + 12].try_into().unwrap());
        let title_chunk_size = 12 + title_size + (title_size & 1);
        let title_chunk = duplicate[title..title + title_chunk_size as usize].to_vec();
        let diin = duplicate
            .windows(4)
            .position(|window| window == b"DIIN")
            .unwrap();
        let diin_size = u64::from_be_bytes(duplicate[diin + 4..diin + 12].try_into().unwrap());
        let insert_at = diin + 12 + diin_size as usize;
        duplicate.splice(insert_at..insert_at, title_chunk.clone());
        duplicate[diin + 4..diin + 12]
            .copy_from_slice(&(diin_size + title_chunk_size).to_be_bytes());
        let form_size = u64::from_be_bytes(duplicate[4..12].try_into().unwrap());
        duplicate[4..12].copy_from_slice(&(form_size + title_chunk_size).to_be_bytes());
        assert!(DsdiffDecoder::new(Cursor::new(duplicate)).is_err());
    }
}
