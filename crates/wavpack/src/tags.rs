use std::io::{Read, Seek, SeekFrom, Write, copy};

use zemer_core::{AudioError, Metadata};
use zemer_metadata::apev2::{ApeTag, MAX_APEV2_TAG_SIZE};

const ID3V1_SIZE: u64 = 128;
const APE_FOOTER_SIZE: u64 = 32;

/// Reads the trailing APEv2 tag, if present, without changing the reader position.
pub fn read_apev2_tag<R: Read + Seek>(reader: &mut R) -> Result<Option<ApeTag>, AudioError> {
    locate_tag(reader).map(|tag| tag.map(|(_, _, parsed)| parsed))
}

/// Rewrites APEv2 metadata and copies WavPack audio and ID3v1 bytes unchanged.
///
/// Callers editing a file should write to a new temporary path and replace the original only after
/// this function succeeds.
pub fn rewrite_metadata<R: Read + Seek, W: Write>(
    mut reader: R,
    mut writer: W,
    metadata: &Metadata,
) -> Result<(), AudioError> {
    let file_end = reader.seek(SeekFrom::End(0))?;
    let id3v1_start = has_id3v1(&mut reader, file_end)?;
    let tag_region = locate_tag(&mut reader)?;
    let copy_end = tag_region
        .as_ref()
        .map_or(id3v1_start.unwrap_or(file_end), |(start, _, _)| *start);

    reader.seek(SeekFrom::Start(0))?;
    copy_exact(&mut reader, &mut writer, copy_end)?;

    let mut tag = tag_region.map_or_else(ApeTag::new, |(_, _, tag)| tag);
    tag.replace_metadata(metadata)
        .map_err(|error| AudioError::InvalidData(error.0))?;
    if !tag.items().is_empty() {
        writer.write_all(
            &tag.encode()
                .map_err(|error| AudioError::InvalidData(error.0))?,
        )?;
    }

    if let Some(start) = id3v1_start {
        reader.seek(SeekFrom::Start(start))?;
        copy_exact(&mut reader, &mut writer, file_end - start)?;
    }
    Ok(())
}

fn copy_exact<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    length: u64,
) -> Result<(), AudioError> {
    let copied = copy(&mut reader.take(length), writer)?;
    if copied != length {
        return Err(AudioError::InvalidFormat(
            "WavPack file ended during metadata rewrite",
        ));
    }
    Ok(())
}

