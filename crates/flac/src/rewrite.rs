use std::io::{Read, Write, copy};

use zemer_core::{AudioError, Metadata};

use crate::metadata::{checked_metadata_size, read_metadata};

pub(crate) const MAX_BLOCK_BYTES: usize = 0x00ff_ffff;
pub(crate) const DEFAULT_VENDOR: &[u8] = b"zemer";

/// Rewrites FLAC Vorbis comments and copies every other metadata block and audio byte unchanged.
///
/// The reader must start at the beginning of a native FLAC stream. The writer receives a new
/// stream; callers that edit a file should write to a separate temporary file and replace the
/// original only after this function succeeds.
pub fn rewrite_metadata<R: Read, W: Write>(
    mut input: R,
    mut output: W,
    metadata: &Metadata,
) -> Result<(), AudioError> {
    let (_stream_info, _current_metadata, blocks) = read_metadata(&mut input)?;
    let vendor = blocks
        .iter()
        .find(|block| block.kind() == 4)
        .map(|block| read_vendor(block.data()))
        .transpose()?
        .unwrap_or(DEFAULT_VENDOR);
    let comments = encode_comments(metadata, vendor)?;

    let has_comments = blocks.iter().any(|block| block.kind() == 4);
    let mut retained_size = 0;
    for block in &blocks {
        retained_size = checked_metadata_size(
            retained_size,
            if block.kind() == 4 {
                comments.len()
            } else {
                block.data().len()
            },
        )?;
    }
    if !has_comments {
        checked_metadata_size(retained_size, comments.len())?;
    }

    output.write_all(b"fLaC")?;
    let output_block_count = blocks.len() + usize::from(!has_comments);
    let mut written = 0;
    for block in &blocks {
        written += 1;
        write_block(
            &mut output,
            block.kind(),
            if block.kind() == 4 {
                &comments
            } else {
                block.data()
            },
            written == output_block_count,
        )?;
    }
    if !has_comments {
        write_block(&mut output, 4, &comments, true)?;
    }
    copy(&mut input, &mut output)?;
    Ok(())
}

fn read_vendor(data: &[u8]) -> Result<&[u8], AudioError> {
    let length_bytes: [u8; 4] = data
        .get(..4)
        .ok_or(AudioError::InvalidData("invalid FLAC Vorbis comment block"))?
        .try_into()
        .unwrap();
    let length = u32::from_le_bytes(length_bytes) as usize;
    let end = 4_usize
        .checked_add(length)
        .filter(|&end| end <= data.len())
        .ok_or(AudioError::InvalidData("invalid FLAC Vorbis comment block"))?;
    Ok(&data[4..end])
}

pub(crate) fn encode_comments(metadata: &Metadata, vendor: &[u8]) -> Result<Vec<u8>, AudioError> {
    let field_count = metadata
        .iter()
        .try_fold(0_usize, |count, (_, values)| {
            count.checked_add(values.len())
        })
        .ok_or(AudioError::InvalidData("too many FLAC comment fields"))?;
    let field_count = u32::try_from(field_count)
        .map_err(|_| AudioError::InvalidData("too many FLAC comment fields"))?;
    let vendor_length = u32::try_from(vendor.len())
        .map_err(|_| AudioError::InvalidData("FLAC comment vendor string is too large"))?;

    let mut block_length = 8_usize
        .checked_add(vendor.len())
        .ok_or(AudioError::InvalidData("FLAC comment block is too large"))?;
    for (key, values) in metadata.iter() {
        if key.is_empty()
            || key
                .bytes()
                .any(|byte| !(0x20..=0x7e).contains(&byte) || byte == b'=')
        {
            return Err(AudioError::InvalidData("invalid FLAC comment field name"));
        }
        for value in values {
            let field_length = key
                .len()
                .checked_add(1)
                .and_then(|length| length.checked_add(value.len()))
                .ok_or(AudioError::InvalidData("FLAC comment field is too large"))?;
            u32::try_from(field_length)
                .map_err(|_| AudioError::InvalidData("FLAC comment field is too large"))?;
            block_length = block_length
                .checked_add(4)
                .and_then(|length| length.checked_add(field_length))
                .filter(|&length| length <= MAX_BLOCK_BYTES)
                .ok_or(AudioError::InvalidData("FLAC comment block is too large"))?;
        }
    }
    if block_length > MAX_BLOCK_BYTES {
        return Err(AudioError::InvalidData("FLAC comment block is too large"));
    }

    let mut block = Vec::with_capacity(block_length);
    block.extend_from_slice(&vendor_length.to_le_bytes());
    block.extend_from_slice(vendor);
    block.extend_from_slice(&field_count.to_le_bytes());
    for (key, values) in metadata.iter() {
        for value in values {
            let field_length = (key.len() + 1 + value.len()) as u32;
            block.extend_from_slice(&field_length.to_le_bytes());
            block.extend_from_slice(key.as_bytes());
            block.push(b'=');
            block.extend_from_slice(value.as_bytes());
        }
    }
    Ok(block)
}

