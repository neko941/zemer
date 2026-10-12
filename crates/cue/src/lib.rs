//! CUE sheet types and checked conversion between 75 Hz CUE frames and audio frames.

pub use cue_rw::{CUEFile, CUETimeStamp, CUETrack, TrackFlags};

/// Decode a UTF-8 or Shift-JIS CUE sheet and require WAVE audio tracks.
pub fn parse_wav(bytes: &[u8]) -> Result<CUEFile, String> {
    let text = if let Ok(text) = std::str::from_utf8(bytes) {
        text
    } else {
        let (decoded, _, had_errors) = encoding_rs::SHIFT_JIS.decode(bytes);
        if had_errors {
            return Err("CUE text is neither valid UTF-8 nor Shift-JIS".to_owned());
        }
        return parse_wav_text(&decoded);
    };
    parse_wav_text(text)
}

fn parse_wav_text(text: &str) -> Result<CUEFile, String> {
    for line in text.lines() {
        let mut words = line.split_ascii_whitespace();
        let Some(command) = words.next() else {
            continue;
        };
        let Some(kind) = line.split_ascii_whitespace().last() else {
            continue;
        };
        match command.to_ascii_uppercase().as_str() {
            "FILE" if !kind.eq_ignore_ascii_case("WAVE") => {
                return Err(format!("only FILE ... WAVE is supported: {line}"));
            }
            "TRACK" if !kind.eq_ignore_ascii_case("AUDIO") => {
                return Err(format!("only TRACK ... AUDIO is supported: {line}"));
            }
            _ => {}
        }
    }
    CUEFile::parse(text).map_err(|error| error.to_string())
}

/// Convert a CUE timestamp to an audio frame position without rounding.
pub fn cue_frame(timestamp: CUETimeStamp) -> Result<u64, &'static str> {
    let mut components = timestamp.to_string().split(':').map(|part| {
        part.parse::<u64>().map_err(|_| "invalid CUE timestamp")
    });
    let minutes = components.next().ok_or("invalid CUE timestamp")??;
    let seconds = components.next().ok_or("invalid CUE timestamp")??;
    let fractions = components.next().ok_or("invalid CUE timestamp")??;
    if seconds >= 60 || fractions >= 75 || components.next().is_some() {
        return Err("invalid CUE timestamp");
    }
    let cue_frames = minutes
        .checked_mul(60 * 75)
        .and_then(|value| value.checked_add(seconds * 75))
        .and_then(|value| value.checked_add(fractions))
        .ok_or("CUE timestamp overflow")?;
    Ok(cue_frames)
}

/// Convert a CUE timestamp to an audio frame position without rounding.
pub fn sample_frame(timestamp: CUETimeStamp, sample_rate: u32) -> Result<u64, &'static str> {
    let scaled = cue_frame(timestamp)?
        .checked_mul(u64::from(sample_rate))
        .ok_or("CUE timestamp is too large")?;
    if scaled % 75 != 0 {
        return Err("CUE timestamp does not fall on an audio sample frame");
    }
    Ok(scaled / 75)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cue_sectors_map_to_sample_frames_without_rounding() {
        assert_eq!(sample_frame(CUETimeStamp::new(1, 2, 3), 44_100), Ok(2_735_964));
        assert_eq!(
            sample_frame(CUETimeStamp::new(0, 0, 1), 44_100),
            Ok(588)
        );
        assert!(sample_frame(CUETimeStamp::new(0, 0, 1), 8_000).is_err());
    }

    #[test]
    fn parses_utf8_and_shift_jis_wave_sheets_and_rejects_data_tracks() {
        let utf8 = b"FILE \"a.wav\" WAVE\n TRACK 01 AUDIO\n INDEX 01 00:00:00\n";
        assert_eq!(parse_wav(utf8).unwrap().files, ["a.wav"]);
        let shift_jis = [
            b"FILE \"", &[0x83, 0x65, 0x83, 0x58, 0x83, 67], b".wav\" WAVE\n TRACK 01 AUDIO\n INDEX 01 00:00:00\n",
        ]
        .concat();
        assert!(parse_wav(&shift_jis).unwrap().files[0].contains("テスト"));
        assert!(parse_wav(b"FILE \"disc.bin\" BINARY\n TRACK 01 MODE1/2352\n INDEX 01 00:00:00\n").is_err());
    }
}
