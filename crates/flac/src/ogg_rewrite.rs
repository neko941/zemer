use std::io::{Read, Write};

use ogg::{OggPacket, OggPacketReader, OggPacketWriter, PAGE_FLAG_EOS};
use zemer_core::{AudioError, Metadata};

use crate::{
    metadata::MAX_METADATA_BYTES,
    ogg::frame_block_size,
    rewrite::{encode_comments, read_vendor, write_block},
};

/// Rewrites the first Ogg-FLAC link's Vorbis comments and preserves every FLAC audio packet.
///
/// The output is repaginated with one packet per page. Chained links after the first retain their
/// original packets and metadata. Callers should write to a new file and replace the source only
/// after this function succeeds.
pub fn rewrite_ogg_metadata<R: Read, W: Write>(
    input: R,
    mut output: W,
    metadata: &Metadata,
) -> Result<(), AudioError> {
    let mut packets = OggPacketReader::new(input);
    let mut link_index = 0;
    loop {
        let first = packets
            .next_packet()?
            .ok_or(AudioError::InvalidFormat("empty Ogg stream"))?;
        let serial = packets
            .stream_serial()
            .ok_or(AudioError::InvalidData("missing Ogg stream serial"))?;
        let (mapping, mut headers, first_audio, first_audio_has_granule) = if first.bytes == b"fLaC"
        {
            read_legacy_headers(&mut packets, &first)?
        } else {
            validate_mapping(&first)?;
            let declared = u16::from_be_bytes([first.bytes[7], first.bytes[8]]);
            let (headers, audio, has_granule) = read_headers(&mut packets, declared)?;
            (first.bytes, headers, audio, has_granule)
        };
        if link_index == 0 {
            rewrite_comment(&mut headers, metadata)?;
        }
        let retained_headers = headers.iter().try_fold(0_usize, |total, packet| {
            total
                .checked_add(packet.bytes.len())
                .filter(|&size| size <= MAX_METADATA_BYTES + 4)
        });
        if retained_headers.is_none() {
            return Err(AudioError::InvalidData(
                "FLAC metadata exceeds the 128 MiB limit",
            ));
        }

        let mut writer = OggPacketWriter::new(&mut output, serial);
        writer.write_packet(&mapping, 0)?;
        for header in headers {
            writer.write_packet(&header.bytes, 0)?;
        }
        let initial_audio =
            normalize_initial_audio(&mut packets, first_audio, first_audio_has_granule)?;
        let mut decoded_samples = initial_audio.iter().try_fold(0_u64, |total, (packet, _)| {
            let samples = frame_block_size(&packet.bytes)? as u64;
            total.checked_add(samples).ok_or(AudioError::InvalidData(
                "Ogg-FLAC initial sample count overflow",
            ))
        })?;
        let (first_packet, first_granule) = initial_audio.first().ok_or(
            AudioError::InvalidData("missing initial Ogg-FLAC audio packet"),
        )?;
        let granule_offset = first_granule
            .saturating_add(1)
            .saturating_sub(frame_block_size(&first_packet.bytes)? as u64);
        let mut audio = initial_audio.into_iter();
        loop {
            let (packet, granule) = match audio.next() {
                Some(packet) => packet,
                None => match packets.next_packet_with_granule()? {
                    Some((packet, owns_granule)) => {
                        if packet.bytes.is_empty() {
                            if packet.page_flags & PAGE_FLAG_EOS == 0 || !owns_granule {
                                return Err(AudioError::InvalidFormat(
                                    "empty non-terminal Ogg-FLAC audio packet",
                                ));
                            }
                            if packet.page_granule_position > i64::MAX as u64 {
                                return Err(AudioError::InvalidFormat(
                                    "invalid Ogg-FLAC final granule",
                                ));
                            }
                            writer
                                .write_packet_owned(packet.bytes, packet.page_granule_position)?;
                            continue;
                        }
                        let block_size = frame_block_size(&packet.bytes)? as u64;
                        decoded_samples = decoded_samples
                            .checked_add(block_size)
                            .ok_or(AudioError::InvalidData("Ogg-FLAC sample count overflow"))?;
                        let granule = if owns_granule {
                            packet.page_granule_position
                        } else {
                            granule_offset
                                .checked_add(decoded_samples)
                                .and_then(|samples| samples.checked_sub(1))
                                .ok_or(AudioError::InvalidData(
                                    "Ogg-FLAC granule position overflow",
                                ))?
                        };
                        (packet, granule)
                    }
                    None => break,
                },
            };
            validate_audio_packet(&packet)?;
            if packet.page_flags & PAGE_FLAG_EOS != 0 && granule > i64::MAX as u64 {
                return Err(AudioError::InvalidFormat("invalid Ogg-FLAC final granule"));
            }
            writer.write_packet_owned(packet.bytes, granule)?;
        }
        writer.finish()?;
        drop(writer);
        link_index += 1;
        let Some(next) = packets.next_stream()? else {
            return Ok(());
        };
        packets = next;
    }
}

