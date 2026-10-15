use std::{
    collections::HashSet,
    io::{Read, Write},
};

use zemer_core::AudioError;

const FIXED_SIZE: u32 = 36;
const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;

/// One 24-byte loop descriptor in a WAVE `smpl` chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WavSampleLoop {
    pub identifier: u32,
    pub loop_type: u32,
    pub start: u32,
    pub end: u32,
    pub fraction: u32,
    pub play_count: u32,
}

/// Fixed WAVE sampler fields, loop descriptors, and opaque sampler-specific data.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WavSamplerChunk {
    pub manufacturer: u32,
    pub product: u32,
    pub sample_period: u32,
    pub midi_unity_note: u32,
    pub midi_pitch_fraction: u32,
    pub smpte_format: u32,
    pub smpte_offset: u32,
    pub loops: Vec<WavSampleLoop>,
    pub sampler_data: Vec<u8>,
}

impl WavSamplerChunk {
    pub(crate) fn read<R: Read>(reader: &mut R, size: u32) -> Result<Self, AudioError> {
        if !(FIXED_SIZE..=MAX_CHUNK_SIZE).contains(&size) {
            return Err(AudioError::InvalidFormat("invalid WAVE sampler chunk size"));
        }
        let mut header = [0; FIXED_SIZE as usize];
        reader.read_exact(&mut header)?;
        let loop_count = u32::from_le_bytes(header[28..32].try_into().unwrap());
        let sampler_data_bytes = u32::from_le_bytes(header[32..36].try_into().unwrap());
        let expected =
            u64::from(FIXED_SIZE) + u64::from(loop_count) * 24 + u64::from(sampler_data_bytes);
        if expected != u64::from(size) {
            return Err(AudioError::InvalidFormat(
                "WAVE sampler counts do not match chunk size",
            ));
        }
        let mut loops = Vec::new();
        let loop_count_usize = usize::try_from(loop_count)
            .map_err(|_| AudioError::InvalidFormat("WAVE sample loop count is too large"))?;
        loops
            .try_reserve_exact(loop_count_usize)
            .map_err(|_| AudioError::InvalidFormat("WAVE sample loop list is too large"))?;
        let mut identifiers = HashSet::new();
        identifiers
            .try_reserve(loop_count_usize)
            .map_err(|_| AudioError::InvalidFormat("WAVE sample loop list is too large"))?;
        for _ in 0..loop_count {
            let mut bytes = [0; 24];
            reader.read_exact(&mut bytes)?;
            let identifier = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
            if !identifiers.insert(identifier) {
                return Err(AudioError::InvalidFormat(
                    "duplicate WAVE sample loop identifier",
                ));
            }
            loops.push(WavSampleLoop {
                identifier,
                loop_type: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                start: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
                end: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
                fraction: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
                play_count: u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
            });
        }
        let mut sampler_data = Vec::new();
        sampler_data
            .try_reserve_exact(sampler_data_bytes as usize)
            .map_err(|_| AudioError::InvalidFormat("WAVE sampler data is too large"))?;
        sampler_data.resize(sampler_data_bytes as usize, 0);
        reader.read_exact(&mut sampler_data)?;
        Ok(Self {
            manufacturer: u32::from_le_bytes(header[0..4].try_into().unwrap()),
            product: u32::from_le_bytes(header[4..8].try_into().unwrap()),
            sample_period: u32::from_le_bytes(header[8..12].try_into().unwrap()),
            midi_unity_note: u32::from_le_bytes(header[12..16].try_into().unwrap()),
            midi_pitch_fraction: u32::from_le_bytes(header[16..20].try_into().unwrap()),
            smpte_format: u32::from_le_bytes(header[20..24].try_into().unwrap()),
            smpte_offset: u32::from_le_bytes(header[24..28].try_into().unwrap()),
            loops,
            sampler_data,
        })
    }

    pub(crate) fn chunk_size(&self) -> Result<u32, AudioError> {
        let loops = u32::try_from(self.loops.len())
            .map_err(|_| AudioError::Unsupported("too many WAVE sample loops"))?;
        let data = u32::try_from(self.sampler_data.len())
            .map_err(|_| AudioError::Unsupported("WAVE sampler data is too large"))?;
        let size = u64::from(FIXED_SIZE) + u64::from(loops) * 24 + u64::from(data);
        if size > u64::from(MAX_CHUNK_SIZE) {
            return Err(AudioError::Unsupported(
                "WAVE sampler chunk exceeds the 16 MiB limit",
            ));
        }
        let mut identifiers = HashSet::new();
        identifiers
            .try_reserve(self.loops.len())
            .map_err(|_| AudioError::Unsupported("WAVE sample loop list is too large"))?;
        if self
            .loops
            .iter()
            .any(|item| !identifiers.insert(item.identifier))
        {
            return Err(AudioError::InvalidData(
                "duplicate WAVE sample loop identifier",
            ));
        }
        Ok(size as u32)
    }

    pub(crate) fn write<W: Write>(&self, writer: &mut W) -> Result<(), AudioError> {
        let size = self.chunk_size()?;
        writer.write_all(b"smpl")?;
        writer.write_all(&size.to_le_bytes())?;
        for value in [
            self.manufacturer,
            self.product,
            self.sample_period,
            self.midi_unity_note,
            self.midi_pitch_fraction,
            self.smpte_format,
            self.smpte_offset,
            self.loops.len() as u32,
            self.sampler_data.len() as u32,
        ] {
            writer.write_all(&value.to_le_bytes())?;
        }
        for item in &self.loops {
            for value in [
                item.identifier,
                item.loop_type,
                item.start,
                item.end,
                item.fraction,
                item.play_count,
            ] {
                writer.write_all(&value.to_le_bytes())?;
            }
        }
        writer.write_all(&self.sampler_data)?;
        if size & 1 != 0 {
            writer.write_all(&[0])?;
        }
        Ok(())
    }
}
