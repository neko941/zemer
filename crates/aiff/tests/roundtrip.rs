use std::io::Cursor;

use aiff::{AiffComment, AiffDecoder, AiffEncoder, AiffMarker, rewrite_metadata};
use g711::{G711Encoding, decode_alaw, decode_mulaw, encode_alaw, encode_mulaw};
use zemer_core::{AudioDecoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

fn audio(format: SampleFormat) -> PcmBuffer {
    let spec = AudioSpec::new(44_100, 2, format).unwrap();
    let samples = match format {
        SampleFormat::PcmU8 => Samples::U8(vec![0, 128, 255, 64]),
        SampleFormat::PcmI16 => Samples::I16(vec![i16::MIN, -1, 0, i16::MAX]),
        SampleFormat::PcmI24 => Samples::I24(vec![-8_388_608, -1, 0, 8_388_607]),
        SampleFormat::PcmI32 => Samples::I32(vec![i32::MIN, -1, 0, i32::MAX]),
        _ => panic!("integer PCM required"),
    };
    PcmBuffer::new(spec, samples).unwrap()
}

fn append_chunk(form: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    form.extend_from_slice(id);
    form.extend_from_slice(&(body.len() as u32).to_be_bytes());
    form.extend_from_slice(body);
    if body.len() & 1 != 0 {
        form.push(0);
    }
}

fn chunk_body<'a>(form: &'a [u8], id: &[u8; 4]) -> &'a [u8] {
    let form_end = 8 + u32::from_be_bytes(form[4..8].try_into().unwrap()) as usize;
    let mut offset = 12;
    while offset < form_end {
        let size = u32::from_be_bytes(form[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        if &form[offset..offset + 4] == id {
            return &form[start..start + size];
        }
        offset = start + size + (size & 1);
    }
    panic!("missing AIFF chunk {:?}", id);
}

fn append_form_chunk(form: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    let end = 8 + u32::from_be_bytes(form[4..8].try_into().unwrap()) as usize;
    let mut chunk = Vec::new();
    append_chunk(&mut chunk, id, body);
    form.splice(end..end, chunk);
    let size = (form.len() - 8) as u32;
    form[4..8].copy_from_slice(&size.to_be_bytes());
}

fn aifc(compression: &[u8; 4], bits: u16, audio: &[u8]) -> Vec<u8> {
    let mut bytes = b"FORM\0\0\0\0AIFC".to_vec();
    append_chunk(&mut bytes, b"FVER", &0xA280_5140_u32.to_be_bytes());
    let name = b"none";
    let mut comm = Vec::from(1_u16.to_be_bytes());
    comm.extend_from_slice(&((audio.len() / usize::from(bits.div_ceil(8))) as u32).to_be_bytes());
    comm.extend_from_slice(&bits.to_be_bytes());
    comm.extend_from_slice(&[0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0]);
    comm.extend_from_slice(compression);
    comm.push(name.len() as u8);
    comm.extend_from_slice(name);
    if comm.len() & 1 != 0 {
        comm.push(0);
    }
    append_chunk(&mut bytes, b"COMM", &comm);
    let mut sound = vec![0; 8];
    sound.extend_from_slice(audio);
    append_chunk(&mut bytes, b"SSND", &sound);
    let size = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&size.to_be_bytes());
    bytes
}

fn aifc_g711(compression: &[u8; 4], audio: &[u8]) -> Vec<u8> {
    let mut bytes = aifc(compression, 16, audio);
    bytes[34..38].copy_from_slice(&(audio.len() as u32).to_be_bytes());
    bytes
}

