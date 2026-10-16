use std::io::{Read, Seek, SeekFrom};

use g711::{decode_alaw, decode_mulaw};
use ima_adpcm::{ImaAdpcmState, decode_ima4_packet};
use zemer_core::{AudioDecoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::info::{MAX_INFO_BYTES, parse_info};

const MAX_BLOCK_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CafCodec {
    Pcm,
    ALaw,
    MuLaw,
    Ima4,
}

#[derive(Clone, Copy)]
struct Description {
    spec: AudioSpec,
    codec: CafCodec,
    little_endian: bool,
    bytes_per_sample: usize,
    bytes_per_packet: u64,
}

pub struct CafDecoder<R> {
    reader: R,
    spec: AudioSpec,
    metadata: Metadata,
    codec: CafCodec,
    little_endian: bool,
    bytes_per_sample: usize,
    bytes_per_packet: u64,
    frames_left: u64,
    ima_states: Vec<Option<ImaAdpcmState>>,
    ima_pending: Vec<i16>,
    ima_pending_offset: usize,
    ima_priming_left: usize,
}

impl<R: Read + Seek> CafDecoder<R> {
    pub fn new(mut reader: R) -> Result<Self, AudioError> {
        reader.seek(SeekFrom::Start(0))?;
        let physical_end = reader.seek(SeekFrom::End(0))?;
        if physical_end < 8 {
            return Err(AudioError::InvalidFormat("CAF header is truncated"));
        }
        reader.seek(SeekFrom::Start(0))?;
        let mut header = [0; 8];
        reader.read_exact(&mut header)?;
        if &header[..4] != b"caff" || u16::from_be_bytes(header[4..6].try_into().unwrap()) != 1 {
            return Err(AudioError::InvalidFormat("expected CAF version 1"));
        }
        let mut desc = None;
        let mut metadata = Metadata::default();
        let mut metadata_seen = false;
        let mut channel_mask = None;
        let mut channel_layout_seen = false;
        let mut pakt = None;
        let mut data = None;
        let mut cursor = 8_u64;
        while cursor < physical_end {
            if physical_end - cursor < 12 {
                return Err(AudioError::InvalidFormat("CAF chunk header is truncated"));
            }
            reader.seek(SeekFrom::Start(cursor))?;
            let mut chunk_header = [0; 12];
            reader.read_exact(&mut chunk_header)?;
            let id = &chunk_header[..4];
            let size = i64::from_be_bytes(chunk_header[4..12].try_into().unwrap());
            let body = cursor + 12;
            if cursor == 8 && (id != b"desc" || size != 32) {
                return Err(AudioError::InvalidFormat(
                    "CAF desc chunk must immediately follow the file header",
                ));
            }
            if id == b"data" && size == -1 {
                if data.is_some() || physical_end - body < 4 {
                    return Err(AudioError::InvalidFormat("invalid CAF data chunk"));
                }
                if desc.is_none() {
                    return Err(AudioError::InvalidFormat("CAF data precedes desc"));
                }
                data = Some((body + 4, physical_end - body - 4));
                break;
            }
            let size = u64::try_from(size)
                .map_err(|_| AudioError::InvalidFormat("invalid negative CAF chunk size"))?;
            let end = body
                .checked_add(size)
                .filter(|&end| end <= physical_end)
                .ok_or(AudioError::InvalidFormat("CAF chunk extends past the file"))?;
            match id {
                b"desc" => {
                    if desc.is_some() || size != 32 || cursor != 8 {
                        return Err(AudioError::InvalidFormat(
                            "invalid or duplicate CAF desc chunk",
                        ));
                    }
                    let mut bytes = [0; 32];
                    reader.read_exact(&mut bytes)?;
                    desc = Some(parse_description(&bytes)?);
                }
                b"info" => {
                    if metadata_seen || size > MAX_INFO_BYTES {
                        return Err(AudioError::InvalidFormat(
                            "invalid or oversized CAF info chunk",
                        ));
                    }
                    let size = usize::try_from(size)
                        .map_err(|_| AudioError::InvalidFormat("CAF info chunk is too large"))?;
                    let mut bytes = Vec::new();
                    bytes
                        .try_reserve_exact(size)
                        .map_err(|_| AudioError::InvalidFormat("CAF info allocation failed"))?;
                    bytes.resize(size, 0);
                    reader.read_exact(&mut bytes)?;
                    metadata = parse_info(&bytes)?;
                    metadata_seen = true;
                }
                b"pakt" => {
                    if pakt.is_some() || size != 24 {
                        return Err(AudioError::InvalidFormat("invalid CAF packet table chunk"));
                    }
                    let mut bytes = [0; 24];
                    reader.read_exact(&mut bytes)?;
                    pakt = Some(parse_packet_table(&bytes)?);
                }
                b"chan" => {
                    if channel_layout_seen || size != 12 {
                        return Err(AudioError::Unsupported(
                            "CAF channel layout profile is not supported",
                        ));
                    }
                    let mut bytes = [0; 12];
                    reader.read_exact(&mut bytes)?;
                    let channels = desc
                        .ok_or(AudioError::InvalidFormat("CAF chan chunk precedes desc"))?
                        .spec
                        .channels();
                    channel_mask = parse_channel_layout(&bytes, channels)?;
                    channel_layout_seen = true;
                }
                b"data" => {
                    if data.is_some() || size < 4 {
                        return Err(AudioError::InvalidFormat(
                            "invalid or duplicate CAF data chunk",
                        ));
                    }
                    let mut edit_count = [0; 4];
                    reader.read_exact(&mut edit_count)?;
                    data = Some((body + 4, size - 4));
                }
                _ => {}
            }
            cursor = end;
        }
        let description = desc.ok_or(AudioError::InvalidFormat("CAF desc chunk is missing"))?;
        if description.spec.channels() > 2 && channel_mask.is_none() {
            return Err(AudioError::InvalidFormat(
                "CAF files with more than two channels require a supported chan layout",
            ));
        }
        let spec = AudioSpec::new_with_speaker_mask(
            description.spec.sample_rate(),
            description.spec.channels(),
            description.spec.sample_format(),
            channel_mask,
        )?;
        let (data_offset, data_bytes) =
            data.ok_or(AudioError::InvalidFormat("CAF data chunk is missing"))?;
        let (frames, priming) = frame_count(description, data_bytes, pakt)?;
        reader.seek(SeekFrom::Start(data_offset))?;
        Ok(Self {
            reader,
            spec,
            metadata,
            codec: description.codec,
            little_endian: description.little_endian,
            bytes_per_sample: description.bytes_per_sample,
            bytes_per_packet: description.bytes_per_packet,
            frames_left: frames,
            ima_states: vec![None; usize::from(spec.channels())],
            ima_pending: Vec::new(),
            ima_pending_offset: 0,
            ima_priming_left: usize::try_from(priming).unwrap_or(usize::MAX),
        })
    }

    pub fn codec(&self) -> CafCodec {
        self.codec
    }

    fn read_bytes(&mut self, size: usize) -> Result<Vec<u8>, AudioError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| AudioError::InvalidData("CAF audio block allocation failed"))?;
        bytes.resize(size, 0);
        self.reader.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    fn read_ima4_frames(&mut self, max_frames: usize) -> Result<PcmBuffer, AudioError> {
        let channels = usize::from(self.spec.channels());
        let max_frames = max_frames.min((MAX_BLOCK_BYTES / (channels * 2)).max(1));
        let mut output = Vec::with_capacity(max_frames * channels);
        while output.len() / channels < max_frames && self.frames_left > 0 {
            if self.ima_pending_offset == self.ima_pending.len() {
                let packet_bytes = usize::try_from(self.bytes_per_packet)
                    .map_err(|_| AudioError::InvalidFormat("CAF IMA4 packet is too large"))?;
                let bytes = self.read_bytes(packet_bytes)?;
                let mut interleaved = vec![0; 64 * channels];
                for channel in 0..channels {
                    let packet: &[u8; 34] =
                        bytes[channel * 34..channel * 34 + 34].try_into().unwrap();
                    let (samples, state) = decode_ima4_packet(packet, self.ima_states[channel])
                        .map_err(AudioError::InvalidFormat)?;
                    self.ima_states[channel] = Some(state);
                    for (frame, sample) in samples.into_iter().enumerate() {
                        interleaved[frame * channels + channel] = sample;
                    }
                }
                let skip = self.ima_priming_left.min(64) * channels;
                self.ima_priming_left -= skip / channels;
                self.ima_pending = interleaved;
                self.ima_pending_offset = skip;
            }
            let available = (self.ima_pending.len() - self.ima_pending_offset) / channels;
            let take = available
                .min(max_frames - output.len() / channels)
                .min(usize::try_from(self.frames_left).unwrap_or(usize::MAX));
            let sample_count = take * channels;
            output.extend_from_slice(
                &self.ima_pending[self.ima_pending_offset..self.ima_pending_offset + sample_count],
            );
            self.ima_pending_offset += sample_count;
            self.frames_left -= take as u64;
        }
        PcmBuffer::new(self.spec, Samples::I16(output)).map_err(Into::into)
    }
}

