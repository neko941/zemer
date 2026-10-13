/// A DSDIFF `DIIN/MARK` position and description.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DsdiffMarker {
    pub hours: u16,
    pub minutes: u8,
    pub seconds: u8,
    pub samples: u32,
    pub offset: i32,
    pub kind: u16,
    pub channel: u16,
    pub track_flags: u16,
    pub text: String,
}

pub(crate) fn validate_markers(
    markers: &[DsdiffMarker],
    sample_rate: u32,
    channel_ids: &[[u8; 4]],
) -> Result<(), zemer_core::AudioError> {
    for marker in markers {
        if marker.hours > 23
            || marker.minutes > 59
            || marker.seconds > 59
            || marker.samples >= sample_rate
            || usize::from(marker.channel) > channel_ids.len()
            || marker.track_flags & !0x070f != 0
            || (marker.kind != 0 || marker.channel != 0) && marker.track_flags != 0
            || marker.text.len() > u32::MAX as usize
            || !marker
                .text
                .bytes()
                .all(|byte| (0x20..=0x7e).contains(&byte))
        {
            return Err(zemer_core::AudioError::InvalidData("invalid DSDIFF marker"));
        }
        if marker.track_flags != 0 {
            let layout = channel_ids;
            let flags = marker.track_flags;
            let valid = match layout {
                [left, right] if left == b"SLFT" && right == b"SRGT" => flags == 0,
                [left, right, center, ls, rs]
                    if left == b"MLFT"
                        && right == b"MRGT"
                        && center == b"C   "
                        && ls == b"LS  "
                        && rs == b"RS  " =>
                {
                    flags & 0x0701 == 0 && flags & 0x000e != 0x000e
                }
                [left, right, center, lfe, ls, rs]
                    if left == b"MLFT"
                        && right == b"MRGT"
                        && center == b"C   "
                        && lfe == b"LFE "
                        && ls == b"LS  "
                        && rs == b"RS  " =>
                {
                    flags & 0x0700 == 0 && flags & 0x0002 == 0 && flags & 0x000f != 0x000f
                }
                _ => false,
            };
            if !valid {
                return Err(zemer_core::AudioError::InvalidData(
                    "DSDIFF track flags do not match the channel layout",
                ));
            }
        }
    }
    Ok(())
}
