use std::io::Read;

use zemer_core::AudioError;

pub(crate) struct BitReader<R> {
    inner: R,
    byte: u8,
    remaining: u8,
    crc8: u8,
    crc16: u16,
    check_header: bool,
    check_frame: bool,
    frame_bytes: usize,
    frame_limit: usize,
}

impl<R: Read> BitReader<R> {
    pub(crate) fn new(inner: R, frame_limit: usize) -> Self {
        Self {
            inner,
            byte: 0,
            remaining: 0,
            crc8: 0,
            crc16: 0,
            check_header: false,
            check_frame: false,
            frame_bytes: 0,
            frame_limit,
        }
    }

    pub(crate) fn begin_frame(&mut self) -> Result<(), AudioError> {
        if self.remaining != 0 {
            return Err(AudioError::InvalidData("FLAC frame is not byte-aligned"));
        }
        self.crc8 = 0;
        self.crc16 = 0;
        self.frame_bytes = 0;
        self.check_header = true;
        self.check_frame = true;
        Ok(())
    }

    pub(crate) fn read_bits_or_eof(&mut self, count: u8) -> Result<Option<u64>, AudioError> {
        if count == 0 || count > 64 {
            return Err(AudioError::InvalidData("invalid FLAC bit count"));
        }
        let mut result = 0_u64;
        for read in 0..count {
            match self.read_bit()? {
                Some(bit) => result = (result << 1) | u64::from(bit),
                None if read == 0 => return Ok(None),
                None => return Err(AudioError::InvalidData("truncated FLAC frame")),
            }
        }
        Ok(Some(result))
    }

    pub(crate) fn read_bits(&mut self, count: u8) -> Result<u64, AudioError> {
        self.read_bits_or_eof(count)?
            .ok_or(AudioError::InvalidData("truncated FLAC frame"))
    }

    pub(crate) fn read_signed(&mut self, count: u8) -> Result<i64, AudioError> {
        if count == 0 || count > 63 {
            return Err(AudioError::InvalidData("invalid FLAC signed integer width"));
        }
        let value = self.read_bits(count)?;
        let sign = 1_u64 << (count - 1);
        Ok(if value & sign == 0 {
            value as i64
        } else {
            (value as i64) - (1_i64 << count)
        })
    }

    pub(crate) fn read_unary(&mut self, maximum: u64) -> Result<u64, AudioError> {
        let mut zeroes = 0_u64;
        loop {
            if self.remaining == 0 {
                let mut byte = [0_u8; 1];
                match self.inner.read_exact(&mut byte) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                        return Err(AudioError::InvalidData("truncated FLAC unary code"));
                    }
                    Err(error) => return Err(AudioError::Io(error)),
                }
                self.byte = byte[0];
                self.remaining = 8;
                if self.check_header {
                    self.crc8 = crc8_update(self.crc8, self.byte);
                }
                if self.check_frame {
                    if self.frame_bytes >= self.frame_limit {
                        return Err(AudioError::InvalidData(
                            "FLAC frame exceeds the configured size limit",
                        ));
                    }
                    self.crc16 = crc16_update(self.crc16, self.byte);
                    self.frame_bytes = self.frame_bytes.saturating_add(1);
                }
            }
            let unread = self.byte & (u8::MAX >> (8 - self.remaining));
            if unread == 0 {
                zeroes += u64::from(self.remaining);
                self.remaining = 0;
            } else {
                let in_byte = self.remaining - (8 - unread.leading_zeros() as u8);
                zeroes += u64::from(in_byte);
                self.remaining -= in_byte + 1;
                if zeroes > maximum {
                    return Err(AudioError::InvalidData(
                        "FLAC Rice code exceeds sample bounds",
                    ));
                }
                return Ok(zeroes);
            }
            if zeroes > maximum {
                return Err(AudioError::InvalidData(
                    "FLAC Rice code exceeds sample bounds",
                ));
            }
        }
    }

    pub(crate) fn align_zero(&mut self) -> Result<(), AudioError> {
        if self.remaining == 0 {
            return Ok(());
        }
        let padding = self.remaining;
        if self.read_bits(padding)? != 0 {
            return Err(AudioError::InvalidData("non-zero FLAC frame padding"));
        }
        Ok(())
    }

    pub(crate) fn header_crc8(&self) -> u8 {
        self.crc8
    }

    pub(crate) fn frame_crc16(&self) -> u16 {
        self.crc16
    }

    pub(crate) fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    pub(crate) fn stop_header_crc(&mut self) {
        self.check_header = false;
    }

    pub(crate) fn stop_frame_crc(&mut self) {
        self.check_header = false;
        self.check_frame = false;
    }

    fn read_bit(&mut self) -> Result<Option<u8>, AudioError> {
        if self.remaining == 0 {
            let mut byte = [0_u8; 1];
            match self.inner.read_exact(&mut byte) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(error) => return Err(AudioError::Io(error)),
            }
            self.byte = byte[0];
            self.remaining = 8;
            if self.check_header {
                self.crc8 = crc8_update(self.crc8, self.byte);
            }
            if self.check_frame {
                if self.frame_bytes >= self.frame_limit {
                    return Err(AudioError::InvalidData(
                        "FLAC frame exceeds the configured size limit",
                    ));
                }
                self.crc16 = crc16_update(self.crc16, self.byte);
                self.frame_bytes = self.frame_bytes.saturating_add(1);
            }
        }
        self.remaining -= 1;
        Ok(Some((self.byte >> self.remaining) & 1))
    }
}

fn crc8_update(mut crc: u8, byte: u8) -> u8 {
    crc ^= byte;
    for _ in 0..8 {
        crc = if crc & 0x80 == 0 {
            crc << 1
        } else {
            (crc << 1) ^ 0x07
        };
    }
    crc
}

fn crc16_update(mut crc: u16, byte: u8) -> u16 {
    crc ^= u16::from(byte) << 8;
    for _ in 0..8 {
        crc = if crc & 0x8000 == 0 {
            crc << 1
        } else {
            (crc << 1) ^ 0x8005
        };
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_variants_match_the_flac_check_values() {
        let (crc8, crc16) = b"123456789".iter().fold((0, 0), |(a, b), byte| {
            (crc8_update(a, *byte), crc16_update(b, *byte))
        });
        assert_eq!(crc8, 0xf4);
        assert_eq!(crc16, 0xfee8);
    }

    #[test]
    fn unary_reads_only_bits_not_yet_consumed_from_the_current_byte() {
        let mut reader = BitReader::new(&[0b1110_0010][..], 8);
        assert_eq!(reader.read_bits(3).unwrap(), 0b111);
        assert_eq!(reader.read_unary(4).unwrap(), 3);
    }
}
