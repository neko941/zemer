use std::{
    collections::{HashSet, VecDeque},
    io::{self, Read},
};

mod write;

pub use write::OggPacketWriter;

pub const DEFAULT_MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;
/// Maximum number of sequential logical streams tracked by one reader.
pub const MAX_CHAINED_STREAMS: usize = 65_536;
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
    /// Whether the packet's first segment begins a page, even if it continues later.
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
    partial_starts_page: bool,
    first_page: bool,
    ended: bool,
    eos_granule_position: Option<u64>,
    pending: VecDeque<(OggPacket, bool)>,
    chained_serials: HashSet<u32>,
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
            partial_starts_page: false,
            first_page: true,
            ended: false,
            eos_granule_position: None,
            pending: VecDeque::new(),
            chained_serials: HashSet::new(),
        }
    }

    /// Returns the serial number of the current logical stream after its first page is read.
    pub fn stream_serial(&self) -> Option<u32> {
        self.serial
    }

    /// Returns the granule position of the terminal page after that page is read.
    pub fn eos_granule_position(&self) -> Option<u64> {
        self.eos_granule_position
    }

    /// Returns the next complete packet, or `None` after this stream's EOS page.
    pub fn next_packet(&mut self) -> io::Result<Option<OggPacket>> {
        self.next_packet_with_granule()
            .map(|packet| packet.map(|(packet, _)| packet))
    }

    /// Returns the next packet and whether this packet owns its page's granule position.
    ///
    /// Ogg stores one granule per page, assigned to the last packet completed on that page.
    pub fn next_packet_with_granule(&mut self) -> io::Result<Option<(OggPacket, bool)>> {
        loop {
            if let Some(packet) = self.pending.pop_front() {
                return Ok(Some(packet));
            }
            if self.ended {
                return Ok(None);
            }
            if !self.read_page()? {
                return Err(invalid_ogg("empty Ogg stream"));
            }
        }
    }

    /// Starts the next sequential logical stream after this stream's EOS page.
    /// Returns `None` only when the input ends exactly at that page. Reused
    /// serial numbers across the chained streams are rejected.
    pub fn next_stream(self) -> io::Result<Option<Self>> {
        if !self.ended || !self.partial.is_empty() || !self.pending.is_empty() {
            return Err(invalid_ogg(
                "Ogg stream has not reached a clean EOS boundary",
            ));
        }
        let mut next = Self::with_packet_limit(self.reader, self.max_packet_bytes);
        next.chained_serials = self.chained_serials;
        Ok(next.read_page()?.then_some(next))
    }

    fn read_page(&mut self) -> io::Result<bool> {
        let mut header = [0_u8; 27];
        loop {
            match self.reader.read(&mut header[..1]) {
                Ok(0) => {
                    self.ended = true;
                    return if self.first_page {
                        Ok(false)
                    } else if self.partial.is_empty() {
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
        let lacing_end = 27 + segment_count;
        let mut page = Vec::new();
        page.try_reserve_exact(lacing_end)
            .map_err(|_| io::Error::other("Ogg page allocation failed"))?;
        page.extend_from_slice(&header);
        page.resize(lacing_end, 0);
        self.reader.read_exact(&mut page[27..])?;
        let body_len: usize = page[27..lacing_end]
            .iter()
            .map(|&length| usize::from(length))
            .sum();
        page.try_reserve_exact(body_len)
            .map_err(|_| io::Error::other("Ogg page allocation failed"))?;
        let page_len = lacing_end + body_len;
        page.resize(page_len, 0);
        self.reader.read_exact(&mut page[lacing_end..])?;
        let expected_crc = u32::from_le_bytes(header[22..26].try_into().unwrap());
        page[22..26].fill(0);
        if ogg_crc(&page) != expected_crc {
            return Err(invalid_ogg("Ogg page CRC mismatch"));
        }
        let lacing = &page[27..lacing_end];
        let body = &page[lacing_end..];
        let complete_packets = lacing.iter().filter(|&&lace| lace < 255).count();
        self.pending
            .try_reserve(complete_packets)
            .map_err(|_| io::Error::other("Ogg packet queue allocation failed"))?;

        if self.first_page {
            if flags & PAGE_FLAG_BOS == 0 || sequence != 0 {
                return Err(invalid_ogg("invalid Ogg beginning-of-stream page"));
            }
            if self.chained_serials.len() >= MAX_CHAINED_STREAMS {
                return Err(invalid_ogg(
                    "Ogg chained stream count exceeds the configured limit",
                ));
            }
            self.chained_serials
                .try_reserve(1)
                .map_err(|_| io::Error::other("Ogg stream serial allocation failed"))?;
            if !self.chained_serials.insert(serial) {
                return Err(invalid_ogg("duplicate Ogg stream serial number"));
            }
            self.serial = Some(serial);
            self.sequence = Some(sequence);
        } else {
            if flags & PAGE_FLAG_BOS != 0 || self.serial != Some(serial) {
                return Err(invalid_ogg("changed Ogg stream serial number"));
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
        let mut packet_starts_page = flags & PAGE_FLAG_CONTINUED == 0;
        let last_complete_segment = lacing.iter().rposition(|&lace| lace < 255);
        let mut cursor = 0;
        for (index, &lace) in lacing.iter().enumerate() {
            if self.partial.is_empty() {
                self.partial_starts_page = packet_starts_page;
            }
            let len = usize::from(lace);
            let new_len = self
                .partial
                .len()
                .checked_add(len)
                .filter(|size| *size <= self.max_packet_bytes)
                .ok_or_else(|| invalid_ogg("Ogg packet exceeds the configured limit"))?;
            self.partial
                .try_reserve(new_len - self.partial.len())
                .map_err(|_| io::Error::other("Ogg packet allocation failed"))?;
            self.partial.extend_from_slice(&body[cursor..cursor + len]);
            cursor += len;
            if lace < 255 {
                self.pending.push_back((
                    OggPacket {
                        bytes: std::mem::take(&mut self.partial),
                        page_sequence: sequence,
                        page_granule_position: granule_position,
                        page_flags: flags,
                        page_segment_count: segment_count,
                        page_is_first: is_first_page,
                        starts_page: self.partial_starts_page,
                        ends_page: index + 1 == lacing.len(),
                    },
                    last_complete_segment == Some(index),
                ));
                packet_starts_page = false;
            }
        }
        if flags & PAGE_FLAG_EOS != 0 {
            if !self.partial.is_empty() {
                return Err(invalid_ogg("Ogg EOS page ends in a partial packet"));
            }
            self.ended = true;
            self.eos_granule_position = Some(granule_position);
        }
        Ok(true)
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

    use super::{MAX_CHAINED_STREAMS, OggPacketReader, PAGE_FLAG_BOS, PAGE_FLAG_EOS, ogg_crc};

    fn page(body: &[u8], lacing: &[u8], flags: u8, sequence: u32, granule: u64) -> Vec<u8> {
        page_with_serial(body, lacing, flags, 7, sequence, granule)
    }

    fn page_with_serial(
        body: &[u8],
        lacing: &[u8],
        flags: u8,
        serial: u32,
        sequence: u32,
        granule: u64,
    ) -> Vec<u8> {
        assert_eq!(
            body.len(),
            lacing.iter().map(|&n| usize::from(n)).sum::<usize>()
        );
        let mut output = vec![0; 27];
        output[..4].copy_from_slice(b"OggS");
        output[5] = flags;
        output[6..14].copy_from_slice(&granule.to_le_bytes());
        output[14..18].copy_from_slice(&serial.to_le_bytes());
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
    fn rejects_every_truncated_page_prefix() {
        let valid = page(&[1, 2, 3], &[3], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 0, 3);
        for length in 0..valid.len() {
            let result = OggPacketReader::new(Cursor::new(&valid[..length])).next_packet();
            assert!(
                result.is_err(),
                "accepted truncated page prefix of {length} bytes"
            );
        }
    }

    #[test]
    fn rejects_every_truncated_continued_packet_prefix() {
        let mut valid = page(
            &vec![0x5a; 255],
            &[255],
            PAGE_FLAG_BOS,
            0,
            super::NO_GRANULE_POSITION,
        );
        valid.extend(page(
            &[0xa5],
            &[1],
            super::PAGE_FLAG_CONTINUED | PAGE_FLAG_EOS,
            1,
            256,
        ));

        for length in 0..valid.len() {
            let result = OggPacketReader::new(Cursor::new(&valid[..length])).next_packet();
            assert!(
                result.is_err(),
                "accepted truncated continued-packet prefix of {length} bytes"
            );
        }
    }

    #[test]
    fn granule_belongs_to_the_last_packet_completed_on_a_page() {
        let input = page(&[1, 2, 3], &[1, 1, 1], PAGE_FLAG_BOS, 0, 9);
        let mut reader = OggPacketReader::new(Cursor::new(input));
        let packets = [
            reader.next_packet_with_granule().unwrap().unwrap(),
            reader.next_packet_with_granule().unwrap().unwrap(),
            reader.next_packet_with_granule().unwrap().unwrap(),
        ];
        assert!(!packets[0].1);
        assert!(!packets[1].1);
        assert!(packets[2].1);

        let input = page(&vec![0; 256], &[1, 255], PAGE_FLAG_BOS, 0, 9);
        let mut reader = OggPacketReader::new(Cursor::new(input));
        assert!(reader.next_packet_with_granule().unwrap().unwrap().1);
    }

    #[test]
    fn joins_packets_continued_across_pages() {
        let first = vec![0x5a; 255];
        let second = [0xa5, b'b', b'c'];
        let mut input = page(&first, &[255], 2, 0, u64::MAX);
        input.extend(page(&second, &[1, 2], 5, 1, 257));
        let mut reader = OggPacketReader::new(Cursor::new(input));
        let packet = reader.next_packet().unwrap().unwrap();
        assert_eq!(packet.bytes.len(), 256);
        assert_eq!(&packet.bytes[..255], first);
        assert_eq!(packet.bytes[255], 0xa5);
        assert_eq!(packet.page_sequence, 1);
        assert_eq!(packet.page_granule_position, 257);
        assert!(!packet.page_is_first);
        assert!(packet.starts_page);
        assert!(!packet.ends_page);

        let packet = reader.next_packet().unwrap().unwrap();
        assert_eq!(packet.bytes, b"bc");
        assert_eq!(packet.page_granule_position, 257);
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
        assert!(first.starts_page);
        assert!(!second.starts_page);
        assert!(third.starts_page);
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
    fn rejects_malformed_page_headers_and_packet_continuations() {
        let mut bad_capture = page(&[1], &[1], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 0, 1);
        bad_capture[0] = b'X';
        assert!(
            OggPacketReader::new(Cursor::new(bad_capture))
                .next_packet()
                .is_err()
        );

        let mut bad_version = page(&[1], &[1], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 0, 1);
        bad_version[4] = 1;
        assert!(
            OggPacketReader::new(Cursor::new(bad_version))
                .next_packet()
                .is_err()
        );

        let mut reserved_flags = page(&[1], &[1], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 0, 1);
        reserved_flags[5] |= 0x08;
        assert!(
            OggPacketReader::new(Cursor::new(reserved_flags))
                .next_packet()
                .is_err()
        );

        let spurious_continuation = page(
            &[1],
            &[1],
            PAGE_FLAG_BOS | super::PAGE_FLAG_CONTINUED | PAGE_FLAG_EOS,
            0,
            1,
        );
        assert!(
            OggPacketReader::new(Cursor::new(spurious_continuation))
                .next_packet()
                .is_err()
        );

        let mut missing_continuation = page(&vec![0; 255], &[255], PAGE_FLAG_BOS, 0, u64::MAX);
        missing_continuation.extend(page(&[1], &[1], PAGE_FLAG_EOS, 1, 256));
        assert!(
            OggPacketReader::new(Cursor::new(missing_continuation))
                .next_packet()
                .is_err()
        );

        let eos_with_partial_packet =
            page(&vec![0; 255], &[255], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 0, 1);
        assert!(
            OggPacketReader::new(Cursor::new(eos_with_partial_packet))
                .next_packet()
                .is_err()
        );
    }

    #[test]
    fn enforces_configured_packet_bound() {
        let input = page(&[1, 2], &[2], 6, 0, 2);
        let mut reader = OggPacketReader::with_packet_limit(Cursor::new(input), 1);
        assert!(reader.next_packet().is_err());
    }

    #[test]
    fn page_reader_does_not_panic_on_single_bit_mutations_with_valid_crc() {
        let valid = page(&[0x55], &[1], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 0, 1);
        for byte in 0..valid.len() {
            if (22..26).contains(&byte) {
                continue;
            }
            for bit in 0..u8::BITS {
                let mut mutated = valid.clone();
                mutated[byte] ^= 1 << bit;
                mutated[22..26].fill(0);
                let crc = ogg_crc(&mutated);
                mutated[22..26].copy_from_slice(&crc.to_le_bytes());
                assert!(
                    std::panic::catch_unwind(|| {
                        OggPacketReader::new(Cursor::new(mutated)).next_packet()
                    })
                    .is_ok(),
                    "Ogg page parser panicked after flipping bit {bit} of byte {byte}"
                );
            }
        }
    }

    #[test]
    fn reads_chained_streams_and_rejects_trailing_garbage() {
        let first = page(&[1], &[1], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 0, 1);
        let mut chained = page(&[2], &[1], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 0, 1);
        chained[14..18].copy_from_slice(&8_u32.to_le_bytes());
        chained[22..26].fill(0);
        let crc = ogg_crc(&chained);
        chained[22..26].copy_from_slice(&crc.to_le_bytes());
        let mut input = first.clone();
        input.extend_from_slice(&chained);
        let mut reader = OggPacketReader::new(Cursor::new(input));
        assert_eq!(reader.next_packet().unwrap().unwrap().bytes, [1]);
        assert!(reader.next_packet().unwrap().is_none());
        let mut next = reader.next_stream().unwrap().unwrap();
        assert_eq!(next.next_packet().unwrap().unwrap().bytes, [2]);
        assert!(next.next_packet().unwrap().is_none());
        assert!(next.next_stream().unwrap().is_none());

        let mut input = first;
        input.push(0);
        let mut reader = OggPacketReader::new(Cursor::new(input));
        assert_eq!(reader.next_packet().unwrap().unwrap().bytes, [1]);
        assert!(reader.next_packet().unwrap().is_none());
        assert!(reader.next_stream().is_err());

        let second_serial = page_with_serial(&[2], &[1], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 8, 0, 1);
        let duplicate_serial = page_with_serial(&[3], &[1], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 7, 0, 1);
        let mut input = page(&[1], &[1], PAGE_FLAG_BOS | PAGE_FLAG_EOS, 0, 1);
        input.extend_from_slice(&second_serial);
        input.extend_from_slice(&duplicate_serial);
        let mut reader = OggPacketReader::new(Cursor::new(input));
        assert_eq!(reader.next_packet().unwrap().unwrap().bytes, [1]);
        assert!(reader.next_packet().unwrap().is_none());
        let mut second = reader.next_stream().unwrap().unwrap();
        assert_eq!(second.next_packet().unwrap().unwrap().bytes, [2]);
        assert!(second.next_packet().unwrap().is_none());
        assert!(second.next_stream().is_err());
    }

    #[test]
    fn bounds_the_number_of_chained_streams() {
        let mut input = Vec::new();
        for serial in 0..=MAX_CHAINED_STREAMS as u32 {
            input.extend_from_slice(&page_with_serial(
                &[0],
                &[1],
                PAGE_FLAG_BOS | PAGE_FLAG_EOS,
                serial,
                0,
                0,
            ));
        }

        let mut reader = OggPacketReader::new(Cursor::new(input));
        for stream in 0..MAX_CHAINED_STREAMS {
            assert!(reader.next_packet().unwrap().is_some());
            if stream + 1 < MAX_CHAINED_STREAMS {
                reader = reader.next_stream().unwrap().unwrap();
            }
        }
        assert!(reader.next_stream().is_err());
    }
}
