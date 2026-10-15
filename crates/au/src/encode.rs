use std::io::{Seek, SeekFrom, Write};

use g711::{encode_alaw, encode_mulaw};
use g726::{G726Encoder, G726Packing, G726Rate};
use zemer_core::{AudioEncoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::{MAX_ANNOTATION_BYTES, UNKNOWN_DATA_SIZE, g726_packing_group};

pub use g711::G711Encoding;

#[derive(Clone, Copy)]
enum Encoding {
    Linear8,
    Linear16,
    Linear24,
    Linear32,
    Float32,
    Float64,
    MuLaw,
    ALaw,
    G726(G726Rate),
}

impl Encoding {
    fn code(self) -> u32 {
        match self {
            Self::MuLaw => 1,
            Self::Linear8 => 2,
            Self::Linear16 => 3,
            Self::Linear24 => 4,
            Self::Linear32 => 5,
            Self::Float32 => 6,
            Self::Float64 => 7,
            Self::ALaw => 27,
            Self::G726(G726Rate::Kbps32) => 23,
            Self::G726(G726Rate::Kbps24) => 25,
            Self::G726(G726Rate::Kbps40) => 26,
            Self::G726(G726Rate::Kbps16) => {
                unreachable!("AU does not assign the 2-bit G.726 profile")
            }
        }
    }

    fn from_spec(spec: AudioSpec) -> Result<Self, AudioError> {
        match spec.sample_format() {
            SampleFormat::PcmU8 => Ok(Self::Linear8),
            SampleFormat::PcmI16 => Ok(Self::Linear16),
            SampleFormat::PcmI24 => Ok(Self::Linear24),
            SampleFormat::PcmI32 => Ok(Self::Linear32),
            SampleFormat::Float32 => Ok(Self::Float32),
            SampleFormat::Float64 => Ok(Self::Float64),
        }
    }
}

pub struct AuEncoder<W> {
    writer: W,
    start: u64,
    spec: AudioSpec,
    encoding: Encoding,
    data_bytes: u64,
    g726_encoder: Option<G726Encoder>,
    g726_samples: u64,
    poisoned: bool,
    finished: bool,
}

impl<W: Write + Seek> AuEncoder<W> {
    pub fn new(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_with_metadata(writer, spec, &Metadata::default())
    }

    pub fn new_with_metadata(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        let encoding = Encoding::from_spec(spec)?;
        Self::new_configured(writer, spec, encoding, metadata)
    }

    pub fn new_g711(
        writer: W,
        spec: AudioSpec,
        encoding: G711Encoding,
    ) -> Result<Self, AudioError> {
        Self::new_g711_with_metadata(writer, spec, encoding, &Metadata::default())
    }

    pub fn new_g711_with_metadata(
        writer: W,
        spec: AudioSpec,
        encoding: G711Encoding,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        if spec.sample_format() != SampleFormat::PcmI16 {
            return Err(AudioError::InvalidData(
                "AU G.711 encoding requires signed 16-bit PCM input",
            ));
        }
        let encoding = match encoding {
            G711Encoding::ALaw => Encoding::ALaw,
            G711Encoding::MuLaw => Encoding::MuLaw,
        };
        Self::new_configured(writer, spec, encoding, metadata)
    }

    /// Creates an AU G.721 encoder. The profile is mono 8 kHz signed 16-bit PCM.
    pub fn new_g721(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_g721_with_metadata(writer, spec, &Metadata::default())
    }

    /// Creates an AU G.721 encoder with COMMENT metadata.
    pub fn new_g721_with_metadata(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        if spec.sample_rate() != 8_000
            || spec.channels() != 1
            || spec.sample_format() != SampleFormat::PcmI16
        {
            return Err(AudioError::Unsupported(
                "AU G.721 requires 8 kHz mono signed 16-bit PCM",
            ));
        }
        Self::new_configured(writer, spec, Encoding::G726(G726Rate::Kbps32), metadata)
    }

    pub fn new_g723_3(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_g723_3_with_metadata(writer, spec, &Metadata::default())
    }

    pub fn new_g723_3_with_metadata(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        Self::new_g723(writer, spec, G726Rate::Kbps24, metadata)
    }

    pub fn new_g723_5(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_g723_5_with_metadata(writer, spec, &Metadata::default())
    }

    pub fn new_g723_5_with_metadata(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        Self::new_g723(writer, spec, G726Rate::Kbps40, metadata)
    }

    fn new_g723(
        writer: W,
        spec: AudioSpec,
        rate: G726Rate,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        if spec.sample_rate() != 8_000
            || spec.channels() != 1
            || spec.sample_format() != SampleFormat::PcmI16
        {
            return Err(AudioError::Unsupported(
                "AU G.723 requires 8 kHz mono signed 16-bit PCM",
            ));
        }
        Self::new_configured(writer, spec, Encoding::G726(rate), metadata)
    }

    fn new_configured(
        mut writer: W,
        spec: AudioSpec,
        encoding: Encoding,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        let annotation = annotation_from_metadata(metadata)?;
        if annotation.len() > MAX_ANNOTATION_BYTES {
            return Err(AudioError::Unsupported("AU annotation exceeds 16 MiB"));
        }
        let start = writer.stream_position()?;
        let data_start = 24_u64
            .checked_add(annotation.len() as u64)
            .ok_or(AudioError::Unsupported("AU annotation size overflow"))?;
        let data_offset = u32::try_from(data_start).map_err(|_| {
            AudioError::Unsupported("AU annotation exceeds the 32-bit offset limit")
        })?;
        writer.write_all(b".snd")?;
        writer.write_all(&data_offset.to_be_bytes())?;
        writer.write_all(&0_u32.to_be_bytes())?;
        writer.write_all(&encoding.code().to_be_bytes())?;
        writer.write_all(&spec.sample_rate().to_be_bytes())?;
        writer.write_all(&u32::from(spec.channels()).to_be_bytes())?;
        writer.write_all(&annotation)?;
        let g726_encoder = if let Encoding::G726(rate) = encoding {
            Some(
                G726Encoder::new(rate, 1, G726Packing::LsbFirst)
                    .map_err(|_| AudioError::InvalidData("invalid AU G.72x configuration"))?,
            )
        } else {
            None
        };
        Ok(Self {
            writer,
            start,
            spec,
            encoding,
            data_bytes: 0,
            g726_encoder,
            g726_samples: 0,
            poisoned: false,
            finished: false,
        })
    }

    /// Replaces the unstructured annotation bytes before any audio is written.
    pub fn set_annotation(&mut self, annotation: &[u8]) -> Result<(), AudioError> {
        if self.finished || self.data_bytes != 0 || self.g726_samples != 0 {
            return Err(AudioError::InvalidData(
                "AU annotation must be set before writing audio",
            ));
        }
        if annotation.len() > MAX_ANNOTATION_BYTES {
            return Err(AudioError::Unsupported("AU annotation exceeds 16 MiB"));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(annotation.len().max(4))
            .map_err(|_| AudioError::InvalidData("AU annotation allocation failed"))?;
        bytes.extend_from_slice(annotation);
        bytes.resize(bytes.len().max(4), 0);
        let data_start = 24_u64
            .checked_add(bytes.len() as u64)
            .ok_or(AudioError::Unsupported("AU annotation size overflow"))?;
        let data_offset = u32::try_from(data_start).map_err(|_| {
            AudioError::Unsupported("AU annotation exceeds the 32-bit offset limit")
        })?;
        self.writer.seek(SeekFrom::Start(self.start))?;
        self.writer.write_all(b".snd")?;
        self.writer.write_all(&data_offset.to_be_bytes())?;
        self.writer.write_all(&0_u32.to_be_bytes())?;
        self.writer.write_all(&self.encoding.code().to_be_bytes())?;
        self.writer
            .write_all(&self.spec.sample_rate().to_be_bytes())?;
        self.writer
            .write_all(&u32::from(self.spec.channels()).to_be_bytes())?;
        self.writer.write_all(&bytes)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W, AudioError> {
        self.finalize()?;
        Ok(self.writer)
    }

    fn finalize(&mut self) -> Result<(), AudioError> {
        if self.finished {
            return Ok(());
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "AU writer failed while writing samples",
            ));
        }
        if let Encoding::G726(rate) = self.encoding {
            let (_, samples_per_group) = g726_packing_group(rate);
            if self.g726_samples % samples_per_group != 0 {
                return Err(AudioError::InvalidData(
                    "AU G.72x output requires complete packing groups",
                ));
            }
        }
        if let Some(encoder) = self.g726_encoder.as_mut() {
            let tail = encoder.finish();
            if !tail.is_empty() {
                if let Err(error) = self.writer.write_all(&tail) {
                    self.poisoned = true;
                    return Err(error.into());
                }
                self.data_bytes = self
                    .data_bytes
                    .checked_add(tail.len() as u64)
                    .ok_or(AudioError::Unsupported("AU audio data size overflow"))?;
            }
        }
        let end = self.writer.stream_position()?;
        self.writer.seek(SeekFrom::Start(self.start + 8))?;
        let size = u32::try_from(self.data_bytes).unwrap_or(UNKNOWN_DATA_SIZE);
        self.writer.write_all(&size.to_be_bytes())?;
        self.writer.seek(SeekFrom::Start(end))?;
        self.finished = true;
        Ok(())
    }
}

impl<W: Write + Seek> AudioEncoder for AuEncoder<W> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData("AU writer is already finalized"));
        }
        let annotation = annotation_from_metadata(metadata)?;
        self.set_annotation(&annotation)
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        self.finalize()
    }

    fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData("AU writer is already finalized"));
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "AU writer is unusable after an I/O failure",
            ));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "PCM block does not match AU writer format",
            ));
        }
        if matches!(self.encoding, Encoding::G726(_)) {
            let Samples::I16(samples) = block.samples() else {
                return Err(AudioError::InvalidData(
                    "AU G.72x requires signed 16-bit PCM input",
                ));
            };
            let encoded = self
                .g726_encoder
                .as_mut()
                .ok_or(AudioError::InvalidData("AU G.72x encoder is unavailable"))?
                .encode(samples)
                .map_err(|_| AudioError::InvalidData("AU G.72x encoding failed"))?;
            let next_size = self
                .data_bytes
                .checked_add(encoded.len() as u64)
                .ok_or(AudioError::Unsupported("AU audio data size overflow"))?;
            let next_samples = self
                .g726_samples
                .checked_add(samples.len() as u64)
                .ok_or(AudioError::Unsupported("AU G.72x sample count overflow"))?;
            if let Err(error) = self.writer.write_all(&encoded) {
                self.poisoned = true;
                return Err(error.into());
            }
            self.data_bytes = next_size;
            self.g726_samples = next_samples;
            return Ok(());
        }
        let bytes_per_sample = match self.encoding {
            Encoding::Linear8 | Encoding::MuLaw | Encoding::ALaw => 1_u64,
            Encoding::Linear16 => 2,
            Encoding::Linear24 => 3,
            Encoding::Linear32 | Encoding::Float32 => 4,
            Encoding::Float64 => 8,
            Encoding::G726(_) => unreachable!("G.72x uses its stateful encoder"),
        };
        let block_bytes = u64::try_from(block.samples().len())
            .ok()
            .and_then(|count| count.checked_mul(bytes_per_sample))
            .ok_or(AudioError::Unsupported("AU sample block size overflow"))?;
        let next_size = self
            .data_bytes
            .checked_add(block_bytes)
            .ok_or(AudioError::Unsupported("AU audio data size overflow"))?;
        let result = write_samples(&mut self.writer, block.samples(), self.encoding);
        if result.is_err() {
            self.poisoned = true;
            return result;
        }
        self.data_bytes = next_size;
        Ok(())
    }
}