#[test]
fn integer_pcm_round_trips_and_maps_ascii_metadata() {
    for format in [
        SampleFormat::PcmU8,
        SampleFormat::PcmI16,
        SampleFormat::PcmI24,
        SampleFormat::PcmI32,
    ] {
        let original = audio(format);
        let mut metadata = Metadata::default();
        metadata.insert("title", "AIFF track");
        metadata.insert("artist", "Composer");
        metadata.insert("comment", "first");
        metadata.insert("comment", "second");
        let mut encoder =
            AiffEncoder::new_with_metadata(Cursor::new(Vec::new()), original.spec(), &metadata)
                .unwrap();
        encoder.write_block(&original).unwrap();
        let encoded = encoder.finish().unwrap().get_ref().clone();
        let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
        let decoded = decoder.read_frames(8).unwrap().unwrap();
        assert_eq!(decoded, original);
        assert_eq!(decoder.metadata(), &metadata);
        assert!(decoder.read_frames(8).unwrap().is_none());
    }
}

#[test]
fn typed_comments_round_trip_timestamp_marker_and_odd_text() {
    let original = audio(SampleFormat::PcmI16);
    let comments = vec![
        AiffComment {
            timestamp: 0,
            marker_id: 0,
            text: b"untimed".to_vec(),
        },
        AiffComment {
            timestamp: 3_900_000_000,
            marker_id: 7,
            text: b"marker comment".to_vec(),
        },
    ];
    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), original.spec()).unwrap();
    encoder.set_comments(&comments).unwrap();
    encoder.write_block(&original).unwrap();
    let encoded = encoder.finish().unwrap().get_ref().clone();
    let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(decoder.comments(), comments);
    assert_eq!(decoder.read_frames(8).unwrap().unwrap(), original);
}

#[test]
fn typed_markers_round_trip_ids_positions_and_pascal_name_padding() {
    let original = audio(SampleFormat::PcmI16);
    let markers = vec![
        AiffMarker {
            id: 1,
            position: 0,
            name: b"start".to_vec(),
        },
        AiffMarker {
            id: 7,
            position: 2,
            name: vec![b'e', b'n', b'd', 0xff],
        },
    ];
    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), original.spec()).unwrap();
    encoder.set_markers(&markers).unwrap();
    encoder.write_block(&original).unwrap();
    let encoded = encoder.finish().unwrap().get_ref().clone();
    let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(decoder.markers(), markers);
    assert_eq!(decoder.read_frames(8).unwrap().unwrap(), original);
}

#[test]
fn validates_marker_ids_positions_and_chunk_structure() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
    let marker = |id, position| AiffMarker {
        id,
        position,
        name: Vec::new(),
    };
    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    assert!(encoder.set_markers(&[marker(0, 0)]).is_err());
    assert!(encoder.set_markers(&[marker(1, 0), marker(1, 0)]).is_err());
    assert!(
        encoder
            .set_markers(&[AiffMarker {
                id: 1,
                position: 0,
                name: vec![0; 256],
            }])
            .is_err()
    );

    let audio = PcmBuffer::new(spec, Samples::U8(vec![128])).unwrap();
    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_markers(&[marker(1, 2)]).unwrap();
    encoder.write_block(&audio).unwrap();
    assert!(encoder.finish().is_err());

    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_block(&audio).unwrap();
    let mut encoded = encoder.finish().unwrap().get_ref().clone();
    append_form_chunk(&mut encoded, b"MARK", &[0, 1]);
    assert!(AiffDecoder::new(Cursor::new(encoded)).is_err());

    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_block(&audio).unwrap();
    let mut encoded = encoder.finish().unwrap().get_ref().clone();
    append_form_chunk(&mut encoded, b"MARK", &[0, 1, 0, 1, 0, 0, 0, 2, 1, b'x']);
    assert!(AiffDecoder::new(Cursor::new(encoded)).is_err());

    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_block(&audio).unwrap();
    let mut encoded = encoder.finish().unwrap().get_ref().clone();
    append_form_chunk(&mut encoded, b"MARK", &[0, 0]);
    append_form_chunk(&mut encoded, b"MARK", &[0, 0]);
    assert!(AiffDecoder::new(Cursor::new(encoded)).is_err());
}

