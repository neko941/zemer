use std::io::{Read, Seek, SeekFrom};

use g711::{decode_alaw, decode_mulaw};
use g726::{G726Decoder, G726Rate};
use zemer_core::{AudioDecoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::{MAX_ANNOTATION_BYTES, MINIMUM_DATA_OFFSET, UNKNOWN_DATA_SIZE};

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
    G721,
}

impl Encoding {
    fn parse(code: u32) -> Result<Self, AudioError> {
        match code {
            1 => Ok(Self::MuLaw),
            2 => Ok(Self::Linear8),
            3 => Ok(Self::Linear16),
            4 => Ok(Self::Linear24),
            5 => Ok(Self::Linear32),
            6 => Ok(Self::Float32),
            7 => Ok(Self::Float64),
            23 => Ok(Self::G721),
            27 => Ok(Self::ALaw),
            _ => Err(AudioError::Unsupported("unsupported AU encoding")),
        }
    }

    fn sample_format(self) -> SampleFormat {
        match self {
            Self::Linear8 => SampleFormat::PcmU8,
            Self::Linear16 | Self::MuLaw | Self::ALaw | Self::G721 => SampleFormat::PcmI16,
            Self::Linear24 => SampleFormat::PcmI24,
            Self::Linear32 => SampleFormat::PcmI32,
            Self::Float32 => SampleFormat::Float32,
            Self::Float64 => SampleFormat::Float64,
        }
    }

    fn bytes_per_sample(self) -> u64 {
        match self {
            Self::Linear8 | Self::MuLaw | Self::ALaw | Self::G721 => 1,
            Self::Linear16 => 2,
            Self::Linear24 => 3,
            Self::Linear32 | Self::Float32 => 4,
            Self::Float64 => 8,
        }
    }
}

pub struct AuDecoder<R> {
    reader: R,
    spec: AudioSpec,
    metadata: Metadata,
    annotation: Vec<u8>,
    encoding: Encoding,
    data_bytes_remaining: u64,
    frame_bytes: u64,
    g721_decoder: Option<G726Decoder>,
    g721_pending_sample: Option<i16>,
}

impl<R: Read + Seek> AuDecoder<R> {
    pub fn new(mut reader: R) -> Result<Self, AudioError> {
        reader.seek(SeekFrom::Start(0))?;
        let mut header = [0; 24];
        reader.read_exact(&mut header)?;
        if &header[..4] != b".snd" {
            return Err(AudioError::InvalidFormat("expected an AU .snd header"));
        }
        let data_offset = u64::from(u32::from_be_bytes(header[4..8].try_into().unwrap()));
        let data_size = u32::from_be_bytes(header[8..12].try_into().unwrap());
        let encoding = Encoding::parse(u32::from_be_bytes(header[12..16].try_into().unwrap()))?;
        let sample_rate = u32::from_be_bytes(header[16..20].try_into().unwrap());
        let channels = u32::from_be_bytes(header[20..24].try_into().unwrap());
        let file_end = reader.seek(SeekFrom::End(0))?;
        if data_offset < MINIMUM_DATA_OFFSET || data_offset > file_end {
            return Err(AudioError::InvalidFormat(
                "AU data offset is outside the file",
            ));
        }
        if sample_rate == 0 || channels == 0 || channels > u32::from(u16::MAX) {
            return Err(AudioError::InvalidFormat(
                "invalid AU sample rate or channel count",
            ));
        }
        if matches!(encoding, Encoding::G721) && (sample_rate != 8_000 || channels != 1) {
            return Err(AudioError::Unsupported(
                "AU G.721 requires 8 kHz mono audio",
            ));
        }
        let data_bytes = if data_size == UNKNOWN_DATA_SIZE {
            file_end - data_offset
        } else {
            u64::from(data_size)
        };
        let data_end = data_offset
            .checked_add(data_bytes)
            .ok_or(AudioError::InvalidFormat("AU data size overflow"))?;
        if data_end > file_end {
            return Err(AudioError::InvalidFormat("AU audio data exceeds the file"));
        }
        let frame_bytes = if matches!(encoding, Encoding::G721) {
            1
        } else {
            encoding
                .bytes_per_sample()
                .checked_mul(u64::from(channels))
                .ok_or(AudioError::InvalidFormat("AU sample frame size overflow"))?
        };
        if data_bytes % frame_bytes != 0 {
            return Err(AudioError::InvalidFormat(
                "AU audio data ends in a partial sample frame",
            ));
        }
        let g721_decoder = if matches!(encoding, Encoding::G721) {
            Some(
                G726Decoder::new(G726Rate::Kbps32, 1)
                    .map_err(|_| AudioError::InvalidFormat("invalid AU G.721 configuration"))?,
            )
        } else {
            None
        };
        let annotation_size = usize::try_from(data_offset - 24)
            .map_err(|_| AudioError::InvalidFormat("AU annotation is too large"))?;
        if annotation_size > MAX_ANNOTATION_BYTES {
            return Err(AudioError::InvalidFormat("AU annotation exceeds 16 MiB"));
        }
        let mut annotation = Vec::new();
        annotation
            .try_reserve_exact(annotation_size)
            .map_err(|_| AudioError::InvalidFormat("AU annotation allocation failed"))?;
        annotation.resize(annotation_size, 0);
        reader.seek(SeekFrom::Start(24))?;
        reader.read_exact(&mut annotation)?;
        reader.seek(SeekFrom::Start(data_offset))?;
        let mut metadata = Metadata::default();
        if let Some(comment) = annotation_comment(&annotation) {
            metadata.insert("COMMENT", comment);
        }
        let spec = AudioSpec::new(sample_rate, channels as u16, encoding.sample_format())?;
        Ok(Self {
            reader,
            spec,
            metadata,
            annotation,
            encoding,
            data_bytes_remaining: data_bytes,
            frame_bytes,
            g721_decoder,
            g721_pending_sample: None,
        })
    }

