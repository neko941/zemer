use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

use flac::FlacDecoder;
use zemer_core::AudioDecoder;

#[test]
#[ignore = "requires the CC0 FLAC decoder testbench in ZEMER_FLAC_CORPUS"]
fn checks_the_official_flac_decoder_testbench() {
    let root = PathBuf::from(
        std::env::var_os("ZEMER_FLAC_CORPUS")
            .expect("set ZEMER_FLAC_CORPUS to the extracted flac-test-files directory"),
    );
    let mut subset = files_in(&root.join("subset")).unwrap();
    let mut uncommon = files_in(&root.join("uncommon")).unwrap();
    let mut faulty = files_in(&root.join("faulty")).unwrap();
    subset.sort();
    uncommon.sort();
    faulty.sort();

    for path in &subset {
        let result = decode(path);
        assert!(result.is_ok(), "{} must decode: {result:?}", path.display());
    }
    for path in &uncommon {
        let case = case_number(path);
        let result = decode(path);
        if matches!(case, 5..=9) {
            assert!(result.is_ok(), "{} must decode", path.display());
        } else {
            assert!(result.is_err(), "{} must be rejected", path.display());
        }
    }
    for path in &faulty {
        assert!(decode(path).is_err(), "{} must be rejected", path.display());
    }

    assert_eq!(subset.len(), 64, "unexpected subset corpus size");
    assert_eq!(uncommon.len(), 11, "unexpected uncommon corpus size");
    assert_eq!(faulty.len(), 11, "unexpected faulty corpus size");
}

fn files_in(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "flac")
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn case_number(path: &Path) -> u8 {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.get(..2))
        .and_then(|digits| digits.parse().ok())
        .expect("testbench cases have a two-digit prefix")
}

fn decode(path: &Path) -> Result<(), zemer_core::AudioError> {
    let mut decoder = FlacDecoder::new(File::open(path)?)?;
    while decoder.read_frames(8192)?.is_some() {}
    Ok(())
}
