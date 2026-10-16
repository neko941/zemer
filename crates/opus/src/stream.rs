use std::io::Read;

use ogg::{OggPacket, OggPacketInfo, OggPacketReader, PAGE_FLAG_BOS};
use zemer_core::AudioError;

use crate::{OpusHead, OpusTags};

/// One Ogg Opus audio packet and its page's granule ownership information.
/// Multistream payloads remain packed and are not split into coded streams.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpusAudioPacket {
    pub packet: OggPacket,
    pub owns_granule: bool,
}

/// Reads the two Opus headers and returns subsequent Ogg audio packets.
pub struct OpusStreamReader<R> {
    packets: OggPacketReader<R>,
    head: OpusHead,
    tags: OpusTags,
}

impl<R: Read> OpusStreamReader<R> {
    pub fn new(reader: R) -> Result<Self, AudioError> {
        let mut packets = OggPacketReader::new(reader);
        let head_info = next_packet(&mut packets)?;
        let head_packet = &head_info.packet;
        if !head_packet.page_is_first
            || !head_packet.starts_page
            || !head_packet.ends_page
            || head_packet.page_flags & PAGE_FLAG_BOS == 0
            || !head_info.owns_granule
            || head_packet.page_granule_position != 0
        {
            return Err(AudioError::InvalidFormat(
                "OpusHead must be alone on the first Ogg page with granule zero",
            ));
        }
        let head = OpusHead::parse(&head_packet.bytes)
            .map_err(|_| AudioError::InvalidFormat("invalid OpusHead packet"))?;

        let tags_info = next_packet(&mut packets)?;
        let tags_packet = &tags_info.packet;
        if tags_packet.page_flags & PAGE_FLAG_BOS != 0
            || tags_info.start_page_sequence != 1
            || !tags_packet.starts_page
            || !tags_packet.ends_page
            || !tags_info.owns_granule
            || tags_packet.page_granule_position != 0
        {
            return Err(AudioError::InvalidFormat(
                "invalid OpusTags page layout or granule position",
            ));
        }
        let tags = OpusTags::parse(&tags_packet.bytes)
            .map_err(|_| AudioError::InvalidFormat("invalid OpusTags packet"))?;
        Ok(Self {
            packets,
            head,
            tags,
        })
    }

    pub const fn head(&self) -> &OpusHead {
        &self.head
    }

    pub const fn tags(&self) -> &OpusTags {
        &self.tags
    }

    /// Returns the next non-empty Ogg audio packet without decoding its payload.
    pub fn next_audio_packet(&mut self) -> Result<Option<OpusAudioPacket>, AudioError> {
        let Some(info) = self.packets.next_packet_info()? else {
            return Ok(None);
        };
        if info.packet.bytes.is_empty() {
            return Err(AudioError::InvalidFormat("empty Opus audio packet"));
        }
        Ok(Some(OpusAudioPacket {
            packet: info.packet,
            owns_granule: info.owns_granule,
        }))
    }
}

fn next_packet<R: Read>(packets: &mut OggPacketReader<R>) -> Result<OggPacketInfo, AudioError> {
    packets
        .next_packet_info()?
        .ok_or(AudioError::InvalidFormat("missing Opus header packet"))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ogg::OggPacketWriter;

    use super::OpusStreamReader;
    use crate::OpusTags;

    fn head() -> Vec<u8> {
        let mut packet = b"OpusHead".to_vec();
        packet.extend_from_slice(&[1, 1]);
        packet.extend_from_slice(&312_u16.to_le_bytes());
        packet.extend_from_slice(&48_000_u32.to_le_bytes());
        packet.extend_from_slice(&0_i16.to_le_bytes());
        packet.push(0);
        packet
    }

    fn stream(tags: &[u8], tags_granule: u64, audio: &[u8]) -> Vec<u8> {
        let mut writer = OggPacketWriter::new(Cursor::new(Vec::new()), 0x5A454D45);
        writer.write_packet(&head(), 0).unwrap();
        writer.write_packet(tags, tags_granule).unwrap();
        writer.write_packet(audio, 792).unwrap();
        writer.finish().unwrap();
        writer.into_inner().into_inner()
    }

    #[test]
    fn reads_headers_and_audio_packets() {
        let tags = OpusTags::new("test", vec!["TITLE=sample".into()])
            .to_packet()
            .unwrap();
        let mut reader = OpusStreamReader::new(Cursor::new(stream(&tags, 0, &[0xF8, 0]))).unwrap();
        assert_eq!(reader.head().channels, 1);
        assert_eq!(reader.tags().comments(), ["TITLE=sample"]);

        let audio = reader.next_audio_packet().unwrap().unwrap();
        assert!(audio.owns_granule);
        assert_eq!(audio.packet.page_granule_position, 792);
        assert_eq!(audio.packet.bytes, [0xF8, 0]);
        assert!(reader.next_audio_packet().unwrap().is_none());
    }

    #[test]
    fn accepts_comment_header_continued_across_pages() {
        let tags = OpusTags::new("test", vec![format!("COMMENT={}", "x".repeat(65_500))])
            .to_packet()
            .unwrap();
        let reader = OpusStreamReader::new(Cursor::new(stream(&tags, 0, &[0xF8, 0]))).unwrap();
        assert_eq!(reader.tags().comments()[0].len(), 65_508);
    }

    #[test]
    fn rejects_invalid_headers_and_audio_packets() {
        let tags = OpusTags::new("test", Vec::new()).to_packet().unwrap();
        assert!(OpusStreamReader::new(Cursor::new(stream(b"not tags", 0, &[0xF8, 0]))).is_err());
        assert!(OpusStreamReader::new(Cursor::new(stream(&tags, 1, &[0xF8, 0]))).is_err());
        let mut reader = OpusStreamReader::new(Cursor::new(stream(&tags, 0, &[]))).unwrap();
        assert!(reader.next_audio_packet().is_err());
    }
}
