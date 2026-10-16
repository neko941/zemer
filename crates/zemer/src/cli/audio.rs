use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use aiff::{
    AiffAudioRecording, AiffComment, AiffDecoder, AiffEncoder, AiffInstrument, AiffMarker, G726Rate,
};
use au::{AuDecoder, AuEncoder};
use caf::{CafDecoder, CafEncoder};
use dsdiff::{DsdiffDecoder, DsdiffEncoder};
use dsf::{DsfDecoder, DsfEncoder};
use flac::{FlacDecoder, FlacEncoder, OggFlacDecoder, OggFlacEncoder};
use g711::G711Encoding;
use ogg::OggPacketReader;
use raw_pcm::{RawPcmDecoder, RawPcmEncoder, RawPcmFormat};
use resample::StreamingResampler;
use vorbis::{VorbisDecoder, VorbisEncoder};
use wav::{WavContainer, WavDecoder, WavWriter};
use wavpack::{
    WavPackDecoder, WavPackDsdDecoder, WavPackDsdEncoder, WavPackEncoder, WavPackHybridEncoder,
};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioError, AudioSpec, DsdDecoder, DsdEncoder, DsdSpec, Metadata,
    SampleFormat, Samples, convert_sample_format, transcode_dsd_with_metadata,
};

use super::{CliError, Report, report::sample_format_name, usage_error};

pub(super) struct AudioInput {
    pub(super) decoder: Box<dyn AudioDecoder>,
    pub(super) format: &'static str,
    pub(super) container: Option<WavContainer>,
    pub(super) aiff_comments: Option<Vec<AiffComment>>,
    pub(super) aiff_markers: Option<Vec<AiffMarker>>,
    pub(super) aiff_instrument: Option<AiffInstrument>,
    pub(super) aiff_audio_recording: Option<AiffAudioRecording>,
    pub(super) aiff_id3_tag: Option<Vec<u8>>,
    pub(super) au_annotation: Option<Vec<u8>>,
}

#[derive(Clone, Copy)]
pub(super) struct RawPcmOptions {
    pub(super) format: RawPcmFormat,
    pub(super) sample_rate: u32,
    pub(super) channels: u16,
}

pub(super) struct ConversionOptions {
    pub(super) container: WavContainer,
    pub(super) bitrate_kbps: Option<u32>,
    pub(super) valid_bits: Option<u16>,
    pub(super) raw: Option<RawPcmOptions>,
    pub(super) output_sample_rate: Option<u32>,
}

pub(super) type DsdInput = (
    Box<dyn DsdDecoder>,
    &'static str,
    Option<Vec<u8>>,
    Option<Vec<dsdiff::DsdiffComment>>,
    Option<Vec<dsdiff::DsdiffMarker>>,
);

pub(super) struct DsdReport {
    pub(super) format: &'static str,
    pub(super) spec: DsdSpec,
    pub(super) octet_frames: u64,
    pub(super) sample_count: u64,
    pub(super) metadata: Metadata,
}

#[derive(Clone, Copy)]
pub(super) enum OutputFormat {
    RawPcm(RawPcmFormat),
    Samples(SampleFormat),
    G711(G711Encoding),
    WavImaAdpcm,
    WavMsAdpcm,
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
    Caf,
    CafG711(G711Encoding),
    CafIma4,
    AifcG711(G711Encoding),
    AifcIma4,
    AifcG726,
    AifcDwvw,
    Dsf,
    Dsdiff,
    DsdiffDst,
}

