use std::io::{Read, Seek, SeekFrom, Write};

use zemer_core::{AudioError, Metadata};

use crate::{MAX_ANNOTATION_BYTES, UNKNOWN_DATA_SIZE, decode::annotation_comment};

const COPY_BUFFER_SIZE: usize = 64 * 1024;

/// Rewrites the AU text annotation and copies the encoded audio without decoding it.
pub fn rewrite_metadata<R: Read + Seek, W: Write + Seek>(
    mut reader: R,
    mut writer: W,
    metadata: &Metadata,
) -> Result<W, AudioError> {
    validate_metadata(metadata)?;
    reader.seek(SeekFrom::Start(0))?;
    let mut header = [0; 24];
    reader.read_exact(&mut header)?;
    if &header[..4] != b".snd" {
        return Err(AudioError::InvalidFormat("expected an AU .snd header"));
    }
    let old_offset = u64::from(u32::from_be_bytes(header[4..8].try_into().unwrap()));
    let data_size = u32::from_be_bytes(header[8..12].try_into().unwrap());
    let file_end = reader.seek(SeekFrom::End(0))?;
    if old_offset < 24 || old_offset > file_end {
        return Err(AudioError::InvalidFormat(
            "AU data offset is outside the file",
        ));
    }
    if data_size != UNKNOWN_DATA_SIZE
        && old_offset
            .checked_add(u64::from(data_size))
            .is_none_or(|end| end > file_end)
    {
        return Err(AudioError::InvalidFormat("AU audio data exceeds the file"));
    }
    let annotation_len = usize::try_from(old_offset - 24)
        .map_err(|_| AudioError::InvalidFormat("AU annotation is too large"))?;
    if annotation_len > MAX_ANNOTATION_BYTES {
        return Err(AudioError::InvalidFormat("AU annotation exceeds 16 MiB"));
    }
    let mut old_annotation = Vec::new();
    old_annotation
        .try_reserve_exact(annotation_len)
        .map_err(|_| AudioError::InvalidFormat("AU annotation allocation failed"))?;
    old_annotation.resize(annotation_len, 0);
    reader.seek(SeekFrom::Start(24))?;
    reader.read_exact(&mut old_annotation)?;

    let old_comment = annotation_comment(&old_annotation);
    let new_comment = metadata.values("COMMENT").first().map(String::as_str);
    let annotation = if old_comment == new_comment {
        old_annotation
    } else {
        annotation_from_comment(new_comment)?
    };
    let new_offset = 24_usize
        .checked_add(annotation.len())
        .and_then(|size| u32::try_from(size).ok())
        .ok_or(AudioError::Unsupported(
            "AU annotation exceeds the 32-bit offset limit",
        ))?;
    header[4..8].copy_from_slice(&new_offset.to_be_bytes());

    writer.write_all(&header)?;
    writer.write_all(&annotation)?;
    reader.seek(SeekFrom::Start(old_offset))?;
    let mut remaining = file_end - old_offset;
    let mut buffer = [0_u8; COPY_BUFFER_SIZE];
    while remaining != 0 {
        let take = remaining.min(buffer.len() as u64) as usize;
        reader.read_exact(&mut buffer[..take])?;
        writer.write_all(&buffer[..take])?;
        remaining -= take as u64;
    }
    Ok(writer)
}

fn validate_metadata(metadata: &Metadata) -> Result<(), AudioError> {
    for (key, values) in metadata.iter() {
        if key != "COMMENT" {
            return Err(AudioError::Unsupported(
                "AU annotation supports only the COMMENT metadata field",
            ));
        }
        if values.len() > 1 {
            return Err(AudioError::Unsupported(
                "AU annotation supports one COMMENT value",
            ));
        }
    }
    if metadata
        .values("COMMENT")
        .iter()
        .any(|value| value.as_bytes().contains(&0))
    {
        return Err(AudioError::InvalidData("AU COMMENT cannot contain NUL"));
    }
    Ok(())
}

fn annotation_from_comment(comment: Option<&str>) -> Result<Vec<u8>, AudioError> {
    let comment = comment.unwrap_or("").as_bytes();
    if comment.len() > MAX_ANNOTATION_BYTES.saturating_sub(4) {
        return Err(AudioError::Unsupported("AU annotation exceeds 16 MiB"));
    }
    let mut annotation = Vec::new();
    annotation
        .try_reserve_exact(comment.len().saturating_add(4))
        .map_err(|_| AudioError::InvalidData("AU annotation allocation failed"))?;
    annotation.extend_from_slice(comment);
    annotation.push(0);
    annotation.resize(annotation.len().max(4), 0);
    let padding = (4 - (annotation.len() & 3)) & 3;
    annotation.resize(annotation.len() + padding, 0);
    Ok(annotation)
}
