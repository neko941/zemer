use std::{fs::File, io::Cursor};

use dsf::{DsfDecoder, DsfEncoder, rewrite_metadata};
use zemer_core::{DsdBuffer, DsdDecoder, DsdEncoder, DsdSpec, Metadata, Picture};

fn dsf(channels: u32, channel_type: u32, sample_count: u64, data: &[Vec<u8>]) -> Vec<u8> {
    assert_eq!(channels as usize, data.len());
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"DSD ");
    bytes.extend_from_slice(&28_u64.to_le_bytes());
    let blocks = sample_count.div_ceil(4096 * 8);
    let data_size = 12 + u64::from(channels) * blocks * 4096;
    let file_size = 28 + 52 + data_size;
    bytes.extend_from_slice(&file_size.to_le_bytes());
    bytes.extend_from_slice(&0_u64.to_le_bytes());
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&52_u64.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&channel_type.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&2_822_400_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&sample_count.to_le_bytes());
    bytes.extend_from_slice(&4096_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    for block in 0..blocks as usize {
        for channel in data {
            let start = block * 4096;
            let content = channel.get(start..).unwrap_or_default();
            let count = content.len().min(4096);
            bytes.extend_from_slice(&content[..count]);
            bytes.resize(bytes.len() + (4096 - count), 0);
        }
    }
    bytes
}

fn id3_frame(id: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = payload.len();
    let mut frame = id.to_vec();
    frame.extend_from_slice(&[
        ((size >> 21) & 0x7f) as u8,
        ((size >> 14) & 0x7f) as u8,
        ((size >> 7) & 0x7f) as u8,
        (size & 0x7f) as u8,
        0,
        0,
    ]);
    frame.extend_from_slice(payload);
    frame
}

fn with_id3(mut bytes: Vec<u8>) -> Vec<u8> {
    let offset = bytes.len() as u64;
    let mut body = id3_frame(b"TIT2", &[3, b'T', b'i', b't', b'l', b'e']);
    body.extend(id3_frame(b"TPE1", &[3, b'A', b'r', b't', b'i', b's', b't']));
    body.extend(id3_frame(
        b"APIC",
        &[
            3, b'i', b'm', b'a', b'g', b'e', b'/', b'p', b'n', b'g', 0, 3, b'f', b'r', b'o', b'n',
            b't', 0, 0x89, b'P', b'N', b'G',
        ],
    ));
    body.extend(id3_frame(b"PRIV", b"opaque frame payload"));
    let size = body.len();
    let mut tag = b"ID3\x04\x00\x00".to_vec();
    tag.extend_from_slice(&[
        ((size >> 21) & 0x7f) as u8,
        ((size >> 14) & 0x7f) as u8,
        ((size >> 7) & 0x7f) as u8,
        (size & 0x7f) as u8,
    ]);
    tag.extend_from_slice(&body);
    bytes[12..20].copy_from_slice(&((offset + tag.len() as u64).to_le_bytes()));
    bytes[20..28].copy_from_slice(&offset.to_le_bytes());
    bytes.extend(tag);
    bytes
}

#[test]
fn decodes_dsf_channel_blocks_into_msb_first_interleaved_octets() {
    let bytes = dsf(2, 2, 16, &[vec![0x01, 0x02], vec![0x80, 0x40]]);
    let mut decoder = DsfDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(
        decoder.dsd_spec(),
        DsdSpec::new_with_speaker_mask(2_822_400, 2, Some(3)).unwrap()
    );
    assert_eq!(decoder.sample_count(), 16);
    let first = decoder.read_dsd_octets(1).unwrap().unwrap();
    assert_eq!(first.bytes(), [0x80, 0x01]);
    let second = decoder.read_dsd_octets(8).unwrap().unwrap();
    assert_eq!(second.bytes(), [0x40, 0x02]);
    assert!(decoder.read_dsd_octets(1).unwrap().is_none());
}

#[test]
fn reports_and_decodes_a_partial_final_octet() {
    let bytes = dsf(1, 1, 9, &[vec![0x01, 0x01]]);
    let mut decoder = DsfDecoder::new(Cursor::new(bytes)).unwrap();
    let decoded = decoder.read_dsd_octets(16).unwrap().unwrap();
    assert_eq!(decoded.bytes(), [0x80, 0x80]);
    assert_eq!(decoder.sample_count(), 9);
    assert!(decoder.read_dsd_octets(1).unwrap().is_none());
}

