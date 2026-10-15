use std::io::{Read, Seek, SeekFrom, Write};

use id3::{encode_metadata, rewrite_metadata_tag};
use zemer_core::{AudioError, Metadata};

use crate::encode::{add_metadata_bytes, id3_metadata, validate_metadata, write_text_chunk};

const COPY_BUFFER_SIZE: usize = 64 * 1024;

pub fn rewrite_metadata<R: Read + Seek, W: Write + Seek>(
    mut reader: R,
    mut writer: W,
    metadata: &Metadata,
) -> Result<(), AudioError> {
    validate_metadata(metadata)?;
    reader.seek(SeekFrom::Start(0))?;
    let mut form = [0; 12];
    reader.read_exact(&mut form)?;
    if &form[..4] != b"FORM" || !matches!(&form[8..12], b"AIFF" | b"AIFC") {
        return Err(AudioError::InvalidFormat(
            "expected a FORM AIFF or AIFC file",
        ));
    }
    let form_end = 8_u64
        .checked_add(u64::from(u32::from_be_bytes(
            form[4..8].try_into().unwrap(),
        )))
        .ok_or(AudioError::InvalidFormat("AIFF FORM size overflow"))?;
    let physical_end = reader.seek(SeekFrom::End(0))?;
    if form_end < 12 || form_end > physical_end {
        return Err(AudioError::InvalidFormat("AIFF FORM extends past the file"));
    }

    let output_start = writer.stream_position()?;
    writer.write_all(b"FORM\0\0\0\0")?;
    writer.write_all(&form[8..12])?;
    reader.seek(SeekFrom::Start(12))?;
    let mut id3_seen = false;
    let mut metadata_bytes = 0_u64;
    while reader.stream_position()? < form_end {
        let position = reader.stream_position()?;
        if form_end - position < 8 {
            return Err(AudioError::InvalidFormat("truncated AIFF chunk header"));
        }
        let mut header = [0; 8];
        reader.read_exact(&mut header)?;
        let size = u64::from(u32::from_be_bytes(header[4..8].try_into().unwrap()));
        let body_start = reader.stream_position()?;
        let chunk_end = body_start
            .checked_add(size)
            .and_then(|end| end.checked_add(size & 1))
            .ok_or(AudioError::InvalidFormat("AIFF chunk size overflow"))?;
        if chunk_end > form_end {
            return Err(AudioError::InvalidFormat("AIFF chunk extends past FORM"));
        }
        if &header[..4] == b"ID3 " {
            if id3_seen || size == 0 || size > 16 * 1024 * 1024 {
                return Err(AudioError::InvalidFormat(
                    "invalid or duplicate AIFF ID3 chunk",
                ));
            }
            id3_seen = true;
            let tag_len = usize::try_from(size)
                .map_err(|_| AudioError::InvalidFormat("AIFF ID3 tag is too large"))?;
            let mut tag = Vec::new();
            tag.try_reserve_exact(tag_len)
                .map_err(|_| AudioError::InvalidFormat("AIFF ID3 tag is too large"))?;
            tag.resize(tag_len, 0);
            reader.read_exact(&mut tag)?;
            reader.seek(SeekFrom::Start(chunk_end))?;
            let tag = rewrite_metadata_tag(&tag, metadata)?;
            if !tag.is_empty() {
                metadata_bytes = add_metadata_bytes(metadata_bytes, tag.len() as u64)?;
                write_id3_chunk(&mut writer, &tag)?;
            }
        } else if matches!(&header[..4], b"NAME" | b"AUTH" | b"ANNO") {
            reader.seek(SeekFrom::Start(chunk_end))?;
        } else {
            if matches!(&header[..4], b"COMT" | b"MARK") {
                metadata_bytes = add_metadata_bytes(metadata_bytes, size)?;
            }
            writer.write_all(&header)?;
            copy_exact(&mut reader, &mut writer, size + (size & 1))?;
        }
    }
    for (key, id) in [
        ("title", *b"NAME"),
        ("artist", *b"AUTH"),
        ("comment", *b"ANNO"),
    ] {
        let values = metadata.values(key);
        if values.iter().all(|value| value.is_ascii()) {
            for value in values {
                metadata_bytes = add_metadata_bytes(
                    metadata_bytes,
                    8 + value.len() as u64 + (value.len() & 1) as u64,
                )?;
                write_text_chunk(&mut writer, &id, value)?;
            }
        }
    }
    if !id3_seen {
        let tag = encode_metadata(&id3_metadata(metadata))?;
        if !tag.is_empty() {
            add_metadata_bytes(metadata_bytes, tag.len() as u64)?;
            write_id3_chunk(&mut writer, &tag)?;
        }
    }
    let rewritten_form_end = writer.stream_position()?;
    let size_offset = output_start
        .checked_add(4)
        .ok_or(AudioError::InvalidData("AIFF output position overflow"))?;
    let form_data_offset = output_start
        .checked_add(8)
        .ok_or(AudioError::InvalidData("AIFF output position overflow"))?;
    let form_size = rewritten_form_end
        .checked_sub(form_data_offset)
        .filter(|size| *size <= u64::from(u32::MAX))
        .ok_or(AudioError::Unsupported(
            "AIFF exceeds the 32-bit FORM size limit",
        ))?;
    writer.seek(SeekFrom::Start(size_offset))?;
    writer.write_all(&(form_size as u32).to_be_bytes())?;
    writer.seek(SeekFrom::Start(rewritten_form_end))?;
    reader.seek(SeekFrom::Start(form_end))?;
    copy_exact(&mut reader, &mut writer, physical_end - form_end)?;
    Ok(())
}

fn write_id3_chunk(writer: &mut impl Write, tag: &[u8]) -> Result<(), AudioError> {
    if tag.len() > 16 * 1024 * 1024 {
        return Err(AudioError::InvalidData("AIFF ID3 tag exceeds 16 MiB"));
    }
    let size = u32::try_from(tag.len())
        .map_err(|_| AudioError::InvalidData("AIFF ID3 tag exceeds its size limit"))?;
    writer.write_all(b"ID3 ")?;
    writer.write_all(&size.to_be_bytes())?;
    writer.write_all(tag)?;
    if size & 1 != 0 {
        writer.write_all(&[0])?;
    }
    Ok(())
}

fn copy_exact(
    reader: &mut impl Read,
    writer: &mut impl Write,
    mut remaining: u64,
) -> Result<(), AudioError> {
    let mut buffer = [0; COPY_BUFFER_SIZE];
    while remaining != 0 {
        let count = usize::try_from(remaining.min(COPY_BUFFER_SIZE as u64)).unwrap();
        reader.read_exact(&mut buffer[..count])?;
        writer.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    Ok(())
}
