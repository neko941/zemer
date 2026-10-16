use std::io::{Seek, SeekFrom, Write};

use g711::{G711Encoding, encode_alaw, encode_mulaw};
use g726::{G726Encoder, G726Packing};
use id3::{encode_metadata, parse_id3_tag, rewrite_metadata_tag};
use zemer_core::{AudioEncoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::G726Rate;
use crate::audio_recording::{AiffAudioRecording, write_audio_recording_chunk};
use crate::comment::{AiffComment, validate_comments, write_comments_chunk};
use crate::dwvw_spool::DwvwSpool;
use crate::instrument::{
    AiffInstrument, validate_instrument, validate_instrument_markers, write_instrument_chunk,
};
use crate::marker::{AiffMarker, validate_markers, write_markers_chunk};
use crate::saxel::{AiffSaxel, validate_saxel_markers, validate_saxels, write_saxels_chunk};

pub struct AiffEncoder<W> {
    writer: W,
    spec: AudioSpec,
    kind: AiffKind,
    bits_per_sample: u16,
    form_size_offset: u64,
    frame_count_offset: u64,
    data_size_offset: u64,
    frames: u64,
    data_bytes: u64,
    metadata: Metadata,
    id3_tag: Option<Vec<u8>>,
    comments: Vec<AiffComment>,
    markers: Vec<AiffMarker>,
    saxels: Vec<AiffSaxel>,
    instrument: Option<AiffInstrument>,
    audio_recording: Option<AiffAudioRecording>,
    g711: Option<G711Encoding>,
    sowt: bool,
    ima4: bool,
    g726: Option<G726Encoder>,
    dwvw: Option<DwvwSpool>,
    ima4_buffer: Vec<i16>,
    ima4_states: Vec<Option<crate::ima4::State>>,
    poisoned: bool,
    finished: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AiffKind {
    Aiff,
    Aifc,
}

#[derive(Clone, Copy)]
enum AiffCodec {
    Pcm,
    Sowt,
    G711(G711Encoding),
    Ima4,
    G726(G726Rate),
    Dwvw,
}

impl<W: Write + Seek> AiffEncoder<W> {
    pub fn new(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_with_metadata(writer, spec, &Metadata::default())
    }

    pub fn new_with_metadata(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        Self::new_with_kind(writer, spec, metadata, AiffKind::Aiff)
    }

    pub fn new_aifc(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_with_kind(writer, spec, &Metadata::default(), AiffKind::Aifc)
    }

    pub fn new_aifc_with_metadata(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        Self::new_with_kind(writer, spec, metadata, AiffKind::Aifc)
    }

    /// Creates an AIFF integer PCM encoder with the requested valid bit depth.
    /// Samples use the high bits of their existing PCM container; discarded
    /// low bits are cleared during encoding.
    pub fn new_with_bits_per_sample(
        writer: W,
        spec: AudioSpec,
        bits_per_sample: u16,
    ) -> Result<Self, AudioError> {
        Self::new_configured(
            writer,
            spec,
            &Metadata::default(),
            AiffKind::Aiff,
            AiffCodec::Pcm,
            Some(bits_per_sample),
        )
    }

    /// Creates an AIFC integer PCM encoder with the requested valid bit depth.
    /// Samples use the high bits of their existing PCM container; discarded
    /// low bits are cleared during encoding.
    pub fn new_aifc_with_bits_per_sample(
        writer: W,
        spec: AudioSpec,
        bits_per_sample: u16,
    ) -> Result<Self, AudioError> {
        Self::new_configured(
            writer,
            spec,
            &Metadata::default(),
            AiffKind::Aifc,
            AiffCodec::Pcm,
            Some(bits_per_sample),
        )
    }

    /// Creates an AIFC encoder that writes integer PCM using little-endian `sowt` audio bytes.
    pub fn new_aifc_sowt(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_aifc_sowt_with_bits_per_sample_and_metadata(
            writer,
            spec,
            &Metadata::default(),
            None,
        )
    }

    pub fn new_aifc_sowt_with_metadata(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        Self::new_aifc_sowt_with_bits_per_sample_and_metadata(writer, spec, metadata, None)
    }

    /// Creates an AIFC `sowt` encoder with an explicit integer PCM valid-bit depth.
    pub fn new_aifc_sowt_with_bits_per_sample(
        writer: W,
        spec: AudioSpec,
        bits_per_sample: u16,
    ) -> Result<Self, AudioError> {
        Self::new_aifc_sowt_with_bits_per_sample_and_metadata(
            writer,
            spec,
            &Metadata::default(),
            Some(bits_per_sample),
        )
    }

    pub fn new_aifc_sowt_with_bits_per_sample_and_metadata(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
        bits_per_sample: Option<u16>,
    ) -> Result<Self, AudioError> {
        Self::new_configured(
            writer,
            spec,
            metadata,
            AiffKind::Aifc,
            AiffCodec::Sowt,
            bits_per_sample,
        )
    }

    pub fn new_aifc_g711(
        writer: W,
        spec: AudioSpec,
        encoding: G711Encoding,
    ) -> Result<Self, AudioError> {
        Self::new_aifc_g711_with_metadata(writer, spec, encoding, &Metadata::default())
    }

    pub fn new_aifc_g711_with_metadata(
        writer: W,
        spec: AudioSpec,
        encoding: G711Encoding,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        Self::new_configured(
            writer,
            spec,
            metadata,
            AiffKind::Aifc,
            AiffCodec::G711(encoding),
            None,
        )
    }

    pub fn new_with_kind(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
        kind: AiffKind,
    ) -> Result<Self, AudioError> {
        Self::new_configured(writer, spec, metadata, kind, AiffCodec::Pcm, None)
    }

    pub fn new_aifc_ima4(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_aifc_ima4_with_metadata(writer, spec, &Metadata::default())
    }

    pub fn new_aifc_ima4_with_metadata(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        Self::new_configured(
            writer,
            spec,
            metadata,
            AiffKind::Aifc,
            AiffCodec::Ima4,
            None,
        )
    }

    /// Creates an 8 kHz AIFC G.726 encoder with an explicit bitrate.
    pub fn new_aifc_g726(writer: W, spec: AudioSpec, rate: G726Rate) -> Result<Self, AudioError> {
        Self::new_aifc_g726_with_metadata(writer, spec, rate, &Metadata::default())
    }

    /// Creates an 8 kHz AIFC G.726 encoder with an explicit bitrate and metadata.
    pub fn new_aifc_g726_with_metadata(
        writer: W,
        spec: AudioSpec,
        rate: G726Rate,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        Self::new_configured(
            writer,
            spec,
            metadata,
            AiffKind::Aifc,
            AiffCodec::G726(rate),
            None,
        )
    }

    pub fn new_aifc_dwvw(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_aifc_dwvw_with_bits_per_sample_and_metadata(
            writer,
            spec,
            &Metadata::default(),
            None,
        )
    }

    pub fn new_aifc_dwvw_with_bits_per_sample(
        writer: W,
        spec: AudioSpec,
        bits_per_sample: u16,
    ) -> Result<Self, AudioError> {
        Self::new_aifc_dwvw_with_bits_per_sample_and_metadata(
            writer,
            spec,
            &Metadata::default(),
            Some(bits_per_sample),
        )
    }

    pub fn new_aifc_dwvw_with_bits_per_sample_and_metadata(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
        bits_per_sample: Option<u16>,
    ) -> Result<Self, AudioError> {
        Self::new_configured(
            writer,
            spec,
            metadata,
            AiffKind::Aifc,
            AiffCodec::Dwvw,
            bits_per_sample,
        )
    }

    fn new_configured(
        mut writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
        kind: AiffKind,
        codec: AiffCodec,
        requested_bits_per_sample: Option<u16>,
    ) -> Result<Self, AudioError> {
        let g711 = match codec {
            AiffCodec::G711(encoding) => Some(encoding),
            _ => None,
        };
        let sowt = matches!(codec, AiffCodec::Sowt);
        let ima4 = matches!(codec, AiffCodec::Ima4);
        let dwvw_enabled = matches!(codec, AiffCodec::Dwvw);
        let g726_rate = match codec {
            AiffCodec::G726(rate) => Some(rate),
            _ => None,
        };
        validate_metadata(metadata)?;
        if ima4
            && (kind != AiffKind::Aifc
                || !(1..=2).contains(&spec.channels())
                || spec.sample_format() != SampleFormat::PcmI16)
        {
            return Err(AudioError::InvalidData(
                "AIFC IMA4 encoding requires mono/stereo signed 16-bit PCM input",
            ));
        }
        if dwvw_enabled
            && (kind != AiffKind::Aifc
                || !matches!(
                    spec.sample_format(),
                    SampleFormat::PcmU8
                        | SampleFormat::PcmI16
                        | SampleFormat::PcmI24
                        | SampleFormat::PcmI32
                ))
        {
            return Err(AudioError::InvalidData(
                "AIFC DWVW encoding requires integer PCM input",
            ));
        }
        if g711.is_some() && spec.sample_format() != SampleFormat::PcmI16 {
            return Err(AudioError::InvalidData(
                "AIFC G.711 encoding requires signed 16-bit PCM input",
            ));
        }
        if sowt
            && (kind != AiffKind::Aifc
                || !matches!(
                    spec.sample_format(),
                    SampleFormat::PcmU8
                        | SampleFormat::PcmI16
                        | SampleFormat::PcmI24
                        | SampleFormat::PcmI32
                ))
        {
            return Err(AudioError::InvalidData(
                "AIFC sowt encoding requires integer PCM input",
            ));
        }
        if g726_rate.is_some()
            && (kind != AiffKind::Aifc
                || spec.sample_format() != SampleFormat::PcmI16
                || spec.sample_rate() != 8_000
                || g711.is_some()
                || ima4)
        {
            return Err(AudioError::InvalidData(
                "AIFC G.726 encoding requires 8 kHz signed 16-bit PCM input",
            ));
        }
        if spec.speaker_mask().is_some() {
            return Err(AudioError::Unsupported(
                "AIFF does not carry a WAVE speaker mask",
            ));
        }
        if !(matches!(
            spec.sample_format(),
            SampleFormat::PcmU8
                | SampleFormat::PcmI16
                | SampleFormat::PcmI24
                | SampleFormat::PcmI32
        ) || kind == AiffKind::Aifc
            && matches!(
                spec.sample_format(),
                SampleFormat::Float32 | SampleFormat::Float64
            ))
        {
            return Err(AudioError::Unsupported(
                "AIFF supports integer PCM; AIFC also supports IEEE float32/64",
            ));
        }
        let container_bits = match spec.sample_format() {
            SampleFormat::PcmU8 => 8,
            SampleFormat::PcmI16 => 16,
            SampleFormat::PcmI24 => 24,
            SampleFormat::PcmI32 => 32,
            SampleFormat::Float32 => 32,
            SampleFormat::Float64 => 64,
        };
        let bits_per_sample = requested_bits_per_sample.unwrap_or(container_bits);
        let g726 = g726_rate
            .map(|rate| {
                G726Encoder::new(rate, usize::from(spec.channels()), G726Packing::LsbFirst)
                    .map_err(|_| AudioError::InvalidData("invalid AIFC G.726 encoder settings"))
            })
            .transpose()?;
        let integer_pcm = matches!(
            spec.sample_format(),
            SampleFormat::PcmU8
                | SampleFormat::PcmI16
                | SampleFormat::PcmI24
                | SampleFormat::PcmI32
        );
        let valid_bit_depth = match spec.sample_format() {
            SampleFormat::PcmU8 => {
                (if dwvw_enabled { 2 } else { 1 }..=8).contains(&bits_per_sample)
            }
            SampleFormat::PcmI16 => (9..=16).contains(&bits_per_sample),
            SampleFormat::PcmI24 => (17..=24).contains(&bits_per_sample),
            SampleFormat::PcmI32 => (25..=32).contains(&bits_per_sample),
            SampleFormat::Float32 | SampleFormat::Float64 => bits_per_sample == container_bits,
        };
        if !valid_bit_depth
            || requested_bits_per_sample.is_some() && (!integer_pcm || g711.is_some() || ima4)
        {
            return Err(AudioError::InvalidData(
                "AIFF bit depth does not match the sample format",
            ));
        }
        let dwvw = dwvw_enabled
            .then(|| DwvwSpool::new(usize::from(spec.channels()), bits_per_sample as u8))
            .transpose()?;
        writer.write_all(b"FORM\0\0\0\0")?;
        writer.write_all(match kind {
            AiffKind::Aiff => b"AIFF",
            AiffKind::Aifc => b"AIFC",
        })?;
        if kind == AiffKind::Aifc {
            writer.write_all(b"FVER\0\0\0\x04\xa2\x80\x51\x40")?;
        }
        let (bits, compression, compression_name) = if dwvw.is_some() {
            (bits_per_sample, *b"DWVW", b"DWVW".as_slice())
        } else if sowt {
            (bits_per_sample, *b"sowt", b"".as_slice())
        } else if g726_rate.is_some() {
            (16_u16, *b"G726", b"G.726".as_slice())
        } else {
            match (g711, ima4, spec.sample_format()) {
                (_, true, _) => (16_u16, *b"ima4", b"IMA 4:1".as_slice()),
                (Some(G711Encoding::ALaw), false, _) => (16_u16, *b"alaw", b"ALaw 2:1".as_slice()),
                (Some(G711Encoding::MuLaw), false, _) => (16_u16, *b"ulaw", b"uLaw 2:1".as_slice()),
                (
                    None,
                    false,
                    SampleFormat::PcmU8
                    | SampleFormat::PcmI16
                    | SampleFormat::PcmI24
                    | SampleFormat::PcmI32,
                ) => (bits_per_sample, *b"NONE", b"not compressed".as_slice()),
                (None, false, SampleFormat::Float32) => {
                    (32, *b"fl32", b"32-bit floating point".as_slice())
                }
                (None, false, SampleFormat::Float64) => {
                    (64, *b"fl64", b"64-bit floating point".as_slice())
                }
            }
        };
        let comm_size = if kind == AiffKind::Aiff {
            18_u32
        } else {
            22 + ((compression_name.len() + 2) & !1) as u32
        };
        writer.write_all(b"COMM")?;
        writer.write_all(&comm_size.to_be_bytes())?;
        writer.write_all(&spec.channels().to_be_bytes())?;
        let frame_count_offset = writer.stream_position()?;
        writer.write_all(&0_u32.to_be_bytes())?;
        writer.write_all(&bits.to_be_bytes())?;
        writer.write_all(&extended_rate(spec.sample_rate())?)?;
        if kind == AiffKind::Aifc {
            writer.write_all(&compression)?;
            writer.write_all(&[compression_name.len() as u8])?;
            writer.write_all(compression_name)?;
            if (compression_name.len() + 1) & 1 != 0 {
                writer.write_all(&[0])?;
            }
        }
        if let Some(rate) = g726_rate {
            writer.write_all(b"APPL\0\0\0\x0cstocG726")?;
            let bitrate: u32 = match rate {
                G726Rate::Kbps16 => 16_000,
                G726Rate::Kbps24 => 24_000,
                G726Rate::Kbps32 => 32_000,
                G726Rate::Kbps40 => 40_000,
            };
            writer.write_all(&bitrate.to_be_bytes())?;
        }
        writer.write_all(b"SSND\0\0\0\0\0\0\0\0\0\0\0\0")?;
        let data_size_offset = writer.stream_position()? - 12;
        let mut encoder = Self {
            writer,
            spec,
            kind,
            bits_per_sample,
            form_size_offset: 4,
            frame_count_offset,
            data_size_offset,
            frames: 0,
            data_bytes: 8,
            metadata: Metadata::default(),
            id3_tag: None,
            comments: Vec::new(),
            markers: Vec::new(),
            saxels: Vec::new(),
            instrument: None,
            audio_recording: None,
            g711,
            sowt,
            ima4,
            g726,
            ima4_buffer: Vec::new(),
            ima4_states: vec![None; usize::from(spec.channels())],
            poisoned: false,
            dwvw,
            finished: false,
        };
        encoder.set_metadata(metadata)?;
        Ok(encoder)
    }

    pub fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        if self.finished || self.poisoned {
            return Err(AudioError::InvalidData("AIFF encoder is unusable"));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData("AIFF block specification changed"));
        }
        let frames = u64::try_from(block.frames())
            .map_err(|_| AudioError::InvalidData("AIFF frame count overflow"))?;
        let next_frames = self
            .frames
            .checked_add(frames)
            .ok_or(AudioError::InvalidData("AIFF frame count overflow"))?;
        if if self.ima4 {
            next_frames.div_ceil(64) > u64::from(u32::MAX)
        } else {
            next_frames > u64::from(u32::MAX)
        } {
            return Err(AudioError::Unsupported(
                "AIFF exceeds the 32-bit COMM frame-count limit",
            ));
        }
        let result = self.write_samples(block.samples());
        if result.is_err() {
            self.poisoned = true;
            return result;
        }
        self.frames = next_frames;
        Ok(())
    }

    pub fn set_comments(&mut self, comments: &[AiffComment]) -> Result<(), AudioError> {
        if self.frames != 0 || self.finished {
            return Err(AudioError::InvalidData(
                "AIFF comments must be set before audio samples",
            ));
        }
        validate_combined_metadata(
            &self.metadata,
            comments,
            &self.markers,
            &self.saxels,
            self.instrument.as_ref(),
            self.audio_recording.as_ref(),
        )?;
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(comments.len())
            .map_err(|_| AudioError::InvalidData("AIFF COMT is too large"))?;
        copied.extend_from_slice(comments);
        self.comments = copied;
        Ok(())
    }

    pub fn set_markers(&mut self, markers: &[AiffMarker]) -> Result<(), AudioError> {
        if self.frames != 0 || self.finished {
            return Err(AudioError::InvalidData(
                "AIFF markers must be set before audio samples",
            ));
        }
        validate_markers(markers)?;
        validate_saxel_markers(&self.saxels, markers)?;
        validate_combined_metadata(
            &self.metadata,
            &self.comments,
            markers,
            &self.saxels,
            self.instrument.as_ref(),
            self.audio_recording.as_ref(),
        )?;
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(markers.len())
            .map_err(|_| AudioError::InvalidData("AIFF MARK is too large"))?;
        copied.extend_from_slice(markers);
        self.markers = copied;
        Ok(())
    }

    pub fn set_saxels(&mut self, saxels: &[AiffSaxel]) -> Result<(), AudioError> {
        if self.kind != AiffKind::Aifc {
            return Err(AudioError::Unsupported(
                "AIFF SAXL sound-accelerator metadata requires AIFC",
            ));
        }
        if self.frames != 0 || self.finished {
            return Err(AudioError::InvalidData(
                "AIFF SAXL metadata must be set before audio samples",
            ));
        }
        validate_saxels(saxels)?;
        if !self.markers.is_empty() {
            validate_saxel_markers(saxels, &self.markers)?;
        }
        validate_combined_metadata(
            &self.metadata,
            &self.comments,
            &self.markers,
            saxels,
            self.instrument.as_ref(),
            self.audio_recording.as_ref(),
        )?;
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(saxels.len())
            .map_err(|_| AudioError::InvalidData("AIFF SAXL is too large"))?;
        copied.extend_from_slice(saxels);
        self.saxels = copied;
        Ok(())
    }

    pub fn set_instrument(&mut self, instrument: &AiffInstrument) -> Result<(), AudioError> {
        if self.frames != 0 || self.finished {
            return Err(AudioError::InvalidData(
                "AIFF instrument must be set before audio samples",
            ));
        }
        validate_instrument(instrument)?;
        validate_combined_metadata(
            &self.metadata,
            &self.comments,
            &self.markers,
            &self.saxels,
            Some(instrument),
            self.audio_recording.as_ref(),
        )?;
        self.instrument = Some(instrument.clone());
        Ok(())
    }

    pub fn set_audio_recording(
        &mut self,
        recording: &AiffAudioRecording,
    ) -> Result<(), AudioError> {
        if self.kind != AiffKind::Aifc {
            return Err(AudioError::Unsupported(
                "AIFF AESD audio-recording metadata requires AIFC",
            ));
        }
        if self.frames != 0 || self.finished {
            return Err(AudioError::InvalidData(
                "AIFF AESD metadata must be set before audio samples",
            ));
        }
        validate_combined_metadata(
            &self.metadata,
            &self.comments,
            &self.markers,
            &self.saxels,
            self.instrument.as_ref(),
            Some(recording),
        )?;
        self.audio_recording = Some(*recording);
        Ok(())
    }

    fn write_samples(&mut self, samples: &Samples) -> Result<(), AudioError> {
        if let Some(encoder) = &mut self.dwvw {
            return encoder.write_interleaved(&signed_dwvw_samples(samples, self.bits_per_sample)?);
        }
        if let Some(encoder) = &mut self.g726 {
            let Samples::I16(values) = samples else {
                return Err(AudioError::InvalidData(
                    "AIFC G.726 encoding requires signed 16-bit PCM input",
                ));
            };
            let bytes = encoder
                .encode(values)
                .map_err(|_| AudioError::InvalidData("AIFC G.726 block is too large"))?;
            return self.write_encoded_block(bytes);
        }
        if self.ima4 {
            let Samples::I16(values) = samples else {
                return Err(AudioError::InvalidData(
                    "AIFC IMA4 encoding requires signed 16-bit PCM input",
                ));
            };
            return self.write_ima4_samples(values);
        }
        let mut bytes = Vec::new();
        if let Some(encoding) = self.g711 {
            let Samples::I16(values) = samples else {
                return Err(AudioError::InvalidData(
                    "AIFC G.711 encoding requires signed 16-bit PCM input",
                ));
            };
            bytes
                .try_reserve_exact(values.len())
                .map_err(|_| AudioError::InvalidData("AIFC G.711 block is too large"))?;
            bytes.extend(values.iter().map(|sample| match encoding {
                G711Encoding::ALaw => encode_alaw(*sample),
                G711Encoding::MuLaw => encode_mulaw(*sample),
            }));
            return self.write_encoded_block(bytes);
        }
        match samples {
            Samples::U8(values) => {
                bytes
                    .try_reserve_exact(values.len())
                    .map_err(|_| AudioError::InvalidData("AIFF block is too large"))?;
                let unused_bits = 8 - self.bits_per_sample;
                let mask = !((1_u8 << unused_bits) - 1);
                bytes.extend(values.iter().map(|sample| (sample ^ 0x80) & mask));
            }
            Samples::I16(values) => {
                bytes
                    .try_reserve_exact(
                        values
                            .len()
                            .checked_mul(2)
                            .ok_or(AudioError::InvalidData("AIFF block size overflow"))?,
                    )
                    .map_err(|_| AudioError::InvalidData("AIFF block is too large"))?;
                let mask = !((1_u16 << (16 - self.bits_per_sample)) - 1);
                for value in values {
                    let sample = *value as u16 & mask;
                    let sample_bytes = if self.sowt {
                        sample.to_le_bytes()
                    } else {
                        sample.to_be_bytes()
                    };
                    bytes.extend_from_slice(&sample_bytes);
                }
            }
            Samples::I24(values) => {
                bytes
                    .try_reserve_exact(
                        values
                            .len()
                            .checked_mul(3)
                            .ok_or(AudioError::InvalidData("AIFF block size overflow"))?,
                    )
                    .map_err(|_| AudioError::InvalidData("AIFF block is too large"))?;
                let mask = !((1_u32 << (24 - self.bits_per_sample)) - 1);
                for &value in values {
                    if !(-8_388_608..=8_388_607).contains(&value) {
                        return Err(AudioError::InvalidData(
                            "AIFF 24-bit sample is out of range",
                        ));
                    }
                    let sample = value as u32 & mask;
                    let bytes24 = if self.sowt {
                        sample.to_le_bytes()
                    } else {
                        sample.to_be_bytes()
                    };
                    bytes.extend_from_slice(if self.sowt {
                        &bytes24[..3]
                    } else {
                        &bytes24[1..]
                    });
                }
            }
            Samples::I32(values) => {
                bytes
                    .try_reserve_exact(
                        values
                            .len()
                            .checked_mul(4)
                            .ok_or(AudioError::InvalidData("AIFF block size overflow"))?,
                    )
                    .map_err(|_| AudioError::InvalidData("AIFF block is too large"))?;
                let mask = !((1_u32 << (32 - self.bits_per_sample)) - 1);
                for value in values {
                    let sample = *value as u32 & mask;
                    let sample_bytes = if self.sowt {
                        sample.to_le_bytes()
                    } else {
                        sample.to_be_bytes()
                    };
                    bytes.extend_from_slice(&sample_bytes);
                }
            }
            Samples::F32(values) => {
                if self.kind != AiffKind::Aifc || self.spec.sample_format() != SampleFormat::Float32
                {
                    return Err(AudioError::InvalidData(
                        "AIFF cannot store floating-point samples",
                    ));
                }
                bytes
                    .try_reserve_exact(
                        values
                            .len()
                            .checked_mul(4)
                            .ok_or(AudioError::InvalidData("AIFF block size overflow"))?,
                    )
                    .map_err(|_| AudioError::InvalidData("AIFF block is too large"))?;
                for value in values {
                    bytes.extend_from_slice(&value.to_bits().to_be_bytes());
                }
            }
            Samples::F64(values) => {
                if self.kind != AiffKind::Aifc || self.spec.sample_format() != SampleFormat::Float64
                {
                    return Err(AudioError::InvalidData(
                        "AIFF cannot store floating-point samples",
                    ));
                }
                bytes
                    .try_reserve_exact(
                        values
                            .len()
                            .checked_mul(8)
                            .ok_or(AudioError::InvalidData("AIFF block size overflow"))?,
                    )
                    .map_err(|_| AudioError::InvalidData("AIFF block is too large"))?;
                for value in values {
                    bytes.extend_from_slice(&value.to_bits().to_be_bytes());
                }
            }
        }
        self.write_encoded_block(bytes)
    }

    fn write_ima4_samples(&mut self, samples: &[i16]) -> Result<(), AudioError> {
        let channels = usize::from(self.spec.channels());
        let packet_samples = channels
            .checked_mul(64)
            .ok_or(AudioError::InvalidData("AIFC IMA4 packet size overflow"))?;
        let mut offset = 0;
        if !self.ima4_buffer.is_empty() {
            let take = (packet_samples - self.ima4_buffer.len()).min(samples.len());
            self.ima4_buffer
                .try_reserve(take)
                .map_err(|_| AudioError::InvalidData("AIFC IMA4 block is too large"))?;
            self.ima4_buffer.extend_from_slice(&samples[..take]);
            offset += take;
            if self.ima4_buffer.len() == packet_samples {
                let packet_samples = std::mem::take(&mut self.ima4_buffer);
                self.write_ima4_packet(&packet_samples)?;
            }
        }
        while samples.len() - offset >= packet_samples {
            self.write_ima4_packet(&samples[offset..offset + packet_samples])?;
            offset += packet_samples;
        }
        let remainder = &samples[offset..];
        self.ima4_buffer
            .try_reserve(remainder.len())
            .map_err(|_| AudioError::InvalidData("AIFC IMA4 block is too large"))?;
        self.ima4_buffer.extend_from_slice(remainder);
        Ok(())
    }

    fn write_ima4_packet(&mut self, samples: &[i16]) -> Result<(), AudioError> {
        let channels = usize::from(self.spec.channels());
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(channels * 34)
            .map_err(|_| AudioError::InvalidData("AIFC IMA4 packet is too large"))?;
        for channel in 0..channels {
            let mut channel_samples = [0; 64];
            for (frame, sample) in channel_samples.iter_mut().enumerate() {
                *sample = samples[frame * channels + channel];
            }
            let (packet, state) =
                crate::ima4::encode_packet(&channel_samples, self.ima4_states[channel]);
            self.ima4_states[channel] = Some(state);
            bytes.extend_from_slice(&packet);
        }
        self.write_encoded_block(bytes)
    }

    fn write_encoded_block(&mut self, bytes: Vec<u8>) -> Result<(), AudioError> {
        let next_data_bytes =
            self.data_bytes
                .checked_add(u64::try_from(bytes.len()).map_err(|_| {
                    AudioError::InvalidData("AIFF block size exceeds the format limit")
                })?)
                .ok_or(AudioError::InvalidData("AIFF data size overflow"))?;
        if next_data_bytes > u64::from(u32::MAX) {
            return Err(AudioError::Unsupported(
                "AIFF exceeds the 32-bit SSND chunk size limit",
            ));
        }
        self.writer.write_all(&bytes)?;
        self.data_bytes = next_data_bytes;
        Ok(())
    }

    pub fn finish(&mut self) -> Result<&mut W, AudioError> {
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "AIFF encoder is unusable after a write error",
            ));
        }
        if self.finished {
            return Err(AudioError::InvalidData("AIFF encoder is already finished"));
        }
        match self.finish_inner() {
            Ok(()) => Ok(&mut self.writer),
            Err(error) => {
                self.poisoned = true;
                Err(error)
            }
        }
    }

    /// Copies an ID3 tag while retaining frames this crate does not interpret.
    pub fn set_id3_tag(&mut self, tag: &[u8]) -> Result<(), AudioError> {
        if self.frames != 0 || self.finished {
            return Err(AudioError::InvalidData(
                "AIFF ID3 tag must be set before audio samples",
            ));
        }
        if tag.len() > 16 * 1024 * 1024 {
            return Err(AudioError::InvalidData("AIFF ID3 tag exceeds 16 MiB"));
        }
        let metadata = parse_id3_tag(tag)?;
        validate_metadata(&metadata)?;
        validate_combined_metadata(
            &metadata,
            &self.comments,
            &self.markers,
            &self.saxels,
            self.instrument.as_ref(),
            self.audio_recording.as_ref(),
        )?;
        self.metadata = metadata;
        self.id3_tag = Some(tag.to_vec());
        Ok(())
    }

    fn finish_inner(&mut self) -> Result<(), AudioError> {
        if let Some(encoder) = self.dwvw.take() {
            let start = self.writer.stream_position()?;
            encoder.finish_to(&mut self.writer)?;
            let end = self.writer.stream_position()?;
            self.data_bytes = self
                .data_bytes
                .checked_add(end - start)
                .ok_or(AudioError::InvalidData("AIFF data size overflow"))?;
        }
        if let Some(encoder) = &mut self.g726 {
            let bytes = encoder.finish();
            self.write_encoded_block(bytes)?;
        }
        if self.ima4 && !self.ima4_buffer.is_empty() {
            let packet_samples = usize::from(self.spec.channels()) * 64;
            self.ima4_buffer.resize(packet_samples, 0);
            let packet_samples = std::mem::take(&mut self.ima4_buffer);
            self.write_ima4_packet(&packet_samples)?;
        }
        let comm_frames = if self.ima4 {
            self.frames.div_ceil(64)
        } else {
            self.frames
        };
        if comm_frames > u64::from(u32::MAX) || self.data_bytes > u64::from(u32::MAX) {
            return Err(AudioError::Unsupported(
                "AIFF exceeds the 32-bit FORM size limit",
            ));
        }
        if self.data_bytes & 1 != 0 {
            self.writer.write_all(&[0])?;
        }
        if self
            .markers
            .iter()
            .any(|marker| u64::from(marker.position) > self.frames)
        {
            return Err(AudioError::InvalidData(
                "AIFF marker position exceeds the sample-frame count",
            ));
        }
        validate_saxel_markers(&self.saxels, &self.markers)?;
        if let Some(instrument) = &self.instrument {
            validate_instrument_markers(instrument, &self.markers)?;
        }
        let id3_tag = match &self.id3_tag {
            Some(tag) => rewrite_metadata_tag(tag, &self.metadata)?,
            None => encode_metadata(&id3_metadata(&self.metadata))?,
        };
        add_metadata_bytes(
            validate_combined_metadata(
                &self.metadata,
                &self.comments,
                &self.markers,
                &self.saxels,
                self.instrument.as_ref(),
                self.audio_recording.as_ref(),
            )?,
            id3_tag.len() as u64,
        )?;
        for (key, id) in [
            ("title", *b"NAME"),
            ("artist", *b"AUTH"),
            ("copyright", *b"(c) "),
            ("comment", *b"ANNO"),
        ] {
            let values = self.metadata.values(key);
            if values.iter().all(|value| value.is_ascii()) {
                for value in values {
                    if let Err(error) = write_text_chunk(&mut self.writer, &id, value) {
                        self.poisoned = true;
                        return Err(error);
                    }
                }
            }
        }
        if !id3_tag.is_empty() {
            write_id3_chunk(&mut self.writer, &id3_tag)?;
        }
        write_comments_chunk(&mut self.writer, &self.comments)?;
        write_markers_chunk(&mut self.writer, &self.markers)?;
        write_saxels_chunk(&mut self.writer, &self.saxels)?;
        if let Some(instrument) = &self.instrument {
            write_instrument_chunk(&mut self.writer, instrument)?;
        }
        if let Some(recording) = &self.audio_recording {
            write_audio_recording_chunk(&mut self.writer, recording)?;
        }
        let end = self.writer.stream_position()?;
        let form_size = end
            .checked_sub(8)
            .ok_or(AudioError::InvalidData("AIFF output position is invalid"))?;
        let sound_size = self.data_bytes;
        if form_size > u64::from(u32::MAX) || sound_size > u64::from(u32::MAX) {
            self.poisoned = true;
            return Err(AudioError::Unsupported(
                "AIFF exceeds the 32-bit FORM size limit",
            ));
        }
        self.writer.seek(SeekFrom::Start(self.frame_count_offset))?;
        self.writer.write_all(&(comm_frames as u32).to_be_bytes())?;
        self.writer.seek(SeekFrom::Start(self.data_size_offset))?;
        self.writer.write_all(&(sound_size as u32).to_be_bytes())?;
        self.writer.seek(SeekFrom::Start(self.form_size_offset))?;
        self.writer.write_all(&(form_size as u32).to_be_bytes())?;
        self.writer.seek(SeekFrom::Start(end))?;
        self.finished = true;
        Ok(())
    }
}

