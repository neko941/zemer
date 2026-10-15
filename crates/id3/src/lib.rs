use std::io::{Read, Seek, SeekFrom};

use zemer_core::{AudioError, Metadata};

const MAX_ID3_TAG_BYTES: usize = 64 * 1024 * 1024;

pub fn encode_metadata(metadata: &Metadata) -> Result<Vec<u8>, AudioError> {
    if metadata.iter().next().is_none() {
        return Ok(Vec::new());
    }
    let mut body = Vec::new();
    for (key, values) in metadata.iter() {
        if values.is_empty() {
            continue;
        }
        if key.is_empty()
            || key.as_bytes().contains(&0)
            || values.iter().any(|value| value.as_bytes().contains(&0))
        {
            return Err(AudioError::InvalidData("ID3 text cannot contain NUL bytes"));
        }
        let frame_id = mapped_frame_id(key);
        let text_size = values.iter().try_fold(0_usize, |size, value| {
            size.checked_add(value.len())
                .ok_or(AudioError::InvalidData("ID3 text frame size overflow"))
        })?;
        let payload_size = 1_usize
            .checked_add(usize::from(frame_id.is_none()) * (key.len() + 1))
            .and_then(|size| size.checked_add(text_size))
            .and_then(|size| size.checked_add(values.len().saturating_sub(1)))
            .ok_or(AudioError::InvalidData("ID3 text frame size overflow"))?;
        if payload_size
            .checked_add(10)
            .filter(|&size| size <= MAX_ID3_TAG_BYTES.saturating_sub(body.len()))
            .is_none()
        {
            return Err(AudioError::InvalidData(
                "ID3 metadata exceeds its size limit",
            ));
        }
        let mut payload = Vec::new();
        payload
            .try_reserve_exact(payload_size)
            .map_err(|_| AudioError::InvalidData("ID3 text frame is too large"))?;
        payload.push(3);
        if frame_id.is_none() {
            payload.extend_from_slice(key.as_bytes());
            payload.push(0);
        }
        for (index, value) in values.iter().enumerate() {
            if index != 0 {
                payload.push(0);
            }
            payload.extend_from_slice(value.as_bytes());
        }
        append_frame(&mut body, frame_id.unwrap_or(*b"TXXX"), &payload)?;
    }
    if body.len() > MAX_ID3_TAG_BYTES {
        return Err(AudioError::InvalidData(
            "ID3 metadata exceeds its size limit",
        ));
    }
    let size = u32::try_from(body.len())
        .ok()
        .filter(|&size| size < (1 << 28))
        .ok_or(AudioError::InvalidData(
            "ID3 metadata exceeds its size limit",
        ))?;
    let mut tag = Vec::new();
    tag.try_reserve_exact(10 + body.len())
        .map_err(|_| AudioError::InvalidData("ID3 metadata is too large"))?;
    tag.extend_from_slice(b"ID3\x04\x00\x00");
    tag.extend_from_slice(&[
        ((size >> 21) & 0x7f) as u8,
        ((size >> 14) & 0x7f) as u8,
        ((size >> 7) & 0x7f) as u8,
        (size & 0x7f) as u8,
    ]);
    tag.extend_from_slice(&body);
    Ok(tag)
}

pub fn rewrite_metadata_tag(tag: &[u8], metadata: &Metadata) -> Result<Vec<u8>, AudioError> {
    let mut reader = std::io::Cursor::new(tag);
    let (parsed, _) = read_tag_at_end(&mut reader, 0, tag.len() as u64)?;
    if parsed != *metadata {
        let header = tag
            .get(..10)
            .filter(|header| &header[..3] == b"ID3")
            .ok_or(AudioError::InvalidFormat("invalid ID3 tag header"))?;
        let version = header[3];
        let flags = header[5];
        if version == 2 && flags & 0x40 != 0 {
            return Err(AudioError::Unsupported(
                "cannot edit compressed ID3v2.2 tags",
            ));
        }
        let body_size = syncsafe(&header[6..10])? as usize;
        let body_end = 10_usize
            .checked_add(body_size)
            .filter(|&end| end <= tag.len())
            .ok_or(AudioError::InvalidFormat("ID3 tag exceeds its boundary"))?;
        let mut body = tag[10..body_end].to_vec();
        if flags & 0x80 != 0 {
            body = unsynchronize(&body);
        }
        let frame_start = extended_header_size(&body, version, flags)?;
        let opaque = retain_opaque_frames(&body[frame_start..], version)?;
        let mut output_body = opaque;
        encode_metadata_frames(&mut output_body, metadata, version)?;
        if output_body.is_empty() {
            return Ok(Vec::new());
        }
        if output_body.len() > MAX_ID3_TAG_BYTES {
            return Err(AudioError::InvalidData(
                "ID3 metadata exceeds its size limit",
            ));
        }
        let size = u32::try_from(output_body.len())
            .ok()
            .filter(|&size| size < (1 << 28))
            .ok_or(AudioError::InvalidData(
                "ID3 metadata exceeds its size limit",
            ))?;
        let output_flags = flags & if version == 4 { 0x30 } else { 0x20 };
        let mut output =
            Vec::with_capacity(10 + output_body.len() + usize::from(output_flags & 0x10 != 0) * 10);
        output.extend_from_slice(b"ID3");
        output.extend_from_slice(&[version, header[4], output_flags]);
        let encoded_size = [
            ((size >> 21) & 0x7f) as u8,
            ((size >> 14) & 0x7f) as u8,
            ((size >> 7) & 0x7f) as u8,
            (size & 0x7f) as u8,
        ];
        output.extend_from_slice(&encoded_size);
        output.extend_from_slice(&output_body);
        if output_flags & 0x10 != 0 {
            output.extend_from_slice(b"3DI");
            output.extend_from_slice(&[version, header[4], output_flags]);
            output.extend_from_slice(&encoded_size);
        }
        return Ok(output);
    }
    Ok(tag.to_vec())
}

