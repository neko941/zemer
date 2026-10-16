use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub const LOGICAL_BLOCK_SIZE: u64 = 2048;
const DESCRIPTOR_START: u64 = 16;
const MAX_DESCRIPTORS: u64 = 256;
const MAX_ENTRIES: usize = 100_000;
const MAX_PATH_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum IsoError {
    Io(io::Error),
    Invalid(&'static str),
    Unsupported(&'static str),
    Limit(&'static str),
}

impl fmt::Display for IsoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Invalid(message) => write!(f, "invalid ISO 9660 image: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported ISO 9660 profile: {message}"),
            Self::Limit(message) => write!(f, "ISO 9660 limit exceeded: {message}"),
        }
    }
}

impl std::error::Error for IsoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for IsoError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Extent {
    offset: u64,
    length: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsoEntry {
    path: Vec<String>,
    name: String,
    directory: bool,
    length: u64,
    extents: Vec<Extent>,
}

impl IsoEntry {
    pub fn path(&self) -> &[String] {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_directory(&self) -> bool {
        self.directory
    }

    pub fn len(&self) -> u64 {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

pub struct IsoImage<R> {
    reader: R,
    volume_blocks: u32,
    volume_id: String,
    joliet: bool,
    root: IsoEntry,
    entries: Vec<IsoEntry>,
}

impl<R: Read + Seek> IsoImage<R> {
    pub fn new(mut reader: R) -> Result<Self, IsoError> {
        let image_length = reader.seek(SeekFrom::End(0))?;
        if image_length < (DESCRIPTOR_START + 1) * LOGICAL_BLOCK_SIZE {
            return Err(IsoError::Invalid(
                "image is too short for a volume descriptor",
            ));
        }
        let mut primary = None;
        let mut joliet = None;
        let mut terminated = false;
        for block in DESCRIPTOR_START..DESCRIPTOR_START + MAX_DESCRIPTORS {
            let offset = block
                .checked_mul(LOGICAL_BLOCK_SIZE)
                .ok_or(IsoError::Invalid("descriptor offset overflow"))?;
            if offset + LOGICAL_BLOCK_SIZE > image_length {
                return Err(IsoError::Invalid("descriptor set is truncated"));
            }
            reader.seek(SeekFrom::Start(offset))?;
            let mut descriptor = [0_u8; LOGICAL_BLOCK_SIZE as usize];
            reader.read_exact(&mut descriptor)?;
            if &descriptor[1..6] != b"CD001" || descriptor[6] != 1 {
                return Err(IsoError::Invalid(
                    "bad volume descriptor identifier or version",
                ));
            }
            match descriptor[0] {
                0 | 3 => {}
                1 => {
                    if primary.is_some() {
                        return Err(IsoError::Invalid("duplicate primary volume descriptor"));
                    }
                    primary = Some(descriptor);
                }
                2 if is_joliet(&descriptor) => joliet = Some(descriptor),
                2 => {}
                255 => {
                    terminated = true;
                    break;
                }
                _ => {}
            }
        }
        if !terminated {
            return Err(IsoError::Invalid("volume descriptor set has no terminator"));
        }
        let primary = primary.ok_or(IsoError::Invalid(
            "volume descriptor set has no primary descriptor",
        ))?;
        let descriptor = joliet.as_ref().unwrap_or(&primary);
        let volume_blocks = both_u32(&descriptor[80..88])?;
        let block_size = both_u16(&descriptor[128..132])?;
        if block_size as u64 != LOGICAL_BLOCK_SIZE {
            return Err(IsoError::Unsupported(
                "logical block size is not 2048 bytes",
            ));
        }
        let volume_length = u64::from(volume_blocks)
            .checked_mul(LOGICAL_BLOCK_SIZE)
            .ok_or(IsoError::Invalid("volume size overflow"))?;
        if volume_length > image_length || volume_blocks <= 16 {
            return Err(IsoError::Invalid("declared volume size exceeds the image"));
        }
        let volume_id = if joliet.is_some() {
            decode_joliet(&descriptor[40..72])?
        } else {
            decode_ascii(&descriptor[40..72])?
        };
        let root_record_length = usize::from(descriptor[156]);
        if root_record_length < 34 || 156 + root_record_length > descriptor.len() {
            return Err(IsoError::Invalid("invalid root directory record"));
        }
        let root_record = parse_record(
            &descriptor[156..156 + root_record_length],
            volume_blocks,
            joliet.is_some(),
        )?;
        if !root_record.directory || root_record.name != "." {
            return Err(IsoError::Invalid("root record is not a directory"));
        }
        let root = root_record.into_entry(Vec::new())?;
        let mut image = Self {
            reader,
            volume_blocks,
            volume_id,
            joliet: joliet.is_some(),
            root,
            entries: Vec::new(),
        };
        image.read_tree()?;
        Ok(image)
    }

    pub fn volume_id(&self) -> &str {
        &self.volume_id
    }

    pub fn entries(&self) -> &[IsoEntry] {
        &self.entries
    }

    pub fn copy_file<W: Write>(&mut self, entry: &IsoEntry, mut output: W) -> Result<(), IsoError> {
        if entry.directory {
            return Err(IsoError::Invalid("cannot copy a directory as a file"));
        }
        if !self.entries.contains(entry) {
            return Err(IsoError::Invalid("entry does not belong to this image"));
        }
        let mut remaining = entry.length;
        let mut buffer = [0_u8; 64 * 1024];
        for extent in &entry.extents {
            self.reader.seek(SeekFrom::Start(extent.offset))?;
            let mut extent_remaining = u64::from(extent.length);
            while extent_remaining > 0 {
                let count = usize::try_from(extent_remaining.min(buffer.len() as u64))
                    .map_err(|_| IsoError::Invalid("extent length overflow"))?;
                self.reader.read_exact(&mut buffer[..count])?;
                output.write_all(&buffer[..count])?;
                extent_remaining -= count as u64;
                remaining -= count as u64;
            }
        }
        if remaining != 0 {
            return Err(IsoError::Invalid(
                "file extents do not cover the declared length",
            ));
        }
        Ok(())
    }

    pub fn extract_to(&mut self, output_dir: &Path) -> Result<(), IsoError> {
        if output_dir.exists() {
            return Err(IsoError::Invalid("output directory already exists"));
        }
        let output_paths = validate_output_paths(&self.entries)?;
        fs::create_dir(output_dir)?;
        let result = (|| {
            for (entry, relative) in self.entries.clone().iter().zip(output_paths) {
                let path = output_dir.join(relative);
                if entry.directory {
                    fs::create_dir_all(&path)?;
                } else {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)?;
                    self.copy_file(entry, file)?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(output_dir);
        }
        result
    }

    fn read_tree(&mut self) -> Result<(), IsoError> {
        let mut pending = vec![(Vec::<String>::new(), self.root.clone())];
        let mut visited = BTreeSet::new();
        let mut path_bytes = 0_usize;
        while let Some((parent, directory)) = pending.pop() {
            for extent in &directory.extents {
                let key = (extent.offset, extent.length);
                if !visited.insert(key) {
                    return Err(IsoError::Invalid(
                        "directory extent is referenced more than once",
                    ));
                }
            }
            let entries = self.read_directory(&directory, &parent)?;
            for entry in entries {
                path_bytes = path_bytes
                    .checked_add(entry.path.iter().map(String::len).sum::<usize>())
                    .ok_or(IsoError::Limit("path metadata size overflow"))?;
                if self.entries.len() == MAX_ENTRIES || path_bytes > MAX_PATH_BYTES {
                    return Err(IsoError::Limit("directory entry metadata"));
                }
                if entry.directory {
                    pending.push((entry.path.clone(), entry.clone()));
                }
                self.entries.push(entry);
            }
        }
        Ok(())
    }

    fn read_directory(
        &mut self,
        directory: &IsoEntry,
        parent: &[String],
    ) -> Result<Vec<IsoEntry>, IsoError> {
        let mut records = Vec::new();
        for extent in &directory.extents {
            self.reader.seek(SeekFrom::Start(extent.offset))?;
            let mut remaining = u64::from(extent.length);
            let mut block = [0_u8; LOGICAL_BLOCK_SIZE as usize];
            while remaining > 0 {
                let length = usize::try_from(remaining.min(LOGICAL_BLOCK_SIZE))
                    .map_err(|_| IsoError::Invalid("directory length overflow"))?;
                self.reader.read_exact(&mut block[..length])?;
                let mut offset = 0;
                while offset < length {
                    let record_length = usize::from(block[offset]);
                    if record_length == 0 {
                        break;
                    }
                    if record_length > length - offset {
                        return Err(IsoError::Invalid(
                            "directory record crosses a logical block",
                        ));
                    }
                    let record = parse_record(
                        &block[offset..offset + record_length],
                        self.volume_blocks,
                        self.joliet,
                    )?;
                    if record.name != "." && record.name != ".." {
                        records.push(record);
                        if records.len() > MAX_ENTRIES {
                            return Err(IsoError::Limit("entries in a directory"));
                        }
                    }
                    offset += record_length;
                }
                let advanced = length as u64;
                remaining -= advanced;
            }
        }
        let mut result = Vec::new();
        let mut pending: Option<(Record, Vec<Extent>)> = None;
        for record in records {
            if let Some((previous, mut extents)) = pending.take() {
                if previous.name != record.name
                    || previous.directory != record.directory
                    || previous.parent != record.parent
                {
                    return Err(IsoError::Invalid(
                        "multi-extent file records are not consecutive",
                    ));
                }
                extents.extend(record.extents.clone());
                if record.multi_extent {
                    pending = Some((record, extents));
                } else {
                    let path = append_path(parent, &record.name);
                    result.push(record.into_entry_with_extents(path, extents)?);
                }
            } else if record.multi_extent {
                pending = Some((record.clone(), record.extents.clone()));
            } else {
                let path = append_path(parent, &record.name);
                result.push(record.into_entry(path)?);
            }
        }
        if pending.is_some() {
            return Err(IsoError::Invalid("unterminated multi-extent file"));
        }
        Ok(result)
    }
}

fn validate_output_paths(entries: &[IsoEntry]) -> Result<Vec<PathBuf>, IsoError> {
    let mut normalized = BTreeMap::<String, bool>::new();
    let mut output_paths = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut output = PathBuf::new();
        let mut key = String::new();
        for component in &entry.path {
            validate_output_component(component)?;
            if !key.is_empty() {
                key.push('/');
            }
            key.push_str(&component.to_lowercase());
            output.push(component);
        }
        let mut prefix = String::new();
        let mut components = key.split('/').peekable();
        while let Some(component) = components.next() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            if components.peek().is_some() && normalized.get(&prefix) == Some(&false) {
                return Err(IsoError::Invalid("a file blocks a child output path"));
            }
        }
        if !entry.directory {
            let child_prefix = format!("{key}/");
            if normalized
                .range(child_prefix.clone()..)
                .next()
                .is_some_and(|(existing, _)| existing.starts_with(&child_prefix))
            {
                return Err(IsoError::Invalid(
                    "a file conflicts with a child output path",
                ));
            }
        }
        if normalized.insert(key, entry.directory).is_some() {
            return Err(IsoError::Invalid("case-insensitive output path collision"));
        }
        output_paths.push(output);
    }
    Ok(output_paths)
}

#[derive(Clone)]
struct Record {
    name: String,
    directory: bool,
    parent: bool,
    multi_extent: bool,
    extents: Vec<Extent>,
}

impl Record {
    fn into_entry(self, path: Vec<String>) -> Result<IsoEntry, IsoError> {
        let extents = self.extents.clone();
        self.into_entry_with_extents(path, extents)
    }

    fn into_entry_with_extents(
        self,
        path: Vec<String>,
        extents: Vec<Extent>,
    ) -> Result<IsoEntry, IsoError> {
        let length = extents
            .iter()
            .try_fold(0_u64, |sum, extent| {
                sum.checked_add(u64::from(extent.length))
            })
            .ok_or(IsoError::Invalid("file length overflow"))?;
        Ok(IsoEntry {
            name: self.name,
            path,
            directory: self.directory,
            length,
            extents,
        })
    }
}

fn parse_record(record: &[u8], volume_blocks: u32, joliet: bool) -> Result<Record, IsoError> {
    if record.len() < 34 || usize::from(record[0]) != record.len() {
        return Err(IsoError::Invalid("directory record has an invalid length"));
    }
    let identifier_length = usize::from(record[32]);
    if identifier_length == 0 {
        return Err(IsoError::Invalid("empty file identifier"));
    }
    let identifier_end = 33_usize
        .checked_add(identifier_length)
        .ok_or(IsoError::Invalid("file identifier length overflow"))?;
    if identifier_end > record.len() {
        return Err(IsoError::Invalid(
            "file identifier exceeds its directory record",
        ));
    }
    let identifier = &record[33..identifier_end];
    let current = identifier == [0];
    let parent = identifier == [1];
    let name = if current {
        ".".to_owned()
    } else if parent {
        "..".to_owned()
    } else if joliet {
        strip_version(decode_joliet(identifier)?)
    } else {
        strip_version(decode_ascii(identifier)?)
    };
    let xar_blocks = u64::from(record[1]);
    let extent_lbn = u64::from(both_u32(&record[2..10])?);
    let data_length = both_u32(&record[10..18])?;
    let flags = record[25];
    if both_u16(&record[28..32])? != 1 {
        return Err(IsoError::Unsupported(
            "multiple-volume sets are not supported",
        ));
    }
    if record[26] != 0 || record[27] != 0 {
        return Err(IsoError::Unsupported(
            "interleaved file sections are not supported",
        ));
    }
    let extent_block = extent_lbn
        .checked_add(xar_blocks)
        .ok_or(IsoError::Invalid("extent location overflow"))?;
    let offset = extent_block
        .checked_mul(LOGICAL_BLOCK_SIZE)
        .ok_or(IsoError::Invalid("extent offset overflow"))?;
    let end = offset
        .checked_add(u64::from(data_length))
        .ok_or(IsoError::Invalid("extent end overflow"))?;
    let volume_length = u64::from(volume_blocks)
        .checked_mul(LOGICAL_BLOCK_SIZE)
        .ok_or(IsoError::Invalid("volume length overflow"))?;
    if end > volume_length {
        return Err(IsoError::Invalid("file extent exceeds the declared volume"));
    }
    Ok(Record {
        name,
        directory: flags & 0x02 != 0,
        parent,
        multi_extent: flags & 0x80 != 0,
        extents: vec![Extent {
            offset,
            length: data_length,
        }],
    })
}

fn append_path(parent: &[String], name: &str) -> Vec<String> {
    let mut path = parent.to_vec();
    path.push(name.to_owned());
    path
}

fn is_joliet(descriptor: &[u8]) -> bool {
    descriptor[0] == 2
        && descriptor[88] == b'%'
        && descriptor[89] == b'/'
        && matches!(descriptor[90], b'@' | b'C' | b'E')
}

fn decode_ascii(bytes: &[u8]) -> Result<String, IsoError> {
    let value = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    let value = value
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .collect::<Vec<_>>();
    if !value.is_ascii() {
        return Err(IsoError::Unsupported("non-ASCII primary identifiers"));
    }
    Ok(String::from_utf8(value)
        .map_err(|_| IsoError::Invalid("invalid ASCII identifier"))?
        .trim_end()
        .to_owned())
}

fn decode_joliet(bytes: &[u8]) -> Result<String, IsoError> {
    if bytes.len() % 2 != 0 {
        return Err(IsoError::Invalid(
            "Joliet identifier has an odd byte length",
        ));
    }
    let units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect::<Vec<_>>();
    Ok(String::from_utf16(&units)
        .map_err(|_| IsoError::Invalid("invalid Joliet UTF-16 identifier"))?
        .trim_end()
        .to_owned())
}

fn strip_version(mut name: String) -> String {
    if let Some((base, version)) = name.rsplit_once(';') {
        if !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit()) {
            name = base.to_owned();
        }
    }
    name
}

fn both_u16(bytes: &[u8]) -> Result<u16, IsoError> {
    if bytes.len() != 4 {
        return Err(IsoError::Invalid("invalid dual-endian 16-bit field"));
    }
    let little = u16::from_le_bytes([bytes[0], bytes[1]]);
    let big = u16::from_be_bytes([bytes[2], bytes[3]]);
    if little != big {
        return Err(IsoError::Invalid("dual-endian 16-bit values disagree"));
    }
    Ok(little)
}

fn both_u32(bytes: &[u8]) -> Result<u32, IsoError> {
    if bytes.len() != 8 {
        return Err(IsoError::Invalid("invalid dual-endian 32-bit field"));
    }
    let little = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let big = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if little != big {
        return Err(IsoError::Invalid("dual-endian 32-bit values disagree"));
    }
    Ok(little)
}

fn validate_output_component(component: &str) -> Result<(), IsoError> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.ends_with(' ')
        || component.ends_with('.')
        || component.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
    {
        return Err(IsoError::Unsupported(
            "file name is not safe on Windows and Unix",
        ));
    }
    let base = component
        .split('.')
        .next()
        .unwrap_or(component)
        .to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(IsoError::Unsupported("file name is reserved on Windows"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_path_preflight_handles_maximum_flat_directory() {
        let entries = (0..MAX_ENTRIES)
            .map(|index| {
                let name = format!("FILE{index:05}.TXT");
                IsoEntry {
                    path: vec![name.clone()],
                    name,
                    directory: false,
                    length: 0,
                    extents: Vec::new(),
                }
            })
            .collect::<Vec<_>>();

        assert_eq!(validate_output_paths(&entries).unwrap().len(), MAX_ENTRIES);
    }

    #[test]
    fn output_path_preflight_rejects_a_file_added_after_its_child() {
        let entries = [
            IsoEntry {
                path: vec!["parent".to_owned(), "child".to_owned()],
                name: "child".to_owned(),
                directory: false,
                length: 0,
                extents: Vec::new(),
            },
            IsoEntry {
                path: vec!["parent".to_owned()],
                name: "parent".to_owned(),
                directory: false,
                length: 0,
                extents: Vec::new(),
            },
        ];

        assert!(matches!(
            validate_output_paths(&entries),
            Err(IsoError::Invalid(
                "a file conflicts with a child output path"
            ))
        ));
    }
}
