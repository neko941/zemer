use std::io::{Seek, SeekFrom, Write};

use zemer_core::{AudioEncoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};
use zemer_metadata::apev2::ApeTag;

use crate::{
    encode::{
        BitWriter, BlockPatch, FINAL_BLOCK, HAS_CHECKSUM, INITIAL_BLOCK, INT32_DATA, MONO_DATA,
        SAMPLE_RATES, block_checksum, get_median, make_header, patch_total_samples,
        sample_checksum, update_median, write_sub_block, wvx_integer_checksum,
    },
    hybrid::HybridState,
};

const HYBRID: u32 = 1 << 3;
const BITRATE_MODE: u32 = 1 << 9;
const BLOCK_FRAMES: usize = 131_072;

struct HybridWords {
    main: Vec<u8>,
    correction: Vec<u8>,
    lossy: Vec<i32>,
    profile: Vec<u8>,
}

/// Writes integer PCM to a lossy `.wv` stream and its lossless `.wvc` correction stream.
pub struct WavPackHybridEncoder<W, C> {
    writer: W,
    correction: C,
    spec: AudioSpec,
    metadata: Metadata,
    samples: Vec<i32>,
    bitrate_kbps: u32,
    frames: u64,
    encoded_frames: u64,
    blocks: Vec<BlockPatch>,
    correction_blocks: Vec<BlockPatch>,
    poisoned: bool,
    finished: bool,
}

impl<W: Write + Seek, C: Write + Seek> WavPackHybridEncoder<W, C> {
    /// Creates paired hybrid streams. `bitrate_kbps` follows WavPack's standard 24–9600 kbps range.
    pub fn new(
        writer: W,
        correction: C,
        spec: AudioSpec,
        bitrate_kbps: u32,
    ) -> Result<Self, AudioError> {
        if !(24..=9_600).contains(&bitrate_kbps) {
            return Err(AudioError::InvalidData(
                "WavPack hybrid bitrate must be 24–9600 kbps",
            ));
        }
        if spec.channels() > 32 {
            return Err(AudioError::Unsupported(
                "WavPack encoding currently supports up to 32 channels",
            ));
        }
        if !matches!(
            spec.sample_format(),
            SampleFormat::PcmU8
                | SampleFormat::PcmI16
                | SampleFormat::PcmI24
                | SampleFormat::PcmI32
        ) {
            return Err(AudioError::Unsupported(
                "WavPack hybrid encoding currently supports integer PCM",
            ));
        }
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(BLOCK_FRAMES * usize::from(spec.channels()))
            .map_err(|_| AudioError::InvalidData("WavPack encoder buffer is too large"))?;
        Ok(Self {
            writer,
            correction,
            spec,
            metadata: Metadata::default(),
            samples,
            bitrate_kbps,
            frames: 0,
            encoded_frames: 0,
            blocks: Vec::new(),
            correction_blocks: Vec::new(),
            poisoned: false,
            finished: false,
        })
    }

    pub fn into_inner(self) -> (W, C) {
        (self.writer, self.correction)
    }

    fn flush_block(&mut self) -> Result<(), AudioError> {
        if self.samples.is_empty() {
            return Ok(());
        }
        let channels = usize::from(self.spec.channels());
        let frames = u32::try_from(self.samples.len() / channels)
            .map_err(|_| AudioError::InvalidData("WavPack block is too large"))?;
        let block_index = self.encoded_frames;
        let samples = std::mem::take(&mut self.samples);
        let mask = self.spec.speaker_mask().unwrap_or_default();
        let channel_info = (channels > 2).then(|| {
            let mut info = vec![self.spec.channels() as u8];
            info.extend_from_slice(&mask.to_le_bytes());
            info
        });
        for first in (0..channels).step_by(2) {
            let group_channels = (channels - first).min(2);
            let samples_ref = samples.as_slice();
            let group: Vec<i32> = (0..frames as usize)
                .flat_map(|frame| {
                    (first..first + group_channels)
                        .map(move |channel| samples_ref[frame * channels + channel])
                })
                .collect();
            let is_initial = first == 0;
            let is_final = first + group_channels == channels;
            let group_info = if is_initial {
                channel_info.as_deref()
            } else {
                None
            };
            self.write_pair_block(
                &group,
                group_channels,
                block_index,
                is_initial,
                is_final,
                group_info,
            )?;
        }
        self.encoded_frames += u64::from(frames);
        self.samples = samples;
        self.samples.clear();
        Ok(())
    }

