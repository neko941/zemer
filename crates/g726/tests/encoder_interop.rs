use std::{env, fs, path::PathBuf};

use g726::{G726Encoder, G726Packing, G726Rate};

#[test]
#[ignore = "requires external linear PCM and SpanDSP codeword oracles"]
fn encoder_matches_spandsp_linear_pcm_oracles_at_all_rates() {
    let input = env::var_os("ZEMER_G726_ENCODER_PCM").expect("set the external PCM input path");
    let oracle_dir = PathBuf::from(
        env::var_os("ZEMER_G726_ENCODER_ORACLE_DIR").expect("set the oracle directory"),
    );
    let bytes = fs::read(input).unwrap();
    assert_eq!(bytes.len() % 2, 0);
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
        .collect();

    for (rate, label) in [
        (G726Rate::Kbps16, "16000"),
        (G726Rate::Kbps24, "24000"),
        (G726Rate::Kbps32, "32000"),
        (G726Rate::Kbps40, "40000"),
    ] {
        let expected = fs::read(oracle_dir.join(format!("{label}.raw"))).unwrap();
        let mut encoder = G726Encoder::new(rate, 1, G726Packing::LsbFirst).unwrap();
        let mut actual = Vec::new();
        for block in samples.chunks(37) {
            actual.extend(encoder.encode(block).unwrap());
        }
        actual.extend(encoder.finish());
        if actual != expected {
            let width = rate.bits_per_sample();
            let mismatch = actual
                .iter()
                .zip(&expected)
                .position(|(actual, expected)| actual != expected)
                .unwrap_or(actual.len().min(expected.len()));
            let code = |bytes: &[u8], index: usize| {
                let offset = index * usize::from(width);
                (0..width).fold(0_u8, |value, bit| {
                    let position = offset + usize::from(bit);
                    value | (((bytes[position / 8] >> (position % 8)) & 1) << bit)
                })
            };
            let code_mismatch = (0..samples.len())
                .find(|&index| code(&actual, index) != code(&expected, index))
                .unwrap_or(samples.len());
            panic!(
                "{label} bit/s differs at byte {mismatch}: actual={:02x}, expected={:02x}; first code difference at sample {code_mismatch} (PCM {}): actual={}, expected={}; codes around divergence: actual={:?}, expected={:?}",
                actual.get(mismatch).copied().unwrap_or_default(),
                expected.get(mismatch).copied().unwrap_or_default(),
                samples.get(code_mismatch).copied().unwrap_or_default(),
                code(&actual, code_mismatch),
                code(&expected, code_mismatch),
                (code_mismatch.saturating_sub(3)..code_mismatch + 4)
                    .map(|index| code(&actual, index))
                    .collect::<Vec<_>>(),
                (code_mismatch.saturating_sub(3)..code_mismatch + 4)
                    .map(|index| code(&expected, index))
                    .collect::<Vec<_>>()
            );
        }
    }
}
