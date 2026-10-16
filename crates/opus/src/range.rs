#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RangeError {
    InvalidFrequencyTable,
    InvalidInverseCdf,
    InvalidLogProbability,
    InvalidLaplaceModel,
    InvalidCeltEnergyLayout,
    InvalidCeltQuality,
    InvalidFineEnergyAllocation,
    InvalidPvqLayout,
    PvqCodebookTooLarge,
    InvalidPvqSpread,
    InvalidPvqBlockCount,
    InvalidTotal,
    CorruptFrame,
    RawBitsTooWide,
    RawBitsExhausted,
}

pub(crate) struct RangeDecoder<'a> {
    data: &'a [u8],
    offset: usize,
    rem: u8,
    val: u32,
    rng: u32,
    nbits_total: u32,
    raw_bits_read: usize,
}

impl<'a> RangeDecoder<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let first = data.first().copied().unwrap_or(0);
        let mut decoder = Self {
            data,
            offset: usize::from(!data.is_empty()),
            rem: first & 1,
            val: 127 - u32::from(first >> 1),
            rng: 128,
            nbits_total: 9,
            raw_bits_read: 0,
        };
        decoder.normalize();
        decoder
    }

    pub(crate) fn decode_symbol(&mut self, frequencies: &[u16]) -> Result<usize, RangeError> {
        let total = frequencies.iter().try_fold(0_u32, |sum, &frequency| {
            sum.checked_add(u32::from(frequency))
        });
        let total = total
            .filter(|&total| total > 0 && total <= u16::MAX.into())
            .ok_or(RangeError::InvalidFrequencyTable)?;
        let value = self.value(total)?;
        let mut low = 0_u32;
        for (symbol, &frequency) in frequencies.iter().enumerate() {
            let high = low + u32::from(frequency);
            if low <= value && value < high {
                self.update(low, high, total)?;
                return Ok(symbol);
            }
            low = high;
        }
        Err(RangeError::CorruptFrame)
    }

    pub(crate) fn decode_bit_logp(&mut self, logp: u8) -> Result<bool, RangeError> {
        if !(1..=15).contains(&logp) {
            return Err(RangeError::InvalidLogProbability);
        }
        Ok(self.decode_symbol(&[u16::MAX >> (16 - logp), 1])? == 1)
    }

    pub(crate) fn decode_icdf(&mut self, icdf: &[u8], ftb: u8) -> Result<usize, RangeError> {
        if !(1..=8).contains(&ftb) || icdf.is_empty() || icdf.len() > (1 << ftb) {
            return Err(RangeError::InvalidInverseCdf);
        }
        let total = 1_u32 << ftb;
        if icdf.last() != Some(&0)
            || icdf.iter().any(|&value| u32::from(value) > total)
            || icdf.windows(2).any(|pair| pair[0] < pair[1])
        {
            return Err(RangeError::InvalidInverseCdf);
        }

        let value = self.value(total)?;
        let symbol = icdf
            .iter()
            .position(|&boundary| value < total - u32::from(boundary))
            .ok_or(RangeError::CorruptFrame)?;
        let low = if symbol == 0 {
            0
        } else {
            total - u32::from(icdf[symbol - 1])
        };
        let high = total - u32::from(icdf[symbol]);
        self.update(low, high, total)?;
        Ok(symbol)
    }

    pub(crate) fn decode_uint(&mut self, total: u32) -> Result<u32, RangeError> {
        if total == 0 {
            return Err(RangeError::InvalidTotal);
        }
        let bits = (u32::BITS - (total - 1).leading_zeros()) as u8;
        let value = if bits <= 8 {
            let value = self.value(total)?;
            self.update(value, value + 1, total)?;
            value
        } else {
            let shift = bits - 8;
            let high_total = ((total - 1) >> shift) + 1;
            let high = self.value(high_total)?;
            self.update(high, high + 1, high_total)?;
            (high << shift) | self.read_raw_bits(shift)?
        };
        if value >= total {
            return Err(RangeError::CorruptFrame);
        }
        Ok(value)
    }

    pub(crate) fn decode_laplace(
        &mut self,
        zero_frequency: u16,
        decay: u16,
    ) -> Result<i32, RangeError> {
        let value = self.value(1 << 15)?;
        let (symbol, low, high) = laplace_interval(value, zero_frequency, decay)?;
        self.update(low, high, 1 << 15)?;
        Ok(symbol)
    }

    pub(crate) fn read_raw_bits(&mut self, count: u8) -> Result<u32, RangeError> {
        if count > u32::BITS as u8 {
            return Err(RangeError::RawBitsTooWide);
        }
        if usize::from(count)
            > self
                .data
                .len()
                .saturating_mul(8)
                .saturating_sub(self.raw_bits_read)
        {
            return Err(RangeError::RawBitsExhausted);
        }
        let mut value = 0_u32;
        for output_bit in 0..count {
            let bit_position = self.raw_bits_read;
            let byte = self.data[self.data.len() - 1 - bit_position / 8];
            value |= u32::from((byte >> (bit_position % 8)) & 1) << output_bit;
            self.raw_bits_read += 1;
        }
        self.nbits_total += u32::from(count);
        Ok(value)
    }

    pub(crate) fn tell(&self) -> u32 {
        self.nbits_total - (u32::BITS - self.rng.leading_zeros())
    }

    pub(crate) fn tell_frac(&self) -> u32 {
        let mut log_range = u32::BITS - self.rng.leading_zeros();
        let mut range_q15 = self.rng >> (log_range - 16);
        for _ in 0..3 {
            range_q15 = (range_q15 * range_q15) >> 15;
            let bit = range_q15 >> 16;
            log_range = 2 * log_range + bit;
            if bit != 0 {
                range_q15 >>= 1;
            }
        }
        self.nbits_total * 8 - log_range
    }

    fn value(&self, total: u32) -> Result<u32, RangeError> {
        if total == 0 || total > u16::MAX.into() {
            return Err(RangeError::InvalidTotal);
        }
        let unit = self.rng / total;
        if unit == 0 {
            return Err(RangeError::InvalidTotal);
        }
        let scaled = (self.val / unit).saturating_add(1);
        Ok(total.saturating_sub(scaled.min(total)))
    }

    fn update(&mut self, low: u32, high: u32, total: u32) -> Result<(), RangeError> {
        if total == 0 || low >= high || high > total || total > u16::MAX.into() {
            return Err(RangeError::InvalidTotal);
        }
        let unit = self.rng / total;
        if unit == 0 {
            return Err(RangeError::InvalidTotal);
        }
        let offset = unit
            .checked_mul(total - high)
            .ok_or(RangeError::CorruptFrame)?;
        self.val = self
            .val
            .checked_sub(offset)
            .ok_or(RangeError::CorruptFrame)?;
        self.rng = if low > 0 {
            unit.checked_mul(high - low)
                .ok_or(RangeError::CorruptFrame)?
        } else {
            self.rng - offset
        };
        self.normalize();
        Ok(())
    }

    fn normalize(&mut self) {
        while self.rng <= 1 << 23 {
            self.rng <<= 8;
            let next = self.data.get(self.offset).copied().unwrap_or(0);
            self.offset = self.offset.saturating_add(1);
            let symbol = (self.rem << 7) | (next >> 1);
            self.rem = next & 1;
            self.val = ((self.val << 8) + u32::from(255 - symbol)) & 0x7fff_ffff;
            self.nbits_total += 8;
        }
    }
}

