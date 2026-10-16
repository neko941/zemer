use std::{collections::BTreeSet, io::Write};

use zemer_core::{AudioError, Metadata};

pub(super) const MAX_INFO_BYTES: u64 = 16 * 1024 * 1024;
const MAX_INFO_ENTRIES: u32 = 65_536;

pub(super) fn parse_info(bytes: &[u8]) -> Result<Metadata, AudioError> {
    if bytes.len() < 4 {
        return Err(AudioError::InvalidFormat("CAF info chunk is truncated"));
    }
    let mut cursor = 4;
    let count = u32::from_be_bytes(bytes[..4].try_into().unwrap());
    if count > MAX_INFO_ENTRIES {
        return Err(AudioError::InvalidFormat(
            "CAF info entry count exceeds the limit",
        ));
    }
    let mut metadata = Metadata::default();
    let mut keys = BTreeSet::new();
    for _ in 0..count {
        let key_end = bytes[cursor..]
            .iter()
            .position(|&byte| byte == 0)
            .map(|offset| cursor + offset)
            .ok_or(AudioError::InvalidFormat("CAF info key is unterminated"))?;
        let key = std::str::from_utf8(&bytes[cursor..key_end])
            .map_err(|_| AudioError::InvalidFormat("CAF info key is not UTF-8"))?;
        if key.is_empty() || !keys.insert(key.to_ascii_lowercase()) {
            return Err(AudioError::InvalidFormat(
                "CAF info keys must be non-empty and unique",
            ));
        }
        cursor = key_end + 1;
        let value_end = bytes[cursor..]
            .iter()
            .position(|&byte| byte == 0)
            .map(|offset| cursor + offset)
            .ok_or(AudioError::InvalidFormat("CAF info value is unterminated"))?;
        let value = std::str::from_utf8(&bytes[cursor..value_end])
            .map_err(|_| AudioError::InvalidFormat("CAF info value is not UTF-8"))?;
        cursor = value_end + 1;
        for value in value.split(',') {
            metadata.insert(key, value);
        }
    }
    if bytes[cursor..].iter().any(|&byte| byte != 0) {
        return Err(AudioError::InvalidFormat(
            "CAF info chunk has invalid trailing data",
        ));
    }
    Ok(metadata)
}

pub(super) fn write_info<W: Write>(writer: &mut W, metadata: &Metadata) -> Result<u64, AudioError> {
    let mut values = Vec::new();
    let mut size = 4_u64;
    for (key, entries) in metadata.iter() {
        if key.is_empty() || key.as_bytes().contains(&0) || entries.is_empty() {
            return Err(AudioError::InvalidData(
                "CAF metadata key or value is empty",
            ));
        }
        if values.len() == MAX_INFO_ENTRIES as usize {
            return Err(AudioError::Unsupported("CAF metadata has too many entries"));
        }
        for entry in entries {
            if entry.as_bytes().contains(&0) {
                return Err(AudioError::InvalidData("CAF metadata contains a NUL byte"));
            }
            size = size
                .checked_add(entry.len() as u64 + 1)
                .ok_or(AudioError::Unsupported("CAF metadata size overflow"))?;
        }
        let key = standard_key(key).unwrap_or(key);
        size = size
            .checked_add(key.len() as u64 + 1)
            .ok_or(AudioError::Unsupported("CAF metadata size overflow"))?;
        if size > MAX_INFO_BYTES {
            return Err(AudioError::Unsupported("CAF info chunk exceeds 16 MiB"));
        }
        let joined = entries.join(",");
        values.push((key, joined));
    }
    let count = u32::try_from(values.len())
        .map_err(|_| AudioError::Unsupported("CAF metadata has too many entries"))?;
    if values.is_empty() {
        return Ok(0);
    }
    writer.write_all(b"info")?;
    writer.write_all(&size.to_be_bytes())?;
    writer.write_all(&count.to_be_bytes())?;
    for (key, value) in values {
        writer.write_all(key.as_bytes())?;
        writer.write_all(&[0])?;
        writer.write_all(value.as_bytes())?;
        writer.write_all(&[0])?;
    }
    Ok(size)
}

fn standard_key(key: &str) -> Option<&'static str> {
    Some(match key {
        "ALBUM" => "album",
        "ARTIST" => "artist",
        "COMMENT" | "COMMENTS" => "comments",
        "COMPOSER" => "composer",
        "COPYRIGHT" => "copyright",
        "ENCODED-BY" | "ENCODER" => "source encoder",
        "GENRE" => "genre",
        "KEY" | "KEY SIGNATURE" => "key signature",
        "LYRICIST" => "lyricist",
        "TEMPO" => "tempo",
        "TIME SIGNATURE" => "time signature",
        "TITLE" => "title",
        "TRACK" | "TRACKNUMBER" | "TRACK NUMBER" => "track number",
        "YEAR" | "DATE" => "year",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_info;

    #[test]
    fn rejects_truncated_and_duplicate_info_entries() {
        assert!(parse_info(&[0, 0, 0, 1]).is_err());
        assert!(
            parse_info(&[
                0, 0, 0, 2, b'T', b'i', b't', b'l', b'e', 0, b'a', 0, b't', b'i', b't', b'l', b'e',
                0, b'b', 0,
            ])
            .is_err()
        );
    }
}
