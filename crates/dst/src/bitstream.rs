use zemer_core::AudioError;

pub(crate) struct BitReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len() * 8 - self.position
    }

    pub(crate) fn read(&mut self, count: usize) -> Result<u32, AudioError> {
        let end = self
            .position
            .checked_add(count)
            .filter(|&end| end <= self.bytes.len().saturating_mul(8))
            .ok_or(AudioError::InvalidFormat("truncated DST frame"))?;
        let mut value = 0;
        while self.position < end {
            let byte = self.bytes[self.position / 8];
            let shift = 7 - self.position % 8;
            value = (value << 1) | u32::from((byte >> shift) & 1);
            self.position += 1;
        }
        Ok(value)
    }

    pub(crate) fn read_signed(&mut self, count: usize) -> Result<i32, AudioError> {
        let value = self.read(count)? as i32;
        let sign = 1_i32 << (count - 1);
        Ok((value ^ sign) - sign)
    }

    pub(crate) fn read_arithmetic_bit(&mut self) -> u16 {
        if self.remaining() == 0 {
            0
        } else {
            let byte = self.bytes[self.position / 8];
            let shift = 7 - self.position % 8;
            self.position += 1;
            u16::from((byte >> shift) & 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BitReader;

    #[test]
    fn reads_signed_fields_in_twos_complement() {
        let mut bits = BitReader::new(&[0b1110_0000]);
        assert_eq!(bits.read_signed(3).unwrap(), -1);
        assert_eq!(bits.read_signed(2).unwrap(), 0);
    }
}
