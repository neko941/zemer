use std::io::{Seek, SeekFrom, Write};

use g711::{encode_alaw, encode_mulaw};
use ima_adpcm::{ImaAdpcmState, encode_ima4_packet};
use zemer_core::{AudioEncoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::{decode::CafCodec, info::write_info};

pub struct CafEncoder<W> {
    writer: W,
    spec: AudioSpec,
    codec: CafCodec,
    little_endian: bool,
    packed_24: bool,
    data_size_offset: u64,
    pakt_offset: Option<u64>,
    data_bytes: u64,
    frames: u64,
    ima_buffer: Vec<i16>,
    ima_states: Vec<Option<ImaAdpcmState>>,
    metadata: Metadata,
    poisoned: bool,
    finished: bool,
}

impl<W: Write + Seek> CafEncoder<W> {
    pub fn new(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_configured(writer, spec, CafCodec::Pcm, false, true)
    }

    pub fn new_g711(
        writer: W,
        spec: AudioSpec,
        encoding: g711::G711Encoding,
    ) -> Result<Self, AudioError> {
        if spec.sample_format() != SampleFormat::PcmI16 {
            return Err(AudioError::InvalidData(
                "CAF G.711 encoding requires signed 16-bit PCM",
            ));
        }
        let codec = match encoding {
            g711::G711Encoding::ALaw => CafCodec::ALaw,
            g711::G711Encoding::MuLaw => CafCodec::MuLaw,
        };
        Self::new_configured(writer, spec, codec, false, true)
    }

    pub fn new_ima4(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        if spec.sample_format() != SampleFormat::PcmI16 {
            return Err(AudioError::InvalidData(
                "CAF IMA4 encoding requires signed 16-bit PCM",
            ));
        }
        Self::new_configured(writer, spec, CafCodec::Ima4, false, true)
    }

    pub fn new_pcm_with_byte_order(
        writer: W,
        spec: AudioSpec,
        little_endian: bool,
        packed_24: bool,
    ) -> Result<Self, AudioError> {
        if spec.sample_format() == SampleFormat::PcmU8 {
            return Err(AudioError::Unsupported(
                "CAF unsigned 8-bit PCM is not supported",
            ));
        }
        Self::new_configured(writer, spec, CafCodec::Pcm, little_endian, packed_24)
    }

    fn new_configured(
        writer: W,
        spec: AudioSpec,
        codec: CafCodec,
        little_endian: bool,
        packed_24: bool,
    ) -> Result<Self, AudioError> {
        if codec == CafCodec::Pcm && spec.sample_format() == SampleFormat::PcmU8 {
            return Err(AudioError::Unsupported(
                "CAF unsigned 8-bit PCM is not supported",
            ));
        }
        match spec.speaker_mask() {
            Some(mask) if mask.count_ones() == u32::from(spec.channels()) => {}
            Some(_) => {
                return Err(AudioError::InvalidData(
                    "CAF speaker bitmap does not match its channel count",
                ));
            }
            None if spec.channels() > 2 => {
                return Err(AudioError::Unsupported(
                    "CAF output with more than two channels requires a speaker bitmap",
                ));
            }
            None => {}
        }
        let bytes_per_sample = match spec.sample_format() {
            SampleFormat::PcmI16 => 2_u64,
            SampleFormat::PcmI24 => 3,
            SampleFormat::PcmI32 | SampleFormat::Float32 => 4,
            SampleFormat::Float64 => 8,
            SampleFormat::PcmU8 => 1,
        };
        let (format_id, flags, bits, frames_per_packet, bytes_per_packet): (
            [u8; 4],
            u32,
            u32,
            u32,
            u64,
        ) = match codec {
            CafCodec::Pcm => {
                let (flags, bits) = match spec.sample_format() {
                    SampleFormat::PcmI16 => (u32::from(little_endian) * 2, 16),
                    SampleFormat::PcmI24 => (u32::from(little_endian) * 2, 24),
                    SampleFormat::PcmI32 => (u32::from(little_endian) * 2, 32),
                    SampleFormat::Float32 => (1 | (u32::from(little_endian) * 2), 32),
                    SampleFormat::Float64 => (1 | (u32::from(little_endian) * 2), 64),
                    SampleFormat::PcmU8 => unreachable!(),
                };
                let bytes = if spec.sample_format() == SampleFormat::PcmI24 && !packed_24 {
                    4
                } else {
                    bytes_per_sample
                };
                (*b"lpcm", flags, bits, 1, bytes * u64::from(spec.channels()))
            }
            CafCodec::ALaw => (*b"alaw", 0, 0, 1, u64::from(spec.channels())),
            CafCodec::MuLaw => (*b"ulaw", 0, 0, 1, u64::from(spec.channels())),
            CafCodec::Ima4 => (*b"ima4", 0, 0, 64, u64::from(spec.channels()) * 34),
        };
        if codec != CafCodec::Pcm && spec.sample_format() != SampleFormat::PcmI16 {
            return Err(AudioError::InvalidData(
                "CAF compressed output requires signed 16-bit PCM",
            ));
        }
        let mut caf = Self {
            writer,
            spec,
            codec,
            little_endian,
            packed_24,
            data_size_offset: 0,
            pakt_offset: None,
            data_bytes: 0,
            frames: 0,
            ima_buffer: Vec::new(),
            ima_states: vec![None; usize::from(spec.channels())],
            metadata: Metadata::default(),
            poisoned: false,
            finished: false,
        };
        caf.writer.write_all(b"caff\0\x01\0\0")?;
        caf.writer.write_all(b"desc\0\0\0\0\0\0\0\x20")?;
        caf.writer
            .write_all(&(f64::from(spec.sample_rate()).to_bits()).to_be_bytes())?;
        caf.writer.write_all(&format_id)?;
        caf.writer.write_all(&flags.to_be_bytes())?;
        caf.writer.write_all(
            &u32::try_from(bytes_per_packet)
                .map_err(|_| AudioError::Unsupported("CAF packet size exceeds the format limit"))?
                .to_be_bytes(),
        )?;
        caf.writer.write_all(&frames_per_packet.to_be_bytes())?;
        caf.writer
            .write_all(&u32::from(spec.channels()).to_be_bytes())?;
        caf.writer.write_all(&bits.to_be_bytes())?;
        if let Some(mask) = spec.speaker_mask() {
            caf.writer.write_all(b"chan\0\0\0\0\0\0\0\x0c")?;
            caf.writer.write_all(&0x0001_0000_u32.to_be_bytes())?;
            caf.writer.write_all(&mask.to_be_bytes())?;
            caf.writer.write_all(&0_u32.to_be_bytes())?;
        }
        if codec == CafCodec::Ima4 {
            caf.writer.write_all(b"pakt\0\0\0\0\0\0\0\x18")?;
            caf.pakt_offset = Some(caf.writer.stream_position()?);
            caf.writer.write_all(&[0; 24])?;
        }
        caf.writer.write_all(b"data")?;
        caf.data_size_offset = caf.writer.stream_position()?;
        caf.writer.write_all(&(-1_i64).to_be_bytes())?;
        caf.writer.write_all(&0_u32.to_be_bytes())?;
        Ok(caf)
    }

    pub fn into_inner(mut self) -> Result<W, AudioError> {
        self.finish()?;
        Ok(self.writer)
    }

    fn write_audio(&mut self, bytes: &[u8]) -> Result<(), AudioError> {
        let next = self
            .data_bytes
            .checked_add(bytes.len() as u64)
            .ok_or(AudioError::Unsupported("CAF audio data size overflow"))?;
        if let Err(error) = self.writer.write_all(bytes) {
            self.poisoned = true;
            return Err(error.into());
        }
        self.data_bytes = next;
        Ok(())
    }

    fn write_ima4_samples(&mut self, samples: &[i16]) -> Result<(), AudioError> {
        let channels = usize::from(self.spec.channels());
        let packet_samples = 64 * channels;
        self.ima_buffer.extend_from_slice(samples);
        while self.ima_buffer.len() >= packet_samples {
            let packet_samples = self.ima_buffer.drain(..packet_samples).collect::<Vec<_>>();
            let mut bytes = Vec::with_capacity(channels * 34);
            for channel in 0..channels {
                let channel_samples =
                    std::array::from_fn(|frame| packet_samples[frame * channels + channel]);
                let (packet, state) =
                    encode_ima4_packet(&channel_samples, self.ima_states[channel]);
                self.ima_states[channel] = Some(state);
                bytes.extend_from_slice(&packet);
            }
            self.write_audio(&bytes)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        if self.finished {
            return Ok(());
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "CAF writer is unusable after an earlier write error",
            ));
        }
        if let Err(error) = self.finish_inner() {
            self.poisoned = true;
            return Err(error);
        }
        Ok(())
    }

    fn finish_inner(&mut self) -> Result<(), AudioError> {
        let mut packet_table = None;
        if self.codec == CafCodec::Ima4 && !self.ima_buffer.is_empty() {
            let channels = usize::from(self.spec.channels());
            self.ima_buffer.resize(64 * channels, 0);
            let samples = std::mem::take(&mut self.ima_buffer);
            let mut bytes = Vec::with_capacity(channels * 34);
            for channel in 0..channels {
                let channel_samples =
                    std::array::from_fn(|frame| samples[frame * channels + channel]);
                let (packet, state) =
                    encode_ima4_packet(&channel_samples, self.ima_states[channel]);
                self.ima_states[channel] = Some(state);
                bytes.extend_from_slice(&packet);
            }
            self.write_audio(&bytes)?;
            let packets = self.data_bytes / (channels as u64 * 34);
            let padded_frames = packets * 64;
            let remainder = padded_frames.saturating_sub(self.frames);
            packet_table = Some((self.frames, remainder));
        } else if self.codec == CafCodec::Ima4 {
            let packets = self.data_bytes / (u64::from(self.spec.channels()) * 34);
            packet_table = Some((self.frames, packets * 64 - self.frames));
        }
        let end = self.writer.stream_position()?;
        if let Some((valid_frames, remainder)) = packet_table {
            self.write_packet_table(valid_frames, remainder)?;
        }
        let data_size = self
            .data_bytes
            .checked_add(4)
            .ok_or(AudioError::Unsupported("CAF data chunk size overflow"))?;
        self.writer.seek(SeekFrom::Start(self.data_size_offset))?;
        self.writer.write_all(
            &i64::try_from(data_size)
                .map_err(|_| AudioError::Unsupported("CAF data exceeds its size limit"))?
                .to_be_bytes(),
        )?;
        self.writer.seek(SeekFrom::Start(end))?;
        write_info(&mut self.writer, &self.metadata)?;
        self.writer.flush()?;
        self.finished = true;
        Ok(())
    }

    fn write_packet_table(&mut self, valid_frames: u64, remainder: u64) -> Result<(), AudioError> {
        let offset = self.pakt_offset.expect("IMA4 writer has a packet table");
        self.writer.seek(SeekFrom::Start(offset))?;
        // IMA4 packets have a fixed size; CAF reserves this count for variable-size packets.
        self.writer.write_all(&0_i64.to_be_bytes())?;
        self.writer.write_all(
            &i64::try_from(valid_frames)
                .map_err(|_| AudioError::Unsupported("CAF frame count overflow"))?
                .to_be_bytes(),
        )?;
        self.writer.write_all(&0_i32.to_be_bytes())?;
        self.writer.write_all(
            &i32::try_from(remainder)
                .map_err(|_| AudioError::Unsupported("CAF remainder frame count overflow"))?
                .to_be_bytes(),
        )?;
        Ok(())
    }
}