    fn write_pair_block(
        &mut self,
        samples: &[i32],
        channels: usize,
        block_index: u64,
        initial: bool,
        final_block: bool,
        channel_info: Option<&[u8]>,
    ) -> Result<(), AudioError> {
        let format = self.spec.sample_format();
        let bytes_per_sample: u32 = match format {
            SampleFormat::PcmU8 => 1,
            SampleFormat::PcmI16 => 2,
            SampleFormat::PcmI24 => 3,
            SampleFormat::PcmI32 => 4,
            _ => unreachable!(),
        };
        let extended = format == SampleFormat::PcmI32;
        let coded: Vec<i32> = if extended {
            samples.iter().map(|sample| sample >> 8).collect()
        } else {
            samples.to_vec()
        };
        let bitrate = bitrate_accumulator(self.bitrate_kbps, self.spec)?;
        let HybridWords {
            main: main_bits,
            correction: correction_bits,
            lossy,
            profile,
        } = encode_hybrid_words(&coded, channels, bitrate)?;

        let mut flags = (bytes_per_sample - 1)
            | (u32::from(if extended {
                23
            } else {
                sample_bits(format) - 1
            }) << 18)
            | HAS_CHECKSUM
            | HYBRID
            | BITRATE_MODE;
        if channels == 1 {
            flags |= MONO_DATA;
        }
        if initial {
            flags |= INITIAL_BLOCK;
        }
        if final_block {
            flags |= FINAL_BLOCK;
        }
        if extended {
            flags |= INT32_DATA;
        }
        let rate_index = SAMPLE_RATES
            .iter()
            .position(|&rate| rate == self.spec.sample_rate());
        if let Some(index) = rate_index {
            flags |= (index as u32) << 23;
        } else {
            flags |= 15 << 23;
        }

        let mut main_payload = Vec::new();
        write_sub_block(&mut main_payload, 0x02, &[22])?;
        let mut entropy = Vec::with_capacity(channels * 6);
        for _ in 0..channels * 3 {
            entropy.extend_from_slice(&4096_u16.to_le_bytes());
        }
        write_sub_block(&mut main_payload, 0x05, &entropy)?;
        if let Some(info) = channel_info {
            write_sub_block(&mut main_payload, 0x0d, info)?;
        }
        if rate_index.is_none() {
            write_sub_block(
                &mut main_payload,
                0x27,
                &self.spec.sample_rate().to_le_bytes(),
            )?;
        }
        write_sub_block(&mut main_payload, 0x06, &profile)?;
        if extended {
            write_sub_block(&mut main_payload, 0x09, &[8, 0, 0, 0])?;
        }
        write_sub_block(&mut main_payload, 0x0a, &main_bits)?;
        let mut correction_wvx = None;
        if extended {
            correction_wvx = Some(extended_integer_wvx(samples));
        }

        let mut correction_payload = Vec::new();
        if !correction_bits.is_empty() {
            write_sub_block(&mut correction_payload, 0x0b, &correction_bits)?;
        }
        if let Some(wvx) = correction_wvx {
            write_sub_block(&mut correction_payload, 0x0c, &wvx)?;
        }
        let frame_count = u32::try_from(samples.len() / channels)
            .map_err(|_| AudioError::InvalidData("WavPack block is too large"))?;
        let main_crc = sample_checksum(&lossy, channels);
        let exact_crc = sample_checksum(&coded, channels);
        let main_header = make_header(
            32 + main_payload.len() + 6,
            block_index,
            frame_count,
            flags,
            main_crc,
        )?;
        let correction_header = make_header(
            32 + correction_payload.len() + 6,
            block_index,
            frame_count,
            flags,
            exact_crc,
        )?;
        let main_check = block_checksum(&main_header, &main_payload);
        let correction_check = block_checksum(&correction_header, &correction_payload);
        let main_offset = self.writer.stream_position()?;
        let correction_offset = self.correction.stream_position()?;
        self.blocks.push(BlockPatch {
            offset: main_offset,
            payload_words: main_payload.len() / 2,
            checksum: main_check,
            header: main_header,
        });
        self.correction_blocks.push(BlockPatch {
            offset: correction_offset,
            payload_words: correction_payload.len() / 2,
            checksum: correction_check,
            header: correction_header,
        });
        main_payload.extend_from_slice(&[0x2f, 2]);
        main_payload.extend_from_slice(&main_check.to_le_bytes());
        correction_payload.extend_from_slice(&[0x2f, 2]);
        correction_payload.extend_from_slice(&correction_check.to_le_bytes());
        self.writer.write_all(&main_header)?;
        self.writer.write_all(&main_payload)?;
        self.correction.write_all(&correction_header)?;
        self.correction.write_all(&correction_payload)?;
        Ok(())
    }
}

