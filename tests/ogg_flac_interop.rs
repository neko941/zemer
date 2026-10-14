use std::{
    fs::{self, File},
    io::{BufReader, Cursor, Read},
    process::Command,
};

use flac::{OggFlacDecoder, OggFlacEncoder, rewrite_ogg_metadata};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples,
};

const FRAMES: u64 = 24_000;

#[test]
#[ignore = "requires FFmpeg at ZEMER_FFMPEG"]
fn zemer_ogg_flac_encodings_decode_exactly_with_ffmpeg() {
    let ffmpeg = std::env::var_os("ZEMER_FFMPEG").expect("set ZEMER_FFMPEG to the FFmpeg binary");
    let profiles = [
        (8_000, 1, SampleFormat::PcmU8, "u8", "pcm_u8"),
        (48_000, 2, SampleFormat::PcmI16, "s16le", "pcm_s16le"),
        (96_000, 6, SampleFormat::PcmI24, "s24le", "pcm_s24le"),
        (192_000, 8, SampleFormat::PcmI32, "s32le", "pcm_s32le"),
    ];

    for (case, (rate, channels, format, raw_format, codec)) in profiles.into_iter().enumerate() {
        const FRAMES: usize = 8_197;
        let spec = AudioSpec::new(rate, channels, format).unwrap();
        let count = FRAMES * usize::from(channels);
        let samples = match format {
            SampleFormat::PcmU8 => Samples::U8((0..count).map(|i| i as u8).collect()),
            SampleFormat::PcmI16 => Samples::I16(
                (0..count)
                    .map(|i| (i as i32 * 7_919 - 32_768) as i16)
                    .collect(),
            ),
            SampleFormat::PcmI24 => Samples::I24(
                (0..count)
                    .map(|i| ((i as i64 * 104_729) % (1 << 24) - (1 << 23)) as i32)
                    .collect(),
            ),
            SampleFormat::PcmI32 => Samples::I32(
                (0..count)
                    .map(|i| (i as u32).wrapping_mul(1_000_003).wrapping_add(0x1234_5678) as i32)
                    .collect(),
            ),
            SampleFormat::Float32 | SampleFormat::Float64 => unreachable!(),
        };
        let expected = raw_pcm_bytes(&samples);
        let mut encoder = OggFlacEncoder::new(Vec::new(), spec).unwrap();
        encoder
            .write_block(&PcmBuffer::new(spec, samples).unwrap())
            .unwrap();
        encoder.finish().unwrap();

        let path = std::env::temp_dir().join(format!(
            "zemer-ogg-flac-encode-{}-{case}.oga",
            std::process::id()
        ));
        fs::write(&path, encoder.into_inner()).unwrap();
        let decoded = Command::new(&ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args([
                "-map", "0:a:0", "-f", raw_format, "-acodec", codec, "pipe:1",
            ])
            .output()
            .unwrap();
        let _ = fs::remove_file(path);
        assert!(
            decoded.status.success(),
            "FFmpeg failed to decode {rate}-Hz {channels}-channel {format:?}: {}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        assert_eq!(decoded.stdout, expected, "profile {format:?} at {rate} Hz");
    }
}

fn raw_pcm_bytes(samples: &Samples) -> Vec<u8> {
    match samples {
        Samples::U8(values) => values.clone(),
        Samples::I16(values) => values
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect(),
        Samples::I24(values) => values
            .iter()
            .flat_map(|sample| {
                let bytes = sample.to_le_bytes();
                [bytes[0], bytes[1], bytes[2]]
            })
            .collect(),
        Samples::I32(values) => values
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect(),
        Samples::F32(_) | Samples::F64(_) => unreachable!(),
    }
}

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
fn seeks_across_sparse_multichannel_flac_subframes() {
    const FRAMES: u64 = 131_072;
    const CHANNELS: u16 = 8;
    const RUN: u64 = 16_384;
    const PCM_FNV1A64: u64 = 0x7613_b95f_079d_d482;

    let input = include_bytes!("fixtures/ogg-flac-sparse-7_1.oga");
    let (frames, digest) = decode_s16le_pcm_digest(input, 48_000, CHANNELS, None);
    assert_eq!(frames, FRAMES);
    assert_eq!(digest, PCM_FNV1A64);

    let mut decoder = OggFlacDecoder::new_seekable(Cursor::new(input)).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(48_000, CHANNELS, SampleFormat::PcmI16).unwrap()
    );
    let mut targets = vec![0, FRAMES - 23];
    for channel in 1..u64::from(CHANNELS) {
        targets.extend([channel * RUN - 11, channel * RUN]);
    }
    for target in targets {
        decoder.seek_to_sample(target).unwrap();
        let block = decoder.read_frames(23).unwrap().unwrap();
        assert_eq!(block.frames(), 23.min((FRAMES - target) as usize));
        let Samples::I16(samples) = block.samples() else {
            panic!("sparse 7.1 Ogg-FLAC fixture must decode to signed 16-bit PCM");
        };
        for (offset, actual) in samples.iter().enumerate() {
            let frame = target + (offset / usize::from(CHANNELS)) as u64;
            let channel = (offset % usize::from(CHANNELS)) as u64;
            let expected = if frame / RUN == channel {
                ((frame * 7_919 + channel * 104_729) % 60_001) as i32 - 30_000
            } else {
                0
            };
            assert_eq!(
                i32::from(*actual),
                expected,
                "frame {frame}, channel {channel}"
            );
        }
    }
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
fn decodes_ffmpeg_ogg_flac_with_three_through_seven_channels() {
    const FRAMES: u64 = 8_197;
    let profiles: [(u16, &[u8]); 4] = [
        (3, include_bytes!("fixtures/ogg-flac-ffmpeg-3ch-s24.oga")),
        (4, include_bytes!("fixtures/ogg-flac-ffmpeg-4ch-s24.oga")),
        (5, include_bytes!("fixtures/ogg-flac-ffmpeg-5ch-s24.oga")),
        (7, include_bytes!("fixtures/ogg-flac-ffmpeg-7ch-s24.oga")),
    ];

    for (channels, fixture) in profiles {
        let actual = decode_i24_fixture(fixture, 44_100, channels, FRAMES);
        assert_eq!(
            actual,
            (0..FRAMES * u64::from(channels))
                .map(|index| expected_multichannel_sample(index, u64::from(channels)))
                .collect::<Vec<_>>(),
            "{channels}-channel profile"
        );
    }
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
fn decodes_archived_commons_white_noise_ogg_flac_against_ffmpeg() {
    const FRAMES: u64 = 480_000;
    let mut decoder = OggFlacDecoder::new(Cursor::new(include_bytes!(
        "fixtures/ogg-flac-white-noise.oga"
    )))
    .unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap()
    );
    let mut oracle = Cursor::new(include_bytes!("fixtures/ogg-flac-white-noise.s16le"));
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(2_048).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("Commons white-noise Ogg-FLAC must decode to signed 16-bit PCM");
        };
        let mut bytes = vec![0; samples.len() * 2];
        oracle.read_exact(&mut bytes).unwrap();
        for (sample, expected) in samples.iter().zip(bytes.chunks_exact(2)) {
            assert_eq!(*sample, i16::from_le_bytes([expected[0], expected[1]]));
        }
        frames += samples.len() as u64;
    }
    assert_eq!(frames, FRAMES);
    let mut trailing = [0; 1];
    assert_eq!(oracle.read(&mut trailing).unwrap(), 0);
}