impl<W: Write + Seek> AudioEncoder for CafEncoder<W> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        if self.finished || self.poisoned {
            return Err(AudioError::InvalidData("CAF writer is no longer usable"));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "PCM block does not match CAF writer specification",
            ));
        }
        let next_frames = self
            .frames
            .checked_add(block.frames() as u64)
            .ok_or(AudioError::Unsupported("CAF frame count overflow"))?;
        match self.codec {
            CafCodec::Pcm => {
                let bytes = encode_pcm(block.samples(), self.little_endian, self.packed_24)?;
                self.write_audio(&bytes)?;
            }
            CafCodec::ALaw | CafCodec::MuLaw => {
                let Samples::I16(samples) = block.samples() else {
                    return Err(AudioError::InvalidData(
                        "CAF G.711 encoding requires signed 16-bit PCM",
                    ));
                };
                let bytes: Vec<_> = samples
                    .iter()
                    .map(|&sample| {
                        if self.codec == CafCodec::ALaw {
                            encode_alaw(sample)
                        } else {
                            encode_mulaw(sample)
                        }
                    })
                    .collect();
                self.write_audio(&bytes)?;
            }
            CafCodec::Ima4 => {
                let Samples::I16(samples) = block.samples() else {
                    return Err(AudioError::InvalidData(
                        "CAF IMA4 encoding requires signed 16-bit PCM",
                    ));
                };
                self.write_ima4_samples(samples)?;
            }
        }
        self.frames = next_frames;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        CafEncoder::finish(self)
    }

    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if !metadata.pictures().is_empty() {
            return Err(AudioError::Unsupported(
                "CAF writer does not support embedded pictures",
            ));
        }
        if self.finished {
            return Err(AudioError::InvalidData(
                "CAF metadata cannot be changed after finishing",
            ));
        }
        self.metadata = metadata.clone();
        Ok(())
    }
}

