use std::{
    env, fs,
    fs::File,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use wav::{WavDecoder, WavWriter};
use zemer_core::{AudioDecoder, AudioEncoder, AudioSpec, PcmBuffer, SampleFormat, Samples};

#[test]
#[ignore = "requires FFmpeg with Microsoft ADPCM encode/decode support"]
fn microsoft_adpcm_interoperates_with_ffmpeg_in_mono_and_stereo() {
    let ffmpeg = env::var_os("ZEMER_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = env::temp_dir().join(format!(
        "zemer-wav-ms-adpcm-interop-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&dir).unwrap();

    for channels in [1, 2] {
        let spec = AudioSpec::new(44_100, channels, SampleFormat::PcmI16).unwrap();
        let source: Vec<i16> = (0..1203 * usize::from(channels))
            .map(|index| ((index * 7919 + 12_345) % 65_536) as i32 as i16)
            .collect();
        let audio = PcmBuffer::new(spec, Samples::I16(source.clone())).unwrap();
        let pcm_wav = dir.join(format!("source-{channels}.wav"));
        let mut writer = WavWriter::new(File::create(&pcm_wav).unwrap(), spec).unwrap();
        writer.write_block(&audio).unwrap();
        writer.finish().unwrap();

        let zemer_wav = dir.join(format!("zemer-{channels}.wav"));
        let mut writer = WavWriter::new_ms_adpcm(File::create(&zemer_wav).unwrap(), spec).unwrap();
        writer.write_block(&audio).unwrap();
        writer.finish().unwrap();

        let ffmpeg_wav = dir.join(format!("ffmpeg-{channels}.wav"));
        run_ffmpeg(
            &ffmpeg,
            &pcm_wav,
            &ffmpeg_wav,
            &["-c:a", "adpcm_ms", "-block_size", "256"],
        );

        let zemer_pcm = decode_zemer(&zemer_wav);
        let ffmpeg_decoded_zemer = decode_ffmpeg(&ffmpeg, &zemer_wav, &dir, "zemer", channels);
        assert_eq!(zemer_pcm.len(), source.len());
        assert_eq!(
            &ffmpeg_decoded_zemer[..zemer_pcm.len()],
            zemer_pcm,
            "FFmpeg did not decode the Zemer {channels}-channel stream identically"
        );

        let zemer_decoded_ffmpeg = decode_zemer(&ffmpeg_wav);
        let ffmpeg_decoded_ffmpeg = decode_ffmpeg(&ffmpeg, &ffmpeg_wav, &dir, "ffmpeg", channels);
        assert_eq!(zemer_decoded_ffmpeg.len(), source.len());
        assert_eq!(
            &ffmpeg_decoded_ffmpeg[..zemer_decoded_ffmpeg.len()],
            zemer_decoded_ffmpeg,
            "Zemer did not decode the FFmpeg {channels}-channel stream identically"
        );
    }

    fs::remove_dir_all(dir).unwrap();
}

fn decode_zemer(path: &Path) -> Vec<i16> {
    let mut decoder = WavDecoder::new(File::open(path).unwrap()).unwrap();
    let mut output = Vec::new();
    while let Some(block) = decoder.read_frames(4096).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("Microsoft ADPCM must decode to signed 16-bit PCM");
        };
        output.extend_from_slice(samples);
    }
    output
}

fn decode_ffmpeg(
    ffmpeg: &std::ffi::OsStr,
    input: &Path,
    dir: &Path,
    label: &str,
    channels: u16,
) -> Vec<i16> {
    let output_path = dir.join(format!("{label}-decoded-{channels}.pcm"));
    run_ffmpeg(ffmpeg, input, &output_path, &["-f", "s16le"]);
    let bytes = fs::read(output_path).unwrap();
    assert_eq!(bytes.len() % 2, 0);
    bytes
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
        .collect()
}

fn run_ffmpeg(ffmpeg: &std::ffi::OsStr, input: &Path, output_path: &Path, options: &[&str]) {
    let output = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(input)
        .args(options)
        .arg(output_path)
        .output()
        .unwrap_or_else(|error| panic!("could not launch FFmpeg: {error}"));
    assert!(
        output.status.success(),
        "FFmpeg failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
