use std::{
    fs::File,
    io::{BufReader, Cursor, Read, Write},
    process::{Command, Stdio},
};

use aiff::{AiffDecoder, AiffEncoder};
use g711::G711Encoding;
use zemer_core::{AudioDecoder, AudioSpec, PcmBuffer, SampleFormat, Samples};

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

fn aiff_chunk_body<'a>(bytes: &'a [u8], wanted: &[u8; 4]) -> &'a [u8] {
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body_start = offset + 8;
        let body_end = body_start.checked_add(size).unwrap();
        assert!(body_end <= bytes.len(), "truncated generated AIFF chunk");
        if &bytes[offset..offset + 4] == wanted {
            return &bytes[body_start..body_end];
        }
        offset = body_end + size % 2;
    }
    panic!("missing generated AIFF chunk {wanted:?}");
}

const FRAMES: u64 = 23_493;

fn decode_ffmpeg(ffmpeg: &std::ffi::OsStr, encoded: &[u8], pcm_format: &str) -> Vec<u8> {
    let mut child = Command::new(ffmpeg)
        .args(["-v", "error", "-i", "pipe:0", "-map", "0:a:0", "-f"])
        .arg(pcm_format)
        .arg("pipe:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(encoded).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "FFmpeg rejected AIFF output: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
#[ignore = "requires FFmpeg in ZEMER_FFMPEG"]
fn encodes_aifc_ima4_compatible_with_ffmpeg() {
    let ffmpeg = std::env::var_os("ZEMER_FFMPEG").expect("set ZEMER_FFMPEG to FFmpeg");
    let spec = AudioSpec::new(8_000, 2, SampleFormat::PcmI16).unwrap();
    let frames = 391;
    let samples: Vec<_> = (0..frames)
        .flat_map(|frame| {
            [0, 1].map(|channel| (((frame * (173 + channel * 19)) % 32_000) as i32 - 16_000) as i16)
        })
        .collect();
    let mut encoder = AiffEncoder::new_aifc_ima4(Cursor::new(Vec::new()), spec).unwrap();
    for (start, end) in [(0, 59), (59, 211), (211, frames)] {
        let block =
            PcmBuffer::new(spec, Samples::I16(samples[start * 2..end * 2].to_vec())).unwrap();
        encoder.write_block(&block).unwrap();
    }
    let encoded = encoder.finish().unwrap().get_ref().clone();
    let oracle = decode_ffmpeg(&ffmpeg, &encoded, "s16le");

    let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(decoder.spec(), spec);
    let mut actual = Vec::new();
    let mut decoded_frames = 0;
    while let Some(block) = decoder.read_frames(37).unwrap() {
        decoded_frames += block.frames();
        append_pcm_bytes(block.samples(), &mut actual);
    }
    assert_eq!(decoded_frames, 7 * 64);
    assert_eq!(actual, oracle, "FFmpeg and Zemer IMA4 decoders differ");
}

#[test]
#[ignore = "requires FFmpeg in ZEMER_FFMPEG"]
fn encodes_aiff_pcm_bit_depths_compatible_with_ffmpeg() {
    let ffmpeg = std::env::var_os("ZEMER_FFMPEG").expect("set ZEMER_FFMPEG to FFmpeg");
    let cases = [
        (
            SampleFormat::PcmU8,
            "u8",
            1,
            1,
            Samples::U8(vec![0, 255, 1, 128, 127, 128, 17, 231, 63, 192]),
        ),
        (
            SampleFormat::PcmI16,
            "s16le",
            2,
            9,
            Samples::I16(vec![
                i16::MIN,
                i16::MAX,
                -3_001,
                3_001,
                -1,
                1,
                0,
                -16,
                -123,
                123,
            ]),
        ),
        (
            SampleFormat::PcmI24,
            "s24le",
            3,
            17,
            Samples::I24(vec![
                -8_388_608, 8_388_607, -123_457, 123_457, -1, 1, 0, -16, -123, 123,
            ]),
        ),
        (
            SampleFormat::PcmI32,
            "s32le",
            4,
            25,
            Samples::I32(vec![
                i32::MIN,
                i32::MAX,
                -123_456_789,
                123_456_789,
                -1,
                1,
                0,
                -16,
                -123,
                123,
            ]),
        ),
    ];

    for (format, pcm_format, bytes_per_sample, first_valid_bits, samples) in cases {
        let spec = AudioSpec::new(48_000, 2, format).unwrap();
        let block = PcmBuffer::new(spec, samples).unwrap();
        let last_valid_bits = first_valid_bits + 7;
        for valid_bits in first_valid_bits..=last_valid_bits {
            let mut encoder =
                AiffEncoder::new_with_bits_per_sample(Cursor::new(Vec::new()), spec, valid_bits)
                    .unwrap();
            encoder.write_block(&block).unwrap();
            let encoded = encoder.finish().unwrap().get_ref().clone();
            let oracle = decode_ffmpeg(&ffmpeg, &encoded, pcm_format);

            let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
            assert_eq!(decoder.spec(), spec, "{format:?}, {valid_bits} valid bits");
            let mut actual = Vec::new();
            while let Some(block) = decoder.read_frames(3).unwrap() {
                append_pcm_bytes(block.samples(), &mut actual);
            }
            assert_eq!(
                actual.len(),
                block.samples().len() * bytes_per_sample,
                "{format:?}, {valid_bits} valid bits frame count"
            );
            assert_eq!(actual, oracle, "{format:?}, {valid_bits} valid bits");
        }
    }
}

#[test]
#[ignore = "requires FFmpeg in ZEMER_FFMPEG"]
fn encodes_aifc_g711_compatible_with_ffmpeg() {
    let ffmpeg = std::env::var_os("ZEMER_FFMPEG").expect("set ZEMER_FFMPEG to FFmpeg");
    let spec = AudioSpec::new(8_000, 2, SampleFormat::PcmI16).unwrap();
    let samples: Vec<_> = (0..=u16::MAX).map(|sample| sample as i16).collect();
    let input = PcmBuffer::new(spec, Samples::I16(samples.clone())).unwrap();

    for encoding in [G711Encoding::ALaw, G711Encoding::MuLaw] {
        let mut encoder =
            AiffEncoder::new_aifc_g711(Cursor::new(Vec::new()), spec, encoding).unwrap();
        for samples in samples.chunks(1_000) {
            encoder
                .write_block(&PcmBuffer::new(spec, Samples::I16(samples.to_vec())).unwrap())
                .unwrap();
        }
        let encoded = encoder.finish().unwrap().get_ref().clone();
        let oracle = decode_ffmpeg(&ffmpeg, &encoded, "s16le");

        let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
        assert!(decoder.is_aifc());
        assert_eq!(decoder.spec(), spec);
        let mut actual = Vec::new();
        let mut frames = 0;
        while let Some(block) = decoder.read_frames(333).unwrap() {
            frames += block.frames();
            append_pcm_bytes(block.samples(), &mut actual);
        }
        assert_eq!(frames, input.frames());
        assert_eq!(oracle.len(), samples.len() * 2);
        assert_eq!(actual, oracle, "{encoding:?} differs from FFmpeg");
    }
}

#[test]
#[ignore = "requires external PCM and SpanDSP packed-codeword oracles"]
fn encodes_aifc_g726_matches_spandsp_oracles() {
    let pcm_path = std::env::var_os("ZEMER_G726_ENCODER_PCM")
        .expect("set ZEMER_G726_ENCODER_PCM to little-endian signed-16 PCM");
    let oracle_dir = std::env::var_os("ZEMER_G726_ENCODER_ORACLE_DIR")
        .expect("set ZEMER_G726_ENCODER_ORACLE_DIR to the SpanDSP oracle directory");
    let pcm = std::fs::read(pcm_path).unwrap();
    assert_eq!(pcm.len() % 2, 0);
    let samples: Vec<_> = pcm
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
        .collect();
    assert_eq!(samples.len(), 4_096);
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();

    for (rate, label) in [
        (aiff::G726Rate::Kbps16, "16000"),
        (aiff::G726Rate::Kbps24, "24000"),
        (aiff::G726Rate::Kbps32, "32000"),
        (aiff::G726Rate::Kbps40, "40000"),
    ] {
        let mut encoder = AiffEncoder::new_aifc_g726(Cursor::new(Vec::new()), spec, rate).unwrap();
        for block in samples.chunks(37) {
            encoder
                .write_block(&PcmBuffer::new(spec, Samples::I16(block.to_vec())).unwrap())
                .unwrap();
        }
        let encoded = encoder.finish().unwrap().get_ref().clone();
        let ssnd = aiff_chunk_body(&encoded, b"SSND");
        assert!(ssnd.len() >= 8);
        assert_eq!(&ssnd[..8], &[0; 8]);
        assert_eq!(
            &ssnd[8..],
            std::fs::read(std::path::Path::new(&oracle_dir).join(format!("{label}.raw"))).unwrap()
        );
        let common = aiff_chunk_body(&encoded, b"COMM");
        assert_eq!(&common[18..22], b"G726");
        assert_eq!(
            u32::from_be_bytes(common[2..6].try_into().unwrap()),
            samples.len() as u32
        );
        let mut hint = b"stocG726".to_vec();
        hint.extend_from_slice(&(u32::from(rate.bits_per_sample()) * 8_000).to_be_bytes());
        assert_eq!(aiff_chunk_body(&encoded, b"APPL"), hint);
    }
}

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
#[ignore = "requires McGill CCRMA AIFF/AIFC samples in ZEMER_MCGILL_CCRMA_AIFF_CORPUS and FFmpeg in ZEMER_FFMPEG"]
fn decodes_mcgill_ccrma_samples_against_ffmpeg() {
    let corpus = std::env::var_os("ZEMER_MCGILL_CCRMA_AIFF_CORPUS")
        .expect("set ZEMER_MCGILL_CCRMA_AIFF_CORPUS to the downloaded sample directory");
    let corpus = std::path::PathBuf::from(corpus);
    let ffmpeg = std::env::var_os("ZEMER_FFMPEG").expect("set ZEMER_FFMPEG to the FFmpeg binary");
    let cases = [
        (
            "aiff-16.snd",
            false,
            44_100,
            2,
            SampleFormat::PcmI16,
            "s16le",
            32_928,
        ),
        (
            "aiff-8.snd",
            false,
            44_100,
            2,
            SampleFormat::PcmU8,
            "u8",
            32_928,
        ),
        (
            "alaw.aifc",
            true,
            44_100,
            1,
            SampleFormat::PcmI16,
            "s16le",
            1_622,
        ),
        (
            "ulaw.aifc",
            true,
            44_100,
            1,
            SampleFormat::PcmI16,
            "s16le",
            1_622,
        ),
        (
            "digit0v0.aiff",
            true,
            8_000,
            1,
            SampleFormat::PcmI16,
            "s16le",
            4_480,
        ),
        (
            "wood12.aiff",
            false,
            44_100,
            1,
            SampleFormat::PcmI16,
            "s16le",
            1_622,
        ),
        (
            "wood24.aiff",
            false,
            44_100,
            1,
            SampleFormat::PcmI24,
            "s24le",
            1_622,
        ),
    ];

    for (name, aifc, sample_rate, channels, sample_format, pcm_muxer, expected_frames) in cases {
        let path = corpus.join(name);
        let mut decoder = AiffDecoder::new(File::open(&path).unwrap()).unwrap();
        assert_eq!(decoder.is_aifc(), aifc, "{name}");
        assert_eq!(decoder.spec().sample_rate(), sample_rate, "{name}");
        assert_eq!(decoder.spec().channels(), channels, "{name}");
        assert_eq!(decoder.spec().sample_format(), sample_format, "{name}");

        let corrected_path = if name == "digit0v0.aiff" {
            let mut corrected = std::fs::read(&path).unwrap();
            let comm = corrected
                .windows(4)
                .position(|window| window == b"COMM")
                .unwrap();
            assert_eq!(&corrected[comm + 4..comm + 8], &18_u32.to_be_bytes());
            assert_eq!(
                &corrected[comm + 26..comm + 46],
                b"NONE\x0enot compressed\0"
            );
            corrected[comm + 4..comm + 8].copy_from_slice(&38_u32.to_be_bytes());
            let corrected_path =
                std::env::temp_dir().join(format!("zemer-digit0v0-{}.aiff", std::process::id()));
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&corrected_path)
                .unwrap()
                .write_all(&corrected)
                .unwrap();
            Some(corrected_path)
        } else {
            None
        };
        let oracle_input = corrected_path.as_deref().unwrap_or(&path);
        let oracle = Command::new(&ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(oracle_input)
            .args(["-map", "0:a:0", "-f", pcm_muxer, "pipe:1"])
            .output();
        if let Some(corrected_path) = corrected_path {
            std::fs::remove_file(corrected_path).unwrap();
        }
        let oracle = oracle.unwrap();
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
        assert_eq!(frames, expected_frames, "{name}");
        assert_eq!(actual, oracle.stdout, "{name} PCM");
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
