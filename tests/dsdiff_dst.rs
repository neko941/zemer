use std::io::Cursor;

use dsdiff::DsdiffDecoder;
use zemer_core::DsdDecoder;

const CHANNELS: usize = 2;
const FRAME_BYTES: usize = 4_704;

#[test]
fn decodes_multiple_uncompressed_dst_frames_and_checks_optional_crc() {
    let first = vec![0x55; FRAME_BYTES * CHANNELS];
    let second = vec![0xaa; FRAME_BYTES * CHANNELS];
    let file = dsdiff_with_dst(&[first.clone(), second.clone()], true, false);
    let mut decoder = DsdiffDecoder::new(Cursor::new(file)).unwrap();
    assert_eq!(decoder.sample_count(), Some((FRAME_BYTES * 2 * 8) as u64));

    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_dsd_octets(137).unwrap() {
        decoded.extend_from_slice(block.bytes());
    }
    assert_eq!(decoded, [first, second].concat());
}

#[test]
fn rejects_a_dst_crc_mismatch_when_the_frame_is_read() {
    let source = vec![0x96; FRAME_BYTES * CHANNELS];
    let file = dsdiff_with_dst(&[source], true, true);
    let mut decoder = DsdiffDecoder::new(Cursor::new(file)).unwrap();
    assert!(decoder.read_dsd_octets(FRAME_BYTES).is_err());
}

fn dsdiff_with_dst(frames: &[Vec<u8>], crc: bool, corrupt_crc: bool) -> Vec<u8> {
    let mut form = Vec::new();
    push_chunk(&mut form, *b"FVER", &[1, 5, 0, 0]);

    let mut properties = b"SND ".to_vec();
    push_chunk(&mut properties, *b"FS  ", &2_822_400_u32.to_be_bytes());
    let mut channels = vec![0, CHANNELS as u8];
    channels.extend_from_slice(b"SLFTSRGT");
    push_chunk(&mut properties, *b"CHNL", &channels);
    push_chunk(&mut properties, *b"CMPR", b"DST \x03DST");
    push_chunk(&mut form, *b"PROP", &properties);

    let mut dst = Vec::new();
    let mut frame_info = (frames.len() as u32).to_be_bytes().to_vec();
    frame_info.extend_from_slice(&75_u16.to_be_bytes());
    push_chunk(&mut dst, *b"FRTE", &frame_info);
    for (index, source) in frames.iter().enumerate() {
        let mut payload = Vec::with_capacity(source.len() + 1);
        payload.push(0); // raw (processing mode 0) DST frame
        payload.extend_from_slice(source);
        push_chunk(&mut dst, *b"DSTF", &payload);
        if crc {
            let mut expected = reference_crc32(source).to_be_bytes().to_vec();
            if corrupt_crc && index == 0 {
                expected[0] ^= 0x80;
            }
            push_chunk(&mut dst, *b"DSTC", &expected);
        }
    }
    push_chunk(&mut form, *b"DST ", &dst);

    let mut file = b"FRM8".to_vec();
    file.extend_from_slice(&(4_u64 + form.len() as u64).to_be_bytes());
    file.extend_from_slice(b"DSD ");
    file.extend_from_slice(&form);
    file
}

fn push_chunk(output: &mut Vec<u8>, id: [u8; 4], payload: &[u8]) {
    output.extend_from_slice(&id);
    output.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    output.extend_from_slice(payload);
    if payload.len() % 2 != 0 {
        output.push(0);
    }
}

fn reference_crc32(data: &[u8]) -> u32 {
    const POLYNOMIAL: u64 = (1_u64 << 32) | (1_u64 << 31) | (1_u64 << 4) | 1;
    let mut dividend = Vec::with_capacity(data.len() * 8 + 32);
    for byte in data {
        for shift in (0..8).rev() {
            dividend.push((byte >> shift) & 1);
        }
    }
    dividend.extend([0; 32]);
    for start in 0..data.len() * 8 {
        if dividend[start] != 0 {
            for (offset, bit) in (0..=32)
                .map(|shift| ((POLYNOMIAL >> (32 - shift)) & 1) as u8)
                .enumerate()
            {
                dividend[start + offset] ^= bit;
            }
        }
    }
    dividend[data.len() * 8..]
        .iter()
        .fold(0_u32, |value, bit| (value << 1) | u32::from(*bit))
}
