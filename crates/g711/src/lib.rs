#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum G711Encoding {
    ALaw,
    MuLaw,
}

pub fn decode_alaw(byte: u8) -> i16 {
    let value = byte ^ 0x55;
    let segment = i32::from((value & 0x70) >> 4);
    let mut sample = i32::from(value & 0x0f) << 4;
    sample = match segment {
        0 => sample + 8,
        1 => sample + 0x108,
        _ => (sample + 0x108) << (segment - 1),
    };
    if value & 0x80 != 0 {
        sample as i16
    } else {
        -(sample as i16)
    }
}

pub fn decode_mulaw(byte: u8) -> i16 {
    let value = !byte;
    let sample = ((i32::from(value & 0x0f) << 3) + 0x84) << ((value & 0x70) >> 4);
    if value & 0x80 != 0 {
        (0x84 - sample) as i16
    } else {
        (sample - 0x84) as i16
    }
}

pub fn encode_alaw(sample: i16) -> u8 {
    const SEGMENT_ENDS: [i32; 8] = [0xff, 0x1ff, 0x3ff, 0x7ff, 0xfff, 0x1fff, 0x3fff, 0x7fff];
    let sample = i32::from(sample);
    let (magnitude, mask) = if sample >= 0 {
        (sample, 0xd5)
    } else {
        (-sample - 1, 0x55)
    };
    let magnitude = magnitude.min(0x7fff);
    let segment = SEGMENT_ENDS
        .iter()
        .position(|&end| magnitude <= end)
        .unwrap_or(8);
    let value = if segment >= 8 {
        0x7f
    } else if segment == 0 {
        (magnitude >> 4) as u8
    } else {
        (((segment as i32) << 4) | ((magnitude >> (segment + 3)) & 0x0f)) as u8
    };
    value ^ mask
}

pub fn encode_mulaw(sample: i16) -> u8 {
    const SEGMENT_ENDS: [i32; 8] = [0xff, 0x1ff, 0x3ff, 0x7ff, 0xfff, 0x1fff, 0x3fff, 0x7fff];
    let sample = i32::from(sample);
    let (magnitude, mask) = if sample >= 0 {
        (sample, 0xff)
    } else {
        (-sample, 0x7f)
    };
    let magnitude = (magnitude.min(32_635) + 0x84).min(32_767);
    let segment = SEGMENT_ENDS
        .iter()
        .position(|&end| magnitude <= end)
        .unwrap_or(8);
    if segment >= 8 {
        return 0x7f ^ mask;
    }
    let value = ((segment as i32) << 4) | ((magnitude >> (segment + 3)) & 0x0f);
    (value as u8) ^ mask
}

#[cfg(test)]
mod tests {
    use super::{decode_alaw, decode_mulaw, encode_alaw, encode_mulaw};

    #[test]
    fn matches_g711_reference_vectors() {
        assert_eq!(decode_alaw(0xd5), 8);
        assert_eq!(decode_alaw(0x55), -8);
        assert_eq!(decode_mulaw(0xff), 0);
        assert_eq!(decode_mulaw(0x00), -32_124);
        assert_eq!(decode_mulaw(0x80), 32_124);
        let samples = [i16::MIN, -1, 0, 1, i16::MAX];
        assert_eq!(samples.map(encode_alaw), [0x2a, 0x55, 0xd5, 0xd5, 0xaa]);
        assert_eq!(samples.map(encode_mulaw), [0x00, 0x7f, 0xff, 0xff, 0x80]);
    }
}
