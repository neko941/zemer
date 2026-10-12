use std::io::{Seek, SeekFrom, Write};

use zemer_core::{AudioEncoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::{
    bitstream::{crc8_update, crc16_update},
    frame::fixed_prediction,
    md5::Md5,
    rewrite::{DEFAULT_VENDOR, encode_comments, write_block},
};

const BLOCK_SIZE: usize = 4096;
const MAX_TOTAL_SAMPLES: u64 = 0x0f_ffff_ffff;

/// Streaming native FLAC encoder for integer PCM.
///
/// Frames use the verbatim subframe, so the output is lossless but larger than
/// output from an encoder that searches for predictors and Rice coding.
pub struct FlacEncoder<W> {
    writer: W,
    spec: AudioSpec,
    channels: usize,
    bits_per_sample: u8,
    pending: Vec<i64>,
    metadata: Metadata,
    start: u64,
    stream_info_offset: Option<u64>,
    frame_number: u64,
    total_samples: u64,
    min_frame_size: u32,
    max_frame_size: u32,
    audio_md5: Md5,
    poisoned: bool,
    finished: bool,
}

impl<W: Write + Seek> FlacEncoder<W> {
    pub fn new(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_with_metadata(writer, spec, &Metadata::default())
    }

    pub fn new_with_metadata(
        mut writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        let bits_per_sample = match spec.sample_format() {
            SampleFormat::PcmU8 => 8,
            SampleFormat::PcmI16 => 16,
            SampleFormat::PcmI24 => 24,
            SampleFormat::PcmI32 => 32,
            SampleFormat::Float32 | SampleFormat::Float64 => {
                return Err(AudioError::Unsupported(
                    "FLAC encoding requires integer PCM samples",
                ));
            }
        };
        if !(1..=8).contains(&spec.channels()) {
            return Err(AudioError::Unsupported(
                "FLAC encoding supports one through eight channels",
            ));
        }
        if spec.sample_rate() > 0x0f_ffff {
            return Err(AudioError::Unsupported(
                "sample rate exceeds the FLAC STREAMINFO limit",
            ));
        }
        let channels = usize::from(spec.channels());
        let start = writer.stream_position()?;
        Ok(Self {
            writer,
            spec,
            channels,
            bits_per_sample,
            pending: Vec::with_capacity(BLOCK_SIZE * channels),
            metadata: metadata.clone(),
            start,
            stream_info_offset: None,
            frame_number: 0,
            total_samples: 0,
            min_frame_size: u32::MAX,
            max_frame_size: 0,
            audio_md5: Md5::new(),
            poisoned: false,
            finished: false,
        })
    }

    fn ensure_started(&mut self) -> Result<(), AudioError> {
        if self.stream_info_offset.is_some() {
            return Ok(());
        }
        let mut metadata = self.metadata.clone();
        if let Some(mask) = self.spec.speaker_mask() {
            metadata.remove("WAVEFORMATEXTENSIBLE_CHANNEL_MASK");
            metadata.insert("WAVEFORMATEXTENSIBLE_CHANNEL_MASK", format!("0x{mask:x}"));
        }
        let comments = encode_comments(&metadata, DEFAULT_VENDOR)?;
        self.writer.seek(SeekFrom::Start(self.start))?;
        self.writer.write_all(b"fLaC")?;
        self.writer.write_all(&[0, 0, 0, 34])?;
        self.stream_info_offset = Some(self.writer.stream_position()?);
        self.writer.write_all(&[0; 34])?;
        write_block(&mut self.writer, 4, &comments, true)?;
        Ok(())
    }

    fn append_sample(&mut self, sample: i64) -> Result<(), AudioError> {
        self.pending.push(sample);
        if self.pending.len() == BLOCK_SIZE * self.channels {
            self.emit_frame()?;
        }
        Ok(())
    }

    fn emit_frame(&mut self) -> Result<(), AudioError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if self.frame_number > 0x7fff_ffff {
            return Err(AudioError::Unsupported(
                "FLAC fixed frame number exceeds the format limit",
            ));
        }
        self.ensure_started()?;
        let frames = self.pending.len() / self.channels;
        let frame = encode_frame(
            &self.pending,
            self.channels,
            self.bits_per_sample,
            self.frame_number,
        )?;
        self.writer.write_all(&frame)?;
        let bytes_per_sample = usize::from(self.bits_per_sample).div_ceil(8);
        for sample in &self.pending {
            self.audio_md5
                .update(&sample.to_le_bytes()[..bytes_per_sample]);
        }
        self.total_samples += frames as u64;
        self.frame_number += 1;
        let frame_size = frame.len() as u32;
        self.min_frame_size = self.min_frame_size.min(frame_size);
        self.max_frame_size = self.max_frame_size.max(frame_size);
        self.pending.clear();
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), AudioError> {
        if self.finished {
            return Ok(());
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "FLAC encoder is unusable after an earlier I/O failure",
            ));
        }
        let result = self.finalize_output();
        if result.is_ok() {
            self.finished = true;
        } else {
            self.poisoned = true;
        }
        result
    }

    fn finalize_output(&mut self) -> Result<(), AudioError> {
        self.emit_frame()?;
        if self.frame_number == 0 {
            return Err(AudioError::InvalidData(
                "cannot encode an empty FLAC stream",
            ));
        }
        self.ensure_started()?;
        let info_offset = self.stream_info_offset.ok_or(AudioError::InvalidData(
            "FLAC encoder did not write STREAMINFO",
        ))?;
        let info = self.encode_stream_info()?;
        let end = self.writer.seek(SeekFrom::End(0))?;
        self.writer.seek(SeekFrom::Start(info_offset))?;
        self.writer.write_all(&info)?;
        self.writer.seek(SeekFrom::Start(end))?;
        self.writer.flush()?;
        Ok(())
    }

    fn encode_stream_info(&self) -> Result<[u8; 34], AudioError> {
        let empty = self.frame_number == 0;
        let min_frame_size = if empty { 0 } else { self.min_frame_size };
        let max_frame_size = if empty { 0 } else { self.max_frame_size };
        if min_frame_size > 0x00ff_ffff || max_frame_size > 0x00ff_ffff {
            return Err(AudioError::Unsupported(
                "FLAC frame size exceeds the STREAMINFO limit",
            ));
        }
        let mut data = [0; 34];
        data[..2].copy_from_slice(&(BLOCK_SIZE as u16).to_be_bytes());
        data[2..4].copy_from_slice(&(BLOCK_SIZE as u16).to_be_bytes());
        put_u24(&mut data[4..7], min_frame_size);
        put_u24(&mut data[7..10], max_frame_size);
        let packed = (u64::from(self.spec.sample_rate()) << 44)
            | (u64::from(self.channels as u8 - 1) << 41)
            | (u64::from(self.bits_per_sample - 1) << 36)
            | self.total_samples;
        data[10..18].copy_from_slice(&packed.to_be_bytes());
        data[18..].copy_from_slice(&self.audio_md5.clone().finalize());
        Ok(data)
    }
}

