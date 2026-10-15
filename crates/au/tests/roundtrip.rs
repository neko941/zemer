use std::io::Cursor;

use au::{AuDecoder, AuEncoder, G711Encoding, rewrite_metadata};
use g726::{G726Decoder, G726Rate};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples,
};

fn round_trip(spec: AudioSpec, samples: Samples, code: u32) {
    let audio = PcmBuffer::new(spec, samples).unwrap();
    let mut writer = AuEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    writer.write_block(&audio).unwrap();
    let encoded = writer.finish().unwrap().into_inner();
    assert_eq!(&encoded[..4], b".snd");
    assert_eq!(
        u32::from_be_bytes(encoded[12..16].try_into().unwrap()),
        code
    );
    let mut decoder = AuDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(decoder.spec(), spec);
    let decoded = decoder.read_frames(audio.frames()).unwrap().unwrap();
    assert_eq!(decoded, audio);
    assert!(decoder.read_frames(1).unwrap().is_none());
}

#[test]
fn every_linear_and_float_encoding_round_trips_without_changing_bits() {
    round_trip(
        AudioSpec::new(44_100, 2, SampleFormat::PcmU8).unwrap(),
        Samples::U8(vec![0, 128, 255, 1]),
        2,
    );
    round_trip(
        AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![i16::MIN, -1, 0, i16::MAX]),
        3,
    );
    round_trip(
        AudioSpec::new(44_100, 2, SampleFormat::PcmI24).unwrap(),
        Samples::I24(vec![-8_388_608, -1, 0, 8_388_607]),
        4,
    );
    round_trip(
        AudioSpec::new(44_100, 2, SampleFormat::PcmI32).unwrap(),
        Samples::I32(vec![i32::MIN, -1, 0, i32::MAX]),
        5,
    );
    round_trip(
        AudioSpec::new(44_100, 2, SampleFormat::Float32).unwrap(),
        Samples::F32(vec![f32::from_bits(0x3f80_0001), -0.0, 0.25, 1.0]),
        6,
    );
    round_trip(
        AudioSpec::new(44_100, 2, SampleFormat::Float64).unwrap(),
        Samples::F64(vec![f64::from_bits(0x3ff0_0000_0000_0001), -0.0, 0.25, 1.0]),
        7,
    );
}

#[test]
fn g711_profiles_round_trip_the_encoded_codes() {
    for (law, code) in [(G711Encoding::MuLaw, 1), (G711Encoding::ALaw, 27)] {
        let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
        let audio = PcmBuffer::new(spec, Samples::I16(vec![-30_000, -1, 0, 1, 30_000])).unwrap();
        let mut writer = AuEncoder::new_g711(Cursor::new(Vec::new()), spec, law).unwrap();
        writer.write_block(&audio).unwrap();
        let encoded = writer.finish().unwrap().into_inner();
        assert_eq!(
            u32::from_be_bytes(encoded[12..16].try_into().unwrap()),
            code
        );
        assert_eq!(encoded.len(), 33);
        let mut decoder = AuDecoder::new(Cursor::new(encoded)).unwrap();
        assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);
        assert_eq!(decoder.read_frames(8).unwrap().unwrap().frames(), 5);
    }
}

