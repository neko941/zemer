//! Optional packet-framing regression over the external RFC 8251 `.bit` corpus.
//!
//! Set `ZEMER_OPUS_TESTVECTORS` to the extracted `opus_newvectors` directory,
//! then run `cargo test -p zemer-tests --test opus_rfc8251_vectors -- --ignored`.
//! The corpus stays outside the repository; this test validates packet
//! framing decisions only, not Opus audio decoding.

use std::{env, fs, path::Path};

use opus::OpusPacket;

#[test]
#[ignore = "requires the external RFC 8251 corpus; see the module docs"]
fn rfc8251_packet_framing_matches_baseline() {
    let directory = env::var_os("ZEMER_OPUS_TESTVECTORS")
        .expect("set ZEMER_OPUS_TESTVECTORS to the extracted opus_newvectors directory");
    let directory = Path::new(&directory);
    let mut accepted = 0_usize;
    let mut rejected = 0_usize;

    for vector in 1..=12 {
        let path = directory.join(format!("testvector{vector:02}.bit"));
        let bytes = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let mut cursor = 0_usize;

        while cursor < bytes.len() {
            let header_end = cursor.checked_add(8).expect("vector offset overflow");
            let header = bytes.get(cursor..header_end).unwrap_or_else(|| {
                panic!("{}: truncated packet record at {cursor}", path.display())
            });
            let packet_bytes = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
            let packet_start = header_end;
            let packet_end = packet_start
                .checked_add(packet_bytes)
                .filter(|&end| end <= bytes.len())
                .unwrap_or_else(|| panic!("{}: invalid packet length at {cursor}", path.display()));
            let packet = &bytes[packet_start..packet_end];
            if OpusPacket::parse(packet).is_ok() {
                accepted += 1;
            } else {
                rejected += 1;
            }
            cursor = packet_end;
        }
    }

    assert_eq!(accepted, 16_982, "accepted packet count changed");
    assert_eq!(rejected, 3_093, "rejected packet count changed");
}
