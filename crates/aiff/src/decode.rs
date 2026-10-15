use std::io::{Read, Seek, SeekFrom};

use g711::{decode_alaw, decode_mulaw};
use g726::G726Decoder;
use id3::parse_id3_tag;
use zemer_core::{AudioDecoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::G726Rate;
use crate::comment::{AiffComment, parse_comments};
use crate::instrument::{AiffInstrument, parse_instrument, validate_instrument_markers};
use crate::marker::{AiffMarker, parse_markers};

const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_COMM_BYTES: u64 = 1024;
const MAX_DECODE_BYTES: u64 = 16 * 1024 * 1024;
const AIFC_VERSION: u32 = 0xA280_5140;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Encoding {
    IntegerBe,
    IntegerLe,
    Float32,
    Float64,
    Ima4,
    ALaw,
    MuLaw,
    G726,
}

pub struct AiffDecoder<R> {
    reader: R,
    spec: AudioSpec,
    metadata: Metadata,
    id3_tag: Option<Vec<u8>>,
    comments: Vec<AiffComment>,
    markers: Vec<AiffMarker>,
    instrument: Option<AiffInstrument>,
    aifc: bool,
    encoding: Encoding,
    bits: u16,
    frame_bytes: u64,
    frames_left: u64,
    ima4_packets_left: u64,
    ima4_pending: Vec<i16>,
    ima4_pending_offset: usize,
    ima4_states: Vec<Option<crate::ima4::State>>,
    g726: Option<G726Decoder>,
}

impl<R: Read + Seek> AiffDecoder<R> {
    pub fn new(reader: R) -> Result<Self, AudioError> {
        Self::new_inner(reader, None)
    }

    /// Decode AIFC `G726` when the file does not contain a recognized bitrate hint.
    pub fn new_with_g726_rate(reader: R, rate: G726Rate) -> Result<Self, AudioError> {
        Self::new_inner(reader, Some(rate))
    }

    fn new_inner(mut reader: R, g726_rate_override: Option<G726Rate>) -> Result<Self, AudioError> {
        reader.seek(SeekFrom::Start(0))?;
        let mut form = [0; 12];
        reader.read_exact(&mut form)?;
        if &form[..4] != b"FORM" || !matches!(&form[8..12], b"AIFF" | b"AIFC") {
            return Err(AudioError::InvalidFormat(
                "expected a FORM AIFF or AIFC file",
            ));
        }
        let aifc = &form[8..12] == b"AIFC";
        let file_end = 8_u64
            .checked_add(u64::from(u32::from_be_bytes(
                form[4..8].try_into().unwrap(),
            )))
            .ok_or(AudioError::InvalidFormat("AIFF FORM size overflow"))?;
        let physical_end = reader.seek(SeekFrom::End(0))?;
        let missing_final_pad = file_end.checked_sub(physical_end) == Some(1);
        if (file_end > physical_end && !missing_final_pad) || file_end < 12 {
            return Err(AudioError::InvalidFormat("AIFF FORM extends past the file"));
        }
        reader.seek(SeekFrom::Start(12))?;

        let mut common = None;
        let mut version = None;
        let mut sound = None;
        let mut native_metadata = Metadata::default();
        let mut id3_metadata = None;
        let mut id3_tag = None;
        let mut comments = None;
        let mut markers = None;
        let mut instrument = None;
        let mut g726_rate_hint = None;
        let mut metadata_bytes = 0_u64;
        while reader.stream_position()? < file_end {
            let position = reader.stream_position()?;
            if file_end - position < 8 {
                return Err(AudioError::InvalidFormat("truncated AIFF chunk header"));
            }
            let mut header = [0; 8];
            reader.read_exact(&mut header)?;
            let size = u64::from(u32::from_be_bytes(header[4..8].try_into().unwrap()));
            let body = reader.stream_position()?;
            let mut end = body
                .checked_add(size)
                .and_then(|value| value.checked_add(size & 1))
                .ok_or(AudioError::InvalidFormat("AIFF chunk size overflow"))?;
            let omitted_pad = missing_final_pad
                && end == file_end
                && size & 1 == 1
                && body.checked_add(size) == Some(physical_end);
            if end > file_end || (end > physical_end && !omitted_pad) {
                return Err(AudioError::InvalidFormat("AIFF chunk extends past FORM"));
            }
            match &header[..4] {
                b"COMM" => {
                    if common.is_some()
                        || (if aifc {
                            size != 18 && size < 23
                        } else {
                            size < 18
                        })
                        || size > MAX_COMM_BYTES
                    {
                        return Err(AudioError::InvalidFormat(
                            "invalid or duplicate AIFF COMM chunk",
                        ));
                    }
                    let read_size = usize::try_from(size)
                        .map_err(|_| AudioError::InvalidFormat("AIFF COMM chunk is too large"))?;
                    let mut bytes = vec![0; read_size];
                    reader.read_exact(&mut bytes)?;
                    if aifc && size == 18 {
                        // McGill's digit0v0 declares the classic fields only, but stores NONE after them.
                        let extension_end = body
                            .checked_add(38)
                            .ok_or(AudioError::InvalidFormat("AIFF-C COMM size overflow"))?;
                        if extension_end > file_end || extension_end > physical_end {
                            return Err(AudioError::InvalidFormat(
                                "AIFF-C COMM extension exceeds FORM",
                            ));
                        }
                        let mut extension = [0; 20];
                        reader.read_exact(&mut extension)?;
                        if &extension != b"NONE\x0enot compressed\0" {
                            return Err(AudioError::InvalidFormat(
                                "invalid short AIFF-C COMM chunk",
                            ));
                        }
                        bytes.extend_from_slice(&extension);
                        end = extension_end;
                    }
                    common = Some(parse_common(&bytes, aifc)?);
                }
                b"FVER" => {
                    if !aifc || version.is_some() || size != 4 {
                        return Err(AudioError::InvalidFormat("invalid AIFF-C FVER chunk"));
                    }
                    let mut bytes = [0; 4];
                    reader.read_exact(&mut bytes)?;
                    version = Some(u32::from_be_bytes(bytes));
                }
                b"SSND" => {
                    if sound.is_some() || size < 8 {
                        return Err(AudioError::InvalidFormat(
                            "invalid or duplicate AIFF SSND chunk",
                        ));
                    }
                    let mut prefix = [0; 8];
                    reader.read_exact(&mut prefix)?;
                    let offset = u64::from(u32::from_be_bytes(prefix[..4].try_into().unwrap()));
                    let block_size = u32::from_be_bytes(prefix[4..8].try_into().unwrap());
                    if block_size != 0 {
                        return Err(AudioError::Unsupported(
                            "blocked AIFF SSND audio is not supported",
                        ));
                    }
                    let sound_data = reader
                        .stream_position()?
                        .checked_add(offset)
                        .ok_or(AudioError::InvalidFormat("AIFF SSND offset overflow"))?;
                    let sound_end = body
                        .checked_add(size)
                        .ok_or(AudioError::InvalidFormat("AIFF SSND size overflow"))?;
                    if sound_data > sound_end {
                        return Err(AudioError::InvalidFormat(
                            "AIFF SSND offset exceeds its chunk",
                        ));
                    }
                    sound = Some((sound_data, sound_end));
                }
                b"NAME" | b"AUTH" | b"ANNO" => {
                    metadata_bytes = metadata_bytes
                        .checked_add(size)
                        .ok_or(AudioError::InvalidFormat("AIFF metadata size overflow"))?;
                    if metadata_bytes > MAX_METADATA_BYTES {
                        return Err(AudioError::InvalidFormat("AIFF metadata exceeds 16 MiB"));
                    }
                    let mut bytes = vec![0; size as usize];
                    reader.read_exact(&mut bytes)?;
                    if !bytes.is_ascii() {
                        return Err(AudioError::InvalidFormat(
                            "AIFF text metadata must be ASCII",
                        ));
                    }
                    let value = String::from_utf8(bytes).unwrap();
                    match &header[..4] {
                        b"NAME" => native_metadata.insert("title", value),
                        b"AUTH" => native_metadata.insert("artist", value),
                        _ => native_metadata.insert("comment", value),
                    }
                }
                b"ID3 " => {
                    if id3_tag.is_some() || size == 0 {
                        return Err(AudioError::InvalidFormat(
                            "invalid or duplicate AIFF ID3 chunk",
                        ));
                    }
                    metadata_bytes = metadata_bytes
                        .checked_add(size)
                        .ok_or(AudioError::InvalidFormat("AIFF metadata size overflow"))?;
                    if metadata_bytes > MAX_METADATA_BYTES {
                        return Err(AudioError::InvalidFormat("AIFF metadata exceeds 16 MiB"));
                    }
                    let read_size = usize::try_from(size)
                        .map_err(|_| AudioError::InvalidFormat("AIFF ID3 tag is too large"))?;
                    let mut bytes = Vec::new();
                    bytes
                        .try_reserve_exact(read_size)
                        .map_err(|_| AudioError::InvalidFormat("AIFF ID3 tag is too large"))?;
                    bytes.resize(read_size, 0);
                    reader.read_exact(&mut bytes)?;
                    id3_metadata = Some(parse_id3_tag(&bytes)?);
                    id3_tag = Some(bytes);
                }
                b"COMT" => {
                    if comments.is_some() {
                        return Err(AudioError::InvalidFormat("duplicate AIFF COMT chunk"));
                    }
                    metadata_bytes = metadata_bytes
                        .checked_add(size)
                        .ok_or(AudioError::InvalidFormat("AIFF metadata size overflow"))?;
                    if metadata_bytes > MAX_METADATA_BYTES {
                        return Err(AudioError::InvalidFormat("AIFF metadata exceeds 16 MiB"));
                    }
                    let read_size = usize::try_from(size)
                        .map_err(|_| AudioError::InvalidFormat("AIFF COMT is too large"))?;
                    let mut bytes = vec![0; read_size];
                    reader.read_exact(&mut bytes)?;
                    comments = Some(parse_comments(&bytes)?);
                }
                b"MARK" => {
                    if markers.is_some() {
                        return Err(AudioError::InvalidFormat("duplicate AIFF MARK chunk"));
                    }
                    metadata_bytes = metadata_bytes
                        .checked_add(size)
                        .ok_or(AudioError::InvalidFormat("AIFF metadata size overflow"))?;
                    if metadata_bytes > MAX_METADATA_BYTES {
                        return Err(AudioError::InvalidFormat("AIFF metadata exceeds 16 MiB"));
                    }
                    let read_size = usize::try_from(size)
                        .map_err(|_| AudioError::InvalidFormat("AIFF MARK is too large"))?;
                    let mut bytes = Vec::new();
                    bytes
                        .try_reserve_exact(read_size)
                        .map_err(|_| AudioError::InvalidFormat("AIFF MARK is too large"))?;
                    bytes.resize(read_size, 0);
                    reader.read_exact(&mut bytes)?;
                    markers = Some(parse_markers(&bytes)?);
                }
                b"INST" if size == 20 => {
                    if instrument.is_some() {
                        return Err(AudioError::InvalidFormat("duplicate AIFF INST chunk"));
                    }
                    metadata_bytes = metadata_bytes
                        .checked_add(size)
                        .ok_or(AudioError::InvalidFormat("AIFF metadata size overflow"))?;
                    if metadata_bytes > MAX_METADATA_BYTES {
                        return Err(AudioError::InvalidFormat("AIFF metadata exceeds 16 MiB"));
                    }
                    let mut bytes = [0; 20];
                    reader.read_exact(&mut bytes)?;
                    instrument = Some(parse_instrument(&bytes)?);
                }
                b"APPL" if size == 12 => {
                    let mut bytes = [0; 12];
                    reader.read_exact(&mut bytes)?;
                    if &bytes[..8] == b"stocG726" {
                        let rate = match u32::from_be_bytes(bytes[8..].try_into().unwrap()) {
                            16_000 => Some(G726Rate::Kbps16),
                            24_000 => Some(G726Rate::Kbps24),
                            32_000 => Some(G726Rate::Kbps32),
                            40_000 => Some(G726Rate::Kbps40),
                            _ => None,
                        };
                        if let Some(rate) = rate {
                            if g726_rate_hint.is_some_and(|previous| previous != rate) {
                                return Err(AudioError::InvalidFormat(
                                    "conflicting AIFF-C G.726 bitrate hints",
                                ));
                            }
                            g726_rate_hint = Some(rate);
                        }
                    }
                }
                _ => {}
            }
            reader.seek(SeekFrom::Start(end))?;
        }
        if reader.stream_position()? != file_end {
            return Err(AudioError::InvalidFormat(
                "AIFF chunk layout does not end at FORM boundary",
            ));
        }
        if aifc && version != Some(AIFC_VERSION) {
            return Err(AudioError::InvalidFormat(
                "AIFF-C requires the supported FVER version",
            ));
        }
        let (channels, frames, bits, rate, encoding) =
            common.ok_or(AudioError::InvalidFormat("AIFF file is missing COMM"))?;
        if encoding == Encoding::G726 && rate != 8_000 {
            return Err(AudioError::Unsupported(
                "AIFC G.726 requires an 8 kHz sample rate",
            ));
        }
        let g726_rate = (encoding == Encoding::G726)
            .then(|| g726_rate_override.or(g726_rate_hint))
            .flatten();
        if encoding == Encoding::G726 && g726_rate.is_none() {
            return Err(AudioError::Unsupported(
                "AIFC G.726 bitrate is unknown; use AiffDecoder::new_with_g726_rate",
            ));
        }
        let (data_offset, sound_end) = match sound {
            Some(sound) => sound,
            None if frames == 0 => (file_end, file_end),
            None => return Err(AudioError::InvalidFormat("AIFF file is missing SSND")),
        };
        let sample_format = encoding.sample_format(bits)?;
        let spec = AudioSpec::new(rate, channels, sample_format)?;
        let sample_bytes = match encoding {
            Encoding::Ima4 => 2,
            Encoding::ALaw | Encoding::MuLaw => 1,
            _ => u64::from(bits.div_ceil(8)),
        };
        let frame_bytes = sample_bytes
            .checked_mul(u64::from(channels))
            .ok_or(AudioError::InvalidFormat("AIFF frame size overflow"))?;
        let (decoded_frames, audio_bytes, ima4_packets_left) = if encoding == Encoding::Ima4 {
            let packets = u64::from(frames);
            (
                packets
                    .checked_mul(64)
                    .ok_or(AudioError::InvalidFormat("AIFF IMA4 frame count overflow"))?,
                packets
                    .checked_mul(u64::from(channels))
                    .and_then(|bytes| bytes.checked_mul(34))
                    .ok_or(AudioError::InvalidFormat("AIFF IMA4 data size overflow"))?,
                packets,
            )
        } else if encoding == Encoding::G726 {
            let sample_count = u64::from(frames)
                .checked_mul(u64::from(channels))
                .ok_or(AudioError::InvalidFormat("AIFF sample count overflow"))?;
            let bits = sample_count
                .checked_mul(u64::from(g726_rate.unwrap().bits_per_sample()))
                .ok_or(AudioError::InvalidFormat("AIFF G.726 size overflow"))?;
            (u64::from(frames), bits.div_ceil(8), 0)
        } else {
            (
                u64::from(frames),
                frame_bytes
                    .checked_mul(u64::from(frames))
                    .ok_or(AudioError::InvalidFormat("AIFF audio size overflow"))?,
                0,
            )
        };
        let markers = markers.unwrap_or_default();
        if markers
            .iter()
            .any(|marker| u64::from(marker.position) > decoded_frames)
        {
            return Err(AudioError::InvalidFormat(
                "AIFF marker position exceeds the sample-frame count",
            ));
        }
        if let Some(instrument) = &instrument {
            validate_instrument_markers(instrument, &markers)
                .map_err(|_| AudioError::InvalidFormat("AIFF INST references an invalid marker"))?;
        }
        if data_offset
            .checked_add(audio_bytes)
            .is_none_or(|end| end > sound_end)
        {
            return Err(AudioError::InvalidFormat(
                "AIFF SSND is shorter than COMM frame count",
            ));
        }
        reader.seek(SeekFrom::Start(data_offset))?;
        let g726 = g726_rate
            .map(|rate| {
                G726Decoder::new(rate, usize::from(channels))
                    .map_err(|_| AudioError::InvalidFormat("invalid AIFF-C G.726 channels"))
            })
            .transpose()?;
        let mut metadata = id3_metadata.unwrap_or_default();
        for (key, values) in native_metadata.iter() {
            metadata.remove(key);
            for value in values {
                metadata.insert(key, value.clone());
            }
        }
        Ok(Self {
            reader,
            spec,
            metadata,
            id3_tag,
            comments: comments.unwrap_or_default(),
            markers,
            instrument,
            aifc,
            encoding,
            bits,
            frame_bytes,
            frames_left: decoded_frames,
            ima4_packets_left,
            ima4_pending: Vec::new(),
            ima4_pending_offset: 0,
            ima4_states: vec![None; usize::from(channels)],
            g726,
        })
    }

    pub fn is_aifc(&self) -> bool {
        self.aifc
    }

    pub fn comments(&self) -> &[AiffComment] {
        &self.comments
    }

    /// Returns the complete bounded ID3 chunk payload for lossless AIFF copying.
    pub fn id3_tag(&self) -> Option<&[u8]> {
        self.id3_tag.as_deref()
    }

    /// Returns all typed MARK records in file order.
    pub fn markers(&self) -> &[AiffMarker] {
        &self.markers
    }

    /// Returns the standard 20-byte AIFF/AIFC Instrument chunk, if present.
    pub fn instrument(&self) -> Option<&AiffInstrument> {
        self.instrument.as_ref()
    }

    fn read_samples(&mut self, samples: usize) -> Result<Samples, AudioError> {
        let bytes_per_sample = match self.encoding {
            Encoding::ALaw | Encoding::MuLaw => 1,
            _ => usize::from(self.bits.div_ceil(8)),
        };
        let mut raw = vec![
            0;
            samples
                .checked_mul(bytes_per_sample)
                .ok_or(AudioError::InvalidData("AIFF block size overflow"))?
        ];
        self.reader.read_exact(&mut raw)?;
        match self.spec.sample_format() {
            SampleFormat::PcmU8 => Ok(Samples::U8(
                raw.iter().map(|byte| byte.wrapping_add(128)).collect(),
            )),
            SampleFormat::PcmI16 => Ok(Samples::I16(match self.encoding {
                Encoding::ALaw => raw.into_iter().map(decode_alaw).collect(),
                Encoding::MuLaw => raw.into_iter().map(decode_mulaw).collect(),
                Encoding::IntegerLe => raw
                    .chunks_exact(2)
                    .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
                    .collect(),
                _ => raw
                    .chunks_exact(2)
                    .map(|bytes| i16::from_be_bytes([bytes[0], bytes[1]]))
                    .collect(),
            })),
            SampleFormat::PcmI24 => Ok(Samples::I24(
                raw.chunks_exact(3)
                    .map(|bytes| match self.encoding {
                        Encoding::IntegerLe => i32::from_le_bytes([
                            bytes[0],
                            bytes[1],
                            bytes[2],
                            if bytes[2] & 0x80 == 0 { 0 } else { 0xff },
                        ]),
                        _ => i32::from_be_bytes([
                            if bytes[0] & 0x80 == 0 { 0 } else { 0xff },
                            bytes[0],
                            bytes[1],
                            bytes[2],
                        ]),
                    })
                    .collect(),
            )),
            SampleFormat::PcmI32 => Ok(Samples::I32(
                raw.chunks_exact(4)
                    .map(|bytes| match self.encoding {
                        Encoding::IntegerLe => i32::from_le_bytes(bytes.try_into().unwrap()),
                        _ => i32::from_be_bytes(bytes.try_into().unwrap()),
                    })
                    .collect(),
            )),
            SampleFormat::Float32 => Ok(Samples::F32(
                raw.chunks_exact(4)
                    .map(|bytes| f32::from_bits(u32::from_be_bytes(bytes.try_into().unwrap())))
                    .collect(),
            )),
            SampleFormat::Float64 => Ok(Samples::F64(
                raw.chunks_exact(8)
                    .map(|bytes| f64::from_bits(u64::from_be_bytes(bytes.try_into().unwrap())))
                    .collect(),
            )),
        }
    }
}

impl Encoding {
    fn sample_format(self, bits: u16) -> Result<SampleFormat, AudioError> {
        match (self, bits) {
            (Self::Ima4, 4 | 16) | (Self::ALaw | Self::MuLaw, 16) => Ok(SampleFormat::PcmI16),
            (Self::G726, 16) => Ok(SampleFormat::PcmI16),
            (Self::IntegerBe | Self::IntegerLe, 1..=8) => Ok(SampleFormat::PcmU8),
            (Self::IntegerBe | Self::IntegerLe, 9..=16) => Ok(SampleFormat::PcmI16),
            (Self::IntegerBe | Self::IntegerLe, 17..=24) => Ok(SampleFormat::PcmI24),
            (Self::IntegerBe | Self::IntegerLe, 25..=32) => Ok(SampleFormat::PcmI32),
            (Self::Float32, 32) => Ok(SampleFormat::Float32),
            (Self::Float64, 64) => Ok(SampleFormat::Float64),
            _ => Err(AudioError::Unsupported(
                "AIFF sample encoding or bit depth is not supported",
            )),
        }
    }
}

fn parse_common(bytes: &[u8], aifc: bool) -> Result<(u16, u32, u16, u32, Encoding), AudioError> {
    let channels = u16::from_be_bytes(bytes[..2].try_into().unwrap());
    let frames = u32::from_be_bytes(bytes[2..6].try_into().unwrap());
    let bits = u16::from_be_bytes(bytes[6..8].try_into().unwrap());
    let rate = extended_rate(&bytes[8..18])?;
    // McGill's digit0v0 uses the 18-byte AIFF PCM COMM body in an AIFC form.
    let encoding = if !aifc {
        Encoding::IntegerBe
    } else {
        if bytes.len() < 23 {
            return Err(AudioError::InvalidFormat(
                "AIFF-C COMM is missing its compression name",
            ));
        }
        let name_length = usize::from(bytes[22]);
        let padded = (name_length + 1 + 1) & !1;
        if bytes.len() != 22 + padded {
            return Err(AudioError::InvalidFormat(
                "AIFF-C compression name length does not match COMM",
            ));
        }
        match &bytes[18..22] {
            b"NONE" | b"twos" => Encoding::IntegerBe,
            b"sowt" => Encoding::IntegerLe,
            b"fl32" | b"FL32" => Encoding::Float32,
            b"fl64" | b"FL64" => Encoding::Float64,
            b"alaw" | b"ALAW" if bits == 16 => Encoding::ALaw,
            b"ulaw" | b"ULAW" if bits == 16 => Encoding::MuLaw,
            b"ima4" if matches!(bits, 4 | 16) => Encoding::Ima4,
            b"G726" if bits == 16 => Encoding::G726,
            _ => {
                return Err(AudioError::Unsupported(
                    "AIFF-C compression type is not supported",
                ));
            }
        }
    };
    Ok((channels, frames, bits, rate, encoding))
}

fn extended_rate(bytes: &[u8]) -> Result<u32, AudioError> {
    let sign_exp = u16::from_be_bytes(bytes[..2].try_into().unwrap());
    let exponent = sign_exp & 0x7fff;
    let mantissa = u64::from_be_bytes(bytes[2..10].try_into().unwrap());
    if sign_exp & 0x8000 != 0 || exponent == 0 || exponent == 0x7fff || mantissa == 0 {
        return Err(AudioError::InvalidFormat(
            "AIFF sample rate is not a positive finite value",
        ));
    }
    let rate = (mantissa as f64) * 2_f64.powi(i32::from(exponent) - 16383 - 63);
    if !rate.is_finite() || rate < 1.0 || rate > f64::from(u32::MAX) {
        return Err(AudioError::InvalidFormat(
            "AIFF sample rate is out of range",
        ));
    }
    let rounded = rate.round();
    if (rate - rounded).abs() > 0.001 {
        return Err(AudioError::Unsupported(
            "fractional AIFF sample rates are not supported",
        ));
    }
    Ok(rounded as u32)
}

impl<R: Read + Seek> AudioDecoder for AiffDecoder<R> {
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
        if self.frames_left == 0 {
            return Ok(None);
        }
        if self.encoding == Encoding::Ima4 {
            return self.read_ima4_frames(max_frames);
        }
        if self.encoding == Encoding::G726 {
            return self.read_g726_frames(max_frames);
        }
        let max_block_frames = (MAX_DECODE_BYTES / self.frame_bytes).max(1);
        let frames = self
            .frames_left
            .min(max_frames as u64)
            .min(max_block_frames) as usize;
        let samples = frames
            .checked_mul(usize::from(self.spec.channels()))
            .ok_or(AudioError::InvalidData("AIFF sample count overflow"))?;
        let data = self.read_samples(samples)?;
        self.frames_left -= frames as u64;
        PcmBuffer::new(self.spec, data)
            .map(Some)
            .map_err(Into::into)
    }
}

