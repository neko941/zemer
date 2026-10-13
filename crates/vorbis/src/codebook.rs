use zemer_core::AudioError;

use crate::bitstream::BitReader;

pub(crate) const MAX_SETUP_BYTES: usize = 64 * 1024 * 1024;

pub(crate) struct AllocationBudget {
    remaining: usize,
}

impl AllocationBudget {
    pub(crate) const fn new() -> Self {
        Self {
            remaining: MAX_SETUP_BYTES,
        }
    }

    fn reserve(&mut self, bytes: usize) -> Result<(), AudioError> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or(AudioError::InvalidData(
                "Vorbis setup exceeds its memory limit",
            ))?;
        Ok(())
    }
}

pub(crate) struct Codebook {
    pub(crate) dimensions: u16,
    pub(crate) entries: u32,
    pub(crate) lengths: Vec<u8>,
    pub(crate) lookup: Lookup,
}

pub(crate) struct Lookup {
    pub(crate) kind: u8,
    pub(crate) minimum: u32,
    pub(crate) delta: u32,
    pub(crate) value_bits: u8,
    pub(crate) sequence: bool,
    pub(crate) multiplicands: Vec<u16>,
}

impl Codebook {
    pub(crate) fn parse(
        bits: &mut BitReader<'_>,
        budget: &mut AllocationBudget,
    ) -> Result<Self, AudioError> {
        if bits.read_bits(24)? != 0x56_43_42 {
            return Err(AudioError::InvalidFormat("invalid Vorbis codebook sync"));
        }
        let dimensions = bits.read_bits(16)? as u16;
        let entries = bits.read_bits(24)? as u32;
        if dimensions == 0 || entries == 0 {
            return Err(AudioError::InvalidFormat("empty Vorbis codebook"));
        }
        let entries_usize = entries as usize;
        budget.reserve(entries_usize)?;
        let mut lengths = Vec::new();
        lengths
            .try_reserve_exact(entries_usize)
            .map_err(|_| AudioError::InvalidData("Vorbis codebook allocation failed"))?;
        lengths.resize(entries_usize, 0);

        if bits.read_bit()? {
            let mut entry = 0_usize;
            let mut length = bits.read_bits(5)? as u8 + 1;
            while entry < entries_usize {
                if length > 32 {
                    return Err(AudioError::InvalidFormat(
                        "Vorbis codeword length exceeds 32 bits",
                    ));
                }
                let remaining = entries_usize - entry;
                let count = bits.read_bits(integer_log(remaining) as u32)? as usize;
                if count > remaining {
                    return Err(AudioError::InvalidFormat(
                        "Vorbis ordered codeword count exceeds its codebook",
                    ));
                }
                lengths[entry..entry + count].fill(length);
                entry += count;
                length += 1;
            }
        } else {
            let sparse = bits.read_bit()?;
            for length in &mut lengths {
                if !sparse || bits.read_bit()? {
                    *length = bits.read_bits(5)? as u8 + 1;
                }
            }
        }
        validate_lengths(&lengths)?;

        let kind = bits.read_bits(4)? as u8;
        let lookup = match kind {
            0 => Lookup {
                kind,
                minimum: 0,
                delta: 0,
                value_bits: 0,
                sequence: false,
                multiplicands: Vec::new(),
            },
            1 | 2 => {
                let minimum = bits.read_bits(32)? as u32;
                let delta = bits.read_bits(32)? as u32;
                let value_bits = bits.read_bits(4)? as u8 + 1;
                let sequence = bits.read_bit()?;
                let values =
                    if kind == 1 {
                        lookup1_values(entries, dimensions)
                    } else {
                        entries.checked_mul(u32::from(dimensions)).ok_or(
                            AudioError::InvalidData("Vorbis codebook lookup size overflow"),
                        )?
                    } as usize;
                let allocation = values.checked_mul(std::mem::size_of::<u16>()).ok_or(
                    AudioError::InvalidData("Vorbis codebook lookup size overflow"),
                )?;
                budget.reserve(allocation)?;
                if values > bits.remaining_bits() / usize::from(value_bits) {
                    return Err(AudioError::InvalidFormat(
                        "truncated Vorbis codebook lookup table",
                    ));
                }
                let mut multiplicands = Vec::new();
                multiplicands
                    .try_reserve_exact(values)
                    .map_err(|_| AudioError::InvalidData("Vorbis lookup allocation failed"))?;
                for _ in 0..values {
                    multiplicands.push(bits.read_bits(u32::from(value_bits))? as u16);
                }
                Lookup {
                    kind,
                    minimum,
                    delta,
                    value_bits,
                    sequence,
                    multiplicands,
                }
            }
            _ => return Err(AudioError::InvalidFormat("reserved Vorbis lookup type")),
        };

        Ok(Self {
            dimensions,
            entries,
            lengths,
            lookup,
        })
    }
}

