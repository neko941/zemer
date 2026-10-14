use std::collections::HashSet;
use std::io::Write;

use zemer_core::AudioError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiffMarker {
    /// A unique, nonzero identifier within the AIFF or AIFC file.
    pub id: i16,
    /// Sample-frame position, between frames; `0` is before the first frame.
    pub position: u32,
    /// Marker name bytes, retained without assuming a character encoding.
    pub name: Vec<u8>,
}

pub(super) fn parse_markers(bytes: &[u8]) -> Result<Vec<AiffMarker>, AudioError> {
    if bytes.len() < 2 {
        return Err(AudioError::InvalidFormat("AIFF MARK chunk is too short"));
    }
    let count = usize::from(u16::from_be_bytes(bytes[..2].try_into().unwrap()));
    let mut markers = Vec::new();
    markers
        .try_reserve_exact(count)
        .map_err(|_| AudioError::InvalidFormat("AIFF MARK count is too large"))?;
    let mut offset = 2_usize;
    for _ in 0..count {
        let header_end = offset
            .checked_add(7)
            .filter(|end| *end <= bytes.len())
            .ok_or(AudioError::InvalidFormat("truncated AIFF MARK record"))?;
        let id = i16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap());
        let position = u32::from_be_bytes(bytes[offset + 2..offset + 6].try_into().unwrap());
        let name_len = usize::from(bytes[offset + 6]);
        let padded_name_len = (name_len + 1 + 1) & !1;
        let end = header_end
            .checked_add(padded_name_len - 1)
            .filter(|end| *end <= bytes.len())
            .ok_or(AudioError::InvalidFormat("truncated AIFF MARK name"))?;
        let mut name = Vec::new();
        name.try_reserve_exact(name_len)
            .map_err(|_| AudioError::InvalidFormat("AIFF MARK name is too large"))?;
        name.extend_from_slice(&bytes[header_end..header_end + name_len]);
        markers.push(AiffMarker { id, position, name });
        offset = end;
    }
    if offset != bytes.len() {
        return Err(AudioError::InvalidFormat(
            "AIFF MARK count does not match its records",
        ));
    }
    validate_markers(&markers)?;
    Ok(markers)
}

pub(super) fn validate_markers(markers: &[AiffMarker]) -> Result<u64, AudioError> {
    if markers.len() > usize::from(u16::MAX) {
        return Err(AudioError::Unsupported("AIFF MARK has too many markers"));
    }
    let mut ids = HashSet::new();
    ids.try_reserve(markers.len())
        .map_err(|_| AudioError::InvalidData("AIFF MARK is too large"))?;
    markers.iter().try_fold(2_u64, |size, marker| {
        if marker.id <= 0 || marker.name.len() > usize::from(u8::MAX) || !ids.insert(marker.id) {
            return Err(AudioError::InvalidData("invalid or duplicate AIFF marker"));
        }
        size.checked_add(6 + 1 + marker.name.len() as u64 + ((marker.name.len() + 1) & 1) as u64)
            .ok_or(AudioError::InvalidData("AIFF MARK size overflow"))
    })
}

pub(super) fn write_markers_chunk(
    writer: &mut impl Write,
    markers: &[AiffMarker],
) -> Result<u64, AudioError> {
    let body_size = validate_markers(markers)?;
    if markers.is_empty() {
        return Ok(0);
    }
    writer.write_all(b"MARK")?;
    writer.write_all(&(body_size as u32).to_be_bytes())?;
    writer.write_all(&(markers.len() as u16).to_be_bytes())?;
    for marker in markers {
        writer.write_all(&marker.id.to_be_bytes())?;
        writer.write_all(&marker.position.to_be_bytes())?;
        writer.write_all(&[marker.name.len() as u8])?;
        writer.write_all(&marker.name)?;
        if (marker.name.len() + 1) & 1 != 0 {
            writer.write_all(&[0])?;
        }
    }
    if body_size & 1 != 0 {
        writer.write_all(&[0])?;
    }
    Ok(body_size + 8 + (body_size & 1))
}