fn parse_channel_layout(bytes: &[u8; 12], channels: u16) -> Result<Option<u32>, AudioError> {
    let tag = u32::from_be_bytes(bytes[..4].try_into().unwrap());
    let bitmap = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
    let descriptions = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
    if descriptions != 0 {
        return Err(AudioError::Unsupported(
            "CAF channel descriptions are not supported",
        ));
    }
    if tag == 0x0001_0000 && bitmap.count_ones() == u32::from(channels) {
        return Ok(Some(bitmap));
    }
    if bitmap == 0
        && (tag == (100 << 16) | 1 && channels == 1 || tag == (101 << 16) | 2 && channels == 2)
    {
        return Ok(None);
    }
    Err(AudioError::Unsupported(
        "CAF chan layout must use a matching channel bitmap or mono/stereo tag",
    ))
}

impl<R: Read + Seek> AudioDecoder for CafDecoder<R> {
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
        if self.codec == CafCodec::Ima4 {
            return self.read_ima4_frames(max_frames).map(Some);
        }
        let frame_bytes = match self.codec {
            CafCodec::Pcm => self.bytes_per_sample * usize::from(self.spec.channels()),
            CafCodec::ALaw | CafCodec::MuLaw => usize::from(self.spec.channels()),
            CafCodec::Ima4 => unreachable!(),
        };
        let output_frame_bytes = match self.codec {
            CafCodec::Pcm => frame_bytes,
            CafCodec::ALaw | CafCodec::MuLaw => usize::from(self.spec.channels()) * 2,
            CafCodec::Ima4 => unreachable!(),
        };
        let frames = max_frames
            .min((MAX_BLOCK_BYTES / output_frame_bytes).max(1))
            .min(usize::try_from(self.frames_left).unwrap_or(usize::MAX));
        let size = frames
            .checked_mul(frame_bytes)
            .ok_or(AudioError::InvalidData("CAF audio block size overflow"))?;
        let bytes = self.read_bytes(size)?;
        self.frames_left -= frames as u64;
        let samples = match self.codec {
            CafCodec::Pcm => decode_pcm(
                &bytes,
                self.spec.sample_format(),
                self.bytes_per_sample,
                self.little_endian,
            )?,
            CafCodec::ALaw => {
                Samples::I16(bytes.iter().map(|&sample| decode_alaw(sample)).collect())
            }
            CafCodec::MuLaw => {
                Samples::I16(bytes.iter().map(|&sample| decode_mulaw(sample)).collect())
            }
            CafCodec::Ima4 => unreachable!(),
        };
        PcmBuffer::new(self.spec, samples)
            .map(Some)
            .map_err(Into::into)
    }
}

