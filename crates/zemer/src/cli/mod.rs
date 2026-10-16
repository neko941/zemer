use std::{env, path::PathBuf, process::ExitCode};

use audio::{DsdReport, OutputFormat, RawPcmOptions};
use raw_pcm::RawPcmFormat;
use report::{
    container_name, conversion_json, dsd_conversion_json, dsd_report_json, iso_report_json,
    json_string, print_dsd_report, print_report, report_json, sample_format_name,
};
use wav::WavContainer;
use zemer_core::{AudioSpec, Metadata};

mod audio;
#[path = "../cue.rs"]
mod cue_cli;
#[path = "../iso.rs"]
mod iso_cli;
mod metadata;
mod report;

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

#[derive(Default)]
struct RawPcmArgs {
    format: Option<RawPcmFormat>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
}

impl RawPcmArgs {
    fn parse_at(
        &mut self,
        args: &[std::ffi::OsString],
        index: &mut usize,
    ) -> Result<bool, CliError> {
        let name = match args.get(*index).and_then(|arg| arg.to_str()) {
            Some("--input-format") => "format",
            Some("--sample-rate") => "sample_rate",
            Some("--channels") => "channels",
            _ => return Ok(false),
        };
        *index += 1;
        let value = args
            .get(*index)
            .and_then(|arg| arg.to_str())
            .ok_or_else(usage_error)?;
        match name {
            "format" => {
                let format = RawPcmFormat::parse(value).ok_or_else(usage_error)?;
                if self.format.replace(format).is_some() {
                    return Err(usage_error());
                }
            }
            "sample_rate" => {
                let rate = value
                    .parse::<u32>()
                    .ok()
                    .filter(|rate| *rate > 0)
                    .ok_or_else(usage_error)?;
                if self.sample_rate.replace(rate).is_some() {
                    return Err(usage_error());
                }
            }
            _ => {
                let channels = value
                    .parse::<u16>()
                    .ok()
                    .filter(|channels| *channels > 0)
                    .ok_or_else(usage_error)?;
                if self.channels.replace(channels).is_some() {
                    return Err(usage_error());
                }
            }
        }
        *index += 1;
        Ok(true)
    }

