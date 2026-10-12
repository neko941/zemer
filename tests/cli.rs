use std::{
    fs::{self, File},
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use wav::{write_wav, write_wav_with_metadata};
use wavpack::WavPackDecoder;
use zemer_core::{AudioDecoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

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

fn flac_comment_block(vendor: &[u8], fields: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut data = Vec::from((vendor.len() as u32).to_le_bytes());
    data.extend_from_slice(vendor);
    data.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    for (key, value) in fields {
        let mut field = Vec::from(*key);
        field.push(b'=');
        field.extend_from_slice(value);
        data.extend_from_slice(&(field.len() as u32).to_le_bytes());
        data.extend_from_slice(&field);
    }
    let length = data.len();
    let mut block = vec![
        0x84,
        (length >> 16) as u8,
        (length >> 8) as u8,
        length as u8,
    ];
    block.extend_from_slice(&data);
    block
}

fn flac_with_comments() -> Vec<u8> {
    let mut data = vec![
        0x66, 0x4c, 0x61, 0x43, 0x80, 0x00, 0x00, 0x22, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00, 0x0f,
        0x00, 0x00, 0x0f, 0x0a, 0xc4, 0x42, 0xf0, 0x00, 0x00, 0x00, 0x01, 0x3e, 0x84, 0xb4, 0x18,
        0x07, 0xdc, 0x69, 0x03, 0x07, 0x58, 0x6a, 0x3d, 0xad, 0x1a, 0x2e, 0x0f, 0xff, 0xf8, 0x69,
        0x18, 0x00, 0x00, 0xbf, 0x03, 0x58, 0xfd, 0x03, 0x12, 0x8b, 0xaa, 0x9a,
    ];
    data[4] = 0;
    let frame = data.split_off(42);
    data.extend(flac_comment_block(
        b"test encoder",
        &[
            (b"TITLE", b"Old title"),
            (b"ALBUM", b"Keep this"),
            (b"ARTIST", b"Original artist"),
            (b"ARTIST", b"Guest artist"),
            (b"COMMENT", b"Remove this"),
        ],
    ));
    data.extend(frame);
    data
}

fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut crc = 0_u32;
    for &byte in bytes {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn ogg_page(packet: &[u8], flags: u8, sequence: u32, granule: u64) -> Vec<u8> {
    let mut lacing = Vec::new();
    let mut remaining = packet.len();
    while remaining >= 255 {
        lacing.push(255);
        remaining -= 255;
    }
    lacing.push(remaining as u8);
    let mut page = vec![0; 27];
    page[..4].copy_from_slice(b"OggS");
    page[5] = flags;
    page[6..14].copy_from_slice(&granule.to_le_bytes());
    page[14..18].copy_from_slice(&7_u32.to_le_bytes());
    page[18..22].copy_from_slice(&sequence.to_le_bytes());
    page[26] = lacing.len() as u8;
    page.extend_from_slice(&lacing);
    page.extend_from_slice(packet);
    let crc = ogg_crc(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

fn ogg_flac_with_rfc_frame() -> Vec<u8> {
    let native = [
        0x66, 0x4c, 0x61, 0x43, 0x80, 0x00, 0x00, 0x22, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00, 0x0f,
        0x00, 0x00, 0x0f, 0x0a, 0xc4, 0x42, 0xf0, 0x00, 0x00, 0x00, 0x01, 0x3e, 0x84, 0xb4, 0x18,
        0x07, 0xdc, 0x69, 0x03, 0x07, 0x58, 0x6a, 0x3d, 0xad, 0x1a, 0x2e, 0x0f, 0xff, 0xf8, 0x69,
        0x18, 0x00, 0x00, 0xbf, 0x03, 0x58, 0xfd, 0x03, 0x12, 0x8b, 0xaa, 0x9a,
    ];
    let mut mapping = Vec::from(&b"\x7fFLAC\x01\x00\x00\x01fLaC"[..]);
    let mut streaminfo_header = native[4..8].to_vec();
    streaminfo_header[0] &= 0x7f;
    mapping.extend_from_slice(&streaminfo_header);
    mapping.extend_from_slice(&native[8..42]);
    let mut stream = ogg_page(&mapping, 2, 0, 0);
    let mut comment = vec![4, 0, 0, 8];
    comment.extend_from_slice(&[0; 8]);
    stream.extend(ogg_page(&comment, 0, 1, 0));
    stream.extend(ogg_page(&native[42..], 4, 2, 1));
    stream
}

fn temporary_directory() -> PathBuf {
    std::env::temp_dir().join(format!(
        "zemer-cli-dir-{}-{}",
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
fn wavpack_inspect_convert_and_metadata_edit_use_trailing_apev2() {
    let input_path = temporary_path();
    let wav_path = temporary_path();
    let tagged_path = temporary_path();
    fs::write(
        &input_path,
        include_bytes!("../crates/wavpack/tests/fixtures/tagged-custom-rate-stereo.wv"),
    )
    .unwrap();

    let inspected = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&input_path)
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    assert!(String::from_utf8_lossy(&inspected.stdout).contains("\"format\":\"wavpack\""));
    assert!(String::from_utf8_lossy(&inspected.stdout).contains("\"TITLE\":["));

    let converted = Command::new(cli_path())
        .args(["convert", "--format", "pcm_i16", "--json"])
        .arg(&input_path)
        .arg(&wav_path)
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    let mut source = WavPackDecoder::new_seekable(File::open(&input_path).unwrap()).unwrap();
    let source_audio = source.read_frames(8192).unwrap();
    let mut converted = wav::WavDecoder::new(File::open(&wav_path).unwrap()).unwrap();
    assert_eq!(source_audio, converted.read_frames(8192).unwrap());

    let edited = Command::new(cli_path())
        .args([
            "metadata",
            "edit",
            "--set",
            "ARTIST=Zemer",
            "--remove",
            "TITLE",
            "--json",
        ])
        .arg(&input_path)
        .arg(&tagged_path)
        .output()
        .unwrap();
    assert!(
        edited.status.success(),
        "{}",
        String::from_utf8_lossy(&edited.stderr)
    );
    let mut edited_decoder =
        WavPackDecoder::new_seekable(File::open(&tagged_path).unwrap()).unwrap();
    assert!(edited_decoder.metadata().values("TITLE").is_empty());
    assert_eq!(edited_decoder.metadata().values("ARTIST"), ["Zemer"]);
    let mut source_again = WavPackDecoder::new_seekable(File::open(&input_path).unwrap()).unwrap();
    assert_eq!(
        source_again.read_frames(8192).unwrap(),
        edited_decoder.read_frames(8192).unwrap()
    );

    for path in [input_path, wav_path, tagged_path] {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn inspect_and_convert_native_flac_by_stream_marker() {
    let input_path = temporary_path();
    let output_path = temporary_path();
    fs::write(
        &input_path,
        [
            0x66, 0x4c, 0x61, 0x43, 0x80, 0x00, 0x00, 0x22, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00,
            0x0f, 0x00, 0x00, 0x0f, 0x0a, 0xc4, 0x42, 0xf0, 0x00, 0x00, 0x00, 0x01, 0x3e, 0x84,
            0xb4, 0x18, 0x07, 0xdc, 0x69, 0x03, 0x07, 0x58, 0x6a, 0x3d, 0xad, 0x1a, 0x2e, 0x0f,
            0xff, 0xf8, 0x69, 0x18, 0x00, 0x00, 0xbf, 0x03, 0x58, 0xfd, 0x03, 0x12, 0x8b, 0xaa,
            0x9a,
        ],
    )
    .unwrap();

    let inspected = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&input_path)
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    assert_eq!(
        String::from_utf8(inspected.stdout).unwrap().trim(),
        r#"{"format":"flac","container":null,"sample_rate":44100,"channels":2,"speaker_mask":null,"sample_format":"pcm_i16","frames":1,"duration_frames":{"numerator":1,"denominator":44100},"metadata":{}}"#
    );

    let converted = Command::new(cli_path())
        .args(["convert", "--format", "pcm_i16", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    let mut decoder = wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(
        decoder.read_frames(1).unwrap().unwrap().samples(),
        &Samples::I16(vec![25_588, 10_416])
    );
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn inspect_ogg_flac_by_container_signature() {
    let input_path = temporary_path();
    let output_path = temporary_path();
    fs::write(&input_path, ogg_flac_with_rfc_frame()).unwrap();
    let output = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&input_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        r#"{"format":"ogg-flac","container":null,"sample_rate":44100,"channels":2,"speaker_mask":null,"sample_format":"pcm_i16","frames":1,"duration_frames":{"numerator":1,"denominator":44100},"metadata":{}}"#
    );
    let converted = Command::new(cli_path())
        .args(["convert", "--format", "pcm_i16", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    let mut decoder = wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(
        decoder.read_frames(1).unwrap().unwrap().samples(),
        &Samples::I16(vec![25_588, 10_416])
    );
    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);
}

#[test]
fn metadata_edit_updates_and_removes_tags_without_changing_audio() {
    let input_path = temporary_path().with_extension("flac");
    let output_path = temporary_path().with_extension("flac");
    fs::write(&input_path, flac_with_comments()).unwrap();

    let output = Command::new(cli_path())
        .args([
            "metadata",
            "edit",
            "--set",
            "TITLE=New title",
            "--set",
            "ARTIST=New artist",
            "--set",
            "ARTIST=Featured artist",
            "--remove",
            "COMMENT",
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
    let json = String::from_utf8(output.stdout).unwrap();
    assert!(json.contains("\"operation\":\"metadata_edit\""));
    assert!(json.contains("\"updated_fields\":3"));
    let escaped_output_path = output_path.to_string_lossy().replace('\\', "\\\\");
    assert!(json.contains(&escaped_output_path));

    let mut decoder = flac::FlacDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["New title"]);
    assert_eq!(
        decoder.metadata().values("ARTIST"),
        ["New artist", "Featured artist"]
    );
    assert_eq!(decoder.metadata().values("ALBUM"), ["Keep this"]);
    assert!(decoder.metadata().values("COMMENT").is_empty());
    assert_eq!(
        decoder.read_frames(1).unwrap().unwrap().samples(),
        &Samples::I16(vec![25_588, 10_416])
    );
    drop(decoder);

    let original_output = fs::read(&output_path).unwrap();
    let overwrite = Command::new(cli_path())
        .args(["metadata", "edit", "--set", "TITLE=overwrite"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert_eq!(overwrite.status.code(), Some(2));
    assert_eq!(fs::read(&output_path).unwrap(), original_output);
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn metadata_edit_updates_wave_info_without_reencoding_audio() {
    let input_path = temporary_path();
    let output_path = temporary_path();
    let audio = PcmBuffer::new(
        AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap(),
        Samples::U8(vec![0, 128, 255]),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "Old title");
    metadata.insert("ALBUM", "Remove me");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    let output = Command::new(cli_path())
        .args([
            "metadata",
            "edit",
            "--set",
            "TITLE=New title",
            "--set",
            "ARTIST=Artist",
            "--remove",
            "ALBUM",
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
    let json = String::from_utf8(output.stdout).unwrap();
    assert!(json.contains("\"format\":\"wav\""));

    let mut decoder = wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["New title"]);
    assert_eq!(decoder.metadata().values("ARTIST"), ["Artist"]);
    assert!(decoder.metadata().values("ALBUM").is_empty());
    assert_eq!(decoder.read_frames(3).unwrap().unwrap(), audio);
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn inspect_json_reports_extensible_speaker_mask() {
    let path = temporary_path();
    let converted_path = temporary_path();
    let spec =
        AudioSpec::new_with_speaker_mask(48_000, 2, SampleFormat::PcmI16, Some(0x3)).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![0, 0, 1, -1])).unwrap();
    write_wav(File::create(&path).unwrap(), &audio).unwrap();

    let output = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&path)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("\"speaker_mask\":3")
    );

    let output = Command::new(cli_path())
        .args(["convert", "--format", "float32"])
        .arg(&path)
        .arg(&converted_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decoder = wav::WavDecoder::new(File::open(&converted_path).unwrap()).unwrap();
    assert_eq!(decoder.spec().speaker_mask(), Some(0x3));
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(converted_path);
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

    let mut decoder = wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
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
    let mut decoder = wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.container(), wav::WavContainer::Rf64);
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
        let mut decoder = wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
        assert_eq!(decoder.metadata(), &metadata);
        assert_eq!(decoder.read_frames(3).unwrap().unwrap().frames(), 3);
        let _ = fs::remove_file(output_path);
    }
    let _ = fs::remove_file(input_path);
}

#[test]
fn convert_encodes_wavpack_pcm_and_metadata() {
    let input_path = temporary_path();
    let output_path = temporary_path().with_extension("wv");
    let audio = PcmBuffer::new(
        AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![i16::MIN, i16::MAX, -1, 0, 1234, -9876]),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("artist", "Zemer");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    let output = Command::new(cli_path())
        .args(["convert", "--format", "wavpack", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"format\":\"wavpack\""));
    let mut decoder = WavPackDecoder::new_seekable(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.metadata(), &metadata);
    assert_eq!(decoder.read_frames(16).unwrap().unwrap(), audio);
    assert!(decoder.read_frames(16).unwrap().is_none());
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn convert_encodes_wavpack_float32_without_changing_sample_bits() {
    let input_path = temporary_path();
    let output_path = temporary_path().with_extension("wv");
    let bits = [
        0x8000_0000,
        0x0000_0001,
        0x3f80_0000,
        0xbf80_0000,
        0x7f80_0000,
        0x7fc1_2345,
    ];
    let audio = PcmBuffer::new(
        AudioSpec::new(48_000, 2, SampleFormat::Float32).unwrap(),
        Samples::F32(bits.map(f32::from_bits).to_vec()),
    )
    .unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();

    let output = Command::new(cli_path())
        .args(["convert", "--format", "wavpack", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut decoder = WavPackDecoder::new(File::open(&output_path).unwrap()).unwrap();
    let Samples::F32(decoded) = decoder.read_frames(16).unwrap().unwrap().samples().clone() else {
        panic!("WavPack output changed the float sample format");
    };
    assert_eq!(
        decoded
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        bits
    );
    assert!(decoder.read_frames(16).unwrap().is_none());
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn cue_split_and_create_preserve_track_samples_and_metadata() {
    let audio_path = temporary_path();
    let cue_path = audio_path.with_extension("cue");
    let output_dir = temporary_directory();
    let output_cue = output_dir.join("rebuilt.cue");
    let samples = (0..1176)
        .map(|sample| sample as i16 - 588)
        .collect::<Vec<_>>();
    let audio = PcmBuffer::new(
        AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(samples.clone()),
    )
    .unwrap();
    write_wav(File::create(&audio_path).unwrap(), &audio).unwrap();
    fs::write(
        &cue_path,
        format!(
            "FILE \"{}\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"First\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Second\"\n    INDEX 01 00:00:01\n",
            audio_path.file_name().unwrap().to_string_lossy()
        ),
    )
    .unwrap();

    let output = Command::new(cli_path())
        .args(["cue", "split", "--json"])
        .arg(&cue_path)
        .arg(&output_dir)
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
            .contains("\"tracks\":2")
    );
    for (number, expected) in [
        ("01 - First.wav", &samples[..588]),
        ("02 - Second.wav", &samples[588..]),
    ] {
        let mut decoder =
            wav::WavDecoder::new(File::open(output_dir.join(number)).unwrap()).unwrap();
        assert_eq!(
            decoder.read_frames(588).unwrap().unwrap().samples(),
            &Samples::I16(expected.to_vec())
        );
        assert_eq!(decoder.metadata().values("TITLE").len(), 1);
    }

    let output = Command::new(cli_path())
        .args(["cue", "create", "--json"])
        .arg(&output_cue)
        .arg(output_dir.join("01 - First.wav"))
        .arg(output_dir.join("02 - Second.wav"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let generated = fs::read_to_string(&output_cue).unwrap();
    assert!(generated.contains("FILE \"01 - First.wav\" WAVE"));
    assert!(generated.contains("TITLE \"Second\""));

    let _ = fs::remove_file(audio_path);
    let _ = fs::remove_file(cue_path);
    let _ = fs::remove_dir_all(output_dir);
}
