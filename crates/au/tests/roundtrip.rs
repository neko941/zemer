use std::io::Cursor;

use au::{AuDecoder, AuEncoder, G711Encoding, rewrite_metadata};
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
