use zemer_core::AudioError;

use crate::{bitstream::BitReader, metadata::FlacStreamInfo};

pub(crate) fn read_frame<R: std::io::Read>(
    reader: &mut BitReader<R>,
    stream: &FlacStreamInfo,
) -> Result<Option<(Vec<i64>, usize, u64)>, AudioError> {
    reader.begin_frame()?;
    let Some(sync) = reader.read_bits_or_eof(16)? else {
        return Ok(None);
    };
    if sync >> 2 != 0x3ffe || sync & 2 != 0 {
        return Err(AudioError::InvalidData(
            "invalid FLAC frame sync or reserved bit",
        ));
    }
    let variable_blocking = sync & 1 != 0;
    let block_code = reader.read_bits(4)? as u8;
    let rate_code = reader.read_bits(4)? as u8;
    let channel_assignment = reader.read_bits(4)? as u8;
    let sample_bits_code = reader.read_bits(3)? as u8;
    if reader.read_bits(1)? != 0 {
        return Err(AudioError::InvalidData("non-zero FLAC frame reserved bit"));
    }
    let coded_number = read_utf8_number(reader)?;
    let frame_start = if variable_blocking {
        coded_number
    } else {
        coded_number
            .checked_mul(u64::from(stream.max_block_size()))
            .ok_or(AudioError::InvalidData("FLAC frame sample number overflow"))?
    };
    let block_size = match block_code {
        0 => return Err(AudioError::InvalidData("reserved FLAC block-size code")),
        1 => 192,
        2..=5 => 576_usize << (block_code - 2),
        6 => reader.read_bits(8)? as usize + 1,
        7 => reader.read_bits(16)? as usize + 1,
        8..=15 => 256_usize << (block_code - 8),
        _ => unreachable!(),
    };
    if block_size == 0 || block_size > usize::from(stream.max_block_size()) {
        return Err(AudioError::InvalidData(
            "FLAC frame block size exceeds STREAMINFO",
        ));
    }
    let sample_rate = match rate_code {
        0 => stream.sample_rate(),
        1 => 88_200,
        2 => 176_400,
        3 => 192_000,
        4 => 8_000,
        5 => 16_000,
        6 => 22_050,
        7 => 24_000,
        8 => 32_000,
        9 => 44_100,
        10 => 48_000,
        11 => 96_000,
        12 => reader.read_bits(8)? as u32 * 1_000,
        13 => reader.read_bits(16)? as u32,
        14 => reader.read_bits(16)? as u32 * 10,
        _ => return Err(AudioError::InvalidData("reserved FLAC sample-rate code")),
    };
    let channels = match channel_assignment {
        0..=7 => channel_assignment + 1,
        8..=10 => 2,
        _ => return Err(AudioError::InvalidData("reserved FLAC channel assignment")),
    };
    let bits_per_sample = match sample_bits_code {
        0 => stream.bits_per_sample(),
        1 => 8,
        2 => 12,
        3 => return Err(AudioError::InvalidData("reserved FLAC sample-size code")),
        4 => 16,
        5 => 20,
        6 => 24,
        7 => 32,
        _ => unreachable!(),
    };
    if sample_rate != stream.sample_rate()
        || channels != stream.channels()
        || bits_per_sample != stream.bits_per_sample()
    {
        return Err(AudioError::InvalidData(
            "FLAC frame properties differ from STREAMINFO",
        ));
    }
    let header_crc = reader.header_crc8();
    reader.stop_header_crc();
    if reader.read_bits(8)? as u8 != header_crc {
        return Err(AudioError::InvalidData("FLAC frame header CRC-8 mismatch"));
    }

    let mut subframes = Vec::with_capacity(usize::from(channels));
    for channel in 0..channels {
        let side_channel = matches!(channel_assignment, 8..=10)
            && ((channel_assignment == 8 && channel == 1)
                || (channel_assignment == 9 && channel == 0)
                || (channel_assignment == 10 && channel == 1));
        let subframe_bits = bits_per_sample + u8::from(side_channel);
        subframes.push(read_subframe(reader, block_size, subframe_bits)?);
    }
    reader.align_zero()?;
    let frame_crc = reader.frame_crc16();
    reader.stop_frame_crc();
    if reader.read_bits(16)? as u16 != frame_crc {
        return Err(AudioError::InvalidData("FLAC frame CRC-16 mismatch"));
    }
    if stream
        .max_frame_size()
        .is_some_and(|maximum| reader.frame_bytes().saturating_add(2) > maximum as usize)
    {
        return Err(AudioError::InvalidData(
            "FLAC frame exceeds STREAMINFO maximum",
        ));
    }

    if channel_assignment >= 8 {
        restore_stereo(&mut subframes, channel_assignment)?;
    }
    let channel_count = usize::from(channels);
    let sample_count = block_size
        .checked_mul(channel_count)
        .ok_or(AudioError::InvalidData("FLAC block sample count overflow"))?;
    let mut interleaved = Vec::with_capacity(sample_count);
    for sample in 0..block_size {
        for channel in &subframes {
            let value = channel[sample];
            let minimum = -(1_i64 << (bits_per_sample - 1));
            let maximum = (1_i64 << (bits_per_sample - 1)) - 1;
            if !(minimum..=maximum).contains(&value) {
                return Err(AudioError::InvalidData(
                    "FLAC sample exceeds STREAMINFO bit depth",
                ));
            }
            interleaved.push(value);
        }
    }
    Ok(Some((interleaved, block_size, frame_start)))
}

