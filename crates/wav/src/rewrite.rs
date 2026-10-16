use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    io::{Read, Seek, SeekFrom, Write},
};

use zemer_core::{AudioError, Metadata};

use crate::{
    BextChunk,
    cset::read_code_page,
    decode::{MAX_DS64_CHUNK, parse_ds64, read_info_metadata},
    encode::{
        metadata_chunk_size, metadata_chunk_size_for_code_page, write_info_chunk,
        write_info_subchunks,
    },
    info::is_managed_id,
    tags::{
        MAX_ID3_CHUNK_BYTES, encode_id3, id3_metadata, info_metadata, read_id3, write_chunk,
        write_chunk_with_id,
    },
};

const COPY_BUFFER_SIZE: usize = 64 * 1024;

struct Chunk {
    header: [u8; 8],
    size: u64,
    size_is_sentinel: bool,
    table_size_offset: Option<u64>,
}

/// Rewrites RIFF INFO and ID3 metadata while copying audio bytes as-is.
///
/// The caller should write to a new temporary file and replace the source only after success.
pub fn rewrite_metadata<R: Read + Seek, W: Write + Seek>(
    reader: R,
    writer: W,
    metadata: &Metadata,
) -> Result<W, AudioError> {
    rewrite_metadata_inner(reader, writer, metadata, None)
}

/// Rewrites WAVE INFO tags and replaces the first BWF `bext` chunk, or adds one if absent.
///
/// The caller should write to a new temporary file and replace the source only after success.
pub fn rewrite_metadata_with_bext<R: Read + Seek, W: Write + Seek>(
    reader: R,
    writer: W,
    metadata: &Metadata,
    bext: &BextChunk,
) -> Result<W, AudioError> {
    rewrite_metadata_inner(reader, writer, metadata, Some(bext))
}