    /// Returns the annotation bytes exactly as stored between the header and audio.
    pub fn annotation(&self) -> &[u8] {
        &self.annotation
    }
}

impl<R: Read + Seek> AudioDecoder for AuDecoder<R> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn read_frames(&mut self, max_frames: usize) -> Result<Option<PcmBuffer>, AudioError> {
        if max_frames == 0 {
            return Err(AudioError::InvalidData(
                "max_frames must be greater than zero",
            ));
        }
        if matches!(self.encoding, Encoding::G721) {
            return self.read_g721_frames(max_frames);
        }
        if self.data_bytes_remaining == 0 {
            return Ok(None);
        }
        let frames = self
            .data_bytes_remaining
            .checked_div(self.frame_bytes)
            .ok_or(AudioError::InvalidData("invalid AU frame size"))?
            .min(u64::try_from(max_frames).unwrap_or(u64::MAX));
        let sample_count = usize::try_from(frames)
            .ok()
            .and_then(|frames| frames.checked_mul(usize::from(self.spec.channels())))
            .ok_or(AudioError::InvalidData("AU sample block is too large"))?;
        let buffer = self.read_samples(sample_count)?;
        self.data_bytes_remaining -= frames * self.frame_bytes;
        Ok(Some(buffer))
    }
}

impl<R: Read + Seek> AuDecoder<R> {
    fn read_g721_frames(&mut self, max_frames: usize) -> Result<Option<PcmBuffer>, AudioError> {
        let mut samples = Vec::new();
        if let Some(sample) = self.g721_pending_sample.take() {
            samples.push(sample);
        }
        let wanted = max_frames - samples.len();
        let byte_count = u64::try_from(wanted.div_ceil(2))
            .unwrap_or(u64::MAX)
            .min(self.data_bytes_remaining);
        if byte_count == 0 && samples.is_empty() {
            return Ok(None);
        }
        if byte_count != 0 {
            let byte_count = usize::try_from(byte_count)
                .map_err(|_| AudioError::InvalidData("AU G.721 block is too large"))?;
            let mut packed = Vec::new();
            packed
                .try_reserve_exact(byte_count)
                .map_err(|_| AudioError::InvalidData("AU G.721 allocation failed"))?;
            packed.resize(byte_count, 0);
            self.reader.read_exact(&mut packed)?;
            let sample_count = byte_count
                .checked_mul(2)
                .ok_or(AudioError::InvalidData("AU G.721 block is too large"))?;
            let decoded = self
                .g721_decoder
                .as_mut()
                .ok_or(AudioError::InvalidData("AU G.721 decoder is unavailable"))?
                .decode_packed_lsb(&packed, sample_count)
                .map_err(|_| AudioError::InvalidData("invalid AU G.721 payload"))?;
            self.data_bytes_remaining -= u64::try_from(byte_count)
                .map_err(|_| AudioError::InvalidData("AU G.721 block is too large"))?;
            samples.extend(decoded);
        }
        if samples.len() > max_frames {
            self.g721_pending_sample = samples.pop();
        }
        PcmBuffer::new(self.spec, Samples::I16(samples))
            .map(Some)
            .map_err(Into::into)
    }

