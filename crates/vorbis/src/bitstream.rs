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

pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    bit_offset: usize,
}

impl BitWriter {
    pub(crate) const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            bit_offset: 0,
        }
    }

    pub(crate) fn write_bit(&mut self, value: bool) -> Result<(), AudioError> {
        self.write_bits(u64::from(value), 1)
    }

    pub(crate) fn write_bits(&mut self, value: u64, count: usize) -> Result<(), AudioError> {
        if count > u64::BITS as usize {
            return Err(AudioError::InvalidData("Vorbis bit write exceeds 64 bits"));
        }
        let end_bit = self
            .bit_offset
            .checked_add(count)
            .ok_or(AudioError::InvalidData("Vorbis bitstream length overflow"))?;
        let byte_count = end_bit.div_ceil(8);
        if byte_count > self.bytes.len() {
            self.bytes
                .try_reserve(byte_count - self.bytes.len())
                .map_err(|_| AudioError::InvalidData("Vorbis bitstream allocation failed"))?;
            self.bytes.resize(byte_count, 0);
        }
        for bit in 0..count {
            if value & (1_u64 << bit) != 0 {
                let position = self.bit_offset + bit;
                self.bytes[position / 8] |= 1 << (position % 8);
            }
        }
        self.bit_offset = end_bit;
        Ok(())
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use zemer_core::AudioError;

    use super::{BitReader, BitWriter};

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

    #[test]
    fn bit_writer_round_trips_lsb_first_fields_across_bytes() {
        let mut writer = BitWriter::new();
        writer.write_bits(0b1011, 4).unwrap();
        writer.write_bit(true).unwrap();
        writer.write_bits(0x1234_5678, 32).unwrap();
        writer.write_bits(0, 0).unwrap();

        let bytes = writer.into_bytes();
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_bits(4).unwrap(), 0b1011);
        assert!(reader.read_bit().unwrap());
        assert_eq!(reader.read_bits(32).unwrap(), 0x1234_5678);
        assert_eq!(reader.remaining_bits(), 3);
        assert_eq!(reader.read_bits(3).unwrap(), 0);
    }

    #[test]
    fn bit_writer_rejects_fields_wider_than_a_u64() {
        let mut writer = BitWriter::new();
        assert!(matches!(
            writer.write_bits(0, 65),
            Err(AudioError::InvalidData(_))
        ));
        assert!(writer.into_bytes().is_empty());
    }
}