#[test]
fn reads_id3v24_text_metadata_from_the_dsf_metadata_pointer() {
    let bytes = with_id3(dsf(1, 1, 8, &[vec![0x55]]));
    let tag = bytes[28 + 52 + 12 + 4096..].to_vec();
    let decoder = DsfDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["Title"]);
    assert_eq!(decoder.metadata().values("ARTIST"), ["Artist"]);
    assert_eq!(decoder.metadata().pictures()[0].description(), "front");
    assert_eq!(decoder.id3_tag(), Some(tag.as_slice()));
}

#[test]
fn rejects_a_gap_before_the_metadata_chunk() {
    let mut bytes = with_id3(dsf(1, 1, 8, &[vec![0x55]]));
    let data_end = 80 + u64::from_le_bytes(bytes[84..92].try_into().unwrap());
    bytes.insert(data_end as usize, 0);
    let file_size = u64::from_le_bytes(bytes[12..20].try_into().unwrap()) + 1;
    let metadata_offset = u64::from_le_bytes(bytes[20..28].try_into().unwrap()) + 1;
    bytes[12..20].copy_from_slice(&file_size.to_le_bytes());
    bytes[20..28].copy_from_slice(&metadata_offset.to_le_bytes());

    assert!(DsfDecoder::new(Cursor::new(bytes)).is_err());
}

#[test]
fn copies_raw_id3_artwork_and_unknown_frames_without_reencoding() {
    let spec = DsdSpec::new(2_822_400, 1).unwrap();
    let original = with_id3(dsf(1, 1, 8, &[vec![0x55]]));
    let tag = original[28 + 52 + 12 + 4096..].to_vec();
    let mut encoder = DsfEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_id3_tag(&tag).unwrap();
    encoder
        .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55]).unwrap())
        .unwrap();
    encoder.finish().unwrap();
    let encoded = encoder.into_inner().into_inner();
    assert_eq!(&encoded[28 + 52 + 12 + 4096..], tag);
    let decoder = DsfDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(decoder.id3_tag(), Some(tag.as_slice()));
    assert_eq!(decoder.metadata().values("TITLE"), ["Title"]);
}

#[test]
fn rewrites_text_metadata_without_changing_audio_or_opaque_id3_frames() {
    let original = with_id3(dsf(1, 1, 8, &[vec![0x55]]));
    let audio_end = 28 + 52 + 12 + 4096;
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "Edited title");
    metadata.insert("ALBUM", "New album");
    metadata.add_picture(Picture::new(
        3,
        "image/png",
        "front",
        0,
        0,
        0,
        0,
        [0x89, b'P', b'N', b'G'],
    ));
    let mut output = Vec::new();
    rewrite_metadata(Cursor::new(&original), &mut output, &metadata).unwrap();

    assert_eq!(&output[92..audio_end], &original[92..audio_end]);
    assert!(
        output
            .windows(b"opaque frame payload".len())
            .any(|window| window == b"opaque frame payload")
    );
    assert!(output.windows(4).any(|window| window == b"APIC"));
    let decoder = DsfDecoder::new(Cursor::new(output)).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["Edited title"]);
    assert_eq!(decoder.metadata().values("ALBUM"), ["New album"]);
    assert_eq!(decoder.metadata().pictures(), metadata.pictures());
    assert!(decoder.metadata().values("ARTIST").is_empty());
}

#[test]
fn encoder_edits_id3_picture_and_keeps_opaque_frames() {
    let spec = DsdSpec::new(2_822_400, 1).unwrap();
    let original = with_id3(dsf(1, 1, 8, &[vec![0x55]]));
    let tag = original[28 + 52 + 12 + 4096..].to_vec();
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "Updated");
    metadata.add_picture(Picture::new(
        4,
        "image/jpeg",
        "back",
        0,
        0,
        0,
        0,
        [0xff, 0xd8],
    ));
    let mut encoder = DsfEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_id3_tag(&tag).unwrap();
    encoder.set_metadata(&metadata).unwrap();
    encoder
        .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55]).unwrap())
        .unwrap();
    encoder.finish().unwrap();
    let encoded = encoder.into_inner().into_inner();
    assert!(
        encoded
            .windows(b"opaque frame payload".len())
            .any(|bytes| bytes == b"opaque frame payload")
    );
    let decoder = DsfDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(decoder.metadata(), &metadata);
}