fn retain_opaque_frames(body: &[u8], version: u8) -> Result<Vec<u8>, AudioError> {
    let mut output = Vec::new();
    let mut position = 0;
    let header_size = if version == 2 { 6 } else { 10 };
    let id_size = if version == 2 { 3 } else { 4 };
    while position < body.len() {
        let remaining = &body[position..];
        if remaining[0] == 0 {
            if remaining.iter().any(|&byte| byte != 0) {
                return Err(AudioError::InvalidFormat("invalid ID3 padding"));
            }
            break;
        }
        if remaining.len() < header_size {
            return Err(AudioError::InvalidFormat("truncated ID3 frame header"));
        }
        let id = &remaining[..id_size];
        let frame_size = match version {
            2 => {
                (usize::from(remaining[3]) << 16)
                    | (usize::from(remaining[4]) << 8)
                    | usize::from(remaining[5])
            }
            3 => u32::from_be_bytes(remaining[4..8].try_into().unwrap()) as usize,
            4 => syncsafe(&remaining[4..8])? as usize,
            _ => unreachable!(),
        };
        let end = header_size
            .checked_add(frame_size)
            .filter(|&end| end <= remaining.len())
            .ok_or(AudioError::InvalidFormat("ID3 frame exceeds tag boundary"))?;
        let text_tag = id[0] == b'T' && (mapped_key(id).is_some() || id == b"TXXX" || id == b"TXX");
        let compressed = match version {
            2 => false,
            3 => remaining[9] & 0xc0 != 0,
            4 => remaining[9] & 0x0c != 0,
            _ => unreachable!(),
        };
        if !text_tag || compressed {
            output.extend_from_slice(&remaining[..end]);
        }
        position += end;
    }
    Ok(output)
}

