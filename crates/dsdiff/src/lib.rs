//! DSDIFF container decoding for raw and DST-compressed DSD, with raw DSD encoding.

mod comment;
mod decode;
mod encode;
mod marker;
mod rewrite;

pub use comment::DsdiffComment;
pub use decode::DsdiffDecoder;
pub use encode::DsdiffEncoder;
pub use marker::DsdiffMarker;
pub use rewrite::rewrite_metadata;

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zemer_core::{DsdBuffer, DsdDecoder, DsdEncoder, DsdSpec, Metadata};

    use crate::{DsdiffComment, DsdiffDecoder, DsdiffEncoder, DsdiffMarker};

    #[test]
    fn raw_dsd_round_trip_preserves_channels_and_text_metadata() {
        let spec = DsdSpec::new(2_822_400, 2).unwrap();
        let audio = DsdBuffer::new(spec, vec![0x80, 0x01, 0x55, 0xaa, 0x7f, 0x20]).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "Title");
        metadata.insert("ARTIST", "Composer");
        metadata.insert("DSDIFF_CHANNEL_ID", "C123");
        metadata.insert("DSDIFF_CHANNEL_ID", "C124");
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.set_metadata(&metadata).unwrap();
        encoder.write_dsd_octets(&audio).unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();

        let mut decoder = DsdiffDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.dsd_spec(), spec);
        assert_eq!(decoder.sample_count(), Some(24));
        assert_eq!(decoder.metadata().values("TITLE"), ["Title"]);
        assert_eq!(decoder.metadata().values("ARTIST"), ["Composer"]);
        assert_eq!(
            decoder.metadata().values("DSDIFF_CHANNEL_ID"),
            ["C123", "C124"]
        );
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_dsd_octets(1).unwrap() {
            decoded.extend_from_slice(block.bytes());
        }
        assert_eq!(decoded, audio.bytes());
    }

    #[test]
    fn raw_dsd_round_trips_more_than_dst_channel_limit() {
        let spec = DsdSpec::new(2_822_400, 65).unwrap();
        assert!(DsdiffEncoder::new_dst(Cursor::new(Vec::new()), spec).is_err());
        let audio = DsdBuffer::new(spec, (0..65).map(|channel| channel as u8).collect()).unwrap();
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_dsd_octets(&audio).unwrap();
        encoder.finish().unwrap();

        let mut decoder =
            DsdiffDecoder::new(Cursor::new(encoder.into_inner().into_inner())).unwrap();
        assert_eq!(decoder.dsd_spec(), spec);
        assert_eq!(decoder.metadata().values("DSDIFF_CHANNEL_ID").len(), 65);
        assert_eq!(
            decoder.read_dsd_octets(1).unwrap().unwrap().bytes(),
            audio.bytes()
        );
        assert!(decoder.read_dsd_octets(1).unwrap().is_none());
    }

    #[test]
    fn raw_dsd_round_trips_the_full_dsdiff_channel_count_field() {
        let spec = DsdSpec::new(2_822_400, u16::MAX).unwrap();
        let audio = DsdBuffer::new(spec, vec![0x5a; usize::from(u16::MAX)]).unwrap();
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_dsd_octets(&audio).unwrap();
        encoder.finish().unwrap();

        let mut decoder =
            DsdiffDecoder::new(Cursor::new(encoder.into_inner().into_inner())).unwrap();
        assert_eq!(decoder.dsd_spec(), spec);
        let ids = decoder.metadata().values("DSDIFF_CHANNEL_ID");
        assert_eq!(ids.len(), 65_535);
        let mut unique_ids = ids.to_vec();
        unique_ids.sort_unstable();
        unique_ids.dedup();
        assert_eq!(unique_ids.len(), ids.len());
        assert_eq!(
            decoder.read_dsd_octets(1).unwrap().unwrap().bytes(),
            audio.bytes()
        );
        assert!(decoder.read_dsd_octets(1).unwrap().is_none());
    }

    #[test]
    fn dst_bypass_round_trip_streams_split_frames() {
        let spec = DsdSpec::new(2_822_400, 2).unwrap();
        let frame_bytes = 4_704;
        let audio = (0..frame_bytes * 2)
            .map(|index| (index as u8).wrapping_mul(37))
            .collect::<Vec<_>>();
        let mut encoder = DsdiffEncoder::new_dst_bypass(Cursor::new(Vec::new()), spec).unwrap();
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, audio[..2_000 * 2].to_vec()).unwrap())
            .unwrap();
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, audio[2_000 * 2..].to_vec()).unwrap())
            .unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();
        assert!(bytes.windows(4).any(|window| window == b"DST "));
        let mut decoder = DsdiffDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.sample_count(), Some((frame_bytes * 8) as u64));
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_dsd_octets(777).unwrap() {
            decoded.extend_from_slice(block.bytes());
        }
        assert_eq!(decoded, audio);
    }

    #[test]
    fn dst_bypass_requires_complete_frames() {
        let spec = DsdSpec::new(2_822_400, 1).unwrap();
        let mut encoder = DsdiffEncoder::new_dst_bypass(Cursor::new(Vec::new()), spec).unwrap();
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55]).unwrap())
            .unwrap();
        assert!(encoder.finish().is_err());
    }

    #[test]
    fn dst_encoder_writes_compressed_frames_when_prediction_saves_space() {
        let spec = DsdSpec::new(2_822_400, 2).unwrap();
        let frame_bytes = 4_704;
        let audio = DsdBuffer::new(spec, vec![0xff; frame_bytes * 2]).unwrap();
        let mut encoder = DsdiffEncoder::new_dst(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_dsd_octets(&audio).unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();
        let frame_header = bytes
            .windows(4)
            .position(|window| window == b"DSTF")
            .unwrap();
        assert_ne!(bytes[frame_header + 12] & 0x80, 0);

        let mut decoder = DsdiffDecoder::new(Cursor::new(bytes)).unwrap();
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_dsd_octets(frame_bytes * 2).unwrap() {
            decoded.extend_from_slice(block.bytes());
        }
        assert_eq!(decoded, audio.bytes());
    }

    #[test]
    fn dst_encoder_falls_back_to_bypass_when_prediction_is_not_usable() {
        let spec = DsdSpec::new(2_822_400, 1).unwrap();
        let frame_bytes = 4_704;
        let audio = DsdBuffer::new(
            spec,
            (0..frame_bytes)
                .map(|index| (index as u8).wrapping_mul(73))
                .collect(),
        )
        .unwrap();
        let mut encoder = DsdiffEncoder::new_dst(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_dsd_octets(&audio).unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();
        let frame_header = bytes
            .windows(4)
            .position(|window| window == b"DSTF")
            .unwrap();
        assert_eq!(bytes[frame_header + 12], 0);

        let mut decoder = DsdiffDecoder::new(Cursor::new(bytes)).unwrap();
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_dsd_octets(frame_bytes).unwrap() {
            decoded.extend_from_slice(block.bytes());
        }
        assert_eq!(decoded, audio.bytes());
    }

    #[test]
    fn dst_bypass_supports_dsd64_128_and_256() {
        for (bit_rate, frame_bytes) in
            [(2_822_400, 4_704), (5_644_800, 9_408), (11_289_600, 18_816)]
        {
            let spec = DsdSpec::new(bit_rate, 1).unwrap();
            let audio = (0..frame_bytes)
                .map(|index| (index as u8).wrapping_mul(73))
                .collect::<Vec<_>>();
            let mut encoder = DsdiffEncoder::new_dst(Cursor::new(Vec::new()), spec).unwrap();
            encoder
                .write_dsd_octets(&DsdBuffer::new(spec, audio.clone()).unwrap())
                .unwrap();
            encoder.finish().unwrap();
            let mut decoder =
                DsdiffDecoder::new(Cursor::new(encoder.into_inner().into_inner())).unwrap();
            let mut decoded = Vec::new();
            while let Some(block) = decoder.read_dsd_octets(frame_bytes).unwrap() {
                decoded.extend_from_slice(block.bytes());
            }
            assert_eq!(decoded, audio);
        }
    }

    #[test]
    fn mono_odd_length_data_uses_an_unplayed_zero_pad_byte() {
        let spec = DsdSpec::new(2_822_400, 1).unwrap();
        let audio = DsdBuffer::new(spec, vec![0xa5]).unwrap();
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_dsd_octets(&audio).unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();
        assert_eq!(bytes.last(), Some(&0));
        let mut decoder = DsdiffDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.read_dsd_octets(8).unwrap().unwrap().bytes(), [0xa5]);
        assert!(decoder.read_dsd_octets(8).unwrap().is_none());
    }

    #[test]
    fn rejects_a_dst_marker_without_dst_sound_data_and_bad_form_boundaries() {
        let spec = DsdSpec::new(2_822_400, 1).unwrap();
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55]).unwrap())
            .unwrap();
        encoder.finish().unwrap();
        let mut bytes = encoder.into_inner().into_inner();
        let compression_id = bytes
            .windows(4)
            .position(|window| window == b"CMPR")
            .unwrap();
        let compression_id = compression_id + 12;
        bytes[compression_id..compression_id + 4].copy_from_slice(b"DST ");
        assert!(DsdiffDecoder::new(Cursor::new(bytes.clone())).is_err());

        bytes[4..12].copy_from_slice(&0_u64.to_be_bytes());
        assert!(DsdiffDecoder::new(Cursor::new(bytes)).is_err());
    }

    #[test]
    fn rejects_mismatched_text_count_and_duplicate_titles() {
        let spec = DsdSpec::new(2_822_400, 1).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "Title");
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.set_metadata(&metadata).unwrap();
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55]).unwrap())
            .unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();

        let mut bad_count = bytes.clone();
        let title = bad_count
            .windows(4)
            .position(|window| window == b"DITI")
            .unwrap();
        bad_count[title + 12..title + 16].copy_from_slice(&99_u32.to_be_bytes());
        assert!(DsdiffDecoder::new(Cursor::new(bad_count)).is_err());

        let mut duplicate = bytes;
        let title_size = u64::from_be_bytes(duplicate[title + 4..title + 12].try_into().unwrap());
        let title_chunk_size = 12 + title_size + (title_size & 1);
        let title_chunk = duplicate[title..title + title_chunk_size as usize].to_vec();
        let diin = duplicate
            .windows(4)
            .position(|window| window == b"DIIN")
            .unwrap();
        let diin_size = u64::from_be_bytes(duplicate[diin + 4..diin + 12].try_into().unwrap());
        let insert_at = diin + 12 + diin_size as usize;
        duplicate.splice(insert_at..insert_at, title_chunk.clone());
        duplicate[diin + 4..diin + 12]
            .copy_from_slice(&(diin_size + title_chunk_size).to_be_bytes());
        let form_size = u64::from_be_bytes(duplicate[4..12].try_into().unwrap());
        duplicate[4..12].copy_from_slice(&(form_size + title_chunk_size).to_be_bytes());
        assert!(DsdiffDecoder::new(Cursor::new(duplicate)).is_err());
    }

    #[test]
    fn round_trips_general_and_typed_comments() {
        let spec = DsdSpec::new(2_822_400, 2).unwrap();
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder
            .set_comments(&[
                DsdiffComment {
                    year: 2024,
                    month: 5,
                    day: 6,
                    hour: 7,
                    minute: 8,
                    kind: 0,
                    reference: 0,
                    text: "Odd".into(),
                },
                DsdiffComment {
                    year: 2025,
                    month: 1,
                    day: 2,
                    hour: 3,
                    minute: 4,
                    kind: 1,
                    reference: 1,
                    text: "Even!".into(),
                },
            ])
            .unwrap();
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55, 0xaa]).unwrap())
            .unwrap();
        encoder.finish().unwrap();

        let decoder = DsdiffDecoder::new(Cursor::new(encoder.into_inner().into_inner())).unwrap();
        assert_eq!(decoder.comments().len(), 2);
        assert_eq!(decoder.comments()[0].text, "Odd");
        assert_eq!(decoder.comments()[1].kind, 1);
        assert_eq!(decoder.comments()[1].reference, 1);
        assert_eq!(decoder.metadata().values("COMMENT"), ["Odd"]);
    }

    #[test]
    fn round_trips_typed_markers_in_file_order() {
        let spec = DsdSpec::new(2_822_400, 2).unwrap();
        let markers = [
            DsdiffMarker {
                hours: 0,
                minutes: 0,
                seconds: 0,
                samples: 100,
                offset: -12,
                kind: 2,
                channel: 0,
                track_flags: 0,
                text: "Program".into(),
            },
            DsdiffMarker {
                hours: 0,
                minutes: 0,
                seconds: 1,
                samples: 200,
                offset: 0,
                kind: 0,
                channel: 1,
                track_flags: 0,
                text: "Track 1".into(),
            },
        ];
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.set_markers(&markers).unwrap();
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55, 0xaa]).unwrap())
            .unwrap();
        encoder.finish().unwrap();
        let decoder = DsdiffDecoder::new(Cursor::new(encoder.into_inner().into_inner())).unwrap();
        assert_eq!(decoder.markers(), markers);
    }

    #[test]
    fn rejects_marker_fields_outside_the_specified_ranges() {
        let spec = DsdSpec::new(2_822_400, 1).unwrap();
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        assert!(
            encoder
                .set_markers(&[DsdiffMarker {
                    hours: 24,
                    minutes: 0,
                    seconds: 0,
                    samples: 0,
                    offset: 0,
                    kind: 0,
                    channel: 0,
                    track_flags: 0,
                    text: "bad".into(),
                }])
                .is_err()
        );
    }

    #[test]
    fn validates_track_flags_against_the_ordered_sacd_channel_layout() {
        let spec = DsdSpec::new_with_speaker_mask(2_822_400, 5, Some(0x37)).unwrap();
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        let marker = |track_flags| DsdiffMarker {
            hours: 0,
            minutes: 0,
            seconds: 0,
            samples: 0,
            offset: 0,
            kind: 0,
            channel: 0,
            track_flags,
            text: String::new(),
        };

        encoder.set_markers(&[marker(0x0002)]).unwrap();
        assert!(encoder.set_markers(&[marker(0x0001)]).is_err());
        assert!(encoder.set_markers(&[marker(0x0100)]).is_err());

        let mut metadata = Metadata::default();
        for id in ["MLFT", "MRGT", "C   ", "LS  ", "RS  "] {
            metadata.insert("DSDIFF_CHANNEL_ID", id);
        }
        encoder.set_markers(&[marker(0x0002)]).unwrap();
        metadata.remove("DSDIFF_CHANNEL_ID");
        for id in ["C001", "C002", "C003", "C004", "C005"] {
            metadata.insert("DSDIFF_CHANNEL_ID", id);
        }
        assert!(encoder.set_metadata(&metadata).is_err());
    }

    #[test]
    fn shared_comment_metadata_encodes_as_general_comments() {
        let spec = DsdSpec::new(2_822_400, 1).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("COMMENT", "First");
        metadata.insert("COMMENT", "Second");
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.set_metadata(&metadata).unwrap();
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55]).unwrap())
            .unwrap();
        encoder.finish().unwrap();
        let mut bytes = encoder.into_inner().into_inner();
        let comt = bytes
            .windows(4)
            .position(|window| window == b"COMT")
            .unwrap();
        let size = u64::from_be_bytes(bytes[comt + 4..comt + 12].try_into().unwrap());
        let end = comt + 12 + size as usize + (size as usize & 1);
        let comt_chunk: Vec<_> = bytes.drain(comt..end).collect();
        bytes.splice(32..32, comt_chunk);
        let decoder = DsdiffDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.metadata().values("COMMENT"), ["First", "Second"]);
    }

    #[test]
    fn rejects_invalid_comment_references_and_record_padding() {
        let spec = DsdSpec::new(2_822_400, 1).unwrap();
        let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        assert!(
            encoder
                .set_comments(&[DsdiffComment {
                    year: 2024,
                    month: 1,
                    day: 1,
                    hour: 0,
                    minute: 0,
                    kind: 0,
                    reference: 1,
                    text: "bad".into(),
                }])
                .is_err()
        );
        assert!(
            encoder
                .set_comments(&[DsdiffComment {
                    year: 2024,
                    month: 13,
                    day: 1,
                    hour: 0,
                    minute: 0,
                    kind: 0,
                    reference: 0,
                    text: "bad".into(),
                }])
                .is_err()
        );

        encoder
            .set_comments(&[DsdiffComment {
                year: 2024,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                kind: 0,
                reference: 0,
                text: "Odd".into(),
            }])
            .unwrap();
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55]).unwrap())
            .unwrap();
        encoder.finish().unwrap();
        let mut bytes = encoder.into_inner().into_inner();
        let comt = bytes
            .windows(4)
            .position(|window| window == b"COMT")
            .unwrap();
        let comment_end = comt + 12 + 2 + 14 + 3;
        bytes[comment_end] = 1;
        assert!(DsdiffDecoder::new(Cursor::new(bytes)).is_err());
    }
}