impl<R: Read + Seek> AiffDecoder<R> {
    fn read_g726_frames(&mut self, max_frames: usize) -> Result<Option<PcmBuffer>, AudioError> {
        let channels = usize::from(self.spec.channels());
        let max_block_frames = (MAX_DECODE_BYTES / self.frame_bytes).max(1);
        let frames = self
            .frames_left
            .min(max_frames as u64)
            .min(max_block_frames) as usize;
        let samples = frames
            .checked_mul(channels)
            .ok_or(AudioError::InvalidData("AIFF sample count overflow"))?;
        let packed_len = self
            .g726
            .as_ref()
            .ok_or(AudioError::InvalidData(
                "AIFF G.726 decoder state is missing",
            ))?
            .packed_lsb_bytes_needed(samples)
            .map_err(|_| AudioError::InvalidData("AIFF G.726 block size overflow"))?;
        let mut packed = Vec::new();
        packed
            .try_reserve_exact(packed_len)
            .map_err(|_| AudioError::InvalidData("AIFF G.726 block is too large"))?;
        packed.resize(packed_len, 0);
        self.reader.read_exact(&mut packed)?;
        let mut input = packed.as_slice();
        let decoded = self
            .g726
            .as_mut()
            .ok_or(AudioError::InvalidData(
                "AIFF G.726 decoder state is missing",
            ))?
            .decode_packed_lsb_stream(&mut input, samples)
            .map_err(|_| AudioError::InvalidData("invalid AIFF G.726 packed block"))?;
        if !input.is_empty() {
            return Err(AudioError::InvalidData(
                "AIFF G.726 decoder did not consume its packed block",
            ));
        }
        self.frames_left -= frames as u64;
        PcmBuffer::new(self.spec, Samples::I16(decoded))
            .map(Some)
            .map_err(Into::into)
    }

