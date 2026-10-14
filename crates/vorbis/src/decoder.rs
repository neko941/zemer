use std::collections::VecDeque;
use std::io::Read;

use ogg::PAGE_FLAG_EOS;
use zemer_core::{AudioDecoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::VorbisStreamReader;
use crate::stream::TimeDomainBlock;

const MAX_READ_FRAMES: usize = 8192;
const MAX_EOS_PAGE_PCM_BYTES: usize = 64 * 1024 * 1024;

/// Streaming Ogg Vorbis decoder that returns interleaved float32 PCM.
pub struct VorbisDecoder<R> {
    stream: Option<VorbisStreamReader<R>>,
    metadata: Metadata,
    spec: AudioSpec,
    previous: Option<TimeDomainBlock>,
    ready: VecDeque<f32>,
    pending_pcm: Option<(Vec<f32>, usize)>,
    eos_page: Vec<f32>,
    committed_frames: u64,
    pending_granule: Option<u64>,
    initial_position_pending: bool,
    stream_finished: bool,
    finished: bool,
}

impl<R: Read> VorbisDecoder<R> {
    pub fn new(reader: R) -> Result<Self, AudioError> {
        let stream = VorbisStreamReader::new(reader)?;
        let spec = audio_spec(&stream)?;
        let metadata = stream.metadata().clone();
        Ok(Self {
            stream: Some(stream),
            metadata,
            spec,
            previous: None,
            ready: VecDeque::new(),
            pending_pcm: None,
            eos_page: Vec::new(),
            committed_frames: 0,
            pending_granule: None,
            initial_position_pending: true,
            stream_finished: false,
            finished: false,
        })
    }

    fn decode_next_packet(&mut self) -> Result<(), AudioError> {
        let (packet, eos_granule) = {
            let stream = self.stream.as_mut().ok_or(AudioError::InvalidData(
                "Vorbis decoder has no active logical stream",
            ))?;
            (
                stream.next_packet_with_granule()?,
                stream.eos_granule_position(),
            )
        };
        let Some((packet, owns_granule)) = packet else {
            if let Some(granule) = eos_granule {
                self.finish_eos_page(granule, usize::from(self.spec.channels()))?;
            } else {
                self.stream_finished = true;
            }
            return Ok(());
        };
        self.validate_pending_granule()?;
        let eos_page = packet.page_flags & PAGE_FLAG_EOS != 0;
        let Some(first_byte) = packet.bytes.first() else {
            return Err(AudioError::InvalidFormat("empty Vorbis audio packet"));
        };
        if first_byte & 1 != 0 {
            if eos_page && owns_granule {
                self.finish_eos_page(
                    packet.page_granule_position,
                    usize::from(self.spec.channels()),
                )?;
            } else {
                self.record_intermediate_granule(&packet, owns_granule);
            }
            return Ok(());
        }
        let block = self
            .stream
            .as_mut()
            .ok_or(AudioError::InvalidData(
                "Vorbis decoder has no active logical stream",
            ))?
            .decode_time_block(&packet)?;
        let Some(previous) = self.previous.replace(block) else {
            if eos_page && owns_granule {
                self.finish_eos_page(
                    packet.page_granule_position,
                    usize::from(self.spec.channels()),
                )?;
            } else {
                self.record_intermediate_granule(&packet, owns_granule);
            }
            return Ok(());
        };
        let current = self.previous.as_ref().ok_or(AudioError::InvalidData(
            "Vorbis overlap state was not initialized",
        ))?;
        let chunk = overlap_add(&previous, current)?;
        let channels = usize::from(self.spec.channels());
        let mut chunk_frames = chunk.len() / channels;
        let mut interleaved = interleave_f32(chunk, channels)?;

        if self.initial_position_pending {
            self.initial_position_pending = false;
            if !eos_page && owns_granule {
                let granule = packet.page_granule_position;
                if granule != ogg::NO_GRANULE_POSITION {
                    let decoded_frames = chunk_frames as u64;
                    if granule != decoded_frames && !packet.ends_page {
                        return Err(AudioError::InvalidFormat(
                            "non-zero initial Vorbis granule requires a fresh page",
                        ));
                    }
                    if granule < decoded_frames {
                        let trim_frames = usize::try_from(decoded_frames - granule)
                            .map_err(|_| AudioError::InvalidData("Vorbis trim is too large"))?;
                        let trim_samples = trim_frames
                            .checked_mul(channels)
                            .ok_or(AudioError::InvalidData("Vorbis trim size overflow"))?;
                        interleaved.drain(..trim_samples);
                        chunk_frames -= trim_frames;
                    } else {
                        self.committed_frames = granule - decoded_frames;
                    }
                }
            }
        }

        if eos_page {
            self.append_pending_to_eos_page()?;
            self.append_eos_pcm(&interleaved)?;
            if owns_granule {
                self.finish_eos_page(packet.page_granule_position, channels)?;
            }
        } else {
            if let Some((pending, pending_frames)) =
                self.pending_pcm.replace((interleaved, chunk_frames))
            {
                self.ready.extend(pending);
                self.committed_frames = self
                    .committed_frames
                    .checked_add(pending_frames as u64)
                    .ok_or(AudioError::InvalidData("Vorbis PCM frame count overflow"))?;
            }
            self.record_intermediate_granule(&packet, owns_granule);
        }
        Ok(())
    }

    fn record_intermediate_granule(&mut self, packet: &ogg::OggPacket, owns_granule: bool) {
        if !self.initial_position_pending
            && owns_granule
            && packet.page_granule_position != ogg::NO_GRANULE_POSITION
        {
            self.pending_granule = Some(packet.page_granule_position);
        }
    }

    fn validate_pending_granule(&mut self) -> Result<(), AudioError> {
        let Some(granule) = self.pending_granule.take() else {
            return Ok(());
        };
        let pending_frames = self
            .pending_pcm
            .as_ref()
            .map_or(0, |(_, frames)| *frames as u64);
        let expected = self
            .committed_frames
            .checked_add(pending_frames)
            .ok_or(AudioError::InvalidData("Vorbis PCM frame count overflow"))?;
        if granule != expected {
            return Err(AudioError::InvalidFormat(
                "Vorbis page granule does not match decoded PCM",
            ));
        }
        Ok(())
    }

    fn append_pending_to_eos_page(&mut self) -> Result<(), AudioError> {
        if let Some((pending, _)) = self.pending_pcm.take() {
            self.append_eos_pcm(&pending)?;
        }
        Ok(())
    }

    fn append_eos_pcm(&mut self, samples: &[f32]) -> Result<(), AudioError> {
        let new_len = self
            .eos_page
            .len()
            .checked_add(samples.len())
            .filter(|&len| {
                len.checked_mul(std::mem::size_of::<f32>())
                    .is_some_and(|bytes| bytes <= MAX_EOS_PAGE_PCM_BYTES)
            })
            .ok_or(AudioError::InvalidData(
                "Vorbis final Ogg page exceeds the PCM buffer limit",
            ))?;
        self.eos_page
            .try_reserve(new_len - self.eos_page.len())
            .map_err(|_| AudioError::InvalidData("Vorbis PCM allocation failed"))?;
        self.eos_page.extend_from_slice(samples);
        Ok(())
    }

    fn finish_eos_page(&mut self, granule: u64, channels: usize) -> Result<(), AudioError> {
        self.pending_granule = None;
        self.append_pending_to_eos_page()?;
        if granule == ogg::NO_GRANULE_POSITION || granule < self.committed_frames {
            return Err(AudioError::InvalidFormat(
                "invalid Vorbis EOS granule position",
            ));
        }
        let keep_frames = usize::try_from(granule - self.committed_frames)
            .map_err(|_| AudioError::InvalidData("Vorbis EOS granule is too large"))?;
        let keep_samples = keep_frames
            .checked_mul(channels)
            .ok_or(AudioError::InvalidData("Vorbis EOS sample count overflow"))?;
        if keep_samples > self.eos_page.len() {
            return Err(AudioError::InvalidFormat(
                "Vorbis EOS granule exceeds decoded PCM",
            ));
        }
        self.ready
            .try_reserve(keep_samples)
            .map_err(|_| AudioError::InvalidData("Vorbis PCM allocation failed"))?;
        self.ready.extend(self.eos_page.drain(..keep_samples));
        self.eos_page.clear();
        self.stream_finished = true;
        Ok(())
    }

    fn advance_stream(&mut self) -> Result<(), AudioError> {
        let stream = self.stream.take().ok_or(AudioError::InvalidData(
            "Vorbis decoder has no active logical stream",
        ))?;
        if let Some(next) = stream.next_stream()? {
            self.spec = audio_spec(&next)?;
            self.stream = Some(next);
            self.previous = None;
            self.pending_pcm = None;
            self.eos_page.clear();
            self.committed_frames = 0;
            self.pending_granule = None;
            self.initial_position_pending = true;
            self.stream_finished = false;
        } else {
            self.finished = true;
        }
        Ok(())
    }
}

impl<R: Read> AudioDecoder for VorbisDecoder<R> {
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
        let frame_limit = max_frames.min(MAX_READ_FRAMES);
        loop {
            let target_samples = frame_limit
                .checked_mul(usize::from(self.spec.channels()))
                .ok_or(AudioError::InvalidData("Vorbis PCM request size overflow"))?;
            if self.ready.len() >= target_samples || self.finished {
                break;
            }
            if self.stream_finished {
                if !self.ready.is_empty() {
                    break;
                }
                self.advance_stream()?;
            } else {
                let channels = usize::from(self.spec.channels());
                // Overlap-add emits at most half the larger Vorbis block.
                let maximum_block = self
                    .stream
                    .as_ref()
                    .ok_or(AudioError::InvalidData(
                        "Vorbis decoder has no active logical stream",
                    ))?
                    .identification()
                    .block_sizes()[1];
                let maximum_packet_samples = (maximum_block / 2)
                    .checked_mul(channels)
                    .ok_or(AudioError::InvalidData("Vorbis PCM request size overflow"))?;
                let reserve_samples = target_samples
                    .saturating_sub(self.ready.len())
                    .checked_add(maximum_packet_samples)
                    .ok_or(AudioError::InvalidData("Vorbis PCM request size overflow"))?;
                self.ready
                    .try_reserve(reserve_samples)
                    .map_err(|_| AudioError::InvalidData("Vorbis PCM allocation failed"))?;
                self.decode_next_packet()?;
            }
        }
        if self.ready.is_empty() {
            return Ok(None);
        }
        let channels = usize::from(self.spec.channels());
        let target_samples = frame_limit * channels;
        let sample_count = self.ready.len().min(target_samples);
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(sample_count)
            .map_err(|_| AudioError::InvalidData("Vorbis PCM allocation failed"))?;
        samples.extend(self.ready.drain(..sample_count));
        PcmBuffer::new(self.spec, Samples::F32(samples))
            .map(Some)
            .map_err(|_| AudioError::InvalidData("Vorbis decoder produced partial PCM frame"))
    }
}

