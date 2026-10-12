use std::{
    collections::BTreeSet,
    env,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use flac::{FlacDecoder, FlacEncoder, OggFlacDecoder};
use wav::{G711Encoding, WavContainer, WavDecoder, WavWriter};
use wavpack::{WavPackDecoder, WavPackEncoder};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioError, AudioSpec, Metadata, SampleFormat,
    convert_sample_format,
};

#[path = "cue.rs"]
mod cue_cli;

struct Report {
    format: &'static str,
    spec: AudioSpec,
    frames: u64,
    metadata: Metadata,
    output_encoding: Option<&'static str>,
    container: Option<WavContainer>,
}

struct AudioInput {
    decoder: Box<dyn AudioDecoder>,
    format: &'static str,
    container: Option<WavContainer>,
}

#[derive(Clone, Copy)]
enum OutputFormat {
    Samples(SampleFormat),
    G711(G711Encoding),
    Flac,
    WavPack,
}

impl OutputFormat {
    fn input_sample_format(self) -> Option<SampleFormat> {
        match self {
            Self::Samples(format) => Some(format),
            Self::G711(_) => Some(SampleFormat::PcmI16),
            Self::Flac => None,
            Self::WavPack => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Samples(format) => sample_format_name(format),
            Self::G711(G711Encoding::ALaw) => "g711_alaw",
            Self::G711(G711Encoding::MuLaw) => "g711_mulaw",
            Self::Flac => "flac",
            Self::WavPack => "wavpack",
        }
    }
}

enum CommandReport {
    Inspect(Report),
    Convert(Report, PathBuf),
    Cue(cue_cli::CueReport),
    MetadataEdit(MetadataEditReport),
}

struct MetadataEditReport {
    output: PathBuf,
    updated_fields: usize,
    format: &'static str,
}

struct CliError {
    kind: &'static str,
    message: String,
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let json = args.iter().any(|arg| arg == "--json");
    match command_args(&args) {
        Ok(CommandReport::Inspect(report)) => {
            if json {
                println!("{}", report_json(&report));
            } else {
                print_report(&report);
            }
            ExitCode::SUCCESS
        }
        Ok(CommandReport::Convert(report, output)) => {
            if json {
                println!("{}", conversion_json(&report, &output));
            } else {
                println!("converted_frames: {}", report.frames);
                println!("output: {}", output.display());
                if let Some(container) = report.container {
                    println!("container: {}", container_name(container));
                }
                println!(
                    "sample_format: {}",
                    sample_format_name(report.spec.sample_format())
                );
                if let Some(encoding) = report.output_encoding {
                    println!("output_encoding: {encoding}");
                }
            }
            ExitCode::SUCCESS
        }
        Ok(CommandReport::Cue(report)) => {
            if json {
                println!(
                    "{{\"operation\":{},\"cue\":{},\"tracks\":{}}}",
                    json_string(report.action),
                    json_string(&report.cue_path.display().to_string()),
                    report.tracks
                );
            } else {
                println!("operation: {}", report.action);
                println!("tracks: {}", report.tracks);
                println!("cue: {}", report.cue_path.display());
            }
            ExitCode::SUCCESS
        }
        Ok(CommandReport::MetadataEdit(report)) => {
            if json {
                println!(
                    "{{\"operation\":\"metadata_edit\",\"format\":{},\"updated_fields\":{},\"output\":{}}}",
                    json_string(report.format),
                    report.updated_fields,
                    json_string(&report.output.display().to_string())
                );
            } else {
                println!("operation: metadata_edit");
                println!("format: {}", report.format.to_ascii_uppercase());
                println!("updated_fields: {}", report.updated_fields);
                println!("output: {}", report.output.display());
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            if json {
                eprintln!(
                    "{{\"error\":{{\"kind\":{},\"message\":{}}}}}",
                    json_string(error.kind),
                    json_string(&error.message)
                );
            } else {
                eprintln!("{}: {}", error.kind, error.message);
            }
            ExitCode::from(2)
        }
    }
}

fn command_args(args: &[std::ffi::OsString]) -> Result<CommandReport, CliError> {
    let Some(command) = args.first().and_then(|arg| arg.to_str()) else {
        return Err(usage_error());
    };
    match command {
        "inspect" => inspect_args(args).map(CommandReport::Inspect),
        "convert" => convert_args(args).map(|(report, path)| CommandReport::Convert(report, path)),
        "cue" => cue_cli::run(&args[1..])
            .map(CommandReport::Cue)
            .map_err(|message| CliError {
                kind: "invalid_data",
                message,
            }),
        "metadata" => metadata_args(&args[1..]).map(CommandReport::MetadataEdit),
        _ => Err(usage_error()),
    }
}

fn metadata_args(args: &[std::ffi::OsString]) -> Result<MetadataEditReport, CliError> {
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
    let AudioInput {
        decoder, format, ..
    } = open_decoder(&input)?;
    if format != "flac" && format != "wav" && format != "wavpack" {
        return Err(CliError {
            kind: "unsupported",
            message: "metadata editing supports WAVE, native FLAC, and WavPack files only"
                .to_owned(),
        });
    }
    let mut metadata = decoder.metadata().clone();
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
            "wav" => wav::rewrite_metadata(input_file, &mut output_file, &metadata).map(|_| ()),
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
        updated_fields: set_keys.len() + removes.iter().collect::<BTreeSet<_>>().len(),
        format,
    })
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

