use std::io::Cursor;

use caf::{CafCodec, CafDecoder, CafEncoder, rewrite_metadata};
use g711::{G711Encoding, decode_alaw, decode_mulaw, encode_alaw, encode_mulaw};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples,
};

fn pcm_roundtrip(spec: AudioSpec, samples: Samples, little: bool, packed24: bool) {
    let audio = PcmBuffer::new(spec, samples.clone()).unwrap();
    let mut encoder =
        CafEncoder::new_pcm_with_byte_order(Cursor::new(Vec::new()), spec, little, packed24)
            .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "CAF title");
    metadata.insert("artist", "One");
    metadata.insert("artist", "Two");
    encoder.set_metadata(&metadata).unwrap();
    encoder.write_block(&audio).unwrap();
    let bytes = encoder.into_inner().unwrap().into_inner();

    let mut decoder = CafDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.codec(), CafCodec::Pcm);
    assert_eq!(decoder.spec(), spec);
    assert_eq!(decoder.metadata(), &metadata);
    let decoded = decoder.read_frames(1024).unwrap().unwrap();
    assert_eq!(decoded.samples(), &samples);
    assert!(decoder.read_frames(1).unwrap().is_none());
}

#[test]
fn pcm_integer_and_float_storage_round_trips_in_both_byte_orders() {
    for little in [false, true] {
        pcm_roundtrip(
            AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap(),
            Samples::I16(vec![i16::MIN, -1, 0, i16::MAX]),
            little,
            true,
        );
        pcm_roundtrip(
            AudioSpec::new(48_000, 1, SampleFormat::PcmI24).unwrap(),
            Samples::I24(vec![-8_388_608, -1, 0, 8_388_607]),
            little,
            true,
        );
        pcm_roundtrip(
            AudioSpec::new(48_000, 1, SampleFormat::PcmI24).unwrap(),
            Samples::I24(vec![-8_388_608, -1, 0, 8_388_607]),
            little,
            false,
        );
        pcm_roundtrip(
            AudioSpec::new(48_000, 1, SampleFormat::PcmI32).unwrap(),
            Samples::I32(vec![i32::MIN, -1, 0, i32::MAX]),
            little,
            true,
        );
        pcm_roundtrip(
            AudioSpec::new(48_000, 1, SampleFormat::Float32).unwrap(),
            Samples::F32(vec![f32::from_bits(0x8000_0000), 0.5, -1.0]),
            little,
            true,
        );
        pcm_roundtrip(
            AudioSpec::new(48_000, 1, SampleFormat::Float64).unwrap(),
            Samples::F64(vec![f64::from_bits(0x8000_0000_0000_0000), 0.5, -1.0]),
            little,
            true,
        );
    }
}

#[test]
fn bitmap_channel_layout_round_trips_through_audio_spec() {
    let spec =
        AudioSpec::new_with_speaker_mask(48_000, 6, SampleFormat::PcmI16, Some(0x3f)).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16((0..12).collect())).unwrap();
    let mut encoder = CafEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_block(&audio).unwrap();
    let bytes = encoder.into_inner().unwrap().into_inner();
    let mut decoder = CafDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.spec(), spec);
    assert_eq!(decoder.read_frames(10).unwrap().unwrap(), audio);
}

#[test]
fn g711_profiles_round_trip_decoded_samples_and_metadata() {
    let spec = AudioSpec::new(8_000, 2, SampleFormat::PcmI16).unwrap();
    let audio =
        PcmBuffer::new(spec, Samples::I16(vec![i16::MIN, -1, 0, 1, i16::MAX, 128])).unwrap();
    let Samples::I16(source) = audio.samples() else {
        panic!("test input must be I16");
    };
    for encoding in [G711Encoding::ALaw, G711Encoding::MuLaw] {
        let expected = source
            .iter()
            .map(|&sample| match encoding {
                G711Encoding::ALaw => decode_alaw(encode_alaw(sample)),
                G711Encoding::MuLaw => decode_mulaw(encode_mulaw(sample)),
            })
            .collect::<Vec<_>>();
        let mut encoder = CafEncoder::new_g711(Cursor::new(Vec::new()), spec, encoding).unwrap();
        encoder.write_block(&audio).unwrap();
        let bytes = encoder.into_inner().unwrap().into_inner();
        let mut decoder = CafDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            decoder.codec(),
            match encoding {
                G711Encoding::ALaw => CafCodec::ALaw,
                G711Encoding::MuLaw => CafCodec::MuLaw,
            }
        );
        assert_eq!(
            decoder.read_frames(16).unwrap().unwrap().samples(),
            &Samples::I16(expected)
        );
    }
}

