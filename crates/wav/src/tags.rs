use std::io::{Read, Write};

use zemer_core::{AudioError, Metadata};

pub(super) const MAX_ID3_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

pub(super) fn info_metadata(metadata: &Metadata) -> Metadata {
    let mut info = Metadata::default();
    for (key, values) in metadata.iter() {
        if crate::supports_info_metadata_key(key) {
            for value in values {
                info.insert(key, value.clone());
            }
        }
    }
    info
}

pub(super) fn id3_metadata(metadata: &Metadata) -> Metadata {
    let mut id3_metadata = Metadata::default();
    for (key, values) in metadata.iter() {
        if !crate::supports_info_metadata_key(key) {
            for value in values {
                id3_metadata.insert(key, value.clone());
            }
        }
    }
    for picture in metadata.pictures() {
        id3_metadata.add_picture(picture.clone());
    }
    id3_metadata
}

pub(super) fn encode_id3(metadata: &Metadata) -> Result<Option<Vec<u8>>, AudioError> {
    let metadata = id3_metadata(metadata);
    if !metadata
        .iter()
        .any(|(_, values)| values.iter().any(|value| !value.is_empty()))
        && metadata.pictures().is_empty()
    {
        return Ok(None);
    }
    let tag = ::id3::encode_metadata(&metadata)?;
    if tag.len() as u64 > MAX_ID3_CHUNK_BYTES {
        return Err(AudioError::InvalidData(
            "WAVE ID3 metadata exceeds the 16 MiB limit",
        ));
    }
    Ok(Some(tag))
}

pub(super) fn read_id3<R: Read>(reader: &mut R, size: u64) -> Result<Vec<u8>, AudioError> {
    let size = usize::try_from(size)
        .ok()
        .filter(|&size| size <= MAX_ID3_CHUNK_BYTES as usize)
        .ok_or(AudioError::InvalidFormat(
            "WAVE ID3 chunk exceeds the 16 MiB limit",
        ))?;
    let mut tag = Vec::new();
    tag.try_reserve_exact(size)
        .map_err(|_| AudioError::InvalidData("WAVE ID3 allocation failed"))?;
    tag.resize(size, 0);
    reader.read_exact(&mut tag)?;
    Ok(tag)
}

pub(super) fn merge_id3(metadata: &mut Metadata, tag: &[u8]) -> Result<(), AudioError> {
    let parsed = ::id3::parse_id3_tag(tag)?;
    for (key, values) in parsed.iter() {
        if metadata.values(key).is_empty() {
            for value in values {
                metadata.insert(key, value.to_owned());
            }
        }
    }
    for picture in parsed.pictures() {
        metadata.add_picture(picture.clone());
    }
    Ok(())
}

pub(super) fn write_chunk<W: Write>(writer: &mut W, tag: &[u8]) -> Result<(), AudioError> {
    write_chunk_with_id(writer, b"id3 ", tag)
}

pub(super) fn write_chunk_with_id<W: Write>(
    writer: &mut W,
    id: &[u8; 4],
    tag: &[u8],
) -> Result<(), AudioError> {
    let size = u32::try_from(tag.len())
        .map_err(|_| AudioError::InvalidData("WAVE ID3 chunk is too large"))?;
    writer.write_all(id)?;
    writer.write_all(&size.to_le_bytes())?;
    writer.write_all(tag)?;
    if size & 1 != 0 {
        writer.write_all(&[0])?;
    }
    Ok(())
}