fn normalize_initial_audio<R: Read>(
    packets: &mut OggPacketReader<R>,
    first: OggPacket,
    has_granule: bool,
) -> Result<Vec<(OggPacket, u64)>, AudioError> {
    if has_granule {
        let granule = first.page_granule_position;
        let decoded_samples = frame_block_size(&first.bytes)? as u64;
        if granule.checked_add(1).is_none_or(|end| end < decoded_samples)
            && first.page_flags & PAGE_FLAG_EOS == 0
        {
            return Err(AudioError::InvalidFormat(
                "invalid Ogg-FLAC initial audio granule",
            ));
        }
        return Ok(vec![(first, granule)]);
    }

    let page_sequence = first.page_sequence;
    let mut initial = vec![first];
    let granule = loop {
        let (packet, has_granule) =
            packets
                .next_packet_with_granule()?
                .ok_or(AudioError::InvalidFormat(
                    "Ogg-FLAC initial audio page has no completed-packet granule",
                ))?;
        if packet.page_sequence != page_sequence {
            return Err(AudioError::InvalidFormat(
                "Ogg-FLAC initial audio page has no completed-packet granule",
            ));
        }
        validate_audio_packet(&packet)?;
        let granule = packet.page_granule_position;
        initial.push(packet);
        if has_granule {
            break granule;
        }
    };
    let sizes = initial
        .iter()
        .map(|packet| frame_block_size(&packet.bytes).map(|size| size as u64))
        .collect::<Result<Vec<_>, _>>()?;
    let total = sizes
        .iter()
        .try_fold(0_u64, |total, size| total.checked_add(*size))
        .ok_or(AudioError::InvalidData(
            "Ogg-FLAC initial sample count overflow",
        ))?;
    let preceding = total - sizes[sizes.len() - 1];
    let granule_end = granule.checked_add(1).ok_or(AudioError::InvalidFormat(
        "invalid Ogg-FLAC initial audio granule",
    ))?;
    if granule_end < preceding
        || (granule_end < total && initial.last().unwrap().page_flags & PAGE_FLAG_EOS == 0)
    {
        return Err(AudioError::InvalidFormat(
            "invalid Ogg-FLAC initial audio granule",
        ));
    }
    let offset = granule_end.saturating_sub(total);
    let count = initial.len();
    let mut decoded = 0_u64;
    let packets = initial
        .into_iter()
        .zip(sizes)
        .enumerate()
        .map(|(index, (packet, size))| {
            decoded += size;
            let position = if index + 1 == count {
                granule
            } else {
                offset + decoded - 1
            };
            (packet, position)
        })
        .collect();
    Ok(packets)
}