#[test]
fn rejects_malformed_and_duplicate_comment_chunks() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder
        .write_block(&PcmBuffer::new(spec, Samples::U8(vec![128])).unwrap())
        .unwrap();
    let original = encoder.finish().unwrap().get_ref().clone();

    let mut truncated = original.clone();
    append_form_chunk(&mut truncated, b"COMT", &[0, 1]);
    assert!(AiffDecoder::new(Cursor::new(truncated)).is_err());

    let mut duplicate = original.clone();
    append_form_chunk(&mut duplicate, b"COMT", &[0, 0]);
    append_form_chunk(&mut duplicate, b"COMT", &[0, 0]);
    assert!(AiffDecoder::new(Cursor::new(duplicate)).is_err());

    let mut negative_marker = original;
    append_form_chunk(
        &mut negative_marker,
        b"COMT",
        &[0, 1, 0, 0, 0, 42, 0xff, 0xff, 0, 0],
    );
    assert!(AiffDecoder::new(Cursor::new(negative_marker)).is_err());
}

#[test]
fn rejects_form_and_chunk_bounds_before_reading_audio() {
    let mut invalid = b"FORM\0\0\0\x04AIFF".to_vec();
    assert!(AiffDecoder::new(Cursor::new(std::mem::take(&mut invalid))).is_err());

    let mut oversized = b"FORM\0\0\0\x14AIFFCOMM\0\0\0\x12".to_vec();
    oversized.extend_from_slice(&[0; 18]);
    assert!(AiffDecoder::new(Cursor::new(oversized)).is_err());
}

#[test]
fn allows_zero_frame_file_without_an_ssnd_chunk() {
    let without_ssnd = |mut bytes: Vec<u8>| {
        let sound = bytes
            .windows(4)
            .position(|window| window == b"SSND")
            .unwrap();
        bytes.truncate(sound);
        let size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&size.to_be_bytes());
        bytes
    };

    for (bytes, aifc) in [(aiff16(16, &[]), false), (aifc(b"NONE", 16, &[]), true)] {
        let mut decoder = AiffDecoder::new(Cursor::new(without_ssnd(bytes))).unwrap();
        assert_eq!(decoder.is_aifc(), aifc);
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    for bytes in [aiff16(16, &[0, 0]), aifc(b"NONE", 16, &[0, 0])] {
        assert!(AiffDecoder::new(Cursor::new(without_ssnd(bytes))).is_err());
    }
}

#[test]
fn validates_aifc_version_code_and_ssnd_block_size() {
    let mut invalid_version = aifc(b"NONE", 16, &[0, 0]);
    invalid_version[23] = 0;
    assert!(AiffDecoder::new(Cursor::new(invalid_version)).is_err());

    let mut unsupported = aifc(b"NONE", 16, &[0, 0]);
    unsupported[50..54].copy_from_slice(b"MACE");
    assert!(AiffDecoder::new(Cursor::new(unsupported)).is_err());

    let mut blocked = aiff16(16, &[0, 0]);
    blocked[50..54].copy_from_slice(&1_u32.to_be_bytes());
    assert!(AiffDecoder::new(Cursor::new(blocked)).is_err());
}

#[test]
fn decodes_aifc_little_endian_and_float32_compression_codes() {
    let bytes = aifc(b"twos", 16, &[0x12, 0x34]);
    let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(
        decoder.read_frames(1).unwrap().unwrap().samples(),
        &Samples::I16(vec![0x1234])
    );

    let bytes = aifc(b"sowt", 16, &[0x34, 0x12, 0xcc, 0xed]);
    let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
    assert!(decoder.is_aifc());
    assert_eq!(
        decoder.read_frames(2).unwrap().unwrap().samples(),
        &Samples::I16(vec![0x1234, -0x1234])
    );

    let bytes = aifc(b"fl32", 32, &[0x3f, 0x80, 0, 0, 0xbf, 0, 0, 0]);
    let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(
        decoder.read_frames(2).unwrap().unwrap().samples(),
        &Samples::F32(vec![1.0, -0.5])
    );

    let bytes = aifc(b"fl64", 64, &[0x3f, 0xf0, 0, 0, 0, 0, 0, 0]);
    let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(
        decoder.read_frames(1).unwrap().unwrap().samples(),
        &Samples::F64(vec![1.0])
    );
}

