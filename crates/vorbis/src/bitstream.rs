use zemer_core::AudioError;

pub(crate) struct BitReader<'a> {
    bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_offset: 0,
        }
    }

    pub(crate) fn read_bit(&mut self) -> Result<bool, AudioError> {
        let byte = self
            .bytes
            .get(self.bit_offset / 8)
            .ok_or(AudioError::InvalidFormat("truncated Vorbis packet"))?;
        let bit = *byte & (1 << (self.bit_offset % 8)) != 0;
        self.bit_offset += 1;
        Ok(bit)
    }

    pub(crate) fn read_bits(&mut self, count: u32) -> Result<u64, AudioError> {
        if count > u64::BITS {
            return Err(AudioError::InvalidData("Vorbis bit read exceeds 64 bits"));
        }
        let mut bits = 0;
        for bit in 0..count {
            bits |= u64::from(self.read_bit()?) << bit;
        }
        Ok(bits)
    }

    pub(crate) fn remaining_bits(&self) -> usize {
        self.bytes.len() * 8 - self.bit_offset
    }
}

#[cfg(test)]
mod tests {
    use zemer_core::AudioError;

    use super::BitReader;

    #[test]
    fn reads_packet_bits_least_significant_bit_first() {
        let mut reader = BitReader::new(&[0b1011_0100, 0b0000_0011]);
        assert_eq!(reader.read_bits(4).unwrap(), 0b0100);
        assert_eq!(reader.read_bits(6).unwrap(), 0b11_1011);
    }

    #[test]
    fn allows_zero_width_reads_and_rejects_truncation_or_oversized_reads() {
        let mut reader = BitReader::new(&[0xff]);
        assert_eq!(reader.read_bits(0).unwrap(), 0);
        assert!(matches!(
            reader.read_bits(65),
            Err(AudioError::InvalidData(_))
        ));
        assert!(matches!(
            reader.read_bits(9),
            Err(AudioError::InvalidFormat(_))
        ));
    }
}