fn parse_description(bytes: &[u8; 32]) -> Result<Description, AudioError> {
    let sample_rate = f64::from_bits(u64::from_be_bytes(bytes[..8].try_into().unwrap()));
    if !sample_rate.is_finite()
        || sample_rate < 1.0
        || sample_rate.fract() != 0.0
        || sample_rate > u32::MAX as f64
    {
        return Err(AudioError::Unsupported(
            "CAF sample rate must be an integer in the shared AudioSpec range",
        ));
    }
    let format = &bytes[8..12];
    let flags = u32::from_be_bytes(bytes[12..16].try_into().unwrap());
    let bytes_per_packet = u64::from(u32::from_be_bytes(bytes[16..20].try_into().unwrap()));
    let frames_per_packet = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    let channels = u32::from_be_bytes(bytes[24..28].try_into().unwrap());
    let bits = u32::from_be_bytes(bytes[28..32].try_into().unwrap());
    let channels = u16::try_from(channels)
        .ok()
        .filter(|&channels| channels > 0)
        .ok_or(AudioError::Unsupported(
            "CAF channel count is outside AudioSpec limits",
        ))?;
    let (codec, sample_format, bytes_per_sample, little_endian) = match format {
        b"lpcm" => {
            if flags & !3 != 0 {
                return Err(AudioError::Unsupported(
                    "CAF linear PCM flags are not supported",
                ));
            }
            let little = flags & 2 != 0;
            if flags & 1 != 0 {
                let format = match bits {
                    32 => SampleFormat::Float32,
                    64 => SampleFormat::Float64,
                    _ => {
                        return Err(AudioError::Unsupported(
                            "CAF floating-point PCM must be 32- or 64-bit",
                        ));
                    }
                };
                (CafCodec::Pcm, format, (bits / 8) as usize, little)
            } else {
                let format = match bits {
                    16 => SampleFormat::PcmI16,
                    24 => SampleFormat::PcmI24,
                    32 => SampleFormat::PcmI32,
                    _ => {
                        return Err(AudioError::Unsupported(
                            "CAF integer PCM must be 16-, 24-, or 32-bit",
                        ));
                    }
                };
                let packed24 = bits == 24 && bytes_per_packet == u64::from(channels) * 3;
                let bytes = if bits == 24 && !packed24 {
                    4
                } else {
                    (bits / 8) as usize
                };
                (CafCodec::Pcm, format, bytes, little)
            }
        }
        b"alaw" | b"ulaw" => {
            if flags != 0
                || bits != 0
                || frames_per_packet != 1
                || bytes_per_packet != u64::from(channels)
            {
                return Err(AudioError::InvalidFormat(
                    "invalid CAF G.711 audio description",
                ));
            }
            (
                if format == b"alaw" {
                    CafCodec::ALaw
                } else {
                    CafCodec::MuLaw
                },
                SampleFormat::PcmI16,
                1,
                false,
            )
        }
        b"ima4" => {
            if flags != 0
                || bits != 0
                || frames_per_packet != 64
                || bytes_per_packet != u64::from(channels) * 34
            {
                return Err(AudioError::InvalidFormat(
                    "invalid CAF IMA4 audio description",
                ));
            }
            (CafCodec::Ima4, SampleFormat::PcmI16, 0, false)
        }
        _ => return Err(AudioError::Unsupported("CAF audio codec is not supported")),
    };
    if codec == CafCodec::Pcm {
        let expected = u64::from(channels)
            .checked_mul(bytes_per_sample as u64)
            .ok_or(AudioError::InvalidFormat("CAF PCM packet size overflow"))?;
        if frames_per_packet != 1 || bytes_per_packet != expected {
            return Err(AudioError::InvalidFormat(
                "CAF PCM packet geometry is inconsistent",
            ));
        }
    }
    let spec = AudioSpec::new(sample_rate as u32, channels, sample_format)?;
    Ok(Description {
        spec,
        codec,
        little_endian,
        bytes_per_sample,
        bytes_per_packet,
    })
}

