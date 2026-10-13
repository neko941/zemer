use std::{
    fs::File,
    io::{BufReader, Cursor, Read},
};

use flac::{OggFlacDecoder, rewrite_ogg_metadata};
use zemer_core::{AudioDecoder, AudioSpec, Metadata, SampleFormat, Samples};

const FRAMES: u64 = 24_000;

#[test]
fn decodes_ffmpeg_ogg_flac_pages_bit_exactly_in_small_reads() {
    let actual = decode_i24_fixture(
        include_bytes!("fixtures/ogg-flac-ffmpeg.oga"),
        44_100,
        1,
        FRAMES,
    );
    assert_eq!(actual, (0..FRAMES).map(expected_sample).collect::<Vec<_>>());
}

#[test]
fn decodes_ffmpeg_stereo_ogg_flac_exactly() {
    const FRAMES: u64 = 8_197;
    let mut decoder = OggFlacDecoder::new(Cursor::new(include_bytes!(
        "fixtures/ogg-flac-ffmpeg-stereo-s16.oga"
    )))
    .unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap()
    );
    let mut actual = Vec::new();
    while let Some(block) = decoder.read_frames(113).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("FFmpeg stereo Ogg-FLAC fixture must decode to signed 16-bit PCM");
        };
        actual.extend_from_slice(samples);
    }
    assert!(decoder.read_frames(1).unwrap().is_none());
    assert_eq!(actual.len(), (FRAMES * 2) as usize);
    assert_eq!(
        actual,
        (0..FRAMES * 2)
            .map(expected_stereo_sample)
            .collect::<Vec<_>>()
    );
}

#[test]
fn decodes_long_ffmpeg_5_1_packets_across_ogg_pages() {
    const FRAMES: u64 = 8_197;
    let actual = decode_i24_fixture(
        include_bytes!("fixtures/ogg-flac-ffmpeg-5_1-s24.oga"),
        96_000,
        6,
        FRAMES,
    );
    assert_eq!(
        actual,
        (0..FRAMES * 6)
            .map(expected_six_channel_sample)
            .collect::<Vec<_>>()
    );
}

#[test]
fn decodes_ffmpeg_7_1_24_bit_at_192khz() {
    const FRAMES: u64 = 8_197;
    let actual = decode_i24_fixture(
        include_bytes!("fixtures/ogg-flac-ffmpeg-7_1-s24-192k.oga"),
        192_000,
        8,
        FRAMES,
    );
    assert_eq!(
        actual,
        (0..FRAMES * 8)
            .map(expected_eight_channel_sample)
            .collect::<Vec<_>>()
    );
}

#[test]
fn decodes_libflac_reference_u8_mono_at_8khz() {
    const FRAMES: u64 = 9_001;
    let mut decoder = OggFlacDecoder::new(Cursor::new(include_bytes!(
        "fixtures/ogg-flac-reference-u8-mono-8k.oga"
    )))
    .unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap()
    );
    let mut actual = Vec::new();
    while let Some(block) = decoder.read_frames(173).unwrap() {
        let Samples::U8(samples) = block.samples() else {
            panic!("libFLAC 8-bit Ogg-FLAC fixture must decode to unsigned 8-bit PCM");
        };
        actual.extend_from_slice(samples);
    }
    assert_eq!(actual.len(), FRAMES as usize);
    assert_eq!(
        actual,
        (0..FRAMES)
            .map(|frame| ((frame * 73 + 19) % 256) as u8)
            .collect::<Vec<_>>()
    );
}

#[test]
fn decodes_libflac_reference_i32_7_1_at_192khz() {
    const FRAMES: u64 = 5_003;
    const CHANNELS: u64 = 8;
    let mut decoder = OggFlacDecoder::new(Cursor::new(include_bytes!(
        "fixtures/ogg-flac-reference-i32-7_1-192k.oga"
    )))
    .unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(192_000, CHANNELS as u16, SampleFormat::PcmI32).unwrap()
    );
    let mut actual = Vec::new();
    while let Some(block) = decoder.read_frames(131).unwrap() {
        let Samples::I32(samples) = block.samples() else {
            panic!("libFLAC 32-bit Ogg-FLAC fixture must decode to signed 32-bit PCM");
        };
        actual.extend_from_slice(samples);
    }
    assert_eq!(actual.len(), (FRAMES * CHANNELS) as usize);
    assert_eq!(
        actual,
        (0..FRAMES * CHANNELS)
            .map(|index| (index as u32)
                .wrapping_mul(1_000_003)
                .wrapping_add(0x0123_4567) as i32)
            .collect::<Vec<_>>()
    );
}

