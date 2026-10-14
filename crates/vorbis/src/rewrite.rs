use std::io::{Read, Write};

use ogg::{NO_GRANULE_POSITION, OggPacket, OggPacketReader, OggPacketWriter, PAGE_FLAG_BOS};
use zemer_core::{AudioError, Metadata};

use crate::{
    VorbisCommentHeader, VorbisIdentificationHeader, VorbisSetupHeader,
    headers::encode_comment_header,
};

/// Rewrites Vorbis comments in the first logical stream and retains encoded
/// packets, including the metadata and audio packets in later chained links.
///
/// Ogg pages are repaginated with one packet per page. Callers editing files
/// should write to a new file and replace the source only after this succeeds.
pub fn rewrite_metadata<R: Read, W: Write>(
    input: R,
    mut output: W,
    metadata: &Metadata,
) -> Result<(), AudioError> {
    let mut packets = OggPacketReader::new(input);
    let mut link_index = 0;
    loop {
        let (first, first_owns_granule) = packets
            .next_packet_with_granule()?
            .ok_or(AudioError::InvalidFormat("empty Ogg Vorbis stream"))?;
        let serial = packets
            .stream_serial()
            .ok_or(AudioError::InvalidData("missing Ogg stream serial"))?;
        if !first.page_is_first
            || !first.starts_page
            || !first.ends_page
            || first.page_flags & PAGE_FLAG_BOS == 0
        {
            return Err(AudioError::InvalidFormat(
                "Vorbis identification packet must be alone on the first Ogg page",
            ));
        }
        let identification = VorbisIdentificationHeader::parse(&first.bytes)?;
        let (comment, comment_owns_granule) = next_header(&mut packets)?;
        let comments = VorbisCommentHeader::parse(&comment.bytes)?;
        let (setup, setup_owns_granule) = next_header(&mut packets)?;
        VorbisSetupHeader::parse(&setup.bytes, &identification)?;

        {
            let mut writer = OggPacketWriter::new(&mut output, serial);
            write_packet(&mut writer, first, first_owns_granule)?;
            let comment_bytes = if link_index == 0 {
                encode_comment_header(comments.vendor(), metadata)?
            } else {
                comment.bytes.clone()
            };
            write_packet_bytes(
                &mut writer,
                comment_bytes,
                comment.page_granule_position,
                comment_owns_granule,
            )?;
            write_packet(&mut writer, setup, setup_owns_granule)?;

            let mut last_packet_has_eos = false;
            while let Some((packet, owns_granule)) = packets.next_packet_with_granule()? {
                last_packet_has_eos = packet.page_flags & ogg::PAGE_FLAG_EOS != 0;
                write_packet(&mut writer, packet, owns_granule)?;
            }
            if !last_packet_has_eos {
                if let Some(granule) = packets.eos_granule_position() {
                    writer.set_pending_granule_position(granule)?;
                }
            }
            writer.finish()?;
        }

        link_index += 1;
        let Some(next) = packets.next_stream()? else {
            return Ok(());
        };
        packets = next;
    }
}

fn next_header<R: Read>(packets: &mut OggPacketReader<R>) -> Result<(OggPacket, bool), AudioError> {
    let packet = packets
        .next_packet_with_granule()?
        .ok_or(AudioError::InvalidFormat("truncated Vorbis headers"))?;
    Ok(packet)
}

fn write_packet<W: Write>(
    writer: &mut OggPacketWriter<W>,
    packet: OggPacket,
    owns_granule: bool,
) -> Result<(), AudioError> {
    write_packet_bytes(
        writer,
        packet.bytes,
        packet.page_granule_position,
        owns_granule,
    )
}

fn write_packet_bytes<W: Write>(
    writer: &mut OggPacketWriter<W>,
    packet: Vec<u8>,
    granule: u64,
    owns_granule: bool,
) -> Result<(), AudioError> {
    writer.write_packet_owned(
        packet,
        if owns_granule {
            granule
        } else {
            NO_GRANULE_POSITION
        },
    )?;
    Ok(())
}