fn encode_pcm(samples: &Samples, little: bool, packed_24: bool) -> Result<Vec<u8>, AudioError> {
    let mut bytes = Vec::new();
    match samples {
        Samples::I16(values) => {
            for sample in values {
                bytes.extend(if little {
                    sample.to_le_bytes()
                } else {
                    sample.to_be_bytes()
                });
            }
        }
        Samples::I24(values) => {
            for &sample in values {
                if !(-8_388_608..=8_388_607).contains(&sample) {
                    return Err(AudioError::InvalidData(
                        "CAF 24-bit PCM sample is out of range",
                    ));
                }
                if packed_24 {
                    let encoded = if little {
                        sample.to_le_bytes()
                    } else {
                        sample.to_be_bytes()
                    };
                    if little {
                        bytes.extend_from_slice(&encoded[..3]);
                    } else {
                        bytes.extend_from_slice(&encoded[1..]);
                    }
                } else {
                    let aligned = sample << 8;
                    bytes.extend(if little {
                        aligned.to_le_bytes()
                    } else {
                        aligned.to_be_bytes()
                    });
                }
            }
        }
        Samples::I32(values) => {
            for sample in values {
                bytes.extend(if little {
                    sample.to_le_bytes()
                } else {
                    sample.to_be_bytes()
                });
            }
        }
        Samples::F32(values) => {
            for sample in values {
                bytes.extend(if little {
                    sample.to_bits().to_le_bytes()
                } else {
                    sample.to_bits().to_be_bytes()
                });
            }
        }
        Samples::F64(values) => {
            for sample in values {
                bytes.extend(if little {
                    sample.to_bits().to_le_bytes()
                } else {
                    sample.to_bits().to_be_bytes()
                });
            }
        }
        Samples::U8(_) => {
            return Err(AudioError::Unsupported(
                "CAF unsigned 8-bit PCM is not supported",
            ));
        }
    }
    Ok(bytes)
}