fn rewrite_metadata_inner<R: Read + Seek, W: Write + Seek>(
    mut reader: R,
    mut writer: W,
    metadata: &Metadata,
    bext: Option<&BextChunk>,
) -> Result<W, AudioError> {
    let info = info_metadata(metadata);
    metadata_chunk_size(&info)?;
    if let Some(bext) = bext {
        bext.chunk_size()?;
    }
    let code_page = read_code_page(&mut reader)?;
    let original_metadata = read_info_metadata(&mut reader)?;
    let mut changed_keys = BTreeSet::new();
    let mut metadata_to_write = Metadata::default();
    for (key, values) in info.iter() {
        if original_metadata.values(key) != values {
            changed_keys.insert(key.to_owned());
            for value in values {
                metadata_to_write.insert(key, value.clone());
            }
        }
    }
    for (key, _) in original_metadata.iter() {
        if info.values(key).is_empty() {
            changed_keys.insert(key.to_owned());
        }
    }
    metadata_chunk_size_for_code_page(&metadata_to_write, code_page)?;
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
    let mut wrote_bext = false;
    let mut id3_seen = false;
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
            if !wrote_metadata
                && metadata_chunk_size_for_code_page(&metadata_to_write, code_page)? != 0
            {
                write_info_chunk(&mut writer, &metadata_to_write, code_page)?;
                wrote_metadata = true;
            }
        }
        if &chunk_header[..4] == b"bext" && !wrote_bext {
            if let Some(bext) = bext {
                reader.seek(SeekFrom::Start(padded_end))?;
                write_bext_chunk(&mut writer, bext, size32 == u32::MAX, table_size_offset)?;
                wrote_bext = true;
            } else {
                writer.write_all(&chunk_header)?;
                copy_exact(&mut reader, &mut writer, size)?;
                if size & 1 != 0 {
                    let mut pad = [0];
                    reader.read_exact(&mut pad)?;
                    writer.write_all(&pad)?;
                }
            }
        } else if matches!(&chunk_header[..4], b"id3 " | b"ID3 ") {
            if id3_seen || size > MAX_ID3_CHUNK_BYTES {
                return Err(AudioError::InvalidFormat(
                    "duplicate or oversized WAVE ID3 chunk",
                ));
            }
            id3_seen = true;
            let id = chunk_header[..4].try_into().unwrap();
            let tag = read_id3(&mut reader, size)?;
            if size & 1 != 0 {
                let mut pad = [0];
                reader.read_exact(&mut pad)?;
            }
            let rewritten = ::id3::rewrite_metadata_tag(&tag, &id3_metadata(metadata))?;
            if rewritten.len() as u64 > MAX_ID3_CHUNK_BYTES {
                return Err(AudioError::InvalidData(
                    "WAVE ID3 metadata exceeds the 16 MiB limit",
                ));
            }
            if !rewritten.is_empty() {
                write_id3_chunk(
                    &mut writer,
                    &id,
                    &rewritten,
                    size32 == u32::MAX,
                    table_size_offset,
                )?;
            }
        } else if &chunk_header[..4] == b"LIST" && size >= 4 {
            rewrite_list_chunk(
                &mut reader,
                &mut writer,
                Chunk {
                    header: chunk_header,
                    size,
                    size_is_sentinel: size32 == u32::MAX,
                    table_size_offset,
                },
                &metadata_to_write,
                &changed_keys,
                code_page,
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
    if !wrote_metadata && metadata_chunk_size_for_code_page(&info, code_page)? != 0 {
        write_info_chunk(&mut writer, &info, code_page)?;
    }
    if !id3_seen {
        if let Some(tag) = encode_id3(metadata)? {
            write_chunk(&mut writer, &tag)?;
        }
    }
    if !wrote_bext {
        if let Some(bext) = bext {
            write_bext_chunk(&mut writer, bext, false, None)?;
        }
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

fn write_id3_chunk<W: Write + Seek>(
    writer: &mut W,
    id: &[u8; 4],
    tag: &[u8],
    size_is_sentinel: bool,
    table_size_offset: Option<u64>,
) -> Result<(), AudioError> {
    if !size_is_sentinel {
        return write_chunk_with_id(writer, id, tag);
    }
    let size = u32::try_from(tag.len())
        .map_err(|_| AudioError::InvalidData("WAVE ID3 chunk is too large"))?;
    writer.write_all(id)?;
    writer.write_all(&u32::MAX.to_le_bytes())?;
    writer.write_all(tag)?;
    if size & 1 != 0 {
        writer.write_all(&[0])?;
    }
    let table_size_offset = table_size_offset.ok_or(AudioError::InvalidFormat(
        "RF64 ID3 size sentinel is missing its ds64 table entry",
    ))?;
    let end = writer.stream_position()?;
    writer.seek(SeekFrom::Start(table_size_offset))?;
    writer.write_all(&u64::from(size).to_le_bytes())?;
    writer.seek(SeekFrom::Start(end))?;
    Ok(())
}

fn write_bext_chunk<W: Write + Seek>(
    writer: &mut W,
    bext: &BextChunk,
    size_is_sentinel: bool,
    table_size_offset: Option<u64>,
) -> Result<(), AudioError> {
    let size = bext.chunk_size()?;
    writer.write_all(b"bext")?;
    writer.write_all(&if size_is_sentinel { u32::MAX } else { size }.to_le_bytes())?;
    bext.write(writer)?;
    if size & 1 != 0 {
        writer.write_all(&[0])?;
    }
    if size_is_sentinel {
        let table_size_offset = table_size_offset.ok_or(AudioError::InvalidData(
            "RF64 BWF size sentinel is missing its ds64 table entry",
        ))?;
        let output_end = writer.stream_position()?;
        writer.seek(SeekFrom::Start(table_size_offset))?;
        writer.write_all(&u64::from(size).to_le_bytes())?;
        writer.seek(SeekFrom::Start(output_end))?;
    }
    Ok(())
}

fn rewrite_list_chunk<R: Read + Seek, W: Write + Seek>(
    reader: &mut R,
    writer: &mut W,
    chunk: Chunk,
    metadata: &Metadata,
    changed_keys: &BTreeSet<String>,
    code_page: u16,
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
        let id = subchunk_header[..4].try_into().unwrap();
        if is_managed_id(&id)
            && crate::info::key_from_id(&id).is_some_and(|key| changed_keys.contains(key))
        {
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
        write_info_subchunks(writer, metadata, code_page)?;
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

    use zemer_core::{AudioDecoder, Metadata, Picture, Samples};

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
        chunk(&mut info, b"IENG", b"Ol\xff\0", 0);
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

    fn rf64_file_with_sentinel_id3() -> Vec<u8> {
        let mut riff = riff_file();
        let mut id3 = Metadata::default();
        id3.insert("ALBUM", "Album");
        id3.add_picture(Picture::new(3, "image/png", "front", 0, 0, 0, 0, [1, 2, 3]));
        let tag = ::id3::encode_metadata(&id3).unwrap();
        let mut id3_chunk = Vec::new();
        crate::tags::write_chunk(&mut id3_chunk, &tag).unwrap();
        riff.splice(riff.len() - 10..riff.len() - 10, id3_chunk);
        let riff_size = (riff.len() - 8) as u32;
        riff[4..8].copy_from_slice(&riff_size.to_le_bytes());
        let id3_offset = riff
            .windows(4)
            .position(|window| window == b"id3 ")
            .unwrap();
        riff[id3_offset + 4..id3_offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());

        let mut bytes = b"RF64\xff\xff\xff\xffWAVE".to_vec();
        let mut ds64 = Vec::new();
        ds64.extend_from_slice(&0_u64.to_le_bytes());
        ds64.extend_from_slice(&2_u64.to_le_bytes());
        ds64.extend_from_slice(&2_u64.to_le_bytes());
        ds64.extend_from_slice(&1_u32.to_le_bytes());
        ds64.extend_from_slice(b"id3 ");
        ds64.extend_from_slice(&(tag.len() as u64).to_le_bytes());
        chunk(&mut bytes, b"ds64", &ds64, 0);
        bytes.extend_from_slice(&riff[12..]);
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
    fn metadata_rewrite_replaces_all_registered_info_fields() {
        let original = riff_file();
        let mut metadata = Metadata::default();
        metadata.insert("engineer", "New engineer");

        let rewritten = rewrite_metadata(Cursor::new(original), Cursor::new(Vec::new()), &metadata)
            .unwrap()
            .into_inner();
        let decoder = WavDecoder::new(Cursor::new(rewritten.clone())).unwrap();
        assert_eq!(decoder.metadata().values("ENGINEER"), ["New engineer"]);
        assert!(rewritten.windows(4).any(|window| window == b"IENG"));
    }

    #[test]
    fn metadata_rewrite_preserves_unchanged_non_utf8_info_bytes() {
        let original = riff_file();
        let source = WavDecoder::new(Cursor::new(original.clone())).unwrap();
        assert_eq!(
            crate::decode::read_info_metadata(&mut Cursor::new(original.clone())).unwrap(),
            *source.metadata()
        );
        let mut metadata = source.metadata().clone();
        metadata.remove("TITLE");
        metadata.insert("TITLE", "Updated title");

        let rewritten = rewrite_metadata(Cursor::new(original), Cursor::new(Vec::new()), &metadata)
            .unwrap()
            .into_inner();
        let decoder = WavDecoder::new(Cursor::new(rewritten.clone())).unwrap();
        assert_eq!(decoder.metadata().values("TITLE"), ["Updated title"]);
        assert_eq!(decoder.metadata().values("ENGINEER"), ["Olÿ"]);
        let engineer = rewritten
            .windows(4)
            .position(|bytes| bytes == b"IENG")
            .unwrap();
        assert_eq!(
            &rewritten[engineer..engineer + 12],
            b"IENG\x04\0\0\0Ol\xff\0"
        );
    }

    #[test]
    fn reads_and_rewrites_uppercase_wave_id3_picture_chunks() {
        let mut original = riff_file();
        let mut embedded = Metadata::default();
        embedded.insert("ALBUM", "Original album");
        embedded.add_picture(Picture::new(3, "image/png", "front", 0, 0, 0, 0, [1, 2, 3]));
        let tag = ::id3::encode_metadata(&embedded).unwrap();
        let mut wave_id3 = Vec::new();
        crate::tags::write_chunk_with_id(&mut wave_id3, b"ID3 ", &tag).unwrap();
        original.splice(original.len() - 10..original.len() - 10, wave_id3);
        let riff_size = (original.len() - 8) as u32;
        original[4..8].copy_from_slice(&riff_size.to_le_bytes());

        let source = WavDecoder::new(Cursor::new(original.clone())).unwrap();
        assert_eq!(source.metadata().values("ALBUM"), ["Original album"]);
        assert_eq!(source.metadata().pictures().len(), 1);

        let mut updated = source.metadata().clone();
        updated.remove("ALBUM");
        updated.insert("ALBUM", "Updated album");
        let output = rewrite_metadata(Cursor::new(original), Cursor::new(Vec::new()), &updated)
            .unwrap()
            .into_inner();
        assert!(output.windows(4).any(|window| window == b"ID3 "));
        let decoded = WavDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoded.metadata().values("ALBUM"), ["Updated album"]);
        assert_eq!(decoded.metadata().pictures(), source.metadata().pictures());
    }

    #[test]
    fn updates_the_ds64_table_for_a_rewritten_sentinel_id3_chunk() {
        let original = rf64_file_with_sentinel_id3();
        let mut metadata = WavDecoder::new(Cursor::new(original.clone()))
            .unwrap()
            .metadata()
            .clone();
        metadata.remove("ALBUM");
        metadata.insert("ALBUM", "Longer album name");

        let output = rewrite_metadata(Cursor::new(original), Cursor::new(Vec::new()), &metadata)
            .unwrap()
            .into_inner();
        assert_eq!(&output[..4], b"RF64");
        assert_eq!(u32::from_le_bytes(output[44..48].try_into().unwrap()), 1);
        let id3 = output
            .windows(8)
            .position(|window| window == b"id3 \xff\xff\xff\xff")
            .unwrap();
        let table_size = u64::from_le_bytes(output[52..60].try_into().unwrap());
        let actual_size = ::id3::encode_metadata(&crate::tags::id3_metadata(&metadata))
            .unwrap()
            .len() as u64;
        assert_eq!(table_size, actual_size);
        let decoded = WavDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoded.metadata().values("ALBUM"), ["Longer album name"]);
        assert_eq!(decoded.metadata().pictures().len(), 1);
        assert!(id3 > 0);
    }

    #[test]
    fn metadata_rewrite_does_not_require_a_supported_audio_codec() {
        let mut original = riff_file();
        original[20..22].copy_from_slice(&0x1234_u16.to_le_bytes());
        let mut metadata =
            crate::decode::read_info_metadata(&mut Cursor::new(original.clone())).unwrap();
        metadata.remove("TITLE");
        metadata.insert("TITLE", "Updated title");

        let rewritten = rewrite_metadata(Cursor::new(original), Cursor::new(Vec::new()), &metadata)
            .unwrap()
            .into_inner();
        assert!(rewritten.windows(4).any(|window| window == b"INAM"));
        assert!(
            rewritten
                .windows(14)
                .any(|window| window == b"Updated title\0")
        );
        assert!(
            rewritten
                .windows(12)
                .any(|bytes| bytes == b"IENG\x04\0\0\0Ol\xff\0")
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
