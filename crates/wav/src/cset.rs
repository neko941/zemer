use std::{
    borrow::Cow,
    io::{Read, Seek, SeekFrom, Write},
};

use encoding_rs::Encoding;
use zemer_core::AudioError;

use crate::decode::{MAX_DS64_CHUNK, parse_ds64};

pub(super) const DEFAULT_CODE_PAGE: u16 = 1004;
pub(super) const UTF8_CODE_PAGE: u16 = 65001;

pub(super) fn encoding_for_code_page(code_page: u16) -> Option<&'static Encoding> {
    let label: &[u8] = match code_page {
        1252 => b"windows-1252",
        1250 => b"windows-1250",
        1251 => b"windows-1251",
        1253 => b"windows-1253",
        1254 => b"windows-1254",
        1255 => b"windows-1255",
        1256 => b"windows-1256",
        1257 => b"windows-1257",
        1258 => b"windows-1258",
        932 => b"shift_jis",
        936 => b"gbk",
        949 => b"euc-kr",
        950 => b"big5",
        UTF8_CODE_PAGE => b"utf-8",
        _ => return None,
    };
    Encoding::for_label(label)
}

pub(super) fn decode_info_value(bytes: &[u8], code_page: u16) -> Cow<'_, str> {
    match code_page {
        0 | DEFAULT_CODE_PAGE | 28591 => {
            Cow::Owned(bytes.iter().map(|byte| char::from(*byte)).collect())
        }
        _ => encoding_for_code_page(code_page).map_or_else(
            || String::from_utf8_lossy(bytes),
            |encoding| encoding.decode_without_bom_handling(bytes).0,
        ),
    }
}

pub(super) fn encode_info_value(value: &str, code_page: u16) -> Result<Cow<'_, [u8]>, AudioError> {
    if matches!(code_page, 0 | DEFAULT_CODE_PAGE | 28591) {
        return value
            .chars()
            .map(|character| u8::try_from(u32::from(character)))
            .collect::<Result<Vec<_>, _>>()
            .map(Cow::Owned)
            .map_err(|_| {
                AudioError::Unsupported(
                    "WAVE INFO value cannot be represented by its CSET code page",
                )
            });
    }
    let encoding = encoding_for_code_page(code_page).ok_or(AudioError::Unsupported(
        "WAVE INFO CSET code page is not supported for writing",
    ))?;
    let (bytes, _, had_errors) = encoding.encode(value);
    if had_errors {
        return Err(AudioError::Unsupported(
            "WAVE INFO value cannot be represented by its CSET code page",
        ));
    }
    Ok(bytes)
}

pub(super) fn read_code_page<R: Read + Seek>(reader: &mut R) -> Result<u16, AudioError> {
    let position = reader.stream_position()?;
    reader.seek(SeekFrom::Start(0))?;
    let result = read_code_page_from_start(reader);
    reader.seek(SeekFrom::Start(position))?;
    result
}

fn read_code_page_from_start<R: Read + Seek>(reader: &mut R) -> Result<u16, AudioError> {
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

    let size32 = u32::from_le_bytes(header[4..8].try_into().unwrap());
    if rf64 != (size32 == u32::MAX) {
        return Err(AudioError::InvalidFormat("invalid RIFF/RF64 size field"));
    }
    let mut ds64 = None;
    if rf64 {
        let mut chunk = [0; 8];
        reader.read_exact(&mut chunk)?;
        if &chunk[..4] != b"ds64" {
            return Err(AudioError::InvalidFormat(
                "RF64 requires ds64 as the first chunk",
            ));
        }
        let size = u32::from_le_bytes(chunk[4..].try_into().unwrap());
        if !(28..=MAX_DS64_CHUNK).contains(&size) {
            return Err(AudioError::InvalidFormat("invalid RF64 ds64 chunk size"));
        }
        let mut bytes = vec![0; size as usize];
        reader.read_exact(&mut bytes)?;
        ds64 = Some(parse_ds64(&bytes)?);
        if size & 1 != 0 {
            reader.seek(SeekFrom::Current(1))?;
        }
    }

    let riff_end = 8_u64
        .checked_add(
            ds64.as_ref()
                .map_or(u64::from(size32), |info| info.riff_size),
        )
        .ok_or(AudioError::InvalidFormat("RIFF size overflow"))?;
    let chunk_start = reader.stream_position()?;
    let file_end = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(chunk_start))?;
    if riff_end > file_end || riff_end < 12 {
        return Err(AudioError::InvalidFormat("RIFF size exceeds the file"));
    }
    let mut first_data_seen = false;
    let mut code_page = None;
    while reader.stream_position()? < riff_end {
        if riff_end - reader.stream_position()? < 8 {
            return Err(AudioError::InvalidFormat("truncated RIFF chunk header"));
        }
        let mut chunk = [0; 8];
        reader.read_exact(&mut chunk)?;
        let id: [u8; 4] = chunk[..4].try_into().unwrap();
        let size32 = u32::from_le_bytes(chunk[4..].try_into().unwrap());
        let size = if size32 != u32::MAX {
            u64::from(size32)
        } else if rf64 && id == *b"data" && !first_data_seen {
            ds64.as_ref().unwrap().data_size
        } else if rf64 {
            ds64.as_mut()
                .and_then(|info| info.take_table_size(&id))
                .ok_or(AudioError::InvalidFormat(
                    "RF64 chunk size sentinel has no ds64 table entry",
                ))?
        } else {
            return Err(AudioError::InvalidFormat(
                "RIFF chunk size sentinel without RF64 ds64",
            ));
        };
        if id == *b"data" {
            first_data_seen = true;
        }
        let body = reader.stream_position()?;
        let padded_end = body
            .checked_add(size)
            .and_then(|end| end.checked_add(size & 1))
            .ok_or(AudioError::InvalidFormat("chunk size overflow"))?;
        if padded_end > riff_end {
            return Err(AudioError::InvalidFormat(
                "chunk extends past the RIFF boundary",
            ));
        }
        if id == *b"CSET" {
            if code_page.is_some() || size != 8 {
                return Err(AudioError::InvalidFormat(
                    "invalid or duplicate WAVE CSET chunk",
                ));
            }
            let mut fields = [0; 8];
            reader.read_exact(&mut fields)?;
            code_page = Some(u16::from_le_bytes(fields[..2].try_into().unwrap()));
        }
        reader.seek(SeekFrom::Start(padded_end))?;
    }
    if reader.stream_position()? != riff_end {
        return Err(AudioError::InvalidFormat(
            "RIFF chunk alignment exceeds the boundary",
        ));
    }
    Ok(code_page.unwrap_or(DEFAULT_CODE_PAGE))
}

pub(super) fn write_utf8_chunk<W: Write>(writer: &mut W) -> Result<(), AudioError> {
    writer.write_all(b"CSET")?;
    writer.write_all(&8_u32.to_le_bytes())?;
    writer.write_all(&UTF8_CODE_PAGE.to_le_bytes())?;
    writer.write_all(&[0; 6])?;
    Ok(())
}
