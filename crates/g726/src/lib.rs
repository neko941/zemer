mod bitstream;
mod rate;
mod state;

pub use rate::G726Rate;
use state::DecoderState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum G726Error {
    InvalidChannelCount,
    NotEnoughPackedData,
    AllocationFailed,
}

impl std::fmt::Display for G726Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidChannelCount => f.write_str("G.726 channel count must be nonzero"),
            Self::NotEnoughPackedData => {
                f.write_str("packed G.726 data is shorter than its sample count")
            }
            Self::AllocationFailed => f.write_str("unable to allocate G.726 output buffer"),
        }
    }
}

impl std::error::Error for G726Error {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum G726Packing {
    MsbFirst,
    LsbFirst,
}

/// Stateful G.726 encoder for interleaved signed 16-bit PCM input.
/// The two least-significant PCM bits are discarded to form the 14-bit input.
#[derive(Debug)]
pub struct G726Encoder {
    rate: G726Rate,
    channels: Vec<DecoderState>,
    packing: G726Packing,
    bit_buffer: u16,
    bit_count: u8,
    next_channel: usize,
}

impl G726Encoder {
    pub fn new(rate: G726Rate, channels: usize, packing: G726Packing) -> Result<Self, G726Error> {
        if channels == 0 {
            return Err(G726Error::InvalidChannelCount);
        }
        let mut states = Vec::new();
        states
            .try_reserve_exact(channels)
            .map_err(|_| G726Error::AllocationFailed)?;
        states.extend((0..channels).map(|_| DecoderState::new(rate)));
        Ok(Self {
            rate,
            channels: states,
            packing,
            bit_buffer: 0,
            bit_count: 0,
            next_channel: 0,
        })
    }

    pub fn rate(&self) -> G726Rate {
        self.rate
    }

    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    /// Encode interleaved PCM samples, retaining incomplete output bytes between calls.
    pub fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>, G726Error> {
        let output_bytes = samples
            .len()
            .checked_mul(usize::from(self.rate.bits_per_sample()))
            .and_then(|bits| bits.checked_add(usize::from(self.bit_count)))
            .ok_or(G726Error::AllocationFailed)?
            / 8;
        let mut output = Vec::new();
        output
            .try_reserve_exact(output_bytes)
            .map_err(|_| G726Error::AllocationFailed)?;
        for &sample in samples {
            let code = self.channels[self.next_channel].encode(sample);
            self.next_channel = (self.next_channel + 1) % self.channels.len();
            self.push_code(code, &mut output);
        }
        Ok(output)
    }

    /// Flush the final partial byte, padded with zero bits.
    pub fn finish(&mut self) -> Vec<u8> {
        if self.bit_count == 0 {
            return Vec::new();
        }
        let byte = match self.packing {
            G726Packing::LsbFirst => self.bit_buffer as u8,
            G726Packing::MsbFirst => (self.bit_buffer << (8 - self.bit_count)) as u8,
        };
        self.bit_buffer = 0;
        self.bit_count = 0;
        vec![byte]
    }

    fn push_code(&mut self, code: u8, output: &mut Vec<u8>) {
        let width = self.rate.bits_per_sample();
        match self.packing {
            G726Packing::LsbFirst => {
                self.bit_buffer |= u16::from(code) << self.bit_count;
                self.bit_count += width;
                while self.bit_count >= 8 {
                    output.push(self.bit_buffer as u8);
                    self.bit_buffer >>= 8;
                    self.bit_count -= 8;
                }
            }
            G726Packing::MsbFirst => {
                self.bit_buffer = (self.bit_buffer << width) | u16::from(code);
                self.bit_count += width;
                while self.bit_count >= 8 {
                    self.bit_count -= 8;
                    output.push((self.bit_buffer >> self.bit_count) as u8);
                    self.bit_buffer &= (1_u16 << self.bit_count) - 1;
                }
            }
        }
    }
}

/// Stateful G.726 decoder for interleaved channels and MSB-first packed codewords.
#[derive(Debug)]
pub struct G726Decoder {
    rate: G726Rate,
    channels: Vec<DecoderState>,
    lsb_buffer: u16,
    lsb_bits: u8,
    next_channel: usize,
}

impl G726Decoder {
    pub fn new(rate: G726Rate, channels: usize) -> Result<Self, G726Error> {
        if channels == 0 {
            return Err(G726Error::InvalidChannelCount);
        }
        Ok(Self {
            rate,
            channels: (0..channels).map(|_| DecoderState::new(rate)).collect(),
            lsb_buffer: 0,
            lsb_bits: 0,
            next_channel: 0,
        })
    }

    pub fn rate(&self) -> G726Rate {
        self.rate
    }

    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    /// Decode `sample_count` interleaved codewords from a packed MSB-first byte stream.
    /// Extra bytes are ignored so callers can pass a container chunk with trailing padding.
    pub fn decode_packed(
        &mut self,
        packed: &[u8],
        sample_count: usize,
    ) -> Result<Vec<i16>, G726Error> {
        self.decode_packed_with_order(packed, sample_count, false)
    }