fn annotation_from_metadata(metadata: &Metadata) -> Result<Vec<u8>, AudioError> {
    for (key, values) in metadata.iter() {
        if key != "COMMENT" {
            return Err(AudioError::Unsupported(
                "AU annotation supports only the COMMENT metadata field",
            ));
        }
        if values.len() > 1 {
            return Err(AudioError::Unsupported(
                "AU annotation supports one COMMENT value",
            ));
        }
    }
    let comment = metadata
        .values("COMMENT")
        .first()
        .map_or("", String::as_str);
    if comment.as_bytes().contains(&0) {
        return Err(AudioError::InvalidData("AU COMMENT cannot contain NUL"));
    }
    if comment.len() > MAX_ANNOTATION_BYTES.saturating_sub(4) {
        return Err(AudioError::Unsupported("AU annotation exceeds 16 MiB"));
    }
    let mut annotation = Vec::new();
    annotation
        .try_reserve_exact(comment.len().saturating_add(4))
        .map_err(|_| AudioError::InvalidData("AU annotation allocation failed"))?;
    annotation.extend_from_slice(comment.as_bytes());
    annotation.push(0);
    if annotation.len() < 4 {
        annotation.resize(4, 0);
    }
    let padding = (4 - (annotation.len() & 3)) & 3;
    annotation.resize(annotation.len() + padding, 0);
    Ok(annotation)
}

