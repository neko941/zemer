//! Bounded parsing of WavPack metadata sub-block headers.

use std::io::{self, Read};

use zemer_core::AudioError;

/// A WavPack metadata sub-block header and its declared data lengths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WavPackMetadataSubBlockHeader {
    /// Metadata function ID, including the optional-ID range, with size flags removed.
    pub id: u8,
    /// Whether a decoder may skip this sub-block when it does not understand it.
    pub decoder_optional: bool,
    /// Actual data size, excluding the alignment byte when present.
    pub data_size: u64,
    /// Padded data size. Consume this many bytes before reading the next header.
    pub padded_data_size: u64,
}

impl WavPackMetadataSubBlockHeader {
    /// Reads a sub-block header bounded by `block_bytes_remaining`.
    ///
    /// The remaining count includes this sub-block's header and padded data;
    /// on success it is reduced by the complete sub-block size. The caller must
    /// consume `padded_data_size` bytes from `reader` before calling again.
    pub fn read_from<R: Read>(
        reader: &mut R,
        block_bytes_remaining: &mut u64,
    ) -> Result<Option<Self>, AudioError> {
        if *block_bytes_remaining == 0 {
            return Ok(None);
        }
        if *block_bytes_remaining < 2 {
            return Err(AudioError::InvalidData(
                "truncated WavPack metadata sub-block header",
            ));
        }

        let mut prefix = [0_u8; 4];
        reader.read_exact(&mut prefix[..2])?;
        let large = prefix[0] & 0x80 != 0;
        let length_field_size = if large { 3 } else { 1 };
        let header_size = 1 + length_field_size;
        if *block_bytes_remaining < header_size {
            return Err(AudioError::InvalidData(
                "truncated WavPack metadata sub-block header",
            ));
        }
        if large {
            reader.read_exact(&mut prefix[2..4])?;
        }

        let words = if large {
            u32::from_le_bytes([prefix[1], prefix[2], prefix[3], 0])
        } else {
            u32::from(prefix[1])
        };
        let padded_data_size = u64::from(words) * 2;
        let odd_data = prefix[0] & 0x40 != 0;
        if odd_data && padded_data_size == 0 {
            return Err(AudioError::InvalidData(
                "odd WavPack metadata sub-block has no padded data",
            ));
        }
        let data_size = padded_data_size - u64::from(odd_data);
        let sub_block_size = header_size + padded_data_size;
        if sub_block_size > *block_bytes_remaining {
            return Err(AudioError::InvalidData(
                "WavPack metadata sub-block exceeds its block",
            ));
        }
        *block_bytes_remaining -= sub_block_size;

        Ok(Some(Self {
            id: prefix[0] & 0x3f,
            decoder_optional: prefix[0] & 0x20 != 0,
            data_size,
            padded_data_size,
        }))
    }
}

