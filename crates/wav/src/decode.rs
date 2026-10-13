use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Seek, SeekFrom},
};

use crate::encode::WavContainer;
use g711::{decode_alaw, decode_mulaw};
use zemer_core::{AudioDecoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

const PCM_GUID: [u8; 16] = [
    1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
];
const FLOAT_GUID: [u8; 16] = [
    3, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
];
const ALAW_GUID: [u8; 16] = [
    6, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
];
const MULAW_GUID: [u8; 16] = [
    7, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
];
const MAX_FORMAT_CHUNK: u32 = 64 * 1024;
const MAX_INFO_VALUE: u32 = 1024 * 1024;
const MAX_AUDIO_SEGMENTS: usize = 1_000_000;
pub(super) const MAX_DS64_CHUNK: u32 = 1024 * 1024;

pub(super) struct Ds64 {
    pub(super) riff_size: u64,
    pub(super) data_size: u64,
    table: HashMap<[u8; 4], VecDeque<u64>>,
}

#[derive(Clone, Copy)]
enum AudioSegment {
    Data {
        next_offset: u64,
        remaining_frames: u64,
    },
    Silence {
        remaining_frames: u64,
    },
}

#[derive(Default)]
struct AudioLayout {
    segments: VecDeque<AudioSegment>,
    data_chunk_seen: bool,
    remaining_frames: u64,
}

impl AudioLayout {
    fn push(&mut self, segment: AudioSegment) -> Result<(), AudioError> {
        if self.segments.len() == MAX_AUDIO_SEGMENTS {
            return Err(AudioError::InvalidFormat(
                "WAVE audio segment count exceeds the safety limit",
            ));
        }
        let frames = match segment {
            AudioSegment::Data {
                remaining_frames, ..
            }
            | AudioSegment::Silence { remaining_frames } => remaining_frames,
        };
        let remaining_frames = self
            .remaining_frames
            .checked_add(frames)
            .ok_or(AudioError::InvalidFormat("WAVE sample count overflow"))?;
        self.segments
            .try_reserve(1)
            .map_err(|_| AudioError::InvalidFormat("WAVE audio segment list is too large"))?;
        self.segments.push_back(segment);
        self.remaining_frames = remaining_frames;
        Ok(())
    }
}

impl Ds64 {
    pub(super) fn take_table_size(&mut self, chunk_id: &[u8]) -> Option<u64> {
        let id: [u8; 4] = chunk_id.try_into().ok()?;
        let sizes = self.table.get_mut(&id)?;
        let size = sizes.pop_front();
        if sizes.is_empty() {
            self.table.remove(&id);
        }
        size
    }
}

pub(super) fn parse_ds64(bytes: &[u8]) -> Result<Ds64, AudioError> {
    if bytes.len() < 28 {
        return Err(AudioError::InvalidFormat("RF64 ds64 chunk is too short"));
    }
    let riff_size = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let data_size = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let _sample_count = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    let table_length = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
    let expected_size = 28_usize
        .checked_add(
            usize::try_from(table_length)
                .map_err(|_| AudioError::InvalidFormat("RF64 ds64 table is too large"))?
                .checked_mul(12)
                .ok_or(AudioError::InvalidFormat("RF64 ds64 table size overflow"))?,
        )
        .ok_or(AudioError::InvalidFormat("RF64 ds64 size overflow"))?;
    if expected_size != bytes.len() {
        return Err(AudioError::InvalidFormat(
            "RF64 ds64 table length does not match its chunk size",
        ));
    }
    let mut table = HashMap::<[u8; 4], VecDeque<u64>>::new();
    for entry in bytes[28..].chunks_exact(12) {
        let id = entry[..4].try_into().unwrap();
        let size = u64::from_le_bytes(entry[4..].try_into().unwrap());
        table.entry(id).or_default().push_back(size);
    }
    Ok(Ds64 {
        riff_size,
        data_size,
        table,
    })
}

fn parse_wavl<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    size: u64,
    frame_bytes: u16,
    container: WavContainer,
    ds64: &mut Option<Ds64>,
    audio: &mut AudioLayout,
) -> Result<(), AudioError> {
    let end = start
        .checked_add(size)
        .ok_or(AudioError::InvalidFormat("wavl LIST size overflow"))?;
    loop {
        let position = reader.stream_position()?;
        if position == end {
            break;
        }
        if position > end || end - position < 8 {
            return Err(AudioError::InvalidFormat("truncated wavl subchunk header"));
        }
        let mut header = [0; 8];
        reader.read_exact(&mut header)?;
        let id: [u8; 4] = header[..4].try_into().unwrap();
        let size_32 = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let chunk_size = if size_32 != u32::MAX {
            u64::from(size_32)
        } else if container == WavContainer::Rf64 {
            let info = ds64
                .as_mut()
                .ok_or(AudioError::InvalidFormat("missing RF64 ds64 sizes"))?;
            if id == *b"data" && !audio.data_chunk_seen {
                info.data_size
            } else {
                info.take_table_size(&id).ok_or(AudioError::InvalidFormat(
                    "RF64 wavl sentinel has no ds64 table entry",
                ))?
            }
        } else {
            return Err(AudioError::InvalidFormat(
                "RIFF wavl chunk uses a size sentinel without RF64 ds64",
            ));
        };
        if id == *b"data" {
            audio.data_chunk_seen = true;
        }
        let data_start = reader.stream_position()?;
        let chunk_end = data_start
            .checked_add(chunk_size)
            .ok_or(AudioError::InvalidFormat("wavl subchunk size overflow"))?;
        let padded_end = chunk_end
            .checked_add(chunk_size & 1)
            .ok_or(AudioError::InvalidFormat("wavl subchunk padding overflow"))?;
        if padded_end > end {
            return Err(AudioError::InvalidFormat(
                "wavl subchunk extends past its LIST boundary",
            ));
        }
        match &id {
            b"data" => {
                if chunk_size % u64::from(frame_bytes) != 0 {
                    return Err(AudioError::InvalidFormat(
                        "wavl data chunk ends in a partial sample frame",
                    ));
                }
                audio.push(AudioSegment::Data {
                    next_offset: data_start,
                    remaining_frames: chunk_size / u64::from(frame_bytes),
                })?;
            }
            b"slnt" => {
                if chunk_size != 4 {
                    return Err(AudioError::InvalidFormat(
                        "wavl slnt chunk must contain one sample count",
                    ));
                }
                let mut bytes = [0; 4];
                reader.read_exact(&mut bytes)?;
                audio.push(AudioSegment::Silence {
                    remaining_frames: u64::from(u32::from_le_bytes(bytes)),
                })?;
            }
            _ => {}
        }
        reader.seek(SeekFrom::Start(padded_end))?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum WaveEncoding {
    Pcm(SampleFormat),
    ALaw,
    MuLaw,
}

pub struct WavDecoder<R> {
    reader: R,
    spec: AudioSpec,
    container: WavContainer,
    metadata: Metadata,
    frame_bytes: u16,
    segments: VecDeque<AudioSegment>,
    remaining_frames: u64,
    last_frame: Option<Vec<u8>>,
    encoding: WaveEncoding,
}

impl<R: Read + Seek> WavDecoder<R> {
    pub fn new(mut reader: R) -> Result<Self, AudioError> {
        reader.seek(SeekFrom::Start(0))?;
        let mut header = [0; 12];
        reader.read_exact(&mut header)?;
        let container = match &header[..4] {
            b"RIFF" => WavContainer::Riff,
            b"RF64" => WavContainer::Rf64,
            _ => {
                return Err(AudioError::InvalidFormat(
                    "expected a little-endian RIFF/WAVE or RF64/WAVE file",
                ));
            }
        };
        if &header[8..12] != b"WAVE" {
            return Err(AudioError::InvalidFormat("expected a WAVE file form"));
        }

        let riff_size_32 = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let mut ds64 = None;
        if container == WavContainer::Rf64 {
            if riff_size_32 != u32::MAX {
                return Err(AudioError::InvalidFormat(
                    "RF64 header must use the 32-bit size sentinel",
                ));
            }
            let mut chunk_header = [0; 8];
            reader.read_exact(&mut chunk_header)?;
            if &chunk_header[..4] != b"ds64" {
                return Err(AudioError::InvalidFormat(
                    "RF64 requires ds64 as the first chunk",
                ));
            }
            let chunk_size = u32::from_le_bytes(chunk_header[4..8].try_into().unwrap());
            if !(28..=MAX_DS64_CHUNK).contains(&chunk_size) {
                return Err(AudioError::InvalidFormat("invalid RF64 ds64 chunk size"));
            }
            let mut bytes = vec![0; chunk_size as usize];
            reader.read_exact(&mut bytes)?;
            ds64 = Some(parse_ds64(&bytes)?);
            if chunk_size & 1 != 0 {
                reader.seek(SeekFrom::Current(1))?;
            }
        }
        let chunk_scan_start = reader.stream_position()?;
        let riff_size = if riff_size_32 == u32::MAX {
            ds64.as_ref()
                .ok_or(AudioError::InvalidFormat("RIFF size sentinel without ds64"))?
                .riff_size
        } else {
            u64::from(riff_size_32)
        };
        let riff_end = 8_u64
            .checked_add(riff_size)
            .ok_or(AudioError::InvalidFormat("RIFF size overflow"))?;
        let file_end = reader.seek(SeekFrom::End(0))?;
        if riff_end > file_end || riff_end < 12 {
            return Err(AudioError::InvalidFormat("RIFF size exceeds the file"));
        }
        reader.seek(SeekFrom::Start(chunk_scan_start))?;

        let mut parsed_spec = None;
        let mut frame_bytes = None;
        let mut encoding = None;
        let mut audio = AudioLayout::default();
        let mut has_audio = false;
        let mut metadata = Metadata::default();
        loop {
            let position = reader.stream_position()?;
            if position == riff_end {
                break;
            }
            if position > riff_end || riff_end - position < 8 {
                return Err(AudioError::InvalidFormat("truncated RIFF chunk header"));
            }
            let mut chunk_header = [0; 8];
            reader.read_exact(&mut chunk_header)?;
            let chunk_size_32 = u32::from_le_bytes(chunk_header[4..8].try_into().unwrap());
            let chunk_size = if chunk_size_32 != u32::MAX {
                u64::from(chunk_size_32)
            } else if container == WavContainer::Rf64 {
                if &chunk_header[..4] == b"data" && !audio.data_chunk_seen {
                    ds64.as_ref()
                        .ok_or(AudioError::InvalidFormat("missing RF64 ds64 sizes"))?
                        .data_size
                } else {
                    ds64.as_mut()
                        .and_then(|info| info.take_table_size(&chunk_header[..4]))
                        .ok_or(AudioError::InvalidFormat(
                            "RF64 chunk size sentinel has no ds64 table entry",
                        ))?
                }
            } else {
                return Err(AudioError::InvalidFormat(
                    "RIFF chunk size sentinel without RF64 ds64",
                ));
            };
            if &chunk_header[..4] == b"data" {
                audio.data_chunk_seen = true;
            }
            let chunk_start = reader.stream_position()?;
            let chunk_end = chunk_start
                .checked_add(chunk_size)
                .ok_or(AudioError::InvalidFormat("chunk size overflow"))?;
            let padded_end = chunk_end
                .checked_add(chunk_size & 1)
                .ok_or(AudioError::InvalidFormat("chunk padding overflow"))?;
            if padded_end > riff_end {
                return Err(AudioError::InvalidFormat(
                    "chunk extends past the RIFF boundary",
                ));
            }

            match &chunk_header[..4] {
                b"fmt " if parsed_spec.is_none() => {
                    if chunk_size > u64::from(MAX_FORMAT_CHUNK) {
                        return Err(AudioError::InvalidFormat(
                            "format chunk is unreasonably large",
                        ));
                    }
                    let mut bytes = vec![0; chunk_size as usize];
                    reader.read_exact(&mut bytes)?;
                    let (spec, align, parsed_encoding) = parse_format(&bytes)?;
                    parsed_spec = Some(spec);
                    frame_bytes = Some(align);
                    encoding = Some(parsed_encoding);
                }
                b"data" if parsed_spec.is_none() => {
                    return Err(AudioError::InvalidFormat("fmt chunk must precede data"));
                }
                b"data" => {
                    let frame_bytes = frame_bytes
                        .ok_or(AudioError::InvalidFormat("fmt chunk must precede data"))?;
                    if chunk_size % u64::from(frame_bytes) != 0 {
                        return Err(AudioError::InvalidFormat(
                            "data chunk ends in a partial sample frame",
                        ));
                    }
                    audio.push(AudioSegment::Data {
                        next_offset: chunk_start,
                        remaining_frames: chunk_size / u64::from(frame_bytes),
                    })?;
                    has_audio = true;
                }
                b"LIST" if chunk_size >= 4 => {
                    let mut list_type = [0; 4];
                    reader.read_exact(&mut list_type)?;
                    match &list_type {
                        b"INFO" if chunk_size <= u64::from(u32::MAX) => {
                            reader.seek(SeekFrom::Start(chunk_start))?;
                            parse_list_info(&mut reader, chunk_size as u32, &mut metadata)?;
                        }
                        b"wavl" => {
                            let frame_bytes = frame_bytes.ok_or(AudioError::InvalidFormat(
                                "fmt chunk must precede wavl audio",
                            ))?;
                            let wavl_start = reader.stream_position()?;
                            parse_wavl(
                                &mut reader,
                                wavl_start,
                                chunk_size - 4,
                                frame_bytes,
                                container,
                                &mut ds64,
                                &mut audio,
                            )?;
                            has_audio = true;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            reader.seek(SeekFrom::Start(padded_end))?;
        }

        let spec = parsed_spec.ok_or(AudioError::InvalidFormat("missing fmt chunk"))?;
        let frame_bytes =
            frame_bytes.ok_or(AudioError::InvalidFormat("missing frame alignment"))?;
        let encoding = encoding.ok_or(AudioError::InvalidFormat("missing WAVE encoding"))?;
        if !has_audio {
            return Err(AudioError::InvalidFormat("missing data or wavl audio"));
        }

        Ok(Self {
            reader,
            spec,
            container,
            metadata,
            frame_bytes,
            segments: audio.segments,
            remaining_frames: audio.remaining_frames,
            last_frame: None,
            encoding,
        })
    }

    pub fn container(&self) -> WavContainer {
        self.container
    }
}

impl<R: Read + Seek> AudioDecoder for WavDecoder<R> {
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
        let frames = self.remaining_frames.min(max_frames as u64);
        if frames == 0 {
            return Ok(None);
        }
        let frames = usize::try_from(frames)
            .map_err(|_| AudioError::InvalidData("requested block size overflow"))?;
        let byte_count = frames
            .checked_mul(usize::from(self.frame_bytes))
            .ok_or(AudioError::InvalidData("requested block size overflow"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|_| AudioError::InvalidData("audio block is too large"))?;
        while bytes.len() < byte_count {
            let Some(segment) = self.segments.front_mut() else {
                break;
            };
            match segment {
                AudioSegment::Data {
                    next_offset,
                    remaining_frames,
                } => {
                    if *remaining_frames == 0 {
                        self.segments.pop_front();
                        continue;
                    }
                    let frames_left = (byte_count - bytes.len()) / usize::from(self.frame_bytes);
                    let take_frames = (*remaining_frames).min(frames_left as u64);
                    let take_bytes = usize::try_from(take_frames)
                        .ok()
                        .and_then(|count| count.checked_mul(usize::from(self.frame_bytes)))
                        .ok_or(AudioError::InvalidData("requested block size overflow"))?;
                    self.reader.seek(SeekFrom::Start(*next_offset))?;
                    let start = bytes.len();
                    bytes.resize(start + take_bytes, 0);
                    self.reader.read_exact(&mut bytes[start..])?;
                    *next_offset = next_offset
                        .checked_add(take_bytes as u64)
                        .ok_or(AudioError::InvalidData("WAVE data offset overflow"))?;
                    *remaining_frames -= take_frames;
                    let frame_bytes = usize::from(self.frame_bytes);
                    self.last_frame = Some(bytes[bytes.len() - frame_bytes..].to_vec());
                }
                AudioSegment::Silence { remaining_frames } => {
                    if *remaining_frames == 0 {
                        self.segments.pop_front();
                        continue;
                    }
                    let frame_bytes = usize::from(self.frame_bytes);
                    let frames_left = (byte_count - bytes.len()) / frame_bytes;
                    let take_frames = (*remaining_frames).min(frames_left as u64);
                    let silence_frame = self.last_frame.get_or_insert_with(|| {
                        initial_silence_frame(self.frame_bytes, self.encoding)
                    });
                    let take_bytes = usize::try_from(take_frames)
                        .ok()
                        .and_then(|count| count.checked_mul(frame_bytes))
                        .ok_or(AudioError::InvalidData("requested block size overflow"))?;
                    let start = bytes.len();
                    bytes.resize(start + take_bytes, 0);
                    for frame in bytes[start..].chunks_exact_mut(frame_bytes) {
                        frame.copy_from_slice(silence_frame);
                    }
                    *remaining_frames -= take_frames;
                }
            }
            let exhausted = match self.segments.front() {
                Some(AudioSegment::Data {
                    remaining_frames, ..
                })
                | Some(AudioSegment::Silence { remaining_frames }) => *remaining_frames == 0,
                None => false,
            };
            if exhausted {
                self.segments.pop_front();
            }
        }

        let decoded_frames = u64::try_from(bytes.len() / usize::from(self.frame_bytes))
            .map_err(|_| AudioError::InvalidData("decoded frame count overflow"))?;
        self.remaining_frames = self
            .remaining_frames
            .checked_sub(decoded_frames)
            .ok_or(AudioError::InvalidData("WAVE sample count underflow"))?;
        let samples = decode_samples(self.encoding, bytes)?;
        PcmBuffer::new(self.spec, samples)
            .map(Some)
            .map_err(AudioError::from)
    }
}

fn decode_samples(encoding: WaveEncoding, bytes: Vec<u8>) -> Result<Samples, AudioError> {
    let format = match encoding {
        WaveEncoding::Pcm(format) => format,
        WaveEncoding::ALaw => {
            let mut output = reserve_samples(bytes.len())?;
            output.extend(bytes.into_iter().map(decode_alaw));
            return Ok(Samples::I16(output));
        }
        WaveEncoding::MuLaw => {
            let mut output = reserve_samples(bytes.len())?;
            output.extend(bytes.into_iter().map(decode_mulaw));
            return Ok(Samples::I16(output));
        }
    };
    let sample_count = match format {
        SampleFormat::PcmU8 => return Ok(Samples::U8(bytes)),
        SampleFormat::PcmI16 => bytes.len() / 2,
        SampleFormat::PcmI24 => bytes.len() / 3,
        SampleFormat::PcmI32 | SampleFormat::Float32 => bytes.len() / 4,
        SampleFormat::Float64 => bytes.len() / 8,
    };
    Ok(match format {
        SampleFormat::PcmU8 => unreachable!(),
        SampleFormat::PcmI16 => {
            let mut output: Vec<i16> = reserve_samples(sample_count)?;
            for sample in bytes.chunks_exact(2) {
                output.push(i16::from_le_bytes([sample[0], sample[1]]));
            }
            Samples::I16(output)
        }
        SampleFormat::PcmI24 => {
            let mut output: Vec<i32> = reserve_samples(sample_count)?;
            for sample in bytes.chunks_exact(3) {
                let value = i32::from(sample[0])
                    | (i32::from(sample[1]) << 8)
                    | (i32::from(sample[2]) << 16);
                output.push((value << 8) >> 8);
            }
            Samples::I24(output)
        }
        SampleFormat::PcmI32 => {
            let mut output: Vec<i32> = reserve_samples(sample_count)?;
            for sample in bytes.chunks_exact(4) {
                output.push(i32::from_le_bytes(sample.try_into().unwrap()));
            }
            Samples::I32(output)
        }
        SampleFormat::Float32 => {
            let mut output: Vec<f32> = reserve_samples(sample_count)?;
            for sample in bytes.chunks_exact(4) {
                output.push(f32::from_le_bytes(sample.try_into().unwrap()));
            }
            Samples::F32(output)
        }
        SampleFormat::Float64 => {
            let mut output: Vec<f64> = reserve_samples(sample_count)?;
            for sample in bytes.chunks_exact(8) {
                output.push(f64::from_le_bytes(sample.try_into().unwrap()));
            }
            Samples::F64(output)
        }
    })
}

fn initial_silence_frame(frame_bytes: u16, encoding: WaveEncoding) -> Vec<u8> {
    let fill = match encoding {
        WaveEncoding::Pcm(SampleFormat::PcmU8) => 0x80,
        WaveEncoding::ALaw => 0xd5,
        WaveEncoding::MuLaw => 0xff,
        _ => 0,
    };
    vec![fill; usize::from(frame_bytes)]
}

fn reserve_samples<T>(count: usize) -> Result<Vec<T>, AudioError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|_| AudioError::InvalidData("decoded audio block is too large"))?;
    Ok(output)
}

fn parse_format(bytes: &[u8]) -> Result<(AudioSpec, u16, WaveEncoding), AudioError> {
    if bytes.len() < 16 {
        return Err(AudioError::InvalidFormat(
            "fmt chunk is shorter than WAVEFORMAT",
        ));
    }
    let mut tag = u16::from_le_bytes(bytes[0..2].try_into().unwrap());
    let channels = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
    let sample_rate = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let byte_rate = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let block_align = u16::from_le_bytes(bytes[12..14].try_into().unwrap());
    let bits = u16::from_le_bytes(bytes[14..16].try_into().unwrap());
    let mut channel_mask = None;

    if tag == 0xfffe {
        if bytes.len() < 40 || u16::from_le_bytes(bytes[16..18].try_into().unwrap()) < 22 {
            return Err(AudioError::InvalidFormat(
                "incomplete WAVE_FORMAT_EXTENSIBLE format chunk",
            ));
        }
        channel_mask = Some(u32::from_le_bytes(bytes[20..24].try_into().unwrap()));
        let valid_bits = u16::from_le_bytes(bytes[18..20].try_into().unwrap());
        let subformat: [u8; 16] = bytes[24..40].try_into().unwrap();
        tag = if subformat == PCM_GUID {
            if valid_bits == 0 || valid_bits > bits {
                return Err(AudioError::InvalidFormat(
                    "extensible PCM valid bits must be between 1 and the container width",
                ));
            }
            1
        } else if subformat == FLOAT_GUID {
            if valid_bits != bits {
                return Err(AudioError::Unsupported(
                    "extensible float valid bits differ from its container",
                ));
            }
            3
        } else if subformat == ALAW_GUID {
            if valid_bits != bits {
                return Err(AudioError::Unsupported(
                    "extensible A-law valid bits differ from its container",
                ));
            }
            6
        } else if subformat == MULAW_GUID {
            if valid_bits != bits {
                return Err(AudioError::Unsupported(
                    "extensible μ-law valid bits differ from its container",
                ));
            }
            7
        } else {
            return Err(AudioError::Unsupported("WAVE_FORMAT_EXTENSIBLE subformat"));
        };
    }

    let (sample_format, encoding) = match (tag, bits) {
        (1, 8) => (SampleFormat::PcmU8, WaveEncoding::Pcm(SampleFormat::PcmU8)),
        (1, 16) => (
            SampleFormat::PcmI16,
            WaveEncoding::Pcm(SampleFormat::PcmI16),
        ),
        (1, 24) => (
            SampleFormat::PcmI24,
            WaveEncoding::Pcm(SampleFormat::PcmI24),
        ),
        (1, 32) => (
            SampleFormat::PcmI32,
            WaveEncoding::Pcm(SampleFormat::PcmI32),
        ),
        (3, 32) => (
            SampleFormat::Float32,
            WaveEncoding::Pcm(SampleFormat::Float32),
        ),
        (3, 64) => (
            SampleFormat::Float64,
            WaveEncoding::Pcm(SampleFormat::Float64),
        ),
        (6, 8) => (SampleFormat::PcmI16, WaveEncoding::ALaw),
        (7, 8) => (SampleFormat::PcmI16, WaveEncoding::MuLaw),
        (1, _) | (3, _) | (6, _) | (7, _) => {
            return Err(AudioError::Unsupported("WAVE encoding bit depth"));
        }
        _ => return Err(AudioError::Unsupported("WAVE encoding")),
    };
    let bytes_per_sample = bits / 8;
    let expected_align = channels
        .checked_mul(bytes_per_sample)
        .ok_or(AudioError::InvalidFormat("block alignment overflow"))?;
    let expected_rate = u64::from(sample_rate) * u64::from(expected_align);
    if block_align != expected_align || u64::from(byte_rate) != expected_rate {
        return Err(AudioError::InvalidFormat(
            "inconsistent WAVE byte rate or block alignment",
        ));
    }
    Ok((
        AudioSpec::new_with_speaker_mask(sample_rate, channels, sample_format, channel_mask)?,
        block_align,
        encoding,
    ))
}

fn parse_list_info<R: Read + Seek>(
    reader: &mut R,
    chunk_size: u32,
    metadata: &mut Metadata,
) -> Result<(), AudioError> {
    let mut list_type = [0; 4];
    reader.read_exact(&mut list_type)?;
    if &list_type != b"INFO" {
        return Ok(());
    }
    let mut remaining = chunk_size - 4;
    while remaining >= 8 {
        let mut header = [0; 8];
        reader.read_exact(&mut header)?;
        let size = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let padded = u64::from(size) + u64::from(size & 1);
        if padded > u64::from(remaining - 8) {
            return Err(AudioError::InvalidFormat(
                "INFO subchunk exceeds LIST boundary",
            ));
        }
        let key = match &header[..4] {
            b"INAM" => Some("TITLE"),
            b"IART" => Some("ARTIST"),
            b"IPRD" => Some("ALBUM"),
            b"ICRD" => Some("DATE"),
            b"IGNR" => Some("GENRE"),
            b"ICMT" => Some("COMMENT"),
            _ => None,
        };
        if let Some(key) = key {
            if size > MAX_INFO_VALUE {
                return Err(AudioError::InvalidFormat(
                    "INFO metadata value is unreasonably large",
                ));
            }
            let mut value = vec![0; size as usize];
            reader.read_exact(&mut value)?;
            let value = String::from_utf8_lossy(&value)
                .trim_end_matches('\0')
                .trim()
                .to_owned();
            if !value.is_empty() {
                metadata.insert(key, value);
            }
        } else {
            reader.seek(SeekFrom::Current(i64::from(size)))?;
        }
        if size & 1 != 0 {
            reader.seek(SeekFrom::Current(1))?;
        }
        remaining -= 8 + padded as u32;
    }
    if remaining != 0 {
        return Err(AudioError::InvalidFormat("trailing bytes in INFO list"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::write_wav;
    use zemer_core::{AudioEncoder, transcode};

    fn round_trip(spec: AudioSpec, samples: Samples) {
        let input = PcmBuffer::new(spec, samples).unwrap();
        let mut encoded = Cursor::new(Vec::new());
        write_wav(&mut encoded, &input).unwrap();
        let mut decoder = WavDecoder::new(Cursor::new(encoded.into_inner())).unwrap();
        assert_eq!(decoder.spec(), spec);
        assert_eq!(decoder.read_frames(input.frames()).unwrap().unwrap(), input);
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    fn append_chunk(output: &mut Vec<u8>, id: &[u8; 4], data: &[u8]) {
        output.extend_from_slice(id);
        output.extend_from_slice(&(data.len() as u32).to_le_bytes());
        output.extend_from_slice(data);
        if data.len() & 1 != 0 {
            output.push(0);
        }
    }

    fn wavl_pcm(channels: u16, bits: u16, segments: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let bytes_per_sample = bits / 8;
        let frame_bytes = channels * bytes_per_sample;
        let rate = 48_000_u32;
        let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1_u16.to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&rate.to_le_bytes());
        fmt.extend_from_slice(&(rate * u32::from(frame_bytes)).to_le_bytes());
        fmt.extend_from_slice(&frame_bytes.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        append_chunk(&mut bytes, b"fmt ", &fmt);

        let mut wavl = b"wavl".to_vec();
        for (id, data) in segments {
            append_chunk(&mut wavl, id, data);
        }
        append_chunk(&mut bytes, b"LIST", &wavl);
        let riff_size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
        bytes
    }

    #[test]
    fn decodes_wavl_data_and_slnt_in_order_across_read_boundaries() {
        let bytes = wavl_pcm(
            1,
            8,
            &[
                (b"data", vec![0x10, 0x20]),
                (b"slnt", 2_u32.to_le_bytes().to_vec()),
                (b"data", vec![0x30]),
            ],
        );
        let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::U8(vec![0x10, 0x20])
        );
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::U8(vec![0x20, 0x20])
        );
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::U8(vec![0x30])
        );
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn wavl_slnt_counts_multichannel_frames_and_uses_initial_pcm_silence() {
        let bytes = wavl_pcm(
            2,
            16,
            &[
                (b"slnt", 1_u32.to_le_bytes().to_vec()),
                (b"data", vec![1, 0, 2, 0]),
                (b"slnt", 1_u32.to_le_bytes().to_vec()),
            ],
        );
        let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            decoder.read_frames(3).unwrap().unwrap().samples(),
            &Samples::I16(vec![0, 0, 1, 2, 1, 2])
        );
        assert!(decoder.read_frames(1).unwrap().is_none());

        let bytes = wavl_pcm(1, 8, &[(b"slnt", 2_u32.to_le_bytes().to_vec())]);
        let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::U8(vec![128, 128])
        );
    }

    #[test]
    fn rejects_malformed_wavl_silence_and_partial_frames() {
        let malformed_slnt = wavl_pcm(1, 8, &[(b"slnt", vec![0, 0, 0])]);
        assert!(WavDecoder::new(Cursor::new(malformed_slnt)).is_err());

        let partial_stereo_frame = wavl_pcm(2, 16, &[(b"data", vec![1, 0])]);
        assert!(WavDecoder::new(Cursor::new(partial_stereo_frame)).is_err());
    }

    #[test]
    fn resolves_rf64_size_sentinel_for_data_inside_wavl() {
        let mut bytes = b"RF64\xff\xff\xff\xffWAVE".to_vec();
        let mut ds64 = Vec::new();
        ds64.extend_from_slice(&0_u64.to_le_bytes());
        ds64.extend_from_slice(&2_u64.to_le_bytes());
        ds64.extend_from_slice(&2_u64.to_le_bytes());
        ds64.extend_from_slice(&0_u32.to_le_bytes());
        append_chunk(&mut bytes, b"ds64", &ds64);
        bytes.extend_from_slice(b"fmt \x10\0\0\0\x01\0\x01\0\x80\xbb\0\0\x80\xbb\0\0\x01\0\x08\0");
        let mut wavl = b"wavl".to_vec();
        wavl.extend_from_slice(b"data\xff\xff\xff\xff\x34\x56");
        append_chunk(&mut bytes, b"LIST", &wavl);
        let riff_size = (bytes.len() - 8) as u64;
        bytes[20..28].copy_from_slice(&riff_size.to_le_bytes());

        let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::U8(vec![0x34, 0x56])
        );
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn round_trips_supported_integer_and_float_formats() {
        round_trip(
            AudioSpec::new(48_000, 2, SampleFormat::PcmU8).unwrap(),
            Samples::U8(vec![0, 255]),
        );
        round_trip(
            AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap(),
            Samples::I16(vec![-32768, 32767]),
        );
        round_trip(
            AudioSpec::new(48_000, 2, SampleFormat::PcmI24).unwrap(),
            Samples::I24(vec![-8_388_608, 8_388_607]),
        );
        round_trip(
            AudioSpec::new(48_000, 2, SampleFormat::PcmI32).unwrap(),
            Samples::I32(vec![i32::MIN, i32::MAX]),
        );
        round_trip(
            AudioSpec::new(48_000, 2, SampleFormat::Float32).unwrap(),
            Samples::F32(vec![-1.0, 0.5]),
        );
        round_trip(
            AudioSpec::new(48_000, 2, SampleFormat::Float64).unwrap(),
            Samples::F64(vec![-1.0, 0.5]),
        );
        round_trip(
            AudioSpec::new(44_100, 1, SampleFormat::PcmI24).unwrap(),
            Samples::I24(vec![-8_388_608, 0, 8_388_607]),
        );
        round_trip(
            AudioSpec::new(44_100, 1, SampleFormat::Float32).unwrap(),
            Samples::F32(vec![-1.0, 0.0, 1.0]),
        );
    }

    #[test]
    fn decodes_float_from_explicit_little_endian_sample_bytes() {
        let mut bytes = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x03\0\x01\0\x40\x1f\0\0\x00\x7d\0\0\x04\0\x20\0data\x04\0\0\0".to_vec();
        bytes.extend_from_slice(&[0x00, 0x00, 0x80, 0x3f]);
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());

        let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            decoder.read_frames(1).unwrap().unwrap().samples(),
            &Samples::F32(vec![1.0])
        );
    }

    #[test]
    fn decodes_g711_alaw_and_mulaw_vectors() {
        assert_eq!(decode_alaw(0xd5), 8);
        assert_eq!(decode_alaw(0x55), -8);
        assert_eq!(decode_mulaw(0xff), 0);
        assert_eq!(decode_mulaw(0x00), -32_124);
        assert_eq!(decode_mulaw(0x80), 32_124);
    }

    fn extensible_pcm(valid_bits: u16, bits: u16, data: &[u8]) -> Vec<u8> {
        let bytes_per_sample = bits / 8;
        let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&40_u32.to_le_bytes());
        bytes.extend_from_slice(&0xfffe_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&48_000_u32.to_le_bytes());
        bytes.extend_from_slice(&(48_000_u32 * u32::from(bytes_per_sample)).to_le_bytes());
        bytes.extend_from_slice(&bytes_per_sample.to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(&22_u16.to_le_bytes());
        bytes.extend_from_slice(&valid_bits.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&PCM_GUID);
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    #[test]
    fn reads_info_metadata_and_odd_sized_data_chunk() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        bytes.extend_from_slice(b"JUNK\x03\0\0\0abc\0");
        bytes.extend_from_slice(b"fmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x40\x1f\0\0\x01\0\x08\0");
        bytes.extend_from_slice(b"LIST\x10\0\0\0INFOINAM\x04\0\0\0Test");
        bytes.extend_from_slice(b"data\x01\0\0\0\x80\0");
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());

        let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.metadata().values("title"), ["Test"]);
        let block = decoder.read_frames(4).unwrap().unwrap();
        assert_eq!(block.samples(), &Samples::U8(vec![128]));
    }

    #[test]
    fn writer_pads_odd_data_chunks_and_counts_padding_in_riff_size() {
        let audio = PcmBuffer::new(
            AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap(),
            Samples::U8(vec![128]),
        )
        .unwrap();
        let encoded = write_wav(Cursor::new(Vec::new()), &audio)
            .unwrap()
            .into_inner();
        assert_eq!(encoded.len(), 46);
        assert_eq!(u32::from_le_bytes(encoded[4..8].try_into().unwrap()), 38);
        assert_eq!(u32::from_le_bytes(encoded[40..44].try_into().unwrap()), 1);
        assert_eq!(&encoded[44..], &[128, 0]);
    }

    #[test]
    fn rejects_truncated_riff_and_non_frame_aligned_data() {
        assert!(WavDecoder::new(Cursor::new(b"RIFF\0\0\0\0WAVE".to_vec())).is_err());
        let spec = AudioSpec::new(8_000, 2, SampleFormat::PcmI16).unwrap();
        let invalid = PcmBuffer::new(spec, Samples::I16(vec![1])).unwrap_err();
        assert_eq!(invalid.0, "sample count is not a whole number of frames");
    }

    #[test]
    fn streams_multiple_blocks_and_handles_extensible_pcm() {
        let audio = PcmBuffer::new(
            AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap(),
            Samples::I16(vec![1, 2, 3]),
        )
        .unwrap();
        let mut encoded = Cursor::new(Vec::new());
        write_wav(&mut encoded, &audio).unwrap();
        let mut decoder = WavDecoder::new(Cursor::new(encoded.into_inner())).unwrap();
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::I16(vec![1, 2])
        );
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::I16(vec![3])
        );
        assert!(decoder.read_frames(2).unwrap().is_none());

        let mut extensible = Vec::new();
        extensible.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt \x28\0\0\0\xfe\xff\x02\0\x80\xbb\0\0\x00\x65\x04\0\x06\0\x18\0\x16\0\x18\0\x03\0\0\0");
        extensible.extend_from_slice(&PCM_GUID);
        extensible.extend_from_slice(b"data\x06\0\0\0\0\0\x80\xff\xff\x7f");
        let size = (extensible.len() - 8) as u32;
        extensible[4..8].copy_from_slice(&size.to_le_bytes());
        let mut decoder = WavDecoder::new(Cursor::new(extensible)).unwrap();
        let block = decoder.read_frames(2).unwrap().unwrap();
        assert_eq!(block.samples(), &Samples::I24(vec![-8_388_608, 8_388_607]));
    }

    #[test]
    fn preserves_left_aligned_extensible_pcm_in_container_precision() {
        let bytes = extensible_pcm(12, 16, &[0x00, 0x80, 0xf0, 0x7f]);
        let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::I16(vec![i16::MIN, 32_752])
        );
    }

    #[test]
    fn preserves_the_extensible_channel_mask_without_guessing_layout() {
        let mut bytes = extensible_pcm(16, 16, &[0, 0]);
        bytes[40..44].copy_from_slice(&0x3_u32.to_le_bytes());
        let decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.spec().speaker_mask(), Some(0x3));
    }

    #[test]
    fn rejects_invalid_extensible_pcm_valid_bit_counts() {
        for valid_bits in [0_u16, 17] {
            let bytes = extensible_pcm(valid_bits, 16, &[0, 0]);
            assert!(WavDecoder::new(Cursor::new(bytes)).is_err());
        }
    }

    #[test]
    fn rejects_data_that_ends_mid_frame() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        bytes.extend_from_slice(b"fmt \x10\0\0\0\x01\0\x02\0\x40\x1f\0\0\x00\x7d\0\0\x04\0\x10\0");
        bytes.extend_from_slice(b"data\x02\0\0\0\0\0");
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        assert!(WavDecoder::new(Cursor::new(bytes)).is_err());
    }

    #[test]
    fn rejects_data_that_ends_mid_sample() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        bytes.extend_from_slice(b"fmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0");
        bytes.extend_from_slice(b"data\x01\0\0\0\0\0");
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        assert!(WavDecoder::new(Cursor::new(bytes)).is_err());
    }

    #[test]
    fn rejects_zero_bit_depth_without_panicking() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        bytes.extend_from_slice(b"fmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x40\x1f\0\0\x01\0\0\0");
        bytes.extend_from_slice(b"data\0\0\0\0");
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        assert!(WavDecoder::new(Cursor::new(bytes)).is_err());
    }

    #[test]
    fn rf64_requires_a_valid_ds64_chunk_and_resolves_table_sized_chunks() {
        let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
        let audio = PcmBuffer::new(spec, Samples::U8(vec![128, 255])).unwrap();
        let mut writer =
            crate::WavWriter::new_with_container(Cursor::new(Vec::new()), spec, WavContainer::Rf64)
                .unwrap();
        writer.write_block(&audio).unwrap();
        let original = writer.finish().unwrap().into_inner();

        let mut missing_ds64 = original.clone();
        missing_ds64[12..16].copy_from_slice(b"fmt ");
        assert!(WavDecoder::new(Cursor::new(missing_ds64)).is_err());

        let mut invalid_table = original.clone();
        invalid_table[44..48].copy_from_slice(&1_u32.to_le_bytes());
        assert!(WavDecoder::new(Cursor::new(invalid_table)).is_err());

        let mut with_large_unknown_chunk = original[..48].to_vec();
        with_large_unknown_chunk[16..20].copy_from_slice(&40_u32.to_le_bytes());
        with_large_unknown_chunk[44..48].copy_from_slice(&1_u32.to_le_bytes());
        with_large_unknown_chunk.extend_from_slice(b"JUNK");
        with_large_unknown_chunk.extend_from_slice(&3_u64.to_le_bytes());
        with_large_unknown_chunk.extend_from_slice(b"JUNK\xff\xff\xff\xffabc\0");
        with_large_unknown_chunk.extend_from_slice(&original[48..]);
        let riff_size = (with_large_unknown_chunk.len() - 8) as u64;
        with_large_unknown_chunk[20..28].copy_from_slice(&riff_size.to_le_bytes());

        let mut decoder = WavDecoder::new(Cursor::new(with_large_unknown_chunk)).unwrap();
        assert_eq!(decoder.read_frames(2).unwrap().unwrap(), audio);
    }

    #[test]
    fn transcodes_streamed_blocks_without_loading_the_whole_file() {
        let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI24).unwrap();
        let expected = PcmBuffer::new(spec, Samples::I24(vec![-5, 0, 5, 8_388_607])).unwrap();
        let input = write_wav(Cursor::new(Vec::new()), &expected)
            .unwrap()
            .into_inner();
        let mut decoder = WavDecoder::new(Cursor::new(input)).unwrap();
        let encoder = crate::WavWriter::new(Cursor::new(Vec::new()), spec).unwrap();
        let mut encoder = encoder;
        assert_eq!(transcode(&mut decoder, &mut encoder, 2).unwrap(), 4);
        let output = encoder.finish().unwrap().into_inner();
        let mut decoded = WavDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoded.read_frames(8).unwrap().unwrap(), expected);
    }
}