#[test]
#[ignore = "requires the FFmpeg Ogg-FLAC sample and PCM oracle paths in ZEMER_OGG_FLAC_SAMPLE and ZEMER_OGG_FLAC_PCM"]
fn decodes_external_ogg_flac_against_ffmpeg_pcm_oracle() {
    const FRAMES: u64 = 3_730_860;
    let input = std::env::var_os("ZEMER_OGG_FLAC_SAMPLE")
        .expect("set ZEMER_OGG_FLAC_SAMPLE to the external Ogg-FLAC sample");
    let oracle = std::env::var_os("ZEMER_OGG_FLAC_PCM")
        .expect("set ZEMER_OGG_FLAC_PCM to FFmpeg's s16le PCM output");
    let mut decoder = OggFlacDecoder::new(File::open(&input).unwrap()).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap()
    );
    let mut oracle_pcm = BufReader::new(File::open(&oracle).unwrap());
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(2_048).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("external FFmpeg Ogg-FLAC sample must decode to signed 16-bit PCM");
        };
        let mut bytes = vec![0; samples.len() * 2];
        oracle_pcm.read_exact(&mut bytes).unwrap();
        for (sample, expected) in samples.iter().zip(bytes.chunks_exact(2)) {
            assert_eq!(*sample, i16::from_le_bytes([expected[0], expected[1]]));
        }
        frames += (samples.len() / 2) as u64;
    }
    assert_eq!(frames, FRAMES);
    let mut trailing = [0; 1];
    assert_eq!(
        oracle_pcm.read(&mut trailing).unwrap(),
        0,
        "PCM oracle has trailing samples"
    );
}

#[test]
#[ignore = "requires the external Commons Ogg-FLAC sample and libFLAC PCM oracle paths in ZEMER_OGG_FLAC_COMMONS_SAMPLE and ZEMER_OGG_FLAC_COMMONS_PCM"]
fn decodes_external_commons_ogg_flac_against_libflac_pcm_oracle() {
    const FRAMES: u64 = 8_731_800;
    let input = std::env::var_os("ZEMER_OGG_FLAC_COMMONS_SAMPLE")
        .expect("set ZEMER_OGG_FLAC_COMMONS_SAMPLE to the external Ogg-FLAC sample");
    let oracle = std::env::var_os("ZEMER_OGG_FLAC_COMMONS_PCM")
        .expect("set ZEMER_OGG_FLAC_COMMONS_PCM to libFLAC's s16le PCM output");
    let mut decoder = OggFlacDecoder::new(File::open(input).unwrap()).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap()
    );
    let mut oracle = BufReader::new(File::open(oracle).unwrap());
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(2_048).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("external Commons Ogg-FLAC sample must decode to signed 16-bit PCM");
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
        "libFLAC PCM oracle has trailing samples"
    );
}

#[test]
#[ignore = "requires the CC BY-SA 4.0 Chopin Ogg-FLAC sample and reference libFLAC PCM oracle paths in ZEMER_OGG_FLAC_CHOPIN_SAMPLE and ZEMER_OGG_FLAC_CHOPIN_PCM"]
fn decodes_external_chopin_ogg_flac_against_libflac_pcm_oracle() {
    const FRAMES: u64 = 5_363_663;
    let input = std::env::var_os("ZEMER_OGG_FLAC_CHOPIN_SAMPLE")
        .expect("set ZEMER_OGG_FLAC_CHOPIN_SAMPLE to the external Ogg-FLAC sample");
    let oracle = std::env::var_os("ZEMER_OGG_FLAC_CHOPIN_PCM")
        .expect("set ZEMER_OGG_FLAC_CHOPIN_PCM to libFLAC's s16le PCM output");
    let mut decoder = OggFlacDecoder::new(File::open(input).unwrap()).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap()
    );
    let mut oracle = BufReader::new(File::open(oracle).unwrap());
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(2_048).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("external Chopin Ogg-FLAC sample must decode to signed 16-bit PCM");
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
        "libFLAC PCM oracle has trailing samples"
    );
}