fn audio_spec(stream: &VorbisStreamReader<impl Read>) -> Result<AudioSpec, AudioError> {
    AudioSpec::new(
        stream.identification().sample_rate(),
        u16::from(stream.identification().channels()),
        SampleFormat::Float32,
    )
    .map_err(|_| AudioError::InvalidData("invalid Vorbis PCM specification"))
}

fn overlap_add(
    previous: &TimeDomainBlock,
    current: &TimeDomainBlock,
) -> Result<Vec<f64>, AudioError> {
    let channel_count = previous.channels.len();
    if channel_count == 0 || channel_count != current.channels.len() {
        return Err(AudioError::InvalidData(
            "Vorbis overlap channel counts differ",
        ));
    }
    let previous_size = previous.channels[0].len();
    let current_size = current.channels[0].len();
    if previous
        .channels
        .iter()
        .any(|channel| channel.len() != previous_size)
        || current
            .channels
            .iter()
            .any(|channel| channel.len() != current_size)
        || previous_size % 4 != 0
        || current_size % 4 != 0
    {
        return Err(AudioError::InvalidData("invalid Vorbis overlap block size"));
    }
    let output_frames = previous_size / 4 + current_size / 4;
    let sample_count = output_frames
        .checked_mul(channel_count)
        .ok_or(AudioError::InvalidData("Vorbis overlap size overflow"))?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(sample_count)
        .map_err(|_| AudioError::InvalidData("Vorbis overlap allocation failed"))?;
    let current_start = (previous_size as isize - current_size as isize) / 4;
    for frame in 0..output_frames {
        let previous_index = previous_size / 2 + frame;
        let current_index: isize = frame as isize - current_start;
        for channel in 0..channel_count {
            let old = previous.channels[channel]
                .get(previous_index)
                .copied()
                .unwrap_or(0.0);
            let new: f64 = usize::try_from(current_index)
                .ok()
                .and_then(|index| current.channels[channel].get(index).copied())
                .unwrap_or(0.0);
            let value = old + new;
            if !value.is_finite() {
                return Err(AudioError::InvalidData("non-finite Vorbis overlap sample"));
            }
            output.push(value);
        }
    }
    Ok(output)
}

