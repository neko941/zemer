//! Clean-room DWVW v1.2 audio codec.
//!
//! DWVW was invented by Magnus Lidstrom and copyrighted in 1993 by NuEdge
//! Development. This implementation follows the published description by
//! Dave Huizing, *Audio File Format Resource Guide*, section 4.1:
//! <https://mirrors.apple2.org.za/ground.icaen.uiowa.edu/Faqs/R011SNDFMTS.htm>.

use std::io::{Read, Seek, SeekFrom, Write};

#[derive(Debug)]
pub enum DwvwError {
    InvalidBitDepth,
    InvalidSample,
    Truncated,
    InvalidState,
    Io(std::io::Error),
}

impl From<std::io::Error> for DwvwError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecoderState {
    bit_position: u64,
    previous_sample: i64,
    previous_width: u8,
}

impl DecoderState {
    pub fn at(bit_position: u64) -> Self {
        Self {
            bit_position,
            previous_sample: 0,
            previous_width: 0,
        }
    }

    pub fn bit_position(&self) -> u64 {
        self.bit_position
    }

    pub fn finish_channel(&mut self, end_bit: u64) -> Result<(), DwvwError> {
        self.bit_position = self
            .bit_position
            .checked_add(15)
            .ok_or(DwvwError::InvalidState)?
            & !15;
        if self.bit_position > end_bit {
            return Err(DwvwError::Truncated);
        }
        self.previous_sample = 0;
        self.previous_width = 0;
        Ok(())
    }
}

pub fn decode_samples<R: Read + Seek>(
    reader: &mut R,
    state: &mut DecoderState,
    bit_depth: u8,
    end_bit: u64,
    output: &mut [i32],
) -> Result<(), DwvwError> {
    validate_depth(bit_depth)?;
    let mut bits = BitReader::new(reader, state.bit_position, end_bit)?;
    for sample in output {
        *sample = decode_one(&mut bits, state, bit_depth)?;
    }
    state.bit_position = bits.position();
    Ok(())
}

pub fn scan_samples<R: Read + Seek>(
    reader: &mut R,
    state: &mut DecoderState,
    bit_depth: u8,
    end_bit: u64,
    sample_count: u64,
) -> Result<(), DwvwError> {
    validate_depth(bit_depth)?;
    let mut bits = BitReader::new(reader, state.bit_position, end_bit)?;
    for _ in 0..sample_count {
        decode_one(&mut bits, state, bit_depth)?;
    }
    state.bit_position = bits.position();
    Ok(())
}

fn decode_one<R: Read + Seek>(
    bits: &mut BitReader<'_, R>,
    state: &mut DecoderState,
    bit_depth: u8,
) -> Result<i32, DwvwError> {
    let half = 1_i64 << (bit_depth - 1);
    let modulus = 1_i64 << bit_depth;
    let maximum_modifier = bit_depth / 2;
    let modifier_magnitude = bits.read_unary(maximum_modifier)?;
    let modifier = if modifier_magnitude == 0 {
        0
    } else {
        let negative = bits.read_bit()?;
        if negative {
            -i64::from(modifier_magnitude)
        } else {
            i64::from(modifier_magnitude)
        }
    };
    let width = (i64::from(state.previous_width) + modifier).rem_euclid(i64::from(bit_depth)) as u8;
    let delta = if width == 0 {
        0
    } else {
        let magnitude = (1_i64 << (width - 1)) | bits.read_bits(width - 1)? as i64;
        let negative = bits.read_bit()?;
        if negative {
            if magnitude == half - 1 && bits.read_bit()? {
                -half
            } else {
                -magnitude
            }
        } else {
            magnitude
        }
    };
    let value = (state.previous_sample + delta).rem_euclid(modulus);
    let signed = if value >= half {
        value - modulus
    } else {
        value
    };
    state.previous_sample = signed;
    state.previous_width = width;
    i32::try_from(signed).map_err(|_| DwvwError::InvalidState)
}

