use std::{
    fs::{self, File},
    io::Cursor,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use aiff::{AiffAudioRecording, AiffComment, AiffDecoder, AiffEncoder, AiffMarker};
use au::{AuDecoder, AuEncoder};
use caf::CafDecoder;
use dsdiff::DsdiffDecoder;
use dsf::{DsfDecoder, DsfEncoder};
use flac::{FlacDecoder, OggFlacDecoder, OggFlacEncoder};
use g711::{decode_alaw, decode_mulaw, encode_alaw, encode_mulaw};
use ogg::OggPacketReader;
use vorbis::{VorbisCommentHeader, VorbisDecoder};
use wav::{write_wav, write_wav_with_metadata};
use wavpack::{WavPackDecoder, WavPackDsdDecoder};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioSpec, DsdBuffer, DsdDecoder, DsdEncoder, DsdSpec, Metadata,
    PcmBuffer, SampleFormat, Samples,
};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn cli_path() -> std::ffi::OsString {
    env!("CARGO_BIN_EXE_zemer_test_cli").into()
}

fn vorbis_link_comment_packets(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut reader = OggPacketReader::new(Cursor::new(bytes));
    let mut comments = Vec::new();
    loop {
        assert!(reader.next_packet().unwrap().is_some());
        comments.push(reader.next_packet().unwrap().unwrap().bytes);
        assert!(reader.next_packet().unwrap().is_some());
        while reader.next_packet().unwrap().is_some() {}
        let Some(next) = reader.next_stream().unwrap() else {
            break;
        };
        reader = next;
    }
    comments
}

fn assert_integer_pcm_output<D: AudioDecoder>(mut decoder: D, expected_frames: usize) {
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI24);
    assert_eq!(decoder.metadata().values("TITLE"), ["Vorbis CLI title"]);
    let mut frames = 0;
    let mut has_audio = false;
    while let Some(block) = decoder.read_frames(8192).unwrap() {
        let Samples::I24(samples) = block.samples() else {
            panic!("float input was not converted to PCM24");
        };
        frames += block.frames();
        has_audio |= samples.iter().any(|&sample| sample != 0);
    }
    assert_eq!(frames, expected_frames);
    assert!(has_audio);
}

