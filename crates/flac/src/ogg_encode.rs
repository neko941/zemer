use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};

use ogg::OggPacketWriter;
use zemer_core::{AudioEncoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::{
    encode::encode_frame,
    rewrite::{DEFAULT_VENDOR, encode_comments},
};

const BLOCK_SIZE: usize = 4096;
const MAX_TOTAL_SAMPLES: u64 = 0x0f_ffff_ffff;
static NEXT_SERIAL: AtomicU32 = AtomicU32::new(0);

/// Streaming Ogg-FLAC encoder for integer PCM.
pub struct OggFlacEncoder<W> {
    writer: OggPacketWriter<W>,
    spec: AudioSpec,
    channels: usize,
    bits_per_sample: u8,
    pending: Vec<i64>,
    metadata: Metadata,
    frame_number: u64,
    total_samples: u64,
    started: bool,
    poisoned: bool,
    finished: bool,
}

impl<W: Write> OggFlacEncoder<W> {
    pub fn new(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
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
        let serial = NEXT_SERIAL
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(std::process::id());
        Ok(Self {
            writer: OggPacketWriter::new(writer, serial),
            spec,
            channels,
            bits_per_sample,
            pending: Vec::with_capacity(BLOCK_SIZE * channels),
            metadata: Metadata::default(),
            frame_number: 0,
            total_samples: 0,
            started: false,
            poisoned: false,
            finished: false,
        })
    }

    pub fn into_inner(self) -> W {
        self.writer.into_inner()
    }

    fn ensure_started(&mut self) -> Result<(), AudioError> {
        if self.started {
            return Ok(());
        }
        let mut metadata = self.metadata.clone();
        if let Some(mask) = self.spec.speaker_mask() {
            metadata.remove("WAVEFORMATEXTENSIBLE_CHANNEL_MASK");
            metadata.insert("WAVEFORMATEXTENSIBLE_CHANNEL_MASK", format!("0x{mask:x}"));
        }
        let comments = encode_comments(&metadata, DEFAULT_VENDOR)?;

        let mut header = Vec::with_capacity(51);
        header.extend_from_slice(b"\x7fFLAC\x01\x00");
        header.extend_from_slice(&1_u16.to_be_bytes());
        header.extend_from_slice(b"fLaC");
        header.extend_from_slice(&[0, 0, 0, 34]);
        let mut streaminfo = [0_u8; 34];
        streaminfo[..2].copy_from_slice(&(BLOCK_SIZE as u16).to_be_bytes());
        streaminfo[2..4].copy_from_slice(&(BLOCK_SIZE as u16).to_be_bytes());
        let packed = (u64::from(self.spec.sample_rate()) << 44)
            | (u64::from(self.channels as u8 - 1) << 41)
            | (u64::from(self.bits_per_sample - 1) << 36);
        streaminfo[10..18].copy_from_slice(&packed.to_be_bytes());
        header.extend_from_slice(&streaminfo);
        self.writer.write_packet(&header, 0)?;

        let comment_length = u32::try_from(comments.len())
            .map_err(|_| AudioError::InvalidData("FLAC comment block is too large"))?;
        let mut comment_packet = Vec::with_capacity(comments.len() + 4);
        comment_packet.push(0x84);
        comment_packet.extend_from_slice(&comment_length.to_be_bytes()[1..]);
        comment_packet.extend_from_slice(&comments);
        self.writer.write_packet(&comment_packet, 0)?;
        self.started = true;
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
        let samples = self
            .total_samples
            .checked_add(frames as u64)
            .filter(|&total| total <= MAX_TOTAL_SAMPLES)
            .ok_or(AudioError::Unsupported("FLAC sample count overflow"))?;
        let granule = samples - 1;
        self.writer.write_packet_owned(frame, granule)?;
        self.total_samples = samples;
        self.frame_number += 1;
        self.pending.clear();
        Ok(())
    }
}

impl<W: Write> AudioEncoder for OggFlacEncoder<W> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if self.finished || self.poisoned {
            return Err(AudioError::InvalidData(
                "Ogg-FLAC encoder is finished or unusable",
            ));
        }
        if self.started {
            return Err(AudioError::InvalidData(
                "Ogg-FLAC metadata cannot change after audio output starts",
            ));
        }
        self.metadata = metadata.clone();
        Ok(())
    }

    fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        if self.finished || self.poisoned {
            return Err(AudioError::InvalidData(
                "Ogg-FLAC encoder is finished or unusable",
            ));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "PCM block does not match the Ogg-FLAC encoder format",
            ));
        }
        let pending_frames = (self.pending.len() / self.channels) as u64;
        if self
            .total_samples
            .checked_add(pending_frames)
            .and_then(|count| count.checked_add(block.frames() as u64))
            .filter(|&count| count <= MAX_TOTAL_SAMPLES)
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
        if self.finished {
            return Ok(());
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "Ogg-FLAC encoder is unusable after an earlier write failure",
            ));
        }
        if self.frame_number == 0 && self.pending.is_empty() {
            return Err(AudioError::InvalidData(
                "cannot encode an empty Ogg-FLAC stream",
            ));
        }
        let result = self
            .emit_frame()
            .and_then(|()| self.writer.finish().map_err(AudioError::from));
        if result.is_err() {
            self.poisoned = true;
        } else {
            self.finished = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zemer_core::{
        AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples,
    };

    use crate::{OggFlacDecoder, OggFlacEncoder};

    #[test]
    fn streams_multiframe_ogg_flac_with_tags_and_terminal_granule() {
        let spec =
            AudioSpec::new_with_speaker_mask(48_000, 2, SampleFormat::PcmI16, Some(3)).unwrap();
        let samples = (0..9000)
            .map(|index| ((index * 37) % 60_001 - 30_000) as i16)
            .collect::<Vec<_>>();
        let input = PcmBuffer::new(spec, Samples::I16(samples.clone())).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "Ogg FLAC test");
        let mut encoder = OggFlacEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.set_metadata(&metadata).unwrap();
        encoder.write_block(&input).unwrap();
        encoder.finish().unwrap();

        let bytes = encoder.into_inner().into_inner();
        let mut packets = ogg::OggPacketReader::new(Cursor::new(bytes.as_slice()));
        let mut final_granule = None;
        while let Some((packet, owns_granule)) = packets.next_packet_with_granule().unwrap() {
            if packet.bytes.first() == Some(&0xff) && owns_granule {
                final_granule = Some(packet.page_granule_position);
            }
        }
        assert_eq!(final_granule, Some(4_499));

        let mut decoder = OggFlacDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.spec(), spec);
        assert_eq!(decoder.metadata().values("title"), ["Ogg FLAC test"]);
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(73).unwrap() {
            let Samples::I16(values) = block.samples() else {
                panic!("unexpected decoded sample type");
            };
            decoded.extend_from_slice(values);
        }
        assert_eq!(decoded, samples);
        assert!(decoder.read_frames(1).unwrap().is_none());
    }
}
