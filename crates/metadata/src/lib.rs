use std::collections::BTreeMap;

pub mod apev2;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Metadata {
    tags: BTreeMap<String, Vec<String>>,
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
}

#[cfg(test)]
mod tests {
    use super::Metadata;

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
}
