use std::{
    fs::{self, File},
    io::{self, Cursor},
    path::{Path, PathBuf},
};

use flac::{FlacDecoder, OggFlacDecoder, OggFlacEncoder};
use zemer_core::{AudioDecoder, AudioEncoder, PcmBuffer, Samples};

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
        let result = round_trip_ogg(path);
        assert!(result.is_ok(), "{} must decode: {result:?}", path.display());
    }
    for path in &uncommon {
        let case = case_number(path);
        let result = round_trip_ogg(path);
        if matches!(case, 5..=9) {
            assert!(
                result.is_ok(),
                "{} must decode and Ogg-round-trip",
                path.display()
            );
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

fn round_trip_ogg(path: &Path) -> Result<(), zemer_core::AudioError> {
    let mut source = FlacDecoder::new(File::open(path)?)?;
    let spec = source.spec();
    let metadata = source.metadata().clone();
    let mut encoder = OggFlacEncoder::new(Vec::new(), spec)?;
    encoder.set_metadata(&metadata)?;
    while let Some(block) = source.read_frames(8192)? {
        encoder.write_block(&block)?;
    }
    encoder.finish()?;

    let mut source = FlacDecoder::new(File::open(path)?)?;
    let mut actual = OggFlacDecoder::new(Cursor::new(encoder.into_inner()))?;
    if actual.spec() != spec {
        return Err(zemer_core::AudioError::InvalidData(
            "Ogg-FLAC round trip changed the audio specification",
        ));
    }
    compare_samples(&mut source, &mut actual)
}

fn decode(path: &Path) -> Result<(), zemer_core::AudioError> {
    let mut decoder = FlacDecoder::new(File::open(path)?)?;
    while decoder.read_frames(8192)?.is_some() {}
    Ok(())
}

fn compare_samples<A: AudioDecoder, B: AudioDecoder>(
    expected: &mut A,
    actual: &mut B,
) -> Result<(), zemer_core::AudioError> {
    let mut expected_samples = Vec::new();
    let mut actual_samples = Vec::new();
    let mut expected_position = 0;
    let mut actual_position = 0;
    let mut expected_done = false;
    let mut actual_done = false;
    loop {
        if expected_position == expected_samples.len() && !expected_done {
            match expected.read_frames(8192)? {
                Some(block) => {
                    expected_samples = integer_samples(block)?;
                    expected_position = 0;
                }
                None => expected_done = true,
            }
        }
        if actual_position == actual_samples.len() && !actual_done {
            match actual.read_frames(8192)? {
                Some(block) => {
                    actual_samples = integer_samples(block)?;
                    actual_position = 0;
                }
                None => actual_done = true,
            }
        }
        let remaining_expected = expected_samples.len() - expected_position;
        let remaining_actual = actual_samples.len() - actual_position;
        if remaining_expected == 0 && remaining_actual == 0 {
            if expected_done && actual_done {
                return Ok(());
            }
            continue;
        }
        let count = remaining_expected.min(remaining_actual);
        if (remaining_expected == 0 && expected_done)
            || (remaining_actual == 0 && actual_done)
            || count == 0
            || expected_samples[expected_position..expected_position + count]
                != actual_samples[actual_position..actual_position + count]
        {
            return Err(zemer_core::AudioError::InvalidData(
                "Ogg-FLAC round trip changed decoded samples",
            ));
        }
        expected_position += count;
        actual_position += count;
    }
}

fn integer_samples(block: PcmBuffer) -> Result<Vec<i64>, zemer_core::AudioError> {
    let samples = match block.samples() {
        Samples::U8(values) => values.iter().map(|&sample| i64::from(sample)).collect(),
        Samples::I16(values) => values.iter().map(|&sample| i64::from(sample)).collect(),
        Samples::I24(values) | Samples::I32(values) => {
            values.iter().map(|&sample| i64::from(sample)).collect()
        }
        Samples::F32(_) | Samples::F64(_) => {
            return Err(zemer_core::AudioError::Unsupported(
                "FLAC test corpus returned floating-point samples",
            ));
        }
    };
    Ok(samples)
}