fn inspect_args(args: &[std::ffi::OsString]) -> Result<Report, CliError> {
    let mut input = None;
    for arg in &args[1..] {
        if arg == "--json" {
            continue;
        }
        if input.replace(PathBuf::from(arg)).is_some() {
            return Err(usage_error());
        }
    }
    inspect_audio(input.ok_or_else(usage_error)?)
}

fn convert_args(args: &[std::ffi::OsString]) -> Result<(Report, PathBuf), CliError> {
    let mut format = None;
    let mut container = None;
    let mut paths = Vec::new();
    let mut index = 1;
    while index < args.len() {
        if args[index] == "--json" {
            index += 1;
        } else if args[index] == "--format" {
            index += 1;
            let Some(value) = args.get(index).and_then(|arg| arg.to_str()) else {
                return Err(usage_error());
            };
            if format
                .replace(parse_output_format(value).ok_or_else(usage_error)?)
                .is_some()
            {
                return Err(usage_error());
            }
            index += 1;
        } else if args[index] == "--container" {
            index += 1;
            let Some(value) = args.get(index).and_then(|arg| arg.to_str()) else {
                return Err(usage_error());
            };
            if container
                .replace(parse_container(value).ok_or_else(usage_error)?)
                .is_some()
            {
                return Err(usage_error());
            }
            index += 1;
        } else {
            paths.push(PathBuf::from(&args[index]));
            index += 1;
        }
    }
    if paths.len() != 2 {
        return Err(usage_error());
    }
    let input = paths.remove(0);
    let output = paths.remove(0);
    let format = format.ok_or_else(usage_error)?;
    if matches!(format, OutputFormat::Flac | OutputFormat::WavPack) && container.is_some() {
        return Err(usage_error());
    }
    convert_wav(
        input,
        output.clone(),
        format,
        container.unwrap_or(WavContainer::Riff),
    )
    .map(|report| (report, output))
}

fn parse_container(value: &str) -> Option<WavContainer> {
    match value {
        "riff" => Some(WavContainer::Riff),
        "rf64" => Some(WavContainer::Rf64),
        _ => None,
    }
}