#[test]
#[ignore = "requires the public-domain 4m33s Ogg-FLAC sample and libFLAC PCM oracle paths in ZEMER_OGG_FLAC_4M33S_SAMPLE and ZEMER_OGG_FLAC_4M33S_PCM"]
fn decodes_external_public_domain_4m33s_ogg_flac_against_libflac_pcm_oracle() {
    const FRAMES: u64 = 13_104_000;
    let input = std::env::var_os("ZEMER_OGG_FLAC_4M33S_SAMPLE")
        .expect("set ZEMER_OGG_FLAC_4M33S_SAMPLE to the external Ogg-FLAC sample");
    let oracle = std::env::var_os("ZEMER_OGG_FLAC_4M33S_PCM")
        .expect("set ZEMER_OGG_FLAC_4M33S_PCM to libFLAC's signed 24-bit PCM output");
    let mut decoder = OggFlacDecoder::new(File::open(input).unwrap()).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(48_000, 1, SampleFormat::PcmI24).unwrap()
    );
    let mut oracle = BufReader::new(File::open(oracle).unwrap());
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(2_048).unwrap() {
        let Samples::I24(samples) = block.samples() else {
            panic!("external 4m33s Ogg-FLAC sample must decode to signed 24-bit PCM");
        };
        let mut bytes = vec![0; samples.len() * 3];
        oracle.read_exact(&mut bytes).unwrap();
        for (sample, expected) in samples.iter().zip(bytes.chunks_exact(3)) {
            let sign = if expected[2] & 0x80 == 0 { 0 } else { 0xff };
            assert_eq!(
                *sample,
                i32::from_le_bytes([expected[0], expected[1], expected[2], sign])
            );
        }
        frames += samples.len() as u64;
    }
    assert_eq!(frames, FRAMES);
    let mut trailing = [0; 1];
    assert_eq!(
        oracle.read(&mut trailing).unwrap(),
        0,
        "libFLAC PCM oracle has trailing samples"
    );
}

#[test]
#[ignore = "requires the NASA public-domain Ogg-FLAC sample and libFLAC PCM oracle paths in ZEMER_OGG_FLAC_NASA_SAMPLE and ZEMER_OGG_FLAC_NASA_PCM"]
fn decodes_external_nasa_martian_dust_devil_against_libflac_pcm_oracle() {
    const FRAMES: u64 = 1_295_695;
    let input = std::env::var_os("ZEMER_OGG_FLAC_NASA_SAMPLE")
        .expect("set ZEMER_OGG_FLAC_NASA_SAMPLE to the external Ogg-FLAC sample");
    let oracle = std::env::var_os("ZEMER_OGG_FLAC_NASA_PCM")
        .expect("set ZEMER_OGG_FLAC_NASA_PCM to libFLAC's s16le PCM output");
    let mut decoder = OggFlacDecoder::new(File::open(&input).unwrap()).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap()
    );
    let mut oracle_pcm = BufReader::new(File::open(&oracle).unwrap());
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(2_048).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("NASA Ogg-FLAC sample must decode to signed 16-bit PCM");
        };
        let mut bytes = vec![0; samples.len() * 2];
        oracle_pcm.read_exact(&mut bytes).unwrap();
        for (sample, expected) in samples.iter().zip(bytes.chunks_exact(2)) {
            assert_eq!(*sample, i16::from_le_bytes([expected[0], expected[1]]));
        }
        frames += (samples.len() / 2) as u64;
    }
    assert_eq!(frames, FRAMES);
    let mut trailing = [0; 1];
    assert_eq!(
        oracle_pcm.read(&mut trailing).unwrap(),
        0,
        "libFLAC PCM oracle has trailing samples"
    );

    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "NASA Ogg-FLAC rewrite regression");
    let mut rewritten = Vec::new();
    rewrite_ogg_metadata(File::open(&input).unwrap(), &mut rewritten, &metadata).unwrap();
    let mut decoder = OggFlacDecoder::new(Cursor::new(rewritten)).unwrap();
    assert_eq!(
        decoder.metadata().values("TITLE"),
        ["NASA Ogg-FLAC rewrite regression"]
    );
    let mut oracle_pcm = BufReader::new(File::open(&oracle).unwrap());
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(2_048).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("rewritten NASA Ogg-FLAC sample must remain signed 16-bit PCM");
        };
        let mut bytes = vec![0; samples.len() * 2];
        oracle_pcm.read_exact(&mut bytes).unwrap();
        for (sample, expected) in samples.iter().zip(bytes.chunks_exact(2)) {
            assert_eq!(*sample, i16::from_le_bytes([expected[0], expected[1]]));
        }
        frames += (samples.len() / 2) as u64;
    }
    assert_eq!(frames, FRAMES);
    let mut trailing = [0; 1];
    assert_eq!(oracle_pcm.read(&mut trailing).unwrap(), 0);
}