impl<W: Write + Seek> AudioEncoder for FlacEncoder<W> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData("FLAC encoder is already finalized"));
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "FLAC encoder is unusable after an earlier I/O failure",
            ));
        }
        if self.stream_info_offset.is_some() {
            return Err(AudioError::InvalidData(
                "FLAC metadata cannot change after audio output starts",
            ));
        }
        self.metadata = metadata.clone();
        Ok(())
    }

    fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData("FLAC encoder is already finalized"));
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "FLAC encoder is unusable after an earlier I/O failure",
            ));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "PCM block does not match the FLAC encoder format",
            ));
        }
        let pending_frames = (self.pending.len() / self.channels) as u64;
        let buffered_total = self
            .total_samples
            .checked_add(pending_frames)
            .ok_or(AudioError::Unsupported("FLAC sample count overflow"))?;
        if buffered_total
            .checked_add(block.frames() as u64)
            .filter(|&total| total <= MAX_TOTAL_SAMPLES)
            .is_none()
        {
            return Err(AudioError::Unsupported(
                "FLAC total sample count exceeds the format limit",
            ));
        }

        let result = match block.samples() {
            Samples::U8(values) => values
                .iter()
                .try_for_each(|&sample| self.append_sample(i64::from(sample) - 128)),
            Samples::I16(values) => values
                .iter()
                .try_for_each(|&sample| self.append_sample(i64::from(sample))),
            Samples::I24(values) | Samples::I32(values) => values
                .iter()
                .try_for_each(|&sample| self.append_sample(i64::from(sample))),
            Samples::F32(_) | Samples::F64(_) => Err(AudioError::InvalidData(
                "PCM block sample storage does not match the FLAC encoder format",
            )),
        };
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        self.finalize()
    }
}