fn validate_lengths(lengths: &[u8]) -> Result<(), AudioError> {
    let mut counts = [0_i64; 33];
    for &length in lengths {
        if length > 32 {
            return Err(AudioError::InvalidFormat(
                "Vorbis codeword length exceeds 32 bits",
            ));
        }
        if length != 0 {
            counts[usize::from(length)] += 1;
        }
    }
    let used = counts.iter().sum::<i64>();
    if used == 0 {
        return Err(AudioError::InvalidFormat("empty Vorbis Huffman tree"));
    }
    if used == 1 {
        return if counts[1] == 1 {
            Ok(())
        } else {
            Err(AudioError::InvalidFormat(
                "single-entry Vorbis codebook must use a one-bit codeword",
            ))
        };
    }
    let mut available = 1_i64;
    for count in counts.iter().skip(1) {
        available = available * 2 - count;
        if available < 0 {
            return Err(AudioError::InvalidFormat(
                "overspecified Vorbis Huffman tree",
            ));
        }
    }
    if available != 0 {
        return Err(AudioError::InvalidFormat(
            "underspecified Vorbis Huffman tree",
        ));
    }
    Ok(())
}

fn integer_log(value: usize) -> usize {
    usize::BITS as usize - value.leading_zeros() as usize
}

fn lookup1_values(entries: u32, dimensions: u16) -> u32 {
    let mut low = 1_u32;
    let mut high = entries;
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if power_at_most(mid, dimensions, entries) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low
}

fn power_at_most(base: u32, exponent: u16, limit: u32) -> bool {
    let mut value = 1_u64;
    for _ in 0..exponent {
        value *= u64::from(base);
        if value > u64::from(limit) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{AllocationBudget, Codebook, integer_log, lookup1_values};
    use crate::bitstream::BitReader;

    struct BitWriter(Vec<bool>);

    impl BitWriter {
        fn new() -> Self {
            Self(Vec::new())
        }

        fn write(&mut self, value: u64, width: usize) {
            self.0.extend((0..width).map(|bit| value & (1 << bit) != 0));
        }

        fn bytes(self) -> Vec<u8> {
            let mut bytes = vec![0; self.0.len().div_ceil(8)];
            for (offset, bit) in self.0.into_iter().enumerate() {
                if bit {
                    bytes[offset / 8] |= 1 << (offset % 8);
                }
            }
            bytes
        }
    }

    fn codebook(ordered: bool, lengths: &[Option<u8>], lookup: u8) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.write(0x56_43_42, 24);
        writer.write(1, 16);
        writer.write(lengths.len() as u64, 24);
        writer.write(u64::from(ordered), 1);
        if ordered {
            let first = lengths.iter().flatten().copied().min().unwrap();
            writer.write(u64::from(first - 1), 5);
            let mut start = 0;
            let mut length = first;
            while start < lengths.len() {
                let count = lengths[start..]
                    .iter()
                    .take_while(|entry| **entry == Some(length))
                    .count();
                let remaining = lengths.len() - start;
                writer.write(count as u64, integer_log(remaining));
                start += count;
                length += 1;
            }
        } else {
            writer.write(0, 1);
            for length in lengths {
                writer.write(u64::from(length.unwrap() - 1), 5);
            }
        }
        writer.write(u64::from(lookup), 4);
        if lookup != 0 {
            writer.write(0, 32);
            writer.write(0, 32);
            writer.write(0, 4);
            writer.write(0, 1);
            let count = if lookup == 1 {
                lookup1_values(lengths.len() as u32, 1) as usize
            } else {
                lengths.len()
            };
            for _ in 0..count {
                writer.write(0, 1);
            }
        }
        writer.bytes()
    }

    #[test]
    fn parses_ordered_and_unordered_complete_huffman_trees() {
        for ordered in [false, true] {
            let packet = codebook(ordered, &[Some(1), Some(2), Some(2)], 0);
            let mut bits = BitReader::new(&packet);
            let parsed = Codebook::parse(&mut bits, &mut AllocationBudget::new()).unwrap();
            assert_eq!(parsed.dimensions, 1);
            assert_eq!(parsed.entries, 3);
            assert_eq!(parsed.lengths, [1, 2, 2]);
            assert_eq!(parsed.lookup.kind, 0);
        }
    }

    #[test]
    fn parses_lookup_type_one_and_two_tables() {
        for lookup in [1, 2] {
            let packet = codebook(false, &[Some(1), Some(1)], lookup);
            let parsed =
                Codebook::parse(&mut BitReader::new(&packet), &mut AllocationBudget::new())
                    .unwrap();
            assert_eq!(parsed.lookup.kind, lookup);
            assert_eq!(parsed.lookup.multiplicands.len(), 2);
        }
    }
}