impl OutputFormat {
    pub(super) fn input_sample_format(self, source: SampleFormat) -> Option<SampleFormat> {
        match self {
            Self::RawPcm(raw) => Some(raw.sample_format()),
            Self::Samples(format) => Some(format),
            Self::G711(_) => Some(SampleFormat::PcmI16),
            Self::WavImaAdpcm | Self::WavMsAdpcm => Some(SampleFormat::PcmI16),
            Self::AifcG711(_) => Some(SampleFormat::PcmI16),
            Self::CafG711(_) | Self::CafIma4 => Some(SampleFormat::PcmI16),
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
            Self::Caf => None,
            Self::Dsf | Self::Dsdiff | Self::DsdiffDst => None,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::RawPcm(format) => format.name(),
            Self::Samples(format) => sample_format_name(format),
            Self::G711(G711Encoding::ALaw) => "g711_alaw",
            Self::G711(G711Encoding::MuLaw) => "g711_mulaw",
            Self::WavImaAdpcm => "wav_ima_adpcm",
            Self::WavMsAdpcm => "wav_ms_adpcm",
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
            Self::Caf => "caf",
            Self::CafG711(G711Encoding::ALaw) => "caf_alaw",
            Self::CafG711(G711Encoding::MuLaw) => "caf_mulaw",
            Self::CafIma4 => "caf_ima4",
            Self::Dsf => "dsf",
            Self::Dsdiff => "dsdiff",
            Self::DsdiffDst => "dsdiff_dst",
        }
    }
}

pub(super) fn parse_container(value: &str) -> Option<WavContainer> {
    match value {
        "riff" => Some(WavContainer::Riff),
        "rf64" => Some(WavContainer::Rf64),
        _ => None,
    }
}