fn interleave_f32(samples: Vec<f64>, channels: usize) -> Result<Vec<f32>, AudioError> {
    if channels == 0 || samples.len() % channels != 0 {
        return Err(AudioError::InvalidData("invalid Vorbis interleaved PCM"));
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(samples.len())
        .map_err(|_| AudioError::InvalidData("Vorbis PCM allocation failed"))?;
    for sample in samples {
        let sample = sample as f32;
        if !sample.is_finite() {
            return Err(AudioError::InvalidData("non-finite Vorbis PCM sample"));
        }
        output.push(sample);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::overlap_add;
    use crate::stream::TimeDomainBlock;

    #[test]
    fn overlap_add_returns_the_interval_between_block_centers() {
        let previous = TimeDomainBlock {
            channels: vec![vec![1.0; 64], vec![10.0; 64]],
        };
        let current = TimeDomainBlock {
            channels: vec![vec![2.0; 64], vec![20.0; 64]],
        };
        let output = overlap_add(&previous, &current).unwrap();
        assert_eq!(output.len(), 64);
        assert_eq!(&output[..4], &[3.0, 30.0, 3.0, 30.0]);
    }

    #[test]
    fn overlap_add_handles_different_block_sizes() {
        let previous = TimeDomainBlock {
            channels: vec![vec![1.0; 64]],
        };
        let current = TimeDomainBlock {
            channels: vec![vec![2.0; 256]],
        };
        let output = overlap_add(&previous, &current).unwrap();
        assert_eq!(output.len(), 80);
        assert_eq!(&output[..32], &[3.0; 32]);
        assert!(output[32..].iter().all(|&sample| sample == 2.0));
    }
}
