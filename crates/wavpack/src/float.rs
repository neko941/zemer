use zemer_core::AudioError;

use crate::bitstream::BitReader;

const SHIFT_ONES: u8 = 0x01;
const SHIFT_SAME: u8 = 0x02;
const SHIFT_SENT: u8 = 0x04;
const ZEROS_SENT: u8 = 0x08;
const NEGATIVE_ZEROS: u8 = 0x10;
const EXCEPTIONS: u8 = 0x20;

#[derive(Clone, Copy)]
pub(crate) struct FloatInfo {
    flags: u8,
    shift: u8,
    max_exp: u8,
    _norm_exp: u8,
}

impl FloatInfo {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, AudioError> {
        let [flags, shift, max_exp, norm_exp] = bytes else {
            return Err(AudioError::InvalidData(
                "invalid WavPack floating-point metadata size",
            ));
        };
        Ok(Self {
            flags: *flags,
            shift: *shift,
            max_exp: *max_exp,
            _norm_exp: *norm_exp,
        })
    }
}

pub(crate) fn restore(
    values: &[i32],
    info: FloatInfo,
    wvx: Option<&[u8]>,
    new_wvx: bool,
) -> Result<Vec<f32>, AudioError> {
    let needs_wvx = info.flags & (SHIFT_SAME | SHIFT_SENT | ZEROS_SENT | EXCEPTIONS) != 0;
    let mut bits = match wvx {
        Some(bytes) if bytes.len() > 4 && bytes.len() % 2 == 0 => Some(BitReader::new(&bytes[4..])),
        Some(_) => {
            return Err(AudioError::InvalidData(
                "invalid WavPack floating-point extended bitstream",
            ));
        }
        None if needs_wvx => {
            return Err(AudioError::InvalidData(
                "missing WavPack floating-point extended bitstream",
            ));
        }
        None => None,
    };

    let (mut min_shifted_zeros, mut max_shifted_ones) = (0, 0);
    if new_wvx {
        let bits = bits.as_mut().ok_or(AudioError::InvalidData(
            "missing WavPack floating-point extended bitstream",
        ))?;
        min_shifted_zeros = bits.read_bits(5)? as u8;
        max_shifted_ones = bits.read_bits(5)? as u8;
    }

    let mut output = Vec::new();
    output
        .try_reserve_exact(values.len())
        .map_err(|_| AudioError::InvalidData("decoded WavPack float block is too large"))?;

    for &word in values {
        let mut mantissa = 0_u32;
        let mut exponent = info.max_exp;
        let mut sign = false;

        if word == 0 {
            exponent = 0;
            if info.flags & ZEROS_SENT != 0 {
                if read_bit(&mut bits)? {
                    mantissa = read_bits(&mut bits, 23)?;
                    if info.max_exp >= 25 {
                        exponent = read_bits(&mut bits, 8)? as u8;
                    }
                    sign = read_bit(&mut bits)?;
                } else if info.flags & NEGATIVE_ZEROS != 0 {
                    sign = read_bit(&mut bits)?;
                }
            }
        } else {
            let mut magnitude = (word as u32).wrapping_shl(u32::from(info.shift & 0x1f));
            if word < 0 {
                magnitude = magnitude.wrapping_neg();
                sign = true;
            }

            if bits.is_none() && magnitude >= 0x0100_0000 {
                while magnitude & 0x0f00_0000 != 0 {
                    magnitude >>= 1;
                    exponent = exponent.checked_add(1).ok_or(AudioError::InvalidData(
                        "WavPack floating-point exponent overflow",
                    ))?;
                }
                mantissa = magnitude & 0x007f_ffff;
            } else if magnitude == 0x0100_0000 {
                if read_bit(&mut bits)? {
                    mantissa = read_bits(&mut bits, 23)?;
                }
                exponent = u8::MAX;
            } else {
                let mut shift_count = 0_u8;
                while exponent != 0 && magnitude & 0x0080_0000 == 0 {
                    exponent -= 1;
                    magnitude <<= 1;
                    shift_count = shift_count.wrapping_add(1);
                }

                shift_count &= 0x1f;
                if shift_count != 0 {
                    let low_mask = (1_u32 << shift_count) - 1;
                    if info.flags & SHIFT_ONES != 0
                        || (info.flags & SHIFT_SAME != 0 && read_bit(&mut bits)?)
                    {
                        magnitude |= low_mask;
                    } else if info.flags & SHIFT_SENT != 0 {
                        let mut zeros = 0_u8;
                        if max_shifted_ones != 0 && shift_count > max_shifted_ones {
                            zeros = shift_count - max_shifted_ones;
                        }
                        if min_shifted_zeros > zeros {
                            zeros = min_shifted_zeros.min(shift_count);
                        }
                        let sent = shift_count - zeros;
                        if sent != 0 {
                            magnitude |= read_bits(&mut bits, u32::from(sent))? << zeros;
                        }
                    }
                }
                mantissa = magnitude & 0x007f_ffff;
            }
        }

        let raw = (u32::from(sign) << 31) | (u32::from(exponent) << 23) | mantissa;
        output.push(f32::from_bits(raw));
    }

    Ok(output)
}

fn read_bit(bits: &mut Option<BitReader<'_>>) -> Result<bool, AudioError> {
    bits.as_mut()
        .ok_or(AudioError::InvalidData(
            "missing WavPack floating-point extended bitstream",
        ))?
        .read_bit()
}

fn read_bits(bits: &mut Option<BitReader<'_>>, count: u32) -> Result<u32, AudioError> {
    bits.as_mut()
        .ok_or(AudioError::InvalidData(
            "missing WavPack floating-point extended bitstream",
        ))?
        .read_bits(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_float_values_without_a_wvx_stream() {
        let info = FloatInfo {
            flags: SHIFT_ONES,
            shift: 0,
            max_exp: 127,
            _norm_exp: 127,
        };
        let values = [0, 0x0080_0000, 0x0100_0000];
        let decoded = restore(&values, info, None, false).unwrap();
        assert_eq!(
            decoded
                .iter()
                .map(|sample| sample.to_bits())
                .collect::<Vec<_>>(),
            [0x0000_0000, 0x3f80_0000, 0x4000_0000]
        );
    }
}
