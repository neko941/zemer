use std::io::{Read, Write};

use zemer_core::AudioError;

const FIXED_SIZE: usize = 602;
const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;

/// Raw fixed-width fields from the BWF `bext` chunk.
///
/// Byte arrays preserve non-UTF-8 source values and opaque versioned fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BextChunk {
    pub description: [u8; 256],
    pub originator: [u8; 32],
    pub originator_reference: [u8; 32],
    pub origination_date: [u8; 10],
    pub origination_time: [u8; 8],
    pub time_reference: u64,
    pub version: u16,
    pub umid: [u8; 64],
    pub reserved: [u8; 190],
    pub coding_history: Vec<u8>,
}

impl Default for BextChunk {
    fn default() -> Self {
        Self {
            description: [0; 256],
            originator: [0; 32],
            originator_reference: [0; 32],
            origination_date: [0; 10],
            origination_time: [0; 8],
            time_reference: 0,
            version: 0,
            umid: [0; 64],
            reserved: [0; 190],
            coding_history: Vec::new(),
        }
    }
}

impl BextChunk {
    pub(crate) fn read<R: Read>(reader: &mut R, size: u32) -> Result<Self, AudioError> {
        if size < FIXED_SIZE as u32 {
            return Err(AudioError::InvalidFormat(
                "BWF bext chunk is shorter than 602 bytes",
            ));
        }
        if size > MAX_CHUNK_SIZE {
            return Err(AudioError::InvalidFormat(
                "BWF bext chunk exceeds the 16 MiB limit",
            ));
        }
        let mut chunk = Self::default();
        reader.read_exact(&mut chunk.description)?;
        reader.read_exact(&mut chunk.originator)?;
        reader.read_exact(&mut chunk.originator_reference)?;
        reader.read_exact(&mut chunk.origination_date)?;
        reader.read_exact(&mut chunk.origination_time)?;
        let mut time_reference = [0; 8];
        reader.read_exact(&mut time_reference)?;
        chunk.time_reference = u64::from_le_bytes(time_reference);
        let mut version = [0; 2];
        reader.read_exact(&mut version)?;
        chunk.version = u16::from_le_bytes(version);
        reader.read_exact(&mut chunk.umid)?;
        reader.read_exact(&mut chunk.reserved)?;
        chunk.coding_history.resize(size as usize - FIXED_SIZE, 0);
        reader.read_exact(&mut chunk.coding_history)?;
        Ok(chunk)
    }

    pub(crate) fn write<W: Write>(&self, writer: &mut W) -> Result<(), AudioError> {
        for bytes in [
            &self.description[..],
            &self.originator[..],
            &self.originator_reference[..],
            &self.origination_date[..],
            &self.origination_time[..],
        ] {
            writer.write_all(bytes)?;
        }
        writer.write_all(&self.time_reference.to_le_bytes())?;
        writer.write_all(&self.version.to_le_bytes())?;
        writer.write_all(&self.umid)?;
        writer.write_all(&self.reserved)?;
        writer.write_all(&self.coding_history)?;
        Ok(())
    }

    pub(crate) fn chunk_size(&self) -> Result<u32, AudioError> {
        let size = FIXED_SIZE
            .checked_add(self.coding_history.len())
            .and_then(|size| u32::try_from(size).ok())
            .ok_or(AudioError::Unsupported(
                "BWF bext chunk exceeds the RIFF size limit",
            ))?;
        if size > MAX_CHUNK_SIZE {
            return Err(AudioError::Unsupported(
                "BWF bext chunk exceeds the 16 MiB limit",
            ));
        }
        Ok(size)
    }
}