fn encode_metadata_frames(
    body: &mut Vec<u8>,
    metadata: &Metadata,
    version: u8,
) -> Result<(), AudioError> {
    for (key, values) in metadata.iter() {
        let (id, custom) = match version {
            2 => match key {
                "TITLE" => (*b"TT2 ", false),
                "ARTIST" => (*b"TP1 ", false),
                "ALBUM" => (*b"TAL ", false),
                "TRACKNUMBER" => (*b"TRK ", false),
                "DISCNUMBER" => (*b"TPA ", false),
                "GENRE" => (*b"TCO ", false),
                "DATE" => (*b"TYE ", false),
                "COMPOSER" => (*b"TCM ", false),
                "ALBUMARTIST" => (*b"TP2 ", false),
                "COPYRIGHT" => (*b"TCR ", false),
                "LYRICIST" => (*b"TXT ", false),
                "ENCODEDBY" => (*b"TEN ", false),
                "BPM" => (*b"TBP ", false),
                "ISRC" => (*b"TRC ", false),
                _ => (*b"TXX ", true),
            },
            _ => (
                mapped_frame_id(key).unwrap_or(*b"TXXX"),
                mapped_frame_id(key).is_none(),
            ),
        };
        if values.is_empty() {
            continue;
        }
        let text_strings = if custom {
            std::iter::once(key)
                .chain(values.iter().map(String::as_str))
                .collect::<Vec<_>>()
        } else {
            values.iter().map(String::as_str).collect::<Vec<_>>()
        };
        let latin1 = text_strings
            .iter()
            .all(|value| value.chars().all(|c| u32::from(c) <= 0xff));
        let encoding = if version == 4 {
            3
        } else if latin1 {
            0
        } else {
            1
        };
        let mut payload = vec![encoding];
        for (index, text) in text_strings.iter().enumerate() {
            if index != 0 {
                if encoding == 1 {
                    payload.extend_from_slice(&[0, 0]);
                } else {
                    payload.push(0);
                }
            }
            if encoding == 1 && index == 0 {
                payload.extend_from_slice(&[0xfe, 0xff]);
            }
            if encoding == 3 {
                payload.extend_from_slice(text.as_bytes());
            } else if encoding == 0 {
                payload.extend(text.chars().map(|c| c as u8));
            } else {
                for unit in text.encode_utf16() {
                    payload.extend_from_slice(&unit.to_be_bytes());
                }
            }
        }
        let size = payload.len();
        match version {
            2 => {
                if size >= (1 << 24) {
                    return Err(AudioError::InvalidData("ID3v2.2 text frame is too large"));
                }
                body.extend_from_slice(&id[..3]);
                body.extend_from_slice(&[(size >> 16) as u8, (size >> 8) as u8, size as u8]);
            }
            3 => {
                let size = u32::try_from(size)
                    .map_err(|_| AudioError::InvalidData("ID3v2.3 text frame is too large"))?;
                body.extend_from_slice(&id);
                body.extend_from_slice(&size.to_be_bytes());
                body.extend_from_slice(&[0, 0]);
            }
            4 => {
                if size >= (1 << 28) {
                    return Err(AudioError::InvalidData("ID3v2.4 text frame is too large"));
                }
                body.extend_from_slice(&id);
                body.extend_from_slice(&[
                    ((size >> 21) & 0x7f) as u8,
                    ((size >> 14) & 0x7f) as u8,
                    ((size >> 7) & 0x7f) as u8,
                    (size & 0x7f) as u8,
                    0,
                    0,
                ]);
            }
            _ => unreachable!(),
        }
        body.extend_from_slice(&payload);
        if body.len() > MAX_ID3_TAG_BYTES {
            return Err(AudioError::InvalidData(
                "ID3 metadata exceeds its size limit",
            ));
        }
    }
    Ok(())
}

fn append_frame(body: &mut Vec<u8>, id: [u8; 4], payload: &[u8]) -> Result<(), AudioError> {
    let size = u32::try_from(payload.len())
        .ok()
        .filter(|&size| size < (1 << 28))
        .ok_or(AudioError::InvalidData("ID3 text frame is too large"))?;
    let frame_size = payload
        .len()
        .checked_add(10)
        .filter(|&size| size <= MAX_ID3_TAG_BYTES.saturating_sub(body.len()))
        .ok_or(AudioError::InvalidData(
            "ID3 metadata exceeds its size limit",
        ))?;
    body.try_reserve_exact(frame_size)
        .map_err(|_| AudioError::InvalidData("ID3 metadata is too large"))?;
    body.extend_from_slice(&id);
    body.extend_from_slice(&[
        ((size >> 21) & 0x7f) as u8,
        ((size >> 14) & 0x7f) as u8,
        ((size >> 7) & 0x7f) as u8,
        (size & 0x7f) as u8,
        0,
        0,
    ]);
    body.extend_from_slice(payload);
    if body.len() > MAX_ID3_TAG_BYTES {
        return Err(AudioError::InvalidData(
            "ID3 metadata exceeds its size limit",
        ));
    }
    Ok(())
}

fn mapped_frame_id(key: &str) -> Option<[u8; 4]> {
    Some(*match key {
        "TITLE" => b"TIT2",
        "ARTIST" => b"TPE1",
        "ALBUM" => b"TALB",
        "TRACKNUMBER" => b"TRCK",
        "DISCNUMBER" => b"TPOS",
        "GENRE" => b"TCON",
        "DATE" => b"TDRC",
        "COMPOSER" => b"TCOM",
        "ALBUMARTIST" => b"TPE2",
        "COPYRIGHT" => b"TCOP",
        "LYRICIST" => b"TEXT",
        "ENCODEDBY" => b"TENC",
        "BPM" => b"TBPM",
        "ISRC" => b"TSRC",
        _ => return None,
    })
}