fn write_samples<W: Write>(
    writer: &mut W,
    samples: &Samples,
    encoding: Encoding,
) -> Result<(), AudioError> {
    match (encoding, samples) {
        (Encoding::Linear8, Samples::U8(values)) => {
            for value in values {
                writer.write_all(&[*value ^ 0x80])?;
            }
        }
        (Encoding::Linear16, Samples::I16(values)) => {
            for value in values {
                writer.write_all(&value.to_be_bytes())?;
            }
        }
        (Encoding::Linear24, Samples::I24(values)) => {
            for &value in values {
                writer.write_all(&value.to_be_bytes()[1..])?;
            }
        }
        (Encoding::Linear32, Samples::I32(values)) => {
            for value in values {
                writer.write_all(&value.to_be_bytes())?;
            }
        }
        (Encoding::Float32, Samples::F32(values)) => {
            for value in values {
                writer.write_all(&value.to_bits().to_be_bytes())?;
            }
        }
        (Encoding::Float64, Samples::F64(values)) => {
            for value in values {
                writer.write_all(&value.to_bits().to_be_bytes())?;
            }
        }
        (Encoding::MuLaw, Samples::I16(values)) => {
            for &value in values {
                writer.write_all(&[encode_mulaw(value)])?;
            }
        }
        (Encoding::ALaw, Samples::I16(values)) => {
            for &value in values {
                writer.write_all(&[encode_alaw(value)])?;
            }
        }
        (Encoding::G726(_), _) => unreachable!("G.72x uses its stateful encoder"),
        _ => {
            return Err(AudioError::InvalidData(
                "PCM storage does not match AU encoding",
            ));
        }
    }
    Ok(())
}