fn temporary_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "zemer-cli-{}-{}.wav",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn cli_converts_pcm_to_ms_adpcm_wave() {
    let input_path = temporary_path();
    let output_path = temporary_path();
    let spec = AudioSpec::new(32_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![1_000; 257])).unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();
    let output = Command::new(cli_path())
        .args(["convert", "--format", "wav_ms_adpcm", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut decoder = wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(19).unwrap() {
        frames += block.frames();
    }
    assert_eq!(frames, audio.frames());
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn cli_inspects_and_converts_explicit_raw_pcm() {
    let raw_input = temporary_path();
    let wav_output = temporary_path();
    let raw_output = temporary_path();
    let expected = [0_i16, 1_000, -2_000, i16::MAX];
    fs::write(
        &raw_input,
        expected
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let inspected = Command::new(cli_path())
        .args([
            "inspect",
            "--json",
            "--input-format",
            "raw_s16le",
            "--sample-rate",
            "48000",
            "--channels",
            "2",
        ])
        .arg(&raw_input)
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    assert!(String::from_utf8_lossy(&inspected.stdout).contains("\"frames\":2"));
    let missing_parameters = Command::new(cli_path())
        .args(["inspect", "--input-format", "raw_s16le"])
        .arg(&raw_input)
        .output()
        .unwrap();
    assert!(!missing_parameters.status.success());

    let converted = Command::new(cli_path())
        .args([
            "convert",
            "--format",
            "pcm_i16",
            "--input-format",
            "raw_s16le",
            "--sample-rate",
            "48000",
            "--channels",
            "2",
        ])
        .arg(&raw_input)
        .arg(&wav_output)
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    let mut decoder = wav::WavDecoder::new(File::open(&wav_output).unwrap()).unwrap();
    let block = decoder.read_frames(8).unwrap().unwrap();
    assert_eq!(block.samples(), &Samples::I16(expected.to_vec()));

    let converted_back = Command::new(cli_path())
        .args(["convert", "--format", "raw_s16be"])
        .arg(&wav_output)
        .arg(&raw_output)
        .output()
        .unwrap();
    assert!(
        converted_back.status.success(),
        "{}",
        String::from_utf8_lossy(&converted_back.stderr)
    );
    let expected_be = expected
        .iter()
        .flat_map(|sample| sample.to_be_bytes())
        .collect::<Vec<_>>();
    assert_eq!(fs::read(&raw_output).unwrap(), expected_be);
    for path in [raw_input, wav_output, raw_output] {
        let _ = fs::remove_file(path);
    }
}

fn id3_private_tag() -> Vec<u8> {
    let payload = b"org.example\0opaque";
    let mut frame = Vec::from(&b"PRIV"[..]);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(payload);
    let body_len = frame.len();
    let mut tag = Vec::from(&b"ID3\x04\0\0"[..]);
    tag.extend_from_slice(&[
        ((body_len >> 21) & 0x7f) as u8,
        ((body_len >> 14) & 0x7f) as u8,
        ((body_len >> 7) & 0x7f) as u8,
        (body_len & 0x7f) as u8,
    ]);
    tag.extend_from_slice(&frame);
    tag
}

fn dsd_bytes<D: DsdDecoder>(decoder: &mut D) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(block) = decoder.read_dsd_octets(257).unwrap() {
        bytes.extend_from_slice(block.bytes());
    }
    bytes
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

fn one_sample_ogg_flac(sample_rate: u32, sample: i16) -> Vec<u8> {
    let spec = AudioSpec::new(sample_rate, 1, SampleFormat::PcmI16).unwrap();
    let block = PcmBuffer::new(spec, Samples::I16(vec![sample])).unwrap();
    let mut encoder = OggFlacEncoder::new(Vec::new(), spec).unwrap();
    encoder.write_block(&block).unwrap();
    encoder.finish().unwrap();
    encoder.into_inner()
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
fn convert_ogg_flac_to_wav_keeps_supported_tags_and_skips_other_comments() {
    let input_path = temporary_path().with_extension("oga");
    let output_path = temporary_path();
    let spec = AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap();
    let mut encoder = OggFlacEncoder::new(Vec::new(), spec).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "test title");
    metadata.insert("ENCODER", "external encoder");
    encoder.set_metadata(&metadata).unwrap();
    encoder
        .write_block(&PcmBuffer::new(spec, Samples::I16(vec![123])).unwrap())
        .unwrap();
    encoder.finish().unwrap();
    fs::write(&input_path, encoder.into_inner()).unwrap();

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
    let mut decoded = wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoded.metadata().values("TITLE"), ["test title"]);
    assert!(decoded.metadata().values("ENCODER").is_empty());
    assert_eq!(
        decoded.read_frames(1).unwrap().unwrap().samples(),
        &Samples::I16(vec![123])
    );
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn inspect_rejects_chained_ogg_flac_with_different_audio_specs() {
    let path = temporary_path();
    let output_path = temporary_path().with_extension("flac");
    let mut stream = one_sample_ogg_flac(44_100, 1);
    stream.extend(one_sample_ogg_flac(48_000, 2));
    fs::write(&path, stream).unwrap();

    let output = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("inspection cannot summarize audio with changing specifications")
    );

    let conversion = Command::new(cli_path())
        .args(["convert", "--format", "flac"])
        .arg(&path)
        .arg(&output_path)
        .output()
        .unwrap();
    let _ = fs::remove_file(&path);
    assert!(!conversion.status.success());
    assert!(!output_path.exists());
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
fn metadata_edit_rewrites_dsf_without_reencoding_audio() {
    let input_path = temporary_path();
    let output_path = temporary_path();
    let spec = DsdSpec::new(2_822_400, 1).unwrap();
    let audio = DsdBuffer::new(spec, vec![0x55, 0xaa]).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "Old title");
    let mut encoder = DsfEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_metadata(&metadata).unwrap();
    encoder.write_dsd_octets(&audio).unwrap();
    encoder.finish().unwrap();
    fs::write(&input_path, encoder.into_inner().into_inner()).unwrap();

    let output = Command::new(cli_path())
        .args(["metadata", "edit", "--set", "TITLE=New title", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decoder = DsfDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["New title"]);
    let mut decoder = decoder;
    assert_eq!(dsd_bytes(&mut decoder), audio.bytes());
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn metadata_edit_rewrites_dsdiff_and_preserves_unknown_chunks_and_audio() {
    let input_path = temporary_path().with_extension("dff");
    let output_path = temporary_path().with_extension("dff");
    let spec = DsdSpec::new(2_822_400, 2).unwrap();
    let audio = DsdBuffer::new(spec, vec![0x55, 0xaa, 0x69, 0x96]).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "Old title");
    let mut encoder = dsdiff::DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_metadata(&metadata).unwrap();
    encoder.write_dsd_octets(&audio).unwrap();
    encoder.finish().unwrap();
    let mut input = encoder.into_inner().into_inner();
    let unknown_chunk = b"JUNK\0\0\0\0\0\0\0\x03xyz\0";
    input.extend_from_slice(unknown_chunk);
    let form_size = u64::from_be_bytes(input[4..12].try_into().unwrap());
    input[4..12].copy_from_slice(&(form_size + unknown_chunk.len() as u64).to_be_bytes());
    fs::write(&input_path, input).unwrap();

    let output = Command::new(cli_path())
        .args(["metadata", "edit", "--set", "TITLE=New title", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output_bytes = fs::read(&output_path).unwrap();
    assert!(
        output_bytes
            .windows(unknown_chunk.len())
            .any(|w| w == unknown_chunk)
    );
    let mut decoder = DsdiffDecoder::new(Cursor::new(output_bytes)).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["New title"]);
    assert_eq!(dsd_bytes(&mut decoder), audio.bytes());
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
    metadata.insert("copyright", "CC0");
    metadata.insert("engineer", "Engineer A");
    metadata.insert("keywords", "quiet; generated");
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
    assert!(json.contains("\"COPYRIGHT\":[\"CC0\"]"));
    assert!(json.contains("\"ENGINEER\":[\"Engineer A\"]"));
    assert!(json.contains("\"KEYWORDS\":[\"quiet; generated\"]"));

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
fn caf_cli_inspects_converts_and_edits_metadata_without_changing_audio() {
    let input_path = temporary_path();
    let caf_path = temporary_path().with_extension("caf");
    let edited_path = temporary_path().with_extension("caf");
    let ima4_path = temporary_path().with_extension("caf");
    let spec = AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(
        spec,
        Samples::I16((0..130).map(|n| (n as i16).wrapping_mul(41)).collect()),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "CAF source");
    metadata.insert("artist", "One");
    metadata.insert("artist", "Two");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    let converted = Command::new(cli_path())
        .args(["convert", "--format", "caf", "--json"])
        .arg(&input_path)
        .arg(&caf_path)
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    assert!(String::from_utf8_lossy(&converted.stdout).contains("\"format\":\"caf\""));

    let inspected = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&caf_path)
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    assert!(String::from_utf8_lossy(&inspected.stdout).contains("\"TITLE\":[\"CAF source\"]"));
    let mut decoder = CafDecoder::new(File::open(&caf_path).unwrap()).unwrap();
    assert_eq!(decoder.metadata(), &metadata);
    assert_eq!(decoder.read_frames(100).unwrap().unwrap(), audio);

    let edited = Command::new(cli_path())
        .args(["metadata", "edit", "--set", "TITLE=Updated", "--json"])
        .arg(&caf_path)
        .arg(&edited_path)
        .output()
        .unwrap();
    assert!(
        edited.status.success(),
        "{}",
        String::from_utf8_lossy(&edited.stderr)
    );
    let mut edited_decoder = CafDecoder::new(File::open(&edited_path).unwrap()).unwrap();
    assert_eq!(edited_decoder.metadata().values("TITLE"), ["Updated"]);
    assert_eq!(edited_decoder.metadata().values("ARTIST"), ["One", "Two"]);
    assert_eq!(edited_decoder.read_frames(100).unwrap().unwrap(), audio);

    let ima4 = Command::new(cli_path())
        .args(["convert", "--format", "caf_ima4", "--json"])
        .arg(&input_path)
        .arg(&ima4_path)
        .output()
        .unwrap();
    assert!(
        ima4.status.success(),
        "{}",
        String::from_utf8_lossy(&ima4.stderr)
    );
    let mut ima4_decoder = CafDecoder::new(File::open(&ima4_path).unwrap()).unwrap();
    let mut frames = 0;
    while let Some(block) = ima4_decoder.read_frames(17).unwrap() {
        frames += block.frames();
    }
    assert_eq!(frames, audio.frames());

    for path in [input_path, caf_path, edited_path, ima4_path] {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn au_cli_inspects_converts_preserves_annotations_and_edits_comments() {
    let input_path = temporary_path().with_extension("au");
    let converted_path = temporary_path().with_extension("au");
    let edited_path = temporary_path().with_extension("au");
    let g711_path = temporary_path().with_extension("au");
    let g721_path = temporary_path().with_extension("au");
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![i16::MIN, -1, 0, 1, i16::MAX, 42])).unwrap();
    let mut encoder = AuEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_annotation(b"\0opaque\xff").unwrap();
    encoder.write_block(&audio).unwrap();
    fs::write(&input_path, encoder.finish().unwrap().into_inner()).unwrap();

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
    assert!(String::from_utf8_lossy(&inspected.stdout).contains("\"format\":\"au\""));

    let converted = Command::new(cli_path())
        .args(["convert", "--format", "au"])
        .arg(&input_path)
        .arg(&converted_path)
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    let mut decoded = AuDecoder::new(File::open(&converted_path).unwrap()).unwrap();
    assert_eq!(decoded.annotation(), b"\0opaque\xff");
    assert_eq!(decoded.read_frames(8).unwrap().unwrap(), audio);

    let edited = Command::new(cli_path())
        .args(["metadata", "edit", "--set", "COMMENT=voice note"])
        .arg(&input_path)
        .arg(&edited_path)
        .output()
        .unwrap();
    assert!(
        edited.status.success(),
        "{}",
        String::from_utf8_lossy(&edited.stderr)
    );
    let edited_bytes = fs::read(&edited_path).unwrap();
    let mut decoded = AuDecoder::new(Cursor::new(edited_bytes.clone())).unwrap();
    assert_eq!(decoded.metadata().values("COMMENT"), ["voice note"]);
    assert_eq!(decoded.read_frames(8).unwrap().unwrap(), audio);
    let edited_offset = u32::from_be_bytes(edited_bytes[4..8].try_into().unwrap()) as usize;
    let original_bytes = fs::read(&input_path).unwrap();
    let original_offset = u32::from_be_bytes(original_bytes[4..8].try_into().unwrap()) as usize;
    assert_eq!(
        &edited_bytes[edited_offset..],
        &original_bytes[original_offset..]
    );

    let g711 = Command::new(cli_path())
        .args(["convert", "--format", "au_mulaw", "--json"])
        .arg(&input_path)
        .arg(&g711_path)
        .output()
        .unwrap();
    assert!(
        g711.status.success(),
        "{}",
        String::from_utf8_lossy(&g711.stderr)
    );
    assert!(String::from_utf8_lossy(&g711.stdout).contains("\"output_encoding\":\"au_mulaw\""));
    let decoded = AuDecoder::new(File::open(&g711_path).unwrap()).unwrap();
    assert_eq!(decoded.spec().sample_format(), SampleFormat::PcmI16);
    assert_eq!(
        u32::from_be_bytes(fs::read(&g711_path).unwrap()[12..16].try_into().unwrap()),
        1
    );

    let g721 = Command::new(cli_path())
        .args(["convert", "--format", "au_g721", "--json"])
        .arg(&input_path)
        .arg(&g721_path)
        .output()
        .unwrap();
    assert!(
        g721.status.success(),
        "{}",
        String::from_utf8_lossy(&g721.stderr)
    );
    assert!(String::from_utf8_lossy(&g721.stdout).contains("\"output_encoding\":\"au_g721\""));
    let g721_bytes = fs::read(&g721_path).unwrap();
    assert_eq!(
        u32::from_be_bytes(g721_bytes[12..16].try_into().unwrap()),
        23
    );
    let mut decoded = AuDecoder::new(Cursor::new(g721_bytes)).unwrap();
    assert_eq!(decoded.spec(), spec);
    assert_eq!(decoded.read_frames(8).unwrap().unwrap().frames(), 6);
    assert!(decoded.read_frames(8).unwrap().is_none());

    for path in [
        input_path,
        converted_path,
        edited_path,
        g711_path,
        g721_path,
    ] {
        let _ = fs::remove_file(path);
    }
}

#[test]
fn au_cli_encodes_g723_three_and_five_bit_profiles() {
    let input_path = temporary_path();
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(
        spec,
        Samples::I16(vec![0, 1_000, -1_000, 3_000, -3_000, 8_000, -8_000, 16_000]),
    )
    .unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();

    for (format, encoding) in [("au_g723_3", 25_u32), ("au_g723_5", 26_u32)] {
        let output_path = temporary_path().with_extension("au");
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
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains(&format!("\"output_encoding\":\"{format}\""))
        );
        let bytes = fs::read(&output_path).unwrap();
        assert_eq!(
            u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
            encoding
        );
        let mut decoder = AuDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.read_frames(8).unwrap().unwrap().frames(), 8);
        assert!(decoder.read_frames(1).unwrap().is_none());
        let _ = fs::remove_file(output_path);
    }
    let _ = fs::remove_file(input_path);
}

#[test]
fn convert_inspects_and_converts_aiff_and_aifc() {
    let input_path = temporary_path();
    let audio = PcmBuffer::new(
        AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![i16::MIN, 1, -2, i16::MAX]),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "AIFF CLI");
    metadata.insert("album", "AIFF album");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    for (format, extension) in [("aiff", "aiff"), ("aifc", "aifc")] {
        let output_path = temporary_path().with_extension(extension);
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
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("\"format\":\"{format}\""))
        );

        let inspect = Command::new(cli_path())
            .args(["inspect", "--json"])
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            inspect.status.success(),
            "{}",
            String::from_utf8_lossy(&inspect.stderr)
        );
        assert!(
            String::from_utf8_lossy(&inspect.stdout).contains(&format!("\"format\":\"{format}\""))
        );
        let mut decoder = AiffDecoder::new(File::open(&output_path).unwrap()).unwrap();
        assert_eq!(decoder.metadata(), &metadata);
        assert_eq!(decoder.read_frames(2).unwrap().unwrap(), audio);
        assert!(decoder.read_frames(1).unwrap().is_none());
        assert_eq!(decoder.is_aifc(), format == "aifc");
        let _ = fs::remove_file(output_path);
    }
    let _ = fs::remove_file(input_path);
}