pub fn read_tag_at_end<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    file_size: u64,
) -> Result<(Metadata, Vec<u8>), AudioError> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut header = [0; 10];
    reader.read_exact(&mut header)?;
    if &header[..3] != b"ID3" {
        return Err(AudioError::InvalidFormat("invalid ID3 tag header"));
    }
    let version = header[3];
    let flags = header[5];
    let allowed_flags = match version {
        2 => 0xc0,
        3 => 0xe0,
        4 => 0xf0,
        _ => {
            return Err(AudioError::Unsupported(
                "ID3 metadata requires ID3v2.3 or ID3v2.4",
            ));
        }
    };
    if header[4] == 0xff || flags & !allowed_flags != 0 {
        return Err(AudioError::InvalidFormat(
            "invalid ID3 tag version or flags",
        ));
    }
    let size = syncsafe(&header[6..10])?;
    let footer_size = usize::from(version == 4 && flags & 0x10 != 0) * 10;
    let total_size = 10_u64
        .checked_add(u64::from(size))
        .and_then(|value| value.checked_add(footer_size as u64))
        .ok_or(AudioError::InvalidData("ID3 tag size overflow"))?;
    if size as usize > MAX_ID3_TAG_BYTES || offset.checked_add(total_size) != Some(file_size) {
        return Err(AudioError::InvalidData(
            "ID3 tag exceeds its bound or is not at end of file",
        ));
    }
    let mut body = Vec::new();
    body.try_reserve_exact(size as usize)
        .map_err(|_| AudioError::InvalidData("ID3 tag is too large"))?;
    body.resize(size as usize, 0);
    reader.read_exact(&mut body)?;
    let mut footer_bytes = Vec::new();
    if footer_size != 0 {
        let mut footer = [0; 10];
        reader.read_exact(&mut footer)?;
        if &footer[..3] != b"3DI" || footer[3..] != header[3..] {
            return Err(AudioError::InvalidFormat("invalid ID3v2.4 footer"));
        }
        footer_bytes.extend_from_slice(&footer);
    }
    if version == 2 && flags & 0x40 != 0 {
        // ID3v2.2 defines no compression scheme, so keep the entire tag opaque.
        let mut raw = Vec::with_capacity(10 + body.len());
        raw.extend_from_slice(&header);
        raw.extend_from_slice(&body);
        return Ok((Metadata::default(), raw));
    }
    if flags & 0x80 != 0 {
        body = unsynchronize(&body);
    }
    let frame_start = extended_header_size(&body, version, flags)?;
    let metadata = read_frames(
        &body[frame_start..],
        version,
        version == 4 && flags & 0x80 != 0,
    )?;
    let mut raw = Vec::with_capacity(10 + body.len() + footer_bytes.len());
    raw.extend_from_slice(&header);
    raw.extend_from_slice(&body);
    raw.extend_from_slice(&footer_bytes);
    Ok((metadata, raw))
}

pub fn parse_id3_tag(bytes: &[u8]) -> Result<Metadata, AudioError> {
    let mut reader = std::io::Cursor::new(bytes);
    read_tag_at_end(&mut reader, 0, bytes.len() as u64).map(|(metadata, _)| metadata)
}

fn extended_header_size(body: &[u8], version: u8, flags: u8) -> Result<usize, AudioError> {
    if flags & 0x40 == 0 {
        return Ok(0);
    }
    let size = match version {
        2 => 0,
        3 => {
            let size = body.get(..4).ok_or(AudioError::InvalidFormat(
                "truncated ID3v2.3 extended header",
            ))?;
            let size = u32::from_be_bytes(size.try_into().unwrap()) as usize;
            if !matches!(size, 6 | 10) {
                return Err(AudioError::InvalidFormat(
                    "invalid ID3v2.3 extended header size",
                ));
            }
            4 + size
        }
        4 => {
            let size = body.get(..4).ok_or(AudioError::InvalidFormat(
                "truncated ID3v2.4 extended header",
            ))?;
            syncsafe(size)? as usize
        }
        _ => unreachable!(),
    };
    if size > body.len() || size < 6 {
        return Err(AudioError::InvalidFormat(
            "invalid ID3 extended header boundary",
        ));
    }
    Ok(size)
}