pub(crate) fn encode_frame(
    samples: &[i64],
    channels: usize,
    bits_per_sample: u8,
    frame_number: u64,
) -> Result<Vec<u8>, AudioError> {
    let frames = samples.len() / channels;
    let block_size = u16::try_from(frames)
        .ok()
        .filter(|&size| (1..=65_535).contains(&size))
        .ok_or(AudioError::InvalidData("invalid FLAC frame block size"))?;
    let mut header = BitWriter::default();
    header.write_bits(0x3ffe, 14)?;
    header.write_bits(0, 1)?;
    header.write_bits(0, 1)?;
    header.write_bits(7, 4)?;
    header.write_bits(0, 4)?;
    header.write_bits((channels - 1) as u64, 4)?;
    header.write_bits(0, 3)?;
    header.write_bits(0, 1)?;
    header.write_bytes(&encode_utf8_number(frame_number)?)?;
    header.write_bits(u64::from(block_size - 1), 16)?;
    let mut bytes = header.finish();
    let crc8 = bytes.iter().fold(0, |crc, &byte| crc8_update(crc, byte));
    bytes.push(crc8);

    let mut payload = BitWriter::default();
    for channel in 0..channels {
        let channel_samples = (0..frames)
            .map(|frame| samples[frame * channels + channel])
            .collect::<Vec<_>>();
        encode_subframe(&mut payload, &channel_samples, bits_per_sample)?;
    }
    bytes.extend_from_slice(&payload.finish());
    let crc16 = bytes.iter().fold(0, |crc, &byte| crc16_update(crc, byte));
    bytes.extend_from_slice(&crc16.to_be_bytes());
    Ok(bytes)
}

