use std::{
    collections::VecDeque,
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use ogg::{OggPacket as Packet, OggPacketReader as OggPackets, PAGE_FLAG_BOS, PAGE_FLAG_EOS};
use zemer_core::{AudioDecoder, AudioError, AudioSpec, Metadata, PcmBuffer};

use crate::{FlacDecoder, metadata::MAX_METADATA_BYTES};

const MAX_FLAC_FRAME_SAMPLES: usize = 65_535;
type OggLink<R> = (OggFlacReader<R>, Arc<AtomicU64>, Arc<AtomicU64>);

pub struct OggFlacDecoder<R> {
    source: Arc<Mutex<R>>,
    start: Option<u64>,
    decoder: Option<FlacDecoder<OggFlacReader<SharedReader<R>>>>,
    spec: AudioSpec,
    initial_spec: AudioSpec,
    metadata: Metadata,
    final_granule: Arc<AtomicU64>,
    granule_offset: Arc<AtomicU64>,
    decoded_samples: u64,
    link_output_samples: u64,
    pending: Option<PcmBuffer>,
    ready: Option<PcmBuffer>,
    link_finished: bool,
    finished: bool,
}

impl<R: Read> OggFlacDecoder<R> {
    pub fn new(reader: R) -> Result<Self, AudioError> {
        Self::with_reader(reader, None)
    }

    fn with_reader(reader: R, start: Option<u64>) -> Result<Self, AudioError> {
        let source = Arc::new(Mutex::new(reader));
        let final_granule = Arc::new(AtomicU64::new(u64::MAX));
        let granule_offset = Arc::new(AtomicU64::new(0));
        let decoder = FlacDecoder::new(OggFlacReader::new(
            SharedReader(Arc::clone(&source)),
            Arc::clone(&final_granule),
            Arc::clone(&granule_offset),
        )?)?;
        Ok(Self {
            source,
            start,
            spec: decoder.spec(),
            initial_spec: decoder.spec(),
            metadata: decoder.metadata().clone(),
            decoder: Some(decoder),
            final_granule,
            granule_offset,
            decoded_samples: 0,
            link_output_samples: 0,
            pending: None,
            ready: None,
            link_finished: false,
            finished: false,
        })
    }
}

impl<R: Read + Seek> OggFlacDecoder<R> {
    /// Creates a decoder that can restart and seek by decoding forward.
    pub fn new_seekable(mut reader: R) -> Result<Self, AudioError> {
        let start = reader.stream_position()?;
        Self::with_reader(reader, Some(start))
    }

    /// Positions the next decoded sample at `sample` (zero-based).
    ///
    /// Seeking restarts from the input's initial Ogg offset and decodes forward.
    pub fn seek_to_sample(&mut self, sample: u64) -> Result<(), AudioError> {
        let start = self.start.ok_or(AudioError::Unsupported(
            "Ogg-FLAC seeking requires OggFlacDecoder::new_seekable",
        ))?;
        self.source
            .lock()
            .map_err(|_| AudioError::InvalidData("Ogg-FLAC input lock is poisoned"))?
            .seek(SeekFrom::Start(start))?;
        let final_granule = Arc::new(AtomicU64::new(u64::MAX));
        let granule_offset = Arc::new(AtomicU64::new(0));
        let decoder = FlacDecoder::new(OggFlacReader::new(
            SharedReader(Arc::clone(&self.source)),
            Arc::clone(&final_granule),
            Arc::clone(&granule_offset),
        )?)?;
        if decoder.spec() != self.initial_spec {
            return Err(AudioError::InvalidData(
                "Ogg-FLAC audio specification changed while seeking",
            ));
        }
        self.spec = decoder.spec();
        self.decoder = Some(decoder);
        self.final_granule = final_granule;
        self.granule_offset = granule_offset;
        self.decoded_samples = 0;
        self.link_output_samples = 0;
        self.pending = None;
        self.ready = None;
        self.link_finished = false;
        self.finished = false;

        // shortcut: seek is linear in decoded samples; add page indexing when repeated long-file seeks need bounded latency.
        let mut remaining = sample;
        while remaining != 0 {
            let frames = remaining.min(MAX_FLAC_FRAME_SAMPLES as u64) as usize;
            let Some(block) = self.read_frames(frames)? else {
                return Err(AudioError::InvalidData(
                    "Ogg-FLAC seek target exceeds the decoded sample count",
                ));
            };
            remaining -= block.frames() as u64;
        }
        Ok(())
    }
}

impl<R: Read> AudioDecoder for OggFlacDecoder<R> {
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
        if self.ready.as_ref().is_some_and(|block| block.frames() != 0) {
            return self.emit_ready(max_frames).map(Some);
        }
        if self.finished {
            return Ok(None);
        }
        loop {
            if self.link_finished {
                self.verify_link_granule()?;
                if !self.advance_link()? {
                    self.finished = true;
                    return Ok(None);
                }
            }
            let next = self.next_block(MAX_FLAC_FRAME_SAMPLES)?;
            match (self.pending.take(), next) {
                (Some(previous), Some(next)) => {
                    self.pending = Some(next);
                    self.ready = Some(previous);
                    return self.emit_ready(max_frames).map(Some);
                }
                (None, Some(next)) => self.pending = Some(next),
                (Some(last), None) => {
                    self.link_finished = true;
                    let granule = self.final_granule.load(Ordering::Acquire);
                    let offset = self.granule_offset.load(Ordering::Acquire);
                    let granule = granule.checked_sub(offset).ok_or(AudioError::InvalidData(
                        "Ogg-FLAC granule precedes the stream origin",
                    ))?;
                    self.ready = trim_final_block(last, self.decoded_samples, granule)?;
                    if self.ready.is_some() {
                        return self.emit_ready(max_frames).map(Some);
                    }
                }
                (None, None) => {
                    self.link_finished = true;
                }
            }
        }
    }
}