pub(crate) fn laplace_interval(
    value: u32,
    zero_frequency: u16,
    decay: u16,
) -> Result<(i32, u32, u32), RangeError> {
    let zero_frequency = u32::from(zero_frequency);
    let decay = u32::from(decay);
    if value >= 1 << 15
        || zero_frequency == 0
        || zero_frequency > (1 << 15) - 32
        || decay >= 1 << 14
    {
        return Err(RangeError::InvalidLaplaceModel);
    }
    if value < zero_frequency {
        return Ok((0, 0, zero_frequency));
    }

    let mut low = zero_frequency;
    let mut side_frequency = ((((1 << 15) - 32 - zero_frequency) * ((1 << 14) - decay)) >> 15) + 1;
    let mut magnitude = 1_i32;
    while side_frequency > 1 && value >= low + 2 * side_frequency {
        low += 2 * side_frequency;
        magnitude += 1;
        side_frequency = (((2 * side_frequency - 2) * decay) >> 15) + 1;
    }
    if side_frequency == 1 {
        let tail_index = (value - low) / 2;
        magnitude += tail_index as i32;
        low += 2 * tail_index;
    }
    let high = if value < low + side_frequency {
        -magnitude
    } else {
        low += side_frequency;
        magnitude
    };
    let upper = low + side_frequency;
    if upper > 1 << 15 {
        return Err(RangeError::InvalidLaplaceModel);
    }
    Ok((high, low, upper))
}

#[cfg(test)]
mod tests {
    use super::{RangeDecoder, RangeError, laplace_interval};

    #[test]
    fn decodes_symbol_interval_edges() {
        let mut low = RangeDecoder::new(&[0x00]);
        assert_eq!(low.decode_symbol(&[1, 1]), Ok(0));

        let mut high = RangeDecoder::new(&[0xff]);
        assert_eq!(high.decode_symbol(&[1, 1]), Ok(1));
    }

    #[test]
    fn decodes_skewed_and_inverse_cdf_contexts() {
        let mut low = RangeDecoder::new(&[0x00]);
        assert_eq!(low.decode_symbol(&[3, 1]), Ok(0));

        let mut high = RangeDecoder::new(&[0xff]);
        assert_eq!(high.decode_symbol(&[3, 1]), Ok(1));

        let mut low = RangeDecoder::new(&[0x00]);
        assert_eq!(low.decode_icdf(&[192, 0], 8), Ok(0));

        let mut high = RangeDecoder::new(&[0xff]);
        assert_eq!(high.decode_icdf(&[192, 0], 8), Ok(1));
    }

