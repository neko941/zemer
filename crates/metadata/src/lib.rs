use std::collections::BTreeMap;

pub mod apev2;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Metadata {
    tags: BTreeMap<String, Vec<String>>,
    pictures: Vec<Picture>,
}

/// An embedded audio-file picture with the common FLAC/ID3 picture fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Picture {
    picture_type: u32,
    media_type: String,
    description: String,
    width: u32,
    height: u32,
    color_depth: u32,
    colors: u32,
    data: Vec<u8>,
}

impl Picture {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        picture_type: u32,
        media_type: impl Into<String>,
        description: impl Into<String>,
        width: u32,
        height: u32,
        color_depth: u32,
        colors: u32,
        data: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            picture_type,
            media_type: media_type.into(),
            description: description.into(),
            width,
            height,
            color_depth,
            colors,
            data: data.into(),
        }
    }

    pub fn picture_type(&self) -> u32 {
        self.picture_type
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn color_depth(&self) -> u32 {
        self.color_depth
    }

    pub fn colors(&self) -> u32 {
        self.colors
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl Metadata {
    pub fn insert(&mut self, key: impl AsRef<str>, value: impl Into<String>) {
        self.tags
            .entry(key.as_ref().trim().to_ascii_uppercase())
            .or_default()
            .push(value.into());
    }

    pub fn values(&self, key: &str) -> &[String] {
        self.tags
            .get(&key.trim().to_ascii_uppercase())
            .map_or(&[], Vec::as_slice)
    }

    pub fn remove(&mut self, key: &str) -> bool {
        self.tags.remove(&key.trim().to_ascii_uppercase()).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.tags
            .iter()
            .map(|(key, values)| (key.as_str(), values.as_slice()))
    }

    pub fn add_picture(&mut self, picture: Picture) {
        self.pictures.push(picture);
    }

    pub fn pictures(&self) -> &[Picture] {
        &self.pictures
    }

    pub fn clear_pictures(&mut self) {
        self.pictures.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{Metadata, Picture};

    #[test]
    fn keys_are_case_insensitive_and_retain_repeated_values() {
        let mut metadata = Metadata::default();
        metadata.insert("artist", "One");
        metadata.insert("ARTIST", "Two");
        assert_eq!(metadata.values("Artist"), ["One", "Two"]);
    }

    #[test]
    fn removes_all_values_for_a_case_insensitive_key() {
        let mut metadata = Metadata::default();
        metadata.insert("artist", "One");
        metadata.insert("ARTIST", "Two");
        assert!(metadata.remove(" Artist "));
        assert!(metadata.values("artist").is_empty());
        assert!(!metadata.remove("artist"));
    }

    #[test]
    fn stores_repeated_pictures_and_clears_them() {
        let picture = Picture::new(3, "image/jpeg", "front cover", 1, 2, 24, 0, [1, 2, 3]);
        let mut metadata = Metadata::default();
        metadata.add_picture(picture.clone());
        metadata.add_picture(picture.clone());
        assert_eq!(metadata.pictures(), [picture.clone(), picture]);
        metadata.clear_pictures();
        assert!(metadata.pictures().is_empty());
    }
}
