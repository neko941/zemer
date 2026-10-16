//! Bounded APEv2 tag parsing and writing.

use std::{error::Error, fmt};

use crate::{Metadata, Picture};

const FOOTER_SIZE: usize = 32;
// shortcut: stay within WavPack's documented interoperable tag size; revisit with a nonstandard opt-in.
/// Maximum encoded APEv2 tag size, including its footer but excluding an optional header.
pub const MAX_APEV2_TAG_SIZE: usize = 16 * 1024 * 1024;
const HEADER_PRESENT: u32 = 1 << 31;
const NO_FOOTER: u32 = 1 << 30;
const IS_HEADER: u32 = 1 << 29;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApeTag {
    items: Vec<ApeItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApeItem {
    key: Vec<u8>,
    flags: u32,
    value: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApeItemType {
    Text,
    Binary,
    External,
    Reserved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApeTagError(pub &'static str);

impl ApeTag {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Parses one complete APEv2 tag region, with a footer and optional header.
    pub fn parse(bytes: &[u8]) -> Result<Self, ApeTagError> {
        if bytes.len() > MAX_APEV2_TAG_SIZE + FOOTER_SIZE {
            return Err(ApeTagError("APEv2 tag exceeds the 16 MiB limit"));
        }
        let footer = bytes
            .get(bytes.len().saturating_sub(FOOTER_SIZE)..)
            .filter(|_| bytes.len() >= FOOTER_SIZE)
            .ok_or(ApeTagError("truncated APEv2 footer"))?;
        validate_header_footer(footer, false)?;
        let declared_size = read_u32(&footer[12..16])? as usize;
        let item_count = read_u32(&footer[16..20])? as usize;
        let flags = read_u32(&footer[20..24])?;
        let header_size = if flags & HEADER_PRESENT != 0 {
            FOOTER_SIZE
        } else {
            0
        };
        if !(FOOTER_SIZE..=MAX_APEV2_TAG_SIZE).contains(&declared_size)
            || declared_size.checked_add(header_size) != Some(bytes.len())
        {
            return Err(ApeTagError("invalid APEv2 tag size"));
        }
        if header_size != 0 {
            let header = &bytes[..FOOTER_SIZE];
            validate_header_footer(header, true)?;
            if read_u32(&header[12..16])? as usize != declared_size
                || read_u32(&header[16..20])? as usize != item_count
                || read_u32(&header[20..24])? & HEADER_PRESENT == 0
            {
                return Err(ApeTagError("APEv2 header and footer disagree"));
            }
        }

        let body_start = header_size;
        let body_end = bytes.len() - FOOTER_SIZE;
        if item_count > (body_end - body_start) / 11 {
            return Err(ApeTagError("too many APEv2 items for the tag size"));
        }
        let mut position = body_start;
        let mut items = Vec::new();
        items
            .try_reserve(item_count)
            .map_err(|_| ApeTagError("too many APEv2 items"))?;
        for _ in 0..item_count {
            let fixed_end = position
                .checked_add(8)
                .filter(|&end| end <= body_end)
                .ok_or(ApeTagError("truncated APEv2 item header"))?;
            let value_size = read_u32(&bytes[position..position + 4])? as usize;
            let item_flags = read_u32(&bytes[position + 4..fixed_end])?;
            position = fixed_end;
            let key_end = bytes[position..body_end]
                .iter()
                .position(|&byte| byte == 0)
                .and_then(|length| position.checked_add(length))
                .ok_or(ApeTagError("unterminated APEv2 item key"))?;
            validate_key(&bytes[position..key_end])?;
            let value_start = key_end + 1;
            let value_end = value_start
                .checked_add(value_size)
                .filter(|&end| end <= body_end)
                .ok_or(ApeTagError("truncated APEv2 item value"))?;
            items.push(ApeItem {
                key: bytes[position..key_end].to_vec(),
                flags: item_flags,
                value: bytes[value_start..value_end].to_vec(),
            });
            position = value_end;
        }
        if position != body_end {
            return Err(ApeTagError("unexpected bytes after APEv2 items"));
        }
        Ok(Self { items })
    }

    /// Encodes a footer-only APEv2 tag, as used at the end of WavPack files.
    pub fn encode(&self) -> Result<Vec<u8>, ApeTagError> {
        let item_count =
            u32::try_from(self.items.len()).map_err(|_| ApeTagError("too many APEv2 items"))?;
        let mut body_size = 0_usize;
        for item in &self.items {
            validate_key(&item.key)?;
            u32::try_from(item.value.len())
                .map_err(|_| ApeTagError("APEv2 item value is too large"))?;
            body_size = body_size
                .checked_add(9)
                .and_then(|size| size.checked_add(item.key.len()))
                .and_then(|size| size.checked_add(item.value.len()))
                .filter(|&size| size <= MAX_APEV2_TAG_SIZE - FOOTER_SIZE)
                .ok_or(ApeTagError("APEv2 tag exceeds the 16 MiB limit"))?;
        }
        let mut body = Vec::new();
        body.try_reserve_exact(body_size)
            .map_err(|_| ApeTagError("APEv2 tag is too large to allocate"))?;
        for item in &self.items {
            let value_size = item.value.len() as u32;
            body.extend_from_slice(&value_size.to_le_bytes());
            body.extend_from_slice(&item.flags.to_le_bytes());
            body.extend_from_slice(&item.key);
            body.push(0);
            body.extend_from_slice(&item.value);
        }
        let tag_size = u32::try_from(body.len() + FOOTER_SIZE)
            .map_err(|_| ApeTagError("APEv2 tag is too large"))?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(body.len() + FOOTER_SIZE)
            .map_err(|_| ApeTagError("APEv2 tag is too large to allocate"))?;
        result.extend_from_slice(&body);
        result.extend_from_slice(b"APETAGEX");
        result.extend_from_slice(&2000_u32.to_le_bytes());
        result.extend_from_slice(&tag_size.to_le_bytes());
        result.extend_from_slice(&item_count.to_le_bytes());
        result.extend_from_slice(&0_u32.to_le_bytes());
        result.extend_from_slice(&[0; 8]);
        Ok(result)
    }

    pub fn items(&self) -> &[ApeItem] {
        &self.items
    }

    /// Returns valid UTF-8 text items as the shared, case-insensitive tag model.
    pub fn metadata(&self) -> Metadata {
        let mut metadata = Metadata::default();
        for item in &self.items {
            if item.item_type() == ApeItemType::Binary {
                if let Some(picture) = item.picture() {
                    metadata.add_picture(picture);
                }
                continue;
            }
            if item.item_type() != ApeItemType::Text {
                continue;
            }
            let Ok(key) = std::str::from_utf8(&item.key) else {
                continue;
            };
            for value in item.value.split(|&byte| byte == 0) {
                if let Ok(value) = std::str::from_utf8(value) {
                    metadata.insert(key, value);
                }
            }
        }
        metadata
    }

    /// Replaces text and WavPack front/back cover items, retaining other non-text items.
    pub fn replace_metadata(&mut self, metadata: &Metadata) -> Result<(), ApeTagError> {
        let mut pictures = Vec::new();
        let mut picture_items_size = 0_usize;
        for picture in metadata.pictures() {
            let key = match picture.picture_type() {
                3 => b"Cover Art (Front)".as_slice(),
                4 => b"Cover Art (Back)".as_slice(),
                _ => {
                    return Err(ApeTagError(
                        "APEv2 supports only front and back cover pictures",
                    ));
                }
            };
            let extension = picture_extension(picture)?;
            let item_size = 9_usize
                .checked_add(key.len())
                .and_then(|size| size.checked_add(extension.len() + 1))
                .and_then(|size| size.checked_add(picture.data().len()))
                .ok_or(ApeTagError("APEv2 tag exceeds the 16 MiB limit"))?;
            picture_items_size = picture_items_size
                .checked_add(item_size)
                .filter(|&size| size <= MAX_APEV2_TAG_SIZE - FOOTER_SIZE)
                .ok_or(ApeTagError("APEv2 tag exceeds the 16 MiB limit"))?;
            let mut value = Vec::new();
            value.extend_from_slice(extension);
            value.push(0);
            value.extend_from_slice(picture.data());
            pictures.push(ApeItem {
                key: key.to_vec(),
                flags: 2,
                value,
            });
        }

        self.replace_text_metadata(metadata)?;
        self.items
            .retain(|item| item.item_type() != ApeItemType::Binary || item.picture().is_none());
        self.items.extend(pictures);
        Ok(())
    }

    /// Replaces text items and retains binary, external, and reserved items.
    pub fn replace_text_metadata(&mut self, metadata: &Metadata) -> Result<(), ApeTagError> {
        for (key, values) in metadata.iter() {
            validate_key(key.as_bytes())?;
            if values.iter().any(|value| value.as_bytes().contains(&0)) {
                return Err(ApeTagError("APEv2 text values cannot contain NUL"));
            }
        }
        self.items.retain(|item| {
            item.item_type() != ApeItemType::Text
                || std::str::from_utf8(&item.key).is_err()
                || std::str::from_utf8(&item.value).is_err()
        });
        for (key, values) in metadata.iter() {
            if values.is_empty() {
                continue;
            }
            let mut value = Vec::new();
            for (index, text) in values.iter().enumerate() {
                if index != 0 {
                    value.push(0);
                }
                value.extend_from_slice(text.as_bytes());
            }
            self.items.push(ApeItem {
                key: key.as_bytes().to_vec(),
                flags: 0,
                value,
            });
        }
        Ok(())
    }
}

impl Default for ApeTag {
    fn default() -> Self {
        Self::new()
    }
}

impl ApeItem {
    pub fn key(&self) -> Option<&str> {
        std::str::from_utf8(&self.key).ok()
    }

    pub fn flags(&self) -> u32 {
        self.flags
    }

    pub fn value(&self) -> &[u8] {
        &self.value
    }

    pub fn item_type(&self) -> ApeItemType {
        match (self.flags >> 1) & 0x3 {
            0 => ApeItemType::Text,
            1 => ApeItemType::Binary,
            2 => ApeItemType::External,
            _ => ApeItemType::Reserved,
        }
    }

    fn picture(&self) -> Option<Picture> {
        let picture_type = cover_type(self.key())?;
        let separator = self.value.iter().position(|&byte| byte == 0)?;
        let filename = &self.value[..separator];
        let data = &self.value[separator + 1..];
        let extension = std::str::from_utf8(filename).ok()?;
        let media_type = match extension.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "bmp" => "image/bmp",
            "webp" => "image/webp",
            "tif" | "tiff" => "image/tiff",
            _ => return None,
        };
        Some(Picture::new(
            picture_type,
            media_type,
            "",
            0,
            0,
            0,
            0,
            data.to_vec(),
        ))
    }
}

fn cover_type(key: Option<&str>) -> Option<u32> {
    match key? {
        key if key.eq_ignore_ascii_case("Cover Art (Front)") => Some(3),
        key if key.eq_ignore_ascii_case("Cover Art (Back)") => Some(4),
        _ => None,
    }
}

fn picture_extension(picture: &Picture) -> Result<&'static [u8], ApeTagError> {
    if !picture.description().is_empty()
        || picture.dimensions() != (0, 0)
        || picture.color_depth() != 0
        || picture.colors() != 0
    {
        return Err(ApeTagError(
            "APEv2 cover items cannot store picture descriptions or dimensions",
        ));
    }
    match picture.media_type().to_ascii_lowercase().as_str() {
        "image/jpeg" => Ok(b"jpg"),
        "image/png" => Ok(b"png"),
        "image/gif" => Ok(b"gif"),
        "image/bmp" => Ok(b"bmp"),
        "image/webp" => Ok(b"webp"),
        "image/tiff" => Ok(b"tif"),
        _ => Err(ApeTagError("unsupported APEv2 cover image media type")),
    }
}

fn validate_header_footer(bytes: &[u8], header: bool) -> Result<(), ApeTagError> {
    if bytes.len() != FOOTER_SIZE || &bytes[..8] != b"APETAGEX" {
        return Err(ApeTagError("missing APEv2 signature"));
    }
    if read_u32(&bytes[8..12])? != 2000 {
        return Err(ApeTagError("unsupported APE tag version"));
    }
    let flags = read_u32(&bytes[20..24])?;
    if (flags & IS_HEADER != 0) != header
        || (flags & NO_FOOTER != 0)
        || bytes[24..].iter().any(|&byte| byte != 0)
    {
        return Err(ApeTagError("invalid APEv2 header/footer flags"));
    }
    Ok(())
}

fn validate_key(key: &[u8]) -> Result<(), ApeTagError> {
    if !(2..=255).contains(&key.len())
        || key
            .iter()
            .any(|&byte| !(0x20..=0x7e).contains(&byte) || byte == b'=')
    {
        return Err(ApeTagError("invalid APEv2 item key"));
    }
    Ok(())
}

fn read_u32(bytes: &[u8]) -> Result<u32, ApeTagError> {
    let bytes: [u8; 4] = bytes
        .try_into()
        .map_err(|_| ApeTagError("truncated APEv2 field"))?;
    Ok(u32::from_le_bytes(bytes))
}

impl fmt::Display for ApeTagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl Error for ApeTagError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_items_round_trip_and_map_repeated_values() {
        let mut metadata = Metadata::default();
        metadata.insert("artist", "One");
        metadata.insert("ARTIST", "Deux");
        metadata.insert("title", "雪");
        let mut tag = ApeTag::new();
        tag.replace_text_metadata(&metadata).unwrap();

        let encoded = tag.encode().unwrap();
        let parsed = ApeTag::parse(&encoded).unwrap();
        assert_eq!(parsed.metadata(), metadata);
        assert_eq!(parsed.encode().unwrap(), encoded);
    }

    #[test]
    fn replacing_text_preserves_non_text_items() {
        let mut tag = ApeTag {
            items: vec![ApeItem {
                key: b"Custom Binary".to_vec(),
                flags: 2,
                value: b"jpg\0image".to_vec(),
            }],
        };
        let mut metadata = Metadata::default();
        metadata.insert("title", "Track");
        tag.replace_text_metadata(&metadata).unwrap();
        let parsed = ApeTag::parse(&tag.encode().unwrap()).unwrap();
        assert_eq!(parsed.items()[0].item_type(), ApeItemType::Binary);
        assert_eq!(parsed.items()[0].value(), b"jpg\0image");
        assert_eq!(parsed.metadata(), metadata);
    }

    #[test]
    fn covers_round_trip_and_unknown_binary_items_stay_opaque() {
        let mut tag = ApeTag {
            items: vec![ApeItem {
                key: b"Custom Binary".to_vec(),
                flags: 2,
                value: b"keep me".to_vec(),
            }],
        };
        let mut metadata = Metadata::default();
        metadata.add_picture(Picture::new(
            3,
            "image/jpeg",
            "",
            0,
            0,
            0,
            0,
            b"front".to_vec(),
        ));
        metadata.add_picture(Picture::new(
            4,
            "image/png",
            "",
            0,
            0,
            0,
            0,
            b"back".to_vec(),
        ));
        tag.replace_metadata(&metadata).unwrap();

        let parsed = ApeTag::parse(&tag.encode().unwrap()).unwrap();
        assert_eq!(parsed.metadata(), metadata);
        assert_eq!(parsed.items()[0].key(), Some("Custom Binary"));
        assert_eq!(parsed.items()[0].value(), b"keep me");
        assert_eq!(parsed.items()[1].value(), b"jpg\0front");
        assert_eq!(parsed.items()[2].value(), b"png\0back");
    }

    #[test]
    fn rejects_picture_fields_apev2_cannot_represent_without_mutating_tag() {
        let mut tag = ApeTag::new();
        let mut original = Metadata::default();
        original.insert("title", "unchanged");
        tag.replace_text_metadata(&original).unwrap();
        let before = tag.clone();

        let mut unsupported = Metadata::default();
        unsupported.add_picture(Picture::new(
            3,
            "image/jpeg",
            "cover",
            0,
            0,
            0,
            0,
            b"x".to_vec(),
        ));
        assert!(tag.replace_metadata(&unsupported).is_err());
        assert_eq!(tag, before);
    }

    #[test]
    fn rejects_cover_items_that_exceed_the_tag_limit_before_copying_them() {
        let mut metadata = Metadata::default();
        metadata.add_picture(Picture::new(
            3,
            "image/jpeg",
            "",
            0,
            0,
            0,
            0,
            vec![0; MAX_APEV2_TAG_SIZE],
        ));

        assert_eq!(
            ApeTag::new().replace_metadata(&metadata).unwrap_err(),
            ApeTagError("APEv2 tag exceeds the 16 MiB limit")
        );
    }

    #[test]
    fn rejects_bad_footer_and_truncated_item_data() {
        assert!(ApeTag::parse(b"not an APE tag").is_err());
        let mut tag = ApeTag::new();
        let mut metadata = Metadata::default();
        metadata.insert("title", "ok");
        tag.replace_text_metadata(&metadata).unwrap();
        let mut encoded = tag.encode().unwrap();
        encoded[0] = 0xff;
        assert!(ApeTag::parse(&encoded).is_err());
        encoded = tag.encode().unwrap();
        encoded[32] = 0xff;
        assert!(ApeTag::parse(&encoded).is_err());
    }

    #[test]
    fn rejects_item_count_larger_than_the_tag_can_contain() {
        let mut tag = ApeTag::new().encode().unwrap();
        let footer = tag.len() - FOOTER_SIZE;
        tag[footer + 16..footer + 20].copy_from_slice(&u32::MAX.to_le_bytes());

        assert_eq!(
            ApeTag::parse(&tag).unwrap_err(),
            ApeTagError("too many APEv2 items for the tag size")
        );
    }

    #[test]
    fn parses_optional_header_and_rejects_header_flag_mismatch() {
        let mut metadata = Metadata::default();
        metadata.insert("title", "Header form");
        let mut footer_only = ApeTag::new();
        footer_only.replace_text_metadata(&metadata).unwrap();
        let encoded = footer_only.encode().unwrap();
        let mut header = encoded[encoded.len() - FOOTER_SIZE..].to_vec();
        header[20..24].copy_from_slice(&(HEADER_PRESENT | IS_HEADER).to_le_bytes());
        let mut footer = encoded[encoded.len() - FOOTER_SIZE..].to_vec();
        footer[20..24].copy_from_slice(&HEADER_PRESENT.to_le_bytes());
        let mut with_header = header;
        with_header.extend_from_slice(&encoded[..encoded.len() - FOOTER_SIZE]);
        with_header.extend_from_slice(&footer);

        assert_eq!(ApeTag::parse(&with_header).unwrap().metadata(), metadata);
        with_header[20..24].copy_from_slice(&0_u32.to_le_bytes());
        assert!(ApeTag::parse(&with_header).is_err());
    }

    #[test]
    fn rejects_invalid_text_value_and_unrepresentable_values() {
        let mut tag = ApeTag::new();
        let mut metadata = Metadata::default();
        metadata.insert("title", "bad\0value");
        assert!(tag.replace_text_metadata(&metadata).is_err());
        assert!(ApeTag::parse(&[0; 32]).is_err());
    }
}