    #[test]
    fn decodes_log_probability_bits_and_uniform_integers() {
        let mut low = RangeDecoder::new(&[0x00]);
        assert_eq!(low.decode_bit_logp(1), Ok(false));
        let mut high = RangeDecoder::new(&[0xff]);
        assert_eq!(high.decode_bit_logp(1), Ok(true));

        let mut low = RangeDecoder::new(&[0x00]);
        assert_eq!(low.decode_uint(10), Ok(0));
        let mut high = RangeDecoder::new(&[0xff]);
        assert_eq!(high.decode_uint(10), Ok(9));
        let mut low = RangeDecoder::new(&[0x00, 0x00]);
        assert_eq!(low.decode_uint(1000), Ok(0));
        let mut high = RangeDecoder::new(&[0xff, 0xff]);
        assert_eq!(high.decode_uint(1000), Ok(999));
        assert_eq!(
            RangeDecoder::new(&[]).decode_uint(0),
            Err(RangeError::InvalidTotal)
        );
    }

    #[test]
    fn reads_raw_bits_from_the_end_in_lsb_first_order() {
        let mut decoder = RangeDecoder::new(&[0x00, 0b1011_0010]);
        assert_eq!(decoder.tell_frac(), 8);
        assert_eq!(decoder.read_raw_bits(4), Ok(0b0010));
        assert_eq!(decoder.read_raw_bits(4), Ok(0b1011));
        assert_eq!(decoder.tell(), 9);
        assert_eq!(decoder.tell_frac(), 72);
        assert_eq!(decoder.read_raw_bits(8), Ok(0));
        assert_eq!(decoder.read_raw_bits(1), Err(RangeError::RawBitsExhausted));
        assert_eq!(decoder.read_raw_bits(33), Err(RangeError::RawBitsTooWide));
    }

    #[test]
    fn fractional_bit_count_rounds_up_to_whole_bits() {
        let mut decoder = RangeDecoder::new(&[0x36, 0x92, 0x17]);
        for frequencies in [&[1, 1][..], &[1, 2, 5][..], &[9, 1][..]] {
            let _ = decoder.decode_symbol(frequencies).unwrap();
            let whole_bits = decoder.tell();
            let fractional_bits = decoder.tell_frac();
            assert!(whole_bits * 8 >= fractional_bits);
            assert!(whole_bits * 8 < fractional_bits + 8);
        }
    }

    #[test]
    fn rejects_invalid_contexts_and_cdf_tables() {
        assert_eq!(
            RangeDecoder::new(&[0]).decode_symbol(&[]),
            Err(RangeError::InvalidFrequencyTable)
        );
        assert_eq!(
            RangeDecoder::new(&[0]).decode_symbol(&[0, 0]),
            Err(RangeError::InvalidFrequencyTable)
        );
        assert_eq!(
            RangeDecoder::new(&[0]).decode_icdf(&[1], 8),
            Err(RangeError::InvalidInverseCdf)
        );
        assert_eq!(
            RangeDecoder::new(&[0]).decode_icdf(&[128, 192, 0], 8),
            Err(RangeError::InvalidInverseCdf)
        );
        assert_eq!(
            RangeDecoder::new(&[0]).decode_bit_logp(0),
            Err(RangeError::InvalidLogProbability)
        );
    }

    #[test]
    fn laplace_context_partitions_zero_signed_and_tail_values() {
        let zero_frequency = 72_u16 << 7;
        let zero_frequency_32 = u32::from(zero_frequency);
        let decay = 127_u16 << 6;
        let first_side_frequency =
            ((((1 << 15) - 32 - zero_frequency_32) * ((1 << 14) - u32::from(decay))) >> 15) + 1;
        assert_eq!(
            laplace_interval(0, zero_frequency, decay),
            Ok((0, 0, zero_frequency_32))
        );
        assert_eq!(
            laplace_interval(zero_frequency_32, zero_frequency, decay),
            Ok((
                -1,
                zero_frequency_32,
                zero_frequency_32 + first_side_frequency
            ))
        );
        assert_eq!(
            laplace_interval(
                zero_frequency_32 + first_side_frequency,
                zero_frequency,
                decay
            ),
            Ok((
                1,
                zero_frequency_32 + first_side_frequency,
                zero_frequency_32 + 2 * first_side_frequency
            ))
        );
        for value in 0..1 << 15 {
            let (_, low, high) = laplace_interval(value, zero_frequency, decay).unwrap();
            assert!(low <= value && value < high && high <= 1 << 15);
        }
    }

    #[test]
    fn rejects_invalid_laplace_models_and_cumulative_values() {
        assert_eq!(
            laplace_interval(0, 0, 100),
            Err(RangeError::InvalidLaplaceModel)
        );
        assert_eq!(
            laplace_interval(0, 32_737, 100),
            Err(RangeError::InvalidLaplaceModel)
        );
        assert_eq!(
            laplace_interval(0, 1, 16_384),
            Err(RangeError::InvalidLaplaceModel)
        );
        assert_eq!(
            laplace_interval(32_768, 100, 100),
            Err(RangeError::InvalidLaplaceModel)
        );
    }
}