pub struct ChannelEncoder<W> {
    writer: BitWriter<W>,
    bit_depth: u8,
    previous_sample: i64,
    previous_width: u8,
}

impl<W: Write> ChannelEncoder<W> {
    pub fn new(writer: W, bit_depth: u8) -> Result<Self, DwvwError> {
        validate_depth(bit_depth)?;
        Ok(Self {
            writer: BitWriter::new(writer),
            bit_depth,
            previous_sample: 0,
            previous_width: 0,
        })
    }

    pub fn write_samples(&mut self, samples: &[i32]) -> Result<(), DwvwError> {
        let bits = self.bit_depth;
        let half = 1_i64 << (bits - 1);
        let modulus = 1_i64 << bits;
        let maximum_modifier = bits / 2;
        for &sample in samples {
            let current = i64::from(sample);
            if !(-half..half).contains(&current) {
                return Err(DwvwError::InvalidSample);
            }
            let mut delta = current - self.previous_sample;
            if delta >= half {
                delta -= modulus;
            } else if delta < -half {
                delta += modulus;
            }
            let negative = delta < 0;
            let magnitude = delta.unsigned_abs();
            let encoded_magnitude = if negative && magnitude == half as u64 {
                magnitude - 1
            } else {
                magnitude
            };
            let width = if encoded_magnitude == 0 {
                0
            } else {
                (u64::BITS - encoded_magnitude.leading_zeros()) as u8
            };
            let mut modifier = i16::from(width) - i16::from(self.previous_width);
            if modifier > i16::from(maximum_modifier) {
                modifier -= i16::from(bits);
            } else if modifier < -i16::from(maximum_modifier) {
                modifier += i16::from(bits);
            }
            let modifier_magnitude = modifier.unsigned_abs() as u8;
            for _ in 0..modifier_magnitude {
                self.writer.write_bit(false)?;
            }
            if modifier_magnitude < maximum_modifier {
                self.writer.write_bit(true)?;
            }
            if modifier_magnitude != 0 {
                self.writer.write_bit(modifier < 0)?;
            }
            for shift in (0..width.saturating_sub(1)).rev() {
                self.writer
                    .write_bit((encoded_magnitude & (1_u64 << shift)) != 0)?;
            }
            if encoded_magnitude != 0 {
                self.writer.write_bit(negative)?;
                if negative && encoded_magnitude == half as u64 - 1 {
                    self.writer.write_bit(magnitude == half as u64)?;
                }
            }
            self.previous_sample = current;
            self.previous_width = width;
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<W, DwvwError> {
        self.writer.align_word()?;
        self.writer.finish()
    }
}

struct BitReader<'a, R> {
    reader: &'a mut R,
    end_bit: u64,
    position: u64,
    byte: Option<u8>,
}

impl<'a, R: Read + Seek> BitReader<'a, R> {
    fn new(reader: &'a mut R, position: u64, end_bit: u64) -> Result<Self, DwvwError> {
        if position > end_bit {
            return Err(DwvwError::Truncated);
        }
        reader.seek(SeekFrom::Start(position / 8))?;
        Ok(Self {
            reader,
            end_bit,
            position,
            byte: None,
        })
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn read_bit(&mut self) -> Result<bool, DwvwError> {
        if self.position >= self.end_bit {
            return Err(DwvwError::Truncated);
        }
        let bit_in_byte = (self.position & 7) as u8;
        if bit_in_byte == 0 || self.byte.is_none() {
            let mut byte = [0];
            self.reader.read_exact(&mut byte)?;
            self.byte = Some(byte[0]);
        }
        let value = self.byte.ok_or(DwvwError::InvalidState)? & (0x80 >> bit_in_byte) != 0;
        self.position += 1;
        Ok(value)
    }

    fn read_bits(&mut self, count: u8) -> Result<u32, DwvwError> {
        let mut value = 0_u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Ok(value)
    }

    fn read_unary(&mut self, maximum: u8) -> Result<u8, DwvwError> {
        let mut count = 0;
        while count < maximum && !self.read_bit()? {
            count += 1;
        }
        if count < maximum {
            Ok(count)
        } else {
            Ok(maximum)
        }
    }
}

struct BitWriter<W> {
    writer: W,
    byte: u8,
    used: u8,
    position: u64,
}

impl<W: Write> BitWriter<W> {
    fn new(writer: W) -> Self {
        Self {
            writer,
            byte: 0,
            used: 0,
            position: 0,
        }
    }

