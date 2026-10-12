//! Clean-room WavPack block inspection and decoding.

use std::io::{self, Read, SeekFrom};

use zemer_core::AudioError;

mod bitstream;
mod checksum;
mod decode;
mod dsd_decode;
mod dsd_encode;
mod encode;
mod entropy;
mod float;
mod hybrid;
mod hybrid_encode;
pub mod metadata;
mod tags;
pub use decode::WavPackDecoder;
pub use dsd_decode::WavPackDsdDecoder;
pub use dsd_encode::WavPackDsdEncoder;
pub use encode::WavPackEncoder;
pub use hybrid_encode::WavPackHybridEncoder;
pub use metadata::{WavPackMetadataSubBlockHeader, visit_metadata_sub_blocks};
pub use tags::{read_apev2_tag, rewrite_metadata};

const HEADER_SIZE: usize = 32;

/// The fixed fields in a WavPack block header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WavPackBlockHeader {
    /// WavPack stream version, as stored in the header.
    pub version: u16,
    /// Upper 8 bits of the 40-bit block index in version 5 streams.
    pub block_index_high: u8,
    /// Upper 8 bits of the 40-bit total sample count in version 5 streams.
    pub total_samples_high: u8,
    /// Lower 32 bits of the total sample count.
    pub total_samples_low: u32,
    /// Lower 32 bits of the block index.
    pub block_index_low: u32,
    /// Number of samples represented by this block.
    pub block_samples: u32,
    /// Raw WavPack block flags.
    pub flags: u32,
    /// Stored block checksum.
    pub crc: u32,
    /// Total block size, including the 32-byte header.
    pub block_size: u64,
}