#[test]
fn g721_matches_ffmpeg_codewords_and_pcm_reference_across_odd_blocks() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let input = [
        0, 1_000, -1_000, 3_000, -3_000, 8_000, -8_000, 16_000, -16_000, 32_767, -32_768, 1_234,
        -2_345, 512, -512, 42,
    ];
    let ffmpeg_au_codewords = [0x7f, 0x78, 0x78, 0x78, 0x78, 0xf8, 0xff, 0xef];
    let ffmpeg_pcm = [
        0, 88, -120, 172, -244, 376, -584, 1_044, -2_116, 4_704, -14_008, 3_068, -2_536, 1_648,
        -1_444, -692,
    ];

    let mut encoder = AuEncoder::new_g721(Cursor::new(Vec::new()), spec).unwrap();
    for samples in [&input[..5], &input[5..]] {
        encoder
            .write_block(&PcmBuffer::new(spec, Samples::I16(samples.to_vec())).unwrap())
            .unwrap();
    }
    let encoded = encoder.finish().unwrap().into_inner();
    assert_eq!(u32::from_be_bytes(encoded[12..16].try_into().unwrap()), 23);
    assert_eq!(u32::from_be_bytes(encoded[8..12].try_into().unwrap()), 8);
    assert_eq!(&encoded[28..], ffmpeg_au_codewords);

    let mut decoder = AuDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(decoder.spec(), spec);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(3).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("G.721 must decode to signed 16-bit PCM");
        };
        decoded.extend_from_slice(samples);
    }
    let max_difference = decoded
        .iter()
        .zip(ffmpeg_pcm)
        .map(|(actual, expected)| actual.abs_diff(expected))
        .max()
        .unwrap();
    assert!(
        max_difference <= 8,
        "FFmpeg PCM differs by {max_difference}"
    );
}

#[test]
fn g723_profiles_round_trip_streaming_blocks_and_use_complete_groups() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let input = [0, 1_000, -1_000, 3_000, -3_000, 8_000, -8_000, 16_000];
    for (code, bytes, is_three_bit) in [(25_u32, 3, true), (26_u32, 5, false)] {
        let mut encoder = if is_three_bit {
            AuEncoder::new_g723_3(Cursor::new(Vec::new()), spec)
        } else {
            AuEncoder::new_g723_5(Cursor::new(Vec::new()), spec)
        }
        .unwrap();
        for chunk in [&input[..3], &input[3..]] {
            encoder
                .write_block(&PcmBuffer::new(spec, Samples::I16(chunk.to_vec())).unwrap())
                .unwrap();
        }
        let encoded = encoder.finish().unwrap().into_inner();
        assert_eq!(
            u32::from_be_bytes(encoded[12..16].try_into().unwrap()),
            code
        );
        assert_eq!(encoded.len(), 28 + bytes);
        let mut decoder = AuDecoder::new(Cursor::new(encoded)).unwrap();
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(1).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("G.723 must decode to signed 16-bit PCM");
            };
            decoded.extend_from_slice(samples);
        }
        assert_eq!(decoded.len(), input.len());
    }

    for is_three_bit in [true, false] {
        let mut encoder = if is_three_bit {
            AuEncoder::new_g723_3(Cursor::new(Vec::new()), spec)
        } else {
            AuEncoder::new_g723_5(Cursor::new(Vec::new()), spec)
        }
        .unwrap();
        encoder
            .write_block(&PcmBuffer::new(spec, Samples::I16(vec![1])).unwrap())
            .unwrap();
        assert!(encoder.finish().is_err());
    }
}

#[test]
fn g723_long_au_stream_matches_shared_decoder_across_read_boundaries() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let input: Vec<i16> = (0..4_096)
        .map(|index| ((index * 12_347 % 65_536) - 32_768) as i16)
        .collect();

    for (rate, is_three_bit) in [(G726Rate::Kbps24, true), (G726Rate::Kbps40, false)] {
        let mut encoder = if is_three_bit {
            AuEncoder::new_g723_3(Cursor::new(Vec::new()), spec)
        } else {
            AuEncoder::new_g723_5(Cursor::new(Vec::new()), spec)
        }
        .unwrap();
        for samples in input.chunks(37) {
            encoder
                .write_block(&PcmBuffer::new(spec, Samples::I16(samples.to_vec())).unwrap())
                .unwrap();
        }
        let encoded = encoder.finish().unwrap().into_inner();
        let data_offset = u32::from_be_bytes(encoded[4..8].try_into().unwrap()) as usize;
        let mut reference = G726Decoder::new(rate, 1).unwrap();
        let expected = reference
            .decode_packed_lsb(&encoded[data_offset..], input.len())
            .unwrap();

        let mut decoder = AuDecoder::new(Cursor::new(encoded)).unwrap();
        let mut actual = Vec::new();
        let read_sizes = [1, 7, 64, 3, 257, 19];
        let mut read_index = 0;
        while let Some(block) = decoder
            .read_frames(read_sizes[read_index % read_sizes.len()])
            .unwrap()
        {
            let Samples::I16(samples) = block.samples() else {
                panic!("AU G.723 must decode to signed 16-bit PCM");
            };
            actual.extend_from_slice(samples);
            read_index += 1;
        }
        assert_eq!(actual, expected, "AU G.723 decode using {rate:?}");
        assert!(decoder.read_frames(1).unwrap().is_none());
    }
}