pub(super) fn write_text_chunk(
    writer: &mut impl Write,
    id: &[u8; 4],
    value: &str,
) -> Result<(), AudioError> {
    if !value.is_ascii() || value.len() > 1_048_576 {
        return Err(AudioError::Unsupported(
            "AIFF metadata must be ASCII and at most 1 MiB",
        ));
    }
    writer.write_all(id)?;
    writer.write_all(&(value.len() as u32).to_be_bytes())?;
    writer.write_all(value.as_bytes())?;
    if value.len() & 1 != 0 {
        writer.write_all(&[0])?;
    }
    Ok(())
}

fn signed_dwvw_samples(samples: &Samples, bits: u16) -> Result<Vec<i32>, AudioError> {
    let values = match samples {
        Samples::U8(values) => values
            .iter()
            .map(|&value| ((i16::from(value) - 128) >> (8 - bits)) as i32)
            .collect(),
        Samples::I16(values) => values
            .iter()
            .map(|&value| i32::from(value >> (16 - bits)))
            .collect(),
        Samples::I24(values) => values.iter().map(|&value| value >> (24 - bits)).collect(),
        Samples::I32(values) => values.iter().map(|&value| value >> (32 - bits)).collect(),
        Samples::F32(_) | Samples::F64(_) => {
            return Err(AudioError::InvalidData(
                "AIFC DWVW encoding requires integer PCM input",
            ));
        }
    };
    Ok(values)
}