fn trim_final_block(
    mut block: PcmBuffer,
    decoded_samples: u64,
    final_granule: u64,
) -> Result<Option<PcmBuffer>, AudioError> {
    let final_samples = final_granule.checked_add(1).ok_or(AudioError::InvalidData(
        "Ogg-FLAC final granule overflows sample count",
    ))?;
    if final_samples > decoded_samples {
        return Err(AudioError::InvalidData(
            "Ogg-FLAC final granule exceeds decoded sample count",
        ));
    }
    let trailing = usize::try_from(decoded_samples - final_samples)
        .map_err(|_| AudioError::InvalidData("Ogg-FLAC trim count overflow"))?;
    if trailing > block.frames() {
        return Err(AudioError::InvalidData(
            "Ogg-FLAC final granule trims more than the final frame",
        ));
    }
    block.truncate_frames(block.frames() - trailing)?;
    Ok((block.frames() != 0).then_some(block))
}

impl<R: Read> OggFlacDecoder<R> {
    fn next_block(&mut self, max_frames: usize) -> Result<Option<PcmBuffer>, AudioError> {
        let Some(block) = self
            .decoder
            .as_mut()
            .ok_or(AudioError::InvalidData("missing Ogg-FLAC link decoder"))?
            .read_frames(max_frames)?
        else {
            return Ok(None);
        };
        self.decoded_samples = self
            .decoded_samples
            .checked_add(block.frames() as u64)
            .ok_or(AudioError::InvalidData(
                "Ogg-FLAC decoded sample count overflow",
            ))?;
        Ok(Some(block))
    }

    fn verify_link_granule(&self) -> Result<(), AudioError> {
        let expected = self
            .granule_offset
            .load(Ordering::Acquire)
            .checked_add(self.link_output_samples)
            .and_then(|samples| samples.checked_sub(1))
            .ok_or(AudioError::InvalidData("Ogg-FLAC final granule overflow"))?;
        if self.final_granule.load(Ordering::Acquire) != expected {
            return Err(AudioError::InvalidData(
                "Ogg-FLAC link granule differs from decoded sample count",
            ));
        }
        Ok(())
    }

    fn advance_link(&mut self) -> Result<bool, AudioError> {
        let reader = self
            .decoder
            .take()
            .ok_or(AudioError::InvalidData("missing Ogg-FLAC link decoder"))?
            .into_inner()?;
        let Some((reader, final_granule, granule_offset)) = reader.next_link()? else {
            return Ok(false);
        };
        let decoder = FlacDecoder::new(reader)?;
        self.spec = decoder.spec();
        self.decoder = Some(decoder);
        self.final_granule = final_granule;
        self.granule_offset = granule_offset;
        self.decoded_samples = 0;
        self.link_output_samples = 0;
        self.pending = None;
        self.link_finished = false;
        Ok(true)
    }

    fn record_output(&mut self, block: PcmBuffer) -> Result<PcmBuffer, AudioError> {
        self.link_output_samples = self
            .link_output_samples
            .checked_add(block.frames() as u64)
            .ok_or(AudioError::InvalidData(
                "Ogg-FLAC link sample count overflow",
            ))?;
        Ok(block)
    }

    fn emit_ready(&mut self, max_frames: usize) -> Result<PcmBuffer, AudioError> {
        let mut ready = self.ready.take().ok_or(AudioError::InvalidData(
            "missing buffered Ogg-FLAC PCM samples",
        ))?;
        let frames = max_frames.min(ready.frames());
        let block = ready.take_prefix_frames(frames)?;
        if ready.frames() != 0 {
            self.ready = Some(ready);
        }
        self.record_output(block)
    }
}

struct SharedReader<R>(Arc<Mutex<R>>);

impl<R: Read> Read for SharedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("Ogg-FLAC input lock is poisoned"))?
            .read(buffer)
    }
}

struct OggFlacReader<R> {
    packets: OggPackets<R>,
    output: Vec<u8>,
    output_pos: usize,
    audio: VecDeque<(Packet, bool)>,
    final_granule: Arc<AtomicU64>,
}

impl<R: Read> OggFlacReader<R> {
    fn new(
        reader: R,
        final_granule: Arc<AtomicU64>,
        granule_offset: Arc<AtomicU64>,
    ) -> Result<Self, AudioError> {
        Self::from_packets(OggPackets::new(reader), final_granule, granule_offset)
    }