#[test]
fn aifc_g711_decodes_apple_and_sgi_compression_codes() {
    for compression in [b"alaw", b"ALAW"] {
        let mut decoder =
            AiffDecoder::new(Cursor::new(aifc_g711(compression, &[0xd5, 0x55]))).unwrap();
        assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::I16(vec![decode_alaw(0xd5), decode_alaw(0x55)])
        );
    }
    for compression in [b"ulaw", b"ULAW"] {
        let mut decoder =
            AiffDecoder::new(Cursor::new(aifc_g711(compression, &[0xff, 0x00]))).unwrap();
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::I16(vec![decode_mulaw(0xff), decode_mulaw(0x00)])
        );
    }
    assert!(AiffDecoder::new(Cursor::new(aifc(b"alaw", 8, &[0xd5, 0x55]))).is_err());
}

#[test]
fn sgi_ulaw_matches_mcgill_pcm_sample() {
    let codes = [
        0xff, 0xff, 0xff, 0xff, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfc, 0xfb, 0xfd,
    ];
    let mut decoder = AiffDecoder::new(Cursor::new(aifc_g711(b"ULAW", &codes))).unwrap();
    assert_eq!(
        decoder.read_frames(codes.len()).unwrap().unwrap().samples(),
        &Samples::I16(vec![0, 0, 0, 0, 8, 8, 8, 8, 16, 24, 32, 16])
    );
}

#[test]
fn aifc_g711_writes_explicitly_selected_compression() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let samples = [i16::MIN, -1, 0, 1, i16::MAX];
    let audio = PcmBuffer::new(spec, Samples::I16(samples.to_vec())).unwrap();
    for (encoding, compression, encoded) in [
        (G711Encoding::ALaw, *b"alaw", samples.map(encode_alaw)),
        (G711Encoding::MuLaw, *b"ulaw", samples.map(encode_mulaw)),
    ] {
        let decoded = match encoding {
            G711Encoding::ALaw => encoded.map(decode_alaw),
            G711Encoding::MuLaw => encoded.map(decode_mulaw),
        };
        let mut encoder =
            AiffEncoder::new_aifc_g711(Cursor::new(Vec::new()), spec, encoding).unwrap();
        encoder.write_block(&audio).unwrap();
        let bytes = encoder.finish().unwrap().get_ref().clone();
        assert_eq!(&chunk_body(&bytes, b"COMM")[18..22], &compression);
        assert_eq!(
            u16::from_be_bytes(chunk_body(&bytes, b"COMM")[6..8].try_into().unwrap()),
            16
        );
        assert_eq!(&chunk_body(&bytes, b"SSND")[8..], encoded);
        let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            decoder
                .read_frames(samples.len())
                .unwrap()
                .unwrap()
                .samples(),
            &Samples::I16(decoded.to_vec())
        );
    }
}

#[test]
fn decodes_ffmpeg_ima4_aifc_packets_against_pcm_oracle() {
    let bytes = include_bytes!("../../../tests/fixtures/aifc-ima4-ffmpeg-8_1.aifc");
    let oracle = include_bytes!("../../../tests/fixtures/aifc-ima4-ffmpeg-8_1.pcm");
    let mut decoder = AiffDecoder::new(Cursor::new(bytes.as_slice())).unwrap();
    assert!(decoder.is_aifc());
    assert_eq!(decoder.spec().sample_rate(), 8_000);
    assert_eq!(decoder.spec().channels(), 1);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(37).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("IMA4 must decode to signed 16-bit PCM");
        };
        decoded.extend_from_slice(samples);
    }
    let expected: Vec<_> = oracle
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    assert_eq!(decoded.len(), 832);
    assert_eq!(decoded, expected);
}

