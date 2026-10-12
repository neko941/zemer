use std::io::Cursor;

use zemer_core::AudioError;

use crate::{WavPackBlockHeader, metadata::WavPackMetadataSubBlockHeader};

const HAS_CHECKSUM: u32 = 1 << 28;
const BLOCK_CHECKSUM_ID: u8 = 0x2f;

pub(crate) fn verify_wvx_integer_checksum(wvx: &[u8], samples: &[i32]) -> Result<(), AudioError> {
    let expected = wvx_checksum_header(wvx)?;
    let mut checksum = u32::MAX;
    for &sample in samples {
        let sample = sample as u32;
        checksum = checksum
            .wrapping_mul(9)
            .wrapping_add((sample & 0xffff).wrapping_mul(3))
            .wrapping_add(sample >> 16);
    }
    verify_wvx_checksum(expected, checksum)
}

pub(crate) fn verify_wvx_float_checksum(wvx: &[u8], samples: &[f32]) -> Result<(), AudioError> {
    let expected = wvx_checksum_header(wvx)?;
    let mut checksum = u32::MAX;
    for sample in samples {
        let bits = sample.to_bits();
        checksum = checksum
            .wrapping_mul(27)
            .wrapping_add((bits & 0x7f_ffff).wrapping_mul(9))
            .wrapping_add(((bits >> 23) & 0xff).wrapping_mul(3))
            .wrapping_add(bits >> 31);
    }
    verify_wvx_checksum(expected, checksum)
}

fn wvx_checksum_header(wvx: &[u8]) -> Result<u32, AudioError> {
    let bytes: [u8; 4] = wvx
        .get(..4)
        .ok_or(AudioError::InvalidData("truncated WavPack WVX checksum"))?
        .try_into()
        .unwrap();
    Ok(u32::from_le_bytes(bytes))
}

fn verify_wvx_checksum(expected: u32, actual: u32) -> Result<(), AudioError> {
    if expected != actual {
        return Err(AudioError::InvalidData("WavPack WVX checksum mismatch"));
    }
    Ok(())
}

pub(crate) fn verify_sample_checksum(
    header: &WavPackBlockHeader,
    channels: usize,
    samples: &[i32],
) -> Result<(), AudioError> {
    if channels == 0 || samples.len() % channels != 0 {
        return Err(AudioError::InvalidData(
            "invalid decoded WavPack checksum sample count",
        ));
    }

    let mut checksum = u32::MAX;
    if channels == 1 {
        for sample in samples {
            checksum = checksum.wrapping_mul(3).wrapping_add(*sample as u32);
        }
    } else if channels == 2 {
        for pair in samples.chunks_exact(2) {
            checksum = checksum
                .wrapping_add(checksum.wrapping_shl(3))
                .wrapping_add((pair[0] as u32).wrapping_shl(1))
                .wrapping_add(pair[0] as u32)
                .wrapping_add(pair[1] as u32);
        }
    } else {
        return Err(AudioError::InvalidData(
            "WavPack block checksum requires a mono or stereo channel stream",
        ));
    }

    if checksum != header.crc {
        return Err(AudioError::InvalidData(
            "WavPack decoded-sample checksum mismatch",
        ));
    }
    Ok(())
}

pub(crate) fn verify_block_checksum(
    header: &WavPackBlockHeader,
    payload: &[u8],
) -> Result<(), AudioError> {
    if header.version < 0x410 {
        return Ok(());
    }

    let mut input = Cursor::new(payload);
    let mut remaining = payload.len() as u64;
    let mut found_checksum = false;
    loop {
        let sub_block_start = input.position();
        let Some(sub_block) = WavPackMetadataSubBlockHeader::read_from(&mut input, &mut remaining)?
        else {
            break;
        };
        let data_offset = usize::try_from(input.position())
            .map_err(|_| AudioError::InvalidData("WavPack checksum offset overflow"))?;
        if sub_block.id == BLOCK_CHECKSUM_ID {
            if found_checksum || sub_block.data_size != sub_block.padded_data_size {
                return Err(AudioError::InvalidData(
                    "invalid WavPack block checksum metadata",
                ));
            }
            let data_size = usize::try_from(sub_block.data_size)
                .map_err(|_| AudioError::InvalidData("WavPack checksum size overflow"))?;
            if !matches!(data_size, 2 | 4) {
                return Err(AudioError::InvalidData(
                    "invalid WavPack block checksum size",
                ));
            }
            let data_end = data_offset
                .checked_add(data_size)
                .ok_or(AudioError::InvalidData("WavPack checksum offset overflow"))?;
            let data = payload
                .get(data_offset..data_end)
                .ok_or(AudioError::InvalidData("truncated WavPack block checksum"))?;
            let prefix_end = usize::try_from(sub_block_start)
                .map_err(|_| AudioError::InvalidData("WavPack checksum offset overflow"))?;
            let checksum = checksum_before_sub_block(header, &payload[..prefix_end])?;
            let valid = if data_size == 4 {
                checksum == u32::from_le_bytes(data.try_into().unwrap())
            } else {
                (checksum ^ (checksum >> 16)) as u16 == u16::from_le_bytes(data.try_into().unwrap())
            };
            if !valid {
                return Err(AudioError::InvalidData("WavPack block checksum mismatch"));
            }
            found_checksum = true;
        }
        let data_end = u64::try_from(data_offset)
            .map_err(|_| AudioError::InvalidData("WavPack metadata offset overflow"))?
            .checked_add(sub_block.padded_data_size)
            .ok_or(AudioError::InvalidData("WavPack metadata offset overflow"))?;
        if data_end > payload.len() as u64 {
            return Err(AudioError::InvalidData(
                "WavPack metadata sub-block exceeds its block",
            ));
        }
        input.set_position(data_end);
    }

    if header.flags & HAS_CHECKSUM != 0 && !found_checksum {
        return Err(AudioError::InvalidData(
            "WavPack block declares a missing checksum",
        ));
    }
    Ok(())
}

fn checksum_before_sub_block(
    header: &WavPackBlockHeader,
    prefix: &[u8],
) -> Result<u32, AudioError> {
    let mut bytes = [0_u8; 32];
    bytes[..4].copy_from_slice(b"wvpk");
    let block_size = header
        .block_size
        .checked_sub(8)
        .and_then(|size| u32::try_from(size).ok())
        .ok_or(AudioError::InvalidData("invalid WavPack block size"))?;
    bytes[4..8].copy_from_slice(&block_size.to_le_bytes());
    bytes[8..10].copy_from_slice(&header.version.to_le_bytes());
    bytes[10] = header.block_index_high;
    bytes[11] = header.total_samples_high;
    bytes[12..16].copy_from_slice(&header.total_samples_low.to_le_bytes());
    bytes[16..20].copy_from_slice(&header.block_index_low.to_le_bytes());
    bytes[20..24].copy_from_slice(&header.block_samples.to_le_bytes());
    bytes[24..28].copy_from_slice(&header.flags.to_le_bytes());
    bytes[28..32].copy_from_slice(&header.crc.to_le_bytes());

    let mut checksum = u32::MAX;
    for word in bytes.chunks_exact(2).chain(prefix.chunks_exact(2)) {
        checksum = checksum
            .wrapping_mul(3)
            .wrapping_add(u32::from(u16::from_le_bytes([word[0], word[1]])));
    }
    Ok(checksum)
}
