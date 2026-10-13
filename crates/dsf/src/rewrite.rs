use std::io::{Read, Seek, SeekFrom, Write};

use zemer_core::{AudioError, Metadata};

use crate::{decode::DsfDecoder, id3};

const DSF_DATA_HEADER_END: u64 = 92;

/// Rewrites a DSF ID3 tag while copying the encoded DSD payload unchanged.
pub fn rewrite_metadata<R: Read + Seek, W: Write>(
    mut input: R,
    output: &mut W,
    metadata: &Metadata,
) -> Result<(), AudioError> {
    let decoder = DsfDecoder::new(&mut input)?;
    let old_tag = decoder.id3_tag().map(<[u8]>::to_vec);
    drop(decoder);

    input.seek(SeekFrom::Start(80))?;
    let mut data_header = [0; 12];
    input.read_exact(&mut data_header)?;
    let data_size = u64::from_le_bytes(data_header[4..12].try_into().unwrap());
    let data_end = 80_u64
        .checked_add(data_size)
        .filter(|&end| end >= DSF_DATA_HEADER_END)
        .ok_or(AudioError::InvalidFormat("invalid DSF data size"))?;
    let tag = match old_tag {
        Some(tag) => id3::rewrite_metadata_tag(&tag, metadata)?,
        None => id3::encode_metadata(metadata)?,
    };
    let has_tag = !tag.is_empty();
    let file_size = data_end
        .checked_add(if has_tag { tag.len() as u64 } else { 0 })
        .ok_or(AudioError::InvalidData("DSF file size overflow"))?;

    input.seek(SeekFrom::Start(0))?;
    let mut header = [0; 28];
    input.read_exact(&mut header)?;
    header[12..20].copy_from_slice(&file_size.to_le_bytes());
    header[20..28].copy_from_slice(&(if has_tag { data_end } else { 0 }).to_le_bytes());
    output.write_all(&header)?;
    input.seek(SeekFrom::Start(28))?;
    let mut payload = (&mut input).take(data_end - 28);
    let copied = std::io::copy(&mut payload, output)?;
    if copied != data_end - 28 {
        return Err(AudioError::InvalidFormat("truncated DSF audio data"));
    }
    if has_tag {
        output.write_all(&tag)?;
    }
    Ok(())
}
