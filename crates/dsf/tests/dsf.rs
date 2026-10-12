use std::io::Cursor;

use dsf::DsfDecoder;
use zemer_core::{DsdDecoder, DsdSpec};

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

    let mut bytes = dsf(1, 1, 8, &[vec![0x80]]);
    bytes[28 + 32..28 + 36].copy_from_slice(&8_u32.to_le_bytes());
    assert!(DsfDecoder::new(Cursor::new(bytes)).is_err());
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