fn write_id3_chunk(writer: &mut impl Write, tag: &[u8]) -> Result<(), AudioError> {
    if tag.len() > 16 * 1024 * 1024 {
        return Err(AudioError::InvalidData("AIFF ID3 tag exceeds 16 MiB"));
    }
    let size = u32::try_from(tag.len())
        .map_err(|_| AudioError::InvalidData("AIFF ID3 tag exceeds its size limit"))?;
    writer.write_all(b"ID3 ")?;
    writer.write_all(&size.to_be_bytes())?;
    writer.write_all(tag)?;
    if size & 1 != 0 {
        writer.write_all(&[0])?;
    }
    Ok(())
}

fn extended_rate(rate: u32) -> Result<[u8; 10], AudioError> {
    if rate == 0 {
        return Err(AudioError::InvalidData("AIFF sample rate must be non-zero"));
    }
    let highest = 31 - rate.leading_zeros();
    let exponent = u16::try_from(16383 + highest)
        .map_err(|_| AudioError::InvalidData("AIFF sample rate is out of range"))?;
    let mantissa = u64::from(rate) << (63 - highest);
    let mut bytes = [0; 10];
    bytes[..2].copy_from_slice(&exponent.to_be_bytes());
    bytes[2..].copy_from_slice(&mantissa.to_be_bytes());
    Ok(bytes)
}