    /// Decode codewords whose bits are packed least-significant-bit first.
    pub fn decode_packed_lsb(
        &mut self,
        packed: &[u8],
        sample_count: usize,
    ) -> Result<Vec<i16>, G726Error> {
        self.decode_packed_with_order(packed, sample_count, true)
    }

    /// Return how many new bytes `decode_packed_lsb_stream` needs for this block.
    pub fn packed_lsb_bytes_needed(&self, sample_count: usize) -> Result<usize, G726Error> {
        let bits = sample_count
            .checked_mul(usize::from(self.rate.bits_per_sample()))
            .ok_or(G726Error::NotEnoughPackedData)?;
        Ok(bits.saturating_sub(usize::from(self.lsb_bits)).div_ceil(8))
    }

    /// Decode the next interleaved LSB-first block while retaining sub-byte state between calls.
    /// Consumed bytes are removed from `packed`; trailing bytes remain available to the caller.
    pub fn decode_packed_lsb_stream(
        &mut self,
        packed: &mut &[u8],
        sample_count: usize,
    ) -> Result<Vec<i16>, G726Error> {
        let bytes_needed = self.packed_lsb_bytes_needed(sample_count)?;
        if packed.len() < bytes_needed {
            return Err(G726Error::NotEnoughPackedData);
        }

        let mut output = Vec::with_capacity(sample_count);
        let width = self.rate.bits_per_sample();
        let mask = (1_u16 << width) - 1;
        for _ in 0..sample_count {
            while self.lsb_bits < width {
                self.lsb_buffer |= u16::from(packed[0]) << self.lsb_bits;
                *packed = &packed[1..];
                self.lsb_bits += 8;
            }
            let code = (self.lsb_buffer & mask) as u8;
            self.lsb_buffer >>= width;
            self.lsb_bits -= width;
            output.push(self.channels[self.next_channel].decode(code));
            self.next_channel = (self.next_channel + 1) % self.channels.len();
        }
        Ok(output)
    }

    fn decode_packed_with_order(
        &mut self,
        packed: &[u8],
        sample_count: usize,
        lsb_first: bool,
    ) -> Result<Vec<i16>, G726Error> {
        let bits = sample_count
            .checked_mul(usize::from(self.rate.bits_per_sample()))
            .ok_or(G726Error::NotEnoughPackedData)?;
        if bits.div_ceil(8) > packed.len() {
            return Err(G726Error::NotEnoughPackedData);
        }

        let mut output = Vec::with_capacity(sample_count);
        let width = self.rate.bits_per_sample();
        for index in 0..sample_count {
            let offset = index * usize::from(width);
            let code = if lsb_first {
                bitstream::read_code_lsb(packed, offset, width)
            } else {
                bitstream::read_code(packed, offset, width)
            };
            let channel = index % self.channels.len();
            output.push(self.channels[channel].decode(code));
        }
        Ok(output)
    }

    /// Decode one unpacked codeword for the next channel in the interleaved stream.
    pub fn decode_code(&mut self, channel: usize, code: u8) -> Option<i16> {
        let state = self.channels.get_mut(channel)?;
        (code < (1 << self.rate.bits_per_sample())).then(|| state.decode(code))
    }
}

#[cfg(test)]
mod tests {
    use super::{G726Decoder, G726Encoder, G726Error, G726Packing, G726Rate, bitstream};

    #[test]
    fn packed_codewords_are_msb_first_across_byte_boundaries() {
        for rate in [
            G726Rate::Kbps16,
            G726Rate::Kbps24,
            G726Rate::Kbps32,
            G726Rate::Kbps40,
        ] {
            let width = rate.bits_per_sample();
            let count = usize::from(width) * 3 + 1;
            let codes: Vec<u8> = (0..count)
                .map(|i| (i as u8 * 7) & ((1 << width) - 1))
                .collect();
            let packed = bitstream::pack_codes(&codes, width);
            let unpacked: Vec<u8> = (0..count)
                .map(|i| bitstream::read_code(&packed, i * usize::from(width), width))
                .collect();
            assert_eq!(unpacked, codes);
        }
    }

    #[test]
    fn packed_codewords_are_lsb_first_across_byte_boundaries() {
        for rate in [
            G726Rate::Kbps16,
            G726Rate::Kbps24,
            G726Rate::Kbps32,
            G726Rate::Kbps40,
        ] {
            let width = rate.bits_per_sample();
            let count = usize::from(width) * 3 + 1;
            let codes: Vec<u8> = (0..count)
                .map(|i| (i as u8 * 7) & ((1 << width) - 1))
                .collect();
            let packed = bitstream::pack_codes_lsb(&codes, width);
            let unpacked: Vec<u8> = (0..count)
                .map(|i| bitstream::read_code_lsb(&packed, i * usize::from(width), width))
                .collect();
            assert_eq!(unpacked, codes);
        }
    }