#[test]
fn ima4_packet_table_trims_final_packet_padding() {
    let spec = AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap();
    let samples: Vec<_> = (0..130)
        .map(|index| ((index as f32 * 0.19).sin() * 4_000.0) as i16)
        .collect();
    let audio = PcmBuffer::new(spec, Samples::I16(samples.clone())).unwrap();
    let mut encoder = CafEncoder::new_ima4(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_block(&audio).unwrap();
    let bytes = encoder.into_inner().unwrap().into_inner();
    let packet_table = bytes.windows(4).position(|chunk| chunk == b"pakt").unwrap();
    assert_eq!(
        &bytes[packet_table + 12..packet_table + 20],
        &0_i64.to_be_bytes()
    );
    let mut decoder = CafDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.codec(), CafCodec::Ima4);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(17).unwrap() {
        let Samples::I16(block) = block.samples() else {
            panic!("IMA4 must decode to signed 16-bit PCM");
        };
        decoded.extend_from_slice(block);
    }
    assert_eq!(decoded.len(), samples.len());
    let max_error = samples
        .iter()
        .zip(decoded)
        .map(|(source, decoded)| (i32::from(*source) - i32::from(decoded)).abs())
        .max()
        .unwrap();
    assert!(max_error < 4_000, "maximum IMA4 error was {max_error}");
}

#[test]
fn rejects_partial_pcm_frames_and_invalid_ima4_packet_tables() {
    let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
    let mut bytes = CafEncoder::new(Cursor::new(Vec::new()), spec)
        .unwrap()
        .into_inner()
        .unwrap()
        .into_inner();
    bytes.truncate(bytes.len() - 1);
    assert!(CafDecoder::new(Cursor::new(bytes)).is_err());

    let spec = AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap();
    let mut bytes = CafEncoder::new_ima4(Cursor::new(Vec::new()), spec)
        .unwrap()
        .into_inner()
        .unwrap()
        .into_inner();
    bytes[64..72].copy_from_slice(&1_i64.to_be_bytes());
    assert!(CafDecoder::new(Cursor::new(bytes)).is_err());
}

#[test]
fn metadata_rewrite_preserves_ima4_audio_packets() {
    let spec = AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![1, 2, 3, 4, 5, 6, 7])).unwrap();
    let mut encoder = CafEncoder::new_ima4(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_block(&audio).unwrap();
    let original = encoder.into_inner().unwrap().into_inner();
    let data_start = original
        .windows(4)
        .position(|chunk| chunk == b"data")
        .unwrap()
        + 16;
    let original_audio = original[data_start..data_start + 34].to_vec();
    let mut edited = Metadata::default();
    edited.insert("title", "Edited");
    let mut output = Cursor::new(Vec::new());
    rewrite_metadata(Cursor::new(original), &mut output, &edited).unwrap();
    let rewritten = output.into_inner();
    let new_data_start = rewritten
        .windows(4)
        .position(|chunk| chunk == b"data")
        .unwrap()
        + 16;
    assert_eq!(
        &rewritten[new_data_start..new_data_start + 34],
        original_audio
    );
    let decoder = CafDecoder::new(Cursor::new(rewritten)).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["Edited"]);
}

#[test]
fn metadata_rewrite_finalizes_a_terminal_unknown_size_data_chunk() {
    let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![1, -2, 3])).unwrap();
    let mut encoder = CafEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_block(&audio).unwrap();
    let mut original = encoder.into_inner().unwrap().into_inner();
    let data_header = original
        .windows(4)
        .position(|chunk| chunk == b"data")
        .unwrap();
    original[data_header + 4..data_header + 12].copy_from_slice(&(-1_i64).to_be_bytes());

    let mut edited = Metadata::default();
    edited.insert("title", "Unknown-size source");
    let mut output = Cursor::new(Vec::new());
    rewrite_metadata(Cursor::new(original), &mut output, &edited).unwrap();
    let rewritten = output.into_inner();
    let mut decoder = CafDecoder::new(Cursor::new(rewritten)).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["Unknown-size source"]);
    assert_eq!(decoder.read_frames(3).unwrap().unwrap(), audio);
    assert!(decoder.read_frames(1).unwrap().is_none());
}
