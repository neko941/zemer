use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Seek, SeekFrom},
};

use crate::encode::WavContainer;
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
const MAX_DS64_CHUNK: u32 = 1024 * 1024;

struct Ds64 {
    riff_size: u64,
    data_size: u64,
    table: HashMap<[u8; 4], VecDeque<u64>>,
}

impl Ds64 {
    fn take_table_size(&mut self, chunk_id: &[u8]) -> Option<u64> {
        let id: [u8; 4] = chunk_id.try_into().ok()?;
        let sizes = self.table.get_mut(&id)?;
        let size = sizes.pop_front();
        if sizes.is_empty() {
            self.table.remove(&id);
        }
        size
    }
}

fn parse_ds64(bytes: &[u8]) -> Result<Ds64, AudioError> {
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
    data_remaining: u64,
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
        let mut data_chunk = None;
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
                if &chunk_header[..4] == b"data" && data_chunk.is_none() {
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
                b"data" if data_chunk.is_none() => data_chunk = Some((chunk_start, chunk_size)),
                b"LIST" if chunk_size >= 4 && chunk_size <= u64::from(u32::MAX) => {
                    parse_list_info(&mut reader, chunk_size as u32, &mut metadata)?
                }
                _ => {}
            }
            reader.seek(SeekFrom::Start(padded_end))?;
        }

        let spec = parsed_spec.ok_or(AudioError::InvalidFormat("missing fmt chunk"))?;
        let frame_bytes =
            frame_bytes.ok_or(AudioError::InvalidFormat("missing frame alignment"))?;
        let encoding = encoding.ok_or(AudioError::InvalidFormat("missing WAVE encoding"))?;
        let (data_start, data_size) =
            data_chunk.ok_or(AudioError::InvalidFormat("missing data chunk"))?;
        if data_size % u64::from(frame_bytes) != 0 {
            return Err(AudioError::InvalidFormat(
                "data chunk ends in a partial sample frame",
            ));
        }
        reader.seek(SeekFrom::Start(data_start))?;

        Ok(Self {
            reader,
            spec,
            container,
            metadata,
            frame_bytes,
            data_remaining: data_size,
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
        if self.data_remaining == 0 {
            return Ok(None);
        }

        let frames_left = self.data_remaining / u64::from(self.frame_bytes);
        let frames = frames_left.min(max_frames as u64) as usize;
        let byte_count = frames
            .checked_mul(usize::from(self.frame_bytes))
            .ok_or(AudioError::InvalidData("requested block size overflow"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|_| AudioError::InvalidData("audio block is too large"))?;
        bytes.resize(byte_count, 0);
        self.reader.read_exact(&mut bytes)?;
        self.data_remaining -= byte_count as u64;

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

fn decode_alaw(byte: u8) -> i16 {
    let value = byte ^ 0x55;
    let segment = i32::from((value & 0x70) >> 4);
    let mut sample = i32::from(value & 0x0f) << 4;
    sample = match segment {
        0 => sample + 8,
        1 => sample + 0x108,
        _ => (sample + 0x108) << (segment - 1),
    };
    if value & 0x80 != 0 {
        sample as i16
    } else {
        -(sample as i16)
    }
}

fn decode_mulaw(byte: u8) -> i16 {
    let value = !byte;
    let sample = ((i32::from(value & 0x0f) << 3) + 0x84) << ((value & 0x70) >> 4);
    if value & 0x80 != 0 {
        (sample - 0x84) as i16
    } else {
        (0x84 - sample) as i16
    }
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
        assert_eq!(decode_mulaw(0x00), 32_124);
        assert_eq!(decode_mulaw(0x80), -32_124);
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
