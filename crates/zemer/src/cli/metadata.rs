use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use wav::{BextChunk, WavDecoder};
use zemer_core::{AudioError, Metadata};

use super::{
    CliError, MetadataEditReport,
    audio::{self, AudioInput},
    usage_error,
};

pub(super) fn metadata_args(args: &[std::ffi::OsString]) -> Result<MetadataEditReport, CliError> {
    if args.first().and_then(|arg| arg.to_str()) != Some("edit") {
        return Err(usage_error());
    }
    let mut paths = Vec::new();
    let mut sets = Vec::new();
    let mut removes = Vec::new();
    let mut bext = BextOptions::default();
    let mut index = 1;
    while index < args.len() {
        if args[index] == "--json" {
            index += 1;
        } else if args[index] == "--set" {
            index += 1;
            let Some(argument) = args.get(index).and_then(|arg| arg.to_str()) else {
                return Err(usage_error());
            };
            let Some((key, value)) = argument.split_once('=') else {
                return Err(usage_error());
            };
            sets.push((normalize_metadata_key(key)?, value.to_owned()));
            index += 1;
        } else if args[index] == "--remove" {
            index += 1;
            let Some(key) = args.get(index).and_then(|arg| arg.to_str()) else {
                return Err(usage_error());
            };
            removes.push(normalize_metadata_key(key)?);
            index += 1;
        } else if let Some((flag, field)) = bext_flag(&args[index]) {
            index += 1;
            let Some(value) = args.get(index).and_then(|arg| arg.to_str()) else {
                return Err(usage_error());
            };
            match field {
                BextField::Description => bext.description = Some(fixed_ascii(value, flag)?),
                BextField::Originator => bext.originator = Some(fixed_ascii(value, flag)?),
                BextField::OriginatorReference => {
                    bext.originator_reference = Some(fixed_ascii(value, flag)?)
                }
                BextField::OriginationDate => {
                    bext.origination_date = Some(fixed_ascii(value, flag)?)
                }
                BextField::OriginationTime => {
                    bext.origination_time = Some(fixed_ascii(value, flag)?)
                }
                BextField::TimeReference => {
                    bext.time_reference = Some(value.parse().map_err(|_| invalid_bext_value(flag))?)
                }
                BextField::Version => {
                    bext.version = Some(value.parse().map_err(|_| invalid_bext_value(flag))?)
                }
                BextField::UmidHex => bext.umid = Some(parse_hex(value, flag)?),
                BextField::ReservedHex => bext.reserved = Some(parse_hex(value, flag)?),
                BextField::CodingHistory => {
                    if !value.is_ascii() {
                        return Err(invalid_bext_value(flag));
                    }
                    bext.coding_history = Some(value.as_bytes().to_vec());
                }
            }
            index += 1;
        } else {
            paths.push(PathBuf::from(&args[index]));
            index += 1;
        }
    }
    if paths.len() != 2 || (sets.is_empty() && removes.is_empty() && bext.is_empty()) {
        return Err(usage_error());
    }

    let set_keys: BTreeSet<_> = sets.iter().map(|(key, _)| key.as_str()).collect();
    if removes.iter().any(|key| set_keys.contains(key.as_str())) {
        return Err(CliError {
            kind: "usage",
            message: "a metadata key cannot be both set and removed".to_owned(),
        });
    }
    let input = paths.remove(0);
    let output = paths.remove(0);
    edit_audio_metadata(input, output, &sets, &removes, &bext)
}

#[derive(Default)]
struct BextOptions {
    description: Option<[u8; 256]>,
    originator: Option<[u8; 32]>,
    originator_reference: Option<[u8; 32]>,
    origination_date: Option<[u8; 10]>,
    origination_time: Option<[u8; 8]>,
    time_reference: Option<u64>,
    version: Option<u16>,
    umid: Option<[u8; 64]>,
    reserved: Option<[u8; 190]>,
    coding_history: Option<Vec<u8>>,
}

impl BextOptions {
    fn is_empty(&self) -> bool {
        self.description.is_none()
            && self.originator.is_none()
            && self.originator_reference.is_none()
            && self.origination_date.is_none()
            && self.origination_time.is_none()
            && self.time_reference.is_none()
            && self.version.is_none()
            && self.umid.is_none()
            && self.reserved.is_none()
            && self.coding_history.is_none()
    }

