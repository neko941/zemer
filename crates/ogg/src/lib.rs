use std::{
    collections::VecDeque,
    io::{self, Read},
};

pub const DEFAULT_MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;
pub const PAGE_FLAG_CONTINUED: u8 = 0x01;
pub const PAGE_FLAG_BOS: u8 = 0x02;
pub const PAGE_FLAG_EOS: u8 = 0x04;
pub const NO_GRANULE_POSITION: u64 = u64::MAX;

/// One complete Ogg packet and the page facts needed by codec mappings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OggPacket {
    pub bytes: Vec<u8>,
    pub page_sequence: u32,
    /// Codec-defined position stored in this packet's page. `u64::MAX` is
    /// Ogg's `-1` sentinel for pages with no completed packets.
    pub page_granule_position: u64,
    pub page_flags: u8,
    pub page_segment_count: usize,
    pub page_is_first: bool,
    pub starts_page: bool,
    pub ends_page: bool,
}

/// Reads one Ogg logical stream as packets, validating page framing and CRCs.
pub struct OggPacketReader<R> {
    reader: R,
    max_packet_bytes: usize,
    serial: Option<u32>,
    sequence: Option<u32>,
    partial: Vec<u8>,
    first_page: bool,
    ended: bool,
    eof_checked: bool,
    pending: VecDeque<OggPacket>,
}

impl<R: Read> OggPacketReader<R> {
    pub fn new(reader: R) -> Self {
        Self::with_packet_limit(reader, DEFAULT_MAX_PACKET_BYTES)
    }

    pub fn with_packet_limit(reader: R, max_packet_bytes: usize) -> Self {
        Self {
            reader,
            max_packet_bytes,
            serial: None,
            sequence: None,
            partial: Vec::new(),
            first_page: true,
            ended: false,
            eof_checked: false,
            pending: VecDeque::new(),
        }
    }

    /// Returns the next complete packet, or `None` after the stream's EOS page.
    pub fn next_packet(&mut self) -> io::Result<Option<OggPacket>> {
        loop {
            if let Some(packet) = self.pending.pop_front() {
                return Ok(Some(packet));
            }
            if self.ended {
                if self.eof_checked {
                    return Ok(None);
                }
                let mut trailing = [0];
                loop {
                    match self.reader.read(&mut trailing) {
                        Ok(0) => {
                            self.eof_checked = true;
                            return Ok(None);
                        }
                        Ok(_) => return Err(invalid_ogg("trailing data after Ogg EOS page")),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => return Err(error),
                    }
                }
            }
            self.read_page()?;
        }
    }