#[test]
fn convert_aifc_preserves_audio_recording_chunk() {
    let input_path = temporary_path().with_extension("aifc");
    let output_path = temporary_path().with_extension("aifc");
    let audio = PcmBuffer::new(
        AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![i16::MIN, 0, i16::MAX]),
    )
    .unwrap();
    let recording = AiffAudioRecording {
        channel_status: std::array::from_fn(|index| (index * 3) as u8),
    };
    let mut encoder =
        AiffEncoder::new_aifc(File::create(&input_path).unwrap(), audio.spec()).unwrap();
    encoder.set_audio_recording(&recording).unwrap();
    encoder.write_block(&audio).unwrap();
    encoder.finish().unwrap();

    let output = Command::new(cli_path())
        .args(["convert", "--format", "aifc"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut decoder = AiffDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.audio_recording(), Some(&recording));
    assert_eq!(decoder.read_frames(3).unwrap().unwrap(), audio);
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn convert_aiff_valid_bits_through_cli() {
    let input_path = temporary_path().with_extension("wav");
    let audio = PcmBuffer::new(
        AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![i16::MIN, 1, -2, i16::MAX]),
    )
    .unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();

    for (format, extension) in [("aiff", "aiff"), ("aifc", "aifc")] {
        let output_path = temporary_path().with_extension(extension);
        let output = Command::new(cli_path())
            .args([
                "convert",
                "--format",
                format,
                "--valid-bits",
                "12",
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
        assert!(String::from_utf8_lossy(&output.stdout).contains("\"output_valid_bits\":12"));

        let bytes = fs::read(&output_path).unwrap();
        let comm = bytes.windows(4).position(|chunk| chunk == b"COMM").unwrap();
        assert_eq!(
            u16::from_be_bytes(bytes[comm + 14..comm + 16].try_into().unwrap()),
            12
        );
        let mut decoder = AiffDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::I16(vec![i16::MIN, 0, -16, 32_752])
        );
        assert!(decoder.read_frames(1).unwrap().is_none());
        let _ = fs::remove_file(output_path);
    }

    for (format, valid_bits) in [("pcm_i16", "12"), ("aiff", "20"), ("aiff", "33")] {
        let output_path = temporary_path();
        let output = Command::new(cli_path())
            .args(["convert", "--format", format, "--valid-bits", valid_bits])
            .arg(&input_path)
            .arg(&output_path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!output_path.exists());
    }
    let _ = fs::remove_file(input_path);
}

#[test]
fn aiff_conversion_preserves_typed_metadata_across_aiff_and_aifc() {
    let input_path = temporary_path().with_extension("aiff");
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![10, 20, 30])).unwrap();
    let markers = [AiffMarker {
        id: 3,
        position: 2,
        name: b"cue point".to_vec(),
    }];
    let comments = [AiffComment {
        timestamp: 3_865_440_000,
        marker_id: 3,
        text: b"mastered take".to_vec(),
    }];
    let mut metadata = Metadata::default();
    metadata.insert("copyright", "1988 Apple Computer, Inc.");
    let id3_tag = id3_private_tag();
    let mut encoder = AiffEncoder::new(File::create(&input_path).unwrap(), spec).unwrap();
    encoder.set_id3_tag(&id3_tag).unwrap();
    encoder.set_metadata(&metadata).unwrap();
    encoder.set_markers(&markers).unwrap();
    encoder.set_comments(&comments).unwrap();
    encoder.write_block(&audio).unwrap();
    encoder.finish().unwrap();

    for (format, extension) in [("aiff", "aiff"), ("aifc", "aifc")] {
        let output_path = temporary_path().with_extension(extension);
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
        let mut decoder = AiffDecoder::new(File::open(&output_path).unwrap()).unwrap();
        assert_eq!(decoder.is_aifc(), format == "aifc");
        assert_eq!(decoder.markers(), markers);
        assert_eq!(decoder.comments(), comments);
        assert_eq!(
            decoder.metadata().values("COPYRIGHT"),
            ["1988 Apple Computer, Inc."]
        );
        assert!(
            decoder
                .id3_tag()
                .unwrap()
                .windows(4)
                .any(|frame| frame == b"PRIV")
        );
        assert_eq!(decoder.read_frames(3).unwrap().unwrap(), audio);
        assert!(decoder.read_frames(1).unwrap().is_none());
        let _ = fs::remove_file(output_path);
    }
    let _ = fs::remove_file(input_path);
}

#[test]
fn convert_encodes_aifc_g711_only_when_explicitly_selected() {
    let input_path = temporary_path();
    let samples = [i16::MIN, -1, 0, 1, i16::MAX];
    let audio = PcmBuffer::new(
        AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(samples.to_vec()),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "G.711 AIFC");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    for (format, decode) in [
        (
            "aifc_alaw",
            samples.map(encode_alaw).map(decode_alaw).to_vec(),
        ),
        (
            "aifc_mulaw",
            samples.map(encode_mulaw).map(decode_mulaw).to_vec(),
        ),
    ] {
        let output_path = temporary_path().with_extension("aifc");
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
        let report = String::from_utf8_lossy(&output.stdout);
        assert!(report.contains("\"format\":\"aifc\""));
        assert!(report.contains(&format!("\"output_encoding\":\"{format}\"")));
        let mut decoder = AiffDecoder::new(File::open(&output_path).unwrap()).unwrap();
        assert_eq!(decoder.metadata(), &metadata);
        assert_eq!(
            decoder
                .read_frames(samples.len())
                .unwrap()
                .unwrap()
                .samples(),
            &Samples::I16(decode)
        );
        assert!(decoder.read_frames(1).unwrap().is_none());
        let _ = fs::remove_file(output_path);
    }
    let _ = fs::remove_file(input_path);
}