pub(crate) fn write_block<W: Write>(
    output: &mut W,
    kind: u8,
    data: &[u8],
    is_last: bool,
) -> Result<(), AudioError> {
    let length = u32::try_from(data.len())
        .ok()
        .filter(|&length| length <= MAX_BLOCK_BYTES as u32)
        .ok_or(AudioError::InvalidData("FLAC metadata block is too large"))?;
    let header = [
        kind | if is_last { 0x80 } else { 0 },
        (length >> 16) as u8,
        (length >> 8) as u8,
        length as u8,
    ];
    output.write_all(&header)?;
    output.write_all(data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zemer_core::{AudioError, Metadata};

    use super::rewrite_metadata;

    fn stream_info() -> [u8; 34] {
        let mut data = [0_u8; 34];
        data[..2].copy_from_slice(&4096_u16.to_be_bytes());
        data[2..4].copy_from_slice(&4096_u16.to_be_bytes());
        let packed = (44_100_u64 << 44) | (1 << 41) | (15 << 36) | 1;
        data[10..18].copy_from_slice(&packed.to_be_bytes());
        data
    }

    fn block(kind: u8, last: bool, payload: &[u8]) -> Vec<u8> {
        let length = payload.len();
        let mut data = vec![
            kind | if last { 0x80 } else { 0 },
            (length >> 16) as u8,
            (length >> 8) as u8,
            length as u8,
        ];
        data.extend_from_slice(payload);
        data
    }

    fn comments(vendor: &[u8], fields: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut data = Vec::from((vendor.len() as u32).to_le_bytes());
        data.extend_from_slice(vendor);
        data.extend_from_slice(&(fields.len() as u32).to_le_bytes());
        for (key, value) in fields {
            let mut field = Vec::from(*key);
            field.push(b'=');
            field.extend_from_slice(value);
            data.extend_from_slice(&(field.len() as u32).to_le_bytes());
            data.extend_from_slice(&field);
        }
        data
    }

    fn audio_start(data: &[u8]) -> usize {
        let mut cursor = 4;
        loop {
            let header = &data[cursor..cursor + 4];
            cursor += 4
                + (usize::from(header[1]) << 16)
                + (usize::from(header[2]) << 8)
                + usize::from(header[3]);
            if header[0] & 0x80 != 0 {
                return cursor;
            }
        }
    }

    #[test]
    fn rewrites_comments_and_preserves_vendor_other_blocks_and_audio() {
        use zemer_core::AudioDecoder;

        let audio = b"frame bytes retained verbatim";
        let picture = [9, 8, 7, 6];
        let mut input = b"fLaC".to_vec();
        input.extend(block(0, false, &stream_info()));
        input.extend(block(
            4,
            false,
            &comments(b"legacy encoder", &[(b"TITLE", b"old")]),
        ));
        input.extend(block(6, true, &picture));
        input.extend_from_slice(audio);

        let mut metadata = Metadata::default();
        metadata.insert("title", "חדש");
        metadata.insert("artist", "Artist");
        metadata.insert("artist", "Guest");
        let mut output = Vec::new();
        rewrite_metadata(Cursor::new(&input), &mut output, &metadata).unwrap();

        let decoder = crate::FlacDecoder::new(Cursor::new(output.clone())).unwrap();
        assert_eq!(decoder.metadata().values("title"), ["חדש"]);
        assert_eq!(decoder.metadata().values("artist"), ["Artist", "Guest"]);
        assert_eq!(decoder.metadata_blocks()[1].kind(), 4);
        assert_eq!(
            super::read_vendor(decoder.metadata_blocks()[1].data()).unwrap(),
            b"legacy encoder"
        );
        assert_eq!(decoder.metadata_blocks()[2].kind(), 6);
        assert_eq!(decoder.metadata_blocks()[2].data(), picture);
        assert_eq!(&output[audio_start(&output)..], audio);
    }

    #[test]
    fn adds_comments_when_missing_and_rejects_unwritable_field_names() {
        let mut input = b"fLaC".to_vec();
        input.extend(block(0, true, &stream_info()));
        input.extend_from_slice(b"audio");
        let mut output = Vec::new();
        rewrite_metadata(Cursor::new(&input), &mut output, &Metadata::default()).unwrap();
        let decoder = crate::FlacDecoder::new(Cursor::new(output.clone())).unwrap();
        assert_eq!(decoder.metadata_blocks().len(), 2);
        assert_eq!(decoder.metadata_blocks()[1].kind(), 4);
        assert_eq!(
            super::read_vendor(decoder.metadata_blocks()[1].data()).unwrap(),
            b"zemer"
        );
        assert_eq!(&output[audio_start(&output)..], b"audio");

        let mut invalid = Metadata::default();
        invalid.insert("bad=key", "value");
        assert!(matches!(
            rewrite_metadata(Cursor::new(input), Vec::new(), &invalid),
            Err(AudioError::InvalidData("invalid FLAC comment field name"))
        ));
    }
}
