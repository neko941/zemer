use std::io::{self, Write};

use crate::{NO_GRANULE_POSITION, PAGE_FLAG_BOS, PAGE_FLAG_CONTINUED, PAGE_FLAG_EOS, ogg_crc};

/// Writes Ogg streams with one packet per page, continuing large packets across pages.
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
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(packet.len())
            .map_err(|_| io::Error::other("Ogg packet allocation failed"))?;
        owned.extend_from_slice(packet);
        self.write_packet_owned(owned, granule_position)
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

    /// Replaces the granule position of the final packet before `finish`.
    pub fn set_pending_granule_position(&mut self, granule_position: u64) -> io::Result<()> {
        let Some((_, pending_granule)) = self.pending.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ogg writer has no pending packet",
            ));
        };
        *pending_granule = granule_position;
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
        let full_segments = packet.len() / 255;
        let segment_count = full_segments
            .checked_add(1)
            .ok_or_else(|| io::Error::other("Ogg packet has too many segments"))?;
        let page_count = segment_count.div_ceil(255);
        for page_index in 0..page_count {
            let lace_start = page_index
                .checked_mul(255)
                .ok_or_else(|| io::Error::other("Ogg page segment offset overflow"))?;
            let lace_count = (segment_count - lace_start).min(255);
            let body_start = lace_start
                .checked_mul(255)
                .ok_or_else(|| io::Error::other("Ogg page body offset overflow"))?;
            let body_len = packet
                .len()
                .checked_sub(body_start)
                .ok_or_else(|| io::Error::other("invalid Ogg page body offset"))?
                .min(lace_count * 255);
            let body_end = body_start
                .checked_add(body_len)
                .ok_or_else(|| io::Error::other("Ogg page body size overflow"))?;
            let page_capacity = 27_usize
                .checked_add(lace_count)
                .and_then(|size| size.checked_add(body_len))
                .ok_or_else(|| io::Error::other("Ogg page size overflow"))?;
            let last_page = page_index + 1 == page_count;
            let mut page = Vec::new();
            page.try_reserve_exact(page_capacity)
                .map_err(|_| io::Error::other("Ogg page allocation failed"))?;
            page.resize(27, 0);
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
            page[26] = lace_count as u8;
            for lace_index in lace_start..lace_start + lace_count {
                page.push(if lace_index < full_segments {
                    255
                } else {
                    (packet.len() % 255) as u8
                });
            }
            page.extend_from_slice(&packet[body_start..body_end]);
            let checksum = ogg_crc(&page);
            page[22..26].copy_from_slice(&checksum.to_le_bytes());
            self.writer.write_all(&page)?;
            self.started = true;
            self.sequence = self.sequence.wrapping_add(1);
        }
        Ok(())
    }
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
        assert!(packet.starts_page);
        assert_eq!(packet.page_flags & PAGE_FLAG_CONTINUED, PAGE_FLAG_CONTINUED);
        assert_eq!(packet.page_flags & PAGE_FLAG_EOS, PAGE_FLAG_EOS);
        assert!(reader.next_packet().unwrap().is_none());
    }

    #[test]
    fn round_trips_packets_at_lacing_page_boundaries() {
        let lengths = [0, 1, 254, 255, 256, 65_024, 65_025, 65_026, 130_050];
        let packets: Vec<Vec<u8>> = lengths
            .iter()
            .enumerate()
            .map(|(index, &length)| vec![index as u8; length])
            .collect();
        let mut writer = OggPacketWriter::new(Cursor::new(Vec::new()), 23);
        for (index, packet) in packets.iter().enumerate() {
            writer.write_packet(packet, index as u64).unwrap();
        }
        writer.finish().unwrap();

        let mut reader = OggPacketReader::new(Cursor::new(writer.into_inner().into_inner()));
        for (index, expected) in packets.iter().enumerate() {
            let packet = reader.next_packet().unwrap().unwrap();
            assert_eq!(&packet.bytes, expected);
            assert_eq!(packet.page_granule_position, index as u64);
        }
        assert!(reader.next_packet().unwrap().is_none());
    }
}