#[test]
fn convert_encodes_aifc_g726_at_each_explicit_rate() {
    let input_path = temporary_path();
    let samples: Vec<_> = (0..259)
        .map(|index| ((index as f32 * 0.13).sin() * 20_000.0).round() as i16)
        .collect();
    let audio = PcmBuffer::new(
        AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(samples),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "G.726 AIFC");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    for (rate, bits_per_second) in [(16, 16_000_u32), (24, 24_000), (32, 32_000), (40, 40_000)] {
        let output_path = temporary_path().with_extension("aifc");
        let rate_arg = rate.to_string();
        let output = Command::new(cli_path())
            .args([
                "convert",
                "--format",
                "aifc_g726",
                "--bitrate",
                &rate_arg,
                "--json",
            ])
            .arg(&input_path)
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{rate} kbit/s: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = String::from_utf8_lossy(&output.stdout);
        assert!(report.contains("\"format\":\"aifc\""));
        assert!(report.contains("\"output_encoding\":\"aifc_g726\""));

        let bytes = fs::read(&output_path).unwrap();
        let mut hint = b"APPL\0\0\0\x0cstocG726".to_vec();
        hint.extend_from_slice(&bits_per_second.to_be_bytes());
        assert!(bytes.windows(hint.len()).any(|chunk| chunk == hint));
        let mut decoder = AiffDecoder::new(File::open(&output_path).unwrap()).unwrap();
        assert!(decoder.is_aifc());
        assert_eq!(decoder.metadata(), &metadata);
        assert_eq!(decoder.spec().sample_rate(), 8_000);
        assert_eq!(decoder.read_frames(512).unwrap().unwrap().frames(), 259);
        assert!(decoder.read_frames(1).unwrap().is_none());
        let _ = fs::remove_file(output_path);
    }

    for args in [
        vec!["convert", "--format", "aifc_g726"],
        vec!["convert", "--format", "aifc_g726", "--bitrate", "20"],
        vec!["convert", "--format", "aiff", "--bitrate", "40"],
    ] {
        let output_path = temporary_path().with_extension("aifc");
        let output = Command::new(cli_path())
            .args(args)
            .arg(&input_path)
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!output_path.exists());
    }
    let _ = fs::remove_file(input_path);
}

#[test]
fn convert_encodes_aifc_dwvw_and_reports_compression() {
    let input_path = temporary_path();
    let samples = [-32_768, 32_767, -1, 1, 1234, -4567, 0, 0];
    let audio = PcmBuffer::new(
        AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap(),
        Samples::I16(samples.to_vec()),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "DWVW AIFC");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    let output_path = temporary_path().with_extension("aifc");
    let output = Command::new(cli_path())
        .args(["convert", "--format", "aifc_dwvw", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"output_encoding\":\"aifc_dwvw\""));
    let mut decoder = AiffDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.metadata(), &metadata);
    assert_eq!(decoder.read_frames(4).unwrap().unwrap(), audio);
    assert!(decoder.read_frames(1).unwrap().is_none());

    let _ = fs::remove_file(output_path);
    let _ = fs::remove_file(input_path);
}

#[test]
fn convert_encodes_aifc_sowt_and_reports_little_endian_pcm() {
    let input_path = temporary_path();
    let spec = AudioSpec::new(44_100, 2, SampleFormat::PcmI24).unwrap();
    let audio = PcmBuffer::new(
        spec,
        Samples::I24(vec![-8_388_608, -1, 0, 8_388_607, 0x123456, -0x123456]),
    )
    .unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();

    let output_path = temporary_path().with_extension("aifc");
    let output = Command::new(cli_path())
        .args(["convert", "--format", "aifc_sowt", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"output_encoding\":\"aifc_sowt\""));
    let encoded = fs::read(&output_path).unwrap();
    assert!(encoded.windows(4).any(|chunk| chunk == b"sowt"));
    let mut decoder = AiffDecoder::new(Cursor::new(encoded)).unwrap();
    assert_eq!(decoder.spec(), spec);
    assert_eq!(decoder.read_frames(3).unwrap().unwrap(), audio);
    assert!(decoder.read_frames(1).unwrap().is_none());

    let _ = fs::remove_file(output_path);
    let _ = fs::remove_file(input_path);
}

#[test]
fn convert_encodes_aifc_ima4_with_packet_frame_count_and_padding() {
    let input_path = temporary_path();
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let samples: Vec<_> = (0..65)
        .map(|index| ((index as f32 * 0.13).sin() * 3_000.0).round() as i16)
        .collect();
    let audio = PcmBuffer::new(spec, Samples::I16(samples)).unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();

    let output_path = temporary_path().with_extension("aifc");
    let output = Command::new(cli_path())
        .args(["convert", "--format", "aifc_ima4", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("\"output_encoding\":\"aifc_ima4\""));
    assert!(report.contains("\"frames\":128"));

    let mut decoder = AiffDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.read_frames(128).unwrap().unwrap().frames(), 128);
    assert!(decoder.read_frames(1).unwrap().is_none());
    let _ = fs::remove_file(output_path);
    let _ = fs::remove_file(input_path);
}

