use std::{
    collections::VecDeque,
    io::{BufReader, Read},
};

use zemer_core::{AudioDecoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::{
    bit_reader::BitReader,
    frame::read_frame,
    md5::Md5,
    metadata::{FlacMetadataBlock, FlacStreamInfo, read_metadata},
};

pub struct FlacDecoder<R> {
    reader: BitReader<BufReader<R>>,
    info: FlacStreamInfo,
    spec: AudioSpec,
    metadata: Metadata,
    metadata_blocks: Vec<FlacMetadataBlock>,
    pending: VecDeque<i64>,
    audio_md5: Md5,
    ended: bool,
    decoded_frames: u64,
}

impl<R: Read> FlacDecoder<R> {
    pub fn new(reader: R) -> Result<Self, AudioError> {
        let mut reader = BufReader::new(reader);
        let (info, metadata, metadata_blocks) = read_metadata(&mut reader)?;
        let format = output_format(info.bits_per_sample());
        let spec = AudioSpec::new(info.sample_rate(), u16::from(info.channels()), format)?;
        Ok(Self {
            // shortcut: unknown-size frames are capped at 16 MiB, upgrade when supporting unusually large valid frames.
            reader: BitReader::new(
                reader,
                info.max_frame_size().unwrap_or(0x00ff_ffff) as usize,
            ),
            info,
            spec,
            metadata,
            metadata_blocks,
            pending: VecDeque::new(),
            audio_md5: Md5::new(),
            ended: false,
            decoded_frames: 0,
        })
    }

    pub fn stream_info(&self) -> &FlacStreamInfo {
        &self.info
    }

    pub fn metadata_blocks(&self) -> &[FlacMetadataBlock] {
        &self.metadata_blocks
    }

    fn fill_pending(&mut self) -> Result<bool, AudioError> {
        if self.ended {
            return Ok(false);
        }
        let Some((samples, frames)) = read_frame(&mut self.reader, &self.info)? else {
            if self
                .info
                .total_samples()
                .is_some_and(|expected| expected != self.decoded_frames)
            {
                return Err(AudioError::InvalidData(
                    "FLAC decoded sample count differs from STREAMINFO",
                ));
            }
            let expected_md5 = self.info.md5();
            if expected_md5 != [0; 16] && self.audio_md5.clone().finalize() != expected_md5 {
                return Err(AudioError::InvalidData(
                    "FLAC decoded audio MD5 differs from STREAMINFO",
                ));
            }
            self.ended = true;
            return Ok(false);
        };
        self.decoded_frames =
            self.decoded_frames
                .checked_add(frames as u64)
                .ok_or(AudioError::InvalidData(
                    "FLAC decoded sample count overflow",
                ))?;
        if self
            .info
            .total_samples()
            .is_some_and(|expected| self.decoded_frames > expected)
        {
            return Err(AudioError::InvalidData(
                "FLAC frame exceeds STREAMINFO total sample count",
            ));
        }
        let channels = usize::from(self.info.channels());
        if samples.len() != frames * channels {
            return Err(AudioError::InvalidData(
                "FLAC decoded frame has invalid sample count",
            ));
        }
        let bytes_per_sample = usize::from(self.info.bits_per_sample().div_ceil(8));
        let mut audio_bytes = [0_u8; 4096];
        let mut audio_bytes_used = 0;
        for sample in &samples {
            audio_bytes[audio_bytes_used..audio_bytes_used + bytes_per_sample]
                .copy_from_slice(&sample.to_le_bytes()[..bytes_per_sample]);
            audio_bytes_used += bytes_per_sample;
            if audio_bytes_used == audio_bytes.len() {
                self.audio_md5.update(&audio_bytes);
                audio_bytes_used = 0;
            }
        }
        self.audio_md5.update(&audio_bytes[..audio_bytes_used]);
        let shift = sample_container_bits(self.spec.sample_format()) - self.info.bits_per_sample();
        for sample in samples {
            let sample = sample
                .checked_mul(1_i64 << shift)
                .ok_or(AudioError::InvalidData("FLAC sample alignment overflow"))?;
            self.pending
                .push_back(if self.spec.sample_format() == SampleFormat::PcmU8 {
                    sample + 128
                } else {
                    sample
                });
        }
        Ok(true)
    }

    fn samples_to_pcm(&mut self, frames: usize) -> Result<PcmBuffer, AudioError> {
        let count = frames
            .checked_mul(usize::from(self.info.channels()))
            .ok_or(AudioError::InvalidData("FLAC output sample count overflow"))?;
        let values: Vec<i64> = self.pending.drain(..count).collect();
        let samples = match self.spec.sample_format() {
            SampleFormat::PcmU8 => Samples::U8(
                values
                    .into_iter()
                    .map(|sample| {
                        u8::try_from(sample)
                            .map_err(|_| AudioError::InvalidData("FLAC sample exceeds 8-bit PCM"))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            SampleFormat::PcmI16 => Samples::I16(
                values
                    .into_iter()
                    .map(|sample| {
                        i16::try_from(sample)
                            .map_err(|_| AudioError::InvalidData("FLAC sample exceeds 16-bit PCM"))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            SampleFormat::PcmI24 => Samples::I24(
                values
                    .into_iter()
                    .map(|sample| {
                        i32::try_from(sample)
                            .map_err(|_| AudioError::InvalidData("FLAC sample exceeds 24-bit PCM"))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            SampleFormat::PcmI32 => Samples::I32(
                values
                    .into_iter()
                    .map(|sample| {
                        i32::try_from(sample)
                            .map_err(|_| AudioError::InvalidData("FLAC sample exceeds 32-bit PCM"))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            SampleFormat::Float32 | SampleFormat::Float64 => {
                return Err(AudioError::InvalidData(
                    "FLAC selected a floating-point output",
                ));
            }
        };
        Ok(PcmBuffer::new(self.spec, samples)?)
    }
}

impl<R: Read> AudioDecoder for FlacDecoder<R> {
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
        if self.pending.is_empty() && !self.fill_pending()? {
            return Ok(None);
        }
        let channels = usize::from(self.info.channels());
        let frames = (self.pending.len() / channels).min(max_frames);
        self.samples_to_pcm(frames).map(Some)
    }
}

fn output_format(bits_per_sample: u8) -> SampleFormat {
    match bits_per_sample {
        1..=8 => SampleFormat::PcmU8,
        9..=16 => SampleFormat::PcmI16,
        17..=24 => SampleFormat::PcmI24,
        _ => SampleFormat::PcmI32,
    }
}

fn sample_container_bits(format: SampleFormat) -> u8 {
    match format {
        SampleFormat::PcmU8 => 8,
        SampleFormat::PcmI16 => 16,
        SampleFormat::PcmI24 => 24,
        SampleFormat::PcmI32 => 32,
        SampleFormat::Float32 | SampleFormat::Float64 => unreachable!(),
    }
}
