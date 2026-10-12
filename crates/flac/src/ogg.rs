use std::{
    io::{self, Read},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use ogg::{OggPacket as Packet, OggPacketReader as OggPackets, PAGE_FLAG_BOS, PAGE_FLAG_EOS};
use zemer_core::{AudioDecoder, AudioError, AudioSpec, Metadata, PcmBuffer};

use crate::{FlacDecoder, metadata::MAX_METADATA_BYTES};

const MAX_FLAC_FRAME_SAMPLES: usize = 65_535;

pub struct OggFlacDecoder<R> {
    decoder: FlacDecoder<OggFlacReader<R>>,
    final_granule: Arc<AtomicU64>,
    decoded_samples: u64,
    output_samples: u64,
    pending: Option<PcmBuffer>,
    ready: Option<PcmBuffer>,
    finished: bool,
}

impl<R: Read> OggFlacDecoder<R> {
    pub fn new(reader: R) -> Result<Self, AudioError> {
        let final_granule = Arc::new(AtomicU64::new(u64::MAX));
        Ok(Self {
            decoder: FlacDecoder::new(OggFlacReader::new(reader, Arc::clone(&final_granule))?)?,
            final_granule,
            decoded_samples: 0,
            output_samples: 0,
            pending: None,
            ready: None,
            finished: false,
        })
    }
}

impl<R: Read> AudioDecoder for OggFlacDecoder<R> {
    fn spec(&self) -> AudioSpec {
        self.decoder.spec()
    }

    fn metadata(&self) -> &Metadata {
        self.decoder.metadata()
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
            self.verify_final_granule()?;
            return Ok(None);
        }
        loop {
            let next = self.next_block(MAX_FLAC_FRAME_SAMPLES)?;
            match (self.pending.take(), next) {
                (Some(previous), Some(next)) => {
                    self.pending = Some(next);
                    self.ready = Some(previous);
                    return self.emit_ready(max_frames).map(Some);
                }
                (None, Some(next)) => self.pending = Some(next),
                (Some(last), None) => {
                    self.finished = true;
                    self.ready = trim_final_block(
                        last,
                        self.decoded_samples,
                        self.final_granule.load(Ordering::Acquire),
                    )?;
                    if self.ready.is_some() {
                        return self.emit_ready(max_frames).map(Some);
                    }
                    self.verify_final_granule()?;
                    return Ok(None);
                }
                (None, None) => {
                    self.finished = true;
                    self.verify_final_granule()?;
                    return Ok(None);
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
    if final_granule > decoded_samples {
        return Err(AudioError::InvalidData(
            "Ogg-FLAC final granule exceeds decoded sample count",
        ));
    }
    let trailing = usize::try_from(decoded_samples - final_granule)
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
        let Some(block) = self.decoder.read_frames(max_frames)? else {
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

    fn verify_final_granule(&self) -> Result<(), AudioError> {
        if self.final_granule.load(Ordering::Acquire) != self.output_samples {
            return Err(AudioError::InvalidData(
                "Ogg-FLAC final granule differs from decoded sample count",
            ));
        }
        Ok(())
    }

    fn record_output(&mut self, block: PcmBuffer) -> Result<PcmBuffer, AudioError> {
        self.output_samples = self
            .output_samples
            .checked_add(block.frames() as u64)
            .ok_or(AudioError::InvalidData(
                "Ogg-FLAC output sample count overflow",
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

struct OggFlacReader<R> {
    packets: OggPackets<R>,
    output: Vec<u8>,
    output_pos: usize,
    audio: Option<Packet>,
    final_granule: Arc<AtomicU64>,
}

impl<R: Read> OggFlacReader<R> {
    fn new(reader: R, final_granule: Arc<AtomicU64>) -> Result<Self, AudioError> {
        let mut packets = OggPackets::new(reader);
        let first = packets
            .next_packet()?
            .ok_or(AudioError::InvalidFormat("empty Ogg stream"))?;
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
        let audio = if declared > 0 {
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
            let packet = packets.next_packet()?.ok_or(AudioError::InvalidFormat(
                "Ogg-FLAC stream has no audio packet",
            ))?;
            if !packet.starts_page || packet.page_granule_position == 0 {
                return Err(AudioError::InvalidFormat(
                    "first Ogg-FLAC audio packet must start an audio page",
                ));
            }
            validate_audio_packet(&packet)?;
            Some(packet)
        } else {
            let mut metadata_count = 0_usize;
            let first_audio = loop {
                let packet = packets
                    .next_packet()?
                    .ok_or(AudioError::InvalidFormat("truncated Ogg-FLAC headers"))?;
                if is_metadata_packet(&packet.bytes) {
                    validate_header_packet(&packet)?;
                    if metadata_count == 0 && packet.bytes[0] & 0x7f != 4 {
                        return Err(AudioError::InvalidFormat(
                            "first Ogg-FLAC metadata packet must be a Vorbis comment",
                        ));
                    }
                    append_metadata_packet(&mut output, &packet.bytes, false)?;
                    metadata_count += 1;
                } else {
                    if metadata_count == 0 {
                        return Err(AudioError::InvalidFormat(
                            "Ogg-FLAC stream is missing its Vorbis comment packet",
                        ));
                    }
                    if !packet.starts_page || packet.page_granule_position == 0 {
                        return Err(AudioError::InvalidFormat(
                            "first Ogg-FLAC audio packet must start an audio page",
                        ));
                    }
                    validate_audio_packet(&packet)?;
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
            Some(first_audio)
        };

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
}

impl<R: Read> Read for OggFlacReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.output_pos == self.output.len() {
            self.output.clear();
            self.output_pos = 0;
            let packet = if let Some(packet) = self.audio.take() {
                packet
            } else {
                let Some(packet) = self.packets.next_packet()? else {
                    return Ok(0);
                };
                packet
            };
            validate_audio_packet(&packet)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            if packet.page_flags & PAGE_FLAG_EOS != 0 && packet.ends_page {
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

    use zemer_core::{AudioDecoder, AudioSpec, SampleFormat, Samples};

    use super::{OggFlacDecoder, trim_final_block};

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
        let mut lacing = Vec::new();
        let mut remaining = packet.len();
        while remaining >= 255 {
            lacing.push(255);
            remaining -= 255;
        }
        lacing.push(remaining as u8);
        let mut output = vec![0; 27];
        output[..4].copy_from_slice(b"OggS");
        output[5] = flags;
        output[6..14].copy_from_slice(&granule.to_le_bytes());
        output[14..18].copy_from_slice(&7_u32.to_le_bytes());
        output[18..22].copy_from_slice(&sequence.to_le_bytes());
        output[26] = lacing.len() as u8;
        output.extend_from_slice(&lacing);
        output.extend_from_slice(packet);
        let crc = ogg_crc(&output);
        output[22..26].copy_from_slice(&crc.to_le_bytes());
        output
    }

    fn update_page_crc(page: &mut [u8]) {
        page[22..26].fill(0);
        let crc = ogg_crc(page);
        page[22..26].copy_from_slice(&crc.to_le_bytes());
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
        stream.extend(page(&NATIVE[42..], 4, 2, 1));
        stream
    }

    fn ogg_flac_missing_comment() -> Vec<u8> {
        let mut header = Vec::from(&b"\x7fFLAC\x01\x00\x00\x00fLaC"[..]);
        header.extend_from_slice(&NATIVE[4..42]);
        let mut stream = page(&header, 2, 0, 0);
        stream.extend(page(&NATIVE[42..], 4, 1, 1));
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
    fn rejects_final_granule_that_disagrees_with_streaminfo() {
        let mut input = ogg_flac();
        let page_start = input.len() - (27 + 1 + NATIVE.len() - 42);
        input[page_start + 6..page_start + 14].copy_from_slice(&2_u64.to_le_bytes());
        update_page_crc(&mut input[page_start..]);

        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
        assert!(decoder.read_frames(1).is_err());
    }

    #[test]
    fn verifies_terminal_granule_when_streaminfo_total_is_unknown() {
        let mut input = ogg_flac();
        clear_streaminfo_total_samples(&mut input);
        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
        assert_eq!(decoder.read_frames(1).unwrap().unwrap().frames(), 1);
        assert!(decoder.read_frames(1).unwrap().is_none());

        let mut input = ogg_flac();
        clear_streaminfo_total_samples(&mut input);
        let page_start = input.len() - (27 + 1 + NATIVE.len() - 42);
        input[page_start + 6..page_start + 14].copy_from_slice(&2_u64.to_le_bytes());
        update_page_crc(&mut input[page_start..]);
        let mut decoder = OggFlacDecoder::new(Cursor::new(input)).unwrap();
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
        let block = trim_final_block(block, 4, 3).unwrap().unwrap();
        assert_eq!(block.frames(), 3);
        assert_eq!(block.samples(), &Samples::I16(vec![1, 10, 2, 20, 3, 30]));
        assert!(trim_final_block(block, 3, 4).is_err());
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