#[test]
fn metadata_edit_preserves_aiff_and_aifc_audio() {
    let input_path = temporary_path();
    let audio = PcmBuffer::new(
        AudioSpec::new(32_000, 1, SampleFormat::PcmI24).unwrap(),
        Samples::I24(vec![-8_388_608, -1, 0, 8_388_607]),
    )
    .unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();

    for (format, extension) in [("aiff", "aiff"), ("aifc", "aifc")] {
        let source = temporary_path().with_extension(extension);
        let conversion = Command::new(cli_path())
            .args(["convert", "--format", format])
            .arg(&input_path)
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            conversion.status.success(),
            "{}",
            String::from_utf8_lossy(&conversion.stderr)
        );

        let output_path = temporary_path().with_extension(extension);
        let edited = Command::new(cli_path())
            .args([
                "metadata",
                "edit",
                "--json",
                "--set",
                "TITLE=Edited",
                "--set",
                "COPYRIGHT=2026 Zemer",
                "--set",
                "COMMENT=first",
                "--set",
                "COMMENT=second",
            ])
            .arg(&source)
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            edited.status.success(),
            "{}",
            String::from_utf8_lossy(&edited.stderr)
        );
        assert!(
            String::from_utf8_lossy(&edited.stdout).contains(&format!("\"format\":\"{format}\""))
        );
        let mut decoder = AiffDecoder::new(File::open(&output_path).unwrap()).unwrap();
        assert_eq!(decoder.metadata().values("TITLE"), ["Edited"]);
        assert_eq!(decoder.metadata().values("COPYRIGHT"), ["2026 Zemer"]);
        assert_eq!(decoder.metadata().values("COMMENT"), ["first", "second"]);
        assert_eq!(decoder.read_frames(8).unwrap().unwrap(), audio);
        assert!(decoder.read_frames(1).unwrap().is_none());
        let _ = fs::remove_file(source);
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
fn convert_wavpack_hybrid_writes_and_reports_lossless_correction_pair() {
    let input_path = temporary_path();
    let output_path = temporary_path().with_extension("wv");
    let correction_path = output_path.with_extension("wvc");
    let audio = PcmBuffer::new(
        AudioSpec::new(22_050, 2, SampleFormat::PcmI16).unwrap(),
        Samples::I16(
            (0..4096)
                .map(|value| (value * 47 - 30_000) as i16)
                .collect(),
        ),
    )
    .unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();

    let output = Command::new(cli_path())
        .args([
            "convert",
            "--format",
            "wavpack",
            "--bitrate",
            "128",
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
    assert!(json.contains("\"correction_output\":"));
    assert!(correction_path.is_file());
    let mut decoder = WavPackDecoder::new_with_correction(
        File::open(&output_path).unwrap(),
        File::open(&correction_path).unwrap(),
    )
    .unwrap();
    assert_eq!(decoder.read_frames(4096).unwrap().unwrap(), audio);
    assert!(decoder.read_frames(1).unwrap().is_none());

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
    let _ = fs::remove_file(correction_path);
}

#[test]
fn hybrid_convert_does_not_overwrite_an_existing_correction_file() {
    let input_path = temporary_path();
    let output_path = temporary_path().with_extension("wv");
    let correction_path = output_path.with_extension("wvc");
    let audio = PcmBuffer::new(
        AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![0; 32]),
    )
    .unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();
    fs::write(&correction_path, b"keep existing correction").unwrap();

    let output = Command::new(cli_path())
        .args(["convert", "--format", "wavpack", "--bitrate", "128"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!output_path.exists());
    assert_eq!(
        fs::read(&correction_path).unwrap(),
        b"keep existing correction"
    );

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(correction_path);
}

#[test]
fn convert_encodes_native_flac_with_metadata_and_predictive_frames() {
    let input_path = temporary_path();
    let output_path = input_path.with_extension("flac");
    let audio = PcmBuffer::new(
        AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![0; 4096]),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "quiet test");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    let output = Command::new(cli_path())
        .args(["convert", "--format", "flac", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"format\":\"flac\""));

    let mut decoder = FlacDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.metadata(), &metadata);
    assert_eq!(decoder.read_frames(4096).unwrap().unwrap(), audio);
    assert!(decoder.read_frames(1).unwrap().is_none());
    assert!(fs::metadata(&output_path).unwrap().len() < 1024);
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn convert_encodes_ogg_flac_with_metadata_and_eos_granule() {
    let input_path = temporary_path();
    let output_path = input_path.with_extension("oga");
    let audio = PcmBuffer::new(
        AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![0; 4097]),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("album", "container test");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    let output = Command::new(cli_path())
        .args(["convert", "--format", "ogg_flac", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"format\":\"ogg-flac\""));

    let mut decoder = OggFlacDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.metadata(), &metadata);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(257).unwrap() {
        let Samples::I16(values) = block.samples() else {
            panic!("unexpected decoded sample type");
        };
        decoded.extend_from_slice(values);
    }
    assert_eq!(decoded, vec![0; 4097]);
    assert!(decoder.read_frames(1).unwrap().is_none());
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn metadata_edit_rewrites_ogg_flac_comments_without_changing_audio() {
    let input_path = temporary_path();
    let oga_path = input_path.with_extension("oga");
    let output_path = temporary_path().with_extension("oga");
    let audio = PcmBuffer::new(
        AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(
            (0..2049)
                .map(|index| (index as i16).wrapping_mul(31))
                .collect(),
        ),
    )
    .unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();

    let encoded = Command::new(cli_path())
        .args(["convert", "--format", "ogg_flac"])
        .arg(&input_path)
        .arg(&oga_path)
        .output()
        .unwrap();
    assert!(
        encoded.status.success(),
        "{}",
        String::from_utf8_lossy(&encoded.stderr)
    );
    let edited = Command::new(cli_path())
        .args(["metadata", "edit", "--set", "TITLE=Edited", "--json"])
        .arg(&oga_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        edited.status.success(),
        "{}",
        String::from_utf8_lossy(&edited.stderr)
    );
    assert!(String::from_utf8_lossy(&edited.stdout).contains("\"format\":\"ogg-flac\""));

    let mut decoder = OggFlacDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["Edited"]);
    assert_eq!(decoder.read_frames(4096).unwrap().unwrap(), audio);
    assert!(decoder.read_frames(1).unwrap().is_none());
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(oga_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn vorbis_cli_inspects_converts_and_edits_comments() {
    let input_path = temporary_path();
    let vorbis_path = temporary_path().with_extension("oga");
    let decoded_wav_path = temporary_path();
    let target_bitrate_path = temporary_path().with_extension("oga");
    let edited_path = temporary_path().with_extension("ogg");
    let audio = PcmBuffer::new(
        AudioSpec::new(44_100, 2, SampleFormat::PcmI16).unwrap(),
        Samples::I16(
            (0..8193)
                .flat_map(|frame| {
                    let phase = std::f64::consts::TAU * 440.0 * f64::from(frame) / 44_100.0;
                    [
                        (phase.sin() * 20_000.0) as i16,
                        (phase.cos() * 12_000.0) as i16,
                    ]
                })
                .collect(),
        ),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "Vorbis CLI title");
    metadata.insert("artist", "Remove me");
    write_wav_with_metadata(File::create(&input_path).unwrap(), &audio, &metadata).unwrap();

    let converted = Command::new(cli_path())
        .args(["convert", "--format", "vorbis", "--json"])
        .arg(&input_path)
        .arg(&vorbis_path)
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    assert!(String::from_utf8_lossy(&converted.stdout).contains("\"format\":\"ogg-vorbis\""));
    {
        let converted_decoder = VorbisDecoder::new(File::open(&vorbis_path).unwrap()).unwrap();
        assert_eq!(
            converted_decoder.metadata().values("TITLE"),
            ["Vorbis CLI title"]
        );
        assert_eq!(converted_decoder.metadata().values("ARTIST"), ["Remove me"]);
    }

    let decoded_wav = Command::new(cli_path())
        .args(["convert", "--format", "pcm_i16", "--json"])
        .arg(&vorbis_path)
        .arg(&decoded_wav_path)
        .output()
        .unwrap();
    assert!(
        decoded_wav.status.success(),
        "{}",
        String::from_utf8_lossy(&decoded_wav.stderr)
    );
    assert!(String::from_utf8_lossy(&decoded_wav.stdout).contains("\"format\":\"wav\""));
    let mut wav_decoder = wav::WavDecoder::new(File::open(&decoded_wav_path).unwrap()).unwrap();
    assert_eq!(wav_decoder.spec(), audio.spec());
    assert_eq!(wav_decoder.metadata().values("TITLE"), ["Vorbis CLI title"]);
    let mut wav_samples = Vec::new();
    let mut wav_frames = 0;
    while let Some(block) = wav_decoder.read_frames(733).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("CLI Vorbis-to-WAVE conversion did not write PCM16 samples");
        };
        wav_samples.extend_from_slice(samples);
        wav_frames += block.frames();
    }
    assert_eq!(wav_frames, audio.frames());
    let Samples::I16(source) = audio.samples() else {
        unreachable!();
    };
    let max_roundtrip_error = wav_samples
        .iter()
        .zip(source)
        .map(|(actual, source)| (i32::from(*actual) - i32::from(*source)).unsigned_abs())
        .max()
        .unwrap_or_default();
    assert!(
        max_roundtrip_error < 3_277,
        "maximum CLI Vorbis-to-WAVE sample error {max_roundtrip_error}"
    );

    for (format, extension) in [("flac", "flac"), ("ogg_flac", "oga"), ("aiff", "aiff")] {
        let output_path = temporary_path().with_extension(extension);
        let converted = Command::new(cli_path())
            .args(["convert", "--format", format, "--json"])
            .arg(&vorbis_path)
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            converted.status.success(),
            "{format}: {}",
            String::from_utf8_lossy(&converted.stderr)
        );
        match format {
            "flac" => assert_integer_pcm_output(
                FlacDecoder::new(File::open(&output_path).unwrap()).unwrap(),
                audio.frames(),
            ),
            "ogg_flac" => assert_integer_pcm_output(
                OggFlacDecoder::new(File::open(&output_path).unwrap()).unwrap(),
                audio.frames(),
            ),
            "aiff" => assert_integer_pcm_output(
                AiffDecoder::new(File::open(&output_path).unwrap()).unwrap(),
                audio.frames(),
            ),
            _ => unreachable!(),
        }
        let _ = fs::remove_file(output_path);
    }

    let target_converted = Command::new(cli_path())
        .args([
            "convert",
            "--format",
            "vorbis",
            "--bitrate",
            "128",
            "--json",
        ])
        .arg(&input_path)
        .arg(&target_bitrate_path)
        .output()
        .unwrap();
    assert!(
        target_converted.status.success(),
        "{}",
        String::from_utf8_lossy(&target_converted.stderr)
    );
    let mut target_decoder = VorbisDecoder::new(File::open(&target_bitrate_path).unwrap()).unwrap();
    let mut target_frames = 0;
    while let Some(block) = target_decoder.read_frames(733).unwrap() {
        target_frames += block.frames();
    }
    assert_eq!(target_frames, audio.frames());
    assert!(!target_bitrate_path.with_extension("wvc").exists());

    let low_rate_input_path = temporary_path();
    let low_rate_audio = PcmBuffer::new(
        AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(
            (0..8193)
                .map(|frame| {
                    ((std::f64::consts::TAU * 440.0 * f64::from(frame) / 8_000.0).sin() * 20_000.0)
                        as i16
                })
                .collect(),
        ),
    )
    .unwrap();
    write_wav(File::create(&low_rate_input_path).unwrap(), &low_rate_audio).unwrap();
    for (bitrate_kbps, source_path, expected_frames) in [
        (24, &low_rate_input_path, low_rate_audio.frames()),
        (9600, &input_path, audio.frames()),
    ] {
        let boundary_path = temporary_path().with_extension("oga");
        let bitrate = bitrate_kbps.to_string();
        let converted = Command::new(cli_path())
            .args([
                "convert",
                "--format",
                "vorbis",
                "--bitrate",
                &bitrate,
                "--json",
            ])
            .arg(source_path)
            .arg(&boundary_path)
            .output()
            .unwrap();
        assert!(
            converted.status.success(),
            "{bitrate_kbps} kbit/s: {}",
            String::from_utf8_lossy(&converted.stderr)
        );
        let mut decoder = VorbisDecoder::new(File::open(&boundary_path).unwrap()).unwrap();
        let mut frames = 0;
        while let Some(block) = decoder.read_frames(733).unwrap() {
            frames += block.frames();
        }
        assert_eq!(frames, expected_frames, "{bitrate_kbps} kbit/s");
        let _ = fs::remove_file(boundary_path);
    }
    let _ = fs::remove_file(low_rate_input_path);

    let rejected_low_rate_path = temporary_path().with_extension("oga");
    let rejected = Command::new(cli_path())
        .args(["convert", "--format", "vorbis", "--bitrate", "24"])
        .arg(&input_path)
        .arg(&rejected_low_rate_path)
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("target bitrate is too low"));
    assert!(!rejected_low_rate_path.exists());

    let inspected = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&vorbis_path)
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    assert!(String::from_utf8_lossy(&inspected.stdout).contains("\"format\":\"ogg-vorbis\""));

    let edited = Command::new(cli_path())
        .args([
            "metadata",
            "edit",
            "--set",
            "TITLE=Edited Vorbis title",
            "--remove",
            "ARTIST",
            "--json",
        ])
        .arg(&vorbis_path)
        .arg(&edited_path)
        .output()
        .unwrap();
    assert!(
        edited.status.success(),
        "{}",
        String::from_utf8_lossy(&edited.stderr)
    );
    assert!(String::from_utf8_lossy(&edited.stdout).contains("\"format\":\"ogg-vorbis\""));

    let mut decoder = VorbisDecoder::new(File::open(&edited_path).unwrap()).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["Edited Vorbis title"]);
    assert!(decoder.metadata().values("ARTIST").is_empty());
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(733).unwrap() {
        let Samples::F32(samples) = block.samples() else {
            panic!("Vorbis output did not decode as Float32 PCM");
        };
        decoded.extend_from_slice(samples);
    }
    assert_eq!(decoded.len(), audio.samples().len());
    let Samples::I16(source) = audio.samples() else {
        unreachable!();
    };
    let max_error = decoded
        .iter()
        .zip(source)
        .map(|(actual, source)| (actual - f32::from(*source) / 32768.0).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        max_error < 0.1,
        "maximum CLI Vorbis conversion error {max_error}"
    );
    assert!(decoder.read_frames(1).unwrap().is_none());
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(vorbis_path);
    let _ = fs::remove_file(decoded_wav_path);
    let _ = fs::remove_file(target_bitrate_path);
    let _ = fs::remove_file(edited_path);
}

