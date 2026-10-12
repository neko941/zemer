use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Seek, SeekFrom, Write},
};

use zemer_core::{AudioError, Metadata};

use crate::{
    decode::{MAX_DS64_CHUNK, parse_ds64},
    encode::{is_managed_info_id, metadata_chunk_size, write_info_chunk, write_info_subchunks},
};

const COPY_BUFFER_SIZE: usize = 64 * 1024;

struct Chunk {
    header: [u8; 8],
    size: u64,
    size_is_sentinel: bool,
    table_size_offset: Option<u64>,
}

/// Rewrites RIFF INFO tags while copying all other WAVE chunk and audio bytes unchanged.
///
/// The caller should write to a new temporary file and replace the source only after success.
pub fn rewrite_metadata<R: Read + Seek, W: Write + Seek>(
    mut reader: R,
    mut writer: W,
    metadata: &Metadata,
) -> Result<W, AudioError> {
    metadata_chunk_size(metadata)?;
    reader.seek(SeekFrom::Start(0))?;
    let mut header = [0; 12];
    reader.read_exact(&mut header)?;
    let rf64 = match &header[..4] {
        b"RIFF" => false,
        b"RF64" => true,
        _ => {
            return Err(AudioError::InvalidFormat(
                "expected a RIFF/WAVE or RF64/WAVE file",
            ));
        }
    };
    if &header[8..] != b"WAVE" {
        return Err(AudioError::InvalidFormat("expected a WAVE file form"));
    }
    let declared_size = u32::from_le_bytes(header[4..8].try_into().unwrap());
    if rf64 != (declared_size == u32::MAX) {
        return Err(AudioError::InvalidFormat("invalid RIFF/RF64 size field"));
    }

    let output_start = writer.stream_position()?;
    writer.write_all(&header)?;
    let mut ds64 = None;
    let mut ds64_table_offsets = HashMap::<[u8; 4], VecDeque<u64>>::new();
    let ds64_riff_offset = if rf64 {
        let mut ds64_header = [0; 8];
        reader.read_exact(&mut ds64_header)?;
        if &ds64_header[..4] != b"ds64" {
            return Err(AudioError::InvalidFormat(
                "RF64 requires ds64 as the first chunk",
            ));
        }
        let size = u32::from_le_bytes(ds64_header[4..].try_into().unwrap());
        if !(28..=MAX_DS64_CHUNK).contains(&size) {
            return Err(AudioError::InvalidFormat("invalid RF64 ds64 chunk size"));
        }
        let mut bytes = vec![0; size as usize];
        reader.read_exact(&mut bytes)?;
        ds64 = Some(parse_ds64(&bytes)?);
        writer.write_all(&ds64_header)?;
        let body_offset = writer.stream_position()?;
        writer.write_all(&bytes)?;
        for (index, entry) in bytes[28..].chunks_exact(12).enumerate() {
            let id = entry[..4].try_into().unwrap();
            let size_offset = body_offset + 28 + (index as u64 * 12) + 4;
            ds64_table_offsets
                .entry(id)
                .or_default()
                .push_back(size_offset);
        }
        if size & 1 != 0 {
            let mut pad = [0];
            reader.read_exact(&mut pad)?;
            writer.write_all(&pad)?;
        }
        Some(body_offset)
    } else {
        None
    };
    let chunk_scan_start = reader.stream_position()?;

    let riff_size = if rf64 {
        ds64.as_ref().unwrap().riff_size
    } else {
        u64::from(declared_size)
    };
    let riff_end = 8_u64
        .checked_add(riff_size)
        .ok_or(AudioError::InvalidFormat("RIFF size overflow"))?;
    let file_end = reader.seek(SeekFrom::End(0))?;
    if riff_end > file_end || riff_end < 12 {
        return Err(AudioError::InvalidFormat("RIFF size exceeds the file"));
    }
    reader.seek(SeekFrom::Start(chunk_scan_start))?;

    let mut wrote_metadata = false;
    let mut first_data_seen = false;
    loop {
        let position = reader.stream_position()?;
        if position == riff_end {
            break;
        }
        if position > riff_end || riff_end - position < 8 {
            return Err(AudioError::InvalidFormat("truncated RIFF chunk header"));
        }
        let mut chunk_header = [0; 8];
        reader.read_exact(&mut chunk_header)?;
        let size32 = u32::from_le_bytes(chunk_header[4..].try_into().unwrap());
        let mut table_size_offset = None;
        let size = if size32 != u32::MAX {
            u64::from(size32)
        } else if rf64 && &chunk_header[..4] == b"data" && !first_data_seen {
            ds64.as_ref().unwrap().data_size
        } else if rf64 {
            let size = ds64
                .as_mut()
                .and_then(|info| info.take_table_size(&chunk_header[..4]))
                .ok_or(AudioError::InvalidFormat(
                    "RF64 chunk size sentinel has no ds64 table entry",
                ))?;
            let id: [u8; 4] = chunk_header[..4].try_into().unwrap();
            table_size_offset = ds64_table_offsets
                .get_mut(&id)
                .and_then(VecDeque::pop_front);
            if table_size_offset.is_none() {
                return Err(AudioError::InvalidFormat(
                    "RF64 chunk size table is inconsistent",
                ));
            }
            size
        } else {
            return Err(AudioError::InvalidFormat(
                "RIFF chunk size sentinel without RF64 ds64",
            ));
        };
        let body_start = reader.stream_position()?;
        let body_end = body_start
            .checked_add(size)
            .ok_or(AudioError::InvalidFormat("chunk size overflow"))?;
        let padded_end = body_end
            .checked_add(size & 1)
            .ok_or(AudioError::InvalidFormat("chunk padding overflow"))?;
        if padded_end > riff_end {
            return Err(AudioError::InvalidFormat(
                "chunk extends past the RIFF boundary",
            ));
        }

        if &chunk_header[..4] == b"data" {
            first_data_seen = true;
            if !wrote_metadata && metadata_chunk_size(metadata)? != 0 {
                write_info_chunk(&mut writer, metadata)?;
                wrote_metadata = true;
            }
        }
        if &chunk_header[..4] == b"LIST" && size >= 4 {
            rewrite_list_chunk(
                &mut reader,
                &mut writer,
                Chunk {
                    header: chunk_header,
                    size,
                    size_is_sentinel: size32 == u32::MAX,
                    table_size_offset,
                },
                metadata,
                &mut wrote_metadata,
            )?;
        } else {
            writer.write_all(&chunk_header)?;
            copy_exact(&mut reader, &mut writer, size)?;
            if size & 1 != 0 {
                let mut pad = [0];
                reader.read_exact(&mut pad)?;
                writer.write_all(&pad)?;
            }
        }
        if reader.stream_position()? != padded_end {
            return Err(AudioError::InvalidData(
                "WAVE metadata rewrite lost chunk alignment",
            ));
        }
    }
    if !wrote_metadata && metadata_chunk_size(metadata)? != 0 {
        write_info_chunk(&mut writer, metadata)?;
    }

    let output_riff_end = writer.stream_position()?;
    let new_riff_size = output_riff_end
        .checked_sub(output_start + 8)
        .ok_or(AudioError::InvalidData("invalid rewritten RIFF size"))?;
    if rf64 {
        writer.seek(SeekFrom::Start(ds64_riff_offset.unwrap()))?;
        writer.write_all(&new_riff_size.to_le_bytes())?;
    } else {
        let size = u32::try_from(new_riff_size)
            .map_err(|_| AudioError::Unsupported("rewritten RIFF/WAVE exceeds 4 GiB"))?;
        writer.seek(SeekFrom::Start(output_start + 4))?;
        writer.write_all(&size.to_le_bytes())?;
    }
    writer.seek(SeekFrom::Start(output_riff_end))?;
    reader.seek(SeekFrom::Start(riff_end))?;
    copy_exact(&mut reader, &mut writer, file_end - riff_end)?;
    Ok(writer)
}