    fn read_ima4_frames(&mut self, max_frames: usize) -> Result<Option<PcmBuffer>, AudioError> {
        let channels = usize::from(self.spec.channels());
        let frame_bytes = u64::try_from(channels)
            .ok()
            .and_then(|count| count.checked_mul(2))
            .ok_or(AudioError::InvalidData("AIFF IMA4 frame size overflow"))?;
        let max_block_frames = (MAX_DECODE_BYTES / frame_bytes).max(1);
        let frames = self
            .frames_left
            .min(max_frames as u64)
            .min(max_block_frames) as usize;
        let sample_count = frames
            .checked_mul(channels)
            .ok_or(AudioError::InvalidData("AIFF IMA4 sample count overflow"))?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(sample_count)
            .map_err(|_| AudioError::InvalidData("AIFF IMA4 block is too large"))?;
        while output.len() < sample_count {
            if self.ima4_pending_offset == self.ima4_pending.len() {
                if self.ima4_packets_left == 0 {
                    return Err(AudioError::InvalidFormat(
                        "AIFF IMA4 packet data is truncated",
                    ));
                }
                self.ima4_pending.resize(64 * channels, 0);
                for channel in 0..channels {
                    let mut packet = [0; 34];
                    self.reader.read_exact(&mut packet)?;
                    let (decoded, state) =
                        crate::ima4::decode_packet(&packet, self.ima4_states[channel])?;
                    self.ima4_states[channel] = Some(state);
                    for (frame, sample) in decoded.into_iter().enumerate() {
                        self.ima4_pending[frame * channels + channel] = sample;
                    }
                }
                self.ima4_packets_left -= 1;
                self.ima4_pending_offset = 0;
            }
            let remaining = sample_count - output.len();
            let available = self.ima4_pending.len() - self.ima4_pending_offset;
            let take = remaining.min(available);
            output.extend_from_slice(
                &self.ima4_pending[self.ima4_pending_offset..self.ima4_pending_offset + take],
            );
            self.ima4_pending_offset += take;
        }
        self.frames_left -= frames as u64;
        PcmBuffer::new(self.spec, Samples::I16(output))
            .map(Some)
            .map_err(Into::into)
    }
}