fn read_utf8_number<R: std::io::Read>(reader: &mut BitReader<R>) -> Result<u64, AudioError> {
    let first = reader.read_bits(8)? as u8;
    if first & 0x80 == 0 {
        return Ok(u64::from(first));
    }
    let leading_ones = first.leading_ones() as u8;
    if !(2..=7).contains(&leading_ones) {
        return Err(AudioError::InvalidData("invalid FLAC UTF-8 coded number"));
    }
    let mut value = u64::from(first & (0x7f >> leading_ones));
    for _ in 1..leading_ones {
        let byte = reader.read_bits(8)? as u8;
        if byte & 0xc0 != 0x80 {
            return Err(AudioError::InvalidData(
                "invalid FLAC UTF-8 continuation byte",
            ));
        }
        value = (value << 6) | u64::from(byte & 0x3f);
    }
    Ok(value)
}

fn read_subframe<R: std::io::Read>(
    reader: &mut BitReader<R>,
    block_size: usize,
    bits_per_sample: u8,
) -> Result<Vec<i64>, AudioError> {
    if reader.read_bits(1)? != 0 {
        return Err(AudioError::InvalidData(
            "non-zero FLAC subframe padding bit",
        ));
    }
    let kind = reader.read_bits(6)? as u8;
    let wasted = if reader.read_bits(1)? != 0 {
        reader.read_unary(u64::from(bits_per_sample - 1))? as u8 + 1
    } else {
        0
    };
    if wasted >= bits_per_sample {
        return Err(AudioError::InvalidData("invalid FLAC wasted-bits count"));
    }
    let coded_bits = bits_per_sample - wasted;
    let mut samples = match kind {
        0 => vec![reader.read_signed(coded_bits)?; block_size],
        1 => read_verbatim(reader, block_size, coded_bits)?,
        8..=12 => {
            let order = usize::from(kind - 8);
            read_predictive(reader, block_size, coded_bits, order)?
        }
        32..=63 => {
            let order = usize::from(kind - 31);
            if order > block_size {
                return Err(AudioError::InvalidData("FLAC LPC order exceeds block size"));
            }
            let mut samples = read_warmup(reader, order, coded_bits)?;
            let precision = reader.read_bits(4)? as u8 + 1;
            if precision == 16 {
                return Err(AudioError::InvalidData("reserved FLAC LPC precision"));
            }
            let shift = reader.read_signed(5)?;
            if shift < 0 {
                return Err(AudioError::InvalidData("negative FLAC LPC shift"));
            }
            let mut coefficients = Vec::with_capacity(order);
            for _ in 0..order {
                coefficients.push(reader.read_signed(precision)?);
            }
            samples.extend(read_residual(reader, block_size - order, order)?);
            for index in order..block_size {
                let prediction = coefficients
                    .iter()
                    .enumerate()
                    .try_fold(0_i64, |sum, (coefficient_index, coefficient)| {
                        sum.checked_add(
                            coefficient.checked_mul(samples[index - coefficient_index - 1])?,
                        )
                    })
                    .ok_or(AudioError::InvalidData("FLAC LPC prediction overflow"))?
                    >> shift;
                samples[index] = samples[index]
                    .checked_add(prediction)
                    .ok_or(AudioError::InvalidData("FLAC LPC prediction overflow"))?;
            }
            samples
        }
        _ => return Err(AudioError::Unsupported("reserved FLAC subframe type")),
    };
    if wasted != 0 {
        for sample in &mut samples {
            *sample = sample
                .checked_mul(1_i64 << wasted)
                .ok_or(AudioError::InvalidData("FLAC wasted-bits shift overflow"))?;
        }
    }
    Ok(samples)
}

fn read_verbatim<R: std::io::Read>(
    reader: &mut BitReader<R>,
    count: usize,
    bits: u8,
) -> Result<Vec<i64>, AudioError> {
    (0..count).map(|_| reader.read_signed(bits)).collect()
}

fn read_warmup<R: std::io::Read>(
    reader: &mut BitReader<R>,
    count: usize,
    bits: u8,
) -> Result<Vec<i64>, AudioError> {
    read_verbatim(reader, count, bits)
}

fn read_predictive<R: std::io::Read>(
    reader: &mut BitReader<R>,
    block_size: usize,
    bits: u8,
    order: usize,
) -> Result<Vec<i64>, AudioError> {
    if order > block_size {
        return Err(AudioError::InvalidData(
            "FLAC predictor order exceeds block size",
        ));
    }
    let mut samples = read_warmup(reader, order, bits)?;
    samples.extend(read_residual(reader, block_size - order, order)?);
    for index in order..block_size {
        let prediction = fixed_prediction(&samples, index, order)
            .ok_or(AudioError::InvalidData("FLAC fixed predictor overflow"))?;
        samples[index] = prediction
            .checked_add(samples[index])
            .ok_or(AudioError::InvalidData("FLAC predictor overflow"))?;
    }
    Ok(samples)
}