    fn from_packets(
        mut packets: OggPackets<R>,
        final_granule: Arc<AtomicU64>,
        granule_offset: Arc<AtomicU64>,
    ) -> Result<Self, AudioError> {
        let first = packets
            .next_packet()?
            .ok_or(AudioError::InvalidFormat("empty Ogg stream"))?;
        if is_legacy_mapping_header(&first) {
            return Self::from_legacy_packets(packets, first, final_granule, granule_offset);
        }
        if !first.page_is_first
            || !first.ends_page
            || first.page_flags != PAGE_FLAG_BOS
            || first.page_granule_position != 0
            || first.page_segment_count != 1
            || first.bytes.len() != 51
            || &first.bytes[..5] != b"\x7fFLAC"
            || first.bytes[5] != 1
            || &first.bytes[9..13] != b"fLaC"
            || first.bytes[13] & 0x7f != 0
            || first.bytes[14..17] != [0, 0, 34]
        {
            return Err(AudioError::InvalidFormat("invalid Ogg-FLAC mapping header"));
        }
        let declared = u16::from_be_bytes([first.bytes[7], first.bytes[8]]);
        if declared != 0 && first.bytes[13] & 0x80 != 0 {
            return Err(AudioError::InvalidFormat(
                "Ogg-FLAC STREAMINFO marked last before declared metadata",
            ));
        }

        let mut output = Vec::from(&first.bytes[9..]);
        let first_audio = if declared > 0 {
            for index in 0..declared {
                let packet = packets
                    .next_packet()?
                    .ok_or(AudioError::InvalidFormat("truncated Ogg-FLAC headers"))?;
                validate_header_packet(&packet)?;
                if index == 0 && packet.bytes[0] & 0x7f != 4 {
                    return Err(AudioError::InvalidFormat(
                        "first Ogg-FLAC metadata packet must be a Vorbis comment",
                    ));
                }
                let last = index + 1 == declared;
                if last && !packet.ends_page {
                    return Err(AudioError::InvalidFormat(
                        "last Ogg-FLAC header packet must end its page",
                    ));
                }
                append_metadata_packet(&mut output, &packet.bytes, last)?;
            }
            let packet = packets
                .next_packet_with_granule()?
                .ok_or(AudioError::InvalidFormat(
                    "Ogg-FLAC stream has no audio packet",
                ))?;
            if !packet.0.starts_page {
                return Err(AudioError::InvalidFormat(
                    "first Ogg-FLAC audio packet must start an audio page",
                ));
            }
            validate_audio_packet(&packet.0)?;
            packet
        } else {
            let mut metadata_count = 0_usize;
            let first_audio = loop {
                let packet = packets
                    .next_packet_with_granule()?
                    .ok_or(AudioError::InvalidFormat("truncated Ogg-FLAC headers"))?;
                if is_metadata_packet(&packet.0.bytes) {
                    validate_header_packet(&packet.0)?;
                    if metadata_count == 0 && packet.0.bytes[0] & 0x7f != 4 {
                        return Err(AudioError::InvalidFormat(
                            "first Ogg-FLAC metadata packet must be a Vorbis comment",
                        ));
                    }
                    append_metadata_packet(&mut output, &packet.0.bytes, false)?;
                    metadata_count += 1;
                } else {
                    if metadata_count == 0 {
                        return Err(AudioError::InvalidFormat(
                            "Ogg-FLAC stream is missing its Vorbis comment packet",
                        ));
                    }
                    if !packet.0.starts_page {
                        return Err(AudioError::InvalidFormat(
                            "first Ogg-FLAC audio packet must start an audio page",
                        ));
                    }
                    validate_audio_packet(&packet.0)?;
                    break packet;
                }
                if metadata_count > MAX_METADATA_BYTES / 4 {
                    return Err(AudioError::InvalidData(
                        "too many Ogg-FLAC metadata packets",
                    ));
                }
            };
            output[4] &= 0x7f;
            let mut cursor = 4;
            while cursor < output.len() {
                let length = (usize::from(output[cursor + 1]) << 16)
                    | (usize::from(output[cursor + 2]) << 8)
                    | usize::from(output[cursor + 3]);
                let next = cursor + 4 + length;
                if next > output.len() {
                    return Err(AudioError::InvalidFormat(
                        "invalid Ogg-FLAC metadata packet",
                    ));
                }
                cursor = next;
                if cursor == output.len() {
                    output[cursor - length - 4] |= 0x80;
                }
            }
            first_audio
        };

        let audio = collect_initial_audio_page(&mut packets, first_audio, &granule_offset)?;
        if output.len() > MAX_METADATA_BYTES + 4 {
            return Err(AudioError::InvalidData(
                "FLAC metadata exceeds the 128 MiB limit",
            ));
        }
        Ok(Self {
            packets,
            output,
            output_pos: 0,
            audio,
            final_granule,
        })
    }

