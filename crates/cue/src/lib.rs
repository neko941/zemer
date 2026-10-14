//! Clean-room parsing, writing, and checked timestamp conversion for WAVE
//! audio CUE sheets.

mod model;
mod parse;

pub use model::{CUEFile, CUETimeStamp, CUETrack, FileID, TrackFlags};

pub const MAX_CUE_BYTES: usize = 16 * 1024 * 1024;

/// Decode a UTF-8 or Shift-JIS CUE sheet and require WAVE audio tracks.
pub fn parse_wav(bytes: &[u8]) -> Result<CUEFile, String> {
    if bytes.len() > MAX_CUE_BYTES {
        return Err("CUE sheet exceeds the 16 MiB size limit".to_owned());
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return CUEFile::parse(text);
    }
    let (text, _, had_errors) = encoding_rs::SHIFT_JIS.decode(bytes);
    if had_errors {
        return Err("CUE text is neither valid UTF-8 nor Shift-JIS".to_owned());
    }
    CUEFile::parse(&text)
}

/// Convert a CUE timestamp to a 75 Hz frame position without rounding.
pub fn cue_frame(timestamp: CUETimeStamp) -> Result<u64, &'static str> {
    if timestamp.seconds() >= 60 || timestamp.fractions() >= 75 {
        return Err("invalid CUE timestamp");
    }
    u64::from(timestamp.minutes())
        .checked_mul(60 * 75)
        .and_then(|value| value.checked_add(u64::from(timestamp.seconds()) * 75))
        .and_then(|value| value.checked_add(u64::from(timestamp.fractions())))
        .ok_or("CUE timestamp overflow")
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
        assert_eq!(
            sample_frame(CUETimeStamp::new(1, 2, 3), 44_100),
            Ok(2_735_964)
        );
        assert_eq!(sample_frame(CUETimeStamp::new(0, 0, 1), 44_100), Ok(588));
        assert!(sample_frame(CUETimeStamp::new(0, 0, 1), 8_000).is_err());
        assert_eq!(cue_frame(CUETimeStamp::new(255, 0, 0)), Ok(1_147_500));
    }

    #[test]
    fn parses_utf8_and_shift_jis_wave_sheets_and_rejects_data_tracks() {
        let utf8 = b"FILE \"a.wav\" WAVE\n TRACK 01 AUDIO\n INDEX 01 00:00:00\n";
        assert_eq!(parse_wav(utf8).unwrap().files, ["a.wav"]);
        let mut shift_jis = b"FILE \"".to_vec();
        shift_jis.extend([0x83, 0x65, 0x83, 0x58, 0x83, 0x67]);
        shift_jis.extend_from_slice(b".wav\" WAVE\n TRACK 01 AUDIO\n INDEX 01 00:00:00\n");
        assert!(parse_wav(&shift_jis).unwrap().files[0].contains("テスト"));
        assert!(
            parse_wav(b"FILE \"disc.bin\" BINARY\n TRACK 01 MODE1/2352\n INDEX 01 00:00:00\n")
                .is_err()
        );
    }

    #[test]
    fn writes_and_reads_supported_disc_and_track_metadata() {
        let source = concat!(
            "REM GENRE Jazz\n",
            "CATALOG 1234567890123\n",
            "TITLE \"An album\"\n",
            "PERFORMER \"An artist\"\n",
            "SONGWRITER \"A writer\"\n",
            "FILE \"one track.wav\" WAVE\n",
            "  TRACK 01 AUDIO\n",
            "    TITLE \"A \\\"quoted\\\" title\"\n",
            "    FLAGS DCP PRE\n",
            "    ISRC USABC1234567\n",
            "    PREGAP 00:00:10\n",
            "    INDEX 00 00:00:00\n",
            "    INDEX 01 00:00:10\n",
            "    POSTGAP 00:00:03\n",
            "    REM SOURCE test\n",
        );
        let sheet = CUEFile::parse(source).unwrap();
        assert_eq!(
            sheet.tracks[0].1.title.as_deref(),
            Some("A \"quoted\" title")
        );
        assert!(sheet.tracks[0].1.flags.unwrap().contains(TrackFlags::DCP));
        let serialized = sheet.to_string();
        assert_eq!(CUEFile::parse(&serialized).unwrap(), sheet);
    }

    #[test]
    fn rejects_malformed_structure_and_oversized_sheets() {
        for input in [
            "TRACK 01 AUDIO\nINDEX 01 00:00:00",
            "FILE \"a.bin\" BINARY\nTRACK 01 AUDIO\nINDEX 01 00:00:00",
            "FILE \"a.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:60:00",
            "FILE \"a.wav\" WAVE\nTRACK 02 AUDIO\nINDEX 01 00:00:00",
        ] {
            assert!(CUEFile::parse(input).is_err(), "accepted: {input}");
        }
        let oversized = " ".repeat(MAX_CUE_BYTES + 1);
        assert!(CUEFile::parse(&oversized).is_err());
        assert!(parse_wav(oversized.as_bytes()).is_err());
    }

    #[test]
    fn refuses_to_serialize_metadata_as_extra_cue_commands() {
        let mut sheet =
            CUEFile::parse("FILE \"one.wav\" WAVE\nTRACK 01 AUDIO\nINDEX 01 00:00:00\n").unwrap();
        sheet.tracks[0].1.title = Some("safe\nTRACK 02 AUDIO".to_owned());
        assert!(sheet.to_cue_string().is_err());
    }
}
