use std::io::{Seek, SeekFrom, Write};

use zemer_core::{AudioError, DsdBuffer, DsdEncoder, DsdSpec, Metadata};
use zemer_metadata::apev2::ApeTag;

use crate::encode::{
    BLOCK_FRAMES, BlockPatch, FINAL_BLOCK, HAS_CHECKSUM, INITIAL_BLOCK, SAMPLE_RATES,
    block_checksum, make_header, patch_total_samples, write_sub_block,
};

const MONO_DATA: u32 = 1 << 2;
const DSD_DATA: u32 = 1 << 31;

/// Writes raw, lossless packed DSD octets in WavPack blocks.
pub struct WavPackDsdEncoder<W> {
    writer: W,
    spec: DsdSpec,
    base_rate: u32,
    rate_exponent: u8,
    rate_index: Option<usize>,
    metadata: Metadata,
    samples: Vec<u8>,
    frames: u64,
    encoded_frames: u64,
    blocks: Vec<BlockPatch>,
    poisoned: bool,
    finished: bool,
}

impl<W: Write + Seek> WavPackDsdEncoder<W> {
    pub fn new(writer: W, spec: DsdSpec) -> Result<Self, AudioError> {
        if spec.channels() > 32 {
            return Err(AudioError::Unsupported(
                "WavPack DSD encoding supports up to 32 channels",
            ));
        }
        if spec.bit_rate() % 8 != 0 {
            return Err(AudioError::Unsupported(
                "WavPack DSD encoding requires a bit rate divisible by eight",
            ));
        }
        let byte_rate = spec.bit_rate() / 8;
        let rate = SAMPLE_RATES
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, &base)| {
                let ratio = byte_rate.checked_div(base)?;
                (byte_rate % base == 0 && ratio.is_power_of_two()).then_some((
                    base,
                    ratio.trailing_zeros() as u8,
                    Some(index),
                ))
            });
        let (base_rate, rate_exponent, rate_index) = rate.unwrap_or((byte_rate, 0, None));
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(BLOCK_FRAMES * usize::from(spec.channels()))
            .map_err(|_| AudioError::InvalidData("WavPack DSD buffer is too large"))?;
        Ok(Self {
            writer,
            spec,
            base_rate,
            rate_exponent,
            rate_index,
            metadata: Metadata::default(),
            samples,
            frames: 0,
            encoded_frames: 0,
            blocks: Vec::new(),
            poisoned: false,
            finished: false,
        })
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    fn flush_block(&mut self) -> Result<(), AudioError> {
        if self.samples.is_empty() {
            return Ok(());
        }
        let channels = usize::from(self.spec.channels());
        let frame_count = u32::try_from(self.samples.len() / channels)
            .map_err(|_| AudioError::InvalidData("WavPack DSD block is too large"))?;
        let samples = std::mem::take(&mut self.samples);
        let mask = self.spec.speaker_mask().unwrap_or_default();
        let mut channel_info = vec![self.spec.channels() as u8];
        channel_info.extend_from_slice(&mask.to_le_bytes());
        let samples_ref = samples.as_slice();
        for first_channel in (0..channels).step_by(2) {
            let block_channels = (channels - first_channel).min(2);
            let channel_samples = if channels <= 2 {
                None
            } else {
                Some(
                    (0..frame_count as usize)
                        .flat_map(|frame| {
                            (first_channel..first_channel + block_channels)
                                .map(move |channel| samples_ref[frame * channels + channel])
                        })
                        .collect::<Vec<_>>(),
                )
            };
            self.write_dsd_block(
                channel_samples.as_deref().unwrap_or(&samples),
                block_channels,
                first_channel == 0,
                first_channel + block_channels == channels,
                (channels > 2 && first_channel == 0).then_some(channel_info.as_slice()),
            )?;
        }
        self.encoded_frames += u64::from(frame_count);
        self.samples = samples;
        self.samples.clear();
        Ok(())
    }

    fn write_dsd_block(
        &mut self,
        samples: &[u8],
        channels: usize,
        initial: bool,
        final_block: bool,
        channel_info: Option<&[u8]>,
    ) -> Result<(), AudioError> {
        let mut flags = DSD_DATA | HAS_CHECKSUM;
        if channels == 1 {
            flags |= MONO_DATA;
        }
        if initial {
            flags |= INITIAL_BLOCK;
        }
        if final_block {
            flags |= FINAL_BLOCK;
        }
        if let Some(index) = self.rate_index {
            flags |= (index as u32) << 23;
        } else {
            flags |= 15 << 23;
        }
        let mut payload = Vec::new();
        if self.rate_index.is_none() {
            write_sub_block(&mut payload, 0x27, &self.base_rate.to_le_bytes())?;
        }
        if let Some(channel_info) = channel_info {
            write_sub_block(&mut payload, 0x0d, channel_info)?;
        }
        let mut dsd = Vec::with_capacity(2 + samples.len());
        dsd.extend([self.rate_exponent, 0]);
        dsd.extend_from_slice(samples);
        write_sub_block(&mut payload, 0x0e, &dsd)?;
        let sample_checksum = samples.iter().fold(u32::MAX, |checksum, &byte| {
            checksum.wrapping_mul(3).wrapping_add(u32::from(byte))
        });
        let block_size = 32_usize
            .checked_add(payload.len())
            .and_then(|size| size.checked_add(6))
            .ok_or(AudioError::InvalidData("WavPack DSD block size overflow"))?;
        let header = make_header(
            block_size,
            self.encoded_frames,
            u32::try_from(samples.len() / channels)
                .map_err(|_| AudioError::InvalidData("WavPack DSD block is too large"))?,
            flags,
            sample_checksum,
        )?;
        let check = block_checksum(&header, &payload);
        let offset = self.writer.stream_position()?;
        self.blocks.push(BlockPatch {
            offset,
            payload_words: payload.len() / 2,
            checksum: check,
            header,
        });
        payload.extend_from_slice(&[0x2f, 2]);
        payload.extend_from_slice(&check.to_le_bytes());
        self.writer.write_all(&header)?;
        self.writer.write_all(&payload)?;
        Ok(())
    }
}