fn read_frames(body: &[u8], version: u8, tag_unsynchronized: bool) -> Result<Metadata, AudioError> {
    let mut metadata = Metadata::default();
    let mut position = 0;
    while position < body.len() {
        let remaining = &body[position..];
        if remaining[0] == 0 {
            if remaining.iter().any(|&byte| byte != 0) {
                return Err(AudioError::InvalidFormat("invalid ID3 padding"));
            }
            break;
        }
        let header_size = if version == 2 { 6 } else { 10 };
        let id_size = if version == 2 { 3 } else { 4 };
        if remaining.len() < header_size {
            return Err(AudioError::InvalidFormat("truncated ID3 frame header"));
        }
        let id = &remaining[..id_size];
        if !id
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return Err(AudioError::InvalidFormat("invalid ID3 frame identifier"));
        }
        let frame_size = match version {
            2 => {
                (usize::from(remaining[3]) << 16)
                    | (usize::from(remaining[4]) << 8)
                    | usize::from(remaining[5])
            }
            3 => u32::from_be_bytes(remaining[4..8].try_into().unwrap()) as usize,
            4 => syncsafe(&remaining[4..8])? as usize,
            _ => unreachable!(),
        };
        if frame_size == 0 {
            return Err(AudioError::InvalidFormat("empty ID3 frame"));
        }
        let end = position
            .checked_add(header_size)
            .and_then(|value| value.checked_add(frame_size))
            .filter(|&value| value <= body.len())
            .ok_or(AudioError::InvalidFormat("ID3 frame exceeds tag boundary"))?;
        let status_flags = if version == 2 { 0 } else { remaining[8] };
        let format_flags = if version == 2 { 0 } else { remaining[9] };
        let (compress_or_encrypt, grouped, frame_unsynchronized, data_length_indicator) =
            match version {
                2 => (false, false, false, false),
                3 => {
                    if status_flags & !0xe0 != 0 || format_flags & !0xe0 != 0 {
                        return Err(AudioError::InvalidFormat("invalid ID3v2.3 frame flags"));
                    }
                    (
                        format_flags & 0xc0 != 0,
                        format_flags & 0x20 != 0,
                        false,
                        false,
                    )
                }
                4 => {
                    if status_flags & !0x70 != 0 || format_flags & !0x4f != 0 {
                        return Err(AudioError::InvalidFormat("invalid ID3v2.4 frame flags"));
                    }
                    (
                        format_flags & 0x0c != 0,
                        format_flags & 0x40 != 0,
                        format_flags & 0x02 != 0,
                        format_flags & 0x01 != 0,
                    )
                }
                _ => unreachable!(),
            };
        if !compress_or_encrypt && id[0] == b'T' {
            let original_frame = &body[position + header_size..end];
            let decoded_frame =
                (tag_unsynchronized || frame_unsynchronized).then(|| unsynchronize(original_frame));
            let mut frame = decoded_frame.as_deref().unwrap_or(original_frame);
            if grouped {
                frame = frame
                    .get(1..)
                    .ok_or(AudioError::InvalidFormat("truncated grouped ID3 frame"))?;
            }
            if data_length_indicator {
                let indicator = frame.get(..4).ok_or(AudioError::InvalidFormat(
                    "truncated ID3 data length indicator",
                ))?;
                syncsafe(indicator)?;
                frame = &frame[4..];
            }
            parse_text_frame(id, frame, version, &mut metadata)?;
        }
        position = end;
    }
    Ok(metadata)
}

fn parse_text_frame(
    id: &[u8],
    frame: &[u8],
    version: u8,
    metadata: &mut Metadata,
) -> Result<(), AudioError> {
    let (&encoding, text) = frame
        .split_first()
        .ok_or(AudioError::InvalidFormat("missing ID3 text encoding"))?;
    if encoding > if matches!(version, 2 | 3) { 1 } else { 3 } {
        return Err(AudioError::InvalidFormat("invalid ID3 text encoding"));
    }
    let values = split_strings(text, encoding)?;
    if id == b"TXXX" || id == b"TXX" {
        let Some((description, values)) = values.split_first() else {
            return Err(AudioError::InvalidFormat("missing TXXX description"));
        };
        if description.trim().is_empty() {
            return Ok(());
        }
        for value in values {
            insert_nonempty(metadata, description, value);
        }
        return Ok(());
    }
    let Some(key) = mapped_key(id) else {
        return Ok(());
    };
    for value in values {
        insert_nonempty(metadata, key, &value);
    }
    Ok(())
}

fn split_strings(bytes: &[u8], encoding: u8) -> Result<Vec<String>, AudioError> {
    let wide = matches!(encoding, 1 | 2);
    if wide && bytes.len() % 2 != 0 {
        return Err(AudioError::InvalidFormat("odd-length ID3 UTF-16 text"));
    }
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut position = 0;
    while position + if wide { 2 } else { 1 } <= bytes.len() {
        let is_end = if wide {
            bytes[position..position + 2] == [0, 0]
        } else {
            bytes[position] == 0
        };
        if is_end {
            pieces.push(&bytes[start..position]);
            position += if wide { 2 } else { 1 };
            start = position;
        } else {
            position += if wide { 2 } else { 1 };
        }
    }
    if start < bytes.len() {
        pieces.push(&bytes[start..]);
    }
    decode_pieces(&pieces, encoding)
}