impl<W: Write + Seek> AudioEncoder for AiffEncoder<W> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        AiffEncoder::write_block(self, block)
    }
    fn finish(&mut self) -> Result<(), AudioError> {
        AiffEncoder::finish(self).map(|_| ())
    }
    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if self.frames != 0 || self.finished {
            return Err(AudioError::InvalidData(
                "AIFF metadata must be set before audio samples",
            ));
        }
        validate_metadata(metadata)?;
        validate_combined_metadata(
            metadata,
            &self.comments,
            &self.markers,
            &self.saxels,
            self.instrument.as_ref(),
            self.audio_recording.as_ref(),
        )?;
        self.metadata = metadata.clone();
        Ok(())
    }
}

pub(super) fn validate_metadata(metadata: &Metadata) -> Result<(), AudioError> {
    if metadata.values("TITLE").len() > 1
        || metadata.values("ARTIST").len() > 1
        || metadata.values("COPYRIGHT").len() > 1
    {
        return Err(AudioError::Unsupported(
            "AIFF supports one TITLE, ARTIST, and COPYRIGHT value",
        ));
    }
    if metadata.iter().any(|(key, values)| {
        key.is_empty()
            || key.len() > 1_048_576
            || key.as_bytes().contains(&0)
            || values.iter().any(|value| value.as_bytes().contains(&0))
            || values.iter().any(|value| value.len() > 1_048_576)
    }) {
        return Err(AudioError::Unsupported(
            "invalid or oversized AIFF metadata value",
        ));
    }
    let metadata_bytes = metadata
        .iter()
        .try_fold(0_u64, |total, (key, values)| {
            values.iter().try_fold(total, |total, value| {
                total.checked_add(key.len() as u64 + value.len() as u64 + 9)
            })
        })
        .ok_or(AudioError::InvalidData("AIFF metadata size overflow"))?;
    if metadata_bytes > 16 * 1024 * 1024 {
        return Err(AudioError::Unsupported("AIFF metadata exceeds 16 MiB"));
    }
    Ok(())
}

