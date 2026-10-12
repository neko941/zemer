use std::io::{self, Write};

use crate::{NO_GRANULE_POSITION, PAGE_FLAG_BOS, PAGE_FLAG_CONTINUED, PAGE_FLAG_EOS};

/// Writes one-packet-per-page Ogg logical streams.
pub struct OggPacketWriter<W> {
    writer: W,
    serial: u32,
    sequence: u32,
    pending: Option<(Vec<u8>, u64)>,
    started: bool,
    poisoned: bool,
    finished: bool,
}

impl<W: Write> OggPacketWriter<W> {
    pub fn new(writer: W, serial: u32) -> Self {
        Self {
            writer,
            serial,
            sequence: 0,
            pending: None,
            started: false,
            poisoned: false,
            finished: false,
        }
    }

    pub fn write_packet(&mut self, packet: &[u8], granule_position: u64) -> io::Result<()> {
        self.write_packet_owned(packet.to_vec(), granule_position)
    }

    pub fn write_packet_owned(&mut self, packet: Vec<u8>, granule_position: u64) -> io::Result<()> {
        if self.finished || self.poisoned {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ogg packet writer is finished or unusable",
            ));
        }
        if let Some((bytes, granule)) = self.pending.take() {
            if let Err(error) = self.write_packet_pages(&bytes, granule, false) {
                self.poisoned = true;
                return Err(error);
            }
        }
        self.pending = Some((packet, granule_position));
        Ok(())
    }

    pub fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        if self.poisoned {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ogg packet writer is unusable after an earlier write failure",
            ));
        }
        let Some((bytes, granule)) = self.pending.take() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot finish an empty Ogg stream",
            ));
        };
        let result = self
            .write_packet_pages(&bytes, granule, true)
            .and_then(|()| self.writer.flush());
        if result.is_err() {
            self.poisoned = true;
        } else {
            self.finished = true;
        }
        result
    }

    pub fn into_inner(self) -> W {
        self.writer
    }

    fn write_packet_pages(
        &mut self,
        packet: &[u8],
        granule_position: u64,
        eos: bool,
    ) -> io::Result<()> {
        let mut lacing = vec![255; packet.len() / 255];
        lacing.push((packet.len() % 255) as u8);
        let page_count = lacing.len().div_ceil(255);
        let mut body_start = 0;
        for (page_index, laces) in lacing.chunks(255).enumerate() {
            let body_len: usize = laces.iter().map(|&lace| usize::from(lace)).sum();
            let last_page = page_index + 1 == page_count;
            let mut page = vec![0; 27];
            page[..4].copy_from_slice(b"OggS");
            page[5] = if page_index != 0 {
                PAGE_FLAG_CONTINUED
            } else {
                0
            };
            if !self.started {
                page[5] |= PAGE_FLAG_BOS;
            }
            if eos && last_page {
                page[5] |= PAGE_FLAG_EOS;
            }
            let granule = if last_page {
                granule_position
            } else {
                NO_GRANULE_POSITION
            };
            page[6..14].copy_from_slice(&granule.to_le_bytes());
            page[14..18].copy_from_slice(&self.serial.to_le_bytes());
            page[18..22].copy_from_slice(&self.sequence.to_le_bytes());
            page[26] = laces.len() as u8;
            page.extend_from_slice(laces);
            page.extend_from_slice(&packet[body_start..body_start + body_len]);
            let checksum = ogg_crc(&page);
            page[22..26].copy_from_slice(&checksum.to_le_bytes());
            self.writer.write_all(&page)?;
            self.started = true;
            self.sequence = self.sequence.wrapping_add(1);
            body_start += body_len;
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::{OggPacketReader, PAGE_FLAG_BOS, PAGE_FLAG_CONTINUED, PAGE_FLAG_EOS};

    use super::OggPacketWriter;

    #[test]
    fn round_trips_single_and_continued_packets_with_final_granule() {
        let mut writer = OggPacketWriter::new(Cursor::new(Vec::new()), 17);
        writer.write_packet(b"header", 0).unwrap();
        let medium = vec![0x31; 500];
        writer.write_packet(&medium, 0).unwrap();
        let audio = vec![0x5a; 70_000];
        writer.write_packet(&audio, 4096).unwrap();
        writer.finish().unwrap();
        let bytes = writer.into_inner().into_inner();

        let mut reader = OggPacketReader::new(Cursor::new(bytes));
        let header = reader.next_packet().unwrap().unwrap();
        assert_eq!(header.bytes, b"header");
        assert_eq!(header.page_flags & PAGE_FLAG_BOS, PAGE_FLAG_BOS);
        assert_eq!(header.page_granule_position, 0);

        let packet = reader.next_packet().unwrap().unwrap();
        assert_eq!(packet.bytes, medium);
        assert!(packet.starts_page);

        let packet = reader.next_packet().unwrap().unwrap();
        assert_eq!(packet.bytes, audio);
        assert_eq!(packet.page_granule_position, 4096);
        assert!(!packet.starts_page);
        assert_eq!(packet.page_flags & PAGE_FLAG_CONTINUED, PAGE_FLAG_CONTINUED);
        assert_eq!(packet.page_flags & PAGE_FLAG_EOS, PAGE_FLAG_EOS);
        assert!(reader.next_packet().unwrap().is_none());
    }
}
