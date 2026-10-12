use std::{
    fs,
    fs::File,
    io,
    path::{Path, PathBuf},
};

use wavpack::{WavPackBlockHeader, WavPackDecoder, WavPackDsdDecoder};
use zemer_core::{AudioDecoder, DsdDecoder};

const DSD_FLAG: u32 = 1 << 31;

#[test]
#[ignore = "requires the external WavPack decoder test suite in ZEMER_WAVPACK_CORPUS"]
fn decodes_supported_wavpack_official_corpus() {
    let root = PathBuf::from(
        std::env::var_os("ZEMER_WAVPACK_CORPUS")
            .expect("set ZEMER_WAVPACK_CORPUS to the extracted test_suite directory"),
    );
    let mut files = Vec::new();
    collect_wavpack_files(&root, &mut files).unwrap();
    files.sort();

    let mut decoded = 0;
    let mut dsd = 0;
    let mut corrected = 0;
    let mut skipped_legacy = 0;
    let mut corruption_cases = 0;

    for path in files {
        let is_legacy = path.components().any(|part| part.as_os_str() == "legacy");
        let is_corruption = path
            .components()
            .any(|part| part.as_os_str() == "corruption");
        let mut input = File::open(&path).unwrap();
        let header = match WavPackBlockHeader::read_from(&mut input) {
            Ok(Some(header)) => header,
            Ok(None) | Err(_) if is_corruption => {
                corruption_cases += 1;
                continue;
            }
            Ok(None) | Err(_) if is_legacy => {
                skipped_legacy += 1;
                continue;
            }
            Ok(None) => panic!("empty WavPack file: {}", path.display()),
            Err(error) => panic!("{} has an invalid header: {error}", path.display()),
        };
        if header.version < 0x402 {
            skipped_legacy += 1;
            continue;
        }

        let correction = correction_path(&root, &path);
        let result = if header.flags & DSD_FLAG != 0 {
            dsd += 1;
            decode_dsd(&path)
        } else {
            if correction.is_some() {
                corrected += 1;
            }
            decode_pcm(&path, correction.as_deref())
        };

        if is_corruption {
            assert!(
                result.is_err(),
                "{} corruption fixture decoded successfully",
                path.display()
            );
            corruption_cases += 1;
        } else {
            result.unwrap_or_else(|error| panic!("{} failed to decode: {error}", path.display()));
            decoded += 1;
        }
    }

    assert!(
        decoded > 0,
        "the corpus contained no supported WavPack files"
    );
    assert!(dsd > 0, "the corpus contained no DSD files");
    assert!(corrected > 0, "correction files were not exercised");
    assert!(corruption_cases > 0, "corruption files were not exercised");
    eprintln!(
        "decoded {decoded} files ({dsd} DSD, {corrected} with correction); skipped {skipped_legacy} legacy files; checked {corruption_cases} corruption files"
    );
}

fn collect_wavpack_files(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_wavpack_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "wv") {
            files.push(path);
        }
    }
    Ok(())
}

fn correction_path(root: &Path, audio: &Path) -> Option<PathBuf> {
    let name = audio.file_stem()?;
    let adjacent = audio.with_extension("wvc");
    if adjacent.is_file() {
        return Some(adjacent);
    }

    let suite_correction = root
        .join("hybrid_bitrates")
        .join("wvc_files")
        .join(name)
        .with_extension("wvc");
    suite_correction.is_file().then_some(suite_correction)
}

fn decode_pcm(audio: &Path, correction: Option<&Path>) -> Result<(), zemer_core::AudioError> {
    if let Some(correction) = correction {
        let mut decoder =
            WavPackDecoder::new_with_correction(File::open(audio)?, File::open(correction)?)?;
        drain_pcm(&mut decoder)
    } else {
        let mut decoder = WavPackDecoder::new(File::open(audio)?)?;
        drain_pcm(&mut decoder)
    }
}

fn drain_pcm<D: AudioDecoder>(decoder: &mut D) -> Result<(), zemer_core::AudioError> {
    while decoder.read_frames(8192)?.is_some() {}
    Ok(())
}

fn decode_dsd(audio: &Path) -> Result<(), zemer_core::AudioError> {
    let mut decoder = WavPackDsdDecoder::new(File::open(audio)?)?;
    while decoder.read_dsd_octets(8192)?.is_some() {}
    Ok(())
}