fn read_legacy_headers<R: Read>(
    packets: &mut OggPacketReader<R>,
    mapping: &OggPacket,
) -> Result<(Vec<u8>, Vec<OggPacket>, OggPacket, bool), AudioError> {
    if !mapping.page_is_first
        || !mapping.ends_page
        || mapping.page_flags != ogg::PAGE_FLAG_BOS
        || mapping.page_granule_position != 0
    {
        return Err(AudioError::InvalidFormat(
            "invalid legacy Ogg-FLAC identification packet",
        ));
    }

    let (streaminfo, _) = packets
        .next_packet_with_granule()?
        .ok_or(AudioError::InvalidFormat(
            "legacy Ogg-FLAC stream is missing STREAMINFO",
        ))?;
    if !is_metadata_packet(&streaminfo.bytes)
        || streaminfo.bytes[0] & 0x7f != 0
        || streaminfo.bytes.len() != 38
        || streaminfo.bytes[0] & 0x80 != 0
    {
        return Err(AudioError::InvalidFormat(
            "legacy Ogg-FLAC must begin with STREAMINFO followed by metadata",
        ));
    }
    let mut total = 0;
    add_header_size(&mut total, streaminfo.bytes.len())?;

    let mut mapping = Vec::from(&b"\x7fFLAC\x01\x00"[..]);
    let mut headers = Vec::new();
    let mut metadata_finished = false;
    loop {
        let (packet, has_granule) =
            packets
                .next_packet_with_granule()?
                .ok_or(AudioError::InvalidFormat(
                    "legacy Ogg-FLAC stream has no audio packet",
                ))?;
        if is_metadata_packet(&packet.bytes) {
            if metadata_finished {
                return Err(AudioError::InvalidFormat(
                    "legacy Ogg-FLAC metadata follows the last metadata block",
                ));
            }
            if headers.is_empty() && packet.bytes[0] & 0x7f != 4 {
                return Err(AudioError::InvalidFormat(
                    "first legacy Ogg-FLAC metadata packet must be a Vorbis comment",
                ));
            }
            if headers.len() == usize::from(u16::MAX) {
                return Err(AudioError::InvalidData(
                    "too many legacy Ogg-FLAC metadata packets",
                ));
            }
            add_header_size(&mut total, packet.bytes.len())?;
            metadata_finished = packet.bytes[0] & 0x80 != 0;
            headers.push(packet);
            continue;
        }
        if headers.is_empty() || !metadata_finished {
            return Err(AudioError::InvalidFormat(
                "legacy Ogg-FLAC metadata is missing its last-block marker",
            ));
        }
        validate_audio_packet(&packet)?;
        let count = u16::try_from(headers.len())
            .map_err(|_| AudioError::InvalidData("too many legacy Ogg-FLAC metadata packets"))?;
        mapping.extend_from_slice(&count.to_be_bytes());
        mapping.extend_from_slice(b"fLaC");
        mapping.extend_from_slice(&streaminfo.bytes);
        return Ok((mapping, headers, packet, has_granule));
    }
}

fn validate_mapping(packet: &OggPacket) -> Result<(), AudioError> {
    if !packet.page_is_first
        || !packet.ends_page
        || packet.page_flags != ogg::PAGE_FLAG_BOS
        || packet.page_granule_position != 0
        || packet.page_segment_count != 1
        || packet.bytes.len() != 51
        || &packet.bytes[..5] != b"\x7fFLAC"
        || packet.bytes[5] != 1
        || &packet.bytes[9..13] != b"fLaC"
        || packet.bytes[13] & 0x7f != 0
        || packet.bytes[14..17] != [0, 0, 34]
    {
        return Err(AudioError::InvalidFormat("invalid Ogg-FLAC mapping header"));
    }
    if u16::from_be_bytes([packet.bytes[7], packet.bytes[8]]) != 0 && packet.bytes[13] & 0x80 != 0 {
        return Err(AudioError::InvalidFormat(
            "Ogg-FLAC STREAMINFO marked last before metadata",
        ));
    }
    Ok(())
}

fn read_headers<R: Read>(
    packets: &mut OggPacketReader<R>,
    declared: u16,
) -> Result<(Vec<OggPacket>, OggPacket, bool), AudioError> {
    let mut headers = Vec::new();
    let mut total = 0_usize;
    if declared != 0 {
        for index in 0..declared {
            let packet = packets
                .next_packet()?
                .ok_or(AudioError::InvalidFormat("truncated Ogg-FLAC headers"))?;
            validate_header(&packet)?;
            let last = usize::from(index + 1 == declared);
            if (packet.bytes[0] & 0x80 != 0) != (last != 0)
                || (index == 0 && packet.bytes[0] & 0x7f != 4)
            {
                return Err(AudioError::InvalidFormat("invalid Ogg-FLAC metadata order"));
            }
            add_header_size(&mut total, packet.bytes.len())?;
            headers.push(packet);
        }
        if !headers.last().is_some_and(|packet| packet.ends_page) {
            return Err(AudioError::InvalidFormat(
                "last Ogg-FLAC header packet must end its page",
            ));
        }
        let audio = packets
            .next_packet_with_granule()?
            .ok_or(AudioError::InvalidFormat(
                "Ogg-FLAC stream has no audio packet",
            ))?;
        validate_first_audio(&audio.0)?;
        Ok((headers, audio.0, audio.1))
    } else {
        loop {
            let (packet, has_granule) = packets
                .next_packet_with_granule()?
                .ok_or(AudioError::InvalidFormat("truncated Ogg-FLAC headers"))?;
            if is_metadata_packet(&packet.bytes) {
                validate_header(&packet)?;
                if headers.len() == usize::from(u16::MAX) {
                    return Err(AudioError::InvalidData(
                        "too many Ogg-FLAC metadata packets",
                    ));
                }
                if headers.is_empty() && packet.bytes[0] & 0x7f != 4 {
                    return Err(AudioError::InvalidFormat(
                        "first Ogg-FLAC metadata packet must be a Vorbis comment",
                    ));
                }
                add_header_size(&mut total, packet.bytes.len())?;
                headers.push(packet);
            } else {
                validate_first_audio(&packet)?;
                let Some(last) = headers.last() else {
                    return Err(AudioError::InvalidFormat(
                        "Ogg-FLAC stream is missing its Vorbis comment packet",
                    ));
                };
                if !last.ends_page || last.bytes[0] & 0x80 == 0 {
                    return Err(AudioError::InvalidFormat("invalid final Ogg-FLAC metadata"));
                }
                for packet in &headers[..headers.len() - 1] {
                    if packet.bytes[0] & 0x80 != 0 {
                        return Err(AudioError::InvalidFormat("invalid Ogg-FLAC metadata order"));
                    }
                }
                return Ok((headers, packet, has_granule));
            }
        }
    }
}