#[test]
fn g723_decoder_rejects_incomplete_packing_groups() {
    for (code, bytes) in [(25_u32, 1_u32), (26_u32, 4_u32)] {
        let mut encoded = vec![0; 28 + bytes as usize];
        encoded[..4].copy_from_slice(b".snd");
        encoded[4..8].copy_from_slice(&28_u32.to_be_bytes());
        encoded[8..12].copy_from_slice(&bytes.to_be_bytes());
        encoded[12..16].copy_from_slice(&code.to_be_bytes());
        encoded[16..20].copy_from_slice(&8_000_u32.to_be_bytes());
        encoded[20..24].copy_from_slice(&1_u32.to_be_bytes());
        assert!(AuDecoder::new(Cursor::new(encoded)).is_err());
    }
}

#[test]
fn g721_requires_even_frame_count_and_8khz_mono_i16() {
    for spec in [
        AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap(),
        AudioSpec::new(8_000, 2, SampleFormat::PcmI16).unwrap(),
        AudioSpec::new(8_000, 1, SampleFormat::PcmI24).unwrap(),
    ] {
        assert!(AuEncoder::new_g721(Cursor::new(Vec::new()), spec).is_err());
    }

    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let mut encoder = AuEncoder::new_g721(Cursor::new(Vec::new()), spec).unwrap();
    encoder
        .write_block(&PcmBuffer::new(spec, Samples::I16(vec![1])).unwrap())
        .unwrap();
    assert!(encoder.finish().is_err());
}

#[test]
fn g721_decoder_rejects_non_8khz_or_multichannel_headers() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let encoded = AuEncoder::new_g721(Cursor::new(Vec::new()), spec)
        .unwrap()
        .finish()
        .unwrap()
        .into_inner();

    for (offset, invalid) in [(16, 44_100_u32), (20, 2_u32)] {
        let mut malformed = encoded.clone();
        malformed[offset..offset + 4].copy_from_slice(&invalid.to_be_bytes());
        assert!(AuDecoder::new(Cursor::new(malformed)).is_err());
    }
}

#[test]
fn annotation_round_trips_as_a_comment_and_retains_raw_bytes() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("COMMENT", "Call the studio");
    let audio = PcmBuffer::new(spec, Samples::U8(vec![128, 129])).unwrap();
    let mut writer =
        AuEncoder::new_with_metadata(Cursor::new(Vec::new()), spec, &metadata).unwrap();
    writer.write_block(&audio).unwrap();
    let encoded = writer.finish().unwrap().into_inner();
    assert_eq!(u32::from_be_bytes(encoded[4..8].try_into().unwrap()), 40);
    let decoder = AuDecoder::new(Cursor::new(encoded.clone())).unwrap();
    assert_eq!(decoder.metadata().values("COMMENT"), ["Call the studio"]);
    assert_eq!(decoder.annotation(), b"Call the studio\0");

    let mut raw_writer = AuEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    raw_writer.set_annotation(b"\x00opaque\xff").unwrap();
    raw_writer.write_block(&audio).unwrap();
    let raw_file = raw_writer.finish().unwrap().into_inner();
    let mut decoder = AuDecoder::new(Cursor::new(raw_file.clone())).unwrap();
    assert_eq!(decoder.annotation(), b"\x00opaque\xff");
    assert!(decoder.metadata().values("COMMENT").is_empty());
    assert_eq!(decoder.read_frames(2).unwrap().unwrap(), audio);
}