fn rewrite_list_chunk<R: Read + Seek, W: Write + Seek>(
    reader: &mut R,
    writer: &mut W,
    chunk: Chunk,
    metadata: &Metadata,
    wrote_metadata: &mut bool,
) -> Result<(), AudioError> {
    let body_start = reader.stream_position()?;
    let size = chunk.size;
    let mut list_type = [0; 4];
    reader.read_exact(&mut list_type)?;
    if &list_type != b"INFO" {
        writer.write_all(&chunk.header)?;
        writer.write_all(&list_type)?;
        copy_exact(reader, writer, size - 4)?;
        if size & 1 != 0 {
            let mut pad = [0];
            reader.read_exact(&mut pad)?;
            writer.write_all(&pad)?;
        }
        return Ok(());
    }

    writer.write_all(b"LIST")?;
    let size_offset = writer.stream_position()?;
    writer.write_all(&if chunk.size_is_sentinel {
        u32::MAX.to_le_bytes()
    } else {
        0_u32.to_le_bytes()
    })?;
    writer.write_all(b"INFO")?;
    let mut remaining = size - 4;
    while remaining != 0 {
        if remaining < 8 {
            return Err(AudioError::InvalidFormat("truncated INFO subchunk header"));
        }
        let mut subchunk_header = [0; 8];
        reader.read_exact(&mut subchunk_header)?;
        let subchunk_size = u32::from_le_bytes(subchunk_header[4..].try_into().unwrap());
        let padded_size = u64::from(subchunk_size) + u64::from(subchunk_size & 1);
        let full_size = 8_u64 + padded_size;
        if full_size > remaining {
            return Err(AudioError::InvalidFormat(
                "INFO subchunk exceeds LIST boundary",
            ));
        }
        if is_managed_info_id(subchunk_header[..4].try_into().unwrap()) {
            reader.seek(SeekFrom::Current(padded_size as i64))?;
        } else {
            writer.write_all(&subchunk_header)?;
            copy_exact(reader, writer, u64::from(subchunk_size))?;
            if subchunk_size & 1 != 0 {
                let mut pad = [0];
                reader.read_exact(&mut pad)?;
                writer.write_all(&pad)?;
            }
        }
        remaining -= full_size;
    }
    if !*wrote_metadata {
        write_info_subchunks(writer, metadata)?;
        *wrote_metadata = true;
    }
    let output_end = writer.stream_position()?;
    let list_size = output_end
        .checked_sub(size_offset + 4)
        .ok_or(AudioError::InvalidData("invalid rewritten INFO size"))?;
    let list_size = u32::try_from(list_size)
        .map_err(|_| AudioError::Unsupported("rewritten INFO list exceeds the RIFF size limit"))?;
    if chunk.size_is_sentinel {
        let table_offset = chunk.table_size_offset.ok_or(AudioError::InvalidFormat(
            "RF64 INFO size sentinel is missing its ds64 table entry",
        ))?;
        writer.seek(SeekFrom::Start(table_offset))?;
        writer.write_all(&u64::from(list_size).to_le_bytes())?;
    } else {
        writer.seek(SeekFrom::Start(size_offset))?;
        writer.write_all(&list_size.to_le_bytes())?;
    }
    writer.seek(SeekFrom::Start(output_end))?;
    if list_size & 1 != 0 {
        writer.write_all(&[0])?;
    }
    let input_end = body_start
        .checked_add(size)
        .and_then(|end| end.checked_add(size & 1))
        .ok_or(AudioError::InvalidFormat("LIST chunk size overflow"))?;
    reader.seek(SeekFrom::Start(input_end))?;
    Ok(())
}

