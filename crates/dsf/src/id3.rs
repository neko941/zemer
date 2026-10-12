use std::io::{Read, Seek, SeekFrom};

use zemer_core::{AudioError, Metadata};

const MAX_ID3_TAG_BYTES: usize = 64 * 1024 * 1024;

pub(super) fn read_metadata<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    file_size: u64,
) -> Result<Metadata, AudioError> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut header = [0; 10];
    reader.read_exact(&mut header)?;
    if &header[..3] != b"ID3" {
        return Err(AudioError::InvalidFormat("invalid DSF ID3 tag header"));
    }
    let version = header[3];
    let flags = header[5];
    let allowed_flags = match version {
        3 => 0xe0,
        4 => 0xf0,
        _ => {
            return Err(AudioError::Unsupported(
                "DSF ID3 metadata requires ID3v2.3 or ID3v2.4",
            ));
        }
    };
    if header[4] == 0xff || flags & !allowed_flags != 0 {
        return Err(AudioError::InvalidFormat(
            "invalid DSF ID3 tag version or flags",
        ));
    }
    let size = syncsafe(&header[6..10])?;
    let footer_size = usize::from(version == 4 && flags & 0x10 != 0) * 10;
    let total_size = 10_u64
        .checked_add(u64::from(size))
        .and_then(|value| value.checked_add(footer_size as u64))
        .ok_or(AudioError::InvalidData("DSF ID3 tag size overflow"))?;
    if size as usize > MAX_ID3_TAG_BYTES || offset.checked_add(total_size) != Some(file_size) {
        return Err(AudioError::InvalidData(
            "DSF ID3 tag exceeds its bound or is not at end of file",
        ));
    }
    let mut body = Vec::new();
    body.try_reserve_exact(size as usize)
        .map_err(|_| AudioError::InvalidData("DSF ID3 tag is too large"))?;
    body.resize(size as usize, 0);
    reader.read_exact(&mut body)?;
    if footer_size != 0 {
        let mut footer = [0; 10];
        reader.read_exact(&mut footer)?;
        if &footer[..3] != b"3DI" || footer[3..] != header[3..] {
            return Err(AudioError::InvalidFormat("invalid ID3v2.4 footer"));
        }
    }
    if version == 3 && flags & 0x80 != 0 {
        body = unsynchronize(&body);
    }
    let frame_start = extended_header_size(&body, version, flags)?;
    read_frames(
        &body[frame_start..],
        version,
        version == 4 && flags & 0x80 != 0,
    )
}

fn extended_header_size(body: &[u8], version: u8, flags: u8) -> Result<usize, AudioError> {
    if flags & 0x40 == 0 {
        return Ok(0);
    }
    let size = match version {
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
        if remaining.len() < 10 {
            return Err(AudioError::InvalidFormat("truncated ID3 frame header"));
        }
        let id = &remaining[..4];
        if !id
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return Err(AudioError::InvalidFormat("invalid ID3 frame identifier"));
        }
        let frame_size = match version {
            3 => u32::from_be_bytes(remaining[4..8].try_into().unwrap()) as usize,
            4 => syncsafe(&remaining[4..8])? as usize,
            _ => unreachable!(),
        };
        if frame_size == 0 {
            return Err(AudioError::InvalidFormat("empty ID3 frame"));
        }
        let end = position
            .checked_add(10)
            .and_then(|value| value.checked_add(frame_size))
            .filter(|&value| value <= body.len())
            .ok_or(AudioError::InvalidFormat("ID3 frame exceeds tag boundary"))?;
        let status_flags = remaining[8];
        let format_flags = remaining[9];
        let (compress_or_encrypt, grouped, frame_unsynchronized, data_length_indicator) =
            match version {
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
            let mut frame = &body[position + 10..end];
            let mut decoded_frame = None;
            if tag_unsynchronized || frame_unsynchronized {
                decoded_frame = Some(unsynchronize(frame));
                frame = decoded_frame.as_deref().unwrap();
            }
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
    if encoding > if version == 3 { 1 } else { 3 } {
        return Err(AudioError::InvalidFormat("invalid ID3 text encoding"));
    }
    let values = split_strings(text, encoding)?;
    if id == b"TXXX" {
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
        insert_nonempty(metadata, key, value);
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
        b"TIT2" => "TITLE",
        b"TPE1" => "ARTIST",
        b"TALB" => "ALBUM",
        b"TRCK" => "TRACKNUMBER",
        b"TPOS" => "DISCNUMBER",
        b"TCON" => "GENRE",
        b"TDRC" | b"TYER" => "DATE",
        b"TCOM" => "COMPOSER",
        b"TPE2" => "ALBUMARTIST",
        b"TCOP" => "COPYRIGHT",
        b"TEXT" => "LYRICIST",
        b"TENC" => "ENCODEDBY",
        b"TBPM" => "BPM",
        b"TSRC" => "ISRC",
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

    use super::{MAX_ID3_TAG_BYTES, read_frames, read_metadata};

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
            read_metadata(&mut reader, 0, file_size)
                .unwrap()
                .values("TITLE"),
            ["Song"]
        );

        *tag.last_mut().unwrap() ^= 1;
        let mut reader = Cursor::new(tag);
        assert!(read_metadata(&mut reader, 0, file_size).is_err());
    }

    #[test]
    fn enforces_the_tag_size_limit_before_allocating() {
        let size = (MAX_ID3_TAG_BYTES as u32 + 1).to_le_bytes();
        let mut header = b"ID3\x04\x00\x00".to_vec();
        header.extend_from_slice(&[
            ((MAX_ID3_TAG_BYTES + 1) >> 21) as u8,
            ((MAX_ID3_TAG_BYTES + 1) >> 14 & 0x7f) as u8,
            ((MAX_ID3_TAG_BYTES + 1) >> 7 & 0x7f) as u8,
            ((MAX_ID3_TAG_BYTES + 1) & 0x7f) as u8,
        ]);
        let file_size = 10 + u64::from_le_bytes(size) as u64;
        let mut reader = Cursor::new(header);
        assert!(read_metadata(&mut reader, 0, file_size).is_err());
    }
}
