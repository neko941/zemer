use std::{
    collections::VecDeque,
    io::{BufReader, Read, Seek, SeekFrom},
};

use zemer_core::{
    AudioDecoder, AudioError, AudioSpec, Metadata, PcmBuffer, Picture, SampleFormat, Samples,
};

use crate::{
    bitstream::BitReader,
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
    audio_start: Option<u64>,
    seek_skip_frames: u64,
    seek_frame_sample: Option<u64>,
    variable_blocking: Option<bool>,
    verify_md5: bool,
}

impl<R: Read> FlacDecoder<R> {
    pub fn new(reader: R) -> Result<Self, AudioError> {
        let mut reader = BufReader::new(reader);
        let (info, mut metadata, metadata_blocks) = read_metadata(&mut reader)?;
        for block in &metadata_blocks {
            // Keep malformed picture blocks available through the raw API without rejecting audio.
            if let Ok(Some(picture)) = block.picture() {
                metadata.add_picture(Picture::new(
                    picture.picture_type,
                    picture.media_type,
                    picture.description,
                    picture.width,
                    picture.height,
                    picture.color_depth,
                    picture.colors,
                    picture.data.to_vec(),
                ));
            }
        }
        let format = output_format(info.bits_per_sample());
        let speaker_mask = metadata
            .values("WAVEFORMATEXTENSIBLE_CHANNEL_MASK")
            .first()
            .and_then(|value| parse_channel_mask(value));
        let spec = AudioSpec::new_with_speaker_mask(
            info.sample_rate(),
            u16::from(info.channels()),
            format,
            speaker_mask,
        )?;
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
            audio_start: None,
            seek_skip_frames: 0,
            seek_frame_sample: None,
            variable_blocking: None,
            verify_md5: true,
        })
    }

    pub fn stream_info(&self) -> &FlacStreamInfo {
        &self.info
    }

    pub fn metadata_blocks(&self) -> &[FlacMetadataBlock] {
        &self.metadata_blocks
    }

    /// Returns the input reader after a fully consumed FLAC stream.
    pub fn into_inner(self) -> Result<R, AudioError> {
        if !self.ended || !self.pending.is_empty() {
            return Err(AudioError::InvalidData(
                "FLAC reader can only be recovered after decoding to the end",
            ));
        }
        let (buffered, remaining_bits) = self.reader.into_inner();
        if remaining_bits != 0 || !buffered.buffer().is_empty() {
            return Err(AudioError::InvalidData(
                "FLAC decoder retained unread input at end of stream",
            ));
        }
        Ok(buffered.into_inner())
    }

    fn fill_pending(&mut self) -> Result<bool, AudioError> {
        if self.ended {
            return Ok(false);
        }
        let Some((samples, frames, frame_start, variable_blocking, coded_number)) =
            read_frame(&mut self.reader, &self.info)?
        else {
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
            if self.verify_md5
                && expected_md5 != [0; 16]
                && self.audio_md5.clone().finalize() != expected_md5
            {
                return Err(AudioError::InvalidData(
                    "FLAC decoded audio MD5 differs from STREAMINFO",
                ));
            }
            self.ended = true;
            return Ok(false);
        };
        if self
            .variable_blocking
            .is_some_and(|previous| previous != variable_blocking)
        {
            return Err(AudioError::InvalidData(
                "FLAC blocking strategy changed within the stream",
            ));
        }
        self.variable_blocking = Some(variable_blocking);
        let expected_frame_start = self.seek_frame_sample.unwrap_or(self.decoded_frames);
        // Before FLAC 1.2, variable-block files used sample numbers with the strategy bit unset.
        let legacy_variable = !variable_blocking
            && self.info.min_block_size() != self.info.max_block_size()
            && coded_number == expected_frame_start;
        if frame_start != expected_frame_start && !legacy_variable {
            if self.seek_frame_sample.is_some() {
                return Err(AudioError::InvalidData(
                    "FLAC seekpoint targets the wrong frame",
                ));
            }
            return Err(AudioError::InvalidData(
                "FLAC frame sequence is not contiguous",
            ));
        }
        self.seek_frame_sample = None;
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
        let mut audio_bytes = [0_u8; 4092];
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

impl<R: Read + Seek> FlacDecoder<R> {
    /// Creates a decoder that can seek within a native FLAC stream.
    pub fn new_seekable(mut reader: R) -> Result<Self, AudioError> {
        let start = reader.stream_position()?;
        let mut decoder = Self::new(reader)?;
        let metadata_size = decoder
            .metadata_blocks
            .iter()
            .try_fold(4_u64, |size, block| {
                size.checked_add(4)
                    .and_then(|size| size.checked_add(block.data().len() as u64))
                    .ok_or(AudioError::InvalidData("FLAC metadata size overflow"))
            })?;
        decoder.audio_start = Some(
            start
                .checked_add(metadata_size)
                .ok_or(AudioError::InvalidData("FLAC stream offset overflow"))?,
        );
        Ok(decoder)
    }

    /// Positions the next decoded sample frame at `sample` (zero-based).
    ///
    /// A valid SEEKTABLE point is used when available; otherwise seeking starts
    /// at the first audio frame and decodes forward. Whole-stream MD5 checking
    /// remains enabled only when decoding resumes from the beginning.
    pub fn seek_to_sample(&mut self, sample: u64) -> Result<(), AudioError> {
        if self
            .info
            .total_samples()
            .is_some_and(|total| sample > total)
        {
            return Err(AudioError::InvalidData(
                "FLAC seek target exceeds STREAMINFO total sample count",
            ));
        }
        let audio_start = self.audio_start.ok_or(AudioError::Unsupported(
            "FLAC seeking requires FlacDecoder::new_seekable",
        ))?;
        if self.info.total_samples() == Some(sample) {
            self.reader.get_mut().seek(SeekFrom::End(0))?;
            self.reader.reset();
            self.pending.clear();
            self.ended = true;
            self.decoded_frames = sample;
            self.seek_skip_frames = 0;
            self.seek_frame_sample = None;
            self.verify_md5 = false;
            self.audio_md5 = Md5::new();
            return Ok(());
        }

        let mut selected = None;
        for block in &self.metadata_blocks {
            let Some(table) = block.seektable()? else {
                continue;
            };
            for point in table.points() {
                if point.is_placeholder() || point.sample_number > sample {
                    continue;
                }
                let Some(offset) = audio_start.checked_add(point.stream_offset) else {
                    continue;
                };
                if sample != 0
                    && selected
                        .map(|(selected_sample, _)| point.sample_number >= selected_sample)
                        .unwrap_or(true)
                {
                    selected = Some((point.sample_number, offset));
                }
            }
        }
        let file_end = self.reader.get_mut().seek(SeekFrom::End(0))?;
        if selected.is_some_and(|(_, offset)| offset >= file_end) {
            selected = None;
        }

        let (base_sample, offset, seek_frame_sample) = if sample == 0 {
            (0, audio_start, None)
        } else if let Some((base_sample, offset)) = selected {
            (base_sample, offset, Some(base_sample))
        } else {
            (0, audio_start, None)
        };
        self.reader.get_mut().seek(SeekFrom::Start(offset))?;
        self.reader.reset();
        self.pending.clear();
        self.ended = false;
        self.decoded_frames = base_sample;
        self.seek_skip_frames = sample - base_sample;
        self.seek_frame_sample = seek_frame_sample;
        self.verify_md5 = base_sample == 0;
        self.audio_md5 = Md5::new();
        Ok(())
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
        loop {
            if self.pending.is_empty() && !self.fill_pending()? {
                if self.seek_skip_frames != 0 {
                    return Err(AudioError::InvalidData(
                        "FLAC seek target exceeds decoded stream length",
                    ));
                }
                return Ok(None);
            }
            if self.seek_skip_frames == 0 {
                break;
            }
            let channels = usize::from(self.info.channels());
            let pending_frames = self.pending.len() / channels;
            let skipped = self.seek_skip_frames.min(pending_frames as u64) as usize;
            self.pending.drain(..skipped * channels);
            self.seek_skip_frames -= skipped as u64;
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

fn parse_channel_mask(value: &str) -> Option<u32> {
    let digits = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))?;
    u32::from_str_radix(digits, 16).ok()
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

#[cfg(test)]
mod tests {
    use std::{io::Cursor, ops::Range};

    use super::parse_channel_mask;
    use crate::{FlacDecoder, FlacEncoder};
    use zemer_core::{AudioDecoder, AudioEncoder, AudioSpec, PcmBuffer, SampleFormat, Samples};

    #[test]
    fn parses_flac_channel_masks_as_case_insensitive_hex() {
        assert_eq!(parse_channel_mask("0x5003"), Some(0x5003));
        assert_eq!(parse_channel_mask("0X0008"), Some(0x8));
        assert_eq!(parse_channel_mask("8"), None);
        assert_eq!(parse_channel_mask("0x100000000"), None);
    }

    fn encoded_test_audio() -> (Vec<u8>, Vec<i16>) {
        let samples: Vec<i16> = (0..12_000)
            .map(|index| ((index * 97) % 60_001 - 30_000) as i16)
            .collect();
        let spec = AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap();
        let block = PcmBuffer::new(spec, Samples::I16(samples.clone())).unwrap();
        let mut writer = Cursor::new(Vec::new());
        {
            let mut encoder = FlacEncoder::new(&mut writer, spec).unwrap();
            encoder.write_block(&block).unwrap();
            encoder.finish().unwrap();
        }
        (writer.into_inner(), samples)
    }

    fn with_seekpoint(mut bytes: Vec<u8>, sample: u64, offset: u64, frame_samples: u16) -> Vec<u8> {
        let comment_header = 42;
        let mut seektable = Vec::with_capacity(22);
        seektable.extend_from_slice(&[3, 0, 0, 18]);
        seektable.extend_from_slice(&sample.to_be_bytes());
        seektable.extend_from_slice(&offset.to_be_bytes());
        seektable.extend_from_slice(&frame_samples.to_be_bytes());
        bytes.splice(comment_header..comment_header, seektable);
        bytes
    }

    fn frame_ranges(bytes: &[u8]) -> Vec<Range<usize>> {
        let decoder = FlacDecoder::new_seekable(Cursor::new(bytes.to_vec())).unwrap();
        let mut reader = crate::bitstream::BitReader::new(
            Cursor::new(bytes[decoder.audio_start.unwrap() as usize..].to_vec()),
            1 << 20,
        );
        let audio_start = decoder.audio_start.unwrap() as usize;
        let mut ranges = Vec::new();
        while crate::frame::read_frame(&mut reader, &decoder.info)
            .unwrap()
            .is_some()
        {
            let end = audio_start + reader.get_mut().position() as usize;
            let start = ranges
                .last()
                .map_or(audio_start, |frame: &Range<usize>| frame.end);
            ranges.push(start..end);
        }
        ranges
    }

    fn rechecksum_frame_header_and_body(bytes: &mut [u8], frame: &Range<usize>) {
        let header_crc = frame.start + 7;
        bytes[header_crc] = bytes[frame.start..header_crc]
            .iter()
            .copied()
            .fold(0, crate::bitstream::crc8_update);
        let frame_crc = bytes[frame.start..frame.end - 2]
            .iter()
            .copied()
            .fold(0, crate::bitstream::crc16_update);
        bytes[frame.end - 2..frame.end].copy_from_slice(&frame_crc.to_be_bytes());
    }

    #[test]
    fn rejects_a_gap_in_fixed_block_frame_numbers() {
        let (mut bytes, _) = encoded_test_audio();
        let frames = frame_ranges(&bytes);
        let repeated_first = bytes[frames[0].clone()].to_vec();
        bytes.splice(frames[1].clone(), repeated_first);

        let mut decoder = FlacDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.read_frames(4096).unwrap().unwrap().frames(), 4096);
        assert!(matches!(
            decoder.read_frames(4096),
            Err(zemer_core::AudioError::InvalidData(
                "FLAC frame sequence is not contiguous"
            ))
        ));
    }

    #[test]
    fn rejects_a_change_in_frame_blocking_strategy() {
        let (mut bytes, _) = encoded_test_audio();
        let frames = frame_ranges(&bytes);
        let second = &frames[1];
        bytes[second.start + 1] |= 1;
        rechecksum_frame_header_and_body(&mut bytes, second);
        let mut decoder = FlacDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.read_frames(4096).unwrap().unwrap().frames(), 4096);
        let result = decoder.read_frames(4096);
        assert!(
            matches!(
                result,
                Err(zemer_core::AudioError::InvalidData(
                    "FLAC blocking strategy changed within the stream"
                ))
            ),
            "unexpected second frame result: {result:?}"
        );
    }

    #[test]
    fn seeks_from_the_stream_start_and_preserves_md5_validation_at_zero() {
        let (bytes, samples) = encoded_test_audio();
        let mut decoder = FlacDecoder::new_seekable(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.read_frames(1).unwrap().unwrap().frames(), 1);

        for start in [0, 1, 4095, 4096, 4097, 8191, 11_999] {
            decoder.seek_to_sample(start).unwrap();
            let count = (samples.len() - start as usize).min(7);
            let mut actual = Vec::with_capacity(count);
            while actual.len() < count {
                let block = decoder.read_frames(count - actual.len()).unwrap().unwrap();
                let Samples::I16(values) = block.samples() else {
                    panic!("unexpected FLAC sample representation");
                };
                actual.extend_from_slice(values);
            }
            assert_eq!(
                actual,
                &samples[start as usize..start as usize + count],
                "seek start {start}"
            );
        }

        decoder.seek_to_sample(samples.len() as u64).unwrap();
        assert!(decoder.read_frames(1).unwrap().is_none());
        assert!(decoder.seek_to_sample(samples.len() as u64 + 1).is_err());

        decoder.seek_to_sample(0).unwrap();
        while decoder.read_frames(997).unwrap().is_some() {}
    }

    #[test]
    fn seeks_from_the_nearest_valid_seektable_point() {
        let (bytes, samples) = encoded_test_audio();
        let decoder = FlacDecoder::new_seekable(Cursor::new(bytes.clone())).unwrap();
        let audio_start = decoder.audio_start.unwrap() as usize;
        let mut first_frame = super::super::bitstream::BitReader::new(
            Cursor::new(bytes[audio_start..].to_vec()),
            1 << 20,
        );
        crate::frame::read_frame(&mut first_frame, &decoder.info)
            .unwrap()
            .unwrap();
        let second_frame_offset = first_frame.frame_bytes() as u64 + 2;
        let bytes = with_seekpoint(bytes, 4096, second_frame_offset, 4096);
        let mut decoder = FlacDecoder::new_seekable(Cursor::new(bytes)).unwrap();
        decoder.seek_to_sample(5000).unwrap();
        let block = decoder.read_frames(32).unwrap().unwrap();
        let Samples::I16(actual) = block.samples() else {
            panic!("unexpected FLAC sample representation");
        };
        assert_eq!(actual, &samples[5000..5032]);
    }

    #[test]
    fn rejects_seekpoints_that_land_on_the_wrong_frame() {
        let (bytes, _) = encoded_test_audio();
        let bytes = with_seekpoint(bytes, 4096, 0, 4096);
        let mut decoder = FlacDecoder::new_seekable(Cursor::new(bytes)).unwrap();
        decoder.seek_to_sample(5000).unwrap();
        assert!(matches!(
            decoder.read_frames(1),
            Err(zemer_core::AudioError::InvalidData(
                "FLAC seekpoint targets the wrong frame"
            ))
        ));
    }

    #[test]
    fn seeks_when_the_flac_stream_starts_at_a_nonzero_reader_position() {
        let (bytes, samples) = encoded_test_audio();
        let mut input = Cursor::new([b"prefix".as_slice(), &bytes].concat());
        input.set_position(6);
        let mut decoder = FlacDecoder::new_seekable(input).unwrap();
        decoder.seek_to_sample(5000).unwrap();
        let block = decoder.read_frames(12).unwrap().unwrap();
        let Samples::I16(actual) = block.samples() else {
            panic!("unexpected FLAC sample representation");
        };
        assert_eq!(actual, &samples[5000..5012]);
    }

    #[test]
    fn streaming_constructor_reports_seeking_as_unsupported() {
        let (bytes, _) = encoded_test_audio();
        let mut decoder = FlacDecoder::new(Cursor::new(bytes)).unwrap();
        assert!(decoder.seek_to_sample(0).is_err());
    }
}
