use std::io::Cursor;

use aiff::{
    AiffAudioRecording, AiffComment, AiffDecoder, AiffEncoder, AiffMarker, AiffSaxel, G726Rate,
    rewrite_metadata,
};
use g711::{G711Encoding, decode_alaw, decode_mulaw, encode_alaw, encode_mulaw};
use g726::{G726Decoder, G726Encoder, G726Packing};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples,
};

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

#[test]
fn aifc_sowt_writer_round_trips_integer_depths_and_emits_little_endian_pcm() {
    let cases = [
        (
            SampleFormat::PcmU8,
            Samples::U8(vec![0, 128, 255, 64]),
            8,
            vec![0x80, 0x00, 0x7f, 0xc0],
        ),
        (
            SampleFormat::PcmI16,
            Samples::I16(vec![i16::MIN, -1, 0, i16::MAX]),
            16,
            vec![0x00, 0x80, 0xff, 0xff, 0x00, 0x00, 0xff, 0x7f],
        ),
        (
            SampleFormat::PcmI24,
            Samples::I24(vec![-8_388_608, -1, 0, 8_388_607]),
            24,
            vec![
                0x00, 0x00, 0x80, 0xff, 0xff, 0xff, 0, 0, 0, 0xff, 0xff, 0x7f,
            ],
        ),
        (
            SampleFormat::PcmI32,
            Samples::I32(vec![i32::MIN, -1, 0, i32::MAX]),
            32,
            vec![
                0, 0, 0, 0x80, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0x7f,
            ],
        ),
    ];

    for (format, samples, bits, expected_payload) in cases {
        let spec = AudioSpec::new(44_100, 2, format).unwrap();
        let input = PcmBuffer::new(spec, samples).unwrap();
        let mut encoder = AiffEncoder::new_aifc_sowt(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_block(&input).unwrap();
        let encoded = encoder.finish().unwrap().get_ref().clone();
        let comm = chunk_body(&encoded, b"COMM");
        assert_eq!(u16::from_be_bytes(comm[6..8].try_into().unwrap()), bits);
        assert_eq!(&comm[18..22], b"sowt");
        assert_eq!(comm[22], 0, "sowt compression name is empty");
        assert_eq!(&chunk_body(&encoded, b"SSND")[8..], expected_payload);

        let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
        assert_eq!(decoder.spec(), spec);
        assert_eq!(decoder.read_frames(2).unwrap().unwrap(), input);
        assert!(decoder.read_frames(1).unwrap().is_none());
    }
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

fn id3_with_private_frame() -> Vec<u8> {
    let mut frame = b"PRIV\0\0\0\x04\0\0x\0\xde\xad".to_vec();
    let mut tag = b"ID3\x04\0\0\0\0\0\x0e".to_vec();
    tag.append(&mut frame);
    tag
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

fn pack_g726_lsb(codes: &[u8], width: u8) -> Vec<u8> {
    let mut packed = vec![0; (codes.len() * usize::from(width)).div_ceil(8)];
    for (index, &code) in codes.iter().enumerate() {
        for bit in 0..usize::from(width) {
            let position = index * usize::from(width) + bit;
            packed[position / 8] |= ((code >> bit) & 1) << (position % 8);
        }
    }
    packed
}

fn aifc_g726(codes: &[u8], include_bitrate_hint: bool) -> Vec<u8> {
    let packed = pack_g726_lsb(codes, 5);
    let mut bytes = aifc(b"G726", 16, &packed);
    bytes[34..38].copy_from_slice(&(codes.len() as u32).to_be_bytes());
    bytes[40..50].copy_from_slice(&[0x40, 0x0b, 0xfa, 0, 0, 0, 0, 0, 0, 0]);
    if include_bitrate_hint {
        let mut hint = b"stocG726".to_vec();
        hint.extend_from_slice(&40_000_u32.to_be_bytes());
        append_form_chunk(&mut bytes, b"APPL", &hint);
    }
    bytes
}

#[test]
fn aifc_g726_decodes_lsb_packing_with_and_without_stoc_bitrate_hint() {
    let codes: Vec<u8> = (0..97).map(|index| ((index * 7 + 3) & 31) as u8).collect();
    let packed = pack_g726_lsb(&codes, 5);
    let mut reference = G726Decoder::new(G726Rate::Kbps40, 1).unwrap();
    let expected = reference.decode_packed_lsb(&packed, codes.len()).unwrap();

    for (include_hint, explicit_rate) in [(true, false), (false, true)] {
        let bytes = aifc_g726(&codes, include_hint);
        let mut decoder = if explicit_rate {
            AiffDecoder::new_with_g726_rate(Cursor::new(bytes), G726Rate::Kbps40).unwrap()
        } else {
            AiffDecoder::new(Cursor::new(bytes)).unwrap()
        };
        assert_eq!(
            decoder.spec(),
            AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap()
        );
        let mut actual = Vec::new();
        for count in [1, 2, 7, 13, 3, 31, 40] {
            if actual.len() == expected.len() {
                break;
            }
            let count = count.min(expected.len() - actual.len());
            let block = decoder.read_frames(count).unwrap().unwrap();
            let Samples::I16(samples) = block.samples() else {
                panic!("G.726 must decode to signed 16-bit PCM");
            };
            actual.extend_from_slice(samples);
        }
        assert_eq!(actual, expected);
        assert!(decoder.read_frames(1).unwrap().is_none());
    }
}

#[test]
fn aifc_g726_requires_a_rate_when_no_recognized_hint_is_present() {
    let bytes = aifc_g726(&[0, 1, 2, 3, 4], false);
    assert!(AiffDecoder::new(Cursor::new(bytes.clone())).is_err());
    assert!(AiffDecoder::new_with_g726_rate(Cursor::new(bytes), G726Rate::Kbps40).is_ok());
}

#[test]
fn aifc_g726_requires_the_standard_8khz_sample_rate() {
    let spec = AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap();
    assert!(AiffEncoder::new_aifc_g726(Cursor::new(Vec::new()), spec, G726Rate::Kbps32).is_err());

    let mut bytes = aifc_g726(&[0, 1, 2, 3], true);
    bytes[40..50].copy_from_slice(&[0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0]);
    assert!(AiffDecoder::new(Cursor::new(bytes)).is_err());
}

#[test]
fn aifc_g726_encoder_writes_hint_and_streams_partial_codewords() {
    for channels in [1, 2] {
        let spec = AudioSpec::new(8_000, channels, SampleFormat::PcmI16).unwrap();
        let samples: Vec<_> = (0..103 * usize::from(channels))
            .map(|index| ((index * 1_231) as i16).wrapping_mul(17))
            .collect();
        for rate in [
            G726Rate::Kbps16,
            G726Rate::Kbps24,
            G726Rate::Kbps32,
            G726Rate::Kbps40,
        ] {
            let mut encoder =
                AiffEncoder::new_aifc_g726(Cursor::new(Vec::new()), spec, rate).unwrap();
            let split = 37 * usize::from(channels);
            encoder
                .write_block(
                    &PcmBuffer::new(spec, Samples::I16(samples[..split].to_vec())).unwrap(),
                )
                .unwrap();
            encoder
                .write_block(
                    &PcmBuffer::new(spec, Samples::I16(samples[split..].to_vec())).unwrap(),
                )
                .unwrap();
            let bytes = encoder.finish().unwrap().get_ref().clone();
            assert_eq!(&chunk_body(&bytes, b"COMM")[18..22], b"G726");
            assert_eq!(
                u32::from_be_bytes(chunk_body(&bytes, b"COMM")[2..6].try_into().unwrap()),
                103
            );
            assert_eq!(
                u16::from_be_bytes(chunk_body(&bytes, b"COMM")[0..2].try_into().unwrap()),
                channels
            );
            let mut expected_encoder =
                G726Encoder::new(rate, usize::from(channels), G726Packing::LsbFirst).unwrap();
            let mut packed = expected_encoder.encode(&samples).unwrap();
            packed.extend(expected_encoder.finish());
            let expected = G726Decoder::new(rate, usize::from(channels))
                .unwrap()
                .decode_packed_lsb(&packed, samples.len())
                .unwrap();
            let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
            let mut actual = Vec::new();
            while let Some(block) = decoder.read_frames(19).unwrap() {
                let Samples::I16(block) = block.samples() else {
                    panic!("G.726 must decode to signed 16-bit PCM");
                };
                actual.extend_from_slice(block);
            }
            assert_eq!(actual, expected, "{rate:?}, {channels} channels");
        }
    }
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
fn requested_pcm_bit_depths_write_left_aligned_valid_bits() {
    let u8_spec = AudioSpec::new(48_000, 1, SampleFormat::PcmU8).unwrap();
    let u8_input = PcmBuffer::new(u8_spec, Samples::U8(vec![0, 1, 127, 128, 255])).unwrap();
    let mut encoder =
        AiffEncoder::new_with_bits_per_sample(Cursor::new(Vec::new()), u8_spec, 4).unwrap();
    encoder.write_block(&u8_input).unwrap();
    let encoded = encoder.finish().unwrap().get_ref().clone();
    assert_eq!(
        u16::from_be_bytes(chunk_body(&encoded, b"COMM")[6..8].try_into().unwrap()),
        4
    );
    let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(
        decoder.read_frames(8).unwrap().unwrap(),
        PcmBuffer::new(u8_spec, Samples::U8(vec![0, 0, 112, 128, 240])).unwrap()
    );

    let i16_spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
    let i16_values = vec![i16::MIN, -3_001, -1, 0, 1, 3_001, i16::MAX];
    let i16_expected: Vec<_> = i16_values.iter().map(|value| value & !0x0f).collect();
    let i16_input = PcmBuffer::new(i16_spec, Samples::I16(i16_values)).unwrap();
    let mut encoder =
        AiffEncoder::new_with_bits_per_sample(Cursor::new(Vec::new()), i16_spec, 12).unwrap();
    encoder.write_block(&i16_input).unwrap();
    let encoded = encoder.finish().unwrap().get_ref().clone();
    assert_eq!(
        u16::from_be_bytes(chunk_body(&encoded, b"COMM")[6..8].try_into().unwrap()),
        12
    );
    let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(
        decoder.read_frames(8).unwrap().unwrap(),
        PcmBuffer::new(i16_spec, Samples::I16(i16_expected)).unwrap()
    );

    let i24_spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI24).unwrap();
    let i24_values = vec![-8_388_608, -123_457, -1, 0, 1, 123_457, 8_388_607];
    let i24_expected: Vec<_> = i24_values.iter().map(|value| value & !0x0f).collect();
    let i24_input = PcmBuffer::new(i24_spec, Samples::I24(i24_values)).unwrap();
    let mut encoder =
        AiffEncoder::new_aifc_with_bits_per_sample(Cursor::new(Vec::new()), i24_spec, 20).unwrap();
    encoder.write_block(&i24_input).unwrap();
    let encoded = encoder.finish().unwrap().get_ref().clone();
    assert_eq!(
        u16::from_be_bytes(chunk_body(&encoded, b"COMM")[6..8].try_into().unwrap()),
        20
    );
    let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(
        decoder.read_frames(8).unwrap().unwrap(),
        PcmBuffer::new(i24_spec, Samples::I24(i24_expected)).unwrap()
    );

    let i32_spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI32).unwrap();
    let i32_values = vec![i32::MIN, -123_457, -1, 0, 1, 123_457, i32::MAX];
    let i32_expected: Vec<_> = i32_values.iter().map(|value| value & !0x0f).collect();
    let i32_input = PcmBuffer::new(i32_spec, Samples::I32(i32_values)).unwrap();
    let mut encoder =
        AiffEncoder::new_with_bits_per_sample(Cursor::new(Vec::new()), i32_spec, 28).unwrap();
    encoder.write_block(&i32_input).unwrap();
    let encoded = encoder.finish().unwrap().get_ref().clone();
    assert_eq!(
        u16::from_be_bytes(chunk_body(&encoded, b"COMM")[6..8].try_into().unwrap()),
        28
    );
    let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(
        decoder.read_frames(8).unwrap().unwrap(),
        PcmBuffer::new(i32_spec, Samples::I32(i32_expected)).unwrap()
    );
}

#[test]
fn requested_pcm_bit_depth_must_fit_sample_container() {
    let u8_spec = AudioSpec::new(48_000, 1, SampleFormat::PcmU8).unwrap();
    assert!(AiffEncoder::new_with_bits_per_sample(Cursor::new(Vec::new()), u8_spec, 0).is_err());
    let i16_spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
    assert!(AiffEncoder::new_with_bits_per_sample(Cursor::new(Vec::new()), i16_spec, 8).is_err());
    assert!(
        AiffEncoder::new_aifc_with_bits_per_sample(Cursor::new(Vec::new()), i16_spec, 17).is_err()
    );
    let float_spec = AudioSpec::new(48_000, 1, SampleFormat::Float32).unwrap();
    assert!(
        AiffEncoder::new_aifc_with_bits_per_sample(Cursor::new(Vec::new()), float_spec, 32)
            .is_err()
    );
    let i32_spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI32).unwrap();
    assert!(AiffEncoder::new_with_bits_per_sample(Cursor::new(Vec::new()), i32_spec, 33).is_err());
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
fn aifc_saxels_round_trip_as_opaque_data_across_multiple_chunks() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
    let samples = PcmBuffer::new(spec, Samples::U8(vec![128, 129])).unwrap();
    let mut encoder = AiffEncoder::new_aifc(Cursor::new(Vec::new()), spec).unwrap();
    encoder
        .set_markers(&[
            AiffMarker {
                id: 1,
                position: 0,
                name: b"start".to_vec(),
            },
            AiffMarker {
                id: 2,
                position: 1,
                name: b"next".to_vec(),
            },
        ])
        .unwrap();
    let saxels = [
        AiffSaxel {
            marker_id: 1,
            data: vec![0x10],
        },
        AiffSaxel {
            marker_id: 2,
            data: vec![0x20, 0x30],
        },
    ];
    encoder.set_saxels(&saxels).unwrap();
    encoder.write_block(&samples).unwrap();
    let mut encoded = encoder.finish().unwrap().get_ref().clone();
    assert_eq!(
        chunk_body(&encoded, b"SAXL"),
        &[0, 2, 0, 1, 0, 1, 0x10, 0, 0, 2, 0, 2, 0x20, 0x30]
    );

    append_form_chunk(&mut encoded, b"SAXL", &[0, 1, 0, 1, 0, 1, 0x40, 0]);
    let rewrite_input = encoded.clone();
    let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(
        decoder.saxels(),
        &[
            saxels[0].clone(),
            saxels[1].clone(),
            AiffSaxel {
                marker_id: 1,
                data: vec![0x40],
            }
        ]
    );
    assert_eq!(decoder.read_frames(2).unwrap().unwrap(), samples);

    let mut edited = Metadata::default();
    edited.insert("title", "Edited");
    let mut rewritten = Cursor::new(Vec::new());
    rewrite_metadata(Cursor::new(rewrite_input), &mut rewritten, &edited).unwrap();
    let rewritten = AiffDecoder::new(Cursor::new(rewritten.into_inner())).unwrap();
    assert_eq!(
        rewritten.saxels(),
        &[
            saxels[0].clone(),
            saxels[1].clone(),
            AiffSaxel {
                marker_id: 1,
                data: vec![0x40],
            }
        ]
    );
}

#[test]
fn rejects_malformed_saxel_chunks_and_aiff_saxel_writes() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
    let saxel = AiffSaxel {
        marker_id: 1,
        data: vec![1],
    };
    let mut aiff_encoder = AiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    assert!(
        aiff_encoder
            .set_saxels(std::slice::from_ref(&saxel))
            .is_err()
    );

    let mut encoder = AiffEncoder::new_aifc(Cursor::new(Vec::new()), spec).unwrap();
    encoder
        .write_block(&PcmBuffer::new(spec, Samples::U8(vec![128])).unwrap())
        .unwrap();
    let original = encoder.finish().unwrap().get_ref().clone();
    for payload in [
        &[][..],
        &[0, 1][..],
        &[0, 1, 0, 1, 0][..],
        &[0, 1, 0, 1, 0, 1][..],
    ] {
        let mut malformed = original.clone();
        append_form_chunk(&mut malformed, b"SAXL", payload);
        assert!(AiffDecoder::new(Cursor::new(malformed)).is_err());
    }
    let mut bad_id = original.clone();
    append_form_chunk(&mut bad_id, b"SAXL", &[0, 1, 0, 0, 0, 0]);
    assert!(AiffDecoder::new(Cursor::new(bad_id)).is_err());

    let mut missing_marker = original;
    append_form_chunk(&mut missing_marker, b"SAXL", &[0, 1, 0, 3, 0, 0]);
    assert!(AiffDecoder::new(Cursor::new(missing_marker)).is_err());
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
fn tolerates_only_a_missing_final_odd_chunk_pad_byte() {
    let mut missing_pad = aifc_g711(b"ULAW", &[0xff]);
    missing_pad.pop();
    let mut decoder = AiffDecoder::new(Cursor::new(missing_pad)).unwrap();
    let block = decoder.read_frames(1).unwrap().unwrap();
    let Samples::I16(samples) = block.samples() else {
        panic!("μ-law audio must decode to signed 16-bit PCM");
    };
    assert_eq!(samples, &[decode_mulaw(0xff)]);

    let mut missing_audio = aifc_g711(b"ULAW", &[0xff, 0x7f]);
    missing_audio.pop();
    assert!(AiffDecoder::new(Cursor::new(missing_audio)).is_err());

    let mut overrun = aifc_g711(b"ULAW", &[0xff]);
    let declared_form_size = u32::from_be_bytes(overrun[4..8].try_into().unwrap()) + 2;
    overrun[4..8].copy_from_slice(&declared_form_size.to_be_bytes());
    assert!(AiffDecoder::new(Cursor::new(overrun)).is_err());
}

#[test]
fn decodes_aifc_with_underreported_comm_size_for_none() {
    let mut bytes = b"FORM\0\0\0\0AIFC".to_vec();
    append_chunk(&mut bytes, b"FVER", &0xA280_5140_u32.to_be_bytes());
    let mut comm = Vec::from(1_u16.to_be_bytes());
    comm.extend_from_slice(&1_u32.to_be_bytes());
    comm.extend_from_slice(&16_u16.to_be_bytes());
    comm.extend_from_slice(&[0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0]);
    comm.extend_from_slice(b"NONE\x0enot compressed");
    append_chunk(&mut bytes, b"COMM", &comm);
    let mut sound = vec![0; 8];
    sound.extend_from_slice(&[0x12, 0x34]);
    append_chunk(&mut bytes, b"SSND", &sound);
    let form_size = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&form_size.to_be_bytes());

    let comm = bytes
        .windows(4)
        .position(|window| window == b"COMM")
        .unwrap();
    bytes[comm + 4..comm + 8].copy_from_slice(&18_u32.to_be_bytes());
    let mut invalid = bytes.clone();
    invalid[comm + 4..comm + 8].copy_from_slice(&19_u32.to_be_bytes());

    let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
    assert!(decoder.is_aifc());
    assert_eq!(
        decoder.read_frames(1).unwrap().unwrap().samples(),
        &Samples::I16(vec![0x1234])
    );

    assert!(AiffDecoder::new(Cursor::new(invalid)).is_err());
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
fn decodes_aifc_little_endian_and_float_compression_codes() {
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

    for compression in [b"fl32", b"FL32"] {
        let bytes = aifc(compression, 32, &[0x3f, 0x80, 0, 0, 0xbf, 0, 0, 0]);
        let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::F32(vec![1.0, -0.5]),
            "{compression:?}"
        );
    }

    for compression in [b"fl64", b"FL64"] {
        let bytes = aifc(compression, 64, &[0x3f, 0xf0, 0, 0, 0, 0, 0, 0]);
        let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            decoder.read_frames(1).unwrap().unwrap().samples(),
            &Samples::F64(vec![1.0]),
            "{compression:?}"
        );
    }
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
fn aifc_ima4_rejects_channel_counts_outside_mono_and_stereo() {
    let spec = AudioSpec::new(8_000, 3, SampleFormat::PcmI16).unwrap();
    assert!(AiffEncoder::new_aifc_ima4(Cursor::new(Vec::new()), spec).is_err());

    let mut bytes = aifc(b"ima4", 16, &[0; 3 * 34]);
    bytes[32..34].copy_from_slice(&3_u16.to_be_bytes());
    bytes[34..38].copy_from_slice(&1_u32.to_be_bytes());
    assert!(AiffDecoder::new(Cursor::new(bytes)).is_err());
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
fn writes_non_native_metadata_to_id3_chunk() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("album", "Álbum");
    metadata.insert("title", "Títle");
    let mut encoder =
        AiffEncoder::new_with_metadata(Cursor::new(Vec::new()), spec, &metadata).unwrap();
    encoder
        .write_block(&PcmBuffer::new(spec, Samples::U8(vec![128])).unwrap())
        .unwrap();
    let bytes = encoder.finish().unwrap().get_ref().clone();
    assert!(chunk_body(&bytes, b"ID3 ").starts_with(b"ID3\x04"));
    let decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.metadata(), &metadata);
}