    fn read_samples(&mut self, sample_count: usize) -> Result<PcmBuffer, AudioError> {
        match self.encoding {
            Encoding::Linear8 => {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(sample_count)
                    .map_err(|_| AudioError::InvalidData("AU sample allocation failed"))?;
                for _ in 0..sample_count {
                    let mut byte = [0; 1];
                    self.reader.read_exact(&mut byte)?;
                    // The shared PCM model uses offset-binary U8 for signed 8-bit samples.
                    values.push(byte[0] ^ 0x80);
                }
                PcmBuffer::new(self.spec, Samples::U8(values)).map_err(Into::into)
            }
            Encoding::Linear16 => {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(sample_count)
                    .map_err(|_| AudioError::InvalidData("AU sample allocation failed"))?;
                for _ in 0..sample_count {
                    let mut bytes = [0; 2];
                    self.reader.read_exact(&mut bytes)?;
                    values.push(i16::from_be_bytes(bytes));
                }
                PcmBuffer::new(self.spec, Samples::I16(values)).map_err(Into::into)
            }
            Encoding::Linear24 => {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(sample_count)
                    .map_err(|_| AudioError::InvalidData("AU sample allocation failed"))?;
                for _ in 0..sample_count {
                    let mut bytes = [0; 3];
                    self.reader.read_exact(&mut bytes)?;
                    let raw = i32::from_be_bytes([
                        if bytes[0] & 0x80 != 0 { 0xff } else { 0 },
                        bytes[0],
                        bytes[1],
                        bytes[2],
                    ]);
                    values.push(raw);
                }
                PcmBuffer::new(self.spec, Samples::I24(values)).map_err(Into::into)
            }
            Encoding::Linear32 => {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(sample_count)
                    .map_err(|_| AudioError::InvalidData("AU sample allocation failed"))?;
                for _ in 0..sample_count {
                    let mut bytes = [0; 4];
                    self.reader.read_exact(&mut bytes)?;
                    values.push(i32::from_be_bytes(bytes));
                }
                PcmBuffer::new(self.spec, Samples::I32(values)).map_err(Into::into)
            }
            Encoding::Float32 => {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(sample_count)
                    .map_err(|_| AudioError::InvalidData("AU sample allocation failed"))?;
                for _ in 0..sample_count {
                    let mut bytes = [0; 4];
                    self.reader.read_exact(&mut bytes)?;
                    values.push(f32::from_bits(u32::from_be_bytes(bytes)));
                }
                PcmBuffer::new(self.spec, Samples::F32(values)).map_err(Into::into)
            }
            Encoding::Float64 => {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(sample_count)
                    .map_err(|_| AudioError::InvalidData("AU sample allocation failed"))?;
                for _ in 0..sample_count {
                    let mut bytes = [0; 8];
                    self.reader.read_exact(&mut bytes)?;
                    values.push(f64::from_bits(u64::from_be_bytes(bytes)));
                }
                PcmBuffer::new(self.spec, Samples::F64(values)).map_err(Into::into)
            }
            Encoding::MuLaw | Encoding::ALaw => {
                let mut values = Vec::new();
                values
                    .try_reserve_exact(sample_count)
                    .map_err(|_| AudioError::InvalidData("AU sample allocation failed"))?;
                for _ in 0..sample_count {
                    let mut byte = [0; 1];
                    self.reader.read_exact(&mut byte)?;
                    values.push(if matches!(self.encoding, Encoding::MuLaw) {
                        decode_mulaw(byte[0])
                    } else {
                        decode_alaw(byte[0])
                    });
                }
                PcmBuffer::new(self.spec, Samples::I16(values)).map_err(Into::into)
            }
            Encoding::G721 => unreachable!("G.721 uses its packed streaming path"),
        }
    }
}

pub(crate) fn annotation_comment(annotation: &[u8]) -> Option<&str> {
    let text = annotation.split(|&byte| byte == 0).next()?;
    if text.is_empty() {
        None
    } else {
        std::str::from_utf8(text).ok()
    }
}