#[test]
#[ignore = "requires the public-domain Wapichan Ogg-FLAC sample and libFLAC PCM oracle paths in ZEMER_OGG_FLAC_WAPICHAN_SAMPLE and ZEMER_OGG_FLAC_WAPICHAN_PCM"]
fn decodes_external_public_domain_wapichan_speech_against_libflac_pcm_oracle() {
    const FRAMES: u64 = 2_867_200;
    let input = std::env::var_os("ZEMER_OGG_FLAC_WAPICHAN_SAMPLE")
        .expect("set ZEMER_OGG_FLAC_WAPICHAN_SAMPLE to the external Ogg-FLAC sample");
    let oracle = std::env::var_os("ZEMER_OGG_FLAC_WAPICHAN_PCM")
        .expect("set ZEMER_OGG_FLAC_WAPICHAN_PCM to libFLAC's s24le PCM output");
    let mut decoder = OggFlacDecoder::new(File::open(input).unwrap()).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(44_100, 2, SampleFormat::PcmI24).unwrap()
    );
    let mut oracle = BufReader::new(File::open(oracle).unwrap());
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(2_048).unwrap() {
        let Samples::I24(samples) = block.samples() else {
            panic!("Wapichan Ogg-FLAC sample must decode to signed 24-bit PCM");
        };
        let mut bytes = vec![0; samples.len() * 3];
        oracle.read_exact(&mut bytes).unwrap();
        for (sample, expected) in samples.iter().zip(bytes.chunks_exact(3)) {
            let sign = if expected[2] & 0x80 == 0 { 0 } else { 0xff };
            assert_eq!(
                *sample,
                i32::from_le_bytes([expected[0], expected[1], expected[2], sign])
            );
        }
        frames += (samples.len() / 2) as u64;
    }
    assert_eq!(frames, FRAMES);
    let mut trailing = [0; 1];
    assert_eq!(
        oracle.read(&mut trailing).unwrap(),
        0,
        "libFLAC PCM oracle has trailing samples"
    );
}

#[test]
#[ignore = "requires both pre-1.1.1 Ogg-FLAC samples and libFLAC PCM oracles in ZEMER_OGG_FLAC_LEGACY_* environment variables"]
fn decodes_legacy_ogg_flac_against_libflac_pcm_oracle() {
    for (sample_var, oracle_var, expected_frames) in [
        (
            "ZEMER_OGG_FLAC_LEGACY_SAMPLE",
            "ZEMER_OGG_FLAC_LEGACY_PCM",
            5_583_744,
        ),
        (
            "ZEMER_OGG_FLAC_LEGACY_SECOND_SAMPLE",
            "ZEMER_OGG_FLAC_LEGACY_SECOND_PCM",
            9_062_784,
        ),
    ] {
        let input = std::env::var_os(sample_var).expect("set the external legacy sample path");
        let oracle = std::env::var_os(oracle_var).expect("set the libFLAC PCM oracle path");
        let mut decoder = OggFlacDecoder::new(File::open(&input).unwrap()).unwrap();
        assert_eq!(
            decoder.spec(),
            AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap()
        );
        let mut oracle = BufReader::new(File::open(oracle).unwrap());
        let mut frames = 0;
        while let Some(block) = decoder.read_frames(2_048).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("legacy Ogg-FLAC sample must decode to signed 16-bit PCM");
            };
            let mut bytes = vec![0; samples.len() * 2];
            oracle.read_exact(&mut bytes).unwrap();
            for (sample, expected) in samples.iter().zip(bytes.chunks_exact(2)) {
                assert_eq!(*sample, i16::from_le_bytes([expected[0], expected[1]]));
            }
            frames += (samples.len() / 2) as u64;
        }
        assert_eq!(frames, expected_frames);
        let mut trailing = [0; 1];
        assert_eq!(
            oracle.read(&mut trailing).unwrap(),
            0,
            "libFLAC PCM oracle has trailing samples"
        );

        let mut metadata = Metadata::default();
        metadata.insert("TITLE", "rewritten legacy stream");
        let mut rewritten = Vec::new();
        rewrite_ogg_metadata(File::open(&input).unwrap(), &mut rewritten, &metadata).unwrap();
        let mut decoder = OggFlacDecoder::new(Cursor::new(rewritten)).unwrap();
        assert_eq!(
            decoder.metadata().values("TITLE"),
            ["rewritten legacy stream"]
        );
        let mut rewritten_frames = 0;
        while let Some(block) = decoder.read_frames(2_048).unwrap() {
            rewritten_frames += block.frames() as u64;
        }
        assert_eq!(rewritten_frames, expected_frames);
    }
}