    fn write_bit(&mut self, value: bool) -> Result<(), DwvwError> {
        if value {
            self.byte |= 0x80 >> self.used;
        }
        self.used += 1;
        self.position += 1;
        if self.used == 8 {
            self.writer.write_all(&[self.byte])?;
            self.byte = 0;
            self.used = 0;
        }
        Ok(())
    }

    fn align_word(&mut self) -> Result<(), DwvwError> {
        while self.position & 15 != 0 {
            self.write_bit(false)?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<W, DwvwError> {
        if self.used != 0 {
            self.writer.write_all(&[self.byte])?;
            self.used = 0;
        }
        Ok(self.writer)
    }
}

fn validate_depth(bit_depth: u8) -> Result<(), DwvwError> {
    if (2..=32).contains(&bit_depth) {
        Ok(())
    } else {
        Err(DwvwError::InvalidBitDepth)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{ChannelEncoder, DecoderState, DwvwError, decode_samples};

    #[test]
    fn decodes_libsndfile_16_bit_oracle() {
        let samples = [287, 4255, 8549, 12341, 15845, 18660, 20931, 22349, 23079];
        let expected = [
            0x01, 0x8f, 0x85, 0xe0, 0x08, 0x31, 0x8f, 0x68, 0x36, 0xc1, 0x5f, 0xe8, 0xdf, 0x36,
            0x28, 0xdb, 0x40, 0x4d, 0x13, 0xd7,
        ];
        let mut state = DecoderState::at(0);
        let mut decoded = [0; 9];
        decode_samples(
            &mut Cursor::new(expected),
            &mut state,
            16,
            (expected.len() * 8) as u64,
            &mut decoded,
        )
        .unwrap();
        assert_eq!(decoded, samples);
    }

    #[test]
    fn round_trips_extreme_deltas_at_supported_widths() {
        for depth in 2..=32 {
            let min = -(1_i64 << (depth - 1));
            let max = (1_i64 << (depth - 1)) - 1;
            let samples = [0, max as i32, min as i32, -1, 0, min as i32, max as i32];
            let mut encoder = ChannelEncoder::new(Vec::new(), depth).unwrap();
            encoder.write_samples(&samples).unwrap();
            let bytes = encoder.finish().unwrap();
            let mut state = DecoderState::at(0);
            let mut decoded = [0; 7];
            decode_samples(
                &mut Cursor::new(bytes),
                &mut state,
                depth,
                128,
                &mut decoded,
            )
            .unwrap();
            assert_eq!(decoded, samples, "{depth}-bit DWVW");
        }
    }

    #[test]
    fn rejects_truncated_samples_and_invalid_depths() {
        let mut encoder = ChannelEncoder::new(Vec::new(), 8).unwrap();
        encoder.write_samples(&[1]).unwrap();
        let bytes = encoder.finish().unwrap();
        let mut state = DecoderState::at(0);
        let mut decoded = [0; 2];
        assert!(matches!(
            decode_samples(&mut Cursor::new(bytes), &mut state, 8, 1, &mut decoded),
            Err(DwvwError::Truncated)
        ));
        assert!(matches!(
            ChannelEncoder::new(Vec::new(), 1),
            Err(DwvwError::InvalidBitDepth)
        ));
    }
}