pub(crate) fn trailing_metadata_and_audio_end<R: Read + Seek>(
    reader: &mut R,
) -> Result<(u64, Metadata), AudioError> {
    let original_position = reader.stream_position()?;
    let result = (|| {
        let file_end = reader.seek(SeekFrom::End(0))?;
        let id3v1_start = has_id3v1(reader, file_end)?;
        let tag = locate_tag(reader)?;
        let audio_end = tag
            .as_ref()
            .map_or(id3v1_start.unwrap_or(file_end), |(start, _, _)| *start);
        Ok((
            audio_end,
            tag.map_or_else(Metadata::default, |(_, _, tag)| tag.metadata()),
        ))
    })();
    let restore = reader.seek(SeekFrom::Start(original_position));
    match (result, restore) {
        (Ok(value), Ok(_)) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

fn locate_tag<R: Read + Seek>(reader: &mut R) -> Result<Option<(u64, u64, ApeTag)>, AudioError> {
    let original_position = reader.stream_position()?;
    let result = (|| {
        let file_end = reader.seek(SeekFrom::End(0))?;
        let tag_end = has_id3v1(reader, file_end)?.unwrap_or(file_end);
        if tag_end < APE_FOOTER_SIZE {
            return Ok(None);
        }
        reader.seek(SeekFrom::Start(tag_end - APE_FOOTER_SIZE))?;
        let mut footer = [0_u8; APE_FOOTER_SIZE as usize];
        reader.read_exact(&mut footer)?;
        if &footer[..8] != b"APETAGEX" {
            return Ok(None);
        }

        let size = u64::from(u32::from_le_bytes(footer[12..16].try_into().unwrap()));
        let flags = u32::from_le_bytes(footer[20..24].try_into().unwrap());
        let header_size = if flags & (1 << 31) != 0 {
            APE_FOOTER_SIZE
        } else {
            0
        };
        let total_size = size
            .checked_add(header_size)
            .filter(|&size| size <= MAX_APEV2_TAG_SIZE as u64 + APE_FOOTER_SIZE)
            .ok_or(AudioError::InvalidData(
                "invalid or oversized WavPack APEv2 tag",
            ))?;
        let tag_start = tag_end
            .checked_sub(total_size)
            .ok_or(AudioError::InvalidData(
                "WavPack APEv2 tag exceeds the file",
            ))?;
        let read_size = usize::try_from(total_size)
            .map_err(|_| AudioError::InvalidData("WavPack APEv2 tag is too large"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(read_size)
            .map_err(|_| AudioError::InvalidData("WavPack APEv2 tag is too large"))?;
        bytes.resize(read_size, 0);
        reader.seek(SeekFrom::Start(tag_start))?;
        reader.read_exact(&mut bytes)?;
        let tag = ApeTag::parse(&bytes).map_err(|error| AudioError::InvalidData(error.0))?;
        Ok(Some((tag_start, tag_end, tag)))
    })();
    let restore = reader.seek(SeekFrom::Start(original_position));
    match (result, restore) {
        (Ok(value), Ok(_)) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

fn has_id3v1<R: Read + Seek>(reader: &mut R, file_end: u64) -> Result<Option<u64>, AudioError> {
    if file_end < ID3V1_SIZE {
        return Ok(None);
    }
    let start = file_end - ID3V1_SIZE;
    reader.seek(SeekFrom::Start(start))?;
    let mut signature = [0; 3];
    reader.read_exact(&mut signature)?;
    Ok((signature == *b"TAG").then_some(start))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Seek, SeekFrom};

    use super::*;
    use zemer_metadata::{Picture, apev2::ApeTag};

    fn file_with_tag(tag: &ApeTag, id3v1: bool) -> Vec<u8> {
        let mut bytes = b"a small fake WavPack audio stream".to_vec();
        bytes.extend_from_slice(&tag.encode().unwrap());
        if id3v1 {
            let mut trailer = [0; 128];
            trailer[..3].copy_from_slice(b"TAG");
            bytes.extend_from_slice(&trailer);
        }
        bytes
    }

    struct EarlyEofAfterRestart {
        reader: Cursor<Vec<u8>>,
        read_limit: u64,
        restarted: bool,
    }

    impl Read for EarlyEofAfterRestart {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if self.restarted && self.reader.position() >= self.read_limit {
                return Ok(0);
            }
            let limit = if self.restarted {
                usize::try_from(self.read_limit - self.reader.position())
                    .unwrap_or(usize::MAX)
                    .min(output.len())
            } else {
                output.len()
            };
            self.reader.read(&mut output[..limit])
        }
    }

    impl Seek for EarlyEofAfterRestart {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            if position == SeekFrom::Start(0) {
                self.restarted = true;
            }
            self.reader.seek(position)
        }
    }

    #[test]
    fn reads_trailing_text_metadata_before_optional_id3v1() {
        let mut expected = Metadata::default();
        expected.insert("title", "Track");
        let mut tag = ApeTag::new();
        tag.replace_text_metadata(&expected).unwrap();
        let bytes = file_with_tag(&tag, true);
        let mut input = Cursor::new(bytes);
        input.set_position(5);

        assert_eq!(
            read_apev2_tag(&mut input).unwrap().unwrap().metadata(),
            expected
        );
        assert_eq!(input.position(), 5);
    }

    #[test]
    fn reads_official_wavpack_tagged_fixture() {
        let mut input = Cursor::new(include_bytes!(
            "../tests/fixtures/tagged-custom-rate-stereo.wv"
        ));
        let tag = read_apev2_tag(&mut input).unwrap().unwrap();
        assert!(!tag.metadata().values("TITLE").is_empty());
    }

    #[test]
    fn rewrite_preserves_audio_and_id3v1_and_can_remove_apev2() {
        let mut original_metadata = Metadata::default();
        original_metadata.insert("title", "Old");
        let mut tag = ApeTag::new();
        tag.replace_text_metadata(&original_metadata).unwrap();
        let original = file_with_tag(&tag, true);

        let mut updated = Metadata::default();
        updated.insert("artist", "New");
        let mut output = Vec::new();
        rewrite_metadata(Cursor::new(&original), &mut output, &updated).unwrap();
        assert!(output.starts_with(b"a small fake WavPack audio stream"));
        assert!(output.ends_with(&original[original.len() - 128..]));
        assert_eq!(
            read_apev2_tag(&mut Cursor::new(&output))
                .unwrap()
                .unwrap()
                .metadata(),
            updated
        );

        output.clear();
        rewrite_metadata(Cursor::new(&original), &mut output, &Metadata::default()).unwrap();
        assert_eq!(output.len(), original.len() - tag.encode().unwrap().len());
        assert!(output.ends_with(&original[original.len() - 128..]));
    }

    #[test]
    fn rewrite_adds_a_tag_to_an_untagged_stream() {
        let source = b"WavPack bytes";
        let mut metadata = Metadata::default();
        metadata.insert("album", "Record");
        let mut output = Vec::new();
        rewrite_metadata(Cursor::new(source), &mut output, &metadata).unwrap();
        assert!(output.starts_with(source));
        assert_eq!(
            read_apev2_tag(&mut Cursor::new(&output))
                .unwrap()
                .unwrap()
                .metadata(),
            metadata
        );
    }

    #[test]
    fn rewriting_text_keeps_parsed_cover_art_and_audio_bytes() {
        let audio = b"a small fake WavPack audio stream";
        let mut original = Metadata::default();
        original.insert("title", "Old");
        original.add_picture(Picture::new(
            3,
            "image/jpeg",
            "",
            0,
            0,
            0,
            0,
            b"cover".to_vec(),
        ));
        let mut tag = ApeTag::new();
        tag.replace_metadata(&original).unwrap();
        let source = file_with_tag(&tag, false);

        let mut updated = tag.metadata();
        updated.remove("title");
        updated.insert("title", "New");
        let mut output = Vec::new();
        rewrite_metadata(Cursor::new(&source), &mut output, &updated).unwrap();

        assert!(output.starts_with(audio));
        let parsed = read_apev2_tag(&mut Cursor::new(&output)).unwrap().unwrap();
        assert_eq!(parsed.metadata().values("TITLE"), ["New"]);
        assert_eq!(parsed.metadata().pictures(), original.pictures());
    }

    #[test]
    fn rewrite_fails_if_the_source_ends_during_a_known_length_copy() {
        let mut original_metadata = Metadata::default();
        original_metadata.insert("title", "Old");
        let mut tag = ApeTag::new();
        tag.replace_text_metadata(&original_metadata).unwrap();
        let source = file_with_tag(&tag, false);
        let reader = EarlyEofAfterRestart {
            reader: Cursor::new(source),
            read_limit: 8,
            restarted: false,
        };
        let mut output = Vec::new();

        assert!(rewrite_metadata(reader, &mut output, &Metadata::default()).is_err());
        assert_eq!(output.len(), 8);
    }
}