pub(super) fn id3_metadata(metadata: &Metadata) -> Metadata {
    let mut id3_metadata = metadata.clone();
    for key in ["title", "artist", "copyright", "comment"] {
        if metadata.values(key).iter().all(|value| value.is_ascii()) {
            id3_metadata.remove(key);
        }
    }
    id3_metadata
}

pub(super) fn add_metadata_bytes(current: u64, size: u64) -> Result<u64, AudioError> {
    current
        .checked_add(size)
        .filter(|&total| total <= 16 * 1024 * 1024)
        .ok_or(AudioError::InvalidData("AIFF metadata exceeds 16 MiB"))
}

fn validate_combined_metadata(
    metadata: &Metadata,
    comments: &[AiffComment],
    markers: &[AiffMarker],
    saxels: &[AiffSaxel],
    instrument: Option<&AiffInstrument>,
    audio_recording: Option<&AiffAudioRecording>,
) -> Result<u64, AudioError> {
    let text_size = ["title", "artist", "copyright", "comment"]
        .iter()
        .try_fold(0_u64, |size, key| {
            let values = metadata.values(key);
            if values.iter().all(|value| value.is_ascii()) {
                values.iter().try_fold(size, |size, value| {
                    size.checked_add(8 + value.len() as u64 + (value.len() & 1) as u64)
                })
            } else {
                Some(size)
            }
        })
        .ok_or(AudioError::InvalidData("AIFF metadata size overflow"))?;
    let comment_size = validate_comments(comments)?;
    let marker_size = validate_markers(markers)?;
    let saxel_size = validate_saxels(saxels)?;
    if let Some(instrument) = instrument {
        validate_instrument(instrument)?;
    }
    let total_size = text_size
        .checked_add(if comments.is_empty() {
            0
        } else {
            comment_size + 8
        })
        .and_then(|size| {
            size.checked_add(if markers.is_empty() {
                0
            } else {
                marker_size + 8
            })
        })
        .and_then(|size| size.checked_add(if instrument.is_some() { 28 } else { 0 }))
        .and_then(|size| size.checked_add(if audio_recording.is_some() { 32 } else { 0 }))
        .and_then(|size| size.checked_add(if saxels.is_empty() { 0 } else { saxel_size + 8 }))
        .ok_or(AudioError::InvalidData("AIFF metadata size overflow"))?;
    if total_size > 16 * 1024 * 1024 {
        return Err(AudioError::Unsupported("AIFF metadata exceeds 16 MiB"));
    }
    Ok(total_size)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zemer_core::{AudioDecoder, AudioSpec, PcmBuffer, SampleFormat, Samples};

    use crate::{AiffDecoder, AiffEncoder};

    use super::{add_metadata_bytes, extended_rate};

    const INDEPENDENT_DWVW_16: [u8; 20] = [
        0x01, 0x8f, 0x85, 0xe0, 0x08, 0x31, 0x8f, 0x68, 0x36, 0xc1, 0x5f, 0xe8, 0xdf, 0x36, 0x28,
        0xdb, 0x40, 0x4d, 0x13, 0xd7,
    ];

    #[test]
    fn aggregate_metadata_limit_includes_id3_and_native_chunks() {
        let limit = 16 * 1024 * 1024;
        assert_eq!(add_metadata_bytes(limit - 1, 1).unwrap(), limit);
        assert!(add_metadata_bytes(limit, 1).is_err());
        assert!(add_metadata_bytes(u64::MAX, 1).is_err());
    }

    #[test]
    fn aifc_dwvw_decodes_independent_16_bit_payload() {
        let file = aifc_dwvw_file(1, 9, &INDEPENDENT_DWVW_16);

        let mut decoder = AiffDecoder::new(Cursor::new(file)).unwrap();
        assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);
        assert_eq!(
            decoder.read_frames(9).unwrap().unwrap().samples(),
            &Samples::I16([287, 4255, 8549, 12341, 15845, 18660, 20931, 22349, 23079].to_vec())
        );
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn aifc_dwvw_decodes_independent_payloads_across_channel_counts() {
        let channel_payload = &INDEPENDENT_DWVW_16[..18];
        let expected_channel = [287, 4255, 8549, 12341, 15845, 18660, 20931, 22349, 23079];
        for channels in 1..=8 {
            let mut payload = Vec::with_capacity(channel_payload.len() * usize::from(channels));
            for _ in 0..channels {
                payload.extend_from_slice(channel_payload);
            }
            let file = aifc_dwvw_file(channels, 9, &payload);
            let mut decoder = AiffDecoder::new(Cursor::new(file)).unwrap();
            let expected: Vec<_> = expected_channel
                .into_iter()
                .flat_map(|sample| std::iter::repeat_n(sample, usize::from(channels)))
                .collect();
            assert_eq!(
                decoder.read_frames(9).unwrap().unwrap().samples(),
                &Samples::I16(expected),
                "failed with {channels} DWVW channels"
            );
            assert!(decoder.read_frames(1).unwrap().is_none());
        }
    }

    fn aifc_dwvw_file(channels: u16, frame_count: u32, payload: &[u8]) -> Vec<u8> {
        let mut file = b"FORM\0\0\0\0AIFC".to_vec();
        file.extend_from_slice(b"FVER\0\0\0\x04\xa2\x80\x51\x40");
        file.extend_from_slice(b"COMM\0\0\0\x1c");
        file.extend_from_slice(&channels.to_be_bytes());
        file.extend_from_slice(&frame_count.to_be_bytes());
        file.extend_from_slice(&16_u16.to_be_bytes());
        file.extend_from_slice(&extended_rate(44_100).unwrap());
        file.extend_from_slice(b"DWVW\x04DWVW");
        file.push(0);
        let sound_chunk_size = 8 + payload.len() as u32;
        file.extend_from_slice(b"SSND");
        file.extend_from_slice(&sound_chunk_size.to_be_bytes());
        file.extend_from_slice(&[0; 8]);
        file.extend_from_slice(payload);
        let form_size = (file.len() - 8) as u32;
        file[4..8].copy_from_slice(&form_size.to_be_bytes());
        file
    }

    #[test]
    fn aifc_dwvw_round_trips_interleaved_channels_across_blocks() {
        let spec = AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap();
        let samples = [
            -30_000, 28_000, -25_000, -28_000, -16_000, 19_000, -8_000, -19_000, -1_000, 9_000, 0,
            -9_000, 1_000, 3_000, 8_000, -3_000, 16_000, 15_000, 25_000, -15_000, 30_000, 27_000,
            1_234, -27_000, -2_345, 5_555, 6_789, -5_555, -9_876, 11_111, 22_222, -11_111, -11_111,
            2_222,
        ];
        let mut encoder = AiffEncoder::new_aifc_dwvw(Cursor::new(Vec::new()), spec).unwrap();
        for block in [&samples[..10], &samples[10..24], &samples[24..]] {
            encoder
                .write_block(&PcmBuffer::new(spec, Samples::I16(block.to_vec())).unwrap())
                .unwrap();
        }
        let file = encoder.finish().unwrap().get_ref().clone();
        assert!(file.windows(4).any(|window| window == b"DWVW"));

        let mut decoder = AiffDecoder::new(Cursor::new(file)).unwrap();
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(5).unwrap() {
            let Samples::I16(block) = block.samples() else {
                panic!("AIFC DWVW sample format changed during decode");
            };
            decoded.extend_from_slice(block);
        }
        assert_eq!(decoded, samples);
    }
}
