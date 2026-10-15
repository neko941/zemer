const INFO_TAGS: &[(&str, [u8; 4])] = &[
    ("ARCHIVAL_LOCATION", *b"IARL"),
    ("ARTIST", *b"IART"),
    ("COMMISSIONED", *b"ICMS"),
    ("COMMENT", *b"ICMT"),
    ("COPYRIGHT", *b"ICOP"),
    ("DATE", *b"ICRD"),
    ("CROPPED", *b"ICRP"),
    ("DIMENSIONS", *b"IDIM"),
    ("DPI", *b"IDPI"),
    ("ENGINEER", *b"IENG"),
    ("GENRE", *b"IGNR"),
    ("KEYWORDS", *b"IKEY"),
    ("LIGHTNESS", *b"ILGT"),
    ("MEDIUM", *b"IMED"),
    ("TITLE", *b"INAM"),
    ("PALETTE_SETTING", *b"IPLT"),
    ("ALBUM", *b"IPRD"),
    ("SUBJECT", *b"ISBJ"),
    ("SOFTWARE", *b"ISFT"),
    ("SHARPNESS", *b"ISHP"),
    ("SOURCE", *b"ISRC"),
    ("SOURCE_FORM", *b"ISRF"),
    ("TECHNICIAN", *b"ITCH"),
];

pub(super) fn key_from_id(id: &[u8; 4]) -> Option<&'static str> {
    INFO_TAGS
        .iter()
        .find_map(|(key, tag)| (tag == id).then_some(*key))
}

pub(super) fn id_from_key(key: &str) -> Option<[u8; 4]> {
    INFO_TAGS
        .iter()
        .find_map(|(name, id)| (*name == key).then_some(*id))
}

pub fn supports_info_metadata_key(key: &str) -> bool {
    id_from_key(&key.trim().to_ascii_uppercase()).is_some()
}

pub(super) fn is_managed_id(id: &[u8; 4]) -> bool {
    key_from_id(id).is_some()
}

#[cfg(test)]
mod tests {
    use super::{INFO_TAGS, id_from_key, is_managed_id, key_from_id, supports_info_metadata_key};

    #[test]
    fn maps_every_registered_info_text_field_in_both_directions() {
        assert_eq!(INFO_TAGS.len(), 23);
        for &(key, id) in INFO_TAGS {
            assert_eq!(id_from_key(key), Some(id));
            assert_eq!(key_from_id(&id), Some(key));
            assert!(is_managed_id(&id));
        }
        assert!(supports_info_metadata_key(" engineer "));
        assert!(!supports_info_metadata_key("TRACKNUMBER"));
    }
}
