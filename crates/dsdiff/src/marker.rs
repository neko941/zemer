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

pub(crate) const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn add_marker_text_length(
    total: &mut usize,
    length: usize,
) -> Result<(), zemer_core::AudioError> {
    *total = total
        .checked_add(length)
        .filter(|&value| value <= MAX_TEXT_BYTES)
        .ok_or(zemer_core::AudioError::InvalidData(
            "DSDIFF marker text exceeds its aggregate limit",
        ))?;
    Ok(())
}

pub(crate) fn validate_markers(
    markers: &[DsdiffMarker],
    sample_rate: u32,
    channel_ids: &[[u8; 4]],
) -> Result<(), zemer_core::AudioError> {
    let mut total_text = 0;
    for marker in markers {
        add_marker_text_length(&mut total_text, marker.text.len())?;
        if marker.hours > 23
            || marker.minutes > 59
            || marker.seconds > 59
            || marker.samples >= sample_rate
            || usize::from(marker.channel) > channel_ids.len()
            || marker.track_flags & !0x070f != 0
            || (marker.kind != 0 || marker.channel != 0) && marker.track_flags != 0
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
                    flags & 0x0703 == 0 && flags & 0x000c != 0x000c
                }
                [left, right, center, lfe, ls, rs]
                    if left == b"MLFT"
                        && right == b"MRGT"
                        && center == b"C   "
                        && lfe == b"LFE "
                        && ls == b"LS  "
                        && rs == b"RS  " =>
                {
                    flags & 0x0702 == 0
                        && flags & 0x000d != 0x000d
                        && (flags & 0x0001 != 0 || flags & 0x000c != 0x000c)
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

#[cfg(test)]
mod tests {
    use super::{MAX_TEXT_BYTES, add_marker_text_length};

    #[test]
    fn marker_text_has_an_aggregate_allocation_limit() {
        let mut total = 0;
        add_marker_text_length(&mut total, MAX_TEXT_BYTES - 1).unwrap();
        assert!(add_marker_text_length(&mut total, 2).is_err());
        assert_eq!(total, MAX_TEXT_BYTES - 1);
    }
}