#[test]
fn encoder_retains_opaque_id3_frames_when_adding_text() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap();
    let tag = id3_with_private_frame();
    let mut metadata = Metadata::default();
    metadata.insert("album", "Album");
    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_id3_tag(&tag).unwrap();
    encoder.set_metadata(&metadata).unwrap();
    encoder
        .write_block(&PcmBuffer::new(spec, Samples::U8(vec![128])).unwrap())
        .unwrap();
    let bytes = encoder.finish().unwrap().get_ref().clone();
    let decoder = AiffDecoder::new(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(decoder.metadata(), &metadata);
    assert!(
        chunk_body(&bytes, b"ID3 ")
            .windows(4)
            .any(|frame| frame == b"PRIV")
    );
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
    append_form_chunk(&mut input, b"ID3 ", &id3_with_private_frame());
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
    new_tags.insert("album", "New album");
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
    assert!(
        chunk_body(&output, b"ID3 ")
            .windows(4)
            .any(|frame| frame == b"PRIV")
    );
    let mut decoder = AiffDecoder::new(Cursor::new(output)).unwrap();
    assert_eq!(decoder.metadata(), &new_tags);
    assert_eq!(decoder.read_frames(3).unwrap().unwrap(), original);
}

#[test]
fn rewrites_aifc_tags_without_changing_audio_or_unknown_chunks() {
    let original = PcmBuffer::new(
        AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![-12_345, 0, 23_456]),
    )
    .unwrap();
    let mut old_tags = Metadata::default();
    old_tags.insert("title", "Old title");
    let mut encoder = AiffEncoder::new_aifc_sowt_with_metadata(
        Cursor::new(Vec::new()),
        original.spec(),
        &old_tags,
    )
    .unwrap();
    encoder.write_block(&original).unwrap();
    let mut input = encoder.finish().unwrap().get_ref().clone();
    append_form_chunk(&mut input, b"TEST", &[1, 2, 3]);
    let original_audio = chunk_body(&input, b"SSND")[8..].to_vec();

    let mut new_tags = Metadata::default();
    new_tags.insert("title", "New title");
    new_tags.insert("album", "New album");
    let mut output = Cursor::new(Vec::new());
    rewrite_metadata(Cursor::new(input), &mut output, &new_tags).unwrap();
    let output = output.into_inner();
    assert_eq!(chunk_body(&output, b"TEST"), [1, 2, 3]);
    assert_eq!(chunk_body(&output, b"SSND")[8..], original_audio);
    let mut decoder = AiffDecoder::new(Cursor::new(output)).unwrap();
    assert_eq!(decoder.metadata(), &new_tags);
    assert_eq!(decoder.read_frames(3).unwrap().unwrap(), original);
}