#[test]
fn vorbis_cli_metadata_edit_preserves_unmodified_comments() {
    let input_path = temporary_path().with_extension("oga");
    let output_path = temporary_path().with_extension("oga");
    let spec = AudioSpec::new(48_000, 1, SampleFormat::Float32).unwrap();
    let audio = PcmBuffer::new(spec, Samples::F32(vec![0.25; 257])).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "Old title");
    metadata.insert("ARTIST", "First artist");
    metadata.insert("ARTIST", "Second artist");
    metadata.insert("COMPOSER", "Composer name");
    metadata.insert("LYRICS", "Keep these lyrics");
    metadata.insert("MUSICBRAINZ_ALBUMID", "album-id");
    let cover_art = "AAAAAwAAAAMtLT4AAAAAAAAAAAAAAAAAAAAAAAAAAAAAACFodHRwczovL2V4YW1wbGUuaW52YWxpZC9jb3Zlci5qcGc=";
    metadata.insert("METADATA_BLOCK_PICTURE", cover_art);
    let mut first_link = Vec::new();
    {
        let mut encoder = vorbis::VorbisEncoder::new(&mut first_link, spec).unwrap();
        encoder.set_metadata(&metadata).unwrap();
        encoder.write_block(&audio).unwrap();
        encoder.finish().unwrap();
    }
    let mut second_metadata = Metadata::default();
    second_metadata.insert("TITLE", "Second link title");
    let mut second_link = Vec::new();
    {
        let mut encoder = vorbis::VorbisEncoder::new(&mut second_link, spec).unwrap();
        encoder.set_metadata(&second_metadata).unwrap();
        encoder.write_block(&audio).unwrap();
        encoder.finish().unwrap();
    }
    let mut chained = first_link;
    chained.extend_from_slice(&second_link);
    fs::write(&input_path, &chained).unwrap();
    let original_comments = vorbis_link_comment_packets(&chained);
    assert_eq!(original_comments.len(), 2);

    let edited = Command::new(cli_path())
        .args([
            "metadata",
            "edit",
            "--set",
            "TITLE=New title",
            "--remove",
            "ARTIST",
            "--json",
        ])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        edited.status.success(),
        "{}",
        String::from_utf8_lossy(&edited.stderr)
    );

    let mut decoder = VorbisDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["New title"]);
    assert!(decoder.metadata().values("ARTIST").is_empty());
    assert_eq!(decoder.metadata().values("COMPOSER"), ["Composer name"]);
    assert_eq!(decoder.metadata().values("LYRICS"), ["Keep these lyrics"]);
    assert_eq!(
        decoder.metadata().values("MUSICBRAINZ_ALBUMID"),
        ["album-id"]
    );
    assert_eq!(
        decoder.metadata().values("METADATA_BLOCK_PICTURE"),
        [cover_art]
    );
    let edited_bytes = fs::read(&output_path).unwrap();
    let edited_comments = vorbis_link_comment_packets(&edited_bytes);
    assert_eq!(edited_comments.len(), 2);
    assert_eq!(edited_comments[1], original_comments[1]);
    let second_link_comments = VorbisCommentHeader::parse(&edited_comments[1]).unwrap();
    assert_eq!(
        second_link_comments.metadata().values("TITLE"),
        ["Second link title"]
    );
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(733).unwrap() {
        frames += block.frames();
    }
    assert_eq!(frames, audio.frames() * 2);
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn vorbis_cli_maps_wave_51_channels_to_vorbis_order() {
    let input_path = temporary_path();
    let output_path = temporary_path().with_extension("ogg");
    let channels = 6;
    let sample_rate = 48_000;
    let frequencies = [137.0_f64, 233.0, 383.0, 523.0, 719.0, 997.0];
    let source_samples: Vec<f32> = (0..8193)
        .flat_map(|frame| {
            frequencies.map(|frequency| {
                let phase =
                    std::f64::consts::TAU * frequency * f64::from(frame) / f64::from(sample_rate);
                (phase.sin() * 0.7) as f32
            })
        })
        .collect();
    let audio = PcmBuffer::new(
        AudioSpec::new_with_speaker_mask(sample_rate, channels, SampleFormat::Float32, Some(0x3f))
            .unwrap(),
        Samples::F32(source_samples.clone()),
    )
    .unwrap();
    write_wav(File::create(&input_path).unwrap(), &audio).unwrap();

    let converted = Command::new(cli_path())
        .args(["convert", "--format", "vorbis", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );

    let mut decoder = VorbisDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.spec().channels(), channels);
    assert_eq!(decoder.spec().speaker_mask(), None);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(733).unwrap() {
        let Samples::F32(samples) = block.samples() else {
            panic!("Vorbis output did not decode as Float32 PCM");
        };
        decoded.extend_from_slice(samples);
    }
    assert_eq!(decoded.len(), source_samples.len());

    // WAVE 5.1 is FL, FR, FC, LFE, BL, BR; Vorbis I is FL, FC, FR, BL, BR, LFE.
    let vorbis_to_wave = [0, 2, 1, 4, 5, 3];
    for (vorbis_channel, &wave_channel) in vorbis_to_wave.iter().enumerate() {
        let max_error = (0..8193)
            .map(|frame| {
                let expected = source_samples[frame * channels as usize + wave_channel];
                let actual = decoded[frame * channels as usize + vorbis_channel];
                (actual - expected).abs()
            })
            .fold(0.0_f32, f32::max);
        assert!(
            max_error < 0.15,
            "Vorbis channel {vorbis_channel} did not map from WAVE channel {wave_channel}; maximum error {max_error}"
        );
    }
    assert!(decoder.read_frames(1).unwrap().is_none());
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn vorbis_cli_converts_independent_six_channel_stream_to_float_wave() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    let input_path = fixtures.join("oxideav-vorbis-0_0_12-48000-6.ogg");
    let oracle_path = fixtures.join("oxideav-vorbis-0_0_12-48000-6.f32le");
    let output_path = temporary_path();
    let converted = Command::new(cli_path())
        .args(["convert", "--format", "float32", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    assert!(String::from_utf8_lossy(&converted.stdout).contains("\"format\":\"wav\""));

    let mut decoder = wav::WavDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.spec().sample_rate(), 48_000);
    assert_eq!(decoder.spec().channels(), 6);
    assert_eq!(decoder.spec().sample_format(), SampleFormat::Float32);
    let mut actual = Vec::new();
    while let Some(block) = decoder.read_frames(733).unwrap() {
        let Samples::F32(samples) = block.samples() else {
            panic!("Vorbis CLI conversion did not write Float32 WAV samples");
        };
        actual.extend_from_slice(samples);
    }
    let expected_bytes = fs::read(oracle_path).unwrap();
    let expected: Vec<f32> = expected_bytes
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    assert_eq!(actual.len(), expected.len());
    let max_error = actual
        .iter()
        .zip(&expected)
        .map(|(actual, expected)| (actual - expected).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        max_error < 0.002,
        "maximum CLI Vorbis-to-WAVE sample error {max_error}"
    );
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
    let first_title = "F".repeat(300);
    fs::write(
        &cue_path,
        format!(
            "FILE \"{}\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"{}\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Second\"\n    INDEX 01 00:00:01\n",
            audio_path.file_name().unwrap().to_string_lossy(),
            first_title
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
    let first_output_filename = format!("01 - {}.wav", "F".repeat(200));
    for (name, expected, title) in [
        (
            first_output_filename.clone(),
            &samples[..588],
            first_title.as_str(),
        ),
        ("02 - Second.wav".to_owned(), &samples[588..], "Second"),
    ] {
        let mut decoder = wav::WavDecoder::new(File::open(output_dir.join(name)).unwrap()).unwrap();
        assert_eq!(
            decoder.read_frames(588).unwrap().unwrap().samples(),
            &Samples::I16(expected.to_vec())
        );
        assert_eq!(decoder.metadata().values("TITLE"), [title]);
    }

    let output = Command::new(cli_path())
        .args(["cue", "create", "--json"])
        .arg(&output_cue)
        .arg(output_dir.join(&first_output_filename))
        .arg(output_dir.join("02 - Second.wav"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let generated = fs::read_to_string(&output_cue).unwrap();
    assert!(generated.contains(&format!("FILE \"{first_output_filename}\" WAVE")));
    assert!(generated.contains(&format!("TITLE \"{first_title}\"")));
    assert!(generated.contains("TITLE \"Second\""));

    let _ = fs::remove_file(audio_path);
    let _ = fs::remove_file(cue_path);
    let _ = fs::remove_dir_all(output_dir);
}

#[test]
fn cue_split_rejects_oversized_sheets_before_creating_output() {
    let cue_path = temporary_path().with_extension("cue");
    let output_dir = temporary_directory();
    File::create(&cue_path)
        .unwrap()
        .set_len(cue::MAX_CUE_BYTES as u64 + 1)
        .unwrap();

    let output = Command::new(cli_path())
        .args(["cue", "split"])
        .arg(&cue_path)
        .arg(&output_dir)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("16 MiB"));
    assert!(!output_dir.exists());
    let _ = fs::remove_file(cue_path);
}

#[test]
fn dsd_cli_inspects_and_transcodes_wavpack_and_dsf_without_pcm() {
    let input = temporary_path().with_extension("wv");
    let dsf_path = temporary_path().with_extension("dsf");
    let dsdiff_path = temporary_path().with_extension("dff");
    let wavpack_path = temporary_path().with_extension("wv");
    fs::write(
        &input,
        include_bytes!("../crates/wavpack/tests/fixtures/dsd-direct-stereo.wv"),
    )
    .unwrap();
    let expected =
        include_bytes!("../crates/wavpack/tests/fixtures/dsd-direct-stereo.expected.dsd");

    let inspected = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    let report = String::from_utf8(inspected.stdout).unwrap();
    assert!(report.contains("\"format\":\"wavpack-dsd\""));
    assert!(report.contains("\"dsd_samples_per_channel\":32768"));

    for (source, target, format) in [
        (&input, &dsf_path, "dsf"),
        (&dsf_path, &dsdiff_path, "dsdiff"),
        (&dsdiff_path, &wavpack_path, "wavpack"),
    ] {
        let converted = Command::new(cli_path())
            .args(["convert", "--format", format, "--json"])
            .arg(source)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            converted.status.success(),
            "{}",
            String::from_utf8_lossy(&converted.stderr)
        );
        assert!(
            String::from_utf8(converted.stdout)
                .unwrap()
                .contains("\"dsd_samples_per_channel\":32768")
        );
    }

    let inspected = Command::new(cli_path())
        .args(["inspect", "--json"])
        .arg(&dsdiff_path)
        .output()
        .unwrap();
    assert!(
        inspected.status.success(),
        "{}",
        String::from_utf8_lossy(&inspected.stderr)
    );
    assert!(
        String::from_utf8(inspected.stdout)
            .unwrap()
            .contains("\"format\":\"dsdiff\"")
    );

    let mut dsf_decoder = DsfDecoder::new(File::open(&dsf_path).unwrap()).unwrap();
    assert_eq!(dsd_bytes(&mut dsf_decoder), expected);
    let mut dsdiff_decoder = DsdiffDecoder::new(File::open(&dsdiff_path).unwrap()).unwrap();
    assert_eq!(dsd_bytes(&mut dsdiff_decoder), expected);
    let mut wavpack_decoder = WavPackDsdDecoder::new(File::open(&wavpack_path).unwrap()).unwrap();
    assert_eq!(dsd_bytes(&mut wavpack_decoder), expected);

    let _ = fs::remove_file(input);
    let _ = fs::remove_file(dsf_path);
    let _ = fs::remove_file(dsdiff_path);
    let _ = fs::remove_file(wavpack_path);
}

#[test]
fn dsf_cli_transcode_preserves_opaque_id3_frames() {
    let input_path = temporary_path().with_extension("dsf");
    let output_path = temporary_path().with_extension("dsf");
    let spec = DsdSpec::new(2_822_400, 1).unwrap();
    let payload = b"org.example\0opaque image or private data";
    let mut body = Vec::from(&b"PRIV"[..]);
    body.extend_from_slice(&[
        ((payload.len() >> 21) & 0x7f) as u8,
        ((payload.len() >> 14) & 0x7f) as u8,
        ((payload.len() >> 7) & 0x7f) as u8,
        (payload.len() & 0x7f) as u8,
        0,
        0,
    ]);
    body.extend_from_slice(payload);
    let size = body.len();
    let mut id3_tag = Vec::from(&b"ID3\x04\0\0"[..]);
    id3_tag.extend_from_slice(&[
        ((size >> 21) & 0x7f) as u8,
        ((size >> 14) & 0x7f) as u8,
        ((size >> 7) & 0x7f) as u8,
        (size & 0x7f) as u8,
    ]);
    id3_tag.extend_from_slice(&body);
    let audio = DsdBuffer::new(spec, vec![0x96, 0x69]).unwrap();
    let mut encoder = DsfEncoder::new(File::create(&input_path).unwrap(), spec).unwrap();
    encoder.set_id3_tag(&id3_tag).unwrap();
    encoder.write_dsd_octets(&audio).unwrap();
    encoder.finish().unwrap();

    let converted = Command::new(cli_path())
        .args(["convert", "--format", "dsf", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    let decoder = DsfDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.id3_tag(), Some(id3_tag.as_slice()));
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn dsd_cli_preserves_partial_final_octet_sample_count() {
    let input = temporary_path().with_extension("dsf");
    let output_path = temporary_path().with_extension("dsf");
    let spec = DsdSpec::new(2_822_400, 2).unwrap();
    let source = DsdBuffer::new(spec, vec![0x80, 0x80, 0x80, 0x80]).unwrap();
    let mut encoder = DsfEncoder::new(File::create(&input).unwrap(), spec).unwrap();
    encoder.write_dsd_octets(&source).unwrap();
    encoder.set_sample_count(9).unwrap();
    encoder.finish().unwrap();

    let converted = Command::new(cli_path())
        .args(["convert", "--format", "dsf", "--json"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    assert!(
        String::from_utf8(converted.stdout)
            .unwrap()
            .contains("\"dsd_samples_per_channel\":9")
    );
    let mut decoder = DsfDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.sample_count(), 9);
    assert_eq!(dsd_bytes(&mut decoder), [0x80, 0x80, 0x80, 0x80]);

    let wavpack_path = temporary_path().with_extension("wv");
    let rejected = Command::new(cli_path())
        .args(["convert", "--format", "wavpack", "--json"])
        .arg(&output_path)
        .arg(&wavpack_path)
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8(rejected.stderr)
            .unwrap()
            .contains("\"kind\":\"unsupported\"")
    );
    assert!(!wavpack_path.exists());

    let _ = fs::remove_file(input);
    let _ = fs::remove_file(output_path);
    let _ = fs::remove_file(wavpack_path);
}

#[test]
fn dsdiff_conversion_preserves_typed_markers_and_comments() {
    let input_path = temporary_path();
    let output_path = temporary_path();
    let spec = DsdSpec::new(2_822_400, 2).unwrap();
    let marker = dsdiff::DsdiffMarker {
        hours: 0,
        minutes: 0,
        seconds: 1,
        samples: 42,
        offset: -3,
        kind: 0,
        channel: 0,
        track_flags: 0,
        text: "Track 1".into(),
    };
    let comment = dsdiff::DsdiffComment {
        year: 2024,
        month: 5,
        day: 6,
        hour: 7,
        minute: 8,
        kind: 1,
        reference: 2,
        text: "Right channel".into(),
    };
    let mut encoder = dsdiff::DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_markers(std::slice::from_ref(&marker)).unwrap();
    encoder
        .set_comments(std::slice::from_ref(&comment))
        .unwrap();
    encoder
        .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55, 0xaa]).unwrap())
        .unwrap();
    encoder.finish().unwrap();
    fs::write(&input_path, encoder.into_inner().into_inner()).unwrap();

    let output = Command::new(cli_path())
        .args(["convert", "--format", "dsdiff", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decoder = DsdiffDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(decoder.markers(), [marker]);
    assert_eq!(decoder.comments(), [comment]);
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn dsdiff_dst_conversion_writes_multiframe_compressed_audio_and_decodes_it() {
    let input_path = temporary_path();
    let output_path = temporary_path().with_extension("dff");
    let bypass_path = temporary_path().with_extension("bypass.dff");
    let spec = DsdSpec::new(2_822_400, 2).unwrap();
    let frame_bytes = 4_704;
    let mut dsd = Vec::with_capacity(frame_bytes * 2 * 3);
    for index in 0..frame_bytes * 3 {
        let frame = index / frame_bytes;
        let byte = match frame {
            0 => 0xff,
            1 => 0x55,
            _ => {
                if index / 16 % 2 == 0 {
                    0xff
                } else {
                    0x00
                }
            }
        };
        dsd.extend_from_slice(&[byte, !byte]);
    }
    let audio = DsdBuffer::new(spec, dsd).unwrap();
    let mut encoder = dsdiff::DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_dsd_octets(&audio).unwrap();
    encoder.finish().unwrap();
    fs::write(&input_path, encoder.into_inner().into_inner()).unwrap();
    let output = Command::new(cli_path())
        .args(["convert", "--format", "dsdiff_dst", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bypass = Command::new(cli_path())
        .args(["convert", "--format", "dsdiff", "--json"])
        .arg(&input_path)
        .arg(&bypass_path)
        .output()
        .unwrap();
    assert!(
        bypass.status.success(),
        "{}",
        String::from_utf8_lossy(&bypass.stderr)
    );
    let bytes = fs::read(&output_path).unwrap();
    let frame_header = bytes
        .windows(4)
        .position(|window| window == b"DSTF")
        .unwrap();
    assert_ne!(bytes[frame_header + 12] & 0x80, 0);
    let mut decoder = DsdiffDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(dsd_bytes(&mut decoder), audio.bytes());
    let ffmpeg_has_dst = Command::new("ffmpeg")
        .arg("-decoders")
        .output()
        .is_ok_and(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.split_whitespace().any(|field| field == "dst"))
        });
    if ffmpeg_has_dst {
        let oracle_path = temporary_path().with_extension("f32");
        let oracle = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&output_path)
            .args(["-f", "f32le", "-acodec", "pcm_f32le"])
            .arg(&oracle_path)
            .output()
            .unwrap();
        assert!(
            oracle.status.success(),
            "{}",
            String::from_utf8_lossy(&oracle.stderr)
        );
        let decoded = fs::read(&oracle_path).unwrap();
        let bypass_oracle_path = temporary_path().with_extension("f32");
        let bypass_oracle = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&bypass_path)
            .args(["-f", "f32le", "-acodec", "pcm_f32le"])
            .arg(&bypass_oracle_path)
            .output()
            .unwrap();
        assert!(
            bypass_oracle.status.success(),
            "{}",
            String::from_utf8_lossy(&bypass_oracle.stderr)
        );
        assert_eq!(decoded, fs::read(&bypass_oracle_path).unwrap());
        let _ = fs::remove_file(oracle_path);
        let _ = fs::remove_file(bypass_oracle_path);
    }
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
    let _ = fs::remove_file(bypass_path);
}

#[test]
#[ignore = "requires the external CC0 DSF sample and FFmpeg DSF decoder"]
fn dsdiff_dst_real_audio_matches_ffmpeg_pcm_oracle() {
    let sample_path = std::env::var_os("ZEMER_DSDIFF_CC0_DSF")
        .map(PathBuf::from)
        .expect("set ZEMER_DSDIFF_CC0_DSF to the external CC0 Variatio 5 DSD64 DSF sample");
    let input_path = temporary_path().with_extension("dsf");
    let output_path = temporary_path().with_extension("dff");
    let round_trip_path = temporary_path().with_extension("dsf");
    let source_pcm_path = temporary_path().with_extension("source.f32le");
    let output_pcm_path = temporary_path().with_extension("output.f32le");

    let mut sample = DsfDecoder::new(File::open(sample_path).unwrap()).unwrap();
    let metadata = sample.metadata().clone();
    let spec = sample.dsd_spec();
    // Ten complete frames keep this real-audio CLI and external-oracle check fast.
    let frame_octets = 4_704 * 10;
    let mut clip = DsfEncoder::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&input_path)
            .unwrap(),
        spec,
    )
    .unwrap();
    clip.set_metadata(&metadata).unwrap();
    let mut copied_octets = 0;
    while copied_octets < frame_octets {
        let audio = sample
            .read_dsd_octets(frame_octets - copied_octets)
            .unwrap()
            .unwrap();
        copied_octets += audio.frames();
        clip.write_dsd_octets(&audio).unwrap();
    }
    clip.finish().unwrap();

    let source = DsfDecoder::new(File::open(&input_path).unwrap()).unwrap();
    let output = Command::new(cli_path())
        .args(["convert", "--format", "dsdiff_dst", "--json"])
        .arg(&input_path)
        .arg(&output_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dsdiff = DsdiffDecoder::new(File::open(&output_path).unwrap()).unwrap();
    assert_eq!(dsdiff.dsd_spec(), source.dsd_spec());
    assert_eq!(dsdiff.sample_count(), Some(source.sample_count()));
    assert_eq!(
        dsdiff.metadata().values("TITLE"),
        source.metadata().values("TITLE")
    );
    assert_eq!(
        dsdiff.metadata().values("ARTIST"),
        source.metadata().values("ARTIST")
    );
    assert!(!source.metadata().values("ALBUM").is_empty());
    assert!(dsdiff.metadata().values("ALBUM").is_empty());
    let restored = Command::new(cli_path())
        .args(["convert", "--format", "dsf", "--json"])
        .arg(&output_path)
        .arg(&round_trip_path)
        .output()
        .unwrap();
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );

    for (input, pcm) in [
        (&input_path, &source_pcm_path),
        (&round_trip_path, &output_pcm_path),
    ] {
        let decoded = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(input)
            .args(["-f", "f32le", "-acodec", "pcm_f32le"])
            .arg(pcm)
            .output()
            .unwrap();
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
    }
    let source_pcm = fs::read(&source_pcm_path).unwrap();
    assert!(!source_pcm.is_empty());
    assert_eq!(source_pcm, fs::read(&output_pcm_path).unwrap());

    for path in [
        input_path,
        output_path,
        round_trip_path,
        source_pcm_path,
        output_pcm_path,
    ] {
        let _ = fs::remove_file(path);
    }
}
