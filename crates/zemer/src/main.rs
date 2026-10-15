use std::{
    collections::BTreeSet,
    env,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use aiff::{
    AiffAudioRecording, AiffComment, AiffDecoder, AiffEncoder, AiffInstrument, AiffMarker, G726Rate,
};
use au::{AuDecoder, AuEncoder};
use dsdiff::{DsdiffDecoder, DsdiffEncoder};
use dsf::{DsfDecoder, DsfEncoder};
use flac::{FlacDecoder, FlacEncoder, OggFlacDecoder, OggFlacEncoder};
use g711::G711Encoding;
use ogg::OggPacketReader;
use vorbis::{VorbisDecoder, VorbisEncoder};
use wav::{WavContainer, WavDecoder, WavWriter};
use wavpack::{
    WavPackDecoder, WavPackDsdDecoder, WavPackDsdEncoder, WavPackEncoder, WavPackHybridEncoder,
};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioError, AudioSpec, DsdDecoder, DsdEncoder, DsdSpec, Metadata,
    SampleFormat, Samples, convert_sample_format, transcode_dsd_with_metadata,
};

#[path = "cue.rs"]
mod cue_cli;
#[path = "iso.rs"]
mod iso_cli;

struct Report {
    format: &'static str,
    spec: AudioSpec,
    frames: u64,
    metadata: Metadata,
    output_encoding: Option<&'static str>,
    output_valid_bits: Option<u16>,
    container: Option<WavContainer>,
    companion_output: Option<PathBuf>,
}

struct AudioInput {
    decoder: Box<dyn AudioDecoder>,
    format: &'static str,
    container: Option<WavContainer>,
    aiff_comments: Option<Vec<AiffComment>>,
    aiff_markers: Option<Vec<AiffMarker>>,
    aiff_instrument: Option<AiffInstrument>,
    aiff_audio_recording: Option<AiffAudioRecording>,
    aiff_id3_tag: Option<Vec<u8>>,
    au_annotation: Option<Vec<u8>>,
}

type DsdInput = (
    Box<dyn DsdDecoder>,
    &'static str,
    Option<Vec<u8>>,
    Option<Vec<dsdiff::DsdiffComment>>,
    Option<Vec<dsdiff::DsdiffMarker>>,
);

struct DsdReport {
    format: &'static str,
    spec: DsdSpec,
    octet_frames: u64,
    sample_count: u64,
    metadata: Metadata,
}

#[derive(Clone, Copy)]
enum OutputFormat {
    Samples(SampleFormat),
    G711(G711Encoding),
    WavImaAdpcm,
    Flac,
    OggFlac,
    Vorbis,
    WavPack,
    Aiff,
    Aifc,
    AifcSowt,
    Au,
    AuG721,
    AuG7233,
    AuG7235,
    AuG711(G711Encoding),
    AifcG711(G711Encoding),
    AifcIma4,
    AifcG726,
    AifcDwvw,
    Dsf,
    Dsdiff,
    DsdiffDst,
}

impl OutputFormat {
    fn input_sample_format(self, source: SampleFormat) -> Option<SampleFormat> {
        match self {
            Self::Samples(format) => Some(format),
            Self::G711(_) => Some(SampleFormat::PcmI16),
            Self::WavImaAdpcm => Some(SampleFormat::PcmI16),
            Self::AifcG711(_) => Some(SampleFormat::PcmI16),
            Self::AuG711(_) => Some(SampleFormat::PcmI16),
            Self::AuG721 => Some(SampleFormat::PcmI16),
            Self::AuG7233 | Self::AuG7235 => Some(SampleFormat::PcmI16),
            Self::AifcIma4 => Some(SampleFormat::PcmI16),
            Self::AifcG726 => Some(SampleFormat::PcmI16),
            Self::AifcDwvw => None,
            Self::Flac | Self::OggFlac | Self::Aiff
                if matches!(source, SampleFormat::Float32 | SampleFormat::Float64) =>
            {
                Some(SampleFormat::PcmI24)
            }
            Self::Flac => None,
            Self::OggFlac => None,
            Self::Vorbis => Some(SampleFormat::Float32),
            Self::WavPack => None,
            Self::Aiff => None,
            Self::Aifc => None,
            Self::AifcSowt if matches!(source, SampleFormat::Float32 | SampleFormat::Float64) => {
                Some(SampleFormat::PcmI24)
            }
            Self::AifcSowt => None,
            Self::Au => None,
            Self::Dsf | Self::Dsdiff | Self::DsdiffDst => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Samples(format) => sample_format_name(format),
            Self::G711(G711Encoding::ALaw) => "g711_alaw",
            Self::G711(G711Encoding::MuLaw) => "g711_mulaw",
            Self::WavImaAdpcm => "wav_ima_adpcm",
            Self::Flac => "flac",
            Self::OggFlac => "ogg_flac",
            Self::Vorbis => "vorbis",
            Self::WavPack => "wavpack",
            Self::Aiff => "aiff",
            Self::Aifc => "aifc",
            Self::AifcSowt => "aifc_sowt",
            Self::Au => "au",
            Self::AuG711(G711Encoding::ALaw) => "au_alaw",
            Self::AuG711(G711Encoding::MuLaw) => "au_mulaw",
            Self::AuG721 => "au_g721",
            Self::AuG7233 => "au_g723_3",
            Self::AuG7235 => "au_g723_5",
            Self::AifcG711(G711Encoding::ALaw) => "aifc_alaw",
            Self::AifcG711(G711Encoding::MuLaw) => "aifc_mulaw",
            Self::AifcIma4 => "aifc_ima4",
            Self::AifcG726 => "aifc_g726",
            Self::AifcDwvw => "aifc_dwvw",
            Self::Dsf => "dsf",
            Self::Dsdiff => "dsdiff",
            Self::DsdiffDst => "dsdiff_dst",
        }
    }
}

