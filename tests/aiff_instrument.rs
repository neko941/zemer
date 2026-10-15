use std::{
    fs::{self, File},
    io::Cursor,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use aiff::{AiffDecoder, AiffEncoder, AiffInstrument, AiffLoop, AiffLoopMode, AiffMarker};
use zemer_core::{AudioSpec, PcmBuffer, SampleFormat, Samples};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn instrument() -> AiffInstrument {
    AiffInstrument {
        base_note: 57,
        detune: -12,
        low_note: 24,
        high_note: 96,
        low_velocity: 2,
        high_velocity: 120,
        gain: -3,
        sustain_loop: AiffLoop {
            mode: AiffLoopMode::Forward,
            begin_marker: 1,
            end_marker: 2,
        },
        release_loop: AiffLoop::default(),
    }
}

fn markers() -> [AiffMarker; 2] {
    [
        AiffMarker {
            id: 1,
            position: 1,
            name: b"loop-start".to_vec(),
        },
        AiffMarker {
            id: 2,
            position: 3,
            name: b"loop-end".to_vec(),
        },
    ]
}

fn sample() -> PcmBuffer {
    let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
    PcmBuffer::new(spec, Samples::I16(vec![1, 2, 3, 4])).unwrap()
}

fn source_aiff() -> Vec<u8> {
    let audio = sample();
    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), audio.spec()).unwrap();
    encoder.set_markers(&markers()).unwrap();
    encoder.set_instrument(&instrument()).unwrap();
    encoder.write_block(&audio).unwrap();
    encoder.finish().unwrap().get_ref().clone()
}

#[test]
fn typed_instrument_chunk_round_trips_in_aiff_and_aifc() {
    let audio = sample();
    let expected_instrument = instrument();
    let expected_markers = markers();

    let mut aiff = AiffEncoder::new(Cursor::new(Vec::new()), audio.spec()).unwrap();
    aiff.set_markers(&expected_markers).unwrap();
    aiff.set_instrument(&expected_instrument).unwrap();
    aiff.write_block(&audio).unwrap();
    let aiff = aiff.finish().unwrap().get_ref().clone();

    let mut aifc = AiffEncoder::new_aifc(Cursor::new(Vec::new()), audio.spec()).unwrap();
    aifc.set_markers(&expected_markers).unwrap();
    aifc.set_instrument(&expected_instrument).unwrap();
    aifc.write_block(&audio).unwrap();
    let aifc = aifc.finish().unwrap().get_ref().clone();

    for bytes in [aiff, aifc] {
        let decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.instrument(), Some(&expected_instrument));
        assert_eq!(decoder.markers(), expected_markers);
    }
}

#[test]
fn instrument_loops_must_reference_existing_markers() {
    let audio = sample();
    let mut encoder = AiffEncoder::new(Cursor::new(Vec::new()), audio.spec()).unwrap();
    encoder.set_instrument(&instrument()).unwrap();
    encoder.write_block(&audio).unwrap();
    assert!(encoder.finish().is_err());
}

#[test]
fn cli_conversion_and_metadata_edit_preserve_instrument_chunk() {
    let suffix = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let input = temp_file("input", suffix);
    let converted = temp_file("converted", suffix);
    let edited = temp_file("edited", suffix);
    fs::write(&input, source_aiff()).unwrap();

    let conversion = Command::new(env!("CARGO_BIN_EXE_zemer_test_cli"))
        .args([
            "convert",
            "--format",
            "aifc",
            input.to_str().unwrap(),
            converted.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(conversion.status.success(), "{:?}", conversion);

    let metadata_edit = Command::new(env!("CARGO_BIN_EXE_zemer_test_cli"))
        .args([
            "metadata",
            "edit",
            input.to_str().unwrap(),
            edited.to_str().unwrap(),
            "--set",
            "TITLE=Instrument",
        ])
        .output()
        .unwrap();
    assert!(metadata_edit.status.success(), "{:?}", metadata_edit);

    for path in [&converted, &edited] {
        let decoder = AiffDecoder::new(File::open(path).unwrap()).unwrap();
        assert_eq!(decoder.instrument(), Some(&instrument()));
        assert_eq!(decoder.markers(), markers());
    }

    fs::remove_file(input).unwrap();
    fs::remove_file(converted).unwrap();
    fs::remove_file(edited).unwrap();
}

fn temp_file(label: &str, suffix: u64) -> PathBuf {
    std::env::temp_dir().join(format!(
        "zemer-aiff-inst-{label}-{}-{suffix}.aif",
        std::process::id()
    ))
}
