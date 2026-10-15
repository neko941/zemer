use g726::{G726Decoder, G726Rate};

fn packed_pattern(width: u8) -> Vec<u8> {
    let sample_count = 4096;
    let mut packed = vec![0; sample_count * usize::from(width) / 8];
    for sample in 0..sample_count {
        let code = (((sample * 73) ^ (sample >> 3) ^ 0x5a) & ((1 << width) - 1)) as u8;
        for bit in 0..width {
            if code & (1 << bit) != 0 {
                let offset = sample * usize::from(width) + usize::from(bit);
                packed[offset / 8] |= 1 << (offset % 8);
            }
        }
    }
    packed
}

fn pcm_digest(samples: &[i16]) -> u64 {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .fold(14_695_981_039_346_656_037, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
        })
}

#[test]
fn matches_annex_a_pcm_from_g191_reconstructed_signal() {
    // Golden digests use G.191 reconstructed samples with the Annex A 14-bit limiter applied.
    for (rate, width, expected_digest) in [
        (G726Rate::Kbps24, 3, 0xf877_0233_47ce_39e4),
        (G726Rate::Kbps40, 5, 0xc39a_f090_e8f3_622f),
    ] {
        let mut decoder = G726Decoder::new(rate, 1).unwrap();
        let decoded = decoder
            .decode_packed_lsb(&packed_pattern(width), 4096)
            .unwrap();
        assert_eq!(decoded.len(), 4096);
        assert_eq!(pcm_digest(&decoded), expected_digest, "rate {rate:?}");
    }
}
