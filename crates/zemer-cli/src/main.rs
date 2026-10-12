use std::{
    env,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::ExitCode,
};

use zemer_core::{
    AudioDecoder, AudioEncoder, AudioError, AudioSpec, Metadata, SampleFormat,
    convert_sample_format,
};
use wav::{G711Encoding, WavContainer, WavDecoder, WavWriter};

struct Report {
    spec: AudioSpec,
    frames: u64,
    metadata: Metadata,
    output_encoding: Option<&'static str>,
    container: WavContainer,
}

#[derive(Clone, Copy)]
enum OutputFormat {
    Samples(SampleFormat),
    G711(G711Encoding),
}

impl OutputFormat {
    fn input_sample_format(self) -> SampleFormat {
        match self {
            Self::Samples(format) => format,
            Self::G711(_) => SampleFormat::PcmI16,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Samples(format) => sample_format_name(format),
            Self::G711(G711Encoding::ALaw) => "g711_alaw",
            Self::G711(G711Encoding::MuLaw) => "g711_mulaw",
        }
    }
}

enum CommandReport {
    Inspect(Report),
    Convert(Report, PathBuf),
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
                println!("container: {}", container_name(report.container));
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
        _ => Err(usage_error()),
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
    inspect_wav(input.ok_or_else(usage_error)?)
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
        _ => None,
    }
}

fn convert_wav(
    input: PathBuf,
    output: PathBuf,
    format: OutputFormat,
    container: WavContainer,
) -> Result<Report, CliError> {
    let file = File::open(&input).map_err(|error| CliError {
        kind: "io",
        message: format!("{}: {error}", input.display()),
    })?;
    let mut decoder = WavDecoder::new(file).map_err(|error| cli_audio_error(error, &input))?;
    let spec = AudioSpec::new(
        decoder.spec().sample_rate(),
        decoder.spec().channels(),
        format.input_sample_format(),
    )
    .map_err(|error| cli_audio_error(error, &input))?;
    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .map_err(|error| CliError {
            kind: "io",
            message: format!("{}: {error}", output.display()),
        })?;

    let result = (|| {
        let mut encoder = match format {
            OutputFormat::Samples(_) => WavWriter::new_with_metadata_and_container(
                output_file,
                spec,
                decoder.metadata(),
                container,
            ),
            OutputFormat::G711(encoding) => WavWriter::new_g711_with_metadata_and_container(
                output_file,
                spec,
                encoding,
                decoder.metadata(),
                container,
            ),
        }
        .map_err(|error| cli_audio_error(error, &output))?;
        let mut frames = 0_u64;
        while let Some(block) = decoder
            .read_frames(8192)
            .map_err(|error| cli_audio_error(error, &input))?
        {
            let converted = convert_sample_format(&block, format.input_sample_format())
                .map_err(|error| cli_audio_error(error, &input))?;
            encoder
                .write_block(&converted)
                .map_err(|error| cli_audio_error(error, &output))?;
            frames = frames
                .checked_add(block.frames() as u64)
                .ok_or_else(|| CliError {
                    kind: "invalid_data",
                    message: "converted frame count overflow".to_owned(),
                })?;
        }
        encoder
            .finish()
            .map_err(|error| cli_audio_error(error, &output))?;
        Ok(Report {
            spec,
            frames,
            metadata: decoder.metadata().clone(),
            output_encoding: matches!(format, OutputFormat::G711(_)).then(|| format.name()),
            container,
        })
    })();

    if result.is_err() {
        let _ = fs::remove_file(&output);
    }
    result
}

fn inspect_wav(path: PathBuf) -> Result<Report, CliError> {
    let file = File::open(&path).map_err(|error| CliError {
        kind: "io",
        message: format!("{}: {error}", path.display()),
    })?;
    let mut decoder = WavDecoder::new(file).map_err(|error| cli_audio_error(error, &path))?;
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
        spec,
        frames,
        metadata: decoder.metadata().clone(),
        output_encoding: None,
        container: decoder.container(),
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
        message: "usage: zemer inspect [--json] <input.wav> | zemer convert --format <pcm_u8|pcm_i16|pcm_i24|pcm_i32|float32|float64|g711_alaw|g711_mulaw> [--container <riff|rf64>] [--json] <input.wav> <output.wav>".to_owned(),
    }
}

fn print_report(report: &Report) {
    println!("format: WAVE");
    println!("container: {}", container_name(report.container));
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
        "{{\"format\":\"wav\",\"container\":{},\"sample_rate\":{},\"channels\":{},\"speaker_mask\":{},\"sample_format\":{},\"frames\":{},\"duration_frames\":{{\"numerator\":{},\"denominator\":{}}},\"metadata\":{{",
        json_string(container_name(report.container)),
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
    use super::json_string;

    #[test]
    fn json_string_escapes_quotes_slashes_and_controls() {
        assert_eq!(
            json_string("a\"\\\n\t\u{0001}é"),
            "\"a\\\"\\\\\\n\\t\\u0001é\""
        );
    }
}