fn parse_output_format(value: &str) -> Option<OutputFormat> {
    match value {
        "pcm_u8" => Some(OutputFormat::Samples(SampleFormat::PcmU8)),
        "pcm_i16" => Some(OutputFormat::Samples(SampleFormat::PcmI16)),
        "pcm_i24" => Some(OutputFormat::Samples(SampleFormat::PcmI24)),
        "pcm_i32" => Some(OutputFormat::Samples(SampleFormat::PcmI32)),
        "float32" => Some(OutputFormat::Samples(SampleFormat::Float32)),
        "float64" => Some(OutputFormat::Samples(SampleFormat::Float64)),
        "g711_alaw" => Some(OutputFormat::G711(G711Encoding::ALaw)),
        "g711_mulaw" => Some(OutputFormat::G711(G711Encoding::MuLaw)),
        "flac" => Some(OutputFormat::Flac),
        "wavpack" => Some(OutputFormat::WavPack),
        _ => None,
    }
}

fn convert_wav(
    input: PathBuf,
    output: PathBuf,
    format: OutputFormat,
    container: WavContainer,
) -> Result<Report, CliError> {
    let AudioInput { mut decoder, .. } = open_decoder(&input)?;
    let input_spec = decoder.spec();
    let spec = if let Some(sample_format) = format.input_sample_format() {
        AudioSpec::new_with_speaker_mask(
            input_spec.sample_rate(),
            input_spec.channels(),
            sample_format,
            input_spec.speaker_mask(),
        )
        .map_err(|error| cli_audio_error(error.into(), &input))?
    } else {
        input_spec
    };
    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .map_err(|error| CliError {
            kind: "io",
            message: format!("{}: {error}", output.display()),
        })?;

    let result = (|| {
        let mut encoder: Box<dyn AudioEncoder> = match format {
            OutputFormat::Samples(_) => Box::new(
                WavWriter::new_with_metadata_and_container(
                    output_file,
                    spec,
                    decoder.metadata(),
                    container,
                )
                .map_err(|error| cli_audio_error(error, &output))?,
            ),
            OutputFormat::G711(encoding) => Box::new(
                WavWriter::new_g711_with_metadata_and_container(
                    output_file,
                    spec,
                    encoding,
                    decoder.metadata(),
                    container,
                )
                .map_err(|error| cli_audio_error(error, &output))?,
            ),
            OutputFormat::Flac => {
                let mut encoder = FlacEncoder::new(output_file, spec)
                    .map_err(|error| cli_audio_error(error, &output))?;
                encoder
                    .set_metadata(decoder.metadata())
                    .map_err(|error| cli_audio_error(error, &output))?;
                Box::new(encoder)
            }
            OutputFormat::WavPack => {
                let mut encoder = WavPackEncoder::new(output_file, spec)
                    .map_err(|error| cli_audio_error(error, &output))?;
                encoder
                    .set_metadata(decoder.metadata())
                    .map_err(|error| cli_audio_error(error, &output))?;
                Box::new(encoder)
            }
        };
        let mut frames = 0_u64;
        while let Some(block) = decoder
            .read_frames(8192)
            .map_err(|error| cli_audio_error(error, &input))?
        {
            let block_frames = block.frames() as u64;
            let converted = if let Some(sample_format) = format.input_sample_format() {
                convert_sample_format(&block, sample_format)
                    .map_err(|error| cli_audio_error(error.into(), &input))?
            } else {
                block
            };
            encoder
                .write_block(&converted)
                .map_err(|error| cli_audio_error(error, &output))?;
            frames = frames.checked_add(block_frames).ok_or_else(|| CliError {
                kind: "invalid_data",
                message: "converted frame count overflow".to_owned(),
            })?;
        }
        encoder
            .finish()
            .map_err(|error| cli_audio_error(error, &output))?;
        Ok(Report {
            format: match format {
                OutputFormat::Flac => "flac",
                OutputFormat::WavPack => "wavpack",
                _ => "wav",
            },
            spec,
            frames,
            metadata: decoder.metadata().clone(),
            output_encoding: matches!(format, OutputFormat::G711(_)).then(|| format.name()),
            container: (!matches!(format, OutputFormat::Flac | OutputFormat::WavPack))
                .then_some(container),
        })
    })();

    if result.is_err() {
        let _ = fs::remove_file(&output);
    }
    result
}