fn add_header_size(total: &mut usize, size: usize) -> Result<(), AudioError> {
    *total = total
        .checked_add(size)
        .filter(|&size| size <= MAX_METADATA_BYTES + 4)
        .ok_or(AudioError::InvalidData(
            "FLAC metadata exceeds the 128 MiB limit",
        ))?;
    Ok(())
}

fn validate_header(packet: &OggPacket) -> Result<(), AudioError> {
    if packet.page_granule_position != 0 || !is_metadata_packet(&packet.bytes) {
        return Err(AudioError::InvalidFormat(
            "invalid Ogg-FLAC metadata packet",
        ));
    }
    Ok(())
}

fn is_metadata_packet(bytes: &[u8]) -> bool {
    bytes.len() >= 4
        && bytes[0] & 0x7f != 127
        && (((usize::from(bytes[1]) << 16) | (usize::from(bytes[2]) << 8) | usize::from(bytes[3]))
            == bytes.len() - 4)
}

fn validate_first_audio(packet: &OggPacket) -> Result<(), AudioError> {
    if !packet.starts_page {
        return Err(AudioError::InvalidFormat(
            "first Ogg-FLAC audio packet must start an audio page",
        ));
    }
    validate_audio_packet(packet)
}

fn validate_audio_packet(packet: &OggPacket) -> Result<(), AudioError> {
    if packet.bytes.first() != Some(&0xff) || is_metadata_packet(&packet.bytes) {
        return Err(AudioError::InvalidFormat("invalid Ogg-FLAC audio packet"));
    }
    Ok(())
}