    fn apply(&self, mut bext: BextChunk) -> BextChunk {
        if let Some(value) = self.description {
            bext.description = value;
        }
        if let Some(value) = self.originator {
            bext.originator = value;
        }
        if let Some(value) = self.originator_reference {
            bext.originator_reference = value;
        }
        if let Some(value) = self.origination_date {
            bext.origination_date = value;
        }
        if let Some(value) = self.origination_time {
            bext.origination_time = value;
        }
        if let Some(value) = self.time_reference {
            bext.time_reference = value;
        }
        if let Some(value) = self.version {
            bext.version = value;
        }
        if let Some(value) = self.umid {
            bext.umid = value;
        }
        if let Some(value) = self.reserved {
            bext.reserved = value;
        }
        if let Some(value) = &self.coding_history {
            bext.coding_history.clone_from(value);
        }
        bext
    }

    fn count(&self) -> usize {
        [
            self.description.is_some(),
            self.originator.is_some(),
            self.originator_reference.is_some(),
            self.origination_date.is_some(),
            self.origination_time.is_some(),
            self.time_reference.is_some(),
            self.version.is_some(),
            self.umid.is_some(),
            self.reserved.is_some(),
            self.coding_history.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count()
    }
}

#[derive(Clone, Copy)]
enum BextField {
    Description,
    Originator,
    OriginatorReference,
    OriginationDate,
    OriginationTime,
    TimeReference,
    Version,
    UmidHex,
    ReservedHex,
    CodingHistory,
}

fn bext_flag(arg: &std::ffi::OsStr) -> Option<(&'static str, BextField)> {
    Some(match arg.to_str()? {
        "--bext-description" => ("--bext-description", BextField::Description),
        "--bext-originator" => ("--bext-originator", BextField::Originator),
        "--bext-originator-reference" => (
            "--bext-originator-reference",
            BextField::OriginatorReference,
        ),
        "--bext-origination-date" => ("--bext-origination-date", BextField::OriginationDate),
        "--bext-origination-time" => ("--bext-origination-time", BextField::OriginationTime),
        "--bext-time-reference" => ("--bext-time-reference", BextField::TimeReference),
        "--bext-version" => ("--bext-version", BextField::Version),
        "--bext-umid-hex" => ("--bext-umid-hex", BextField::UmidHex),
        "--bext-reserved-hex" => ("--bext-reserved-hex", BextField::ReservedHex),
        "--bext-coding-history" => ("--bext-coding-history", BextField::CodingHistory),
        _ => return None,
    })
}

fn fixed_ascii<const N: usize>(value: &str, flag: &str) -> Result<[u8; N], CliError> {
    if !value.is_ascii() || value.as_bytes().contains(&0) || value.len() > N {
        return Err(invalid_bext_value(flag));
    }
    let mut field = [0; N];
    field[..value.len()].copy_from_slice(value.as_bytes());
    Ok(field)
}

fn parse_hex<const N: usize>(value: &str, flag: &str) -> Result<[u8; N], CliError> {
    if value.len() != N * 2 || !value.is_ascii() {
        return Err(invalid_bext_value(flag));
    }
    let mut bytes = [0; N];
    for (byte, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let high = hex_nibble(pair[0]).ok_or_else(|| invalid_bext_value(flag))?;
        let low = hex_nibble(pair[1]).ok_or_else(|| invalid_bext_value(flag))?;
        *byte = (high << 4) | low;
    }
    Ok(bytes)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn invalid_bext_value(flag: &str) -> CliError {
    CliError {
        kind: "invalid_data",
        message: format!("invalid value for {flag}"),
    }
}

fn normalize_metadata_key(key: &str) -> Result<String, CliError> {
    let key = key.trim();
    if key.is_empty()
        || key
            .bytes()
            .any(|byte| !(0x20..=0x7e).contains(&byte) || byte == b'=')
    {
        return Err(CliError {
            kind: "invalid_data",
            message: "metadata keys must be printable ASCII without '='".to_owned(),
        });
    }
    Ok(key.to_ascii_uppercase())
}

fn edit_audio_metadata(
    input: PathBuf,
    output: PathBuf,
    sets: &[(String, String)],
    removes: &[String],
    bext_options: &BextOptions,
) -> Result<MetadataEditReport, CliError> {
    if let Some((decoder, format @ ("dsf" | "dsdiff"), _, _, _)) = audio::open_dsd_decoder(&input)?
    {
        if !bext_options.is_empty() {
            return Err(CliError {
                kind: "unsupported",
                message: "BWF bext options are supported only for WAVE files".to_owned(),
            });
        }
        let (metadata, updated_fields) =
            apply_metadata_changes(decoder.metadata().clone(), sets, removes);
        drop(decoder);
        let input_file = File::open(&input).map_err(|error| CliError {
            kind: "io",
            message: format!("{}: {error}", input.display()),
        })?;
        let mut output_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .map_err(|error| CliError {
                kind: "io",
                message: format!("{}: {error}", output.display()),
            })?;
        let result = match format {
            "dsf" => dsf::rewrite_metadata(input_file, &mut output_file, &metadata),
            "dsdiff" => dsdiff::rewrite_metadata(input_file, &mut output_file, &metadata),
            _ => unreachable!(),
        }
        .map_err(|error| cli_rewrite_error(error, &input, &output))
        .and_then(|()| {
            output_file.flush().map_err(|error| CliError {
                kind: "io",
                message: format!("{}: {error}", output.display()),
            })
        });
        drop(output_file);
        if let Err(error) = result {
            let _ = fs::remove_file(&output);
            return Err(error);
        }
        return Ok(MetadataEditReport {
            output,
            updated_fields,
            format,
        });
    }
    let AudioInput {
        decoder, format, ..
    } = audio::open_decoder(&input, None)?;
    if format != "flac"
        && format != "ogg-flac"
        && format != "ogg-vorbis"
        && format != "wav"
        && format != "wavpack"
        && format != "aiff"
        && format != "aifc"
        && format != "au"
        && format != "caf"
    {
        return Err(CliError {
            kind: "unsupported",
            message: "metadata editing supports WAVE, AIFF/AIFC, AU, CAF, DSF, DSDIFF, native FLAC, Ogg-FLAC, Ogg Vorbis, and WavPack files"
                .to_owned(),
        });
    }
    if !bext_options.is_empty() && format != "wav" {
        return Err(CliError {
            kind: "unsupported",
            message: "BWF bext options are supported only for WAVE files".to_owned(),
        });
    }
    let (metadata, updated_fields) =
        apply_metadata_changes(decoder.metadata().clone(), sets, removes);
    let updated_fields = updated_fields + bext_options.count();
    let input_file = File::open(&input).map_err(|error| CliError {
        kind: "io",
        message: format!("{}: {error}", input.display()),
    })?;
    let mut output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .map_err(|error| CliError {
            kind: "io",
            message: format!("{}: {error}", output.display()),
        })?;
    let result = (|| {
        match format {
            "flac" => flac::rewrite_metadata(input_file, &mut output_file, &metadata),
            "ogg-flac" => flac::rewrite_ogg_metadata(input_file, &mut output_file, &metadata),
            "ogg-vorbis" => vorbis::rewrite_metadata(input_file, &mut output_file, &metadata),
            "wav" if !bext_options.is_empty() => (|| {
                let bext_file = File::open(&input)?;
                let existing = WavDecoder::new(bext_file)?;
                let updated_bext = bext_options.apply(existing.bext().cloned().unwrap_or_default());
                drop(existing);
                wav::rewrite_metadata_with_bext(
                    input_file,
                    &mut output_file,
                    &metadata,
                    &updated_bext,
                )
                .map(|_| ())
            })(),
            "wav" => wav::rewrite_metadata(input_file, &mut output_file, &metadata).map(|_| ()),
            "aiff" | "aifc" => aiff::rewrite_metadata(input_file, &mut output_file, &metadata),
            "au" => au::rewrite_metadata(input_file, &mut output_file, &metadata).map(|_| ()),
            "caf" => caf::rewrite_metadata(input_file, &mut output_file, &metadata),
            "wavpack" => wavpack::rewrite_metadata(input_file, &mut output_file, &metadata),
            _ => unreachable!("format was checked above"),
        }
        .map_err(|error| cli_rewrite_error(error, &input, &output))?;
        output_file.flush().map_err(|error| CliError {
            kind: "io",
            message: format!("{}: {error}", output.display()),
        })
    })();
    drop(output_file);
    if let Err(error) = result {
        let _ = fs::remove_file(&output);
        return Err(error);
    }
    Ok(MetadataEditReport {
        output,
        updated_fields,
        format,
    })
}

fn apply_metadata_changes(
    mut metadata: Metadata,
    sets: &[(String, String)],
    removes: &[String],
) -> (Metadata, usize) {
    for key in removes {
        metadata.remove(key);
    }
    let mut set_keys = BTreeSet::new();
    for (key, value) in sets {
        if set_keys.insert(key.as_str()) {
            metadata.remove(key);
        }
        metadata.insert(key, value.clone());
    }
    let updated_fields = set_keys.len() + removes.iter().collect::<BTreeSet<_>>().len();
    (metadata, updated_fields)
}

fn cli_rewrite_error(error: AudioError, input: &Path, output: &Path) -> CliError {
    let kind = match &error {
        AudioError::Io(_) => "io",
        AudioError::InvalidFormat(_) => "invalid_format",
        AudioError::InvalidData(_) => "invalid_data",
        AudioError::Unsupported(_) => "unsupported",
    };
    CliError {
        kind,
        message: format!("{} -> {}: {error}", input.display(), output.display()),
    }
}
