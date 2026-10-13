use std::{
    fs::File,
    io::{BufReader, Read},
};

use aiff::AiffDecoder;
use zemer_core::{AudioDecoder, AudioSpec, SampleFormat, Samples};

const FRAMES: u64 = 23_493;

#[test]
#[ignore = "requires McGill AIFF/AIFC samples and FFmpeg PCM oracles in ZEMER_AIFF_12BIT_* environment variables"]
fn decodes_external_12_bit_aiff_and_aifc_against_pcm_oracles() {
    for (sample_var, oracle_var, aifc) in [
        ("ZEMER_AIFF_12BIT_SAMPLE", "ZEMER_AIFF_12BIT_PCM", false),
        ("ZEMER_AIFC_12BIT_SAMPLE", "ZEMER_AIFC_12BIT_PCM", true),
    ] {
        let sample = std::env::var_os(sample_var).expect("AIFF/AIFC sample path is required");
        let oracle = std::env::var_os(oracle_var).expect("FFmpeg PCM oracle path is required");
        let mut decoder = AiffDecoder::new(File::open(sample).unwrap()).unwrap();
        assert_eq!(decoder.is_aifc(), aifc);
        assert_eq!(
            decoder.spec(),
            AudioSpec::new(8_000, 2, SampleFormat::PcmI16).unwrap()
        );
        let mut oracle = BufReader::new(File::open(oracle).unwrap());
        let mut frames = 0;
        while let Some(block) = decoder.read_frames(2_048).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("McGill 12-bit AIFF/AIFC sample must decode to signed 16-bit PCM");
            };
            let mut bytes = vec![0; samples.len() * 2];
            oracle.read_exact(&mut bytes).unwrap();
            for (sample, expected) in samples.iter().zip(bytes.chunks_exact(2)) {
                assert_eq!(*sample, i16::from_le_bytes([expected[0], expected[1]]));
            }
            frames += (samples.len() / 2) as u64;
        }
        assert_eq!(frames, FRAMES);
        let mut trailing = [0; 1];
        assert_eq!(
            oracle.read(&mut trailing).unwrap(),
            0,
            "FFmpeg PCM oracle has trailing samples"
        );
    }
}