fn rewrite_comment(headers: &mut [OggPacket], metadata: &Metadata) -> Result<(), AudioError> {
    let Some(comment) = headers.first_mut() else {
        return Err(AudioError::InvalidFormat(
            "Ogg-FLAC stream is missing its Vorbis comment packet",
        ));
    };
    if comment.bytes[0] & 0x7f != 4 {
        return Err(AudioError::InvalidFormat(
            "first Ogg-FLAC metadata packet must be a Vorbis comment",
        ));
    }
    let vendor = read_vendor(&comment.bytes[4..])?;
    let encoded = encode_comments(metadata, vendor)?;
    let mut packet = Vec::with_capacity(encoded.len() + 4);
    write_block(
        &mut packet,
        comment.bytes[0] & 0x7f,
        &encoded,
        comment.bytes[0] & 0x80 != 0,
    )?;
    comment.bytes = packet;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ogg::{NO_GRANULE_POSITION, OggPacketReader, OggPacketWriter};
    use zemer_core::{
        AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples,
    };

    use crate::{OggFlacDecoder, OggFlacEncoder};

    use super::rewrite_ogg_metadata;

    fn encode(values: Vec<i16>, title: &str) -> Vec<u8> {
        let spec = AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap();
        let block = PcmBuffer::new(spec, Samples::I16(values)).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", title);
        let mut encoder = OggFlacEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.set_metadata(&metadata).unwrap();
        encoder.write_block(&block).unwrap();
        encoder.finish().unwrap();
        encoder.into_inner().into_inner()
    }

    fn audio_packets(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut reader = OggPacketReader::new(Cursor::new(bytes));
        let mut packets = Vec::new();
        while let Some(packet) = reader.next_packet().unwrap() {
            if packet.bytes.first() == Some(&0xff) {
                packets.push(packet.bytes);
            }
        }
        packets
    }

    fn assert_audio_packets_have_granules(bytes: &[u8]) {
        let mut reader = OggPacketReader::new(Cursor::new(bytes));
        while let Some((packet, owns_granule)) = reader.next_packet_with_granule().unwrap() {
            if packet.bytes.first() == Some(&0xff) {
                assert!(owns_granule, "each repaginated FLAC packet needs a granule");
            }
        }
    }

    fn to_legacy(input: &[u8]) -> Vec<u8> {
        let mut reader = OggPacketReader::new(Cursor::new(input));
        let mapping = reader.next_packet().unwrap().unwrap();
        let serial = reader.stream_serial().unwrap();
        let header_count = u16::from_be_bytes([mapping.bytes[7], mapping.bytes[8]]);
        let mut writer = OggPacketWriter::new(Vec::new(), serial);
        writer.write_packet(b"fLaC", 0).unwrap();
        writer.write_packet(&mapping.bytes[13..], 0).unwrap();
        for _ in 0..header_count {
            let packet = reader.next_packet().unwrap().unwrap();
            writer.write_packet(&packet.bytes, 0).unwrap();
        }
        while let Some((packet, has_granule)) = reader.next_packet_with_granule().unwrap() {
            let granule = if has_granule {
                packet.page_granule_position
            } else {
                NO_GRANULE_POSITION
            };
            writer.write_packet(&packet.bytes, granule).unwrap();
        }
        writer.finish().unwrap();
        writer.into_inner()
    }

    #[test]
    fn rewrites_ogg_comments_and_preserves_audio_packets_and_pcm() {
        let input = encode(
            (0..5000)
                .map(|index| (index as i16).wrapping_mul(23))
                .collect(),
            "old",
        );
        let original_audio = audio_packets(&input);
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "new");
        metadata.insert("ARTIST", "artist");
        let mut output = Vec::new();
        rewrite_ogg_metadata(Cursor::new(&input), &mut output, &metadata).unwrap();

        assert_eq!(audio_packets(&output), original_audio);
        assert_audio_packets_have_granules(&output);
        let mut decoder = OggFlacDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoder.metadata().values("TITLE"), ["new"]);
        assert_eq!(decoder.metadata().values("ARTIST"), ["artist"]);
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(333).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("unexpected Ogg-FLAC sample format");
            };
            decoded.extend_from_slice(samples);
        }
        let expected: Vec<_> = (0..5000)
            .map(|index| (index as i16).wrapping_mul(23))
            .collect();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn rewrites_legacy_ogg_comments_and_normalizes_the_mapping() {
        let samples: Vec<_> = (0..5000)
            .map(|index| (index as i16).wrapping_mul(23))
            .collect();
        let legacy = to_legacy(&encode(samples.clone(), "old"));
        let original_audio = audio_packets(&legacy);
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "updated legacy");
        let mut output = Vec::new();
        rewrite_ogg_metadata(Cursor::new(legacy), &mut output, &metadata).unwrap();

        let mut reader = OggPacketReader::new(Cursor::new(&output));
        assert!(
            reader
                .next_packet()
                .unwrap()
                .unwrap()
                .bytes
                .starts_with(b"\x7fFLAC")
        );
        assert_eq!(audio_packets(&output), original_audio);
        assert_audio_packets_have_granules(&output);
        let mut decoder = OggFlacDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoder.metadata().values("TITLE"), ["updated legacy"]);
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(333).unwrap() {
            let Samples::I16(block) = block.samples() else {
                panic!("unexpected Ogg-FLAC sample format");
            };
            decoded.extend_from_slice(block);
        }
        assert_eq!(decoded, samples);
    }

    #[test]
    fn chained_links_after_first_keep_their_comments_and_audio() {
        let mut input = encode(vec![1; 16], "first");
        let second = encode(vec![2; 16], "second");
        input.extend_from_slice(&second);
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "updated first");
        let mut output = Vec::new();
        rewrite_ogg_metadata(Cursor::new(&input), &mut output, &metadata).unwrap();

        assert_eq!(audio_packets(&output), audio_packets(&input));
        let mut decoder = OggFlacDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoder.metadata().values("TITLE"), ["updated first"]);
        let mut values = Vec::new();
        while let Some(block) = decoder.read_frames(32).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("unexpected Ogg-FLAC sample format");
            };
            values.extend_from_slice(samples);
        }
        assert_eq!(values, [vec![1; 16], vec![2; 16]].concat());
    }
}