fn encode_subframe(
    writer: &mut BitWriter,
    samples: &[i64],
    bits_per_sample: u8,
) -> Result<(), AudioError> {
    let verbatim_bits = 8_u64 + samples.len() as u64 * u64::from(bits_per_sample);
    let mut best = None;
    for order in 0..=samples.len().min(4) {
        let mut residuals = Vec::with_capacity(samples.len() - order);
        let mut valid = true;
        for index in order..samples.len() {
            let Some(prediction) = fixed_prediction(samples, index, order) else {
                valid = false;
                break;
            };
            let Some(residual) = samples[index].checked_sub(prediction) else {
                valid = false;
                break;
            };
            residuals.push(residual);
        }
        if !valid {
            continue;
        }
        let warmup_bits = order as u64 * u64::from(bits_per_sample);
        for rice_parameter in 0..15_u8 {
            let residual_bits = residuals.iter().fold(0_u64, |bits, residual| {
                let folded = ((*residual as u64) << 1) ^ ((*residual >> 63) as u64);
                bits.saturating_add((folded >> rice_parameter) + 1 + u64::from(rice_parameter))
            });
            let cost = 18_u64
                .saturating_add(warmup_bits)
                .saturating_add(residual_bits);
            if cost < verbatim_bits
                && best
                    .as_ref()
                    .is_none_or(|(best_cost, _, _)| cost < *best_cost)
            {
                best = Some((cost, order, rice_parameter));
            }
        }
    }

    let Some((_, order, rice_parameter)) = best else {
        writer.write_bits(0, 1)?;
        writer.write_bits(1, 6)?;
        writer.write_bits(0, 1)?;
        for &sample in samples {
            writer.write_bits(sample as u64, bits_per_sample)?;
        }
        return Ok(());
    };
    let residuals = (order..samples.len())
        .map(|index| {
            let prediction = fixed_prediction(samples, index, order)
                .ok_or(AudioError::InvalidData("FLAC fixed predictor overflow"))?;
            samples[index]
                .checked_sub(prediction)
                .ok_or(AudioError::InvalidData("FLAC residual overflow"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    writer.write_bits(0, 1)?;
    writer.write_bits(8 + order as u64, 6)?;
    writer.write_bits(0, 1)?;
    for &sample in &samples[..order] {
        writer.write_bits(sample as u64, bits_per_sample)?;
    }
    writer.write_bits(0, 2)?;
    writer.write_bits(0, 4)?;
    writer.write_bits(u64::from(rice_parameter), 4)?;
    for residual in residuals {
        let folded = ((residual as u64) << 1) ^ ((residual >> 63) as u64);
        let quotient = folded >> rice_parameter;
        for _ in 0..quotient {
            writer.write_bits(0, 1)?;
        }
        writer.write_bits(1, 1)?;
        writer.write_bits(folded & ((1_u64 << rice_parameter) - 1), rice_parameter)?;
    }
    Ok(())
}

fn encode_utf8_number(value: u64) -> Result<Vec<u8>, AudioError> {
    if value < 0x80 {
        return Ok(vec![value as u8]);
    }
    for length in 2..=6_u8 {
        let payload_bits = 5 * u32::from(length) + 1;
        if value < (1_u64 << payload_bits) {
            let continuation_bits = 6 * (u32::from(length) - 1);
            let prefix = u8::MAX << (8 - length);
            let first = prefix | ((value >> continuation_bits) as u8);
            let mut output = vec![first];
            for index in (0..length - 1).rev() {
                output.push(0x80 | ((value >> (u32::from(index) * 6)) as u8 & 0x3f));
            }
            return Ok(output);
        }
    }
    Err(AudioError::Unsupported(
        "FLAC coded frame number exceeds the format limit",
    ))
}

fn put_u24(target: &mut [u8], value: u32) {
    target.copy_from_slice(&value.to_be_bytes()[1..]);
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    byte: u8,
    used: u8,
}

impl BitWriter {
    fn write_bits(&mut self, value: u64, count: u8) -> Result<(), AudioError> {
        if count > 64 {
            return Err(AudioError::InvalidData("invalid FLAC bit count"));
        }
        for bit in (0..count).rev() {
            self.byte = (self.byte << 1) | ((value >> bit) as u8 & 1);
            self.used += 1;
            if self.used == 8 {
                self.bytes.push(self.byte);
                self.byte = 0;
                self.used = 0;
            }
        }
        Ok(())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), AudioError> {
        for &byte in bytes {
            self.write_bits(u64::from(byte), 8)?;
        }
        Ok(())
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used != 0 {
            self.bytes.push(self.byte << (8 - self.used));
        }
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zemer_core::{AudioDecoder, AudioEncoder, AudioSpec, PcmBuffer, SampleFormat, Samples};

    use crate::{FlacDecoder, FlacEncoder};

    #[test]
    fn round_trips_integer_pcm_and_metadata_for_supported_depths() {
        for (format, samples) in [
            (SampleFormat::PcmU8, Samples::U8(vec![0, 128, 255, 32])),
            (
                SampleFormat::PcmI16,
                Samples::I16(vec![-32768, 1, 32767, -17]),
            ),
            (
                SampleFormat::PcmI24,
                Samples::I24(vec![-8_388_608, 1, 8_388_607, -17]),
            ),
            (
                SampleFormat::PcmI32,
                Samples::I32(vec![i32::MIN, 1, i32::MAX, -17]),
            ),
        ] {
            let spec = AudioSpec::new(48_000, 2, format).unwrap();
            let input = PcmBuffer::new(spec, samples).unwrap();
            let mut metadata = zemer_core::Metadata::default();
            metadata.insert("title", "round trip");
            let mut encoder = FlacEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
            encoder.set_metadata(&metadata).unwrap();
            encoder.write_block(&input).unwrap();
            encoder.finish().unwrap();
            let mut decoder = FlacDecoder::new(Cursor::new(encoder.writer.into_inner())).unwrap();
            assert_eq!(decoder.metadata().values("title"), ["round trip"]);
            let decoded = decoder.read_frames(16).unwrap().unwrap();
            assert_eq!(decoded, input);
            assert!(decoder.read_frames(16).unwrap().is_none());
        }
    }

    #[test]
    fn streams_multiple_frames_and_validates_the_final_md5() {
        let spec = AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap();
        let input =
            PcmBuffer::new(spec, Samples::I16((0..9000).map(|i| i - 4500).collect())).unwrap();
        let mut encoder = FlacEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_block(&input).unwrap();
        encoder.finish().unwrap();
        let mut decoder = FlacDecoder::new(Cursor::new(encoder.writer.into_inner())).unwrap();
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(777).unwrap() {
            let Samples::I16(values) = block.samples() else {
                panic!("unexpected decoded sample type");
            };
            decoded.extend_from_slice(values);
        }
        assert_eq!(
            decoded,
            match input.samples() {
                Samples::I16(v) => v.clone(),
                _ => unreachable!(),
            }
        );
    }

    #[test]
    fn uses_fixed_predictors_and_rice_coding_for_redundant_audio() {
        for samples in [
            vec![0_i16; 4096],
            (0..4096).map(|index| index as i16).collect(),
        ] {
            let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
            let input = PcmBuffer::new(spec, Samples::I16(samples.clone())).unwrap();
            let mut encoder = FlacEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
            encoder.write_block(&input).unwrap();
            encoder.finish().unwrap();
            let bytes = encoder.writer.into_inner();
            assert!(bytes.len() < 1024, "predictive frame was not selected");

            let mut decoder = FlacDecoder::new(Cursor::new(bytes)).unwrap();
            let mut decoded = Vec::new();
            while let Some(block) = decoder.read_frames(4096).unwrap() {
                let Samples::I16(values) = block.samples() else {
                    panic!("unexpected decoded sample type");
                };
                decoded.extend_from_slice(values);
            }
            assert_eq!(decoded, samples);
        }
    }

    #[test]
    fn decodes_24_bit_samples_across_internal_md5_buffer_boundaries() {
        let samples: Vec<i32> = (0..1400)
            .map(|index| (index * 31) % 16_000_001 - 8_000_000)
            .collect();
        let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI24).unwrap();
        let input = PcmBuffer::new(spec, Samples::I24(samples.clone())).unwrap();
        let mut encoder = FlacEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_block(&input).unwrap();
        encoder.finish().unwrap();

        let mut decoder = FlacDecoder::new(Cursor::new(encoder.writer.into_inner())).unwrap();
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(317).unwrap() {
            let Samples::I24(values) = block.samples() else {
                panic!("unexpected FLAC sample representation");
            };
            decoded.extend_from_slice(values);
        }
        assert_eq!(decoded, samples);
    }

    #[test]
    fn rejects_empty_streams_and_non_integer_specs() {
        let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
        let mut encoder = FlacEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        assert!(encoder.finish().is_err());
        assert!(encoder.writer.get_ref().is_empty());

        let float = AudioSpec::new(48_000, 2, SampleFormat::Float32).unwrap();
        assert!(FlacEncoder::new(Cursor::new(Vec::new()), float).is_err());

        let too_many_channels = AudioSpec::new(48_000, 9, SampleFormat::PcmI16).unwrap();
        assert!(FlacEncoder::new(Cursor::new(Vec::new()), too_many_channels).is_err());
    }

    #[test]
    fn preserves_speaker_mask_through_the_flac_comment_convention() {
        let spec =
            AudioSpec::new_with_speaker_mask(48_000, 2, SampleFormat::PcmI16, Some(0x3)).unwrap();
        let input = PcmBuffer::new(spec, Samples::I16(vec![10, -10])).unwrap();
        let mut encoder = FlacEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_block(&input).unwrap();
        encoder.finish().unwrap();
        let mut decoder = FlacDecoder::new(Cursor::new(encoder.writer.into_inner())).unwrap();
        assert_eq!(decoder.spec().speaker_mask(), Some(0x3));
        assert_eq!(
            decoder
                .metadata()
                .values("WAVEFORMATEXTENSIBLE_CHANNEL_MASK"),
            ["0x3"]
        );
        assert_eq!(decoder.read_frames(1).unwrap().unwrap(), input);
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn encodes_flac_frame_numbers_with_the_extended_utf8_ranges() {
        assert_eq!(super::encode_utf8_number(0x7f).unwrap(), [0x7f]);
        assert_eq!(super::encode_utf8_number(0x80).unwrap(), [0xc2, 0x80]);
        assert_eq!(
            super::encode_utf8_number(0x800).unwrap(),
            [0xe0, 0xa0, 0x80]
        );
        assert_eq!(
            super::encode_utf8_number(0x200000).unwrap(),
            [0xf8, 0x88, 0x80, 0x80, 0x80]
        );
        assert_eq!(
            super::encode_utf8_number(0x7fff_ffff).unwrap(),
            [0xfd, 0xbf, 0xbf, 0xbf, 0xbf, 0xbf]
        );
    }
}