fn open_decoder(path: &Path) -> Result<AudioInput, CliError> {
    let file = File::open(path).map_err(|error| CliError {
        kind: "io",
        message: format!("{}: {error}", path.display()),
    })?;
    let mut file = file;
    let is_flac =
        has_signature(&mut file, b"fLaC").map_err(|error| cli_audio_error(error.into(), path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| cli_audio_error(error.into(), path))?;
    let is_ogg =
        has_signature(&mut file, b"OggS").map_err(|error| cli_audio_error(error.into(), path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| cli_audio_error(error.into(), path))?;
    let is_wavpack =
        has_signature(&mut file, b"wvpk").map_err(|error| cli_audio_error(error.into(), path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| cli_audio_error(error.into(), path))?;
    if is_flac {
        let decoder = FlacDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
        Ok(AudioInput {
            decoder: Box::new(decoder),
            format: "flac",
            container: None,
        })
    } else if is_ogg {
        let decoder = OggFlacDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
        Ok(AudioInput {
            decoder: Box::new(decoder),
            format: "ogg-flac",
            container: None,
        })
    } else if is_wavpack {
        let decoder =
            WavPackDecoder::new_seekable(file).map_err(|error| cli_audio_error(error, path))?;
        Ok(AudioInput {
            decoder: Box::new(decoder),
            format: "wavpack",
            container: None,
        })
    } else {
        let decoder = WavDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
        let container = decoder.container();
        Ok(AudioInput {
            decoder: Box::new(decoder),
            format: "wav",
            container: Some(container),
        })
    }
}

fn has_signature(reader: &mut impl Read, expected: &[u8; 4]) -> io::Result<bool> {
    let mut found = [0; 4];
    match reader.read_exact(&mut found) {
        Ok(()) => Ok(found == *expected),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

fn inspect_audio(path: PathBuf) -> Result<Report, CliError> {
    let AudioInput {
        mut decoder,
        format,
        container,
    } = open_decoder(&path)?;
    let spec = decoder.spec();
    let mut frames = 0_u64;
    while let Some(block) = decoder
        .read_frames(8192)
        .map_err(|error| cli_audio_error(error, &path))?
    {
        frames = frames
            .checked_add(block.frames() as u64)
            .ok_or_else(|| CliError {
                kind: "invalid_data",
                message: "decoded frame count overflow".to_owned(),
            })?;
    }
    Ok(Report {
        format,
        spec,
        frames,
        metadata: decoder.metadata().clone(),
        output_encoding: None,
        container,
    })
}

fn cli_audio_error(error: AudioError, path: &Path) -> CliError {
    let kind = match &error {
        AudioError::Io(_) => "io",
        AudioError::InvalidFormat(_) => "invalid_format",
        AudioError::InvalidData(_) => "invalid_data",
        AudioError::Unsupported(_) => "unsupported",
    };
    CliError {
        kind,
        message: format!("{}: {error}", path.display()),
    }
}

fn usage_error() -> CliError {
    CliError {
        kind: "usage",
        message: "usage: zemer inspect [--json] <input.wav|input.flac|input.ogg|input.oga|input.wv> | zemer convert --format <pcm_u8|pcm_i16|pcm_i24|pcm_i32|float32|float64|g711_alaw|g711_mulaw|flac|wavpack> [--container <riff|rf64>] [--json] <input.wav|input.flac|input.ogg|input.oga|input.wv> <output.wav|output.flac|output.wv> | zemer metadata edit [--json] <input.wav|input.flac|input.wv> <new-output.wav|new-output.flac|new-output.wv> [--set KEY=VALUE]... [--remove KEY]... | zemer cue split [--json] <input.cue> <new-output-dir> | zemer cue create [--json] <output.cue> <track.wav>...".to_owned(),
    }
}

fn print_report(report: &Report) {
    println!("format: {}", report.format.to_ascii_uppercase());
    if let Some(container) = report.container {
        println!("container: {}", container_name(container));
    }
    println!("sample_rate: {}", report.spec.sample_rate());
    println!("channels: {}", report.spec.channels());
    if let Some(mask) = report.spec.speaker_mask() {
        println!("speaker_mask: 0x{mask:08x}");
    }
    println!(
        "sample_format: {}",
        sample_format_name(report.spec.sample_format())
    );
    println!("frames: {}", report.frames);
    println!(
        "duration: {}/{} seconds",
        report.frames,
        report.spec.sample_rate()
    );
    for (key, values) in report.metadata.iter() {
        for value in values {
            println!("metadata.{key}: {value}");
        }
    }
}

fn report_json(report: &Report) -> String {
    let mut json = format!(
        "{{\"format\":{},\"container\":{},\"sample_rate\":{},\"channels\":{},\"speaker_mask\":{},\"sample_format\":{},\"frames\":{},\"duration_frames\":{{\"numerator\":{},\"denominator\":{}}},\"metadata\":{{",
        json_string(report.format),
        report.container.map_or_else(
            || "null".to_owned(),
            |container| json_string(container_name(container))
        ),
        report.spec.sample_rate(),
        report.spec.channels(),
        report
            .spec
            .speaker_mask()
            .map_or_else(|| "null".to_owned(), |mask| mask.to_string()),
        json_string(sample_format_name(report.spec.sample_format())),
        report.frames,
        report.frames,
        report.spec.sample_rate(),
    );
    for (index, (key, values)) in report.metadata.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        let _ = write!(json, "{}:[", json_string(key));
        for (value_index, value) in values.iter().enumerate() {
            if value_index > 0 {
                json.push(',');
            }
            json.push_str(&json_string(value));
        }
        json.push(']');
    }
    json.push_str("}}");
    json
}

fn conversion_json(report: &Report, output: &Path) -> String {
    let mut json = report_json(report);
    json.insert_str(
        1,
        &format!("\"output\":{},", json_string(&output.display().to_string())),
    );
    if let Some(encoding) = report.output_encoding {
        json.insert_str(
            1,
            &format!("\"output_encoding\":{},", json_string(encoding)),
        );
    }
    json
}

fn sample_format_name(format: SampleFormat) -> &'static str {
    match format {
        SampleFormat::PcmU8 => "pcm_u8",
        SampleFormat::PcmI16 => "pcm_i16",
        SampleFormat::PcmI24 => "pcm_i24",
        SampleFormat::PcmI32 => "pcm_i32",
        SampleFormat::Float32 => "float32",
        SampleFormat::Float64 => "float64",
    }
}

fn container_name(container: WavContainer) -> &'static str {
    match container {
        WavContainer::Riff => "riff",
        WavContainer::Rf64 => "rf64",
    }
}

fn json_string(value: &str) -> String {
    let mut json = String::with_capacity(value.len() + 2);
    json.push('"');
    for character in value.chars() {
        match character {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            '\u{0008}' => json.push_str("\\b"),
            '\u{000c}' => json.push_str("\\f"),
            control if control <= '\u{001f}' => {
                let _ = write!(json, "\\u{:04x}", control as u32);
            }
            other => json.push(other),
        }
    }
    json.push('"');
    json
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor, Read};

    use super::{has_signature, json_string};

    struct OneByteAtATime<R>(R);

    impl<R: Read> Read for OneByteAtATime<R> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let length = output.len().min(1);
            self.0.read(&mut output[..length])
        }
    }

    #[test]
    fn json_string_escapes_quotes_slashes_and_controls() {
        assert_eq!(
            json_string("a\"\\\n\t\u{0001}é"),
            "\"a\\\"\\\\\\n\\t\\u0001é\""
        );
    }

    #[test]
    fn reads_signature_across_short_reads() {
        let mut reader = OneByteAtATime(Cursor::new(b"fLaC audio"));
        assert!(has_signature(&mut reader, b"fLaC").unwrap());
    }

    #[test]
    fn treats_truncated_signature_as_non_match() {
        let mut reader = OneByteAtATime(Cursor::new(b"fLa"));
        assert!(!has_signature(&mut reader, b"fLaC").unwrap());
    }
}