impl<W: Write + Seek, C: Write + Seek> AudioEncoder for WavPackHybridEncoder<W, C> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        if self.finished || self.poisoned {
            return Err(AudioError::InvalidData(
                "WavPack hybrid encoder is unusable",
            ));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "WavPack input PCM specification changed",
            ));
        }
        let frames = u64::try_from(block.frames())
            .map_err(|_| AudioError::InvalidData("WavPack frame count overflow"))?;
        let next_frames = self
            .frames
            .checked_add(frames)
            .filter(|&count| count < (1_u64 << 40))
            .ok_or(AudioError::InvalidData(
                "WavPack sample count exceeds 40 bits",
            ))?;
        let channels = usize::from(self.spec.channels());
        let result = match block.samples() {
            Samples::U8(values) => {
                self.push_samples(values.iter().map(|&value| i32::from(value) - 128), channels)
            }
            Samples::I16(values) => {
                self.push_samples(values.iter().map(|&value| i32::from(value)), channels)
            }
            Samples::I24(values) | Samples::I32(values) => {
                self.push_samples(values.iter().copied(), channels)
            }
            _ => Err(AudioError::Unsupported(
                "WavPack hybrid integer PCM required",
            )),
        };
        if result.is_err() {
            self.poisoned = true;
        } else {
            self.frames = next_frames;
        }
        result
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        if self.finished {
            return Ok(());
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "WavPack hybrid encoder is unusable",
            ));
        }
        if self.frames == 0 {
            return Err(AudioError::InvalidData(
                "cannot encode an empty WavPack stream",
            ));
        }
        let result = (|| {
            self.flush_block()?;
            patch_total_samples(&mut self.writer, self.frames, &self.blocks)?;
            patch_total_samples(&mut self.correction, self.frames, &self.correction_blocks)?;
            self.writer.seek(SeekFrom::End(0))?;
            self.correction.seek(SeekFrom::End(0))?;
            if self.metadata.iter().next().is_some() {
                let mut tag = ApeTag::new();
                tag.replace_text_metadata(&self.metadata)
                    .map_err(|_| AudioError::InvalidData("invalid WavPack APEv2 metadata"))?;
                self.writer.write_all(&tag.encode().map_err(|_| {
                    AudioError::InvalidData("WavPack APEv2 metadata exceeds the supported size")
                })?)?;
            }
            self.writer.flush()?;
            self.correction.flush()?;
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
        if self.finished {
            return Err(AudioError::InvalidData(
                "WavPack hybrid encoder is already finished",
            ));
        }
        self.metadata = metadata.clone();
        Ok(())
    }
}

impl<W: Write + Seek, C: Write + Seek> WavPackHybridEncoder<W, C> {
    fn push_samples(
        &mut self,
        values: impl Iterator<Item = i32>,
        channels: usize,
    ) -> Result<(), AudioError> {
        for sample in values {
            self.samples.push(sample);
            if self.samples.len() == BLOCK_FRAMES * channels {
                self.flush_block()?;
            }
        }
        Ok(())
    }
}

fn extended_integer_wvx(samples: &[i32]) -> Vec<u8> {
    let mut wvx = Vec::with_capacity(4 + samples.len());
    wvx.extend_from_slice(&wvx_integer_checksum(samples).to_le_bytes());
    let mut low = BitWriter::default();
    for sample in samples {
        for bit in 0..8 {
            low.write_bit(*sample as u32 & (1 << bit) != 0);
        }
    }
    wvx.extend_from_slice(&low.finish());
    if wvx.len() % 2 != 0 {
        wvx.push(0);
    }
    wvx
}

fn bitrate_accumulator(kbps: u32, spec: AudioSpec) -> Result<u16, AudioError> {
    let numerator = u64::from(kbps) * 256_000;
    let denominator = u64::from(spec.sample_rate()) * u64::from(spec.channels());
    let bits_per_sample = (numerator + denominator / 2) / denominator;
    Ok(bits_per_sample.min(64 * 256).saturating_sub(568) as u16)
}

fn sample_bits(format: SampleFormat) -> u8 {
    match format {
        SampleFormat::PcmU8 => 8,
        SampleFormat::PcmI16 => 16,
        SampleFormat::PcmI24 => 24,
        SampleFormat::PcmI32 => 32,
        _ => unreachable!(),
    }
}