#[test]
fn decodes_interleaved_stereo_ffmpeg_ima4_aifc_against_pcm_oracle() {
    let bytes = include_bytes!("../../../tests/fixtures/aifc-ima4-ffmpeg-stereo-8_1.aifc");
    let oracle = include_bytes!("../../../tests/fixtures/aifc-ima4-ffmpeg-stereo-8_1.pcm");
    let mut decoder = AiffDecoder::new(Cursor::new(bytes.as_slice())).unwrap();
    assert_eq!(decoder.spec().channels(), 2);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(53).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("IMA4 must decode to signed 16-bit PCM");
        };
        decoded.extend_from_slice(samples);
    }
    let expected: Vec<_> = oracle
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect();
    assert_eq!(decoded.len(), 832 * 2);
    assert_eq!(decoded, expected);
}

#[test]
fn ima4_encoder_pads_the_final_packet_and_streams_across_input_blocks() {
    let spec = AudioSpec::new(8_000, 2, SampleFormat::PcmI16).unwrap();
    let mut encoder = AiffEncoder::new_aifc_ima4(Cursor::new(Vec::new()), spec).unwrap();
    let samples: Vec<_> = (0..142).map(|index| (index as i16) * 100).collect();
    encoder
        .write_block(&PcmBuffer::new(spec, Samples::I16(samples[..58].to_vec())).unwrap())
        .unwrap();
    encoder
        .write_block(&PcmBuffer::new(spec, Samples::I16(samples[58..].to_vec())).unwrap())
        .unwrap();
    let bytes = encoder.finish().unwrap().get_ref().clone();
    assert_eq!(
        u32::from_be_bytes(chunk_body(&bytes, b"COMM")[2..6].try_into().unwrap()),
        2
    );
    assert_eq!(chunk_body(&bytes, b"SSND").len(), 8 + 2 * 34 * 2);
    let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.read_frames(128).unwrap().unwrap().frames(), 128);
    assert!(decoder.read_frames(1).unwrap().is_none());
}

#[test]
fn writes_aifc_float32_and_preserves_float_bits() {
    let spec = AudioSpec::new(96_000, 1, SampleFormat::Float32).unwrap();
    let audio =
        PcmBuffer::new(spec, Samples::F32(vec![f32::from_bits(0x7fc1_2345), -0.5])).unwrap();
    let mut encoder = AiffEncoder::new_aifc(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_block(&audio).unwrap();
    let bytes = encoder.finish().unwrap().get_ref().clone();
    let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
    let decoded = decoder.read_frames(2).unwrap().unwrap();
    let Samples::F32(decoded_values) = decoded.samples() else {
        panic!("expected float32 samples");
    };
    let Samples::F32(original_values) = audio.samples() else {
        panic!("expected float32 samples");
    };
    assert_eq!(
        decoded_values
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        original_values
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>()
    );
}

#[test]
fn pads_odd_size_ssnd_before_following_chunks() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
    let audio = PcmBuffer::new(spec, Samples::U8(vec![0, 128, 255])).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "odd");
    let mut encoder =
        AiffEncoder::new_with_metadata(Cursor::new(Vec::new()), spec, &metadata).unwrap();
    encoder.write_block(&audio).unwrap();
    let bytes = encoder.finish().unwrap().get_ref().clone();
    let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.read_frames(3).unwrap().unwrap(), audio);
    assert_eq!(decoder.metadata(), &metadata);
}

#[test]
fn rejects_unrepresentable_metadata_before_writing_a_header() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("composer", "not mapped by AIFF");
    let mut output = Cursor::new(Vec::new());
    assert!(AiffEncoder::new_with_metadata(&mut output, spec, &metadata).is_err());
    assert!(output.into_inner().is_empty());
}