fn copy_exact<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    mut remaining: u64,
) -> Result<(), AudioError> {
    let mut buffer = [0; COPY_BUFFER_SIZE];
    while remaining != 0 {
        let count = usize::try_from(remaining.min(buffer.len() as u64)).unwrap();
        reader.read_exact(&mut buffer[..count])?;
        writer.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zemer_core::{AudioDecoder, Metadata, Samples};

    use super::rewrite_metadata;
    use crate::WavDecoder;

    fn chunk(bytes: &mut Vec<u8>, id: &[u8; 4], body: &[u8], pad: u8) {
        bytes.extend_from_slice(id);
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(body);
        if body.len() & 1 != 0 {
            bytes.push(pad);
        }
    }

    fn riff_file() -> Vec<u8> {
        let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1_u16.to_le_bytes());
        fmt.extend_from_slice(&1_u16.to_le_bytes());
        fmt.extend_from_slice(&8_000_u32.to_le_bytes());
        fmt.extend_from_slice(&8_000_u32.to_le_bytes());
        fmt.extend_from_slice(&1_u16.to_le_bytes());
        fmt.extend_from_slice(&8_u16.to_le_bytes());
        chunk(&mut bytes, b"fmt ", &fmt, 0);
        chunk(&mut bytes, b"JUNK", &[0x31, 0x32, 0x33], 0xa5);
        let mut info = b"INFO".to_vec();
        chunk(&mut info, b"INAM", b"Old\0", 0);
        chunk(&mut info, b"XTRA", &[0x41, 0x42, 0x43], 0xcc);
        chunk(&mut bytes, b"LIST", &info, 0);
        chunk(&mut bytes, b"data", &[0x80, 0xff], 0);
        let riff_size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
        bytes
    }

    fn rf64_file() -> Vec<u8> {
        let riff = riff_file();
        let mut bytes = b"RF64\xff\xff\xff\xffWAVE".to_vec();
        let mut ds64 = Vec::new();
        ds64.extend_from_slice(&((riff.len() - 8 + 36) as u64).to_le_bytes());
        ds64.extend_from_slice(&2_u64.to_le_bytes());
        ds64.extend_from_slice(&2_u64.to_le_bytes());
        ds64.extend_from_slice(&0_u32.to_le_bytes());
        chunk(&mut bytes, b"ds64", &ds64, 0);
        bytes.extend_from_slice(&riff[12..]);
        let riff_size = (bytes.len() - 8) as u64;
        bytes[20..28].copy_from_slice(&riff_size.to_le_bytes());
        bytes
    }

    fn rf64_file_with_sentinel_info() -> Vec<u8> {
        let mut bytes = rf64_file();
        let list_header = info_list_header(&bytes);
        let list_size =
            u32::from_le_bytes(bytes[list_header + 4..list_header + 8].try_into().unwrap());
        bytes[16..20].copy_from_slice(&40_u32.to_le_bytes());
        bytes[44..48].copy_from_slice(&1_u32.to_le_bytes());
        bytes.splice(48..48, b"LIST\x00\x00\x00\x00\0\0\0\0".iter().copied());
        bytes[48..52].copy_from_slice(b"LIST");
        bytes[52..60].copy_from_slice(&u64::from(list_size).to_le_bytes());
        let list_header = info_list_header(&bytes);
        bytes[list_header + 4..list_header + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        let riff_size = (bytes.len() - 8) as u64;
        bytes[20..28].copy_from_slice(&riff_size.to_le_bytes());
        bytes
    }

    fn info_list_header(bytes: &[u8]) -> usize {
        bytes
            .windows(12)
            .position(|window| &window[..4] == b"LIST" && &window[8..12] == b"INFO")
            .unwrap()
    }

    fn rewritten(input: Vec<u8>) -> Vec<u8> {
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "New");
        metadata.insert("ARTIST", "Artist");
        rewrite_metadata(Cursor::new(input), Cursor::new(Vec::new()), &metadata)
            .unwrap()
            .into_inner()
    }

    #[test]
    fn rewrites_info_tags_and_preserves_unknown_chunks_and_audio() {
        let input = riff_file();
        let output = rewritten(input.clone());
        assert!(
            output
                .windows(12)
                .any(|bytes| bytes == b"JUNK\x03\0\0\0\x31\x32\x33\xa5")
        );
        assert!(
            output
                .windows(12)
                .any(|bytes| bytes == b"XTRA\x03\0\0\0\x41\x42\x43\xcc")
        );
        let input_data = input
            .windows(6)
            .position(|bytes| bytes == b"data\x02\0")
            .unwrap();
        assert_eq!(&input[input_data + 8..input_data + 10], &[0x80, 0xff]);
        let data = output
            .windows(6)
            .position(|bytes| bytes == b"data\x02\0")
            .unwrap();
        assert_eq!(&output[data + 8..data + 10], &[0x80, 0xff]);
        assert_eq!(
            u32::from_le_bytes(output[4..8].try_into().unwrap()) as usize + 8,
            output.len()
        );
        let mut decoder = WavDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoder.metadata().values("TITLE"), ["New"]);
        assert_eq!(decoder.metadata().values("ARTIST"), ["Artist"]);
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::U8(vec![0x80, 0xff])
        );
    }

    #[test]
    fn updates_rf64_riff_size_and_keeps_decoding() {
        let output = rewritten(rf64_file());
        assert_eq!(&output[..4], b"RF64");
        assert_eq!(
            u32::from_le_bytes(output[4..8].try_into().unwrap()),
            u32::MAX
        );
        assert_eq!(
            u64::from_le_bytes(output[20..28].try_into().unwrap()) as usize + 8,
            output.len()
        );
        let mut decoder = WavDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoder.container(), crate::WavContainer::Rf64);
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::U8(vec![0x80, 0xff])
        );
    }

    #[test]
    fn updates_the_ds64_table_for_a_rewritten_sentinel_info_chunk() {
        let output = rewritten(rf64_file_with_sentinel_info());
        assert_eq!(u32::from_le_bytes(output[16..20].try_into().unwrap()), 40);
        let table_size = u64::from_le_bytes(output[52..60].try_into().unwrap());
        let list_header = info_list_header(&output);
        assert_eq!(
            u32::from_le_bytes(output[list_header + 4..list_header + 8].try_into().unwrap()),
            u32::MAX
        );
        let mut decoder = WavDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoder.metadata().values("TITLE"), ["New"]);
        assert_eq!(decoder.metadata().values("ARTIST"), ["Artist"]);
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::U8(vec![0x80, 0xff])
        );
        assert_eq!(table_size, 44);
    }
}