#[test]
fn copyright_chunk_round_trips_and_rewrites_without_changing_audio() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![-123, 456])).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("copyright", "1988 Apple Computer, Inc.");
    let mut encoder =
        AiffEncoder::new_with_metadata(Cursor::new(Vec::new()), spec, &metadata).unwrap();
    encoder.write_block(&audio).unwrap();
    let mut input = encoder.finish().unwrap().get_ref().clone();
    assert_eq!(chunk_body(&input, b"(c) "), b"1988 Apple Computer, Inc.");
    assert_eq!(
        AiffDecoder::new(Cursor::new(input.clone()))
            .unwrap()
            .metadata(),
        &metadata
    );

    let mut edited = Metadata::default();
    edited.insert("copyright", "2026 Zemer");
    let mut output = Cursor::new(Vec::new());
    rewrite_metadata(Cursor::new(input.clone()), &mut output, &edited).unwrap();
    input = output.into_inner();
    assert_eq!(chunk_body(&input, b"(c) "), b"2026 Zemer");
    let mut decoder = AiffDecoder::new(Cursor::new(input)).unwrap();
    assert_eq!(decoder.metadata(), &edited);
    assert_eq!(decoder.read_frames(2).unwrap().unwrap(), audio);
}

#[test]
fn decoder_rejects_duplicate_copyright_chunks() {
    let mut bytes = AiffEncoder::new(
        Cursor::new(Vec::new()),
        AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap(),
    )
    .unwrap()
    .finish()
    .unwrap()
    .get_ref()
    .clone();
    append_form_chunk(&mut bytes, b"(c) ", b"first");
    append_form_chunk(&mut bytes, b"(c) ", b"second");
    assert!(AiffDecoder::new(Cursor::new(bytes)).is_err());
}