    #[test]
    fn packed_lsb_decode_matches_unpacked_codewords() {
        for rate in [
            G726Rate::Kbps16,
            G726Rate::Kbps24,
            G726Rate::Kbps32,
            G726Rate::Kbps40,
        ] {
            let width = rate.bits_per_sample();
            let codes: Vec<u8> = (0..37)
                .map(|i| ((i * 11) as u8) & ((1 << width) - 1))
                .collect();
            let packed = bitstream::pack_codes_lsb(&codes, width);
            let mut packed_decoder = G726Decoder::new(rate, 1).unwrap();
            let mut code_decoder = G726Decoder::new(rate, 1).unwrap();
            let packed_pcm = packed_decoder
                .decode_packed_lsb(&packed, codes.len())
                .unwrap();
            let code_pcm: Vec<i16> = codes
                .iter()
                .map(|&code| code_decoder.decode_code(0, code).unwrap())
                .collect();
            assert_eq!(packed_pcm, code_pcm, "{rate:?}");
        }
    }

    #[test]
    fn validates_channel_count_and_packed_length() {
        assert_eq!(
            G726Decoder::new(G726Rate::Kbps32, 0).unwrap_err(),
            G726Error::InvalidChannelCount
        );
        let mut decoder = G726Decoder::new(G726Rate::Kbps40, 1).unwrap();
        assert_eq!(
            decoder.decode_packed(&[], 1),
            Err(G726Error::NotEnoughPackedData)
        );
    }

    #[test]
    fn encoder_keeps_bit_packing_continuous_across_input_blocks() {
        for rate in [
            G726Rate::Kbps16,
            G726Rate::Kbps24,
            G726Rate::Kbps32,
            G726Rate::Kbps40,
        ] {
            for channels in [1, 2, 3] {
                let samples: Vec<i16> = (0..113 * channels)
                    .map(|index| ((index * 1_237) as i16).wrapping_mul(19))
                    .collect();
                for packing in [G726Packing::MsbFirst, G726Packing::LsbFirst] {
                    let mut whole = G726Encoder::new(rate, channels, packing).unwrap();
                    let mut expected = whole.encode(&samples).unwrap();
                    expected.extend(whole.finish());

                    let mut split = G726Encoder::new(rate, channels, packing).unwrap();
                    let mut actual = Vec::new();
                    for block in samples.chunks(7) {
                        actual.extend(split.encode(block).unwrap());
                    }
                    actual.extend(split.finish());
                    assert_eq!(
                        actual, expected,
                        "{rate:?}, {channels} channels, {packing:?}"
                    );

                    let mut decoder = G726Decoder::new(rate, channels).unwrap();
                    let decoded = match packing {
                        G726Packing::MsbFirst => decoder.decode_packed(&actual, samples.len()),
                        G726Packing::LsbFirst => decoder.decode_packed_lsb(&actual, samples.len()),
                    }
                    .unwrap();
                    assert_eq!(decoded.len(), samples.len());
                }
            }
        }
    }

    #[test]
    fn streaming_lsb_decode_matches_whole_stream_across_arbitrary_block_boundaries() {
        for rate in [
            G726Rate::Kbps16,
            G726Rate::Kbps24,
            G726Rate::Kbps32,
            G726Rate::Kbps40,
        ] {
            for channels in [1, 2, 3] {
                let sample_count = 97;
                let width = rate.bits_per_sample();
                let codes: Vec<u8> = (0..sample_count)
                    .map(|i| ((i * 13 + i / channels) as u8) & ((1 << width) - 1))
                    .collect();
                let packed = bitstream::pack_codes_lsb(&codes, width);
                let expected = G726Decoder::new(rate, channels)
                    .unwrap()
                    .decode_packed_lsb(&packed, sample_count)
                    .unwrap();
                let mut streaming = G726Decoder::new(rate, channels).unwrap();
                let mut remaining = packed.as_slice();
                let mut actual = Vec::with_capacity(sample_count);
                let mut decoded = 0;
                for block_size in [1, 4, 2, 11, 7, 19, 3, 23, 27] {
                    if decoded == sample_count {
                        break;
                    }
                    let count = block_size.min(sample_count - decoded);
                    let block = streaming
                        .decode_packed_lsb_stream(&mut remaining, count)
                        .unwrap();
                    actual.extend(block);
                    decoded += count;
                }
                if decoded < sample_count {
                    actual.extend(
                        streaming
                            .decode_packed_lsb_stream(&mut remaining, sample_count - decoded)
                            .unwrap(),
                    );
                }
                assert_eq!(actual, expected, "{rate:?}, {channels} channels");
                assert!(remaining.is_empty());
            }
        }
    }
}
