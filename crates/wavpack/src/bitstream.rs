use zemer_core::AudioError;

pub(crate) struct BitReader<'a> {
    bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_offset: 0,
        }
    }

    pub(crate) fn read_bit(&mut self) -> Result<bool, AudioError> {
        let byte = self
            .bytes
            .get(self.bit_offset / 8)
            .ok_or(AudioError::InvalidData("truncated WavPack bitstream"))?;
        let bit = *byte & (1 << (self.bit_offset % 8)) != 0;
        self.bit_offset += 1;
        Ok(bit)
    }

    pub(crate) fn read_bits(&mut self, count: u32) -> Result<u32, AudioError> {
        if count > u32::BITS {
            return Err(AudioError::InvalidData("WavPack bit read exceeds 32 bits"));
        }
        let mut bits = 0;
        for bit in 0..count {
            bits |= u32::from(self.read_bit()?) << bit;
        }
        Ok(bits)
    }

    pub(crate) fn read_code(&mut self, max_code: u32) -> Result<u32, AudioError> {
        if max_code < 2 {
            return if max_code == 0 {
                Ok(0)
            } else {
                Ok(u32::from(self.read_bit()?))
            };
        }

        let bit_count = u32::BITS - max_code.leading_zeros();
        let extras = (1_u64 << bit_count) - u64::from(max_code) - 1;
        let prefix = u64::from(self.read_bits(bit_count - 1)?);
        if prefix < extras {
            Ok(prefix as u32)
        } else {
            Ok(((prefix << 1) - extras + u64::from(self.read_bit()?)) as u32)
        }
    }

    pub(crate) fn read_zero_run(&mut self) -> Result<Option<u32>, AudioError> {
        let mut prefix_ones = 0;
        loop {
            if prefix_ones == 33 {
                return Ok(None);
            }
            if !self.read_bit()? {
                break;
            }
            prefix_ones += 1;
        }

        if prefix_ones < 2 {
            return Ok(Some(prefix_ones));
        }

        let suffix = self.read_bits(prefix_ones - 1)?;
        Ok(Some((1 << (prefix_ones - 1)) | suffix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lsb_first_zero_run_lengths_from_wavpack_silence_streams() {
        for (bytes, samples) in [
            (&[0xff, 0x00][..], 128),
            (&[0xff, 0x01, 0xfc, 0xff][..], 256),
            (&[0xff, 0x03, 0xf0, 0xff][..], 512),
            (&[0xff, 0x07, 0xc0, 0xff][..], 1024),
            (&[0xff, 0x0f, 0x00, 0xff][..], 2048),
        ] {
            assert_eq!(
                BitReader::new(bytes).read_zero_run().unwrap(),
                Some(samples)
            );
        }
    }

    #[test]
    fn distinguishes_zero_and_one_prefixes_and_end_marker() {
        assert_eq!(BitReader::new(&[0]).read_zero_run().unwrap(), Some(0));
        assert_eq!(BitReader::new(&[1]).read_zero_run().unwrap(), Some(1));
        assert_eq!(BitReader::new(&[0xff; 5]).read_zero_run().unwrap(), None);
    }

    #[test]
    fn rejects_truncated_unary_and_suffix_bits() {
        assert!(matches!(
            BitReader::new(&[0xff]).read_zero_run(),
            Err(AudioError::InvalidData(_))
        ));
        assert!(matches!(
            BitReader::new(&[0xff, 0x01]).read_zero_run(),
            Err(AudioError::InvalidData(_))
        ));
        assert!(BitReader::new(&[0; 8]).read_bits(33).is_err());
    }

    #[test]
    fn adjusted_binary_codes_cover_non_power_of_two_ranges() {
        // LSB-first encodings for values 0..=5 with max_code = 5.
        let values = [0, 1, 2, 3, 4, 5];
        let mut bits = Vec::new();
        for value in values {
            let code = match value {
                0 => vec![false, false],
                1 => vec![true, false],
                2 => vec![false, true, false],
                3 => vec![false, true, true],
                4 => vec![true, true, false],
                5 => vec![true, true, true],
                _ => unreachable!(),
            };
            bits.extend(code);
        }
        let bytes: Vec<u8> = bits
            .chunks(8)
            .map(|chunk| {
                chunk
                    .iter()
                    .enumerate()
                    .fold(0, |byte, (index, bit)| byte | (u8::from(*bit) << index))
            })
            .collect();
        let mut reader = BitReader::new(&bytes);
        for expected in values {
            assert_eq!(reader.read_code(5).unwrap(), expected);
        }
    }
}