#[test]
fn aifc_audio_recording_roundtrips_and_rejects_invalid_chunks() {
    let spec = AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap();
    let recording = AiffAudioRecording {
        channel_status: std::array::from_fn(|index| index as u8),
    };
    let mut encoder = AiffEncoder::new_aifc(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_audio_recording(&recording).unwrap();
    let mut bytes = encoder.finish().unwrap().get_ref().clone();
    assert_eq!(chunk_body(&bytes, b"AESD"), &recording.channel_status);
    assert_eq!(
        AiffDecoder::new(Cursor::new(bytes.clone()))
            .unwrap()
            .audio_recording(),
        Some(&recording)
    );

    append_form_chunk(&mut bytes, b"AESD", &recording.channel_status);
    assert!(AiffDecoder::new(Cursor::new(bytes)).is_err());

    let mut empty = AiffEncoder::new_aifc(Cursor::new(Vec::new()), spec).unwrap();
    let mut bytes = empty.finish().unwrap().get_ref().clone();
    append_form_chunk(&mut bytes, b"AESD", &[0; 23]);
    assert!(AiffDecoder::new(Cursor::new(bytes)).is_err());

    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    assert!(encoder.set_audio_recording(&recording).is_err());
}

#[test]
fn aiff_metadata_rewrite_rejects_multiple_single_value_tags() {
    let mut tags = Metadata::default();
    tags.insert("artist", "First");
    tags.insert("artist", "Second");
    let mut output = Cursor::new(Vec::new());
    assert!(rewrite_metadata(Cursor::new(Vec::new()), &mut output, &tags).is_err());
    assert!(output.into_inner().is_empty());

    tags.remove("artist");
    tags.insert("copyright", "First");
    tags.insert("copyright", "Second");
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
