use std::io::Write;

use zemer_core::AudioError;

use crate::AiffMarker;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AiffLoopMode {
    #[default]
    None,
    Forward,
    ForwardBackward,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AiffLoop {
    pub mode: AiffLoopMode,
    pub begin_marker: u16,
    pub end_marker: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiffInstrument {
    pub base_note: u8,
    pub detune: i8,
    pub low_note: u8,
    pub high_note: u8,
    pub low_velocity: u8,
    pub high_velocity: u8,
    pub gain: i16,
    pub sustain_loop: AiffLoop,
    pub release_loop: AiffLoop,
}

impl Default for AiffInstrument {
    fn default() -> Self {
        Self {
            base_note: 60,
            detune: 0,
            low_note: 0,
            high_note: 127,
            low_velocity: 1,
            high_velocity: 127,
            gain: 0,
            sustain_loop: AiffLoop::default(),
            release_loop: AiffLoop::default(),
        }
    }
}

pub(super) fn parse_instrument(bytes: &[u8]) -> Result<AiffInstrument, AudioError> {
    if bytes.len() != 20 {
        return Err(AudioError::InvalidFormat(
            "AIFF INST chunk must be 20 bytes",
        ));
    }
    let instrument = AiffInstrument {
        base_note: bytes[0],
        detune: bytes[1] as i8,
        low_note: bytes[2],
        high_note: bytes[3],
        low_velocity: bytes[4],
        high_velocity: bytes[5],
        gain: i16::from_be_bytes(bytes[6..8].try_into().unwrap()),
        sustain_loop: parse_loop(&bytes[8..14])?,
        release_loop: parse_loop(&bytes[14..20])?,
    };
    validate_instrument(&instrument)
        .map_err(|_| AudioError::InvalidFormat("invalid AIFF INST values"))?;
    Ok(instrument)
}

fn parse_loop(bytes: &[u8]) -> Result<AiffLoop, AudioError> {
    let mode = match u16::from_be_bytes(bytes[..2].try_into().unwrap()) {
        0 => AiffLoopMode::None,
        1 => AiffLoopMode::Forward,
        2 => AiffLoopMode::ForwardBackward,
        _ => return Err(AudioError::InvalidFormat("invalid AIFF INST loop mode")),
    };
    Ok(AiffLoop {
        mode,
        begin_marker: u16::from_be_bytes(bytes[2..4].try_into().unwrap()),
        end_marker: u16::from_be_bytes(bytes[4..6].try_into().unwrap()),
    })
}

pub(super) fn validate_instrument(instrument: &AiffInstrument) -> Result<(), AudioError> {
    if instrument.base_note > 127
        || !(-50..=50).contains(&instrument.detune)
        || instrument.low_note > instrument.high_note
        || instrument.high_note > 127
        || !(1..=127).contains(&instrument.low_velocity)
        || instrument.low_velocity > instrument.high_velocity
        || instrument.high_velocity > 127
    {
        return Err(AudioError::InvalidData(
            "invalid AIFF instrument parameters",
        ));
    }
    Ok(())
}

pub(super) fn validate_instrument_markers(
    instrument: &AiffInstrument,
    markers: &[AiffMarker],
) -> Result<(), AudioError> {
    for sample_loop in [&instrument.sustain_loop, &instrument.release_loop] {
        if sample_loop.mode == AiffLoopMode::None {
            continue;
        }
        let marker_position = |marker_id| {
            markers
                .iter()
                .find(|marker| u16::try_from(marker.id).ok() == Some(marker_id))
                .map(|marker| marker.position)
        };
        let begin = marker_position(sample_loop.begin_marker).ok_or(AudioError::InvalidData(
            "AIFF instrument loop references a missing marker",
        ))?;
        let end = marker_position(sample_loop.end_marker).ok_or(AudioError::InvalidData(
            "AIFF instrument loop references a missing marker",
        ))?;
        if begin >= end {
            return Err(AudioError::InvalidData(
                "AIFF instrument loop begin marker must precede end marker",
            ));
        }
    }
    Ok(())
}

pub(super) fn write_instrument_chunk(
    writer: &mut impl Write,
    instrument: &AiffInstrument,
) -> Result<(), AudioError> {
    validate_instrument(instrument)?;
    writer.write_all(b"INST\0\0\0\x14")?;
    writer.write_all(&[
        instrument.base_note,
        instrument.detune as u8,
        instrument.low_note,
        instrument.high_note,
        instrument.low_velocity,
        instrument.high_velocity,
    ])?;
    writer.write_all(&instrument.gain.to_be_bytes())?;
    write_loop(writer, instrument.sustain_loop)?;
    write_loop(writer, instrument.release_loop)?;
    Ok(())
}

fn write_loop(writer: &mut impl Write, sample_loop: AiffLoop) -> Result<(), AudioError> {
    let mode: u16 = match sample_loop.mode {
        AiffLoopMode::None => 0,
        AiffLoopMode::Forward => 1,
        AiffLoopMode::ForwardBackward => 2,
    };
    writer.write_all(&mode.to_be_bytes())?;
    writer.write_all(&sample_loop.begin_marker.to_be_bytes())?;
    writer.write_all(&sample_loop.end_marker.to_be_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AiffInstrument, AiffLoop, AiffLoopMode, parse_instrument, write_instrument_chunk};

    #[test]
    fn instrument_chunk_round_trips_signed_detune_and_both_loop_modes() {
        let instrument = AiffInstrument {
            detune: -17,
            sustain_loop: AiffLoop {
                mode: AiffLoopMode::Forward,
                begin_marker: 3,
                end_marker: 7,
            },
            release_loop: AiffLoop {
                mode: AiffLoopMode::ForwardBackward,
                begin_marker: 8,
                end_marker: 9,
            },
            ..AiffInstrument::default()
        };
        let mut bytes = Vec::new();
        write_instrument_chunk(&mut bytes, &instrument).unwrap();
        assert_eq!(&bytes[..8], b"INST\0\0\0\x14");
        assert_eq!(parse_instrument(&bytes[8..]).unwrap(), instrument);
    }

    #[test]
    fn rejects_invalid_modes_ranges_and_lengths() {
        let mut bytes = vec![0; 20];
        bytes[8..10].copy_from_slice(&3_u16.to_be_bytes());
        assert!(parse_instrument(&bytes).is_err());
        bytes[8..10].copy_from_slice(&0_u16.to_be_bytes());
        bytes[0] = 128;
        assert!(parse_instrument(&bytes).is_err());
        assert!(parse_instrument(&bytes[..19]).is_err());
    }

    #[test]
    fn active_loop_markers_must_be_in_playback_order() {
        use crate::AiffMarker;

        let instrument = AiffInstrument {
            sustain_loop: AiffLoop {
                mode: AiffLoopMode::Forward,
                begin_marker: 1,
                end_marker: 2,
            },
            ..AiffInstrument::default()
        };
        let markers = [
            AiffMarker {
                id: 1,
                position: 3,
                name: Vec::new(),
            },
            AiffMarker {
                id: 2,
                position: 1,
                name: Vec::new(),
            },
        ];
        assert!(super::validate_instrument_markers(&instrument, &markers).is_err());
    }
}