fn decode_pieces(pieces: &[&[u8]], encoding: u8) -> Result<Vec<String>, AudioError> {
    let mut utf16_le = None;
    let mut values = Vec::with_capacity(pieces.len());
    for piece in pieces {
        let value = match encoding {
            0 => piece.iter().map(|&byte| char::from(byte)).collect(),
            1 | 2 => {
                let (little_endian, content) = if encoding == 2 {
                    (false, *piece)
                } else if let Some(&[0xfe, 0xff]) = piece.get(..2) {
                    utf16_le = Some(false);
                    (false, &piece[2..])
                } else if let Some(&[0xff, 0xfe]) = piece.get(..2) {
                    utf16_le = Some(true);
                    (true, &piece[2..])
                } else if let Some(little_endian) = utf16_le {
                    (little_endian, *piece)
                } else {
                    return Err(AudioError::InvalidFormat(
                        "ID3 UTF-16 text is missing its BOM",
                    ));
                };
                if content.len() % 2 != 0 {
                    return Err(AudioError::InvalidFormat("odd-length ID3 UTF-16 text"));
                }
                let units = content.chunks_exact(2).map(|pair| {
                    if little_endian {
                        u16::from_le_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_be_bytes([pair[0], pair[1]])
                    }
                });
                char::decode_utf16(units)
                    .collect::<Result<String, _>>()
                    .map_err(|_| AudioError::InvalidFormat("invalid ID3 UTF-16 text"))?
            }
            3 => std::str::from_utf8(piece)
                .map_err(|_| AudioError::InvalidFormat("invalid ID3 UTF-8 text"))?
                .to_owned(),
            _ => unreachable!(),
        };
        values.push(value);
    }
    Ok(values)
}

fn insert_nonempty(metadata: &mut Metadata, key: &str, value: &str) {
    if !value.trim().is_empty() {
        metadata.insert(key, value.trim());
    }
}

fn mapped_key(id: &[u8]) -> Option<&'static str> {
    Some(match id {
        b"TT2" | b"TIT2" => "TITLE",
        b"TP1" | b"TPE1" => "ARTIST",
        b"TAL" | b"TALB" => "ALBUM",
        b"TRK" | b"TRCK" => "TRACKNUMBER",
        b"TPA" | b"TPOS" => "DISCNUMBER",
        b"TCO" | b"TCON" => "GENRE",
        b"TYE" | b"TYER" | b"TDRC" => "DATE",
        b"TCM" | b"TCOM" => "COMPOSER",
        b"TP2" | b"TPE2" => "ALBUMARTIST",
        b"TCR" | b"TCOP" => "COPYRIGHT",
        b"TXT" | b"TEXT" => "LYRICIST",
        b"TEN" | b"TENC" => "ENCODEDBY",
        b"TBP" | b"TBPM" => "BPM",
        b"TRC" | b"TSRC" => "ISRC",
        _ => return None,
    })
}

fn syncsafe(bytes: &[u8]) -> Result<u32, AudioError> {
    if bytes.len() != 4 || bytes.iter().any(|byte| byte & 0x80 != 0) {
        return Err(AudioError::InvalidFormat("invalid ID3 synchsafe integer"));
    }
    Ok((u32::from(bytes[0]) << 21)
        | (u32::from(bytes[1]) << 14)
        | (u32::from(bytes[2]) << 7)
        | u32::from(bytes[3]))
}