#[test]
fn reads_channel_blocks_in_order_across_dsf_block_boundaries() {
    let mut left = vec![0; 4096];
    let mut right = vec![0; 4096];
    left[4095] = 0x01;
    right[4095] = 0x80;
    left.push(0x01);
    right.push(0x01);
    let bytes = dsf(2, 2, 4096 * 8 + 1, &[left, right]);
    let mut decoder = DsfDecoder::new(Cursor::new(bytes)).unwrap();
    let first = decoder.read_dsd_octets(4096).unwrap().unwrap();
    assert_eq!(first.frames(), 4096);
    assert_eq!(&first.bytes()[first.bytes().len() - 2..], [0x80, 0x01]);
    assert_eq!(
        decoder.read_dsd_octets(1).unwrap().unwrap().bytes(),
        [0x80, 0x80]
    );
    assert!(decoder.read_dsd_octets(1).unwrap().is_none());
}

#[test]
fn rejects_nonzero_dsf_padding_and_unsupported_sample_packing() {
    let mut bytes = dsf(1, 1, 1, &[vec![0x01]]);
    let mut decoder = DsfDecoder::new(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(decoder.read_dsd_octets(1).unwrap().unwrap().bytes(), [0x80]);

    // The DSF 1.01 profile requires unused samples in a block to be zero.
    let padding_offset = 28 + 52 + 12 + 1;
    bytes[padding_offset] = 1;
    let mut decoder = DsfDecoder::new(Cursor::new(bytes)).unwrap();
    assert!(decoder.read_dsd_octets(1).is_err());

    let mut bytes = dsf(1, 1, 8, &[vec![0x96]]);
    bytes[28 + 32..28 + 36].copy_from_slice(&8_u32.to_le_bytes());
    let mut decoder = DsfDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.read_dsd_octets(1).unwrap().unwrap().bytes(), [0x96]);
}

#[test]
fn encodes_eight_bit_dsf_storage_without_reversing_dsd_octets() {
    let spec = DsdSpec::new(2_822_400, 1).unwrap();
    let input = DsdBuffer::new(spec, vec![0x96]).unwrap();
    let mut encoder =
        DsfEncoder::new_with_bits_per_sample(Cursor::new(Vec::new()), spec, 8).unwrap();
    encoder.write_dsd_octets(&input).unwrap();
    encoder.finish().unwrap();
    let bytes = encoder.into_inner().into_inner();
    assert_eq!(&bytes[60..64], &8_u32.to_le_bytes());
    assert_eq!(bytes[92], 0x96);
    let mut decoder = DsfDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.read_dsd_octets(1).unwrap().unwrap().bytes(), [0x96]);
}

#[test]
fn rejects_invalid_chunk_bounds_and_zero_read_size() {
    let bytes = dsf(1, 1, 8, &[vec![0x01]]);
    let mut decoder = DsfDecoder::new(Cursor::new(bytes.clone())).unwrap();
    assert!(decoder.read_dsd_octets(0).is_err());
    assert!(DsfDecoder::new(Cursor::new(bytes[..90].to_vec())).is_err());

    let mut bytes = bytes;
    bytes[4..12].copy_from_slice(&27_u64.to_le_bytes());
    assert!(DsfDecoder::new(Cursor::new(bytes)).is_err());
}