enum CommandReport {
    Inspect(Report),
    InspectDsd(DsdReport),
    Convert(Report, PathBuf),
    ConvertDsd(DsdReport, PathBuf),
    Cue(cue_cli::CueReport),
    Iso(iso_cli::IsoReport),
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
        Ok(CommandReport::InspectDsd(report)) => {
            if json {
                println!("{}", dsd_report_json(&report));
            } else {
                print_dsd_report(&report);
            }
            ExitCode::SUCCESS
        }
        Ok(CommandReport::Convert(report, output)) => {
            if json {
                println!("{}", conversion_json(&report, &output));
            } else {
                println!("converted_frames: {}", report.frames);
                println!("output: {}", output.display());
                if let Some(companion) = &report.companion_output {
                    println!("correction_output: {}", companion.display());
                }
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
                if let Some(valid_bits) = report.output_valid_bits {
                    println!("output_valid_bits: {valid_bits}");
                }
            }
            ExitCode::SUCCESS
        }
        Ok(CommandReport::ConvertDsd(report, output)) => {
            if json {
                println!("{}", dsd_conversion_json(&report, &output));
            } else {
                println!("converted_octet_frames: {}", report.octet_frames);
                println!("output: {}", output.display());
                println!("format: {}", report.format);
                println!("dsd_samples_per_channel: {}", report.sample_count);
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
        Ok(CommandReport::Iso(report)) => {
            if json {
                println!("{}", iso_report_json(&report));
            } else {
                println!("operation: iso_{}", report.action);
                println!("image: {}", report.image.display());
                println!("volume_id: {}", report.volume_id);
                println!("entry_count: {}", report.entries.len());
                for entry in &report.entries {
                    if entry.directory {
                        println!("directory: {}", entry.path);
                    } else {
                        println!("file: {} ({})", entry.path, entry.size);
                    }
                }
                if let Some(output) = &report.output {
                    println!("output: {}", output.display());
                }
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
        "inspect" => inspect_args(args),
        "convert" => convert_args(args),
        "cue" => cue_cli::run(&args[1..])
            .map(CommandReport::Cue)
            .map_err(|message| CliError {
                kind: "invalid_data",
                message,
            }),
        "iso" => iso_cli::run(&args[1..])
            .map(CommandReport::Iso)
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
    if let Some((decoder, format @ ("dsf" | "dsdiff"), _, _, _)) = open_dsd_decoder(&input)? {
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
    } = open_decoder(&input)?;
    if format != "flac"
        && format != "ogg-flac"
        && format != "ogg-vorbis"
        && format != "wav"
        && format != "wavpack"
        && format != "aiff"
        && format != "aifc"
        && format != "au"
    {
        return Err(CliError {
            kind: "unsupported",
            message: "metadata editing supports WAVE, AIFF/AIFC, AU, DSF, DSDIFF, native FLAC, Ogg-FLAC, Ogg Vorbis, and WavPack files"
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

fn inspect_args(args: &[std::ffi::OsString]) -> Result<CommandReport, CliError> {
    let mut input = None;
    for arg in &args[1..] {
        if arg == "--json" {
            continue;
        }
        if input.replace(PathBuf::from(arg)).is_some() {
            return Err(usage_error());
        }
    }
    let input = input.ok_or_else(usage_error)?;
    if let Some((decoder, format, _, _, _)) = open_dsd_decoder(&input)? {
        return inspect_dsd(decoder, format, &input).map(CommandReport::InspectDsd);
    }
    inspect_audio(input).map(CommandReport::Inspect)
}

fn convert_args(args: &[std::ffi::OsString]) -> Result<CommandReport, CliError> {
    let mut format = None;
    let mut container = None;
    let mut bitrate = None;
    let mut valid_bits = None;
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
        } else if args[index] == "--bitrate" {
            index += 1;
            let Some(value) = args
                .get(index)
                .and_then(|arg| arg.to_str())
                .and_then(|value| value.parse::<u32>().ok())
            else {
                return Err(usage_error());
            };
            if bitrate.replace(value).is_some() {
                return Err(usage_error());
            }
            index += 1;
        } else if args[index] == "--valid-bits" {
            index += 1;
            let Some(value) = args
                .get(index)
                .and_then(|arg| arg.to_str())
                .and_then(|value| value.parse::<u16>().ok())
                .filter(|value| (1..=32).contains(value))
            else {
                return Err(usage_error());
            };
            if valid_bits.replace(value).is_some() {
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
    let dsd_input = open_dsd_decoder(&input)?;
    if matches!(
        format,
        OutputFormat::Dsf | OutputFormat::Dsdiff | OutputFormat::DsdiffDst
    ) && dsd_input.is_none()
    {
        return Err(CliError {
            kind: "unsupported",
            message: "DSD output requires a DSF, DSDIFF, or native DSD WavPack input".to_owned(),
        });
    }
    if let Some((decoder, _, id3_tag, comments, markers)) = dsd_input {
        if !matches!(
            format,
            OutputFormat::Dsf
                | OutputFormat::Dsdiff
                | OutputFormat::DsdiffDst
                | OutputFormat::WavPack
        ) || container.is_some()
            || bitrate.is_some()
            || valid_bits.is_some()
        {
            return Err(CliError {
                kind: "unsupported",
                message:
                    "native DSD conversion supports DSF, DSDIFF, and lossless WavPack outputs only"
                        .to_owned(),
            });
        }
        return convert_dsd(input, output, format, decoder, id3_tag, comments, markers)
            .map(|(report, path)| CommandReport::ConvertDsd(report, path));
    }
    let valid_bitrate = match format {
        OutputFormat::WavPack | OutputFormat::Vorbis => {
            bitrate.is_some_and(|value| (24..=9600).contains(&value))
        }
        OutputFormat::AifcG726 => bitrate.is_some_and(|value| matches!(value, 16 | 24 | 32 | 40)),
        _ => false,
    };
    if (bitrate.is_some() && !valid_bitrate)
        || (matches!(format, OutputFormat::AifcG726) && bitrate.is_none())
        || (valid_bits.is_some()
            && !matches!(
                format,
                OutputFormat::Aiff
                    | OutputFormat::Aifc
                    | OutputFormat::AifcDwvw
                    | OutputFormat::AifcSowt
            ))
    {
        return Err(usage_error());
    }
    if matches!(
        format,
        OutputFormat::Flac
            | OutputFormat::OggFlac
            | OutputFormat::Vorbis
            | OutputFormat::WavPack
            | OutputFormat::Aiff
            | OutputFormat::Aifc
            | OutputFormat::Au
            | OutputFormat::AuG721
            | OutputFormat::AuG7233
            | OutputFormat::AuG7235
            | OutputFormat::AuG711(_)
            | OutputFormat::AifcG711(_)
            | OutputFormat::AifcIma4
            | OutputFormat::AifcG726
            | OutputFormat::AifcDwvw
    ) && container.is_some()
    {
        return Err(usage_error());
    }
    let report = convert_audio(
        input,
        output.clone(),
        format,
        container.unwrap_or(WavContainer::Riff),
        bitrate,
        valid_bits,
    )?;
    Ok(CommandReport::Convert(report, output))
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
        "wav_ima_adpcm" => Some(OutputFormat::WavImaAdpcm),
        "aifc_alaw" => Some(OutputFormat::AifcG711(G711Encoding::ALaw)),
        "aifc_mulaw" => Some(OutputFormat::AifcG711(G711Encoding::MuLaw)),
        "au_alaw" => Some(OutputFormat::AuG711(G711Encoding::ALaw)),
        "au_mulaw" => Some(OutputFormat::AuG711(G711Encoding::MuLaw)),
        "au_g721" => Some(OutputFormat::AuG721),
        "au_g723_3" => Some(OutputFormat::AuG7233),
        "au_g723_5" => Some(OutputFormat::AuG7235),
        "aifc_ima4" => Some(OutputFormat::AifcIma4),
        "aifc_g726" => Some(OutputFormat::AifcG726),
        "aifc_dwvw" => Some(OutputFormat::AifcDwvw),
        "flac" => Some(OutputFormat::Flac),
        "ogg_flac" => Some(OutputFormat::OggFlac),
        "vorbis" => Some(OutputFormat::Vorbis),
        "wavpack" => Some(OutputFormat::WavPack),
        "aiff" => Some(OutputFormat::Aiff),
        "aifc" => Some(OutputFormat::Aifc),
        "aifc_sowt" => Some(OutputFormat::AifcSowt),
        "au" => Some(OutputFormat::Au),
        "dsf" => Some(OutputFormat::Dsf),
        "dsdiff" => Some(OutputFormat::Dsdiff),
        "dsdiff_dst" => Some(OutputFormat::DsdiffDst),
        _ => None,
    }
}

fn convert_audio(
    input: PathBuf,
    output: PathBuf,
    format: OutputFormat,
    container: WavContainer,
    bitrate_kbps: Option<u32>,
    valid_bits: Option<u16>,
) -> Result<Report, CliError> {
    let AudioInput {
        mut decoder,
        aiff_comments,
        aiff_markers,
        aiff_instrument,
        aiff_audio_recording,
        aiff_id3_tag,
        au_annotation,
        ..
    } = open_decoder(&input)?;
    let input_spec = decoder.spec();
    let spec = if matches!(format, OutputFormat::Vorbis) {
        AudioSpec::new(
            input_spec.sample_rate(),
            input_spec.channels(),
            SampleFormat::Float32,
        )
        .map_err(|error| cli_audio_error(error.into(), &input))?
    } else if let Some(sample_format) = format.input_sample_format(input_spec.sample_format()) {
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
    let correction_output = matches!(format, OutputFormat::WavPack)
        .then(|| bitrate_kbps.map(|_| output.with_extension("wvc")))
        .flatten();
    if correction_output.as_deref() == Some(output.as_path()) {
        let _ = fs::remove_file(&output);
        return Err(usage_error());
    }
    let correction_file = match correction_output.as_deref() {
        Some(path) => match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => Some(file),
            Err(error) => {
                let _ = fs::remove_file(&output);
                return Err(CliError {
                    kind: "io",
                    message: format!("{}: {error}", path.display()),
                });
            }
        },
        None => None,
    };

    let result = (|| {
        let mut encoder: Box<dyn AudioEncoder> = match format {
            OutputFormat::Samples(_) => Box::new(
                WavWriter::new_with_metadata_and_container(
                    output_file,
                    spec,
                    &wave_metadata(decoder.metadata()),
                    container,
                )
                .map_err(|error| cli_audio_error(error, &output))?,
            ),
            OutputFormat::G711(encoding) => Box::new(
                WavWriter::new_g711_with_metadata_and_container(
                    output_file,
                    spec,
                    encoding,
                    &wave_metadata(decoder.metadata()),
                    container,
                )
                .map_err(|error| cli_audio_error(error, &output))?,
            ),
            OutputFormat::WavImaAdpcm => Box::new(
                WavWriter::new_ima_adpcm_with_metadata_and_container(
                    output_file,
                    spec,
                    &wave_metadata(decoder.metadata()),
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
            OutputFormat::OggFlac => {
                let mut encoder = OggFlacEncoder::new(output_file, spec)
                    .map_err(|error| cli_audio_error(error, &output))?;
                encoder
                    .set_metadata(decoder.metadata())
                    .map_err(|error| cli_audio_error(error, &output))?;
                Box::new(encoder)
            }
            OutputFormat::Vorbis => {
                let mut encoder = if let Some(bitrate_kbps) = bitrate_kbps {
                    let bitrate = bitrate_kbps.checked_mul(1000).ok_or_else(|| {
                        cli_audio_error(AudioError::InvalidData("Vorbis bitrate overflow"), &output)
                    })?;
                    VorbisEncoder::new_with_target_bitrate(output_file, spec, bitrate)
                } else {
                    VorbisEncoder::new(output_file, spec)
                }
                .map_err(|error| cli_audio_error(error, &output))?;
                encoder
                    .set_metadata(decoder.metadata())
                    .map_err(|error| cli_audio_error(error, &output))?;
                Box::new(encoder)
            }
            OutputFormat::WavPack => {
                if let Some(bitrate_kbps) = bitrate_kbps {
                    let correction_file = correction_file.ok_or_else(|| {
                        cli_audio_error(
                            AudioError::InvalidData("missing WavPack correction output"),
                            &output,
                        )
                    })?;
                    let mut encoder =
                        WavPackHybridEncoder::new(output_file, correction_file, spec, bitrate_kbps)
                            .map_err(|error| cli_audio_error(error, &output))?;
                    encoder
                        .set_metadata(decoder.metadata())
                        .map_err(|error| cli_audio_error(error, &output))?;
                    Box::new(encoder)
                } else {
                    let mut encoder = WavPackEncoder::new(output_file, spec)
                        .map_err(|error| cli_audio_error(error, &output))?;
                    encoder
                        .set_metadata(decoder.metadata())
                        .map_err(|error| cli_audio_error(error, &output))?;
                    Box::new(encoder)
                }
            }
            OutputFormat::Aiff => {
                let mut encoder = if let Some(valid_bits) = valid_bits {
                    AiffEncoder::new_with_bits_per_sample(output_file, spec, valid_bits)
                } else {
                    AiffEncoder::new(output_file, spec)
                }
                .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(tag) = &aiff_id3_tag {
                    encoder
                        .set_id3_tag(tag)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                encoder
                    .set_metadata(decoder.metadata())
                    .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(markers) = &aiff_markers {
                    encoder
                        .set_markers(markers)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(instrument) = &aiff_instrument {
                    encoder
                        .set_instrument(instrument)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(comments) = &aiff_comments {
                    encoder
                        .set_comments(comments)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::Aifc | OutputFormat::AifcSowt => {
                let mut encoder = if let Some(valid_bits) = valid_bits {
                    if matches!(format, OutputFormat::AifcSowt) {
                        AiffEncoder::new_aifc_sowt_with_bits_per_sample(
                            output_file,
                            spec,
                            valid_bits,
                        )
                    } else {
                        AiffEncoder::new_aifc_with_bits_per_sample(output_file, spec, valid_bits)
                    }
                } else if matches!(format, OutputFormat::AifcSowt) {
                    AiffEncoder::new_aifc_sowt(output_file, spec)
                } else {
                    AiffEncoder::new_aifc(output_file, spec)
                }
                .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(recording) = &aiff_audio_recording {
                    encoder
                        .set_audio_recording(recording)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(tag) = &aiff_id3_tag {
                    encoder
                        .set_id3_tag(tag)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                encoder
                    .set_metadata(decoder.metadata())
                    .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(markers) = &aiff_markers {
                    encoder
                        .set_markers(markers)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(instrument) = &aiff_instrument {
                    encoder
                        .set_instrument(instrument)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(comments) = &aiff_comments {
                    encoder
                        .set_comments(comments)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::Au => {
                let mut encoder = AuEncoder::new_with_metadata(
                    output_file,
                    spec,
                    &au_metadata(decoder.metadata()),
                )
                .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(annotation) = &au_annotation {
                    encoder
                        .set_annotation(annotation)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::AuG711(encoding) => {
                let mut encoder = AuEncoder::new_g711_with_metadata(
                    output_file,
                    spec,
                    encoding,
                    &au_metadata(decoder.metadata()),
                )
                .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(annotation) = &au_annotation {
                    encoder
                        .set_annotation(annotation)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::AuG721 => {
                let mut encoder = AuEncoder::new_g721_with_metadata(
                    output_file,
                    spec,
                    &au_metadata(decoder.metadata()),
                )
                .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(annotation) = &au_annotation {
                    encoder
                        .set_annotation(annotation)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::AuG7233 | OutputFormat::AuG7235 => {
                let metadata = au_metadata(decoder.metadata());
                let mut encoder = if matches!(format, OutputFormat::AuG7233) {
                    AuEncoder::new_g723_3_with_metadata(output_file, spec, &metadata)
                } else {
                    AuEncoder::new_g723_5_with_metadata(output_file, spec, &metadata)
                }
                .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(annotation) = &au_annotation {
                    encoder
                        .set_annotation(annotation)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::AifcG711(encoding) => {
                let mut encoder = AiffEncoder::new_aifc_g711_with_metadata(
                    output_file,
                    spec,
                    encoding,
                    decoder.metadata(),
                )
                .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(recording) = &aiff_audio_recording {
                    encoder
                        .set_audio_recording(recording)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(tag) = &aiff_id3_tag {
                    encoder
                        .set_id3_tag(tag)
                        .map_err(|error| cli_audio_error(error, &output))?;
                    encoder
                        .set_metadata(decoder.metadata())
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(markers) = &aiff_markers {
                    encoder
                        .set_markers(markers)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(instrument) = &aiff_instrument {
                    encoder
                        .set_instrument(instrument)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(comments) = &aiff_comments {
                    encoder
                        .set_comments(comments)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::AifcIma4 => {
                let mut encoder =
                    AiffEncoder::new_aifc_ima4_with_metadata(output_file, spec, decoder.metadata())
                        .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(recording) = &aiff_audio_recording {
                    encoder
                        .set_audio_recording(recording)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(tag) = &aiff_id3_tag {
                    encoder
                        .set_id3_tag(tag)
                        .map_err(|error| cli_audio_error(error, &output))?;
                    encoder
                        .set_metadata(decoder.metadata())
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(markers) = &aiff_markers {
                    encoder
                        .set_markers(markers)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(instrument) = &aiff_instrument {
                    encoder
                        .set_instrument(instrument)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(comments) = &aiff_comments {
                    encoder
                        .set_comments(comments)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::AifcG726 => {
                let rate = match bitrate_kbps {
                    Some(16) => G726Rate::Kbps16,
                    Some(24) => G726Rate::Kbps24,
                    Some(32) => G726Rate::Kbps32,
                    Some(40) => G726Rate::Kbps40,
                    _ => unreachable!("G.726 bitrate is validated by convert_args"),
                };
                let mut encoder = AiffEncoder::new_aifc_g726_with_metadata(
                    output_file,
                    spec,
                    rate,
                    decoder.metadata(),
                )
                .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(recording) = &aiff_audio_recording {
                    encoder
                        .set_audio_recording(recording)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(tag) = &aiff_id3_tag {
                    encoder
                        .set_id3_tag(tag)
                        .map_err(|error| cli_audio_error(error, &output))?;
                    encoder
                        .set_metadata(decoder.metadata())
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(markers) = &aiff_markers {
                    encoder
                        .set_markers(markers)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(instrument) = &aiff_instrument {
                    encoder
                        .set_instrument(instrument)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(comments) = &aiff_comments {
                    encoder
                        .set_comments(comments)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::AifcDwvw => {
                let mut encoder = AiffEncoder::new_aifc_dwvw_with_bits_per_sample_and_metadata(
                    output_file,
                    spec,
                    decoder.metadata(),
                    valid_bits,
                )
                .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(recording) = &aiff_audio_recording {
                    encoder
                        .set_audio_recording(recording)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(tag) = &aiff_id3_tag {
                    encoder
                        .set_id3_tag(tag)
                        .map_err(|error| cli_audio_error(error, &output))?;
                    encoder
                        .set_metadata(decoder.metadata())
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(markers) = &aiff_markers {
                    encoder
                        .set_markers(markers)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(instrument) = &aiff_instrument {
                    encoder
                        .set_instrument(instrument)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(comments) = &aiff_comments {
                    encoder
                        .set_comments(comments)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::Dsf | OutputFormat::Dsdiff | OutputFormat::DsdiffDst => {
                unreachable!("DSD output uses the DSD interface")
            }
        };
        let mut frames = 0_u64;
        while let Some(block) = decoder
            .read_frames(8192)
            .map_err(|error| cli_audio_error(error, &input))?
        {
            let block_frames = block.frames() as u64;
            let mut converted = if let Some(sample_format) =
                format.input_sample_format(input_spec.sample_format())
            {
                convert_sample_format(&block, sample_format)
                    .map_err(|error| cli_audio_error(error.into(), &input))?
            } else {
                block
            };
            if matches!(format, OutputFormat::Vorbis) {
                converted = vorbis_channel_order(&converted, spec)
                    .map_err(|error| cli_audio_error(error, &input))?;
            }
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
        if matches!(format, OutputFormat::AifcIma4) {
            frames = frames.div_ceil(64) * 64;
        }
        Ok(Report {
            format: match format {
                OutputFormat::Flac => "flac",
                OutputFormat::OggFlac => "ogg-flac",
                OutputFormat::Vorbis => "ogg-vorbis",
                OutputFormat::WavPack => "wavpack",
                OutputFormat::Aiff => "aiff",
                OutputFormat::Aifc => "aifc",
                OutputFormat::AifcSowt => "aifc",
                OutputFormat::Au
                | OutputFormat::AuG721
                | OutputFormat::AuG7233
                | OutputFormat::AuG7235
                | OutputFormat::AuG711(_) => "au",
                OutputFormat::AifcG711(_) => "aifc",
                OutputFormat::AifcIma4 => "aifc",
                OutputFormat::AifcG726 => "aifc",
                OutputFormat::AifcDwvw => "aifc",
                OutputFormat::Dsf | OutputFormat::Dsdiff | OutputFormat::DsdiffDst => {
                    unreachable!("DSD output uses the DSD interface")
                }
                _ => "wav",
            },
            spec,
            frames,
            metadata: decoder.metadata().clone(),
            output_encoding: matches!(
                format,
                OutputFormat::G711(_)
                    | OutputFormat::WavImaAdpcm
                    | OutputFormat::AuG711(_)
                    | OutputFormat::AuG721
                    | OutputFormat::AuG7233
                    | OutputFormat::AuG7235
                    | OutputFormat::AifcG711(_)
                    | OutputFormat::AifcIma4
                    | OutputFormat::AifcG726
                    | OutputFormat::AifcDwvw
                    | OutputFormat::AifcSowt
            )
            .then(|| format.name()),
            output_valid_bits: valid_bits,
            container: (!matches!(
                format,
                OutputFormat::Flac
                    | OutputFormat::OggFlac
                    | OutputFormat::Vorbis
                    | OutputFormat::WavPack
                    | OutputFormat::Aiff
                    | OutputFormat::Aifc
                    | OutputFormat::AifcG711(_)
                    | OutputFormat::AifcIma4
                    | OutputFormat::AifcG726
                    | OutputFormat::AifcDwvw
                    | OutputFormat::Au
                    | OutputFormat::AuG711(_)
                    | OutputFormat::AuG721
                    | OutputFormat::AuG7233
                    | OutputFormat::AuG7235
                    | OutputFormat::Dsf
            ))
            .then_some(container),
            companion_output: correction_output.clone(),
        })
    })();

    if result.is_err() {
        let _ = fs::remove_file(&output);
        if let Some(path) = &correction_output {
            let _ = fs::remove_file(path);
        }
    }
    result
}

fn vorbis_channel_order(
    audio: &zemer_core::PcmBuffer,
    output_spec: AudioSpec,
) -> Result<zemer_core::PcmBuffer, AudioError> {
    let channels = usize::from(audio.spec().channels());
    if output_spec.channels() != audio.spec().channels()
        || output_spec.sample_rate() != audio.spec().sample_rate()
        || output_spec.sample_format() != SampleFormat::Float32
    {
        return Err(AudioError::InvalidData(
            "Vorbis channel-order output specification does not match input",
        ));
    }
    let Samples::F32(samples) = audio.samples() else {
        return Err(AudioError::InvalidData(
            "Vorbis channel ordering expects Float32 PCM",
        ));
    };

    // WAVE extensible channels are stored in ascending speaker-mask bit order.
    // Vorbis I's channel order differs for several common multichannel layouts.
    let channel_order: &[usize] = match (audio.spec().speaker_mask(), channels) {
        (None, _) | (Some(0x0000_0004), 1) | (Some(0x0000_0003), 2) => {
            return zemer_core::PcmBuffer::new(output_spec, Samples::F32(samples.clone()))
                .map_err(Into::into);
        }
        (Some(0x0000_0007), 3) => &[0, 2, 1],
        (Some(0x0000_0033), 4) => &[0, 1, 2, 3],
        (Some(0x0000_0037), 5) => &[0, 2, 1, 3, 4],
        (Some(0x0000_003f), 6) => &[0, 2, 1, 4, 5, 3],
        (Some(0x0000_070f), 7) => &[0, 2, 1, 5, 6, 4, 3],
        (Some(0x0000_063f), 8) => &[0, 2, 1, 6, 7, 4, 5, 3],
        (Some(_), _) => {
            return Err(AudioError::Unsupported(
                "WAVE speaker mask has no standard Vorbis channel mapping",
            ));
        }
    };

    let mut reordered = Vec::new();
    reordered
        .try_reserve_exact(samples.len())
        .map_err(|_| AudioError::InvalidData("Vorbis channel allocation failed"))?;
    for frame in samples.chunks_exact(channels) {
        reordered.extend(channel_order.iter().map(|&source| frame[source]));
    }
    zemer_core::PcmBuffer::new(output_spec, Samples::F32(reordered)).map_err(Into::into)
}

fn wave_metadata(metadata: &Metadata) -> Metadata {
    let mut compatible = Metadata::default();
    for (key, values) in metadata.iter() {
        if wav::supports_info_metadata_key(key) {
            for value in values {
                compatible.insert(key, value.clone());
            }
        }
    }
    compatible
}

fn au_metadata(metadata: &Metadata) -> Metadata {
    let mut compatible = Metadata::default();
    for value in metadata.values("COMMENT") {
        compatible.insert("COMMENT", value.clone());
    }
    compatible
}

fn dsdiff_metadata(metadata: &Metadata) -> Metadata {
    let mut compatible = Metadata::default();
    for (key, values) in metadata.iter() {
        if matches!(key, "TITLE" | "ARTIST" | "COMMENT" | "DSDIFF_CHANNEL_ID") {
            for value in values {
                compatible.insert(key, value.clone());
            }
        }
    }
    compatible
}

fn convert_dsd(
    input: PathBuf,
    output: PathBuf,
    format: OutputFormat,
    mut decoder: Box<dyn DsdDecoder>,
    id3_tag: Option<Vec<u8>>,
    comments: Option<Vec<dsdiff::DsdiffComment>>,
    markers: Option<Vec<dsdiff::DsdiffMarker>>,
) -> Result<(DsdReport, PathBuf), CliError> {
    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .map_err(|error| CliError {
            kind: "io",
            message: format!("{}: {error}", output.display()),
        })?;
    let spec = decoder.dsd_spec();
    let declared_sample_count = decoder.sample_count();
    let metadata = decoder.metadata().clone();
    let output_metadata = if matches!(format, OutputFormat::Dsdiff | OutputFormat::DsdiffDst) {
        dsdiff_metadata(&metadata)
    } else {
        metadata.clone()
    };
    let result = (|| {
        let mut encoder: Box<dyn DsdEncoder> = match format {
            OutputFormat::Dsf => {
                let mut encoder = DsfEncoder::new(output_file, spec)
                    .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(tag) = &id3_tag {
                    encoder
                        .set_id3_tag(tag)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::Dsdiff | OutputFormat::DsdiffDst => {
                let mut encoder = if matches!(format, OutputFormat::DsdiffDst) {
                    DsdiffEncoder::new_dst(output_file, spec)
                } else {
                    DsdiffEncoder::new(output_file, spec)
                }
                .map_err(|error| cli_audio_error(error, &output))?;
                encoder
                    .set_metadata(&output_metadata)
                    .map_err(|error| cli_audio_error(error, &output))?;
                if let Some(comments) = &comments {
                    encoder
                        .set_comments(comments)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                if let Some(markers) = &markers {
                    encoder
                        .set_markers(markers)
                        .map_err(|error| cli_audio_error(error, &output))?;
                }
                Box::new(encoder)
            }
            OutputFormat::WavPack => {
                let encoder = WavPackDsdEncoder::new(output_file, spec)
                    .map_err(|error| cli_audio_error(error, &output))?;
                Box::new(encoder)
            }
            _ => unreachable!("DSD conversion format was checked"),
        };
        let octet_frames =
            transcode_dsd_with_metadata(&mut *decoder, &mut *encoder, &output_metadata, 8192)
                .map_err(|error| cli_audio_error(error, &input))?;
        let sample_count = sample_count_for_octets(octet_frames, declared_sample_count)
            .map_err(|error| cli_audio_error(error, &input))?;
        Ok(DsdReport {
            format: match format {
                OutputFormat::Dsf => "dsf",
                OutputFormat::Dsdiff => "dsdiff",
                OutputFormat::DsdiffDst => "dsdiff_dst",
                OutputFormat::WavPack => "wavpack-dsd",
                _ => unreachable!("DSD conversion format was checked"),
            },
            spec,
            octet_frames,
            sample_count,
            metadata: output_metadata,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&output);
    }
    result.map(|report| (report, output))
}

fn inspect_dsd(
    mut decoder: Box<dyn DsdDecoder>,
    format: &'static str,
    path: &Path,
) -> Result<DsdReport, CliError> {
    let spec = decoder.dsd_spec();
    let declared_sample_count = decoder.sample_count();
    let metadata = decoder.metadata().clone();
    let mut octet_frames = 0_u64;
    while let Some(block) = decoder
        .read_dsd_octets(8192)
        .map_err(|error| cli_audio_error(error, path))?
    {
        octet_frames = octet_frames
            .checked_add(block.frames() as u64)
            .ok_or_else(|| CliError {
                kind: "invalid_data",
                message: "decoded DSD octet frame count overflow".to_owned(),
            })?;
    }
    let sample_count = sample_count_for_octets(octet_frames, declared_sample_count)
        .map_err(|error| cli_audio_error(error, path))?;
    Ok(DsdReport {
        format,
        spec,
        octet_frames,
        sample_count,
        metadata,
    })
}

fn sample_count_for_octets(
    octet_frames: u64,
    declared_sample_count: Option<u64>,
) -> Result<u64, AudioError> {
    let Some(sample_count) = declared_sample_count else {
        return octet_frames
            .checked_mul(8)
            .ok_or(AudioError::InvalidData("DSD sample count overflow"));
    };
    let expected_octets = sample_count
        .checked_sub(1)
        .map(|count| count / 8 + 1)
        .ok_or(AudioError::InvalidData("DSD sample count must be non-zero"))?;
    if octet_frames != expected_octets {
        return Err(AudioError::InvalidData(
            "DSD sample count does not match decoded octets",
        ));
    }
    Ok(sample_count)
}

fn open_dsd_decoder(path: &Path) -> Result<Option<DsdInput>, CliError> {
    let mut file = File::open(path).map_err(|error| CliError {
        kind: "io",
        message: format!("{}: {error}", path.display()),
    })?;
    let is_dsf =
        has_signature(&mut file, b"DSD ").map_err(|error| cli_audio_error(error.into(), path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| cli_audio_error(error.into(), path))?;
    if is_dsf {
        let decoder = DsfDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
        let id3_tag = decoder.id3_tag().map(<[u8]>::to_vec);
        return Ok(Some((Box::new(decoder), "dsf", id3_tag, None, None)));
    }
    let is_dsdiff =
        has_signature(&mut file, b"FRM8").map_err(|error| cli_audio_error(error.into(), path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| cli_audio_error(error.into(), path))?;
    if is_dsdiff {
        let decoder = DsdiffDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
        let comments = decoder.comments().to_vec();
        let markers = decoder.markers().to_vec();
        return Ok(Some((
            Box::new(decoder),
            "dsdiff",
            None,
            Some(comments),
            Some(markers),
        )));
    }
    let is_wavpack =
        has_signature(&mut file, b"wvpk").map_err(|error| cli_audio_error(error.into(), path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| cli_audio_error(error.into(), path))?;
    if !is_wavpack {
        return Ok(None);
    }
    match WavPackDsdDecoder::new(file) {
        Ok(decoder) => Ok(Some((Box::new(decoder), "wavpack-dsd", None, None, None))),
        Err(AudioError::Unsupported("WavPack stream is not native DSD")) => Ok(None),
        Err(error) => Err(cli_audio_error(error, path)),
    }
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
    let is_au =
        has_signature(&mut file, b".snd").map_err(|error| cli_audio_error(error.into(), path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| cli_audio_error(error.into(), path))?;
    let is_form =
        has_signature(&mut file, b"FORM").map_err(|error| cli_audio_error(error.into(), path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| cli_audio_error(error.into(), path))?;
    if is_au {
        let decoder = AuDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
        let annotation = decoder.annotation().to_vec();
        Ok(AudioInput {
            decoder: Box::new(decoder),
            format: "au",
            container: None,
            aiff_comments: None,
            aiff_markers: None,
            aiff_instrument: None,
            aiff_audio_recording: None,
            aiff_id3_tag: None,
            au_annotation: Some(annotation),
        })
    } else if is_flac {
        let decoder = FlacDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
        Ok(AudioInput {
            decoder: Box::new(decoder),
            format: "flac",
            container: None,
            aiff_comments: None,
            aiff_markers: None,
            aiff_instrument: None,
            aiff_audio_recording: None,
            aiff_id3_tag: None,
            au_annotation: None,
        })
    } else if is_ogg {
        let is_vorbis = {
            let mut packets = OggPacketReader::new(&mut file);
            let packet = packets
                .next_packet()
                .map_err(|error| cli_audio_error(error.into(), path))?
                .ok_or_else(|| {
                    cli_audio_error(AudioError::InvalidFormat("empty Ogg stream"), path)
                })?;
            packet.bytes.starts_with(b"\x01vorbis")
        };
        file.seek(SeekFrom::Start(0))
            .map_err(|error| cli_audio_error(error.into(), path))?;
        if is_vorbis {
            let decoder = VorbisDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
            Ok(AudioInput {
                decoder: Box::new(decoder),
                format: "ogg-vorbis",
                container: None,
                aiff_comments: None,
                aiff_markers: None,
                aiff_instrument: None,
                aiff_audio_recording: None,
                aiff_id3_tag: None,
                au_annotation: None,
            })
        } else {
            let decoder =
                OggFlacDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
            Ok(AudioInput {
                decoder: Box::new(decoder),
                format: "ogg-flac",
                container: None,
                aiff_comments: None,
                aiff_markers: None,
                aiff_instrument: None,
                aiff_audio_recording: None,
                aiff_id3_tag: None,
                au_annotation: None,
            })
        }
    } else if is_wavpack {
        let decoder =
            WavPackDecoder::new_seekable(file).map_err(|error| cli_audio_error(error, path))?;
        Ok(AudioInput {
            decoder: Box::new(decoder),
            format: "wavpack",
            container: None,
            aiff_comments: None,
            aiff_markers: None,
            aiff_instrument: None,
            aiff_audio_recording: None,
            aiff_id3_tag: None,
            au_annotation: None,
        })
    } else if is_form {
        let decoder = AiffDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
        let format = if decoder.is_aifc() { "aifc" } else { "aiff" };
        let id3_tag = decoder.id3_tag().map(<[u8]>::to_vec);
        let instrument = decoder.instrument().cloned();
        let audio_recording = decoder.audio_recording().copied();
        let mut comments = Vec::new();
        comments
            .try_reserve_exact(decoder.comments().len())
            .map_err(|_| {
                cli_audio_error(AudioError::InvalidData("AIFF COMT is too large"), path)
            })?;
        comments.extend_from_slice(decoder.comments());
        let mut markers = Vec::new();
        markers
            .try_reserve_exact(decoder.markers().len())
            .map_err(|_| {
                cli_audio_error(AudioError::InvalidData("AIFF MARK is too large"), path)
            })?;
        markers.extend_from_slice(decoder.markers());
        Ok(AudioInput {
            decoder: Box::new(decoder),
            format,
            container: None,
            aiff_comments: (!comments.is_empty()).then_some(comments),
            aiff_markers: (!markers.is_empty()).then_some(markers),
            aiff_instrument: instrument,
            aiff_audio_recording: audio_recording,
            aiff_id3_tag: id3_tag,
            au_annotation: None,
        })
    } else {
        let decoder = WavDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
        let container = decoder.container();
        Ok(AudioInput {
            decoder: Box::new(decoder),
            format: "wav",
            container: Some(container),
            aiff_comments: None,
            aiff_markers: None,
            aiff_instrument: None,
            aiff_audio_recording: None,
            aiff_id3_tag: None,
            au_annotation: None,
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
        ..
    } = open_decoder(&path)?;
    let spec = decoder.spec();
    let mut frames = 0_u64;
    while let Some(block) = decoder
        .read_frames(8192)
        .map_err(|error| cli_audio_error(error, &path))?
    {
        if block.spec() != spec {
            return Err(cli_audio_error(
                AudioError::Unsupported(
                    "CLI inspection cannot summarize audio with changing specifications",
                ),
                &path,
            ));
        }
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
        output_valid_bits: None,
        container,
        companion_output: None,
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
        message: "usage: zemer inspect [--json] <input.wav|input.aiff|input.aifc|input.au|input.flac|input.ogg|input.oga|input.wv|input.dsf|input.dff> | zemer convert --format <pcm_u8|pcm_i16|pcm_i24|pcm_i32|float32|float64|g711_alaw|g711_mulaw|wav_ima_adpcm|aiff|aifc|aifc_sowt|au|au_alaw|au_mulaw|au_g721|au_g723_3|au_g723_5|aifc_alaw|aifc_mulaw|aifc_ima4|aifc_g726|aifc_dwvw|flac|ogg_flac|vorbis|wavpack|dsf|dsdiff|dsdiff_dst> [--container <riff|rf64>] [--valid-bits <1..32 for AIFF/AIFC PCM; 2..32 for AIFC DWVW>] [--bitrate <24..9600 kbps for Vorbis/WavPack; 16|24|32|40 for AIFC G.726>] [--json] <input.wav|input.aiff|input.aifc|input.au|input.flac|input.ogg|input.oga|input.wv|input.dsf|input.dff> <output.wav|output.aiff|output.aifc|output.flac|output.oga|output.wv|output.dsf|output.dff> | zemer metadata edit [--json] <input.wav|input.aiff|input.aifc|input.au|input.flac|input.oga|input.ogg|input.wv|input.dsf|input.dff> <new-output.wav|new-output.aiff|new-output.aifc|new-output.flac|new-output.oga|new-output.ogg|new-output.wv|new-output.dsf|new-output.dff> [--set KEY=VALUE]... [--remove KEY]... | zemer cue split [--json] <input.cue> <new-output-dir> | zemer cue create [--json] <output.cue> <track.wav>... | zemer iso inspect [--json] <image.iso> | zemer iso extract [--json] <image.iso> <new-output-dir>".to_owned(),
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

fn print_dsd_report(report: &DsdReport) {
    println!("format: {}", report.format);
    println!("dsd_bit_rate: {}", report.spec.bit_rate());
    println!("channels: {}", report.spec.channels());
    if let Some(mask) = report.spec.speaker_mask() {
        println!("speaker_mask: 0x{mask:08x}");
    }
    println!("octet_frames: {}", report.octet_frames);
    println!("dsd_samples_per_channel: {}", report.sample_count);
    println!(
        "duration: {}/{} seconds",
        report.sample_count,
        report.spec.bit_rate()
    );
    for (key, values) in report.metadata.iter() {
        for value in values {
            println!("metadata.{key}: {value}");
        }
    }
}

fn dsd_report_json(report: &DsdReport) -> String {
    let mut json = format!(
        "{{\"format\":{},\"dsd_bit_rate\":{},\"channels\":{},\"speaker_mask\":{},\"octet_frames\":{},\"dsd_samples_per_channel\":{},\"duration_samples\":{{\"numerator\":{},\"denominator\":{}}},\"metadata\":{{",
        json_string(report.format),
        report.spec.bit_rate(),
        report.spec.channels(),
        report
            .spec
            .speaker_mask()
            .map_or_else(|| "null".to_owned(), |mask| mask.to_string()),
        report.octet_frames,
        report.sample_count,
        report.sample_count,
        report.spec.bit_rate(),
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

fn dsd_conversion_json(report: &DsdReport, output: &Path) -> String {
    let mut json = dsd_report_json(report);
    json.insert_str(
        1,
        &format!("\"output\":{},", json_string(&output.display().to_string())),
    );
    json
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

fn iso_report_json(report: &iso_cli::IsoReport) -> String {
    let mut json = format!(
        "{{\"operation\":{},\"image\":{},\"volume_id\":{},\"entry_count\":{},\"entries\":[",
        json_string(&format!("iso_{}", report.action)),
        json_string(&report.image.display().to_string()),
        json_string(&report.volume_id),
        report.entries.len(),
    );
    for (index, entry) in report.entries.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        let _ = write!(
            json,
            "{{\"path\":{},\"type\":{},\"size\":{}}}",
            json_string(&entry.path),
            json_string(if entry.directory { "directory" } else { "file" }),
            entry.size,
        );
    }
    json.push(']');
    if let Some(output) = &report.output {
        let _ = write!(
            json,
            ",\"output\":{}",
            json_string(&output.display().to_string())
        );
    }
    json.push('}');
    json
}

fn conversion_json(report: &Report, output: &Path) -> String {
    let mut json = report_json(report);
    json.insert_str(
        1,
        &format!("\"output\":{},", json_string(&output.display().to_string())),
    );
    if let Some(companion) = &report.companion_output {
        json.insert_str(
            1,
            &format!(
                "\"correction_output\":{},",
                json_string(&companion.display().to_string())
            ),
        );
    }
    if let Some(encoding) = report.output_encoding {
        json.insert_str(
            1,
            &format!("\"output_encoding\":{},", json_string(encoding)),
        );
    }
    if let Some(valid_bits) = report.output_valid_bits {
        json.insert_str(1, &format!("\"output_valid_bits\":{valid_bits},"));
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

    use super::{
        AudioSpec, SampleFormat, Samples, has_signature, json_string, vorbis_channel_order,
    };

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

    #[test]
    fn maps_standard_wave_speaker_masks_to_vorbis_channel_order() {
        let cases: &[(u16, u32, &[f32])] = &[
            (1, 0x0004, &[0.0]),
            (2, 0x0003, &[0.0, 1.0]),
            (3, 0x0007, &[0.0, 2.0, 1.0]),
            (4, 0x0033, &[0.0, 1.0, 2.0, 3.0]),
            (5, 0x0037, &[0.0, 2.0, 1.0, 3.0, 4.0]),
            (6, 0x003f, &[0.0, 2.0, 1.0, 4.0, 5.0, 3.0]),
            (7, 0x070f, &[0.0, 2.0, 1.0, 5.0, 6.0, 4.0, 3.0]),
            (8, 0x063f, &[0.0, 2.0, 1.0, 6.0, 7.0, 4.0, 5.0, 3.0]),
        ];
        for &(channels, mask, expected) in cases {
            let input_spec = AudioSpec::new_with_speaker_mask(
                48_000,
                channels,
                SampleFormat::Float32,
                Some(mask),
            )
            .unwrap();
            let output_spec = AudioSpec::new(48_000, channels, SampleFormat::Float32).unwrap();
            let source = (0..channels).map(f32::from).collect();
            let audio = zemer_core::PcmBuffer::new(input_spec, Samples::F32(source)).unwrap();

            let reordered = vorbis_channel_order(&audio, output_spec).unwrap();
            assert_eq!(reordered.spec(), output_spec);
            assert_eq!(reordered.samples(), &Samples::F32(expected.to_vec()));
        }
    }

    #[test]
    fn rejects_unrecognized_wave_speaker_masks_for_vorbis() {
        let input_spec =
            AudioSpec::new_with_speaker_mask(48_000, 3, SampleFormat::Float32, Some(0x000b))
                .unwrap();
        let output_spec = AudioSpec::new(48_000, 3, SampleFormat::Float32).unwrap();
        let audio = zemer_core::PcmBuffer::new(input_spec, Samples::F32(vec![0.0; 3])).unwrap();

        assert!(matches!(
            vorbis_channel_order(&audio, output_spec),
            Err(zemer_core::AudioError::Unsupported(_))
        ));
    }
}
