use std::{
    fs::{self, File},
    io::Cursor,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use dsdiff::{DsdiffComment, DsdiffDecoder, DsdiffEncoder, DsdiffMarker};
use zemer_core::{DsdBuffer, DsdDecoder, DsdEncoder, DsdSpec, Metadata};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

fn temporary_path(extension: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "zemer-dsd-cli-{}-{}.{extension}",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn converts_raw_dsd_to_dst_and_edits_metadata_without_losing_dsd_or_markers() {
    let spec = DsdSpec::new(2_822_400, 2).unwrap();
    let frame_bytes = 4_704;
    let audio = (0..frame_bytes * 2)
        .map(|index| (index as u8).wrapping_mul(37))
        .collect::<Vec<_>>();
    let input = temporary_path("dff");
    let compressed = temporary_path("dff");
    let edited = temporary_path("dff");
    let comments = [
        DsdiffComment {
            year: 2025,
            month: 2,
            day: 3,
            hour: 4,
            minute: 5,
            kind: 0,
            reference: 0,
            text: "Keep comment".into(),
        },
        DsdiffComment {
            year: 2025,
            month: 2,
            day: 3,
            hour: 4,
            minute: 5,
            kind: 1,
            reference: 1,
            text: "Channel comment".into(),
        },
    ];
    let marker = DsdiffMarker {
        hours: 0,
        minutes: 0,
        seconds: 0,
        samples: 123,
        offset: -2,
        kind: 2,
        channel: 0,
        track_flags: 0,
        text: "Keep marker".into(),
    };
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "Before");
    metadata.insert("COMMENT", "Keep comment");
    let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_metadata(&metadata).unwrap();
    encoder.set_comments(&comments).unwrap();
    encoder.set_markers(std::slice::from_ref(&marker)).unwrap();
    encoder
        .write_dsd_octets(&DsdBuffer::new(spec, audio.clone()).unwrap())
        .unwrap();
    encoder.finish().unwrap();
    fs::write(&input, encoder.into_inner().into_inner()).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_zemer_test_cli"))
        .args([
            "convert",
            "--format",
            "dsdiff_dst",
            input.to_str().unwrap(),
            compressed.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success());

    let mut decoder = DsdiffDecoder::new(File::open(&compressed).unwrap()).unwrap();
    assert_eq!(decoder.comments(), comments);
    assert_eq!(decoder.markers(), std::slice::from_ref(&marker));
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_dsd_octets(257).unwrap() {
        decoded.extend_from_slice(block.bytes());
    }
    assert_eq!(decoded, audio);

    let status = Command::new(env!("CARGO_BIN_EXE_zemer_test_cli"))
        .args([
            "metadata",
            "edit",
            compressed.to_str().unwrap(),
            edited.to_str().unwrap(),
            "--set",
            "TITLE=After",
        ])
        .status()
        .unwrap();
    assert!(status.success());

    let mut decoder = DsdiffDecoder::new(File::open(&edited).unwrap()).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["After"]);
    assert_eq!(decoder.comments(), comments);
    assert_eq!(decoder.markers(), [marker]);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_dsd_octets(257).unwrap() {
        decoded.extend_from_slice(block.bytes());
    }
    assert_eq!(decoded, audio);

    let _ = fs::remove_file(input);
    let _ = fs::remove_file(compressed);
    let _ = fs::remove_file(edited);
}
