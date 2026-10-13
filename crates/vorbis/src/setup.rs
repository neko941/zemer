use zemer_core::AudioError;

use crate::{
    VorbisIdentificationHeader,
    bitstream::BitReader,
    codebook::{AllocationBudget, Codebook},
};

const SETUP_PREFIX: &[u8] = b"\x05vorbis";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VorbisSetupHeader {
    pub codebooks: Vec<VorbisCodebook>,
    pub floors: Vec<VorbisFloor>,
    pub residues: Vec<VorbisResidue>,
    pub mappings: Vec<VorbisMapping>,
    pub modes: Vec<VorbisMode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VorbisCodebook {
    pub dimensions: u16,
    pub entries: u32,
    pub codeword_lengths: Vec<u8>,
    pub lookup: VorbisLookup,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VorbisLookup {
    pub kind: u8,
    pub minimum: u32,
    pub delta: u32,
    pub value_bits: u8,
    pub sequence: bool,
    pub multiplicands: Vec<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VorbisFloor {
    Type0(VorbisFloor0),
    Type1(VorbisFloor1),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VorbisFloor0 {
    pub order: u8,
    pub rate: u16,
    pub bark_map_size: u16,
    pub amplitude_bits: u8,
    pub amplitude_offset: u8,
    pub books: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VorbisFloor1 {
    pub partition_classes: Vec<u8>,
    pub classes: Vec<VorbisFloor1Class>,
    pub multiplier: u8,
    pub range_bits: u8,
    pub x_list: Vec<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VorbisFloor1Class {
    pub dimensions: u8,
    pub subclasses: u8,
    pub masterbook: Option<u8>,
    pub subclass_books: Vec<Option<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VorbisResidue {
    pub kind: u16,
    pub begin: u32,
    pub end: u32,
    pub partition_size: u32,
    pub classbook: u8,
    pub cascades: Vec<[Option<u8>; 8]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VorbisMapping {
    pub channel_mux: Vec<u8>,
    pub coupling: Vec<VorbisCouplingStep>,
    pub submaps: Vec<VorbisSubmap>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VorbisCouplingStep {
    pub magnitude: u8,
    pub angle: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VorbisSubmap {
    pub floor: u8,
    pub residue: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VorbisMode {
    pub long_block: bool,
    pub mapping: u8,
}

impl VorbisSetupHeader {
    pub fn parse(
        packet: &[u8],
        identification: &VorbisIdentificationHeader,
    ) -> Result<Self, AudioError> {
        if packet.len() < SETUP_PREFIX.len() + 1 || !packet.starts_with(SETUP_PREFIX) {
            return Err(AudioError::InvalidFormat("invalid Vorbis setup header"));
        }
        if packet.len() > super::codebook::MAX_SETUP_BYTES {
            return Err(AudioError::InvalidData(
                "Vorbis setup packet exceeds 64 MiB",
            ));
        }
        let mut bits = BitReader::new(&packet[SETUP_PREFIX.len()..]);
        let mut budget = AllocationBudget::new();

        let codebook_count = bits.read_bits(8)? as usize + 1;
        let mut codebooks = Vec::new();
        codebooks
            .try_reserve_exact(codebook_count)
            .map_err(|_| AudioError::InvalidData("Vorbis setup allocation failed"))?;
        for _ in 0..codebook_count {
            let parsed = Codebook::parse(&mut bits, &mut budget)?;
            codebooks.push(VorbisCodebook {
                dimensions: parsed.dimensions,
                entries: parsed.entries,
                codeword_lengths: parsed.lengths,
                lookup: VorbisLookup {
                    kind: parsed.lookup.kind,
                    minimum: parsed.lookup.minimum,
                    delta: parsed.lookup.delta,
                    value_bits: parsed.lookup.value_bits,
                    sequence: parsed.lookup.sequence,
                    multiplicands: parsed.lookup.multiplicands,
                },
            });
        }

        let time_count = bits.read_bits(6)? as usize + 1;
        for _ in 0..time_count {
            if bits.read_bits(16)? != 0 {
                return Err(AudioError::InvalidFormat(
                    "reserved Vorbis time transform is nonzero",
                ));
            }
        }

        let floor_count = bits.read_bits(6)? as usize + 1;
        let mut floors = Vec::new();
        floors
            .try_reserve_exact(floor_count)
            .map_err(|_| AudioError::InvalidData("Vorbis floor allocation failed"))?;
        for _ in 0..floor_count {
            floors.push(match bits.read_bits(16)? {
                0 => VorbisFloor::Type0(parse_floor0(&mut bits, codebooks.len())?),
                1 => VorbisFloor::Type1(parse_floor1(&mut bits, codebooks.len())?),
                _ => return Err(AudioError::InvalidFormat("reserved Vorbis floor type")),
            });
        }

        let residue_count = bits.read_bits(6)? as usize + 1;
        let mut residues = Vec::new();
        residues
            .try_reserve_exact(residue_count)
            .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
        for _ in 0..residue_count {
            residues.push(parse_residue(&mut bits, &codebooks)?);
        }

        let mapping_count = bits.read_bits(6)? as usize + 1;
        let mut mappings = Vec::new();
        mappings
            .try_reserve_exact(mapping_count)
            .map_err(|_| AudioError::InvalidData("Vorbis mapping allocation failed"))?;
        for _ in 0..mapping_count {
            mappings.push(parse_mapping(
                &mut bits,
                usize::from(identification.channels()),
                floor_count,
                residue_count,
            )?);
        }

        let mode_count = bits.read_bits(6)? as usize + 1;
        let mut modes = Vec::new();
        modes
            .try_reserve_exact(mode_count)
            .map_err(|_| AudioError::InvalidData("Vorbis mode allocation failed"))?;
        for _ in 0..mode_count {
            let long_block = bits.read_bit()?;
            if bits.read_bits(16)? != 0 || bits.read_bits(16)? != 0 {
                return Err(AudioError::InvalidFormat(
                    "unsupported Vorbis window or transform type",
                ));
            }
            let mapping = bits.read_bits(8)? as u8;
            if usize::from(mapping) >= mapping_count {
                return Err(AudioError::InvalidFormat("invalid Vorbis mode mapping"));
            }
            modes.push(VorbisMode {
                long_block,
                mapping,
            });
        }
        if !bits.read_bit()? {
            return Err(AudioError::InvalidFormat(
                "invalid Vorbis setup framing bit",
            ));
        }

        Ok(Self {
            codebooks,
            floors,
            residues,
            mappings,
            modes,
        })
    }
}

fn parse_floor0(
    bits: &mut BitReader<'_>,
    codebook_count: usize,
) -> Result<VorbisFloor0, AudioError> {
    let order = bits.read_bits(8)? as u8;
    let rate = bits.read_bits(16)? as u16;
    let bark_map_size = bits.read_bits(16)? as u16;
    let amplitude_bits = bits.read_bits(6)? as u8;
    let amplitude_offset = bits.read_bits(8)? as u8;
    let book_count = bits.read_bits(4)? as usize + 1;
    let mut books = Vec::with_capacity(book_count);
    for _ in 0..book_count {
        let book = bits.read_bits(8)? as u8;
        validate_reference(usize::from(book), codebook_count, "floor-0 codebook")?;
        books.push(book);
    }
    Ok(VorbisFloor0 {
        order,
        rate,
        bark_map_size,
        amplitude_bits,
        amplitude_offset,
        books,
    })
}

fn parse_floor1(
    bits: &mut BitReader<'_>,
    codebook_count: usize,
) -> Result<VorbisFloor1, AudioError> {
    let partition_count = bits.read_bits(5)? as usize;
    let mut partition_classes = Vec::with_capacity(partition_count);
    let mut maximum_class = None;
    for _ in 0..partition_count {
        let class = bits.read_bits(4)? as u8;
        maximum_class = Some(maximum_class.map_or(class, |max: u8| max.max(class)));
        partition_classes.push(class);
    }
    let mut classes = Vec::new();
    if let Some(maximum_class) = maximum_class {
        classes.reserve(usize::from(maximum_class) + 1);
        for _ in 0..=maximum_class {
            let dimensions = bits.read_bits(3)? as u8 + 1;
            let subclasses = bits.read_bits(2)? as u8;
            let masterbook = if subclasses != 0 {
                let book = bits.read_bits(8)? as u8;
                validate_reference(usize::from(book), codebook_count, "floor-1 masterbook")?;
                Some(book)
            } else {
                None
            };
            let mut subclass_books = Vec::with_capacity(1 << subclasses);
            for _ in 0..(1 << subclasses) {
                let encoded = bits.read_bits(8)? as u8;
                let book = encoded.checked_sub(1);
                if let Some(book) = book {
                    validate_reference(usize::from(book), codebook_count, "floor-1 subclass book")?;
                }
                subclass_books.push(book);
            }
            classes.push(VorbisFloor1Class {
                dimensions,
                subclasses,
                masterbook,
                subclass_books,
            });
        }
    }
    let multiplier = bits.read_bits(2)? as u8 + 1;
    let range_bits = bits.read_bits(4)? as u8;
    let endpoint = 1_u32 << range_bits;
    let mut x_list = vec![0_u16, endpoint as u16];
    for class in &partition_classes {
        for _ in 0..classes[usize::from(*class)].dimensions {
            if x_list.len() == 65 {
                return Err(AudioError::InvalidFormat("too many Vorbis floor-1 points"));
            }
            let x = bits.read_bits(u32::from(range_bits))? as u16;
            if x_list.contains(&x) {
                return Err(AudioError::InvalidFormat("duplicate Vorbis floor-1 point"));
            }
            x_list.push(x);
        }
    }
    Ok(VorbisFloor1 {
        partition_classes,
        classes,
        multiplier,
        range_bits,
        x_list,
    })
}

fn parse_residue(
    bits: &mut BitReader<'_>,
    codebooks: &[VorbisCodebook],
) -> Result<VorbisResidue, AudioError> {
    let kind = bits.read_bits(16)? as u16;
    if kind > 2 {
        return Err(AudioError::InvalidFormat("reserved Vorbis residue type"));
    }
    let begin = bits.read_bits(24)? as u32;
    let end = bits.read_bits(24)? as u32;
    let partition_size = bits.read_bits(24)? as u32 + 1;
    let classifications = bits.read_bits(6)? as usize + 1;
    let classbook = bits.read_bits(8)? as u8;
    let classbook_index = usize::from(classbook);
    validate_reference(classbook_index, codebooks.len(), "residue classbook")?;
    let classbook_config = &codebooks[classbook_index];
    if bounded_power(classifications, usize::from(classbook_config.dimensions))
        > classbook_config.entries as usize
    {
        return Err(AudioError::InvalidFormat(
            "residue classbook cannot represent its classifications",
        ));
    }
    if end < begin {
        return Err(AudioError::InvalidFormat("invalid Vorbis residue range"));
    }
    let mut cascades = Vec::with_capacity(classifications);
    for _ in 0..classifications {
        let low = bits.read_bits(3)? as u8;
        let high = if bits.read_bit()? {
            bits.read_bits(5)? as u8
        } else {
            0
        };
        let cascade = low | (high << 3);
        let mut books = [None; 8];
        for (pass, book) in books.iter_mut().enumerate() {
            if cascade & (1 << pass) != 0 {
                let reference = bits.read_bits(8)? as u8;
                validate_reference(
                    usize::from(reference),
                    codebooks.len(),
                    "residue cascade codebook",
                )?;
                if codebooks[usize::from(reference)].lookup.kind == 0 {
                    return Err(AudioError::InvalidFormat(
                        "residue codebook has no value mapping",
                    ));
                }
                *book = Some(reference);
            }
        }
        cascades.push(books);
    }
    Ok(VorbisResidue {
        kind,
        begin,
        end,
        partition_size,
        classbook,
        cascades,
    })
}

fn parse_mapping(
    bits: &mut BitReader<'_>,
    channels: usize,
    floor_count: usize,
    residue_count: usize,
) -> Result<VorbisMapping, AudioError> {
    if bits.read_bits(16)? != 0 {
        return Err(AudioError::InvalidFormat("reserved Vorbis mapping type"));
    }
    let submap_count = if bits.read_bit()? {
        bits.read_bits(4)? as usize + 1
    } else {
        1
    };
    let mut coupling = Vec::new();
    if bits.read_bit()? {
        let coupling_count = bits.read_bits(8)? as usize + 1;
        coupling.reserve(coupling_count);
        let channel_bits = integer_log(channels - 1) as u32;
        for _ in 0..coupling_count {
            let magnitude = bits.read_bits(channel_bits)? as u8;
            let angle = bits.read_bits(channel_bits)? as u8;
            if usize::from(magnitude) >= channels
                || usize::from(angle) >= channels
                || magnitude == angle
            {
                return Err(AudioError::InvalidFormat("invalid Vorbis channel coupling"));
            }
            coupling.push(VorbisCouplingStep { magnitude, angle });
        }
    }
    if bits.read_bits(2)? != 0 {
        return Err(AudioError::InvalidFormat(
            "nonzero Vorbis mapping reserved bits",
        ));
    }
    let mut channel_mux = Vec::with_capacity(channels);
    for _ in 0..channels {
        let submap = if submap_count > 1 {
            bits.read_bits(4)? as u8
        } else {
            0
        };
        if usize::from(submap) >= submap_count {
            return Err(AudioError::InvalidFormat("invalid Vorbis channel mux"));
        }
        channel_mux.push(submap);
    }
    let mut submaps = Vec::with_capacity(submap_count);
    for _ in 0..submap_count {
        if bits.read_bits(8)? != 0 {
            return Err(AudioError::InvalidFormat(
                "reserved Vorbis submap time configuration is nonzero",
            ));
        }
        let floor = bits.read_bits(8)? as u8;
        let residue = bits.read_bits(8)? as u8;
        validate_reference(usize::from(floor), floor_count, "mapping floor")?;
        validate_reference(usize::from(residue), residue_count, "mapping residue")?;
        submaps.push(VorbisSubmap { floor, residue });
    }
    Ok(VorbisMapping {
        channel_mux,
        coupling,
        submaps,
    })
}

fn validate_reference(index: usize, count: usize, name: &'static str) -> Result<(), AudioError> {
    if index >= count {
        return Err(AudioError::InvalidFormat(name));
    }
    Ok(())
}

fn bounded_power(base: usize, exponent: usize) -> usize {
    let mut value = 1_usize;
    for _ in 0..exponent {
        value = value.saturating_mul(base);
    }
    value
}

fn integer_log(value: usize) -> usize {
    usize::BITS as usize - value.leading_zeros() as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VorbisIdentificationHeader;

    struct BitWriter(Vec<bool>);

    impl BitWriter {
        fn new() -> Self {
            Self(Vec::new())
        }

        fn write(&mut self, value: u64, width: usize) {
            self.0.extend((0..width).map(|bit| value & (1 << bit) != 0));
        }

        fn finish(self) -> Vec<u8> {
            let mut packet = b"\x05vorbis".to_vec();
            let mut payload = vec![0; self.0.len().div_ceil(8)];
            for (offset, bit) in self.0.into_iter().enumerate() {
                if bit {
                    payload[offset / 8] |= 1 << (offset % 8);
                }
            }
            packet.extend(payload);
            packet
        }
    }

    fn minimal_setup(floor_type: u16, floor0_book: u8) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.write(0, 8); // One codebook.
        writer.write(0x56_43_42, 24);
        writer.write(1, 16);
        writer.write(2, 24);
        writer.write(0, 1); // Unordered.
        writer.write(0, 1); // Dense.
        writer.write(0, 5);
        writer.write(0, 5);
        writer.write(0, 4); // No lookup table.
        writer.write(0, 6); // One time transform.
        writer.write(0, 16);
        writer.write(0, 6); // One floor.
        writer.write(u64::from(floor_type), 16);
        if floor_type == 0 {
            writer.write(1, 8); // Order.
            writer.write(44_100, 16);
            writer.write(14, 16); // Bark map size.
            writer.write(1, 6); // Amplitude bits.
            writer.write(0, 8); // Amplitude offset.
            writer.write(0, 4); // One book.
            writer.write(u64::from(floor0_book), 8); // Floor-0 codebook.
        } else {
            writer.write(0, 5); // No partitions.
            writer.write(0, 2); // Multiplier 1.
            writer.write(0, 4); // Range bits 0.
        }
        writer.write(0, 6); // One residue.
        writer.write(0, 16); // Residue type 0.
        writer.write(0, 24); // Begin.
        writer.write(0, 24); // End.
        writer.write(0, 24); // Partition size 1.
        writer.write(0, 6); // One classification.
        writer.write(0, 8); // Classbook 0.
        writer.write(0, 3); // Cascade low bits.
        writer.write(0, 1); // No high cascade bits.
        writer.write(0, 6); // One mapping.
        writer.write(0, 16); // Mapping type 0.
        writer.write(0, 1); // One submap.
        writer.write(0, 1); // No coupling.
        writer.write(0, 2); // Reserved.
        writer.write(0, 8); // Submap time placeholder.
        writer.write(0, 8); // Floor 0.
        writer.write(0, 8); // Residue 0.
        writer.write(0, 6); // One mode.
        writer.write(0, 1); // Short block.
        writer.write(0, 16); // Window type.
        writer.write(0, 16); // Transform type.
        writer.write(0, 8); // Mapping 0.
        writer.write(1, 1); // Framing.
        writer.finish()
    }

    fn identification(channels: u8) -> VorbisIdentificationHeader {
        let mut packet = [0; 30];
        packet[..7].copy_from_slice(b"\x01vorbis");
        packet[11] = channels;
        packet[12..16].copy_from_slice(&44_100_u32.to_le_bytes());
        packet[28] = 0x66;
        packet[29] = 1;
        VorbisIdentificationHeader::parse(&packet).unwrap()
    }

    #[test]
    fn parses_complete_minimal_setup_with_both_floor_types() {
        for floor_type in [0, 1] {
            let parsed =
                VorbisSetupHeader::parse(&minimal_setup(floor_type, 0), &identification(2))
                    .unwrap();
            assert_eq!(parsed.codebooks.len(), 1);
            assert_eq!(parsed.floors.len(), 1);
            assert_eq!(parsed.residues.len(), 1);
            assert_eq!(parsed.mappings.len(), 1);
            assert_eq!(parsed.modes.len(), 1);
            assert!(!parsed.modes[0].long_block);
        }
    }

    #[test]
    fn rejects_out_of_range_codebook_references_and_truncated_headers() {
        let valid = minimal_setup(1, 0);
        let mut missing_framing = valid.clone();
        *missing_framing.last_mut().unwrap() = 0;
        assert!(VorbisSetupHeader::parse(&missing_framing, &identification(2)).is_err());
        assert!(VorbisSetupHeader::parse(&valid[..valid.len() - 1], &identification(2)).is_err());

        let invalid_reference = minimal_setup(0, 1);
        assert!(VorbisSetupHeader::parse(&invalid_reference, &identification(2)).is_err());
    }
}