fn unsynchronize(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut position = 0;
    while position < bytes.len() {
        let byte = bytes[position];
        output.push(byte);
        position += 1;
        if byte == 0xff && bytes.get(position) == Some(&0) {
            match bytes.get(position + 1) {
                Some(0) | Some(0xe0..=0xff) | None => position += 1,
                _ => {}
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zemer_core::Metadata;

    use super::{MAX_ID3_TAG_BYTES, read_frames, read_tag_at_end, rewrite_metadata_tag};

    #[test]
    fn ignores_and_preserves_id3v22_compressed_tags() {
        let mut tag = b"ID3\x02\0\x40\0\0\0\x03".to_vec();
        tag.extend_from_slice(&[0x01, 0x02, 0x03]);
        let (metadata, raw) = read_tag_at_end(&mut Cursor::new(&tag), 0, tag.len() as u64).unwrap();
        assert_eq!(metadata, Metadata::default());
        assert_eq!(raw, tag);
        let mut edited = Metadata::default();
        edited.insert("TITLE", "Cannot edit compressed tag");
        assert!(rewrite_metadata_tag(&tag, &edited).is_err());
    }

    fn frame(version: u8, id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut frame = id.to_vec();
        let size = if version == 3 {
            (data.len() as u32).to_be_bytes()
        } else {
            [
                ((data.len() >> 21) & 0x7f) as u8,
                ((data.len() >> 14) & 0x7f) as u8,
                ((data.len() >> 7) & 0x7f) as u8,
                (data.len() & 0x7f) as u8,
            ]
        };
        frame.extend_from_slice(&size);
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(data);
        frame
    }

    fn v22_frame(id: &[u8; 3], data: &[u8]) -> Vec<u8> {
        let size = data.len();
        let mut frame = id.to_vec();
        frame.extend_from_slice(&[(size >> 16) as u8, (size >> 8) as u8, size as u8]);
        frame.extend_from_slice(data);
        frame
    }

    #[test]
    fn maps_id3v22_text_and_user_text_frames() {
        let mut body = v22_frame(b"TT2", &[0, b'T', b'i', b't', b'l', b'e']);
        body.extend(v22_frame(
            b"TXX",
            &[
                0, b'R', b'E', b'P', b'L', b'A', b'Y', b'G', b'A', b'I', b'N', b'_', b'G', b'A',
                b'I', b'N', 0, b'-', b'3', b' ', b'd', b'B',
            ],
        ));
        let size = body.len();
        let mut tag = b"ID3\x02\x00\x00".to_vec();
        tag.extend_from_slice(&[
            ((size >> 21) & 0x7f) as u8,
            ((size >> 14) & 0x7f) as u8,
            ((size >> 7) & 0x7f) as u8,
            (size & 0x7f) as u8,
        ]);
        tag.extend_from_slice(&body);
        let mut reader = Cursor::new(tag.clone());
        let (metadata, raw) = read_tag_at_end(&mut reader, 0, tag.len() as u64).unwrap();
        assert_eq!(metadata.values("TITLE"), ["Title"]);
        assert_eq!(metadata.values("REPLAYGAIN_GAIN"), ["-3 dB"]);
        assert_eq!(raw, tag);
    }

    fn parse(version: u8, body: Vec<u8>, flags: u8) -> Metadata {
        read_frames(&body, version, flags != 0).unwrap()
    }

    #[test]
    fn maps_v23_iso_text_and_utf16_frames() {
        let mut body = frame(3, b"TIT2", &[0, b'S', b'o', b'n', b'g']);
        body.extend(frame(
            3,
            b"TPE1",
            &[
                1, 0xff, 0xfe, b'A', 0, b'r', 0, b't', 0, b'i', 0, b's', 0, b't', 0,
            ],
        ));
        let metadata = parse(3, body, 0);
        assert_eq!(metadata.values("title"), ["Song"]);
        assert_eq!(metadata.values("artist"), ["Artist"]);
    }

    #[test]
    fn maps_v24_utf8_repeated_values_and_txxx() {
        let mut body = frame(4, b"TIT2", &[3, b'T', b'i', b't', b'l', b'e']);
        body.extend(frame(4, b"TPE1", &[3, b'A', 0, b'B']));
        body.extend(frame(
            4,
            b"TXXX",
            &[
                3, b'R', b'E', b'P', b'L', b'A', b'Y', b'G', b'A', b'I', b'N', b'_', b'G', b'A',
                b'I', b'N', 0, b'-', b'3', b' ', b'd', b'B',
            ],
        ));
        let metadata = parse(4, body, 0);
        assert_eq!(metadata.values("TITLE"), ["Title"]);
        assert_eq!(metadata.values("ARTIST"), ["A", "B"]);
        assert_eq!(metadata.values("REPLAYGAIN_GAIN"), ["-3 dB"]);
    }

    #[test]
    fn decodes_tag_level_unsynchronisation_and_skips_binary_frames() {
        let mut body = frame(4, b"TIT2", &[0, b'A', 0xff, 0, 0xe1]);
        body.extend(frame(4, b"APIC", &[1, 2, 3]));
        let metadata = parse(4, body, 0x80);
        assert_eq!(metadata.values("TITLE"), ["Aÿá"]);
    }

    #[test]
    fn rejects_malformed_frame_sizes_and_invalid_text() {
        assert!(read_frames(&[b'T', b'I', b'T', b'2', 0, 0, 0, 0x7f, 0, 0], 4, false).is_err());
        assert!(read_frames(&frame(4, b"TIT2", &[3, 0xff]), 4, false).is_err());
    }

    #[test]
    fn validates_and_reads_an_appended_id3v24_footer() {
        let body = frame(4, b"TIT2", &[3, b'S', b'o', b'n', b'g']);
        let size = body.len();
        let mut tag = b"ID3\x04\x00\x10".to_vec();
        tag.extend_from_slice(&[
            ((size >> 21) & 0x7f) as u8,
            ((size >> 14) & 0x7f) as u8,
            ((size >> 7) & 0x7f) as u8,
            (size & 0x7f) as u8,
        ]);
        tag.extend_from_slice(&body);
        tag.extend_from_slice(b"3DI\x04\x00\x10");
        tag.extend_from_slice(&[
            ((size >> 21) & 0x7f) as u8,
            ((size >> 14) & 0x7f) as u8,
            ((size >> 7) & 0x7f) as u8,
            (size & 0x7f) as u8,
        ]);
        let file_size = tag.len() as u64;
        let mut reader = Cursor::new(tag.clone());
        assert_eq!(
            read_tag_at_end(&mut reader, 0, file_size)
                .unwrap()
                .0
                .values("TITLE"),
            ["Song"]
        );

        let mut edited = Metadata::default();
        edited.insert("TITLE", "New song");
        let rewritten = super::rewrite_metadata_tag(&tag, &edited).unwrap();
        assert_eq!(
            &rewritten[rewritten.len() - 10..rewritten.len() - 7],
            b"3DI"
        );
        let mut reader = Cursor::new(rewritten.clone());
        let actual = read_tag_at_end(&mut reader, 0, rewritten.len() as u64)
            .unwrap()
            .0;
        assert_eq!(actual, edited);

        *tag.last_mut().unwrap() ^= 1;
        let mut reader = Cursor::new(tag);
        assert!(read_tag_at_end(&mut reader, 0, file_size).is_err());
    }

    #[test]
    fn removes_an_id3_extended_header_when_rewriting() {
        let mut body = vec![0, 0, 0, 6, 0, 0, 0, 0, 0, 0];
        body.extend(frame(3, b"TIT2", &[0, b'O', b'l', b'd']));
        let size = body.len();
        let mut tag = b"ID3\x03\x00\x40".to_vec();
        tag.extend_from_slice(&[
            ((size >> 21) & 0x7f) as u8,
            ((size >> 14) & 0x7f) as u8,
            ((size >> 7) & 0x7f) as u8,
            (size & 0x7f) as u8,
        ]);
        tag.extend_from_slice(&body);
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "New");
        let rewritten = super::rewrite_metadata_tag(&tag, &metadata).unwrap();
        assert_eq!(rewritten[5] & 0x40, 0);
        let mut reader = Cursor::new(rewritten.clone());
        assert_eq!(
            read_tag_at_end(&mut reader, 0, rewritten.len() as u64)
                .unwrap()
                .0
                .values("TITLE"),
            ["New"]
        );
    }

    #[test]
    fn rewrites_tag_level_unsynchronised_frames_without_losing_binary_bytes() {
        let mut body = frame(3, b"TIT2", &[0, b'O', b'l', b'd']);
        let artwork = [1, 0xff, 0xe0, 0xff, 0, 1];
        body.extend(frame(3, b"APIC", &artwork));
        let mut stored = Vec::new();
        for (index, &byte) in body.iter().enumerate() {
            stored.push(byte);
            if byte == 0xff
                && body
                    .get(index + 1)
                    .is_none_or(|next| *next == 0 || *next >= 0xe0)
            {
                stored.push(0);
            }
        }
        let size = stored.len();
        let mut tag = b"ID3\x03\x00\x80".to_vec();
        tag.extend_from_slice(&[
            ((size >> 21) & 0x7f) as u8,
            ((size >> 14) & 0x7f) as u8,
            ((size >> 7) & 0x7f) as u8,
            (size & 0x7f) as u8,
        ]);
        tag.extend_from_slice(&stored);
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "New");

        let rewritten = super::rewrite_metadata_tag(&tag, &metadata).unwrap();
        assert_eq!(rewritten[5] & 0x80, 0);
        assert!(rewritten.windows(4).any(|bytes| bytes == b"APIC"));
        assert!(
            rewritten
                .windows(artwork.len())
                .any(|bytes| bytes == artwork)
        );
        let mut reader = Cursor::new(rewritten.clone());
        assert_eq!(
            read_tag_at_end(&mut reader, 0, rewritten.len() as u64)
                .unwrap()
                .0
                .values("TITLE"),
            ["New"]
        );
    }

    #[test]
    fn enforces_the_tag_size_limit_before_allocating() {
        let mut header = b"ID3\x04\x00\x00".to_vec();
        header.extend_from_slice(&[
            ((MAX_ID3_TAG_BYTES + 1) >> 21) as u8,
            (((MAX_ID3_TAG_BYTES + 1) >> 14) & 0x7f) as u8,
            (((MAX_ID3_TAG_BYTES + 1) >> 7) & 0x7f) as u8,
            ((MAX_ID3_TAG_BYTES + 1) & 0x7f) as u8,
        ]);
        let file_size = 10 + (MAX_ID3_TAG_BYTES + 1) as u64;
        let mut reader = Cursor::new(header);
        assert!(read_tag_at_end(&mut reader, 0, file_size).is_err());
    }

    #[test]
    fn writes_standard_and_custom_text_for_the_existing_reader() {
        let mut expected = Metadata::default();
        expected.insert("TITLE", "A title");
        expected.insert("ARTIST", "One");
        expected.insert("ARTIST", "Two");
        expected.insert("REPLAYGAIN_GAIN", "-3 dB");
        let tag = super::encode_metadata(&expected).unwrap();
        let mut reader = Cursor::new(tag.clone());
        let actual = read_tag_at_end(&mut reader, 0, tag.len() as u64).unwrap().0;
        assert_eq!(actual, expected);
    }

    #[test]
    fn rejects_nul_in_metadata_values() {
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "bad\0value");
        assert!(super::encode_metadata(&metadata).is_err());
    }
}