#[test]
fn encodes_and_decodes_channel_blocks_and_id3_text() {
    let spec = DsdSpec::new_with_speaker_mask(2_822_400, 2, Some(3)).unwrap();
    let expected: Vec<u8> = (0..4097 * 2).map(|sample| (sample * 37) as u8).collect();
    let input = DsdBuffer::new(spec, expected.clone()).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "DSF round trip");
    metadata.insert("ARTIST", "One");
    metadata.insert("ARTIST", "Two");
    metadata.insert("REPLAYGAIN_GAIN", "-3 dB");

    let mut encoder = DsfEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_metadata(&metadata).unwrap();
    encoder.write_dsd_octets(&input).unwrap();
    encoder.finish().unwrap();
    let bytes = encoder.into_inner().into_inner();

    let blocks_per_channel = 2_usize;
    let data_end = 92 + 2 * 4096 * blocks_per_channel;
    assert_eq!(&bytes[..4], b"DSD ");
    assert_eq!(
        u64::from_le_bytes(bytes[12..20].try_into().unwrap()),
        bytes.len() as u64
    );
    assert_eq!(
        u64::from_le_bytes(bytes[20..28].try_into().unwrap()),
        data_end as u64
    );
    assert_eq!(
        u64::from_le_bytes(bytes[64..72].try_into().unwrap()),
        4097 * 8
    );
    assert_eq!(
        u64::from_le_bytes(bytes[84..92].try_into().unwrap()),
        (12 + 2 * 4096 * blocks_per_channel) as u64
    );
    for channel in 0..2 {
        let mut expected_block = vec![0; 4096];
        for frame in 0..4097 {
            expected_block[frame % 4096] = expected[frame * 2 + channel].reverse_bits();
            if frame == 4095 {
                let start = 92 + (channel * 4096);
                assert_eq!(&bytes[start..start + 4096], expected_block);
                expected_block.fill(0);
            }
        }
        let start = 92 + (2 * 4096) + (channel * 4096);
        assert_eq!(&bytes[start..start + 4096], expected_block);
    }

    let mut decoder = DsfDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.sample_count(), 4097 * 8);
    assert_eq!(decoder.metadata(), &metadata);
    let first = decoder.read_dsd_octets(8192).unwrap().unwrap();
    let second = decoder.read_dsd_octets(8192).unwrap().unwrap();
    assert_eq!(first.frames(), 4096);
    assert_eq!(second.frames(), 1);
    assert_eq!([first.bytes(), second.bytes()].concat(), expected);
    assert!(decoder.read_dsd_octets(1).unwrap().is_none());
}

#[test]
fn encodes_every_declared_dsf_channel_layout() {
    for (channels, mask) in [
        (1, None),
        (2, Some(0x3)),
        (3, Some(0x7)),
        (4, Some(0x33)),
        (4, Some(0x0f)),
        (5, Some(0x37)),
        (6, Some(0x3f)),
    ] {
        let spec = DsdSpec::new_with_speaker_mask(2_822_400, channels, mask).unwrap();
        let input = DsdBuffer::new(spec, vec![0x96; usize::from(channels)]).unwrap();
        let mut encoder = DsfEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_dsd_octets(&input).unwrap();
        encoder.finish().unwrap();
        let bytes = encoder.into_inner().into_inner();
        let decoder = DsfDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.dsd_spec(), spec);
    }
}

#[test]
fn preserves_partial_final_sample_counts() {
    let spec = DsdSpec::new(2_822_400, 1).unwrap();
    let mut encoder = DsfEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder
        .write_dsd_octets(&DsdBuffer::new(spec, vec![0x80, 0x80]).unwrap())
        .unwrap();
    encoder.set_sample_count(9).unwrap();
    encoder.finish().unwrap();
    let bytes = encoder.into_inner().into_inner();
    let mut decoder = DsfDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.sample_count(), 9);
    assert_eq!(
        decoder.read_dsd_octets(8).unwrap().unwrap().bytes(),
        [0x80, 0x80]
    );

    let mut encoder = DsfEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder
        .write_dsd_octets(&DsdBuffer::new(spec, vec![0x80, 0x81]).unwrap())
        .unwrap();
    assert!(encoder.set_sample_count(9).is_err());
}

#[test]
fn rejects_unsupported_dsf_layouts_and_empty_streams() {
    assert!(
        DsfEncoder::new(
            Cursor::new(Vec::new()),
            DsdSpec::new_with_speaker_mask(2_822_400, 4, Some(0x3f)).unwrap(),
        )
        .is_err()
    );
    let spec = DsdSpec::new(2_822_400, 1).unwrap();
    let mut encoder = DsfEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    assert!(encoder.finish().is_err());
    assert!(
        encoder
            .write_dsd_octets(&DsdBuffer::new(spec, vec![0]).unwrap())
            .is_err()
    );
}

#[test]
#[ignore = "requires the temporary public-domain DSF sample described in docs-internal/REFERENCES.md"]
fn decodes_external_public_domain_dsf_sample() {
    let path = std::env::var_os("ZEMER_DSF_SAMPLE").expect("set ZEMER_DSF_SAMPLE");
    let mut decoder = DsfDecoder::new(File::open(path).unwrap()).unwrap();
    assert_eq!(decoder.dsd_spec().bit_rate(), 2_822_400);
    assert_eq!(decoder.dsd_spec().channels(), 2);
    assert!(decoder.sample_count() > 32_768);
    assert!(decoder.metadata().iter().next().is_some());
    assert_eq!(
        decoder.read_dsd_octets(4096).unwrap().unwrap().frames(),
        4096
    );
}