/// Visits each metadata sub-block in a block payload without buffering it.
///
/// The visitor sees only the unpadded data bytes. Any unread data is drained
/// before the next header is parsed, and an odd-byte alignment byte is skipped.
pub fn visit_metadata_sub_blocks<R, F>(
    reader: &mut R,
    block_payload_size: u64,
    mut visit: F,
) -> Result<(), AudioError>
where
    R: Read,
    F: FnMut(WavPackMetadataSubBlockHeader, &mut dyn Read) -> Result<(), AudioError>,
{
    let mut remaining = block_payload_size;
    while let Some(header) = WavPackMetadataSubBlockHeader::read_from(reader, &mut remaining)? {
        let mut data = reader.take(header.data_size);
        visit(header, &mut data)?;
        let unread = data.limit();
        let consumed = io::copy(&mut data, &mut io::sink())?;
        if consumed != unread {
            return Err(AudioError::InvalidData(
                "truncated WavPack metadata sub-block data",
            ));
        }
        if header.padded_data_size != header.data_size {
            let mut padding = [0_u8; 1];
            reader.read_exact(&mut padding)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_small_optional_sub_block_and_leaves_data_unread() {
        let mut input = Cursor::new([0x2a, 2, 1, 2, 3, 4]);
        let mut remaining = 6;
        let header = WavPackMetadataSubBlockHeader::read_from(&mut input, &mut remaining)
            .unwrap()
            .unwrap();

        assert_eq!(header.id, 0x2a);
        assert!(header.decoder_optional);
        assert_eq!(header.data_size, 4);
        assert_eq!(header.padded_data_size, 4);
        assert_eq!(remaining, 0);
        assert_eq!(input.position(), 2);
    }

    #[test]
    fn odd_data_length_excludes_its_alignment_byte() {
        let mut input = Cursor::new([0x4a, 2, 1, 2, 3, 0]);
        let mut remaining = 6;
        let header = WavPackMetadataSubBlockHeader::read_from(&mut input, &mut remaining)
            .unwrap()
            .unwrap();

        assert_eq!(header.data_size, 3);
        assert_eq!(header.padded_data_size, 4);
    }

    #[test]
    fn parses_large_word_count_without_allocating_payload() {
        let mut input = Cursor::new([0x8d, 2, 0, 0, 9, 8, 7, 6]);
        let mut remaining = 8;
        let header = WavPackMetadataSubBlockHeader::read_from(&mut input, &mut remaining)
            .unwrap()
            .unwrap();

        assert_eq!(header.id, 0x0d);
        assert_eq!(header.data_size, 4);
        assert_eq!(header.padded_data_size, 4);
        assert_eq!(input.position(), 4);
        assert_eq!(remaining, 0);
    }

    #[test]
    fn rejects_headers_and_sizes_outside_block_boundary() {
        let mut short_header = Cursor::new([0x80, 1, 0, 0]);
        assert!(matches!(
            WavPackMetadataSubBlockHeader::read_from(&mut short_header, &mut 3),
            Err(AudioError::InvalidData(_))
        ));

        let mut too_large = Cursor::new([0x02, 2, 1, 2, 3, 4]);
        assert!(matches!(
            WavPackMetadataSubBlockHeader::read_from(&mut too_large, &mut 5),
            Err(AudioError::InvalidData(_))
        ));

        let mut impossible_odd = Cursor::new([0x42, 0]);
        assert!(matches!(
            WavPackMetadataSubBlockHeader::read_from(&mut impossible_odd, &mut 2),
            Err(AudioError::InvalidData(_))
        ));
    }

    #[test]
    fn every_truncated_header_or_payload_is_rejected() {
        let complete = [0x4a, 2, 1, 2, 3, 0];
        for length in 1..complete.len() {
            let mut input = Cursor::new(&complete[..length]);
            let mut remaining = length as u64;
            assert!(
                WavPackMetadataSubBlockHeader::read_from(&mut input, &mut remaining).is_err(),
                "accepted {length}-byte prefix"
            );
        }

        let mut input = Cursor::new(complete);
        let mut remaining = complete.len() as u64;
        assert!(
            WavPackMetadataSubBlockHeader::read_from(&mut input, &mut remaining)
                .unwrap()
                .is_some()
        );
        assert_eq!(remaining, 0);
    }

    #[test]
    fn visitor_drains_data_and_skips_odd_byte_padding() {
        let mut input = Cursor::new([0x4a, 2, 1, 2, 3, 0, 0x2a, 1, 9, 8]);
        let mut visited = Vec::new();

        visit_metadata_sub_blocks(&mut input, 10, |header, data| {
            visited.push((header.id, header.data_size));
            let mut first = [0; 1];
            if header.data_size == 3 {
                data.read_exact(&mut first)?;
                assert_eq!(first, [1]);
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(visited, [(0x0a, 3), (0x2a, 2)]);
        assert_eq!(input.position(), 10);
    }

    #[test]
    fn visitor_rejects_truncated_data_and_padding() {
        for (bytes, size) in [(&[0x0a, 2, 1, 2, 3][..], 6), (&[0x4a, 2, 1, 2, 3][..], 6)] {
            assert!(
                visit_metadata_sub_blocks(&mut Cursor::new(bytes), size, |_, _| Ok(())).is_err()
            );
        }
    }
}
