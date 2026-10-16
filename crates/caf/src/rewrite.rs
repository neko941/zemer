use std::io::{Read, Seek, SeekFrom, Write};

use zemer_core::{AudioError, Metadata};

use crate::{
    CafDecoder,
    info::{MAX_INFO_BYTES, write_info},
};

pub fn rewrite_metadata<R: Read + Seek, W: Write + Seek>(
    mut reader: R,
    mut writer: W,
    metadata: &Metadata,
) -> Result<(), AudioError> {
    let _ = CafDecoder::new(&mut reader)?;
    let end = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    let mut file_header = [0; 8];
    reader.read_exact(&mut file_header)?;
    writer.write_all(&file_header)?;
    let mut cursor = 8_u64;
    while cursor < end {
        reader.seek(SeekFrom::Start(cursor))?;
        let mut header = [0; 12];
        reader.read_exact(&mut header)?;
        let size = i64::from_be_bytes(header[4..12].try_into().unwrap());
        let body = cursor + 12;
        let size = if size == -1 {
            end - body
        } else {
            u64::try_from(size).map_err(|_| AudioError::InvalidFormat("negative CAF chunk size"))?
        };
        let chunk_end = body
            .checked_add(size)
            .filter(|&chunk_end| chunk_end <= end)
            .ok_or(AudioError::InvalidFormat("CAF chunk extends past the file"))?;
        if &header[..4] == b"info" {
            if size > MAX_INFO_BYTES {
                return Err(AudioError::InvalidFormat("CAF info chunk exceeds 16 MiB"));
            }
        } else {
            writer.write_all(&header[..4])?;
            writer.write_all(
                &i64::try_from(size)
                    .map_err(|_| AudioError::Unsupported("CAF chunk exceeds size limit"))?
                    .to_be_bytes(),
            )?;
            reader.seek(SeekFrom::Start(body))?;
            copy_bounded(&mut reader, &mut writer, size)?;
        }
        cursor = chunk_end;
    }
    write_info(&mut writer, metadata)?;
    writer.flush()?;
    Ok(())
}

fn copy_bounded<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    mut remaining: u64,
) -> Result<(), AudioError> {
    let mut buffer = [0; 64 * 1024];
    while remaining > 0 {
        let count = usize::try_from(remaining.min(buffer.len() as u64)).unwrap();
        reader.read_exact(&mut buffer[..count])?;
        writer.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    Ok(())
}
