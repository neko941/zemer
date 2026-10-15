use std::{collections::HashSet, io::{Read, Write}};

use zemer_core::AudioError;

const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;

/// One 24-byte entry in a WAVE `cue ` chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WavCuePoint {
    pub name: u32,
    pub position: u32,
    pub chunk_id: [u8; 4],
    pub chunk_start: u32,
    pub block_start: u32,
    pub sample_offset: u32,
}

/// Cue points from one WAVE `cue ` chunk, including an explicitly empty chunk.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WavCueChunk {
    pub points: Vec<WavCuePoint>,
}

impl WavCueChunk {
    pub(crate) fn read<R: Read>(reader: &mut R, size: u32) -> Result<Self, AudioError> {
        if !(4..=MAX_CHUNK_SIZE).contains(&size) {
            return Err(AudioError::InvalidFormat("invalid WAVE cue chunk size"));
        }
        let mut count = [0; 4];
        reader.read_exact(&mut count)?;
        let count = u32::from_le_bytes(count);
        let expected = 4_u64 + u64::from(count) * 24;
        if expected != u64::from(size) {
            return Err(AudioError::InvalidFormat(
                "WAVE cue point count does not match chunk size",
            ));
        }
        let mut points = Vec::new();
        let count = usize::try_from(count)
            .map_err(|_| AudioError::InvalidFormat("WAVE cue point count is too large"))?;
        points.try_reserve_exact(count).map_err(|_| {
            AudioError::InvalidFormat("WAVE cue point list is too large")
        })?;
        let mut names = HashSet::new();
        names.try_reserve(count).map_err(|_| {
            AudioError::InvalidFormat("WAVE cue point list is too large")
        })?;
        for _ in 0..count {
            let mut bytes = [0; 24];
            reader.read_exact(&mut bytes)?;
            let name = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
            if !names.insert(name) {
                return Err(AudioError::InvalidFormat("duplicate WAVE cue point name"));
            }
            points.push(WavCuePoint {
                name,
                position: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                chunk_id: bytes[8..12].try_into().unwrap(),
                chunk_start: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
                block_start: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
                sample_offset: u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
            });
        }
        Ok(Self { points })
    }

    pub(crate) fn chunk_size(&self) -> Result<u32, AudioError> {
        let count = u32::try_from(self.points.len())
            .map_err(|_| AudioError::Unsupported("too many WAVE cue points"))?;
        let size = 4_u64 + u64::from(count) * 24;
        if size > u64::from(MAX_CHUNK_SIZE) {
            return Err(AudioError::Unsupported("WAVE cue chunk exceeds the 16 MiB limit"));
        }
        let mut names = HashSet::new();
        names
            .try_reserve(self.points.len())
            .map_err(|_| AudioError::Unsupported("WAVE cue point list is too large"))?;
        if self.points.iter().any(|point| !names.insert(point.name)) {
            return Err(AudioError::InvalidData("duplicate WAVE cue point name"));
        }
        Ok(size as u32)
    }

    pub(crate) fn write<W: Write>(&self, writer: &mut W) -> Result<(), AudioError> {
        let size = self.chunk_size()?;
        writer.write_all(b"cue ")?;
        writer.write_all(&size.to_le_bytes())?;
        writer.write_all(&(self.points.len() as u32).to_le_bytes())?;
        for point in &self.points {
            writer.write_all(&point.name.to_le_bytes())?;
            writer.write_all(&point.position.to_le_bytes())?;
            writer.write_all(&point.chunk_id)?;
            writer.write_all(&point.chunk_start.to_le_bytes())?;
            writer.write_all(&point.block_start.to_le_bytes())?;
            writer.write_all(&point.sample_offset.to_le_bytes())?;
        }
        Ok(())
    }
}