#[test]
fn rewrites_tags_in_ogg_flac_with_a_continued_audio_packet() {
    let input = include_bytes!("fixtures/ogg-flac-ffmpeg-5_1-s24.oga");
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "updated without audio re-encoding");
    let mut output = Vec::new();
    rewrite_ogg_metadata(Cursor::new(input), &mut output, &metadata).unwrap();
    assert_eq!(
        decode_i24_fixture(&output, 96_000, 6, 8_197),
        (0..8_197 * 6)
            .map(expected_six_channel_sample)
            .collect::<Vec<_>>()
    );
    let decoder = OggFlacDecoder::new(Cursor::new(output)).unwrap();
    assert_eq!(
        decoder.metadata().values("TITLE"),
        ["updated without audio re-encoding"]
    );
}

fn decode_i24_fixture(bytes: &[u8], sample_rate: u32, channels: u16, frames: u64) -> Vec<i32> {
    let mut decoder = OggFlacDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(sample_rate, channels, SampleFormat::PcmI24).unwrap()
    );
    let mut actual = Vec::new();
    while let Some(block) = decoder.read_frames(257).unwrap() {
        let Samples::I24(samples) = block.samples() else {
            panic!("FFmpeg Ogg-FLAC fixture must decode to signed 24-bit PCM");
        };
        actual.extend_from_slice(samples);
    }
    assert!(decoder.read_frames(1).unwrap().is_none());
    assert_eq!(actual.len(), (frames * u64::from(channels)) as usize);
    actual
}

fn expected_sample(index: u64) -> i32 {
    const LIMIT: u64 = 1 << 24;
    const HALF: i64 = 1 << 23;
    match index {
        0 => -(HALF as i32),
        1 => (HALF - 1) as i32,
        2 => -1,
        3 => 0,
        4 => 1,
        _ => ((index * 104_729 + (index / 7) * 1_048_573) % LIMIT) as i32 - HALF as i32,
    }
}

fn expected_stereo_sample(index: u64) -> i16 {
    const LIMIT: u64 = 1 << 16;
    const HALF: i64 = 1 << 15;
    match index {
        0 => (-HALF) as i16,
        1 => (HALF - 1) as i16,
        2 => -1,
        3 => 0,
        4 => 1,
        _ => (((index * 31_337 + (index / 11) * 7_919) % LIMIT) as i64 - HALF) as i16,
    }
}

fn expected_six_channel_sample(index: u64) -> i32 {
    const LIMIT: u64 = 1 << 24;
    const HALF: i64 = 1 << 23;
    match index {
        0 => -(HALF as i32),
        1 => (HALF - 1) as i32,
        2 => -1,
        3 => 0,
        4 => 1,
        _ => {
            let frame = index / 6;
            let channel = index % 6;
            ((frame * 104_729 + channel * 65_537 + (frame / 7) * 1_048_573) % LIMIT) as i32
                - HALF as i32
        }
    }
}

fn expected_eight_channel_sample(index: u64) -> i32 {
    const LIMIT: u64 = 1 << 24;
    const HALF: i64 = 1 << 23;
    match index {
        0 => -(HALF as i32),
        1 => (HALF - 1) as i32,
        2 => -1,
        3 => 0,
        4 => 1,
        _ => ((index * 104_729 + (index / 11) * 7_919) % LIMIT) as i32 - HALF as i32,
    }
}