impl<W: Write + Seek> DsdEncoder for WavPackDsdEncoder<W> {
    fn dsd_spec(&self) -> DsdSpec {
        self.spec
    }

    fn write_dsd_octets(&mut self, block: &DsdBuffer) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData(
                "WavPack encoder is already finished",
            ));
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "WavPack DSD encoder is unusable after an earlier write failure",
            ));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "WavPack DSD specification changed while encoding",
            ));
        }
        let frames = u64::try_from(block.frames())
            .map_err(|_| AudioError::InvalidData("WavPack DSD frame count overflow"))?;
        self.frames = self
            .frames
            .checked_add(frames)
            .filter(|&count| count < (1_u64 << 40))
            .ok_or(AudioError::InvalidData(
                "WavPack DSD sample count exceeds 40 bits",
            ))?;
        let result = (|| {
            for &byte in block.bytes() {
                self.samples.push(byte);
                if self.samples.len() == BLOCK_FRAMES * usize::from(self.spec.channels()) {
                    self.flush_block()?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        if self.finished {
            return Ok(());
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "WavPack DSD encoder is unusable after an earlier write failure",
            ));
        }
        if self.frames == 0 {
            return Err(AudioError::InvalidData(
                "cannot encode an empty WavPack DSD stream",
            ));
        }
        let result = (|| {
            self.flush_block()?;
            patch_total_samples(&mut self.writer, self.frames, &self.blocks)?;
            self.writer.seek(SeekFrom::End(0))?;
            if self.metadata.iter().next().is_some() {
                let mut tag = ApeTag::new();
                tag.replace_text_metadata(&self.metadata)
                    .map_err(|_| AudioError::InvalidData("invalid WavPack APEv2 metadata"))?;
                self.writer.write_all(&tag.encode().map_err(|_| {
                    AudioError::InvalidData("WavPack APEv2 metadata exceeds the supported size")
                })?)?;
            }
            self.writer.flush()?;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        } else {
            self.finished = true;
        }
        result
    }

    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if !metadata.pictures().is_empty() {
            return Err(AudioError::Unsupported(
                "WavPack writer does not support embedded pictures",
            ));
        }
        if self.finished {
            return Err(AudioError::InvalidData(
                "WavPack encoder is already finished",
            ));
        }
        self.metadata = metadata.clone();
        Ok(())
    }
}