#[test]
#[ignore = "requires the CC BY-SA Commons Ogg-FLAC sample and FFmpeg PCM oracle paths in ZEMER_OGG_FLAC_DISSONANCE_SAMPLE and ZEMER_OGG_FLAC_DISSONANCE_PCM"]
fn decodes_commons_libflac_1_2_1_dissonance_against_ffmpeg() {
    const FRAMES: u64 = 1_440_000;
    let input = std::env::var_os("ZEMER_OGG_FLAC_DISSONANCE_SAMPLE")
        .expect("set ZEMER_OGG_FLAC_DISSONANCE_SAMPLE to the Commons Ogg-FLAC file");
    let oracle = std::env::var_os("ZEMER_OGG_FLAC_DISSONANCE_PCM")
        .expect("set ZEMER_OGG_FLAC_DISSONANCE_PCM to FFmpeg's s24le PCM output");
    let mut decoder = OggFlacDecoder::new(File::open(input).unwrap()).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(48_000, 2, SampleFormat::PcmI24).unwrap()
    );
    let mut oracle = BufReader::new(File::open(oracle).unwrap());
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(2_048).unwrap() {
        let Samples::I24(samples) = block.samples() else {
            panic!("Commons dissonance Ogg-FLAC must decode to signed 24-bit PCM");
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
    assert_eq!(oracle.read(&mut trailing).unwrap(), 0);
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
fn decodes_public_domain_4m33s_ogg_flac_fixture() {
    const FRAMES: u64 = 13_104_000;
    let mut decoder =
        OggFlacDecoder::new(Cursor::new(include_bytes!("fixtures/ogg-flac-4m33s.oga"))).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(48_000, 1, SampleFormat::PcmI24).unwrap()
    );
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(2_048).unwrap() {
        let Samples::I24(samples) = block.samples() else {
            panic!("4m33s Ogg-FLAC fixture must decode to signed 24-bit PCM");
        };
        assert!(samples.iter().all(|&sample| sample == 0));
        frames += samples.len() as u64;
    }
    assert_eq!(frames, FRAMES);
    assert!(decoder.read_frames(1).unwrap().is_none());
}

#[test]
fn decodes_and_rewrites_nasa_martian_dust_devil_fixture() {
    const FRAMES: u64 = 1_295_695;
    const PCM_FNV1A64: u64 = 0xcb02_a204_7c6f_0a19;
    const TITLE: &str = "NASA Ogg-FLAC rewrite regression";

    let input = include_bytes!("fixtures/ogg-flac-nasa-dust-devil.oga");
    let (frames, digest) = decode_s16le_pcm_digest(input, 48_000, 2, None);
    assert_eq!(frames, FRAMES);
    assert_eq!(digest, PCM_FNV1A64);

    let mut metadata = Metadata::default();
    metadata.insert("TITLE", TITLE);
    let mut rewritten = Vec::new();
    rewrite_ogg_metadata(Cursor::new(input), &mut rewritten, &metadata).unwrap();
    let (frames, digest) = decode_s16le_pcm_digest(&rewritten, 48_000, 2, Some(TITLE));
    assert_eq!(frames, FRAMES);
    assert_eq!(digest, PCM_FNV1A64);
}

#[test]
fn decodes_and_rewrites_cc0_gymnopedie_ogg_flac_fixture() {
    const FRAMES: u64 = 3_442_678;
    const PCM_FNV1A64: u64 = 0x1b94_80cf_175d_6580;
    const TITLE: &str = "CC0 Gymnopedie Ogg-FLAC rewrite regression";

    let input = include_bytes!("fixtures/ogg-flac-gymnopedie.oga");
    let (frames, digest) = decode_s16le_pcm_digest(input, 22_050, 2, None);
    assert_eq!(frames, FRAMES);
    assert_eq!(digest, PCM_FNV1A64);

    let mut metadata = Metadata::default();
    metadata.insert("TITLE", TITLE);
    let mut rewritten = Vec::new();
    rewrite_ogg_metadata(Cursor::new(input), &mut rewritten, &metadata).unwrap();
    let (frames, digest) = decode_s16le_pcm_digest(&rewritten, 22_050, 2, Some(TITLE));
    assert_eq!(frames, FRAMES);
    assert_eq!(digest, PCM_FNV1A64);
}

#[test]
fn decodes_and_rewrites_cc0_david_allen_speech_ogg_flac_fixture() {
    const FRAMES: u64 = 561_254;
    const PCM_FNV1A64: u64 = 0x66d7_3cd4_a324_cdb1;
    const TITLE: &str = "CC0 David Allen speech rewrite regression";

    let input = include_bytes!("fixtures/ogg-flac-david-allen-voice.oga");
    let (frames, digest) = decode_s16le_pcm_digest(input, 44_100, 2, None);
    assert_eq!(frames, FRAMES);
    assert_eq!(digest, PCM_FNV1A64);

    let mut metadata = Metadata::default();
    metadata.insert("TITLE", TITLE);
    let mut rewritten = Vec::new();
    rewrite_ogg_metadata(Cursor::new(input), &mut rewritten, &metadata).unwrap();
    let (frames, digest) = decode_s16le_pcm_digest(&rewritten, 44_100, 2, Some(TITLE));
    assert_eq!(frames, FRAMES);
    assert_eq!(digest, PCM_FNV1A64);
}

#[test]
fn decodes_and_rewrites_libflac_1_3_0_cc_by_sa_toilet_flush_fixture() {
    const FRAMES: u64 = 317_827;
    const PCM_FNV1A64: u64 = 0xf534_fae5_f393_257a;
    const TITLE: &str = "libFLAC 1.3.0 Ogg-FLAC rewrite regression";

    let input = include_bytes!("fixtures/ogg-flac-toilet-flush.oga");
    let (frames, digest) = decode_s16le_pcm_digest(input, 44_100, 2, None);
    assert_eq!(frames, FRAMES);
    assert_eq!(digest, PCM_FNV1A64);

    let mut metadata = Metadata::default();
    metadata.insert("TITLE", TITLE);
    let mut rewritten = Vec::new();
    rewrite_ogg_metadata(Cursor::new(input), &mut rewritten, &metadata).unwrap();
    let (frames, digest) = decode_s16le_pcm_digest(&rewritten, 44_100, 2, Some(TITLE));
    assert_eq!(frames, FRAMES);
    assert_eq!(digest, PCM_FNV1A64);
}

#[test]
fn chains_independent_ogg_flac_links_with_different_specs_and_rewrites_first() {
    const FIRST_FRAMES: u64 = 317_827;
    const FIRST_DIGEST: u64 = 0xf534_fae5_f393_257a;
    const SECOND_FRAMES: u64 = 8_197;
    const TITLE: &str = "rewritten first link only";

    let first = include_bytes!("fixtures/ogg-flac-toilet-flush.oga");
    let second = include_bytes!("fixtures/ogg-flac-ffmpeg-stereo-s16.oga");
    let serial = |stream: &[u8]| u32::from_le_bytes(stream[14..18].try_into().unwrap());
    assert_ne!(serial(first), serial(second));

    let mut chained = Vec::with_capacity(first.len() + second.len());
    chained.extend_from_slice(first);
    chained.extend_from_slice(second);
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", TITLE);
    let mut rewritten = Vec::new();
    rewrite_ogg_metadata(Cursor::new(chained.clone()), &mut rewritten, &metadata).unwrap();

    let first_spec = AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap();
    let second_spec = AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap();
    let mut decoder = OggFlacDecoder::new(Cursor::new(rewritten)).unwrap();
    assert_eq!(decoder.spec(), first_spec);
    assert_eq!(decoder.metadata().values("TITLE"), [TITLE]);

    let mut first_frames = 0;
    let mut second_frames = 0;
    let mut first_digest = 0xcbf2_9ce4_8422_2325_u64;
    while let Some(block) = decoder.read_frames(4_096).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("chained Ogg-FLAC links must decode to signed 16-bit PCM");
        };
        if block.spec() == first_spec {
            for byte in samples.iter().flat_map(|sample| sample.to_le_bytes()) {
                first_digest = (first_digest ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
            }
            first_frames += block.frames() as u64;
        } else {
            assert_eq!(block.spec(), second_spec);
            for (offset, sample) in samples.iter().enumerate() {
                assert_eq!(
                    *sample,
                    expected_stereo_sample(second_frames * 2 + offset as u64)
                );
            }
            second_frames += block.frames() as u64;
        }
    }
    assert_eq!(first_frames, FIRST_FRAMES);
    assert_eq!(first_digest, FIRST_DIGEST);
    assert_eq!(second_frames, SECOND_FRAMES);
    assert!(decoder.read_frames(1).unwrap().is_none());

    let mut seeker = OggFlacDecoder::new_seekable(Cursor::new(chained.clone())).unwrap();
    seeker.seek_to_sample(FIRST_FRAMES - 1).unwrap();
    let block = seeker.read_frames(1).unwrap().unwrap();
    assert_eq!(block.spec(), first_spec);
    let block = seeker.read_frames(1).unwrap().unwrap();
    assert_eq!(block.spec(), second_spec);
    let Samples::I16(samples) = block.samples() else {
        panic!("the second chained link must decode to signed 16-bit PCM");
    };
    assert_eq!(
        samples.as_slice(),
        &[expected_stereo_sample(0), expected_stereo_sample(1)]
    );

    seeker.seek_to_sample(FIRST_FRAMES).unwrap();
    let block = seeker.read_frames(23).unwrap().unwrap();
    assert_eq!(block.spec(), second_spec);
    let Samples::I16(samples) = block.samples() else {
        panic!("the second chained link must decode to signed 16-bit PCM");
    };
    assert_eq!(samples.len(), 46);
    for (index, sample) in samples.iter().enumerate() {
        assert_eq!(*sample, expected_stereo_sample(index as u64));
    }

    seeker.seek_to_sample(FIRST_FRAMES + SECOND_FRAMES).unwrap();
    assert!(seeker.read_frames(1).unwrap().is_none());
    assert!(
        seeker
            .seek_to_sample(FIRST_FRAMES + SECOND_FRAMES + 1)
            .is_err()
    );
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

fn decode_s16le_pcm_digest(
    bytes: &[u8],
    sample_rate: u32,
    channels: u16,
    expected_title: Option<&str>,
) -> (u64, u64) {
    let mut decoder = OggFlacDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(sample_rate, channels, SampleFormat::PcmI16).unwrap()
    );
    if let Some(title) = expected_title {
        assert_eq!(decoder.metadata().values("TITLE"), [title]);
    }
    let mut frames = 0;
    let mut digest = 0xcbf2_9ce4_8422_2325;
    while let Some(block) = decoder.read_frames(2_048).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("Ogg-FLAC fixture must decode to signed 16-bit PCM");
        };
        for byte in samples.iter().flat_map(|sample| sample.to_le_bytes()) {
            digest = (digest ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        frames += (samples.len() / usize::from(channels)) as u64;
    }
    (frames, digest)
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
    expected_multichannel_sample(index, 6)
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

fn expected_multichannel_sample(index: u64, channels: u64) -> i32 {
    const LIMIT: u64 = 1 << 24;
    const HALF: i64 = 1 << 23;
    match index {
        0 => -(HALF as i32),
        1 => (HALF - 1) as i32,
        2 => -1,
        3 => 0,
        4 => 1,
        _ => {
            let frame = index / channels;
            let channel = index % channels;
            ((frame * 104_729 + channel * 65_537 + (frame / 7) * 1_048_573) % LIMIT) as i32
                - HALF as i32
        }
    }
}
