use std::io::{Seek, SeekFrom, Write};

use g711::{G711Encoding, encode_alaw, encode_mulaw};
use g726::{G726Encoder, G726Packing};
use zemer_core::{AudioEncoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::G726Rate;
use crate::comment::{AiffComment, validate_comments, write_comments_chunk};
use crate::marker::{AiffMarker, validate_markers, write_markers_chunk};

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
    comments: Vec<AiffComment>,
    markers: Vec<AiffMarker>,
    g711: Option<G711Encoding>,
    ima4: bool,
    g726: Option<G726Encoder>,
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
    G711(G711Encoding),
    Ima4,
    G726(G726Rate),
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
        let ima4 = matches!(codec, AiffCodec::Ima4);
        let g726_rate = match codec {
            AiffCodec::G726(rate) => Some(rate),
            _ => None,
        };
        validate_metadata(metadata)?;
        if ima4 && (kind != AiffKind::Aifc || spec.sample_format() != SampleFormat::PcmI16) {
            return Err(AudioError::InvalidData(
                "AIFC IMA4 encoding requires signed 16-bit PCM input",
            ));
        }
        if g711.is_some() && spec.sample_format() != SampleFormat::PcmI16 {
            return Err(AudioError::InvalidData(
                "AIFC G.711 encoding requires signed 16-bit PCM input",
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
            SampleFormat::PcmU8 => (1..=8).contains(&bits_per_sample),
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
        writer.write_all(b"FORM\0\0\0\0")?;
        writer.write_all(match kind {
            AiffKind::Aiff => b"AIFF",
            AiffKind::Aifc => b"AIFC",
        })?;
        if kind == AiffKind::Aifc {
            writer.write_all(b"FVER\0\0\0\x04\xa2\x80\x51\x40")?;
        }
        let (bits, compression, compression_name) = if g726_rate.is_some() {
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
            comments: Vec::new(),
            markers: Vec::new(),
            g711,
            ima4,
            g726,
            ima4_buffer: Vec::new(),
            ima4_states: vec![None; usize::from(spec.channels())],
            poisoned: false,
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
        validate_combined_metadata(&self.metadata, comments, &self.markers)?;
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
        validate_combined_metadata(&self.metadata, &self.comments, markers)?;
        let mut copied = Vec::new();
        copied
            .try_reserve_exact(markers.len())
            .map_err(|_| AudioError::InvalidData("AIFF MARK is too large"))?;
        copied.extend_from_slice(markers);
        self.markers = copied;
        Ok(())
    }

    fn write_samples(&mut self, samples: &Samples) -> Result<(), AudioError> {
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
                    bytes.extend_from_slice(&((*value as u16 & mask).to_be_bytes()));
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
                    bytes.extend_from_slice(&((value as u32 & mask).to_be_bytes()[1..]));
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
                    bytes.extend_from_slice(&((*value as u32 & mask).to_be_bytes()));
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

    fn finish_inner(&mut self) -> Result<(), AudioError> {
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
        for (key, id) in [
            ("title", *b"NAME"),
            ("artist", *b"AUTH"),
            ("comment", *b"ANNO"),
        ] {
            for value in self.metadata.values(key) {
                if let Err(error) = write_text_chunk(&mut self.writer, &id, value) {
                    self.poisoned = true;
                    return Err(error);
                }
            }
        }
        write_comments_chunk(&mut self.writer, &self.comments)?;
        write_markers_chunk(&mut self.writer, &self.markers)?;
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
        validate_combined_metadata(metadata, &self.comments, &self.markers)?;
        self.metadata = metadata.clone();
        Ok(())
    }
}

pub(super) fn validate_metadata(metadata: &Metadata) -> Result<(), AudioError> {
    if metadata.values("TITLE").len() > 1 || metadata.values("ARTIST").len() > 1 {
        return Err(AudioError::Unsupported(
            "AIFF supports one TITLE and one ARTIST value",
        ));
    }
    if metadata.iter().any(|(key, _)| {
        !matches!(
            key.to_ascii_lowercase().as_str(),
            "title" | "artist" | "comment"
        )
    }) {
        return Err(AudioError::Unsupported(
            "AIFF metadata supports TITLE, ARTIST, and COMMENT",
        ));
    }
    if metadata.iter().any(|(_, values)| {
        values
            .iter()
            .any(|value| !value.is_ascii() || value.len() > 1_048_576)
    }) {
        return Err(AudioError::Unsupported(
            "AIFF metadata must be ASCII and at most 1 MiB per value",
        ));
    }
    let metadata_bytes = metadata
        .iter()
        .flat_map(|(_, values)| values)
        .try_fold(0_u64, |total, value| {
            total.checked_add(value.len() as u64 + 9)
        })
        .ok_or(AudioError::InvalidData("AIFF metadata size overflow"))?;
    if metadata_bytes > 16 * 1024 * 1024 {
        return Err(AudioError::Unsupported("AIFF metadata exceeds 16 MiB"));
    }
    Ok(())
}

fn validate_combined_metadata(
    metadata: &Metadata,
    comments: &[AiffComment],
    markers: &[AiffMarker],
) -> Result<(), AudioError> {
    let text_size = metadata
        .iter()
        .flat_map(|(_, values)| values)
        .try_fold(0_u64, |size, value| {
            size.checked_add(8 + value.len() as u64 + (value.len() & 1) as u64)
        })
        .ok_or(AudioError::InvalidData("AIFF metadata size overflow"))?;
    let comment_size = validate_comments(comments)?;
    let marker_size = validate_markers(markers)?;
    if text_size
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
        .is_none_or(|size| size > 16 * 1024 * 1024)
    {
        return Err(AudioError::Unsupported("AIFF metadata exceeds 16 MiB"));
    }
    Ok(())
}