    fn from_legacy_packets(
        mut packets: OggPackets<R>,
        first: Packet,
        final_granule: Arc<AtomicU64>,
        granule_offset: Arc<AtomicU64>,
    ) -> Result<Self, AudioError> {
        if !first.page_is_first
            || !first.ends_page
            || first.page_flags != PAGE_FLAG_BOS
            || first.page_granule_position != 0
        {
            return Err(AudioError::InvalidFormat(
                "invalid legacy Ogg-FLAC identification packet",
            ));
        }

        let mut output = Vec::from(&b"fLaC"[..]);
        let mut metadata_count = 0_usize;
        let mut metadata_finished = false;
        let first_audio = loop {
            let packet = packets
                .next_packet_with_granule()?
                .ok_or(AudioError::InvalidFormat(
                    "legacy Ogg-FLAC stream has no audio packet",
                ))?;
            if is_metadata_packet(&packet.0.bytes) {
                // Legacy mappings may share a page's granule with completed audio packets.
                let block_type = packet.0.bytes[0] & 0x7f;
                if metadata_count == 0 && (block_type != 0 || packet.0.bytes.len() != 38) {
                    return Err(AudioError::InvalidFormat(
                        "legacy Ogg-FLAC must begin with STREAMINFO",
                    ));
                }
                if metadata_finished {
                    return Err(AudioError::InvalidFormat(
                        "legacy Ogg-FLAC metadata follows the last metadata block",
                    ));
                }
                metadata_finished = packet.0.bytes[0] & 0x80 != 0;
                append_metadata_packet(&mut output, &packet.0.bytes, metadata_finished)?;
                metadata_count += 1;
                if metadata_count > MAX_METADATA_BYTES / 4 {
                    return Err(AudioError::InvalidData(
                        "too many legacy Ogg-FLAC metadata packets",
                    ));
                }
                continue;
            }
            if metadata_count == 0 || !metadata_finished {
                return Err(AudioError::InvalidFormat(
                    "legacy Ogg-FLAC metadata is missing its last-block marker",
                ));
            }
            validate_audio_packet(&packet.0)?;
            break packet;
        };

        let audio = collect_initial_audio_page(&mut packets, first_audio, &granule_offset)?;
        if output.len() > MAX_METADATA_BYTES + 4 {
            return Err(AudioError::InvalidData(
                "FLAC metadata exceeds the 128 MiB limit",
            ));
        }
        Ok(Self {
            packets,
            output,
            output_pos: 0,
            audio,
            final_granule,
        })
    }

    fn next_link(mut self) -> Result<Option<OggLink<R>>, AudioError> {
        if self.output_pos != self.output.len()
            || !self.audio.is_empty()
            || self.packets.next_packet()?.is_some()
        {
            return Err(AudioError::InvalidData(
                "Ogg-FLAC decoder stopped before the link boundary",
            ));
        }
        let Some(packets) = self.packets.next_stream()? else {
            return Ok(None);
        };
        let final_granule = Arc::new(AtomicU64::new(u64::MAX));
        let granule_offset = Arc::new(AtomicU64::new(0));
        let reader = Self::from_packets(
            packets,
            Arc::clone(&final_granule),
            Arc::clone(&granule_offset),
        )?;
        Ok(Some((reader, final_granule, granule_offset)))
    }
}

fn is_legacy_mapping_header(packet: &Packet) -> bool {
    packet.bytes == b"fLaC"
}

fn collect_initial_audio_page<R: Read>(
    packets: &mut OggPackets<R>,
    first: (Packet, bool),
    granule_offset: &AtomicU64,
) -> Result<VecDeque<(Packet, bool)>, AudioError> {
    let page_sequence = first.0.page_sequence;
    let mut samples = 0_u64;
    let mut audio = VecDeque::new();
    let mut packet = first;
    loop {
        if packet.0.page_sequence != page_sequence {
            return Err(AudioError::InvalidFormat(
                "first Ogg-FLAC audio page has no completed-packet granule",
            ));
        }
        samples = samples
            .checked_add(frame_block_size(&packet.0.bytes)? as u64)
            .ok_or(AudioError::InvalidData(
                "Ogg-FLAC initial page sample count overflow",
            ))?;
        let owns_granule = packet.1;
        let granule = packet.0.page_granule_position;
        let terminal_page = packet.0.page_flags & PAGE_FLAG_EOS != 0;
        audio.push_back(packet);
        if owns_granule {
            if granule == u64::MAX
                || (granule.checked_add(1).is_none_or(|end| end < samples) && !terminal_page)
            {
                return Err(AudioError::InvalidFormat(
                    "invalid Ogg-FLAC initial audio granule",
                ));
            }
            granule_offset.store(
                granule.saturating_add(1).saturating_sub(samples),
                Ordering::Release,
            );
            return Ok(audio);
        }
        packet = packets
            .next_packet_with_granule()?
            .ok_or(AudioError::InvalidFormat(
                "Ogg-FLAC stream has no initial audio granule",
            ))?;
        validate_audio_packet(&packet.0)?;
    }
}

