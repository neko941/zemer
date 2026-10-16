use std::collections::HashSet;
use std::io::Write;

use zemer_core::AudioError;

use crate::AiffMarker;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiffSaxel {
    /// Positive marker ID whose decompression accelerator this payload belongs to.
    pub marker_id: i16,
    /// Codec-specific accelerator bytes, retained without interpretation.
    pub data: Vec<u8>,
}

pub(super) fn parse_saxels(bytes: &[u8]) -> Result<Vec<AiffSaxel>, AudioError> {
    if bytes.len() < 2 {
        return Err(AudioError::InvalidFormat("AIFF SAXL chunk is too short"));
    }
    let count = usize::from(u16::from_be_bytes(bytes[..2].try_into().unwrap()));
    let mut saxels = Vec::new();
    saxels
        .try_reserve_exact(count)
        .map_err(|_| AudioError::InvalidFormat("AIFF SAXL count is too large"))?;
    let mut offset = 2_usize;
    for _ in 0..count {
        let header_end = offset
            .checked_add(4)
            .filter(|end| *end <= bytes.len())
            .ok_or(AudioError::InvalidFormat("truncated AIFF SAXL record"))?;
        let marker_id = i16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap());
        let size = usize::from(u16::from_be_bytes(
            bytes[offset + 2..header_end].try_into().unwrap(),
        ));
        if marker_id <= 0 {
            return Err(AudioError::InvalidFormat(
                "AIFF SAXL marker ID must be positive",
            ));
        }
        let padded_size = size + (size & 1);
        let end = header_end
            .checked_add(padded_size)
            .filter(|end| *end <= bytes.len())
            .ok_or(AudioError::InvalidFormat("truncated AIFF SAXL data"))?;
        let mut data = Vec::new();
        data.try_reserve_exact(size)
            .map_err(|_| AudioError::InvalidFormat("AIFF SAXL data is too large"))?;
        data.extend_from_slice(&bytes[header_end..header_end + size]);
        saxels.push(AiffSaxel { marker_id, data });
        offset = end;
    }
    if offset != bytes.len() {
        return Err(AudioError::InvalidFormat(
            "AIFF SAXL count does not match its records",
        ));
    }
    Ok(saxels)
}

pub(super) fn validate_saxels(saxels: &[AiffSaxel]) -> Result<u64, AudioError> {
    if saxels.len() > usize::from(u16::MAX) {
        return Err(AudioError::Unsupported("AIFF SAXL has too many records"));
    }
    saxels.iter().try_fold(2_u64, |size, saxel| {
        if saxel.marker_id <= 0 || saxel.data.len() > usize::from(u16::MAX) {
            return Err(AudioError::InvalidData("invalid AIFF SAXL record"));
        }
        size.checked_add(4 + saxel.data.len() as u64 + (saxel.data.len() & 1) as u64)
            .ok_or(AudioError::InvalidData("AIFF SAXL size overflow"))
    })
}

pub(super) fn validate_saxel_markers(
    saxels: &[AiffSaxel],
    markers: &[AiffMarker],
) -> Result<(), AudioError> {
    let mut marker_ids = HashSet::new();
    marker_ids
        .try_reserve(markers.len())
        .map_err(|_| AudioError::InvalidData("AIFF SAXL marker index is too large"))?;
    marker_ids.extend(markers.iter().map(|marker| marker.id));
    if saxels
        .iter()
        .any(|saxel| !marker_ids.contains(&saxel.marker_id))
    {
        return Err(AudioError::InvalidData(
            "AIFF SAXL record references a missing marker",
        ));
    }
    Ok(())
}

pub(super) fn write_saxels_chunk(
    writer: &mut impl Write,
    saxels: &[AiffSaxel],
) -> Result<u64, AudioError> {
    if saxels.is_empty() {
        return Ok(0);
    }
    let body_size = validate_saxels(saxels)?;
    let chunk_size = u32::try_from(body_size)
        .map_err(|_| AudioError::Unsupported("AIFF SAXL exceeds its 32-bit chunk limit"))?;
    writer.write_all(b"SAXL")?;
    writer.write_all(&chunk_size.to_be_bytes())?;
    writer.write_all(&(saxels.len() as u16).to_be_bytes())?;
    for saxel in saxels {
        writer.write_all(&saxel.marker_id.to_be_bytes())?;
        writer.write_all(&(saxel.data.len() as u16).to_be_bytes())?;
        writer.write_all(&saxel.data)?;
        if saxel.data.len() & 1 != 0 {
            writer.write_all(&[0])?;
        }
    }
    if body_size & 1 != 0 {
        writer.write_all(&[0])?;
    }
    Ok(8 + body_size + (body_size & 1))
}
