use std::{fs::File, path::PathBuf, process::Command};

use au::AuDecoder;
use zemer_core::{AudioDecoder, SampleFormat, Samples};

fn append_signed_pcm(samples: &Samples, output: &mut Vec<u8>) {
    match samples {
        Samples::U8(values) => output.extend(values.iter().map(|sample| sample ^ 0x80)),
        Samples::I16(values) => values
            .iter()
            .for_each(|sample| output.extend_from_slice(&sample.to_be_bytes())),
        Samples::I24(values) => values
            .iter()
            .for_each(|sample| output.extend_from_slice(&sample.to_be_bytes()[1..])),
        Samples::I32(values) => values
            .iter()
            .for_each(|sample| output.extend_from_slice(&sample.to_be_bytes())),
        Samples::F32(values) => values
            .iter()
            .for_each(|sample| output.extend_from_slice(&sample.to_bits().to_be_bytes())),
        Samples::F64(values) => values
            .iter()
            .for_each(|sample| output.extend_from_slice(&sample.to_bits().to_be_bytes())),
    }
}

#[test]
#[ignore = "requires McGill AU samples in ZEMER_MCGILL_AU_CORPUS and FFmpeg in ZEMER_FFMPEG"]
fn decodes_mcgill_au_profiles_against_ffmpeg() {
    let corpus = PathBuf::from(
        std::env::var_os("ZEMER_MCGILL_AU_CORPUS")
            .expect("set ZEMER_MCGILL_AU_CORPUS to the downloaded sample directory"),
    );
    let ffmpeg = std::env::var_os("ZEMER_FFMPEG").expect("set ZEMER_FFMPEG to FFmpeg");
    let cases = [
        ("M1F1-Alaw-AFsp.au", SampleFormat::PcmI16, "s16be"),
        ("M1F1-mulaw-AFsp.au", SampleFormat::PcmI16, "s16be"),
        ("M1F1-int8-AFsp.au", SampleFormat::PcmU8, "s8"),
        ("M1F1-int16-AFsp.au", SampleFormat::PcmI16, "s16be"),
        ("M1F1-int24-AFsp.au", SampleFormat::PcmI24, "s24be"),
        ("M1F1-int32-AFsp.au", SampleFormat::PcmI32, "s32be"),
        ("M1F1-float32-AFsp.au", SampleFormat::Float32, "f32be"),
        ("M1F1-float64-AFsp.au", SampleFormat::Float64, "f64be"),
    ];

    for (name, sample_format, pcm_muxer) in cases {
        let path = corpus.join(name);
        let mut decoder = AuDecoder::new(File::open(&path).unwrap()).unwrap();
        assert_eq!(decoder.spec().sample_rate(), 8_000, "{name}");
        assert_eq!(decoder.spec().channels(), 2, "{name}");
        assert_eq!(decoder.spec().sample_format(), sample_format, "{name}");

        let oracle = Command::new(&ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-map", "0:a:0", "-f", pcm_muxer, "pipe:1"])
            .output()
            .unwrap();
        assert!(
            oracle.status.success(),
            "FFmpeg failed for {name}: {}",
            String::from_utf8_lossy(&oracle.stderr)
        );

        let mut actual = Vec::new();
        let mut frames = 0;
        while let Some(block) = decoder.read_frames(257).unwrap() {
            frames += block.frames();
            append_signed_pcm(block.samples(), &mut actual);
        }
        assert_eq!(frames, 23_493, "{name}");
        assert_eq!(actual.len(), oracle.stdout.len(), "{name} byte count");
        assert!(
            actual == oracle.stdout,
            "{name} differs from FFmpeg at byte {:?}",
            actual
                .iter()
                .zip(&oracle.stdout)
                .position(|(actual, expected)| actual != expected)
        );
    }
}

#[test]
#[ignore = "requires the two malformed McGill AU samples in ZEMER_MCGILL_AU_PERVERSE_CORPUS"]
fn rejects_mcgill_perverse_au_samples() {
    let corpus = PathBuf::from(
        std::env::var_os("ZEMER_MCGILL_AU_PERVERSE_CORPUS")
            .expect("set ZEMER_MCGILL_AU_PERVERSE_CORPUS to the downloaded sample directory"),
    );

    for name in ["monkey.au", "soundB.au"] {
        assert!(
            AuDecoder::new(File::open(corpus.join(name)).unwrap()).is_err(),
            "{name} should be rejected"
        );
    }
}