#[test]
fn unknown_data_size_reads_to_end_and_rewrite_preserves_audio_and_opaque_annotation() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
    let audio = PcmBuffer::new(spec, Samples::U8(vec![128, 129, 127])).unwrap();
    let mut writer = AuEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    writer.set_annotation(b"\0binary\xff").unwrap();
    writer.write_block(&audio).unwrap();
    let mut encoded = writer.finish().unwrap().into_inner();
    let offset = u32::from_be_bytes(encoded[4..8].try_into().unwrap()) as usize;
    let data = encoded[offset..].to_vec();
    encoded[8..12].copy_from_slice(&u32::MAX.to_be_bytes());

    let mut decoder = AuDecoder::new(Cursor::new(encoded.clone())).unwrap();
    assert_eq!(decoder.read_frames(8).unwrap().unwrap(), audio);
    assert!(decoder.read_frames(1).unwrap().is_none());

    let mut output = Cursor::new(Vec::new());
    rewrite_metadata(Cursor::new(encoded), &mut output, &Metadata::default()).unwrap();
    let rewritten = output.into_inner();
    let new_offset = u32::from_be_bytes(rewritten[4..8].try_into().unwrap()) as usize;
    assert_eq!(&rewritten[24..new_offset], b"\0binary\xff");
    assert_eq!(&rewritten[new_offset..], data);
}

#[test]
fn malformed_headers_and_partial_frames_are_rejected() {
    let invalid = [0; 24];
    assert!(AuDecoder::new(Cursor::new(invalid)).is_err());

    let mut partial = b".snd".to_vec();
    partial.extend_from_slice(&28_u32.to_be_bytes());
    partial.extend_from_slice(&1_u32.to_be_bytes());
    partial.extend_from_slice(&3_u32.to_be_bytes());
    partial.extend_from_slice(&8_000_u32.to_be_bytes());
    partial.extend_from_slice(&1_u32.to_be_bytes());
    partial.extend_from_slice(&[0; 4]);
    partial.push(0);
    assert!(AuDecoder::new(Cursor::new(partial)).is_err());
}

#[test]
fn malformed_mcgill_headers_are_rejected_by_decode_and_metadata_rewrite() {
    for (offset, data_size, annotation) in
        [(24_u32, 1_u32, &[][..]), (28_u32, 2_u32, &[0, 0, 0, 0][..])]
    {
        let mut file = b".snd".to_vec();
        file.extend_from_slice(&offset.to_be_bytes());
        file.extend_from_slice(&data_size.to_be_bytes());
        file.extend_from_slice(&1_u32.to_be_bytes());
        file.extend_from_slice(&8_000_u32.to_be_bytes());
        file.extend_from_slice(&1_u32.to_be_bytes());
        file.extend_from_slice(annotation);
        file.push(0xff);

        assert!(AuDecoder::new(Cursor::new(file.clone())).is_err());
        assert!(
            rewrite_metadata(
                Cursor::new(file),
                &mut Cursor::new(Vec::new()),
                &Metadata::default(),
            )
            .is_err()
        );
    }
}

#[test]
fn metadata_rewrite_changes_only_annotation_and_keeps_unknown_annotation_when_unchanged() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
    let audio = PcmBuffer::new(spec, Samples::U8(vec![128, 129, 127])).unwrap();
    let mut writer = AuEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    writer.write_block(&audio).unwrap();
    let original = writer.finish().unwrap().into_inner();
    let original_offset = u32::from_be_bytes(original[4..8].try_into().unwrap()) as usize;
    let audio_bytes = original[original_offset..].to_vec();

    let mut metadata = Metadata::default();
    metadata.insert("COMMENT", "Edited note");
    let mut output = Cursor::new(Vec::new());
    rewrite_metadata(Cursor::new(original), &mut output, &metadata).unwrap();
    let rewritten = output.into_inner();
    let decoder = AuDecoder::new(Cursor::new(rewritten.clone())).unwrap();
    assert_eq!(decoder.metadata().values("COMMENT"), ["Edited note"]);
    let rewritten_offset = u32::from_be_bytes(rewritten[4..8].try_into().unwrap()) as usize;
    assert_eq!(&rewritten[rewritten_offset..], audio_bytes);
}