fn parse_packet_table(bytes: &[u8; 24]) -> Result<(u64, u64, u32, u32), AudioError> {
    let packets = i64::from_be_bytes(bytes[..8].try_into().unwrap());
    let valid = i64::from_be_bytes(bytes[8..16].try_into().unwrap());
    let priming = i32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let remainder = i32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if packets < 0 || valid < 0 || priming < 0 || remainder < 0 {
        return Err(AudioError::InvalidFormat(
            "CAF packet table contains negative counts",
        ));
    }
    Ok((
        packets as u64,
        valid as u64,
        priming as u32,
        remainder as u32,
    ))
}

fn frame_count(
    description: Description,
    data_bytes: u64,
    pakt: Option<(u64, u64, u32, u32)>,
) -> Result<(u64, u64), AudioError> {
    match description.codec {
        CafCodec::Pcm | CafCodec::ALaw | CafCodec::MuLaw => {
            if pakt.is_some() {
                return Err(AudioError::InvalidFormat(
                    "CAF packet table is unexpected for fixed-frame audio",
                ));
            }
            let frame_bytes = description.bytes_per_packet;
            if data_bytes % frame_bytes != 0 {
                return Err(AudioError::InvalidFormat(
                    "CAF data ends in a partial audio frame",
                ));
            }
            Ok((data_bytes / frame_bytes, 0))
        }
        CafCodec::Ima4 => {
            if data_bytes % description.bytes_per_packet != 0 {
                return Err(AudioError::InvalidFormat(
                    "CAF IMA4 data ends in a partial packet",
                ));
            }
            let packet_count = data_bytes / description.bytes_per_packet;
            let decoded = packet_count
                .checked_mul(64)
                .ok_or(AudioError::InvalidFormat("CAF IMA4 frame count overflow"))?;
            let Some((packets, valid, priming, remainder)) = pakt else {
                return Ok((decoded, 0));
            };
            if packets != 0 || u64::from(priming) + u64::from(remainder) + valid != decoded {
                return Err(AudioError::InvalidFormat(
                    "CAF IMA4 packet table disagrees with audio data",
                ));
            }
            Ok((valid, u64::from(priming)))
        }
    }
}

