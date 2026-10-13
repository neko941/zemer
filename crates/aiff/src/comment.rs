use std::io::Write;

use zemer_core::AudioError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiffComment {
    /// Seconds since 1904-01-01 (the Macintosh epoch), as stored by AIFF.
    pub timestamp: u32,
    /// Zero for an unassociated comment; otherwise a positive AIFF marker ID.
    pub marker_id: i16,
    /// Original comment bytes, retained without assuming a character encoding.
    pub text: Vec<u8>,
}

pub(super) fn parse_comments(bytes: &[u8]) -> Result<Vec<AiffComment>, AudioError> {
    if bytes.len() < 2 {
        return Err(AudioError::InvalidFormat("AIFF COMT chunk is too short"));
    }
    let count = usize::from(u16::from_be_bytes(bytes[..2].try_into().unwrap()));
    let mut offset = 2_usize;
    let mut comments = Vec::new();
    comments
        .try_reserve_exact(count)
        .map_err(|_| AudioError::InvalidFormat("AIFF COMT count is too large"))?;
    for _ in 0..count {
        let header_end = offset
            .checked_add(8)
            .filter(|end| *end <= bytes.len())
            .ok_or(AudioError::InvalidFormat("truncated AIFF COMT record"))?;
        let timestamp = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let marker_id = i16::from_be_bytes(bytes[offset + 4..offset + 6].try_into().unwrap());
        if marker_id < 0 {
            return Err(AudioError::InvalidFormat(
                "AIFF COMT marker ID must be zero or positive",
            ));
        }
        let text_len = usize::from(u16::from_be_bytes(
            bytes[offset + 6..header_end].try_into().unwrap(),
        ));
        let record_end = header_end
            .checked_add(text_len)
            .and_then(|end| end.checked_add(text_len & 1))
            .filter(|end| *end <= bytes.len())
            .ok_or(AudioError::InvalidFormat("truncated AIFF COMT text"))?;
        comments.push(AiffComment {
            timestamp,
            marker_id,
            text: bytes[header_end..header_end + text_len].to_vec(),
        });
        offset = record_end;
    }
    if offset != bytes.len() {
        return Err(AudioError::InvalidFormat(
            "AIFF COMT count does not match its records",
        ));
    }
    Ok(comments)
}

pub(super) fn validate_comments(comments: &[AiffComment]) -> Result<u64, AudioError> {
    if comments.len() > usize::from(u16::MAX) {
        return Err(AudioError::Unsupported("AIFF COMT has too many records"));
    }
    comments.iter().try_fold(2_u64, |size, comment| {
        if comment.marker_id < 0 {
            return Err(AudioError::InvalidData(
                "AIFF COMT marker ID must be zero or positive",
            ));
        }
        if comment.text.len() > usize::from(u16::MAX) {
            return Err(AudioError::Unsupported(
                "AIFF COMT text exceeds the 16-bit record limit",
            ));
        }
        size.checked_add(8 + comment.text.len() as u64 + (comment.text.len() & 1) as u64)
            .ok_or(AudioError::InvalidData("AIFF COMT size overflow"))
    })
}

pub(super) fn write_comments_chunk(
    writer: &mut impl Write,
    comments: &[AiffComment],
) -> Result<(), AudioError> {
    let body_size = validate_comments(comments)?;
    if comments.is_empty() {
        return Ok(());
    }
    writer.write_all(b"COMT")?;
    writer.write_all(&(body_size as u32).to_be_bytes())?;
    writer.write_all(&(comments.len() as u16).to_be_bytes())?;
    for comment in comments {
        writer.write_all(&comment.timestamp.to_be_bytes())?;
        writer.write_all(&comment.marker_id.to_be_bytes())?;
        writer.write_all(&(comment.text.len() as u16).to_be_bytes())?;
        writer.write_all(&comment.text)?;
        if comment.text.len() & 1 != 0 {
            writer.write_all(&[0])?;
        }
    }
    if body_size & 1 != 0 {
        writer.write_all(&[0])?;
    }
    Ok(())
}