pub(crate) fn fixed_prediction(samples: &[i64], index: usize, order: usize) -> Option<i64> {
    match order {
        0 => Some(0),
        1 => Some(samples[index - 1]),
        2 => samples[index - 1]
            .checked_mul(2)?
            .checked_sub(samples[index - 2]),
        3 => samples[index - 1]
            .checked_mul(3)?
            .checked_sub(samples[index - 2].checked_mul(3)?)?
            .checked_add(samples[index - 3]),
        4 => samples[index - 1]
            .checked_mul(4)?
            .checked_sub(samples[index - 2].checked_mul(6)?)?
            .checked_add(samples[index - 3].checked_mul(4)?)?
            .checked_sub(samples[index - 4]),
        _ => unreachable!(),
    }
}

fn read_residual<R: std::io::Read>(
    reader: &mut BitReader<R>,
    count: usize,
    predictor_order: usize,
) -> Result<Vec<i64>, AudioError> {
    let method = reader.read_bits(2)? as u8;
    let parameter_bits = match method {
        0 => 4,
        1 => 5,
        _ => {
            return Err(AudioError::InvalidData(
                "reserved FLAC residual coding method",
            ));
        }
    };
    let partition_order = reader.read_bits(4)? as u8;
    let partitions = 1_usize << partition_order;
    let partitioned_count = count
        .checked_add(predictor_order)
        .ok_or(AudioError::InvalidData("FLAC residual count overflow"))?;
    if partitioned_count % partitions != 0 {
        return Err(AudioError::InvalidData(
            "invalid FLAC residual partition size",
        ));
    }
    let partition_size = partitioned_count / partitions;
    if partition_size < predictor_order {
        return Err(AudioError::InvalidData(
            "FLAC residual partition smaller than predictor",
        ));
    }
    let mut residual = Vec::with_capacity(count);
    for partition in 0..partitions {
        let number = if partition == 0 {
            partition_size - predictor_order
        } else {
            partition_size
        };
        let parameter = reader.read_bits(parameter_bits)? as u8;
        if parameter == (1 << parameter_bits) - 1 {
            let raw_bits = reader.read_bits(5)? as u8;
            for _ in 0..number {
                residual.push(if raw_bits == 0 {
                    0
                } else {
                    reader.read_signed(raw_bits)?
                });
            }
        } else {
            for _ in 0..number {
                let quotient = reader.read_unary(u64::MAX >> parameter)?;
                let remainder = if parameter == 0 {
                    0
                } else {
                    reader.read_bits(parameter)?
                };
                if quotient > (u64::MAX >> parameter) {
                    return Err(AudioError::InvalidData("FLAC Rice residual overflow"));
                }
                let unsigned = quotient
                    .checked_shl(u32::from(parameter))
                    .and_then(|value| value.checked_add(remainder))
                    .ok_or(AudioError::InvalidData("FLAC Rice residual overflow"))?;
                let half = (unsigned >> 1) as i64;
                residual.push(if unsigned & 1 == 0 { half } else { -half - 1 });
            }
        }
    }
    if residual.len() != count {
        return Err(AudioError::InvalidData("FLAC residual count mismatch"));
    }
    Ok(residual)
}

fn restore_stereo(subframes: &mut [Vec<i64>], assignment: u8) -> Result<(), AudioError> {
    if subframes.len() != 2 || subframes[0].len() != subframes[1].len() {
        return Err(AudioError::InvalidData(
            "invalid FLAC stereo subframe shape",
        ));
    }
    let (left_channel, right_channel) = subframes.split_at_mut(1);
    for (left, right) in left_channel[0].iter_mut().zip(&mut right_channel[0]) {
        let (restored_left, restored_right) = match assignment {
            8 => (Some(*left), left.checked_sub(*right)),
            9 => (left.checked_add(*right), Some(*right)),
            10 => {
                let mid = left
                    .checked_mul(2)
                    .and_then(|value| value.checked_add(*right & 1))
                    .ok_or(AudioError::InvalidData("FLAC mid-side overflow"))?;
                (
                    mid.checked_add(*right).map(|value| value >> 1),
                    mid.checked_sub(*right).map(|value| value >> 1),
                )
            }
            _ => unreachable!(),
        };
        *left = restored_left.ok_or(AudioError::InvalidData(
            "FLAC stereo reconstruction overflow",
        ))?;
        *right = restored_right.ok_or(AudioError::InvalidData(
            "FLAC stereo reconstruction overflow",
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::fixed_prediction;

    #[test]
    fn fixed_predictor_reports_overflow_instead_of_panicking() {
        assert_eq!(fixed_prediction(&[0, i64::MAX, 0], 2, 2), None);
        assert_eq!(fixed_prediction(&[0, 4, 3, 2, 1], 4, 4), Some(6));
    }
}
