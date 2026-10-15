use std::{
    ffi::OsString,
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

fn replace_aiff_chunk_body(bytes: &[u8], wanted: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut output = bytes[..12].to_vec();
    let mut offset = 12;
    let mut replaced = false;
    while offset + 8 <= bytes.len() {
        let size = u32::from_be_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let end = offset + 8 + size;
        let padded_end = end + (size & 1);
        assert!(padded_end <= bytes.len(), "truncated AIFF chunk");
        if &bytes[offset..offset + 4] == wanted {
            assert!(!replaced, "duplicate AIFF chunk {wanted:?}");
            output.extend_from_slice(wanted);
            output.extend_from_slice(&(body.len() as u32).to_be_bytes());
            output.extend_from_slice(body);
            if body.len() & 1 != 0 {
                output.push(0);
            }
            replaced = true;
        } else {
            output.extend_from_slice(&bytes[offset..padded_end]);
        }
        offset = padded_end;
    }
    assert!(replaced, "missing AIFF chunk {wanted:?}");
    let form_size = (output.len() - 8) as u32;
    output[4..8].copy_from_slice(&form_size.to_be_bytes());
    output
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
    let frames = 391;
    for channels in 1..=2 {
        let spec = AudioSpec::new(8_000, channels, SampleFormat::PcmI16).unwrap();
        let samples: Vec<_> = (0..frames)
            .flat_map(|frame| {
                (0..usize::from(channels)).map(move |channel| {
                    (((frame * (173 + channel * 19)) % 32_000) as i32 - 16_000) as i16
                })
            })
            .collect();
        let mut encoder = AiffEncoder::new_aifc_ima4(Cursor::new(Vec::new()), spec).unwrap();
        for (start, end) in [(0, 59), (59, 211), (211, frames)] {
            let block = PcmBuffer::new(
                spec,
                Samples::I16(
                    samples[start * usize::from(channels)..end * usize::from(channels)].to_vec(),
                ),
            )
            .unwrap();
            encoder.write_block(&block).unwrap();
        }
        let encoded = encoder.finish().unwrap().get_ref().clone();
        let oracle = decode_ffmpeg(&ffmpeg, &encoded, "s16le");

        let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
        assert_eq!(decoder.spec(), spec, "{channels} channels");
        let mut actual = Vec::new();
        let mut decoded_frames = 0;
        while let Some(block) = decoder.read_frames(37).unwrap() {
            decoded_frames += block.frames();
            append_pcm_bytes(block.samples(), &mut actual);
        }
        assert_eq!(decoded_frames, 7 * 64, "{channels} channels");
        assert_eq!(
            actual, oracle,
            "FFmpeg and Zemer IMA4 differ at {channels} channels"
        );
    }
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
fn encodes_aiff_multichannel_pcm_compatible_with_ffmpeg() {
    let ffmpeg = std::env::var_os("ZEMER_FFMPEG").expect("set ZEMER_FFMPEG to FFmpeg");

    for channels in 1..=8 {
        let spec = AudioSpec::new(48_000, channels, SampleFormat::PcmI24).unwrap();
        let samples: Vec<_> = (0..7 * usize::from(channels))
            .map(|index| ((index * 1_003_117 % (1 << 24)) as i32) - (1 << 23))
            .collect();
        let input = PcmBuffer::new(spec, Samples::I24(samples)).unwrap();
        let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_block(&input).unwrap();
        let encoded = encoder.finish().unwrap().get_ref().clone();
        let oracle = decode_ffmpeg(&ffmpeg, &encoded, "s24le");

        let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
        assert_eq!(decoder.spec(), spec, "{channels} channels");
        let mut actual = Vec::new();
        let mut frames = 0;
        while let Some(block) = decoder.read_frames(3).unwrap() {
            frames += block.frames();
            append_pcm_bytes(block.samples(), &mut actual);
        }
        let mut expected = Vec::new();
        append_pcm_bytes(input.samples(), &mut expected);
        assert_eq!(frames, 7, "{channels} channels");
        assert_eq!(actual, expected, "Zemer, {channels} channels");
        assert_eq!(oracle, expected, "FFmpeg, {channels} channels");
    }
}

#[test]
#[ignore = "requires FFmpeg in ZEMER_FFMPEG"]
fn encodes_aifc_float_compatible_with_ffmpeg() {
    let ffmpeg = std::env::var_os("ZEMER_FFMPEG").expect("set ZEMER_FFMPEG to FFmpeg");
    let cases = [
        (
            SampleFormat::Float32,
            "f32le",
            Samples::F32(vec![
                0.0,
                -0.0,
                1.0,
                -1.0,
                0.5,
                -0.5,
                f32::MIN_POSITIVE,
                f32::MAX,
                0.125,
                -0.125,
            ]),
        ),
        (
            SampleFormat::Float64,
            "f64le",
            Samples::F64(vec![
                0.0,
                -0.0,
                1.0,
                -1.0,
                0.5,
                -0.5,
                f64::MIN_POSITIVE,
                f64::MAX,
                0.125,
                -0.125,
            ]),
        ),
    ];

    for (format, pcm_format, samples) in cases {
        let spec = AudioSpec::new(48_000, 2, format).unwrap();
        let input = PcmBuffer::new(spec, samples).unwrap();
        let mut encoder = AiffEncoder::new_aifc(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_block(&input).unwrap();
        let encoded = encoder.finish().unwrap().get_ref().clone();
        let oracle = decode_ffmpeg(&ffmpeg, &encoded, pcm_format);

        let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
        assert_eq!(decoder.spec(), spec);
        let mut actual = Vec::new();
        let mut frames = 0;
        while let Some(block) = decoder.read_frames(3).unwrap() {
            frames += block.frames();
            append_pcm_bytes(block.samples(), &mut actual);
        }
        let mut expected = Vec::new();
        append_pcm_bytes(input.samples(), &mut expected);
        assert_eq!(frames, input.frames());
        assert_eq!(actual, expected, "Zemer {format:?} round-trip");
        assert_eq!(oracle, expected, "FFmpeg {format:?} decode");
    }
}

#[test]
#[ignore = "requires Python 3 and libsndfile"]
fn encodes_mono_24_bit_aifc_dwvw_compatible_with_libsndfile() {
    let python = std::env::var_os("ZEMER_SNDFILE_PYTHON")
        .unwrap_or_else(|| OsString::from(if cfg!(windows) { "python" } else { "python3" }));
    let spec = AudioSpec::new(44_100, 1, SampleFormat::PcmI24).unwrap();
    let frames = 257_usize;
    let samples: Vec<_> = (0..frames)
        .map(|frame| {
            let phase = i64::try_from(frame).unwrap() * 65_537;
            (phase % 8_000_000 - 4_000_000) as i32
        })
        .collect();
    let mut encoder =
        AiffEncoder::new_aifc_dwvw_with_bits_per_sample(Cursor::new(Vec::new()), spec, 24).unwrap();
    for (start, end) in [(0, 41), (41, 187), (187, frames)] {
        let block = PcmBuffer::new(spec, Samples::I24(samples[start..end].to_vec())).unwrap();
        encoder.write_block(&block).unwrap();
    }
    let encoded = encoder.finish().unwrap().get_ref().clone();
    let input_path =
        std::env::temp_dir().join(format!("zemer-aifc-dwvw-{}.aifc", std::process::id()));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&input_path)
        .unwrap()
        .write_all(&encoded)
        .unwrap();
    let oracle_script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scripts/decode_libsndfile.py");
    let output = Command::new(python)
        .arg(oracle_script)
        .arg(&input_path)
        .output()
        .unwrap();
    std::fs::remove_file(input_path).unwrap();
    assert!(
        output.status.success(),
        "libsndfile rejected AIFF output: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut expected_i24 = Vec::with_capacity(samples.len() * 3);
    let mut expected_s32 = Vec::with_capacity(samples.len() * 4);
    for sample in &samples {
        expected_i24.extend_from_slice(&sample.to_le_bytes()[..3]);
        expected_s32.extend_from_slice(&(*sample << 8).to_le_bytes());
    }
    assert_eq!(output.stdout.len(), expected_s32.len());
    for (index, (actual, expected)) in output
        .stdout
        .chunks_exact(4)
        .zip(expected_s32.chunks_exact(4))
        .enumerate()
    {
        assert_eq!(
            i32::from_le_bytes(actual.try_into().unwrap()),
            i32::from_le_bytes(expected.try_into().unwrap()),
            "libsndfile DWVW sample {index}"
        );
    }

    let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(decoder.spec(), spec);
    let mut actual = Vec::new();
    let mut decoded_frames = 0;
    while let Some(block) = decoder.read_frames(37).unwrap() {
        decoded_frames += block.frames();
        append_pcm_bytes(block.samples(), &mut actual);
    }
    assert_eq!(decoded_frames, frames);
    assert_eq!(actual, expected_i24, "Zemer DWVW round trip differs");
}

#[test]
#[ignore = "requires Python 3 and libsndfile"]
fn interoperates_stereo_aifc_dwvw_channels_with_libsndfile() {
    let python = std::env::var_os("ZEMER_SNDFILE_PYTHON")
        .unwrap_or_else(|| OsString::from(if cfg!(windows) { "python" } else { "python3" }));
    let encoder_script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/scripts/encode_libsndfile_dwvw.py");
    let decoder_script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scripts/decode_libsndfile.py");

    for bits in [16_u16, 24] {
        let sample_format = match bits {
            16 => SampleFormat::PcmI16,
            24 => SampleFormat::PcmI24,
            _ => unreachable!(),
        };
        let spec = AudioSpec::new(44_100, 2, sample_format).unwrap();
        let span = if bits == 16 {
            12_000_i64
        } else {
            4_000_000_i64
        };
        let values: Vec<i32> = (0..257)
            .flat_map(|frame| {
                (0..2).map(move |channel| {
                    let phase = i64::from(frame) * 977 + i64::from(channel) * 9_973;
                    (phase % (span * 2 + 1) - span) as i32
                })
            })
            .collect();
        let samples = match bits {
            16 => Samples::I16(values.iter().map(|&sample| sample as i16).collect()),
            24 => Samples::I24(values.clone()),
            _ => unreachable!(),
        };
        let input = PcmBuffer::new(spec, samples).unwrap();
        let mut expected_payload = Vec::new();
        for channel in 0..2 {
            let raw_path = std::env::temp_dir().join(format!(
                "zemer-aifc-dwvw-ref-{bits}-{channel}-{}.raw",
                std::process::id()
            ));
            let aifc_path = std::env::temp_dir().join(format!(
                "zemer-aifc-dwvw-ref-{bits}-{channel}-{}.aifc",
                std::process::id()
            ));
            let mut raw = Vec::with_capacity(257 * 4);
            for sample in values.iter().skip(channel).step_by(2) {
                raw.extend_from_slice(&(*sample << (32 - bits)).to_le_bytes());
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&raw_path)
                .unwrap()
                .write_all(&raw)
                .unwrap();
            let oracle = Command::new(&python)
                .arg(&encoder_script)
                .arg(&raw_path)
                .arg(&aifc_path)
                .arg(bits.to_string())
                .output();
            std::fs::remove_file(&raw_path).unwrap();
            let oracle = oracle.unwrap();
            assert!(
                oracle.status.success(),
                "libsndfile failed to encode {bits}-bit channel {channel}: {}",
                String::from_utf8_lossy(&oracle.stderr)
            );
            let oracle_file = std::fs::read(&aifc_path).unwrap();
            std::fs::remove_file(&aifc_path).unwrap();
            let sound = aiff_chunk_body(&oracle_file, b"SSND");
            assert_eq!(&sound[..8], &[0; 8]);
            let channel_payload = &sound[8..];
            let mut state = dwvw::DecoderState::at(0);
            dwvw::scan_samples(
                &mut Cursor::new(channel_payload),
                &mut state,
                bits as u8,
                (channel_payload.len() as u64) * 8,
                257,
            )
            .unwrap();
            state
                .finish_channel((channel_payload.len() as u64) * 8)
                .unwrap();
            let channel_bytes = (state.bit_position() / 8) as usize;
            assert!(channel_bytes <= channel_payload.len());
            expected_payload.extend_from_slice(&channel_payload[..channel_bytes]);
        }

        let mut encoder =
            AiffEncoder::new_aifc_dwvw_with_bits_per_sample(Cursor::new(Vec::new()), spec, bits)
                .unwrap();
        encoder.write_block(&input).unwrap();
        let template = encoder.finish().unwrap().get_ref().clone();
        let common = aiff_chunk_body(&template, b"COMM");
        assert_eq!(u16::from_be_bytes(common[..2].try_into().unwrap()), 2);
        assert_eq!(u32::from_be_bytes(common[2..6].try_into().unwrap()), 257);

        let sound = aiff_chunk_body(&template, b"SSND");
        assert_eq!(&sound[..8], &[0; 8]);
        let writer_payload = &sound[8..];
        let mut channel_start_bit = 0;
        for channel in 0..2 {
            let mut state = dwvw::DecoderState::at(channel_start_bit);
            dwvw::scan_samples(
                &mut Cursor::new(writer_payload),
                &mut state,
                bits as u8,
                (writer_payload.len() as u64) * 8,
                257,
            )
            .unwrap();
            state
                .finish_channel((writer_payload.len() as u64) * 8)
                .unwrap();
            let channel_end_bit = state.bit_position();
            let channel_payload =
                &writer_payload[(channel_start_bit / 8) as usize..(channel_end_bit / 8) as usize];
            let mono_spec = AudioSpec::new(44_100, 1, sample_format).unwrap();
            let channel_samples: Vec<i32> =
                values.iter().skip(channel).step_by(2).copied().collect();
            let mono_samples = match bits {
                16 => Samples::I16(channel_samples.iter().map(|&value| value as i16).collect()),
                24 => Samples::I24(channel_samples.clone()),
                _ => unreachable!(),
            };
            let mono_input = PcmBuffer::new(mono_spec, mono_samples).unwrap();
            let mut mono_encoder = AiffEncoder::new_aifc_dwvw_with_bits_per_sample(
                Cursor::new(Vec::new()),
                mono_spec,
                bits,
            )
            .unwrap();
            mono_encoder.write_block(&mono_input).unwrap();
            let mono_template = mono_encoder.finish().unwrap().get_ref().clone();
            let mut mono_sound = vec![0; 8];
            mono_sound.extend_from_slice(channel_payload);
            let mono_file = replace_aiff_chunk_body(&mono_template, b"SSND", &mono_sound);
            let mono_path = std::env::temp_dir().join(format!(
                "zemer-aifc-dwvw-stereo-{bits}-{channel}-{}.aifc",
                std::process::id()
            ));
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&mono_path)
                .unwrap()
                .write_all(&mono_file)
                .unwrap();
            let decoded = Command::new(&python)
                .arg(&decoder_script)
                .arg(&mono_path)
                .output();
            std::fs::remove_file(&mono_path).unwrap();
            let decoded = decoded.unwrap();
            assert!(
                decoded.status.success(),
                "libsndfile failed to decode Zemer {bits}-bit channel {channel}: {}",
                String::from_utf8_lossy(&decoded.stderr)
            );
            let expected_s32: Vec<u8> = channel_samples
                .iter()
                .flat_map(|&sample| (sample << (32 - bits)).to_le_bytes())
                .collect();
            assert_eq!(
                decoded.stdout, expected_s32,
                "Zemer writer channel {channel}"
            );
            channel_start_bit = channel_end_bit;
        }
        assert_eq!(channel_start_bit, (writer_payload.len() as u64) * 8);

        let mut sound = vec![0; 8];
        sound.extend_from_slice(&expected_payload);
        let independent = replace_aiff_chunk_body(&template, b"SSND", &sound);
        let mut decoder = AiffDecoder::new(Cursor::new(independent)).unwrap();
        assert_eq!(decoder.spec(), spec);
        let mut actual = Vec::new();
        let mut frames = 0;
        while let Some(block) = decoder.read_frames(37).unwrap() {
            frames += block.frames();
            append_pcm_bytes(block.samples(), &mut actual);
        }
        let mut expected_pcm = Vec::new();
        append_pcm_bytes(input.samples(), &mut expected_pcm);
        assert_eq!(frames, 257);
        assert_eq!(
            actual, expected_pcm,
            "{bits}-bit stereo DWVW decoder oracle"
        );
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

#[test]
#[ignore = "requires Audacity DWVW sample and libsndfile 1.2.2 S32LE oracle paths in ZEMER_AIFC_DWVW_*"]
fn decodes_audacity_24_bit_dwvw_against_libsndfile_oracle() {
    let sample = std::env::var_os("ZEMER_AIFC_DWVW_SAMPLE")
        .expect("set ZEMER_AIFC_DWVW_SAMPLE to lena-24-dwvw.aiff");
    let oracle = std::env::var_os("ZEMER_AIFC_DWVW_PCM_ORACLE")
        .expect("set ZEMER_AIFC_DWVW_PCM_ORACLE to libsndfile S32LE PCM");
    let oracle = std::fs::read(oracle).unwrap();
    assert_eq!(oracle.len(), 541_184 * 4);

    let mut decoder = AiffDecoder::new(File::open(sample).unwrap()).unwrap();
    assert!(decoder.is_aifc());
    assert_eq!(decoder.spec().sample_rate(), 44_100);
    assert_eq!(decoder.spec().channels(), 1);
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI24);

    let mut decoded = Vec::with_capacity(oracle.len());
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(113).unwrap() {
        let Samples::I24(samples) = block.samples() else {
            panic!("AIFC DWVW sample format changed during decode");
        };
        frames += block.frames();
        for sample in samples {
            decoded.extend_from_slice(&sample.wrapping_shl(8).to_le_bytes());
        }
    }
    assert_eq!(frames, 541_184);
    assert_eq!(decoded, oracle, "Zemer and libsndfile DWVW decoders differ");
}