pub(super) fn parse_output_format(value: &str) -> Option<OutputFormat> {
    if let Some(raw) = RawPcmFormat::parse(value) {
        return Some(OutputFormat::RawPcm(raw));
    }
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
        "wav_ms_adpcm" => Some(OutputFormat::WavMsAdpcm),
        "aifc_alaw" => Some(OutputFormat::AifcG711(G711Encoding::ALaw)),
        "aifc_mulaw" => Some(OutputFormat::AifcG711(G711Encoding::MuLaw)),
        "caf" => Some(OutputFormat::Caf),
        "caf_alaw" => Some(OutputFormat::CafG711(G711Encoding::ALaw)),
        "caf_mulaw" => Some(OutputFormat::CafG711(G711Encoding::MuLaw)),
        "caf_ima4" => Some(OutputFormat::CafIma4),
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

pub(super) fn convert_audio(
    input: PathBuf,
    output: PathBuf,
    format: OutputFormat,
    options: ConversionOptions,
) -> Result<Report, CliError> {
    let ConversionOptions {
        container,
        bitrate_kbps,
        valid_bits,
        raw,
        output_sample_rate,
    } = options;
    let AudioInput {
        mut decoder,
        aiff_comments,
        aiff_markers,
        aiff_instrument,
        aiff_audio_recording,
        aiff_id3_tag,
        au_annotation,
        ..
    } = open_decoder(&input, raw)?;
    let input_spec = decoder.spec();
    let output_sample_rate = output_sample_rate.unwrap_or(input_spec.sample_rate());
    let spec = if let OutputFormat::RawPcm(raw_format) = format {
        AudioSpec::new(
            output_sample_rate,
            input_spec.channels(),
            raw_format.sample_format(),
        )
        .map_err(|error| cli_audio_error(error.into(), &input))?
    } else if matches!(format, OutputFormat::Vorbis) {
        AudioSpec::new(
            output_sample_rate,
            input_spec.channels(),
            SampleFormat::Float32,
        )
        .map_err(|error| cli_audio_error(error.into(), &input))?
    } else if let Some(sample_format) = format.input_sample_format(input_spec.sample_format()) {
        AudioSpec::new_with_speaker_mask(
            output_sample_rate,
            input_spec.channels(),
            sample_format,
            input_spec.speaker_mask(),
        )
        .map_err(|error| cli_audio_error(error.into(), &input))?
    } else {
        AudioSpec::new_with_speaker_mask(
            output_sample_rate,
            input_spec.channels(),
            input_spec.sample_format(),
            input_spec.speaker_mask(),
        )
        .map_err(|error| cli_audio_error(error.into(), &input))?
    };
    let mut resampler = (output_sample_rate != input_spec.sample_rate())
        .then(|| StreamingResampler::new(input_spec, output_sample_rate))
        .transpose()
        .map_err(|error| cli_audio_error(AudioError::InvalidData(error.0), &input))?;
    let resampling = resampler.is_some();
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
            OutputFormat::RawPcm(raw_format) => Box::new(
                RawPcmEncoder::new(output_file, spec, raw_format)
                    .map_err(|error| cli_audio_error(error, &output))?,
            ),
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
            OutputFormat::WavMsAdpcm => Box::new(
                WavWriter::new_ms_adpcm_with_metadata_and_container(
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
            OutputFormat::Caf => {
                let mut encoder = CafEncoder::new(output_file, spec)
                    .map_err(|error| cli_audio_error(error, &output))?;
                encoder
                    .set_metadata(decoder.metadata())
                    .map_err(|error| cli_audio_error(error, &output))?;
                Box::new(encoder)
            }
            OutputFormat::CafG711(encoding) => {
                let mut encoder = CafEncoder::new_g711(output_file, spec, encoding)
                    .map_err(|error| cli_audio_error(error, &output))?;
                encoder
                    .set_metadata(decoder.metadata())
                    .map_err(|error| cli_audio_error(error, &output))?;
                Box::new(encoder)
            }
            OutputFormat::CafIma4 => {
                let mut encoder = CafEncoder::new_ima4(output_file, spec)
                    .map_err(|error| cli_audio_error(error, &output))?;
                encoder
                    .set_metadata(decoder.metadata())
                    .map_err(|error| cli_audio_error(error, &output))?;
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
        let input_block_frames = if resampling {
            (8192_u64 * u64::from(input_spec.sample_rate()) / u64::from(output_sample_rate))
                .clamp(1, 8192) as usize
        } else {
            8192
        };
        {
            let mut write_block = |block: &zemer_core::PcmBuffer| -> Result<(), CliError> {
                if block.frames() == 0 {
                    return Ok(());
                }
                let mut converted = if resampling {
                    convert_sample_format(block, spec.sample_format())
                        .map_err(|error| cli_audio_error(error.into(), &input))?
                } else if let Some(sample_format) =
                    format.input_sample_format(input_spec.sample_format())
                {
                    convert_sample_format(block, sample_format)
                        .map_err(|error| cli_audio_error(error.into(), &input))?
                } else {
                    block.clone()
                };
                if matches!(format, OutputFormat::Vorbis) {
                    converted = vorbis_channel_order(&converted, spec)
                        .map_err(|error| cli_audio_error(error, &input))?;
                }
                frames = frames
                    .checked_add(converted.frames() as u64)
                    .ok_or_else(|| CliError {
                        kind: "invalid_data",
                        message: "converted frame count overflow".to_owned(),
                    })?;
                encoder
                    .write_block(&converted)
                    .map_err(|error| cli_audio_error(error, &output))
            };
            while let Some(block) = decoder
                .read_frames(input_block_frames)
                .map_err(|error| cli_audio_error(error, &input))?
            {
                if let Some(resampler) = &mut resampler {
                    let converted = resampler.push(&block).map_err(|error| {
                        cli_audio_error(AudioError::InvalidData(error.0), &input)
                    })?;
                    write_block(&converted)?;
                } else {
                    write_block(&block)?;
                }
            }
            if let Some(resampler) = &mut resampler {
                let tail = resampler
                    .finish()
                    .map_err(|error| cli_audio_error(AudioError::InvalidData(error.0), &input))?;
                write_block(&tail)?;
            }
        }
        encoder
            .finish()
            .map_err(|error| cli_audio_error(error, &output))?;
        if matches!(format, OutputFormat::AifcIma4) {
            frames = frames.div_ceil(64) * 64;
        }
        Ok(Report {
            format: match format {
                OutputFormat::RawPcm(_) => "raw-pcm",
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
                OutputFormat::Caf | OutputFormat::CafG711(_) | OutputFormat::CafIma4 => "caf",
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
                OutputFormat::RawPcm(_)
                    | OutputFormat::G711(_)
                    | OutputFormat::WavImaAdpcm
                    | OutputFormat::WavMsAdpcm
                    | OutputFormat::AuG711(_)
                    | OutputFormat::AuG721
                    | OutputFormat::AuG7233
                    | OutputFormat::AuG7235
                    | OutputFormat::AifcG711(_)
                    | OutputFormat::AifcIma4
                    | OutputFormat::AifcG726
                    | OutputFormat::AifcDwvw
                    | OutputFormat::AifcSowt
                    | OutputFormat::CafG711(_)
                    | OutputFormat::CafIma4
            )
            .then(|| format.name()),
            output_valid_bits: valid_bits,
            container: (!matches!(
                format,
                OutputFormat::RawPcm(_)
                    | OutputFormat::Flac
                    | OutputFormat::OggFlac
                    | OutputFormat::Vorbis
                    | OutputFormat::WavPack
                    | OutputFormat::Aiff
                    | OutputFormat::Aifc
                    | OutputFormat::AifcG711(_)
                    | OutputFormat::AifcIma4
                    | OutputFormat::AifcG726
                    | OutputFormat::AifcDwvw
                    | OutputFormat::Caf
                    | OutputFormat::CafG711(_)
                    | OutputFormat::CafIma4
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

pub(super) fn vorbis_channel_order(
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

pub(super) fn wave_metadata(metadata: &Metadata) -> Metadata {
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

pub(super) fn au_metadata(metadata: &Metadata) -> Metadata {
    let mut compatible = Metadata::default();
    for value in metadata.values("COMMENT") {
        compatible.insert("COMMENT", value.clone());
    }
    compatible
}

pub(super) fn dsdiff_metadata(metadata: &Metadata) -> Metadata {
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

pub(super) fn convert_dsd(
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

pub(super) fn inspect_dsd(
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

pub(super) fn sample_count_for_octets(
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

pub(super) fn open_dsd_decoder(path: &Path) -> Result<Option<DsdInput>, CliError> {
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
    match WavPackDsdDecoder::new_seekable(file) {
        Ok(decoder) => Ok(Some((Box::new(decoder), "wavpack-dsd", None, None, None))),
        Err(AudioError::Unsupported("WavPack stream is not native DSD")) => Ok(None),
        Err(error) => Err(cli_audio_error(error, path)),
    }
}

pub(super) fn open_decoder(
    path: &Path,
    raw: Option<RawPcmOptions>,
) -> Result<AudioInput, CliError> {
    let file = File::open(path).map_err(|error| CliError {
        kind: "io",
        message: format!("{}: {error}", path.display()),
    })?;
    let mut file = file;
    if let Some(raw) = raw {
        let decoder = RawPcmDecoder::new(file, raw.sample_rate, raw.channels, raw.format)
            .map_err(|error| cli_audio_error(error, path))?;
        return Ok(AudioInput {
            decoder: Box::new(decoder),
            format: "raw-pcm",
            container: None,
            aiff_comments: None,
            aiff_markers: None,
            aiff_instrument: None,
            aiff_audio_recording: None,
            aiff_id3_tag: None,
            au_annotation: None,
        });
    }
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
    let is_caf =
        has_signature(&mut file, b"caff").map_err(|error| cli_audio_error(error.into(), path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| cli_audio_error(error.into(), path))?;
    let is_form =
        has_signature(&mut file, b"FORM").map_err(|error| cli_audio_error(error.into(), path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| cli_audio_error(error.into(), path))?;
    if is_caf {
        let decoder = CafDecoder::new(file).map_err(|error| cli_audio_error(error, path))?;
        Ok(AudioInput {
            decoder: Box::new(decoder),
            format: "caf",
            container: None,
            aiff_comments: None,
            aiff_markers: None,
            aiff_instrument: None,
            aiff_audio_recording: None,
            aiff_id3_tag: None,
            au_annotation: None,
        })
    } else if is_au {
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

pub(super) fn has_signature(reader: &mut impl Read, expected: &[u8; 4]) -> io::Result<bool> {
    let mut found = [0; 4];
    match reader.read_exact(&mut found) {
        Ok(()) => Ok(found == *expected),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn inspect_audio(path: PathBuf, raw: Option<RawPcmOptions>) -> Result<Report, CliError> {
    let AudioInput {
        mut decoder,
        format,
        container,
        ..
    } = open_decoder(&path, raw)?;
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

pub(super) fn cli_audio_error(error: AudioError, path: &Path) -> CliError {
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