    fn read_page(&mut self) -> io::Result<()> {
        let mut header = [0_u8; 27];
        loop {
            match self.reader.read(&mut header[..1]) {
                Ok(0) => {
                    self.ended = true;
                    return if self.partial.is_empty() {
                        Err(invalid_ogg("Ogg stream ended without an EOS page"))
                    } else {
                        Err(invalid_ogg("truncated Ogg packet"))
                    };
                }
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        self.reader.read_exact(&mut header[1..])?;
        if &header[..4] != b"OggS" || header[4] != 0 || header[5] & !7 != 0 {
            return Err(invalid_ogg("invalid Ogg page header"));
        }
        let flags = header[5];
        let granule_position = u64::from_le_bytes(header[6..14].try_into().unwrap());
        let serial = u32::from_le_bytes(header[14..18].try_into().unwrap());
        let sequence = u32::from_le_bytes(header[18..22].try_into().unwrap());
        let segment_count = usize::from(header[26]);
        let mut lacing = vec![0; segment_count];
        self.reader.read_exact(&mut lacing)?;
        let body_len: usize = lacing.iter().map(|&length| usize::from(length)).sum();
        let mut body = vec![0; body_len];
        self.reader.read_exact(&mut body)?;

        let mut page = Vec::with_capacity(27 + segment_count + body_len);
        page.extend_from_slice(&header);
        page.extend_from_slice(&lacing);
        page.extend_from_slice(&body);
        let expected_crc = u32::from_le_bytes(header[22..26].try_into().unwrap());
        page[22..26].fill(0);
        if ogg_crc(&page) != expected_crc {
            return Err(invalid_ogg("Ogg page CRC mismatch"));
        }

        if self.first_page {
            if flags & PAGE_FLAG_BOS == 0 || sequence != 0 {
                return Err(invalid_ogg("invalid Ogg beginning-of-stream page"));
            }
            self.serial = Some(serial);
            self.sequence = Some(sequence);
        } else {
            if flags & PAGE_FLAG_BOS != 0 || self.serial != Some(serial) {
                return Err(invalid_ogg("chained or changed Ogg stream is unsupported"));
            }
            let expected = self.sequence.unwrap().wrapping_add(1);
            if sequence != expected {
                return Err(invalid_ogg("Ogg page sequence is discontinuous"));
            }
            self.sequence = Some(sequence);
        }
        if (flags & PAGE_FLAG_CONTINUED != 0) == self.partial.is_empty() {
            return Err(invalid_ogg("Ogg continued-packet flag mismatch"));
        }

        let is_first_page = self.first_page;
        self.first_page = false;
        let mut cursor = 0;
        for (index, &lace) in lacing.iter().enumerate() {
            let len = usize::from(lace);
            let new_len = self
                .partial
                .len()
                .checked_add(len)
                .filter(|size| *size <= self.max_packet_bytes)
                .ok_or_else(|| invalid_ogg("Ogg packet exceeds the configured limit"))?;
            self.partial.reserve(new_len - self.partial.len());
            self.partial.extend_from_slice(&body[cursor..cursor + len]);
            cursor += len;
            if lace < 255 {
                self.pending.push_back(OggPacket {
                    bytes: std::mem::take(&mut self.partial),
                    page_sequence: sequence,
                    page_granule_position: granule_position,
                    page_flags: flags,
                    page_segment_count: segment_count,
                    page_is_first: is_first_page,
                    starts_page: index == 0 && flags & PAGE_FLAG_CONTINUED == 0,
                    ends_page: index + 1 == lacing.len(),
                });
            }
        }
        if flags & PAGE_FLAG_EOS != 0 {
            if !self.partial.is_empty() {
                return Err(invalid_ogg("Ogg EOS page ends in a partial packet"));
            }
            self.ended = true;
        }
        Ok(())
    }
}

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

fn invalid_ogg(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{OggPacketReader, PAGE_FLAG_BOS, PAGE_FLAG_EOS, ogg_crc};

    fn page(body: &[u8], lacing: &[u8], flags: u8, sequence: u32, granule: u64) -> Vec<u8> {
        assert_eq!(
            body.len(),
            lacing.iter().map(|&n| usize::from(n)).sum::<usize>()
        );
        let mut output = vec![0; 27];
        output[..4].copy_from_slice(b"OggS");
        output[5] = flags;
        output[6..14].copy_from_slice(&granule.to_le_bytes());
        output[14..18].copy_from_slice(&7_u32.to_le_bytes());
        output[18..22].copy_from_slice(&sequence.to_le_bytes());
        output[26] = lacing.len() as u8;
        output.extend_from_slice(lacing);
        output.extend_from_slice(body);
        let crc = ogg_crc(&output);
        output[22..26].copy_from_slice(&crc.to_le_bytes());
        output
    }

    #[test]
    fn reads_packet_and_page_boundaries() {
        let input = page(&[1, 2, 3], &[3], 6, 0, 3);
        let mut reader = OggPacketReader::new(Cursor::new(input));
        let packet = reader.next_packet().unwrap().unwrap();
        assert_eq!(packet.bytes, [1, 2, 3]);
        assert_eq!(packet.page_sequence, 0);
        assert_eq!(packet.page_granule_position, 3);
        assert!(packet.page_is_first && packet.starts_page && packet.ends_page);
        assert!(reader.next_packet().unwrap().is_none());
    }

    #[test]
    fn joins_packets_continued_across_pages() {
        let first = vec![0x5a; 255];
        let second = [0xa5];
        let mut input = page(&first, &[255], 2, 0, u64::MAX);
        input.extend(page(&second, &[1], 5, 1, 256));
        let mut reader = OggPacketReader::new(Cursor::new(input));
        let packet = reader.next_packet().unwrap().unwrap();
        assert_eq!(packet.bytes.len(), 256);
        assert_eq!(&packet.bytes[..255], first);
        assert_eq!(packet.bytes[255], 0xa5);
        assert_eq!(packet.page_sequence, 1);
        assert_eq!(packet.page_granule_position, 256);
        assert!(!packet.page_is_first);
        assert!(!packet.starts_page);
        assert!(packet.ends_page);
    }

    #[test]
    fn preserves_page_granule_for_each_packet() {
        let mut input = page(b"ab", &[1, 1], 2, 0, 0);
        input.extend(page(b"c", &[1], 4, 1, 2));
        let mut reader = OggPacketReader::new(Cursor::new(input));

        let first = reader.next_packet().unwrap().unwrap();
        let second = reader.next_packet().unwrap().unwrap();
        let third = reader.next_packet().unwrap().unwrap();
        assert_eq!(first.page_granule_position, 0);
        assert_eq!(second.page_granule_position, 0);
        assert_eq!(third.page_granule_position, 2);
        assert!(reader.next_packet().unwrap().is_none());
    }

    #[test]
    fn rejects_bad_crc_sequence_and_unterminated_streams() {
        let mut bad_crc = page(&[1], &[1], 6, 0, 1);
        *bad_crc.last_mut().unwrap() ^= 1;
        assert!(
            OggPacketReader::new(Cursor::new(bad_crc))
                .next_packet()
                .is_err()
        );

        let mut bad_sequence = page(&[1], &[1], 2, 0, 1);
        bad_sequence.extend(page(&[2], &[1], 4, 2, 2));
        let mut reader = OggPacketReader::new(Cursor::new(bad_sequence));
        assert_eq!(reader.next_packet().unwrap().unwrap().bytes, [1]);
        assert!(reader.next_packet().is_err());

        let unterminated = page(&[1], &[1], 2, 0, 1);
        let mut reader = OggPacketReader::new(Cursor::new(unterminated));
        assert_eq!(reader.next_packet().unwrap().unwrap().bytes, [1]);
        assert!(reader.next_packet().is_err());
    }

    #[test]
    fn enforces_configured_packet_bound() {
        let input = page(&[1, 2], &[2], 6, 0, 2);
        let mut reader = OggPacketReader::with_packet_limit(Cursor::new(input), 1);
        assert!(reader.next_packet().is_err());
    }

    #[test]
    fn rejects_chained_streams_and_trailing_bytes_after_eos() {
        let first = page(&[1], &[1], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 0, 1);
        let chained = page(&[2], &[1], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 0, 1);
        let mut input = first.clone();
        input.extend_from_slice(&chained);
        let mut reader = OggPacketReader::new(Cursor::new(input));
        assert_eq!(reader.next_packet().unwrap().unwrap().bytes, [1]);
        assert!(reader.next_packet().is_err());

        let mut input = first;
        input.push(0);
        let mut reader = OggPacketReader::new(Cursor::new(input));
        assert_eq!(reader.next_packet().unwrap().unwrap().bytes, [1]);
        assert!(reader.next_packet().is_err());
    }
}