pub(crate) fn frame_block_size(frame: &[u8]) -> Result<usize, AudioError> {
    if frame.len() < 5 || frame[0] != 0xff || frame[1] & 0xfe != 0xf8 {
        return Err(AudioError::InvalidFormat("invalid Ogg-FLAC audio frame"));
    }
    let block_code = frame[2] >> 4;
    let first_number_byte = frame[4];
    let number_bytes = match first_number_byte.leading_ones() {
        0 => 1,
        count @ 2..=7 => count as usize,
        _ => return Err(AudioError::InvalidFormat("invalid FLAC coded frame number")),
    };
    let number_end = 4 + number_bytes;
    if number_end > frame.len() || frame[5..number_end].iter().any(|byte| byte & 0xc0 != 0x80) {
        return Err(AudioError::InvalidFormat("invalid FLAC coded frame number"));
    }
    let extra = match block_code {
        6 => 1,
        7 => 2,
        _ => 0,
    };
    if number_end + extra > frame.len() {
        return Err(AudioError::InvalidFormat("truncated FLAC block size"));
    }
    Ok(match block_code {
        1 => 192,
        2..=5 => 576_usize << (block_code - 2),
        6 => usize::from(frame[number_end]) + 1,
        7 => {
            usize::from(u16::from_be_bytes([
                frame[number_end],
                frame[number_end + 1],
            ])) + 1
        }
        8..=15 => 256_usize << (block_code - 8),
        _ => return Err(AudioError::InvalidFormat("reserved FLAC block-size code")),
    })
}

impl<R: Read> Read for OggFlacReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.output_pos == self.output.len() {
            self.output.clear();
            self.output_pos = 0;
            let (packet, owns_granule) = if let Some(packet) = self.audio.pop_front() {
                packet
            } else {
                let Some(packet) = self.packets.next_packet_with_granule()? else {
                    return Ok(0);
                };
                packet
            };
            let terminal_granule = packet.page_flags & PAGE_FLAG_EOS != 0 && owns_granule;
            if terminal_granule {
                if packet.page_granule_position == u64::MAX
                    || packet.page_granule_position > i64::MAX as u64
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid Ogg-FLAC final granule position",
                    ));
                }
                self.final_granule
                    .store(packet.page_granule_position, Ordering::Release);
            }
            if packet.bytes.is_empty() {
                if terminal_granule {
                    return Ok(0);
                }
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "empty non-terminal Ogg-FLAC audio packet",
                ));
            }
            validate_audio_packet(&packet)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            self.output = packet.bytes;
        }
        let count = buffer.len().min(self.output.len() - self.output_pos);
        buffer[..count].copy_from_slice(&self.output[self.output_pos..self.output_pos + count]);
        self.output_pos += count;
        Ok(count)
    }
}

fn validate_header_packet(packet: &Packet) -> Result<(), AudioError> {
    if packet.page_granule_position != 0 || !is_metadata_packet(&packet.bytes) {
        return Err(AudioError::InvalidFormat(
            "invalid Ogg-FLAC metadata packet",
        ));
    }
    Ok(())
}

fn validate_audio_packet(packet: &Packet) -> Result<(), AudioError> {
    if packet.bytes.first() != Some(&0xff) || is_metadata_packet(&packet.bytes) {
        return Err(AudioError::InvalidFormat("invalid Ogg-FLAC audio packet"));
    }
    Ok(())
}

fn is_metadata_packet(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || bytes[0] & 0x7f == 127 {
        return false;
    }
    let length =
        (usize::from(bytes[1]) << 16) | (usize::from(bytes[2]) << 8) | usize::from(bytes[3]);
    length == bytes.len() - 4
}

