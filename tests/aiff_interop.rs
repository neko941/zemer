use std::{
    fs::File,
    io::{BufReader, Read},
    process::Command,
};

use aiff::AiffDecoder;
use zemer_core::{AudioDecoder, AudioSpec, SampleFormat, Samples};

fn append_pcm_bytes(samples: &Samples, output: &mut Vec<u8>) {
    match samples {
        Samples::U8(values) => output.extend_from_slice(values),
        Samples::I16(values) => values
            .iter()
            .for_each(|sample| output.extend_from_slice(&sample.to_le_bytes())),
        Samples::I24(values) => values
            .iter()
            .for_each(|sample| output.extend_from_slice(&sample.to_le_bytes()[..3])),
        Samples::I32(values) => values
            .iter()
            .for_each(|sample| output.extend_from_slice(&sample.to_le_bytes())),
        Samples::F32(values) => values
            .iter()
            .for_each(|sample| output.extend_from_slice(&sample.to_le_bytes())),
        Samples::F64(values) => values
            .iter()
            .for_each(|sample| output.extend_from_slice(&sample.to_le_bytes())),
    }
}

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

#[test]
#[ignore = "requires McGill AFsp AIFF/AIFC samples in ZEMER_MCGILL_AIFF_CORPUS and FFmpeg in ZEMER_FFMPEG"]
fn decodes_mcgill_afsp_profiles_against_ffmpeg() {
    let corpus = std::env::var_os("ZEMER_MCGILL_AIFF_CORPUS")
        .expect("set ZEMER_MCGILL_AIFF_CORPUS to the downloaded AFsp sample directory");
    let corpus = std::path::PathBuf::from(corpus);
    let ffmpeg = std::env::var_os("ZEMER_FFMPEG").expect("set ZEMER_FFMPEG to the FFmpeg binary");
    let cases = [
        ("M1F1-AlawC-AFsp.aif", true, SampleFormat::PcmI16, "s16le"),
        ("M1F1-mulawC-AFsp.aif", true, SampleFormat::PcmI16, "s16le"),
        ("M1F1-int8-AFsp.aif", false, SampleFormat::PcmU8, "u8"),
        ("M1F1-int8C-AFsp.aif", true, SampleFormat::PcmU8, "u8"),
        ("M1F1-int12-AFsp.aif", false, SampleFormat::PcmI16, "s16le"),
        ("M1F1-int12C-AFsp.aif", true, SampleFormat::PcmI16, "s16le"),
        ("M1F1-int16-AFsp.aif", false, SampleFormat::PcmI16, "s16le"),
        ("M1F1-int16C-AFsp.aif", true, SampleFormat::PcmI16, "s16le"),
        ("M1F1-int16s-AFsp.aif", true, SampleFormat::PcmI16, "s16le"),
        ("M1F1-int24-AFsp.aif", false, SampleFormat::PcmI24, "s24le"),
        ("M1F1-int24C-AFsp.aif", true, SampleFormat::PcmI24, "s24le"),
        ("M1F1-int32-AFsp.aif", false, SampleFormat::PcmI32, "s32le"),
        ("M1F1-int32C-AFsp.aif", true, SampleFormat::PcmI32, "s32le"),
        (
            "M1F1-float32C-AFsp.aif",
            true,
            SampleFormat::Float32,
            "f32le",
        ),
        (
            "M1F1-float64C-AFsp.aif",
            true,
            SampleFormat::Float64,
            "f64le",
        ),
    ];

    for (name, aifc, sample_format, pcm_muxer) in cases {
        let path = corpus.join(name);
        let mut decoder = AiffDecoder::new(File::open(&path).unwrap()).unwrap();
        assert_eq!(decoder.is_aifc(), aifc, "{name}");
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
            append_pcm_bytes(block.samples(), &mut actual);
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
#[ignore = "requires McGill AIFF/AIFC perverse samples in ZEMER_MCGILL_AIFF_PERVERSE_CORPUS and FFmpeg in ZEMER_FFMPEG"]
fn decodes_mcgill_aifc_perverse_files_against_ffmpeg() {
    let corpus = std::env::var_os("ZEMER_MCGILL_AIFF_PERVERSE_CORPUS")
        .expect("set ZEMER_MCGILL_AIFF_PERVERSE_CORPUS to the downloaded sample directory");
    let corpus = std::path::PathBuf::from(corpus);
    let ffmpeg = std::env::var_os("ZEMER_FFMPEG").expect("set ZEMER_FFMPEG to the FFmpeg binary");
    let cases = [
        ("Pmiscck.aif", 9),
        ("Pnossnd.aif", 0),
        ("Poffset.aif", 9),
        ("Porder.aif", 9),
        ("Ptjunk.aif", 9),
        ("Fnonull.aif", 9),
    ];

    for (name, expected_frames) in cases {
        let path = corpus.join(name);
        let mut decoder = AiffDecoder::new(File::open(&path).unwrap()).unwrap();
        assert!(decoder.is_aifc(), "{name}");
        assert_eq!(decoder.spec().sample_rate(), 8_000, "{name}");
        assert_eq!(decoder.spec().channels(), 1, "{name}");
        assert_eq!(
            decoder.spec().sample_format(),
            SampleFormat::PcmI16,
            "{name}"
        );

        let oracle = Command::new(&ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-map", "0:a:0", "-f", "s16le", "pipe:1"])
            .output()
            .unwrap();
        assert!(
            oracle.status.success(),
            "FFmpeg failed for {name}: {}",
            String::from_utf8_lossy(&oracle.stderr)
        );

        let mut actual = Vec::new();
        let mut frames = 0;
        while let Some(block) = decoder.read_frames(4).unwrap() {
            frames += block.frames();
            append_pcm_bytes(block.samples(), &mut actual);
        }
        assert_eq!(frames, expected_frames, "{name}");
        let expected_bytes = expected_frames * 2;
        assert!(
            oracle.stdout.len() >= expected_bytes,
            "{name} oracle byte count"
        );
        // These files deliberately include SSND padding/trailing data beyond the COMM frame count.
        assert_eq!(&actual, &oracle.stdout[..expected_bytes], "{name} PCM");
    }
}

#[test]
#[ignore = "requires McGill o2_726.aiff and external SpanDSP PCM oracle paths in ZEMER_AIFC_G726_*"]
fn decodes_mcgill_aifc_g726_against_spandsp_pcm_oracle() {
    let sample = std::env::var_os("ZEMER_AIFC_G726_SAMPLE").expect("G.726 sample path is required");
    let oracle = std::env::var_os("ZEMER_AIFC_G726_SPANDSP_PCM")
        .expect("SpanDSP PCM oracle path is required");
    let mut decoder = AiffDecoder::new(File::open(sample).unwrap()).unwrap();
    assert!(decoder.is_aifc());
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap()
    );
    let mut oracle = BufReader::new(File::open(oracle).unwrap());
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(17).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("McGill G.726 sample must decode to signed 16-bit PCM");
        };
        let mut bytes = vec![0; samples.len() * 2];
        oracle.read_exact(&mut bytes).unwrap();
        for (sample, expected) in samples.iter().zip(bytes.chunks_exact(2)) {
            assert_eq!(*sample, i16::from_le_bytes([expected[0], expected[1]]));
        }
        frames += samples.len() as u64;
    }
    assert_eq!(frames, 294);
    let mut packed_padding_samples = [0; 4];
    oracle.read_exact(&mut packed_padding_samples).unwrap();
    let mut trailing = [0; 1];
    assert_eq!(oracle.read(&mut trailing).unwrap(), 0);
}
