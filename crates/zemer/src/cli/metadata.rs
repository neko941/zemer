use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

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
        } else {
            paths.push(PathBuf::from(&args[index]));
            index += 1;
        }
    }
    if paths.len() != 2 || (sets.is_empty() && removes.is_empty()) {
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
    edit_audio_metadata(input, output, &sets, &removes)
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
) -> Result<MetadataEditReport, CliError> {
    if let Some((decoder, format @ ("dsf" | "dsdiff"), _, _, _)) = audio::open_dsd_decoder(&input)?
    {
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
    let (metadata, updated_fields) =
        apply_metadata_changes(decoder.metadata().clone(), sets, removes);
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