fn encode_hybrid_words(
    samples: &[i32],
    channels: usize,
    bitrate: u16,
) -> Result<HybridWords, AudioError> {
    let mut main = BitWriter::default();
    let mut correction = BitWriter::default();
    let mut medians = [[32_768_u32; 3]; 2];
    let mut hybrid = HybridState::for_encoding(channels, bitrate, samples)?;
    let profile = hybrid.profile();
    let mut holding_zero = false;
    let mut holding_one = false;
    let mut lossy = Vec::with_capacity(samples.len());
    for (index, &sample) in samples.iter().enumerate() {
        let channel = index % channels;
        let magnitude = if sample < 0 {
            !(sample as u32)
        } else {
            sample as u32
        };
        if medians[0][0] & !1 == 0
            && (channels == 1 || medians[1][0] & !1 == 0)
            && !holding_zero
            && !holding_one
        {
            main.write_zero_run(0);
        }
        let median0 = get_median(medians[channel][0]);
        let (category, low, width) = if magnitude < median0 {
            update_median(&mut medians[channel][0], 0, false)?;
            (0, 0, median0)
        } else {
            let median1 = get_median(medians[channel][1]);
            if magnitude - median0 < median1 {
                update_median(&mut medians[channel][0], 0, true)?;
                update_median(&mut medians[channel][1], 1, false)?;
                (1, median0, median1)
            } else {
                let median2 = get_median(medians[channel][2]);
                let category = 2 + (magnitude - median0 - median1) / median2;
                let low = u64::from(median0)
                    + u64::from(median1)
                    + u64::from(category - 2) * u64::from(median2);
                update_median(&mut medians[channel][0], 0, true)?;
                update_median(&mut medians[channel][1], 1, true)?;
                update_median(&mut medians[channel][2], 2, category > 2)?;
                (
                    category,
                    u32::try_from(low)
                        .map_err(|_| AudioError::InvalidData("WavPack word range overflow"))?,
                    median2,
                )
            }
        };
        let next_channel = (channel + 1) % channels;
        let next = samples.get(index + 1).copied().unwrap_or(0);
        let next_magnitude = if next < 0 {
            !(next as u32)
        } else {
            next as u32
        };
        let next_category = category_for(next_magnitude, medians[next_channel]);
        if holding_zero {
            if category != 0 {
                return Err(AudioError::InvalidData("invalid WavPack entropy state"));
            }
            holding_zero = false;
            holding_one = false;
        } else {
            let raw = if holding_one {
                (category - 1) * 2 + u32::from(next_category != 0)
            } else {
                category * 2 + u32::from(next_category != 0)
            };
            main.write_magnitude_prefix(raw);
            holding_one = next_category != 0;
            holding_zero = !holding_one;
        }
        if channel == 0 {
            hybrid.prepare_frame()?;
        }
        let range_low = low & 0x7fff_ffff;
        let range_high = range_low.wrapping_add(width.saturating_sub(1)) & 0x7fff_ffff;
        let mut lo = range_low;
        let mut hi = range_high.max(range_low);
        let error = hybrid.error_limit(channel);
        if error == 0 {
            let offset = magnitude
                .checked_sub(lo)
                .filter(|&value| value <= hi - lo)
                .ok_or(AudioError::InvalidData(
                    "WavPack magnitude is outside its code range",
                ))?;
            main.write_code(hi - lo, offset)?;
        } else {
            let mut midpoint = (hi + lo + 1) >> 1;
            while hi - lo > error {
                if magnitude >= midpoint {
                    main.write_bit(true);
                    lo = midpoint;
                } else {
                    main.write_bit(false);
                    hi = midpoint - 1;
                }
                midpoint = (hi + lo + 1) >> 1;
            }
            let offset = magnitude
                .checked_sub(lo)
                .filter(|&value| value <= hi - lo)
                .ok_or(AudioError::InvalidData(
                    "WavPack hybrid correction range overflow",
                ))?;
            correction.write_code(hi - lo, offset)?;
        }
        let midpoint = if error == 0 {
            magnitude
        } else {
            (hi + lo + 1) >> 1
        };
        main.write_bit(sample < 0);
        lossy.push(if sample < 0 {
            !(midpoint as i32)
        } else {
            midpoint as i32
        });
        hybrid.record_word(channel, midpoint);
    }
    let mut main = main.finish();
    let mut correction = correction.finish();
    if main.len() % 2 != 0 {
        main.push(0);
    }
    if correction.len() % 2 != 0 {
        correction.push(0);
    }
    Ok(HybridWords {
        main,
        correction,
        lossy,
        profile,
    })
}

fn category_for(magnitude: u32, medians: [u32; 3]) -> u32 {
    let median0 = get_median(medians[0]);
    if magnitude < median0 {
        return 0;
    }
    let median1 = get_median(medians[1]);
    if magnitude - median0 < median1 {
        return 1;
    }
    let median2 = get_median(medians[2]);
    2 + (magnitude - median0 - median1) / median2
}

#[cfg(test)]
mod tests {
    use super::bitrate_accumulator;
    use zemer_core::{AudioSpec, SampleFormat};

    #[test]
    fn target_kbps_maps_to_the_standard_fixed_point_rate() {
        let stereo = AudioSpec::new(22_050, 2, SampleFormat::PcmI16).unwrap();
        let mono = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();

        assert_eq!(bitrate_accumulator(128, stereo).unwrap(), 175);
        assert_eq!(bitrate_accumulator(24, mono).unwrap(), 200);
    }

}