fn append_metadata_packet(
    output: &mut Vec<u8>,
    bytes: &[u8],
    last: bool,
) -> Result<(), AudioError> {
    if !is_metadata_packet(bytes) {
        return Err(AudioError::InvalidFormat(
            "invalid Ogg-FLAC metadata packet",
        ));
    }
    let new_len = output
        .len()
        .checked_add(bytes.len())
        .filter(|len| *len <= MAX_METADATA_BYTES + 4)
        .ok_or(AudioError::InvalidData(
            "FLAC metadata exceeds the 128 MiB limit",
        ))?;
    let start = output.len();
    output.reserve(new_len - output.len());
    output.extend_from_slice(bytes);
    output[start] = (output[start] & 0x7f) | if last { 0x80 } else { 0 };
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ogg::{OggPacketReader, OggPacketWriter, PAGE_FLAG_EOS};
    use zemer_core::{AudioDecoder, AudioEncoder, AudioSpec, PcmBuffer, SampleFormat, Samples};

    use super::{OggFlacDecoder, trim_final_block};
    use crate::{OggFlacEncoder, rewrite_ogg_metadata};

    fn ogg_crc(bytes: &[u8]) -> u32 {
        let mut crc = 0_u32;
        for &byte in bytes {
            crc ^= u32::from(byte) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 {
                    (crc << 1) ^ 0x04c1_1db7
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    const NATIVE: [u8; 57] = [
        0x66, 0x4c, 0x61, 0x43, 0x80, 0x00, 0x00, 0x22, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00, 0x0f,
        0x00, 0x00, 0x0f, 0x0a, 0xc4, 0x42, 0xf0, 0x00, 0x00, 0x00, 0x01, 0x3e, 0x84, 0xb4, 0x18,
        0x07, 0xdc, 0x69, 0x03, 0x07, 0x58, 0x6a, 0x3d, 0xad, 0x1a, 0x2e, 0x0f, 0xff, 0xf8, 0x69,
        0x18, 0x00, 0x00, 0xbf, 0x03, 0x58, 0xfd, 0x03, 0x12, 0x8b, 0xaa, 0x9a,
    ];

    fn page(packet: &[u8], flags: u8, sequence: u32, granule: u64) -> Vec<u8> {
        page_with_packets(&[packet], flags, sequence, granule)
    }

    fn page_with_packets(packets: &[&[u8]], flags: u8, sequence: u32, granule: u64) -> Vec<u8> {
        let mut lacing = Vec::new();
        for packet in packets {
            let mut remaining = packet.len();
            while remaining >= 255 {
                lacing.push(255);
                remaining -= 255;
            }
            lacing.push(remaining as u8);
        }
        let mut output = vec![0; 27];
        output[..4].copy_from_slice(b"OggS");
        output[5] = flags;
        output[6..14].copy_from_slice(&granule.to_le_bytes());
        output[14..18].copy_from_slice(&7_u32.to_le_bytes());
        output[18..22].copy_from_slice(&sequence.to_le_bytes());
        output[26] = lacing.len() as u8;
        output.extend_from_slice(&lacing);
        for packet in packets {
            output.extend_from_slice(packet);
        }
        let crc = ogg_crc(&output);
        output[22..26].copy_from_slice(&crc.to_le_bytes());
        output
    }

    fn update_page_crc(page: &mut [u8]) {
        page[22..26].fill(0);
        let crc = ogg_crc(page);
        page[22..26].copy_from_slice(&crc.to_le_bytes());
    }

    fn set_stream_serial(stream: &mut [u8], serial: u32) {
        let mut cursor = 0;
        let mut sequence = 0_u32;
        while cursor < stream.len() {
            let segment_count = usize::from(stream[cursor + 26]);
            let body_len: usize = stream[cursor + 27..cursor + 27 + segment_count]
                .iter()
                .map(|&length| usize::from(length))
                .sum();
            let page_len = 27 + segment_count + body_len;
            let page = &mut stream[cursor..cursor + page_len];
            page[14..18].copy_from_slice(&serial.to_le_bytes());
            page[18..22].copy_from_slice(&sequence.to_le_bytes());
            update_page_crc(page);
            cursor += page_len;
            sequence += 1;
        }
    }

    fn two_frame_ogg_flac() -> (Vec<u8>, Vec<i16>) {
        let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
        let samples: Vec<i16> = (0..4097)
            .map(|index| (index * 29 % 65_536 - 32_768) as i16)
            .collect();
        let block = PcmBuffer::new(spec, Samples::I16(samples.clone())).unwrap();
        let mut encoder = OggFlacEncoder::new(Vec::new(), spec).unwrap();
        encoder.write_block(&block).unwrap();
        encoder.finish().unwrap();
        (encoder.into_inner(), samples)
    }

    fn adjust_audio_granules(input: &[u8], offset: u64, bad_terminal: bool) -> Vec<u8> {
        let mut reader = OggPacketReader::new(Cursor::new(input));
        let mut writer = OggPacketWriter::new(Vec::new(), 91);
        while let Some((packet, owns_granule)) = reader.next_packet_with_granule().unwrap() {
            let audio = packet.bytes.first() == Some(&0xff);
            let mut granule = packet.page_granule_position;
            if audio && granule != u64::MAX {
                granule += offset;
                if bad_terminal && owns_granule && packet.page_flags & PAGE_FLAG_EOS != 0 {
                    granule += 1;
                }
            }
            writer.write_packet(&packet.bytes, granule).unwrap();
        }
        writer.finish().unwrap();
        writer.into_inner()
    }

    fn clear_streaminfo_total_samples(stream: &mut [u8]) {
        let segment_count = usize::from(stream[26]);
        let body_start = 27 + segment_count;
        let page_len = body_start
            + stream[27..body_start]
                .iter()
                .map(|&length| usize::from(length))
                .sum::<usize>();
        let sample_field = body_start + 27;
        let mut packed =
            u64::from_be_bytes(stream[sample_field..sample_field + 8].try_into().unwrap());
        packed &= !0x0f_ffff_ffff;
        stream[sample_field..sample_field + 8].copy_from_slice(&packed.to_be_bytes());
        update_page_crc(&mut stream[..page_len]);
    }

    fn ogg_flac() -> Vec<u8> {
        ogg_flac_with_comment(1, false)
    }

    fn ogg_flac_with_comment(declared_count: u16, streaminfo_last: bool) -> Vec<u8> {
        let mut header = Vec::from(&b"\x7fFLAC\x01\x00"[..]);
        header.extend_from_slice(&declared_count.to_be_bytes());
        header.extend_from_slice(b"fLaC");
        let mut streaminfo_header = NATIVE[4..8].to_vec();
        streaminfo_header[0] =
            (streaminfo_header[0] & 0x7f) | if streaminfo_last { 0x80 } else { 0 };
        header.extend_from_slice(&streaminfo_header);
        header.extend_from_slice(&NATIVE[8..42]);
        let mut stream = page(&header, 2, 0, 0);
        let mut comment = vec![4, 0, 0, 8];
        comment.extend_from_slice(&[0; 8]);
        stream.extend(page(&comment, 0, 1, 0));
        stream.extend(page(&NATIVE[42..], 4, 2, 0));
        stream
    }

    fn ogg_flac_missing_comment() -> Vec<u8> {
        let mut header = Vec::from(&b"\x7fFLAC\x01\x00\x00\x00fLaC"[..]);
        header.extend_from_slice(&NATIVE[4..42]);
        let mut stream = page(&header, 2, 0, 0);
        stream.extend(page(&NATIVE[42..], 4, 1, 0));
        stream
    }

    #[test]
    fn decodes_rfc_frame_from_ogg_flac_mapping() {
        let mut decoder = OggFlacDecoder::new(Cursor::new(ogg_flac())).unwrap();
        assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);
        assert_eq!(
            decoder.read_frames(1).unwrap().unwrap().samples(),
            &Samples::I16(vec![25_588, 10_416])
        );
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn accepts_empty_terminal_ogg_packet_as_eos_granule_carrier() {
        let mut input = ogg_flac();
        let last_page = input
            .windows(4)
            .rposition(|bytes| bytes == b"OggS")
            .unwrap();
        input[last_page + 5] &= !PAGE_FLAG_EOS;
        update_page_crc(&mut input[last_page..]);
        input.extend(page(&[], PAGE_FLAG_EOS, 3, 1));

        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
        assert_eq!(
            decoder.read_frames(1).unwrap().unwrap().samples(),
            &Samples::I16(vec![25_588, 10_416])
        );
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn rejects_empty_nonterminal_ogg_audio_packets() {
        let mut input = ogg_flac();
        let last_page = input
            .windows(4)
            .rposition(|bytes| bytes == b"OggS")
            .unwrap();
        input[last_page + 5] &= !PAGE_FLAG_EOS;
        update_page_crc(&mut input[last_page..]);
        input.extend(page(&[], 0, 3, u64::MAX));

        let mut decoder = OggFlacDecoder::new(Cursor::new(input.clone())).unwrap();
        assert!(decoder.read_frames(1).is_err());
        assert!(
            rewrite_ogg_metadata(Cursor::new(input), &mut Vec::new(), &Default::default()).is_err()
        );
    }

    #[test]
    fn decodes_legacy_ogg_flac_packet_layout() {
        let mut input = page(b"fLaC", 2, 0, 0);
        let mut streaminfo = NATIVE[4..42].to_vec();
        streaminfo[0] &= 0x7f;
        input.extend(page(&streaminfo, 0, 1, 0));
        let mut comment = vec![0x84, 0, 0, 8];
        comment.extend_from_slice(&[0; 8]);
        input.extend(page_with_packets(
            &[&comment, &NATIVE[42..]],
            PAGE_FLAG_EOS,
            2,
            1,
        ));

        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
        assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);
        assert_eq!(
            decoder.read_frames(1).unwrap().unwrap().samples(),
            &Samples::I16(vec![25_588, 10_416])
        );
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn decodes_chained_links_and_keeps_the_first_link_metadata() {
        let mut input = ogg_flac();
        let mut second = ogg_flac();
        set_stream_serial(&mut second, 8);
        input.extend(second);

        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
        assert_eq!(decoder.read_frames(8).unwrap().unwrap().frames(), 1);
        assert_eq!(decoder.read_frames(8).unwrap().unwrap().frames(), 1);
        assert!(decoder.read_frames(8).unwrap().is_none());
    }

    #[test]
    fn seeks_through_chained_links_and_restarts_from_the_initial_offset() {
        let mut input = vec![0, 1, 2];
        input.extend(ogg_flac());
        let mut second = ogg_flac();
        set_stream_serial(&mut second, 8);
        input.extend(second);

        let mut reader = Cursor::new(input);
        reader.set_position(3);
        let mut decoder = OggFlacDecoder::new_seekable(reader).unwrap();
        decoder.seek_to_sample(1).unwrap();
        assert_eq!(
            decoder.read_frames(1).unwrap().unwrap().samples(),
            &Samples::I16(vec![25_588, 10_416])
        );
        decoder.seek_to_sample(0).unwrap();
        assert_eq!(decoder.read_frames(1).unwrap().unwrap().frames(), 1);
        assert!(decoder.seek_to_sample(3).is_err());
    }

    #[test]
    fn seek_requires_the_seekable_constructor() {
        let mut decoder = OggFlacDecoder::new(Cursor::new(ogg_flac())).unwrap();
        assert!(decoder.seek_to_sample(0).is_err());
    }

    #[test]
    fn decodes_chained_links_with_different_audio_specifications() {
        let mut input = ogg_flac();
        let second_spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
        let second_block = PcmBuffer::new(second_spec, Samples::I16(vec![123])).unwrap();
        let mut second_encoder = OggFlacEncoder::new(Vec::new(), second_spec).unwrap();
        second_encoder.write_block(&second_block).unwrap();
        second_encoder.finish().unwrap();
        let mut second = second_encoder.into_inner();
        set_stream_serial(&mut second, 8);
        input.extend(second);

        let mut decoder = OggFlacDecoder::new(Cursor::new(input.clone())).unwrap();
        let first = decoder.read_frames(8).unwrap().unwrap();
        assert_eq!(first.spec().sample_rate(), 44_100);
        assert_eq!(decoder.spec().sample_rate(), 44_100);
        let second = decoder.read_frames(8).unwrap().unwrap();
        assert_eq!(second.spec().sample_rate(), 48_000);
        assert_eq!(decoder.spec().sample_rate(), 48_000);
        assert!(decoder.read_frames(8).unwrap().is_none());

        let mut decoder = OggFlacDecoder::new_seekable(Cursor::new(input)).unwrap();
        decoder.seek_to_sample(1).unwrap();
        let block = decoder.read_frames(8).unwrap().unwrap();
        assert_eq!(block.spec().sample_rate(), 48_000);
        assert_eq!(block.samples(), &Samples::I16(vec![123]));
        assert_eq!(decoder.spec().sample_rate(), 48_000);
        assert!(decoder.read_frames(8).unwrap().is_none());
        decoder.seek_to_sample(0).unwrap();
        assert_eq!(decoder.spec().sample_rate(), 44_100);
    }

    #[test]
    fn rejects_corrupt_ogg_page_crc() {
        let mut input = ogg_flac();
        *input.last_mut().unwrap() ^= 1;
        assert!(OggFlacDecoder::new(Cursor::new(input)).is_err());
    }

    #[test]
    fn accepts_forward_compatible_mapping_minor_versions() {
        let mut input = ogg_flac();
        input[34] = 1;
        update_page_crc(&mut input[..79]);
        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
        assert_eq!(decoder.read_frames(1).unwrap().unwrap().frames(), 1);
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn rejects_missing_ogg_eos_page() {
        let mut input = ogg_flac();
        let page_start = input.len() - (27 + 1 + NATIVE.len() - 42);
        input[page_start + 5] = 0;
        update_page_crc(&mut input[page_start..]);
        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
        assert!(decoder.read_frames(1).is_err());
    }

    #[test]
    fn decodes_nonzero_initial_granule_offset() {
        let (input, expected) = two_frame_ogg_flac();
        let input = adjust_audio_granules(&input, 100, false);
        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
        let mut actual = Vec::new();
        while let Some(block) = decoder.read_frames(257).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("offset Ogg-FLAC must decode to signed 16-bit PCM");
            };
            actual.extend_from_slice(samples);
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn rejects_terminal_granule_that_disagrees_with_initial_offset() {
        let (input, _) = two_frame_ogg_flac();
        let input = adjust_audio_granules(&input, 100, true);
        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
        while decoder.read_frames(257).is_ok_and(|block| block.is_some()) {}
        assert!(decoder.read_frames(1).is_err());
    }

    #[test]
    fn verifies_terminal_granule_when_streaminfo_total_is_unknown() {
        let mut input = ogg_flac();
        clear_streaminfo_total_samples(&mut input);
        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
        assert_eq!(decoder.read_frames(1).unwrap().unwrap().frames(), 1);
        assert!(decoder.read_frames(1).unwrap().is_none());

        let (mut input, _) = two_frame_ogg_flac();
        input = adjust_audio_granules(&input, 100, true);
        clear_streaminfo_total_samples(&mut input);
        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
        while decoder.read_frames(257).is_ok_and(|block| block.is_some()) {}
        assert!(decoder.read_frames(1).is_err());
    }

    #[test]
    fn decodes_declared_metadata_packet() {
        let mut decoder =
            OggFlacDecoder::new(Cursor::new(ogg_flac_with_comment(1, false))).unwrap();
        assert_eq!(decoder.read_frames(1).unwrap().unwrap().frames(), 1);
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn discovers_metadata_when_packet_count_is_unknown() {
        let mut decoder =
            OggFlacDecoder::new(Cursor::new(ogg_flac_with_comment(0, false))).unwrap();
        assert_eq!(decoder.read_frames(1).unwrap().unwrap().frames(), 1);
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn rejects_missing_required_vorbis_comment_packet() {
        assert!(OggFlacDecoder::new(Cursor::new(ogg_flac_missing_comment())).is_err());
    }

    #[test]
    fn trims_only_trailing_interleaved_samples_from_the_final_frame() {
        let spec = AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap();
        let block =
            zemer_core::PcmBuffer::new(spec, Samples::I16(vec![1, 10, 2, 20, 3, 30, 4, 40]))
                .unwrap();
        let block = trim_final_block(block, 4, 2).unwrap().unwrap();
        assert_eq!(block.frames(), 3);
        assert_eq!(block.samples(), &Samples::I16(vec![1, 10, 2, 20, 3, 30]));
        assert!(trim_final_block(block, 3, 3).is_err());
    }

    #[test]
    fn decodes_granule_trim_fixture_against_independent_pcm_oracle() {
        let mut decoder = OggFlacDecoder::new(Cursor::new(include_bytes!(
            "../../../tests/fixtures/ogg-flac-granule-trim.oga"
        )))
        .unwrap();
        let mut actual = Vec::new();
        while let Some(block) = decoder.read_frames(73).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("fixture must decode to signed 16-bit PCM");
            };
            for sample in samples {
                actual.extend_from_slice(&sample.to_le_bytes());
            }
        }
        let oracle = include_bytes!("../../../tests/fixtures/ogg-flac-granule-oracle.pcm");
        assert_eq!(actual, oracle[..509 * 2]);
    }
}