#[test]
fn rewrites_tags_without_changing_audio_unknown_chunks_or_trailing_data() {
    let original = PcmBuffer::new(
        AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![-12_345, 0, 23_456]),
    )
    .unwrap();
    let mut old_tags = Metadata::default();
    old_tags.insert("title", "Old title");
    let mut encoder =
        AiffEncoder::new_with_metadata(Cursor::new(Vec::new()), original.spec(), &old_tags)
            .unwrap();
    encoder.write_block(&original).unwrap();
    let mut input = encoder.finish().unwrap().get_ref().clone();
    let comment_body = [0, 1, 0, 0, 0, 42, 0, 3, 0, 3, b'n', b'o', b't', 0];
    append_form_chunk(&mut input, b"COMT", &comment_body);
    let original_unknown = b"TEST\0\0\0\x03\x01\x02\x03\0".to_vec();
    let form_size = 8 + u32::from_be_bytes(input[4..8].try_into().unwrap()) as usize;
    input.splice(form_size..form_size, original_unknown.clone());
    let new_form_size = (form_size + original_unknown.len() - 8) as u32;
    input[4..8].copy_from_slice(&new_form_size.to_be_bytes());
    let trailer = b"ID3 trailer";
    input.extend_from_slice(trailer);
    let original_audio = chunk_body(&input, b"SSND")[8..].to_vec();

    let mut new_tags = Metadata::default();
    new_tags.insert("title", "New title");
    new_tags.insert("comment", "one");
    new_tags.insert("comment", "two");
    let mut output = Cursor::new(Vec::new());
    rewrite_metadata(Cursor::new(input), &mut output, &new_tags).unwrap();
    let output = output.into_inner();
    let output_end = 8 + u32::from_be_bytes(output[4..8].try_into().unwrap()) as usize;
    assert_eq!(&output[output_end..], trailer);
    assert!(
        output
            .windows(original_unknown.len())
            .any(|chunk| chunk == original_unknown)
    );
    assert_eq!(chunk_body(&output, b"SSND")[8..], original_audio);
    assert_eq!(chunk_body(&output, b"COMT"), comment_body);
    let mut decoder = AiffDecoder::new(Cursor::new(output)).unwrap();
    assert_eq!(decoder.metadata(), &new_tags);
    assert_eq!(decoder.read_frames(3).unwrap().unwrap(), original);
}

#[test]
fn aiff_metadata_rewrite_rejects_multiple_single_value_tags() {
    let mut tags = Metadata::default();
    tags.insert("artist", "First");
    tags.insert("artist", "Second");
    let mut output = Cursor::new(Vec::new());
    assert!(rewrite_metadata(Cursor::new(Vec::new()), &mut output, &tags).is_err());
    assert!(output.into_inner().is_empty());
}

#[test]
fn retains_left_aligned_valid_bits_in_sample_containers() {
    let bytes = aiff16(12, &[0x7f, 0xf0, 0x80, 0x00]);
    let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(
        decoder.read_frames(2).unwrap().unwrap().samples(),
        &Samples::I16(vec![0x7ff0, i16::MIN])
    );
}

fn aiff16(bits: u16, audio: &[u8]) -> Vec<u8> {
    let mut bytes = b"FORM\0\0\0\0AIFF".to_vec();
    let mut comm = Vec::from(1_u16.to_be_bytes());
    comm.extend_from_slice(&((audio.len() / 2) as u32).to_be_bytes());
    comm.extend_from_slice(&bits.to_be_bytes());
    comm.extend_from_slice(&[0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0]);
    append_chunk(&mut bytes, b"COMM", &comm);
    let mut sound = vec![0; 8];
    sound.extend_from_slice(audio);
    append_chunk(&mut bytes, b"SSND", &sound);
    let size = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&size.to_be_bytes());
    bytes
}