fn decode_pcm(
    bytes: &[u8],
    format: SampleFormat,
    bytes_per_sample: usize,
    little: bool,
) -> Result<Samples, AudioError> {
    let samples = match format {
        SampleFormat::PcmI16 => Samples::I16(
            bytes
                .chunks_exact(2)
                .map(|x| {
                    let a = [x[0], x[1]];
                    if little {
                        i16::from_le_bytes(a)
                    } else {
                        i16::from_be_bytes(a)
                    }
                })
                .collect(),
        ),
        SampleFormat::PcmI24 => {
            let values = if bytes_per_sample == 3 {
                bytes
                    .chunks_exact(3)
                    .map(|x| {
                        let value = if little {
                            u32::from_le_bytes([x[0], x[1], x[2], 0])
                        } else {
                            u32::from_be_bytes([0, x[0], x[1], x[2]])
                        };
                        (value as i32) << 8 >> 8
                    })
                    .collect()
            } else {
                bytes
                    .chunks_exact(4)
                    .map(|x| {
                        let value = if little {
                            i32::from_le_bytes(x.try_into().unwrap())
                        } else {
                            i32::from_be_bytes(x.try_into().unwrap())
                        };
                        value >> 8
                    })
                    .collect()
            };
            Samples::I24(values)
        }
        SampleFormat::PcmI32 => Samples::I32(
            bytes
                .chunks_exact(4)
                .map(|x| {
                    let a: [u8; 4] = x.try_into().unwrap();
                    if little {
                        i32::from_le_bytes(a)
                    } else {
                        i32::from_be_bytes(a)
                    }
                })
                .collect(),
        ),
        SampleFormat::Float32 => Samples::F32(
            bytes
                .chunks_exact(4)
                .map(|x| {
                    let a: [u8; 4] = x.try_into().unwrap();
                    f32::from_bits(if little {
                        u32::from_le_bytes(a)
                    } else {
                        u32::from_be_bytes(a)
                    })
                })
                .collect(),
        ),
        SampleFormat::Float64 => Samples::F64(
            bytes
                .chunks_exact(8)
                .map(|x| {
                    let a: [u8; 8] = x.try_into().unwrap();
                    f64::from_bits(if little {
                        u64::from_le_bytes(a)
                    } else {
                        u64::from_be_bytes(a)
                    })
                })
                .collect(),
        ),
        SampleFormat::PcmU8 => {
            return Err(AudioError::Unsupported("CAF unsigned PCM is not supported"));
        }
    };
    Ok(samples)
}
