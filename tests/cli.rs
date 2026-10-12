use std::{
    fs::{self, File},
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use zemer_core::{AudioDecoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};
use zemer_wav::{write_wav, write_wav_with_metadata};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn cli_path() -> std::ffi::OsString {
    env!("CARGO_BIN_EXE_zemer_test_cli").into()
}

fn temporary_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "zemer-cli-{}-{}.wav",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn inspect_json_reports_pcm_properties_and_exact_duration() {
    let path = temporary_path();
    let audio = PcmBuffer::new(
        AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap(),
        Samples::U8(vec![128]),
    )
    .unwrap();
    write_wav(File::create(&path).unwrap(), &audio).unwrap();

    let output = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&path)
        .output()
        .unwrap();
    let _ = fs::remove_file(&path);

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        r#"{"format":"wav","container":"riff","sample_rate":8000,"channels":1,"speaker_mask":null,"sample_format":"pcm_u8","frames":1,"duration_frames":{"numerator":1,"denominator":8000},"metadata":{}}"#
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn inspect_json_reports_extensible_speaker_mask() {
    let path = temporary_path();
    let spec =
        AudioSpec::new_with_speaker_mask(48_000, 2, SampleFormat::PcmI16, Some(0x3)).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![0, 0])).unwrap();
    write_wav(File::create(&path).unwrap(), &audio).unwrap();

    let output = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&path)
        .output()
        .unwrap();
    let _ = fs::remove_file(&path);

    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("\"speaker_mask\":3")
    );
}

#[test]
fn inspect_json_reports_machine_readable_io_errors() {
    let path = temporary_path();
    let output = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with(r#"{"error":{"kind":"io","message":""#));
    assert!(stderr.ends_with("\"}}\n"));
    assert!(stderr.contains(path.file_name().unwrap().to_string_lossy().as_ref()));
}

#[test]
fn inspect_json_reports_malformed_wave_errors() {
    let path = temporary_path();
    fs::write(&path, b"not a wave file").unwrap();
    let output = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&path)
        .output()
        .unwrap();
    let _ = fs::remove_file(&path);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with(r#"{"error":{"kind":"invalid_format","message":""#));
    assert!(stderr.contains(path.file_name().unwrap().to_string_lossy().as_ref()));
    assert!(stderr.ends_with("\"}}\n"));
}

#[test]
fn convert_changes_sample_format_and_preserves_metadata() {
    let input_path = temporary_path();
    let output_path = temporary_path();
    let audio = PcmBuffer::new(
        AudioSpec::new(8_000, 1, SampleFormat::Float32).unwrap(),
        Samples::F32(vec![-1.0, 0.0, 1.0]),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "Endian test");
    metadata.insert("artist", "Artist A");
    metadata.insert("artist", "Artist B");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    let output = Command::new(cli_path())
        .args(["convert", "--format", "pcm_i16", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = String::from_utf8(output.stdout).unwrap();
    assert!(json.contains("\"sample_format\":\"pcm_i16\""));
    assert!(json.contains("\"TITLE\":[\"Endian test\"]"));
    assert!(json.contains("\"ARTIST\":[\"Artist A\",\"Artist B\"]"));

    let mut decoder = zemer_wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);
    assert_eq!(
        decoder.read_frames(3).unwrap().unwrap().samples(),
        &Samples::I16(vec![i16::MIN, 0, i16::MAX])
    );
    assert_eq!(decoder.metadata(), &metadata);
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn convert_can_explicitly_write_rf64() {
    let input_path = temporary_path();
    let output_path = temporary_path();
    let audio = PcmBuffer::new(
        AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![123]),
    )
    .unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();

    let output = Command::new(cli_path())
        .args([
            "convert",
            "--format",
            "pcm_i16",
            "--container",
            "rf64",
            "--json",
        ])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("\"container\":\"rf64\"")
    );

    let bytes = fs::read(&output_path).unwrap();
    assert_eq!(&bytes[..4], b"RF64");
    let mut decoder = zemer_wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.container(), zemer_wav::WavContainer::Rf64);
    assert_eq!(decoder.read_frames(1).unwrap().unwrap(), audio);
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn convert_does_not_overwrite_an_existing_output() {
    let input_path = temporary_path();
    let output_path = temporary_path();
    let audio = PcmBuffer::new(
        AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![1]),
    )
    .unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();
    fs::write(&output_path, b"keep this file").unwrap();

    let output = Command::new(cli_path())
        .args(["convert", "--format", "float32"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(fs::read(&output_path).unwrap(), b"keep this file");
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn convert_encodes_g711_only_when_explicitly_selected() {
    let input_path = temporary_path();
    let audio = PcmBuffer::new(
        AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![i16::MIN, 0, i16::MAX]),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "Voice sample");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    for (format, tag) in [("g711_alaw", 6_u16), ("g711_mulaw", 7_u16)] {
        let output_path = temporary_path();
        let output = Command::new(cli_path())
            .args(["convert", "--format", format, "--json"])
            .arg(&input_path)
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json = String::from_utf8(output.stdout).unwrap();
        assert!(json.contains(&format!("\"output_encoding\":\"{format}\"")));

        let bytes = fs::read(&output_path).unwrap();
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), tag);
        let mut decoder = zemer_wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
        assert_eq!(decoder.metadata(), &metadata);
        assert_eq!(decoder.read_frames(3).unwrap().unwrap().frames(), 3);
        let _ = fs::remove_file(output_path);
    }
    let _ = fs::remove_file(input_path);
}
