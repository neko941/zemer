use std::{fs::File, io::Read, path::PathBuf};

use caf::CafDecoder;
use zemer_core::{AudioDecoder, AudioSpec, SampleFormat, Samples};

#[test]
fn decodes_ffmpeg_big_endian_pcm_fixture_exactly() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/caf");
    let mut decoder =
        CafDecoder::new(File::open(fixtures.join("ffmpeg-1.0-s16be.caf")).unwrap()).unwrap();
    assert_eq!(
        decoder.spec(),
        AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap()
    );

    let mut expected = Vec::new();
    File::open(fixtures.join("oracle.s16le"))
        .unwrap()
        .read_to_end(&mut expected)
        .unwrap();
    let mut actual = Vec::new();
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(73).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("CAF decoder must return signed 16-bit samples");
        };
        frames += block.frames();
        for sample in samples {
            actual.extend_from_slice(&sample.to_le_bytes());
        }
    }
    assert_eq!(frames, 512);
    assert_eq!(actual, expected);
}