    fn finish(self) -> Result<Option<RawPcmOptions>, CliError> {
        match (self.format, self.sample_rate, self.channels) {
            (None, None, None) => Ok(None),
            (Some(format), Some(sample_rate), Some(channels)) => Ok(Some(RawPcmOptions {
                format,
                sample_rate,
                channels,
            })),
            _ => Err(usage_error()),
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

pub(super) fn run() -> ExitCode {
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
        "metadata" => metadata::metadata_args(&args[1..]).map(CommandReport::MetadataEdit),
        _ => Err(usage_error()),
    }
}

fn inspect_args(args: &[std::ffi::OsString]) -> Result<CommandReport, CliError> {
    let mut input = None;
    let mut raw = RawPcmArgs::default();
    let mut index = 1;
    while index < args.len() {
        if args[index] == "--json" {
            index += 1;
        } else if raw.parse_at(args, &mut index)? {
        } else {
            if input.replace(PathBuf::from(&args[index])).is_some() {
                return Err(usage_error());
            }
            index += 1;
        }
    }
    let input = input.ok_or_else(usage_error)?;
    let raw = raw.finish()?;
    if raw.is_none() {
        if let Some((decoder, format, _, _, _)) = audio::open_dsd_decoder(&input)? {
            return audio::inspect_dsd(decoder, format, &input).map(CommandReport::InspectDsd);
        }
    }
    audio::inspect_audio(input, raw).map(CommandReport::Inspect)
}

fn convert_args(args: &[std::ffi::OsString]) -> Result<CommandReport, CliError> {
    let mut format = None;
    let mut container = None;
    let mut bitrate = None;
    let mut valid_bits = None;
    let mut raw = RawPcmArgs::default();
    let mut paths = Vec::new();
    let mut index = 1;
    while index < args.len() {
        if args[index] == "--json" {
            index += 1;
        } else if raw.parse_at(args, &mut index)? {
            continue;
        } else if args[index] == "--format" {
            index += 1;
            let Some(value) = args.get(index).and_then(|arg| arg.to_str()) else {
                return Err(usage_error());
            };
            if format
                .replace(audio::parse_output_format(value).ok_or_else(usage_error)?)
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
                .replace(audio::parse_container(value).ok_or_else(usage_error)?)
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
    let raw = raw.finish()?;
    let dsd_input = if raw.is_none() {
        audio::open_dsd_decoder(&input)?
    } else {
        None
    };
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
        if raw.is_some() {
            return Err(usage_error());
        }
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
        return audio::convert_dsd(input, output, format, decoder, id3_tag, comments, markers)
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
            | OutputFormat::RawPcm(_)
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
            | OutputFormat::Caf
            | OutputFormat::CafG711(_)
            | OutputFormat::CafIma4
    ) && container.is_some()
    {
        return Err(usage_error());
    }
    let report = audio::convert_audio(
        input,
        output.clone(),
        format,
        container.unwrap_or(WavContainer::Riff),
        bitrate,
        valid_bits,
        raw,
    )?;
    Ok(CommandReport::Convert(report, output))
}

fn usage_error() -> CliError {
    CliError {
        kind: "usage",
        message: "usage: zemer inspect [--json] [--input-format <raw encoding> --sample-rate <Hz> --channels <count>] <input> | zemer convert --format <pcm_u8|pcm_i16|pcm_i24|pcm_i32|float32|float64|raw_u8|raw_s16le|raw_s16be|raw_s24le|raw_s24be|raw_s32le|raw_s32be|raw_f32le|raw_f32be|raw_f64le|raw_f64be|g711_alaw|g711_mulaw|wav_ima_adpcm|wav_ms_adpcm|aiff|aifc|aifc_sowt|au|au_alaw|au_mulaw|au_g721|au_g723_3|au_g723_5|aifc_alaw|aifc_mulaw|aifc_ima4|aifc_g726|aifc_dwvw|caf|caf_alaw|caf_mulaw|caf_ima4|flac|ogg_flac|vorbis|wavpack|dsf|dsdiff|dsdiff_dst> [--input-format <raw encoding> --sample-rate <Hz> --channels <count>] [--container <riff|rf64>] [--valid-bits <1..32 for AIFF/AIFC PCM; 2..32 for AIFC DWVW>] [--bitrate <24..9600 kbps for Vorbis/WavPack; 16|24|32|40 for AIFC G.726>] [--json] <input> <output> | zemer metadata edit ... | zemer cue split/create ... | zemer iso inspect/extract ...".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor, Read};

    use super::{AudioSpec, OutputFormat, audio, report};
    use zemer_core::{SampleFormat, Samples};

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
            report::json_string("a\"\\\n\t\u{0001}é"),
            "\"a\\\"\\\\\\n\\t\\u0001é\""
        );
    }

    #[test]
    fn reads_signature_across_short_reads() {
        let mut reader = OneByteAtATime(Cursor::new(b"fLaC audio"));
        assert!(audio::has_signature(&mut reader, b"fLaC").unwrap());
    }

    #[test]
    fn treats_truncated_signature_as_non_match() {
        let mut reader = OneByteAtATime(Cursor::new(b"fLa"));
        assert!(!audio::has_signature(&mut reader, b"fLaC").unwrap());
    }

    #[test]
    fn exposes_microsoft_adpcm_as_a_wav_output_format() {
        assert!(matches!(
            audio::parse_output_format("wav_ms_adpcm"),
            Some(OutputFormat::WavMsAdpcm)
        ));
        assert!(matches!(
            OutputFormat::WavMsAdpcm.input_sample_format(SampleFormat::PcmI24),
            Some(SampleFormat::PcmI16)
        ));
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

            let reordered = audio::vorbis_channel_order(&audio, output_spec).unwrap();
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
            audio::vorbis_channel_order(&audio, output_spec),
            Err(zemer_core::AudioError::Unsupported(_))
        ));
    }
}