impl WavPackBlockHeader {
    /// Reads one fixed-size header, leaving the reader at the block payload.
    /// Returns `Ok(None)` only when the reader is already at clean EOF.
    pub fn read_from<R: Read>(reader: &mut R) -> Result<Option<Self>, AudioError> {
        let mut bytes = [0_u8; HEADER_SIZE];
        loop {
            match reader.read(&mut bytes[..1]) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        reader.read_exact(&mut bytes[1..])?;

        if &bytes[..4] != b"wvpk" {
            return Err(AudioError::InvalidFormat("missing WavPack block signature"));
        }

        let ck_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if ck_size < (HEADER_SIZE - 8) as u32 {
            return Err(AudioError::InvalidData(
                "WavPack block is smaller than its header",
            ));
        }

        Ok(Some(Self {
            version: u16::from_le_bytes(bytes[8..10].try_into().unwrap()),
            block_index_high: bytes[10],
            total_samples_high: bytes[11],
            total_samples_low: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            block_index_low: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
            block_samples: u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
            flags: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            crc: u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
            block_size: u64::from(ck_size) + 8,
        }))
    }

    /// Returns the total sample count, using the extended field in version 5.
    pub fn total_samples(&self) -> u64 {
        if self.version >= 0x410 {
            (u64::from(self.total_samples_high) << 32) | u64::from(self.total_samples_low)
        } else {
            u64::from(self.total_samples_low)
        }
    }

    /// Returns the block index, using the extended field in version 5.
    pub fn block_index(&self) -> u64 {
        if self.version >= 0x410 {
            (u64::from(self.block_index_high) << 32) | u64::from(self.block_index_low)
        } else {
            u64::from(self.block_index_low)
        }
    }

    /// Returns the bytes after this fixed header that belong to the block.
    pub fn payload_size(&self) -> u64 {
        self.block_size - HEADER_SIZE as u64
    }
}

impl<R: io::Read + io::Seek> WavPackDecoder<R> {
    /// Opens a seekable WavPack file and loads its trailing APEv2 text metadata.
    ///
    /// Streaming inputs should use [`WavPackDecoder::new`]; they cannot inspect an end tag
    /// before audio decoding reaches the end of the stream.
    pub fn new_seekable(mut reader: R) -> Result<WavPackDecoder<io::Take<R>>, AudioError> {
        let start = reader.stream_position()?;
        let (audio_end, metadata) = tags::trailing_metadata_and_audio_end(&mut reader)?;
        let audio_size = audio_end
            .checked_sub(start)
            .ok_or(AudioError::InvalidData("invalid WavPack audio range"))?;
        reader.seek(SeekFrom::Start(start))?;
        let mut decoder = WavPackDecoder::new(reader.take(audio_size))?;
        decoder.metadata = metadata;
        Ok(decoder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn block(payload: &[u8]) -> Vec<u8> {
        let ck_size = u32::try_from(24 + payload.len()).unwrap();
        let mut bytes = Vec::with_capacity(usize::try_from(ck_size + 8).unwrap());
        bytes.extend_from_slice(b"wvpk");
        bytes.extend_from_slice(&ck_size.to_le_bytes());
        bytes.extend_from_slice(&0x410_u16.to_le_bytes());
        bytes.extend_from_slice(&[0x12, 0x34]);
        bytes.extend_from_slice(&0x5678_9abc_u32.to_le_bytes());
        bytes.extend_from_slice(&0xdef0_1234_u32.to_le_bytes());
        bytes.extend_from_slice(&4096_u32.to_le_bytes());
        bytes.extend_from_slice(&0x1234_5678_u32.to_le_bytes());
        bytes.extend_from_slice(&0x9abc_def0_u32.to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn reads_header_without_consuming_payload() {
        let mut input = Cursor::new(block(b"audio payload"));
        let header = WavPackBlockHeader::read_from(&mut input).unwrap().unwrap();

        assert_eq!(header.version, 0x410);
        assert_eq!(header.total_samples(), 0x0034_5678_9abc);
        assert_eq!(header.block_index(), 0x0012_def0_1234);
        assert_eq!(header.block_samples, 4096);
        assert_eq!(header.flags, 0x1234_5678);
        assert_eq!(header.crc, 0x9abc_def0);
        assert_eq!(header.payload_size(), 13);
        let position = usize::try_from(input.position()).unwrap();
        assert_eq!(&input.get_ref()[position..], b"audio payload");
    }

    #[test]
    fn clean_eof_has_no_header() {
        assert_eq!(
            WavPackBlockHeader::read_from(&mut Cursor::new([])).unwrap(),
            None
        );
    }

    #[test]
    fn version_4_0_7_uses_32_bit_count_and_index_fields() {
        let mut bytes = block(&[]);
        bytes[8..10].copy_from_slice(&0x407_u16.to_le_bytes());
        let header = WavPackBlockHeader::read_from(&mut Cursor::new(bytes))
            .unwrap()
            .unwrap();

        assert_eq!(header.total_samples(), 0x5678_9abc);
        assert_eq!(header.block_index(), 0xdef0_1234);
    }

    #[test]
    fn rejects_invalid_signature_and_too_small_block() {
        let mut bad_signature = block(&[]);
        bad_signature[..4].copy_from_slice(b"nope");
        assert!(matches!(
            WavPackBlockHeader::read_from(&mut Cursor::new(bad_signature)),
            Err(AudioError::InvalidFormat(_))
        ));

        let mut short_block = block(&[]);
        short_block[4..8].copy_from_slice(&23_u32.to_le_bytes());
        assert!(matches!(
            WavPackBlockHeader::read_from(&mut Cursor::new(short_block)),
            Err(AudioError::InvalidData(_))
        ));
    }

    #[test]
    fn rejects_truncated_header() {
        for length in 1..HEADER_SIZE {
            let mut input = Cursor::new(block(&[])[..length].to_vec());
            assert!(
                matches!(
                    WavPackBlockHeader::read_from(&mut input),
                    Err(AudioError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof
                ),
                "accepted {length}-byte prefix"
            );
        }
    }
}
