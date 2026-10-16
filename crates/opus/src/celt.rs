use crate::range::{RangeDecoder, RangeError};

const BAND_WIDTHS_2_5_MS: [u16; 21] = [
    1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 6, 6, 8, 12, 18, 22,
];

const STATIC_ALLOCATION: [[u8; 11]; 21] = [
    [0, 90, 110, 118, 126, 134, 144, 152, 162, 172, 200],
    [0, 80, 100, 110, 119, 127, 137, 145, 155, 165, 200],
    [0, 75, 90, 103, 112, 120, 130, 138, 148, 158, 200],
    [0, 69, 84, 93, 104, 114, 124, 132, 142, 152, 200],
    [0, 63, 78, 86, 95, 103, 113, 123, 133, 143, 200],
    [0, 56, 71, 80, 89, 97, 107, 117, 127, 137, 200],
    [0, 49, 65, 75, 83, 91, 101, 111, 121, 131, 200],
    [0, 40, 58, 70, 78, 85, 95, 105, 115, 125, 200],
    [0, 34, 51, 65, 72, 78, 88, 98, 108, 118, 198],
    [0, 29, 45, 59, 66, 72, 82, 92, 102, 112, 193],
    [0, 20, 39, 53, 60, 66, 76, 86, 96, 106, 188],
    [0, 18, 32, 47, 54, 60, 70, 80, 90, 100, 183],
    [0, 10, 26, 40, 47, 54, 64, 74, 84, 94, 178],
    [0, 0, 20, 31, 39, 47, 57, 67, 77, 87, 173],
    [0, 0, 12, 23, 32, 41, 51, 61, 71, 81, 168],
    [0, 0, 0, 15, 25, 35, 45, 55, 65, 75, 163],
    [0, 0, 0, 4, 17, 29, 39, 49, 59, 69, 158],
    [0, 0, 0, 0, 12, 23, 33, 43, 53, 63, 153],
    [0, 0, 0, 0, 1, 16, 26, 36, 46, 56, 148],
    [0, 0, 0, 0, 0, 10, 15, 20, 30, 45, 129],
    [0, 0, 0, 0, 0, 1, 1, 1, 1, 20, 104],
];

const ALLOCATION_CAPS: [[u8; 21]; 8] = [
    [
        224, 224, 224, 224, 224, 224, 224, 224, 160, 160, 160, 160, 185, 185, 185, 178, 178, 168,
        134, 61, 37,
    ],
    [
        224, 224, 224, 224, 224, 224, 224, 224, 240, 240, 240, 240, 207, 207, 207, 198, 198, 183,
        144, 66, 40,
    ],
    [
        160, 160, 160, 160, 160, 160, 160, 160, 185, 185, 185, 185, 193, 193, 193, 183, 183, 172,
        138, 64, 38,
    ],
    [
        240, 240, 240, 240, 240, 240, 240, 240, 207, 207, 207, 207, 204, 204, 204, 193, 193, 180,
        143, 66, 40,
    ],
    [
        185, 185, 185, 185, 185, 185, 185, 185, 193, 193, 193, 193, 193, 193, 193, 183, 183, 172,
        138, 65, 39,
    ],
    [
        207, 207, 207, 207, 207, 207, 207, 207, 204, 204, 204, 204, 201, 201, 201, 188, 188, 176,
        141, 66, 40,
    ],
    [
        193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 193, 194, 194, 194, 184, 184, 173,
        139, 65, 39,
    ],
    [
        204, 204, 204, 204, 204, 204, 204, 204, 201, 201, 201, 201, 198, 198, 198, 187, 187, 175,
        140, 66, 40,
    ],
];

const LOG2_FRAC_EIGHTHS: [u8; 21] = [
    0, 8, 12, 16, 18, 20, 22, 24, 25, 26, 27, 28, 29, 30, 31, 32, 32, 33, 33, 34, 35,
];

const BAND_LOG2_EIGHTHS: [u8; 21] = [
    0, 0, 0, 0, 0, 0, 0, 0, 8, 8, 8, 8, 16, 16, 16, 21, 21, 24, 29, 34, 36,
];
const MAX_FINE_ENERGY_BITS: u8 = 8;
const FINE_ENERGY_OFFSET_EIGHTHS: i64 = 21;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AllocationReservations {
    pub(crate) remaining_eighth_bits: u32,
    pub(crate) anti_collapse_eighth_bits: u8,
    pub(crate) skip_eighth_bits: u8,
    pub(crate) intensity_eighth_bits: u8,
    pub(crate) dual_stereo_eighth_bits: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BandAllocation {
    pub(crate) start_band: u8,
    pub(crate) end_band: u8,
    pub(crate) quality_q64: u16,
    pub(crate) eighth_bits: [u32; 21],
    pub(crate) total_eighth_bits: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReallocatedBands {
    pub(crate) start_band: u8,
    pub(crate) coded_end_band: u8,
    pub(crate) eighth_bits: [u32; 21],
    pub(crate) intensity_band: u8,
    pub(crate) dual_stereo: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FineShapeAllocation {
    pub(crate) start_band: u8,
    pub(crate) coded_end_band: u8,
    pub(crate) shape_eighth_bits: [u32; 21],
    pub(crate) fine_bits: [u8; 21],
    pub(crate) fine_priority: [bool; 21],
    pub(crate) balance_eighth_bits: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FinalFineEnergy {
    pub(crate) energies_q15: [i32; 42],
    pub(crate) fine_bits: [u8; 21],
    pub(crate) remaining_bits: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AllocationControls {
    pub(crate) band_boosts: [u16; 21],
    pub(crate) total_boost: u32,
    pub(crate) trim: u8,
}

const ENERGY_LAPLACE_MODELS: [[[u8; 42]; 2]; 4] = [
    [
        [
            72, 127, 65, 129, 66, 128, 65, 128, 64, 128, 62, 128, 64, 128, 64, 128, 92, 78, 92, 79,
            92, 78, 90, 79, 116, 41, 115, 40, 114, 40, 132, 26, 132, 26, 145, 17, 161, 12, 176, 10,
            177, 11,
        ],
        [
            24, 179, 48, 138, 54, 135, 54, 132, 53, 134, 56, 133, 55, 132, 55, 132, 61, 114, 70,
            96, 74, 88, 75, 88, 87, 74, 89, 66, 91, 67, 100, 59, 108, 50, 120, 40, 122, 37, 97, 43,
            78, 50,
        ],
    ],
    [
        [
            83, 78, 84, 81, 88, 75, 86, 74, 87, 71, 90, 73, 93, 74, 93, 74, 109, 40, 114, 36, 117,
            34, 117, 34, 143, 17, 145, 18, 146, 19, 162, 12, 165, 10, 178, 7, 189, 6, 190, 8, 177,
            9,
        ],
        [
            23, 178, 54, 115, 63, 102, 66, 98, 69, 99, 74, 89, 71, 91, 73, 91, 78, 89, 86, 80, 92,
            66, 93, 64, 102, 59, 103, 60, 104, 60, 117, 52, 123, 44, 138, 35, 133, 31, 97, 38, 77,
            45,
        ],
    ],
    [
        [
            61, 90, 93, 60, 105, 42, 107, 41, 110, 45, 116, 38, 113, 38, 112, 38, 124, 26, 132, 27,
            136, 19, 140, 20, 155, 14, 159, 16, 158, 18, 170, 13, 177, 10, 187, 8, 192, 6, 175, 9,
            159, 10,
        ],
        [
            21, 178, 59, 110, 71, 86, 75, 85, 84, 83, 91, 66, 88, 73, 87, 72, 92, 75, 98, 72, 105,
            58, 107, 54, 115, 52, 114, 55, 112, 56, 129, 51, 132, 40, 150, 33, 140, 29, 98, 35, 77,
            42,
        ],
    ],
    [
        [
            42, 121, 96, 66, 108, 43, 111, 40, 117, 44, 123, 32, 120, 36, 119, 33, 127, 33, 134,
            34, 139, 21, 147, 23, 152, 20, 158, 25, 154, 26, 166, 21, 173, 16, 184, 13, 184, 10,
            150, 13, 139, 15,
        ],
        [
            22, 178, 63, 114, 74, 82, 84, 83, 92, 82, 103, 62, 96, 72, 96, 67, 101, 73, 107, 72,
            113, 55, 118, 52, 125, 52, 118, 52, 117, 55, 135, 49, 137, 39, 157, 32, 145, 29, 97,
            33, 77, 40,
        ],
    ],
];

const ENERGY_PREDICTION_COEFFICIENTS: [(i32, i32); 4] = [
    (29_440, 30_147),
    (26_112, 22_282),
    (21_248, 12_124),
    (16_384, 6_554),
];
const ENERGY_INTRA_BETA_Q15: i32 = 4_915;

pub(crate) fn band_edges(frame_samples_48k: u16) -> Option<[u16; 22]> {
    let scale = match frame_samples_48k {
        120 => 1,
        240 => 2,
        480 => 4,
        960 => 8,
        _ => return None,
    };
    let mut edges = [0; 22];
    for (index, width) in BAND_WIDTHS_2_5_MS.into_iter().enumerate() {
        edges[index + 1] = edges[index] + width * scale;
    }
    Some(edges)
}

pub(crate) fn static_allocation(
    frame_samples_48k: u16,
    band: u8,
    channels: u8,
    quality_q64: u16,
) -> Result<u32, RangeError> {
    band_edges(frame_samples_48k).ok_or(RangeError::InvalidCeltEnergyLayout)?;
    if band >= 21 || !(1..=2).contains(&channels) {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    if quality_q64 > 640 {
        return Err(RangeError::InvalidCeltQuality);
    }
    let column = usize::from((quality_q64 / 64).min(9));
    let fraction = if quality_q64 == 640 {
        64
    } else {
        u32::from(quality_q64 % 64)
    };
    let row = STATIC_ALLOCATION[usize::from(band)];
    let interpolated_q64 =
        u32::from(row[column]) * (64 - fraction) + u32::from(row[column + 1]) * fraction;
    let width = u32::from(BAND_WIDTHS_2_5_MS[usize::from(band)]);
    let lm = u32::from(frame_samples_48k / 120).trailing_zeros();
    Ok(u32::from(channels) * width * interpolated_q64 * (1 << lm) / 256)
}

pub(crate) fn allocation_thresholds(
    frame_samples_48k: u16,
    start_band: u8,
    end_band: u8,
    channels: u8,
) -> Result<[u16; 21], RangeError> {
    let edges = band_edges(frame_samples_48k).ok_or(RangeError::InvalidCeltEnergyLayout)?;
    if start_band >= end_band || end_band > 21 || !(1..=2).contains(&channels) {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    let mut thresholds = [0; 21];
    for band in start_band..end_band {
        let width = u32::from(edges[usize::from(band) + 1] - edges[usize::from(band)]);
        let threshold = ((24 * width) >> 4).max(8 * u32::from(channels));
        thresholds[usize::from(band)] = threshold as u16;
    }
    Ok(thresholds)
}

pub(crate) fn allocation_caps(
    frame_samples_48k: u16,
    end_band: u8,
    channels: u8,
) -> Result<[u16; 21], RangeError> {
    let edges = band_edges(frame_samples_48k).ok_or(RangeError::InvalidCeltEnergyLayout)?;
    if end_band == 0 || end_band > 21 || !(1..=2).contains(&channels) {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    let lm = u32::from(frame_samples_48k / 120).trailing_zeros();
    let mode = (2 * lm + u32::from(channels == 2)) as usize;
    let mut caps = [0; 21];
    for band in 0..end_band {
        let width = u32::from(edges[usize::from(band) + 1] - edges[usize::from(band)]);
        let cache_cap = u32::from(ALLOCATION_CAPS[mode][usize::from(band)]) + 64;
        caps[usize::from(band)] = ((cache_cap * u32::from(channels) * width) / 4) as u16;
    }
    Ok(caps)
}

pub(crate) fn reserve_allocation_bits(
    frame_samples_48k: u16,
    start_band: u8,
    end_band: u8,
    channels: u8,
    transient: bool,
    available_eighth_bits: u32,
) -> Result<AllocationReservations, RangeError> {
    if band_edges(frame_samples_48k).is_none()
        || start_band >= end_band
        || end_band > 21
        || !(1..=2).contains(&channels)
    {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    let lm = u32::from(frame_samples_48k / 120).trailing_zeros();
    let mut total = available_eighth_bits.saturating_sub(1);
    let anti_collapse = if transient && lm > 1 && total >= (lm + 2) * 8 {
        total -= 8;
        8
    } else {
        0
    };
    let skip = if total > 8 {
        total -= 8;
        8
    } else {
        0
    };
    let intensity = if channels == 2 {
        let reserved = u32::from(LOG2_FRAC_EIGHTHS[usize::from(end_band - start_band - 1)]);
        if reserved <= total {
            total -= reserved;
            reserved as u8
        } else {
            0
        }
    } else {
        0
    };
    let dual_stereo = if channels == 2 && total > 8 {
        total -= 8;
        8
    } else {
        0
    };
    Ok(AllocationReservations {
        remaining_eighth_bits: total,
        anti_collapse_eighth_bits: anti_collapse,
        skip_eighth_bits: skip,
        intensity_eighth_bits: intensity,
        dual_stereo_eighth_bits: dual_stereo,
    })
}

pub(crate) fn select_band_allocation(
    frame_samples_48k: u16,
    start_band: u8,
    end_band: u8,
    channels: u8,
    controls: &AllocationControls,
    available_eighth_bits: u32,
) -> Result<Option<BandAllocation>, RangeError> {
    let trim_offsets = allocation_trim_offsets(
        frame_samples_48k,
        start_band,
        end_band,
        channels,
        controls.trim,
    )?;
    let thresholds = allocation_thresholds(frame_samples_48k, start_band, end_band, channels)?;
    let caps = allocation_caps(frame_samples_48k, end_band, channels)?;
    let mut best = None;
    let floor = 8 * u32::from(channels);
    for quality_q64 in 0..=640 {
        let mut eighth_bits = [0; 21];
        let mut total_eighth_bits = 0_u32;
        let mut done = false;
        for band in (start_band..end_band).rev() {
            let band_index = usize::from(band);
            let static_bits = static_allocation(frame_samples_48k, band, channels, quality_q64)?;
            let tilted_bits = if static_bits > 0 {
                (i64::from(static_bits) + i64::from(trim_offsets[band_index])).max(0) as u32
            } else {
                0
            };
            let adjusted = tilted_bits + u32::from(controls.band_boosts[band_index]);
            let allocation = if adjusted >= u32::from(thresholds[band_index]) || done {
                done = true;
                adjusted.min(u32::from(caps[band_index]))
            } else if adjusted >= floor {
                floor
            } else {
                0
            };
            eighth_bits[band_index] = allocation;
            total_eighth_bits += allocation;
        }
        if total_eighth_bits > available_eighth_bits {
            break;
        }
        best = Some(BandAllocation {
            start_band,
            end_band,
            quality_q64,
            eighth_bits,
            total_eighth_bits,
        });
    }
    Ok(best)
}

pub(crate) fn reallocate_skipped_bands(
    range: &mut RangeDecoder<'_>,
    frame_samples_48k: u16,
    channels: u8,
    controls: &AllocationControls,
    reservations: AllocationReservations,
    allocation: BandAllocation,
) -> Result<ReallocatedBands, RangeError> {
    let edges = band_edges(frame_samples_48k).ok_or(RangeError::InvalidCeltEnergyLayout)?;
    let start_band = allocation.start_band;
    let end_band = allocation.end_band;
    if start_band >= end_band || end_band > 21 || !(1..=2).contains(&channels) {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    let thresholds = allocation_thresholds(frame_samples_48k, start_band, end_band, channels)?;
    let floor = 8 * u32::from(channels);
    let skip_start = (start_band..end_band)
        .rev()
        .find(|&band| controls.band_boosts[usize::from(band)] > 0)
        .unwrap_or(start_band);
    let mut bits = allocation.eighth_bits;
    let mut sum = i64::from(allocation.total_eighth_bits);
    let mut total = i64::from(reservations.remaining_eighth_bits);
    let mut intensity_reservation = i64::from(reservations.intensity_eighth_bits);
    let dual_reservation = i64::from(reservations.dual_stereo_eighth_bits);
    let mut coded_end_band = end_band;

    let allocated_sum = allocation.eighth_bits[usize::from(start_band)..usize::from(end_band)]
        .iter()
        .map(|&bits| u64::from(bits))
        .sum::<u64>();
    if sum > total || allocated_sum != u64::from(allocation.total_eighth_bits) {
        return Err(RangeError::CorruptFrame);
    }

    loop {
        let band = coded_end_band - 1;
        if band <= skip_start {
            total += i64::from(reservations.skip_eighth_bits);
            break;
        }
        if sum > total {
            return Err(RangeError::CorruptFrame);
        }

        let span = i64::from(edges[usize::from(coded_end_band)] - edges[usize::from(start_band)]);
        let left = total - sum;
        let per_coefficient = left / span;
        let remainder = left - span * per_coefficient;
        let width_before = i64::from(edges[usize::from(band)] - edges[usize::from(start_band)]);
        let width = i64::from(edges[usize::from(coded_end_band)] - edges[usize::from(band)]);
        let mut band_bits = i64::from(bits[usize::from(band)])
            + per_coefficient * width
            + (remainder - width_before).max(0);

        let skip_threshold = u32::from(thresholds[usize::from(band)]).max(floor + 8);
        if band_bits >= i64::from(skip_threshold) {
            if range.decode_bit_logp(1)? {
                break;
            }
            sum += 8;
            band_bits -= 8;
        }

        sum -= i64::from(bits[usize::from(band)]) + intensity_reservation;
        if intensity_reservation > 0 {
            intensity_reservation = i64::from(LOG2_FRAC_EIGHTHS[usize::from(band - start_band)]);
        }
        sum += intensity_reservation;
        if band_bits >= i64::from(floor) {
            sum += i64::from(floor);
            bits[usize::from(band)] = floor;
        } else {
            bits[usize::from(band)] = 0;
        }
        coded_end_band -= 1;
    }

    let intensity_band = if intensity_reservation > 0 {
        let choices = u32::from(coded_end_band - start_band) + 1;
        start_band + range.decode_uint(choices)? as u8
    } else {
        0
    };
    let dual_stereo = if intensity_band <= start_band {
        total += dual_reservation;
        false
    } else if dual_reservation > 0 {
        range.decode_bit_logp(1)?
    } else {
        false
    };

    let coded_width =
        i64::from(edges[usize::from(coded_end_band)] - edges[usize::from(start_band)]);
    let mut left = total - sum;
    if left < 0 || coded_width == 0 {
        return Err(RangeError::CorruptFrame);
    }
    let per_coefficient = left / coded_width;
    left -= coded_width * per_coefficient;
    for band in start_band..coded_end_band {
        let index = usize::from(band);
        let width = i64::from(edges[index + 1] - edges[index]);
        bits[index] += (per_coefficient * width) as u32;
    }
    for band in start_band..coded_end_band {
        let index = usize::from(band);
        let extra = left.min(i64::from(edges[index + 1] - edges[index]));
        bits[index] += extra as u32;
        left -= extra;
    }
    Ok(ReallocatedBands {
        start_band,
        coded_end_band,
        eighth_bits: bits,
        intensity_band,
        dual_stereo,
    })
}

pub(crate) fn split_fine_shape_allocation(
    frame_samples_48k: u16,
    channels: u8,
    allocation: ReallocatedBands,
) -> Result<FineShapeAllocation, RangeError> {
    let edges = band_edges(frame_samples_48k).ok_or(RangeError::InvalidCeltEnergyLayout)?;
    let start_band = allocation.start_band;
    let end_band = allocation.coded_end_band;
    if start_band >= end_band || end_band > 21 || !(1..=2).contains(&channels) {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }

    let lm = u32::from(frame_samples_48k / 120).trailing_zeros();
    let mut shape = allocation.eighth_bits;
    let mut fine_bits = [0; 21];
    let mut fine_priority = [false; 21];
    let mut balance = 0_i64;
    let channel_count = i64::from(channels);
    let stereo = i64::from(channels == 2);
    let allocation_floor = channel_count * 8;
    let caps = allocation_caps(frame_samples_48k, end_band, channels)?;

    for band in start_band..end_band {
        let index = usize::from(band);
        let width = u32::from(edges[index + 1] - edges[index]);
        let bins = width << lm;
        let mut bits = i64::from(shape[index]) + balance;
        let mut excess: i64;

        if bins > 1 {
            excess = (bits - i64::from(caps[index])).max(0);
            bits -= excess;

            let joint_side_dof = i64::from(
                channels == 2
                    && bins > 2
                    && !allocation.dual_stereo
                    && band < allocation.intensity_band,
            );
            let denominator = channel_count * i64::from(bins) + joint_side_dof;
            let log_n = i64::from(BAND_LOG2_EIGHTHS[index]) + i64::from(lm) * 8;
            let n_log_n = denominator * log_n;
            let mut offset = (n_log_n >> 1) - denominator * FINE_ENERGY_OFFSET_EIGHTHS;
            if bins == 2 {
                offset += denominator * 2;
            }
            if bits + offset < denominator * 16 {
                offset += n_log_n >> 2;
            } else if bits + offset < denominator * 24 {
                offset += n_log_n >> 3;
            }

            let rounded = (bits + offset + denominator * 4) / (denominator * 8);
            fine_bits[index] = rounded.max(0).min(i64::from(MAX_FINE_ENERGY_BITS)) as u8;
            if channel_count * i64::from(fine_bits[index]) > (bits >> 3) {
                fine_bits[index] = (bits >> stereo >> 3) as u8;
            }
            fine_priority[index] = i64::from(fine_bits[index]) * denominator * 8 >= bits + offset;
            bits -= channel_count * i64::from(fine_bits[index]) * 8;
        } else {
            excess = (bits - allocation_floor).max(0);
            bits -= excess;
            fine_priority[index] = true;
        }

        if excess > 0 {
            let extra_fine = (excess >> (stereo as u32 + 3))
                .min(i64::from(MAX_FINE_ENERGY_BITS - fine_bits[index]));
            fine_bits[index] += extra_fine as u8;
            let extra_bits = extra_fine * channel_count * 8;
            fine_priority[index] = extra_bits >= excess - balance;
            excess -= extra_bits;
        }
        balance = excess;
        if bits < 0 || fine_bits[index] > MAX_FINE_ENERGY_BITS {
            return Err(RangeError::CorruptFrame);
        }
        shape[index] = bits as u32;
    }

    for band in end_band..21 {
        let index = usize::from(band);
        let bits = allocation.eighth_bits[index];
        let divisor = channel_count * 8;
        if i64::from(bits) % divisor != 0 {
            return Err(RangeError::CorruptFrame);
        }
        let energy_bits = i64::from(bits) / divisor;
        if energy_bits > i64::from(MAX_FINE_ENERGY_BITS) {
            return Err(RangeError::CorruptFrame);
        }
        fine_bits[index] = energy_bits as u8;
        shape[index] = 0;
        fine_priority[index] = energy_bits < 1;
    }

    if balance < 0 || balance > i64::from(u32::MAX) {
        return Err(RangeError::CorruptFrame);
    }

    Ok(FineShapeAllocation {
        start_band,
        coded_end_band: end_band,
        shape_eighth_bits: shape,
        fine_bits,
        fine_priority,
        balance_eighth_bits: balance as u32,
    })
}

pub(crate) fn decode_final_fine_energy(
    range: &mut RangeDecoder<'_>,
    energies_q15: &[i32; 42],
    allocation: FineShapeAllocation,
    channels: u8,
    mut remaining_bits: u32,
) -> Result<FinalFineEnergy, RangeError> {
    let start_band = allocation.start_band;
    let end_band = allocation.coded_end_band;
    if start_band >= end_band || end_band > 21 || !(1..=2).contains(&channels) {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    if allocation.fine_bits[usize::from(start_band)..usize::from(end_band)]
        .iter()
        .any(|&bits| bits > MAX_FINE_ENERGY_BITS)
    {
        return Err(RangeError::InvalidFineEnergyAllocation);
    }

    let mut energies = *energies_q15;
    let mut final_bits = allocation.fine_bits;
    for priority in [false, true] {
        for band in start_band..end_band {
            let index = usize::from(band);
            if final_bits[index] >= MAX_FINE_ENERGY_BITS
                || allocation.fine_priority[index] != priority
                || remaining_bits < u32::from(channels)
            {
                continue;
            }
            let shift = 13 - final_bits[index];
            for channel in 0..channels {
                let bit = range.read_raw_bits(1)? as i32;
                let correction = ((bit * 2) - 1) << shift;
                let energy_index = index * 2 + usize::from(channel);
                energies[energy_index] = energies[energy_index]
                    .checked_add(correction)
                    .ok_or(RangeError::CorruptFrame)?;
                remaining_bits -= 1;
            }
            final_bits[index] += 1;
        }
    }

    Ok(FinalFineEnergy {
        energies_q15: energies,
        fine_bits: final_bits,
        remaining_bits,
    })
}

pub(crate) fn allocation_trim_offsets(
    frame_samples_48k: u16,
    start_band: u8,
    end_band: u8,
    channels: u8,
    trim: u8,
) -> Result<[i32; 21], RangeError> {
    let edges = band_edges(frame_samples_48k).ok_or(RangeError::InvalidCeltEnergyLayout)?;
    let lm = u32::from(frame_samples_48k / 120).trailing_zeros();
    if start_band >= end_band || end_band > 21 || !(1..=2).contains(&channels) || trim > 10 {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    let mut offsets = [0; 21];
    for band in start_band..end_band {
        let remaining_bands = i32::from(end_band - band);
        let base_width = i32::from(BAND_WIDTHS_2_5_MS[usize::from(band)]);
        let adjustment = ((i32::from(trim) - 5 - lm as i32)
            * i32::from(channels)
            * base_width
            * remaining_bands
            * (1 << lm)
            * 8)
            >> 6;
        let width = edges[usize::from(band) + 1] - edges[usize::from(band)];
        offsets[usize::from(band)] = adjustment
            - if width == 1 {
                8 * i32::from(channels)
            } else {
                0
            };
    }
    Ok(offsets)
}

pub(crate) fn decode_coarse_energy_residuals(
    range: &mut RangeDecoder<'_>,
    frame_samples_48k: u16,
    intra: bool,
    start_band: u8,
    end_band: u8,
    channels: u8,
    frame_bytes: usize,
) -> Result<[i16; 42], RangeError> {
    let lm = match frame_samples_48k {
        120 => 0,
        240 => 1,
        480 => 2,
        960 => 3,
        _ => return Err(RangeError::InvalidCeltEnergyLayout),
    };
    if !(1..=2).contains(&channels) || start_band >= end_band || end_band > 21 {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }

    let model = &ENERGY_LAPLACE_MODELS[lm][usize::from(intra)];
    let budget = frame_bytes.saturating_mul(64);
    let mut residuals = [0; 42];
    for band in start_band..end_band {
        let zero_frequency = u16::from(model[usize::from(band) * 2]) << 7;
        let decay = u16::from(model[usize::from(band) * 2 + 1]) << 6;
        for channel in 0..channels {
            let remaining = budget.saturating_sub(range.tell_frac() as usize);
            let residual = if remaining >= 15 * 8 {
                range.decode_laplace(zero_frequency, decay)?
            } else if remaining >= 2 * 8 {
                let value = range.decode_icdf(&[2, 1, 0], 2)? as i32;
                (value >> 1) ^ -(value & 1)
            } else if remaining >= 8 {
                -i32::from(range.decode_bit_logp(1)?)
            } else {
                -1
            };
            let index = usize::from(band) * 2 + usize::from(channel);
            residuals[index] = residual as i16;
        }
    }
    Ok(residuals)
}

pub(crate) fn predict_coarse_energies(
    residuals: &[i16; 42],
    previous_final_energies_q15: &[i32; 42],
    frame_samples_48k: u16,
    intra: bool,
    end_band: u8,
    channels: u8,
) -> Result<[i32; 42], RangeError> {
    let lm = match frame_samples_48k {
        120 => 0,
        240 => 1,
        480 => 2,
        960 => 3,
        _ => return Err(RangeError::InvalidCeltEnergyLayout),
    };
    if !(1..=2).contains(&channels) || end_band == 0 || end_band > 21 {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }

    let (alpha, inter_beta) = ENERGY_PREDICTION_COEFFICIENTS[lm];
    let beta = if intra {
        ENERGY_INTRA_BETA_Q15
    } else {
        inter_beta
    };
    let alpha = if intra { 0 } else { alpha };
    let mut energies = [0; 42];
    let mut previous_residual_q15 = [0_i64; 2];

    // Solve the RFC's 2-D prediction filter from low to high band.
    for band in 0..end_band {
        for channel in 0..channels {
            let index = usize::from(band) * 2 + usize::from(channel);
            let previous_band_index = index.saturating_sub(2);
            let current_frequency_prediction = if band == 0 {
                0_i64
            } else {
                i64::from(energies[previous_band_index])
            };
            let previous_frame_band = i64::from(previous_final_energies_q15[index]);
            let previous_frame_frequency = if band == 0 {
                0_i64
            } else {
                i64::from(previous_final_energies_q15[previous_band_index])
            };
            let temporal_prediction =
                multiply_q15(previous_frame_band - previous_frame_frequency, alpha);
            let residual_q15 = i64::from(residuals[index]) << 15;
            let energy = current_frequency_prediction + temporal_prediction + residual_q15
                - multiply_q15(previous_residual_q15[usize::from(channel)], beta);
            energies[index] = energy.try_into().map_err(|_| RangeError::CorruptFrame)?;
            previous_residual_q15[usize::from(channel)] = residual_q15;
        }
    }
    Ok(energies)
}

fn multiply_q15(value: i64, coefficient: i32) -> i64 {
    let product = value * i64::from(coefficient);
    if product >= 0 {
        (product + (1 << 14)) >> 15
    } else {
        -((-product + (1 << 14)) >> 15)
    }
}

pub(crate) fn decode_fine_energies(
    range: &mut RangeDecoder<'_>,
    coarse_energies_q15: &[i32; 42],
    fine_bits: &[u8; 21],
    end_band: u8,
    channels: u8,
) -> Result<[i32; 42], RangeError> {
    if !(1..=2).contains(&channels) || end_band == 0 || end_band > 21 {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    if fine_bits[..usize::from(end_band)]
        .iter()
        .any(|&bits| bits > MAX_FINE_ENERGY_BITS)
    {
        return Err(RangeError::InvalidFineEnergyAllocation);
    }

    let mut energies_q15 = *coarse_energies_q15;
    for band in 0..end_band {
        for channel in 0..channels {
            let index = usize::from(band) * 2 + usize::from(channel);
            let bits = fine_bits[usize::from(band)];
            if bits == 0 {
                continue;
            }
            let value = i32::try_from(range.read_raw_bits(bits)?)
                .map_err(|_| RangeError::InvalidFineEnergyAllocation)?;
            let correction_numerator = 2 * value + 1 - (1 << bits);
            let correction_q15 = correction_numerator << (14 - bits);
            energies_q15[index] = energies_q15[index]
                .checked_add(correction_q15)
                .ok_or(RangeError::CorruptFrame)?;
        }
    }
    Ok(energies_q15)
}

pub(crate) fn decode_band_boosts(
    range: &mut RangeDecoder<'_>,
    frame_samples_48k: u16,
    start_band: u8,
    end_band: u8,
    frame_bytes: usize,
    caps: &[u16; 21],
) -> Result<([u16; 21], u32), RangeError> {
    let edges = band_edges(frame_samples_48k).ok_or(RangeError::InvalidCeltEnergyLayout)?;
    if start_band >= end_band || end_band > 21 {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }

    let total_bits = frame_bytes.saturating_mul(64);
    let mut total_boost = 0_usize;
    let mut dynalloc_logp = 6;
    let mut boosts = [0; 21];
    for band in start_band..end_band {
        let width = usize::from(edges[usize::from(band) + 1] - edges[usize::from(band)]);
        let quanta = (8 * width).min(48_usize.max(width));
        let mut boost = 0_usize;
        let mut loop_logp = dynalloc_logp;
        while boost < usize::from(caps[usize::from(band)]) {
            let tell = range.tell_frac() as usize;
            let symbol_cost = usize::from(loop_logp) * 8;
            if tell.saturating_add(symbol_cost) >= total_bits.saturating_add(total_boost) {
                break;
            }
            if !range.decode_bit_logp(loop_logp)? {
                break;
            }
            boost = boost.saturating_add(quanta);
            total_boost = total_boost.saturating_add(quanta);
            loop_logp = 1;
        }
        boosts[usize::from(band)] = boost.try_into().map_err(|_| RangeError::CorruptFrame)?;
        if boost > 0 && dynalloc_logp > 2 {
            dynalloc_logp -= 1;
        }
    }
    Ok((
        boosts,
        total_boost
            .try_into()
            .map_err(|_| RangeError::CorruptFrame)?,
    ))
}

pub(crate) fn decode_allocation_trim(
    range: &mut RangeDecoder<'_>,
    frame_bytes: usize,
    total_boost: u32,
) -> Result<u8, RangeError> {
    let total_bits = frame_bytes.saturating_mul(64);
    let remaining = total_bits.saturating_sub(total_boost as usize);
    if (range.tell_frac() as usize).saturating_add(48) <= remaining {
        Ok(range.decode_symbol(&[2, 2, 5, 10, 22, 46, 22, 10, 5, 2, 2])? as u8)
    } else {
        Ok(5)
    }
}

pub(crate) fn decode_allocation_controls(
    range: &mut RangeDecoder<'_>,
    frame_samples_48k: u16,
    start_band: u8,
    end_band: u8,
    channels: u8,
    frame_bytes: usize,
) -> Result<AllocationControls, RangeError> {
    let caps = allocation_caps(frame_samples_48k, end_band, channels)?;
    let (band_boosts, total_boost) = decode_band_boosts(
        range,
        frame_samples_48k,
        start_band,
        end_band,
        frame_bytes,
        &caps,
    )?;
    let trim = decode_allocation_trim(range, frame_bytes, total_boost)?;
    Ok(AllocationControls {
        band_boosts,
        total_boost,
        trim,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PostFilter {
    pub(crate) period: u16,
    pub(crate) gain: u8,
    pub(crate) tapset: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FrameHeader {
    pub(crate) silence: bool,
    pub(crate) postfilter: Option<PostFilter>,
    pub(crate) transient: bool,
    pub(crate) intra: bool,
}

pub(crate) fn decode_header(range: &mut RangeDecoder<'_>) -> Result<FrameHeader, RangeError> {
    let silence = range.decode_symbol(&[32_767, 1])? == 1;
    if silence {
        return Ok(FrameHeader {
            silence,
            postfilter: None,
            transient: false,
            intra: false,
        });
    }

    let postfilter = if range.decode_bit_logp(1)? {
        let octave = range.decode_uint(6)? as u8;
        let period = (16_u32 << octave) + range.read_raw_bits(4 + octave)? - 1;
        let gain = range.read_raw_bits(3)? as u8;
        let tapset = range.decode_symbol(&[2, 1, 1])? as u8;
        Some(PostFilter {
            period: period as u16,
            gain,
            tapset,
        })
    } else {
        None
    };
    let transient = range.decode_symbol(&[7, 1])? == 1;
    let intra = range.decode_symbol(&[7, 1])? == 1;
    Ok(FrameHeader {
        silence,
        postfilter,
        transient,
        intra,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        AllocationControls, ENERGY_LAPLACE_MODELS, FrameHeader, allocation_caps,
        allocation_thresholds, allocation_trim_offsets, band_edges, decode_allocation_controls,
        decode_allocation_trim, decode_band_boosts, decode_coarse_energy_residuals,
        decode_final_fine_energy, decode_fine_energies, decode_header, predict_coarse_energies,
        reallocate_skipped_bands, reserve_allocation_bits, select_band_allocation,
        split_fine_shape_allocation, static_allocation,
    };
    use crate::range::{RangeDecoder, RangeError};

    #[test]
    fn band_edges_scale_for_all_celt_frame_durations() {
        let base = [
            0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 34, 40, 48, 60, 78, 100,
        ];
        for (frame_samples, scale) in [(120, 1), (240, 2), (480, 4), (960, 8)] {
            let edges = band_edges(frame_samples).unwrap();
            assert_eq!(edges, base.map(|edge| edge * scale));
        }
        assert_eq!(band_edges(60), None);
    }

    #[test]
    fn static_allocation_interpolates_quality_and_scales_frame_geometry() {
        assert_eq!(static_allocation(120, 0, 1, 0), Ok(0));
        assert_eq!(static_allocation(120, 0, 1, 64), Ok(22));
        assert_eq!(static_allocation(120, 0, 1, 640), Ok(50));
        assert_eq!(static_allocation(240, 0, 1, 640), Ok(100));
        assert_eq!(static_allocation(480, 0, 1, 640), Ok(200));
        assert_eq!(static_allocation(960, 0, 1, 640), Ok(400));
        assert_eq!(static_allocation(120, 20, 2, 640), Ok(1_144));
    }

    #[test]
    fn static_allocation_rejects_invalid_inputs() {
        assert_eq!(
            static_allocation(60, 0, 1, 0),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            static_allocation(120, 21, 1, 0),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            static_allocation(120, 0, 3, 0),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            static_allocation(120, 0, 1, 641),
            Err(RangeError::InvalidCeltQuality)
        );
    }

    #[test]
    fn allocation_limits_follow_frame_width_and_trim_rules() {
        let thresholds = allocation_thresholds(120, 0, 2, 1).unwrap();
        assert_eq!(&thresholds[..3], &[8, 8, 0]);
        let thresholds = allocation_thresholds(960, 0, 2, 2).unwrap();
        assert_eq!(&thresholds[..3], &[16, 16, 0]);

        let neutral = allocation_trim_offsets(120, 0, 2, 1, 5).unwrap();
        assert_eq!(&neutral[..2], &[-8, -8]);
        let high = allocation_trim_offsets(240, 0, 2, 1, 10).unwrap();
        assert_eq!(&high[..2], &[2, 1]);

        let caps = allocation_caps(120, 2, 1).unwrap();
        assert_eq!(&caps[..2], &[72, 72]);
        assert_eq!(allocation_caps(120, 1, 2).unwrap()[0], 144);
        assert_eq!(allocation_caps(240, 1, 1).unwrap()[0], 112);
        let caps = allocation_caps(240, 2, 2).unwrap();
        assert_eq!(&caps[..2], &[304, 304]);
        assert_eq!(allocation_caps(480, 1, 1).unwrap()[0], 249);
        assert_eq!(allocation_caps(480, 1, 2).unwrap()[0], 542);
        assert_eq!(allocation_caps(960, 1, 1).unwrap()[0], 514);
        assert_eq!(allocation_caps(960, 1, 2).unwrap()[0], 1_072);
    }

    #[test]
    fn allocation_limits_reject_invalid_layout_and_trim() {
        assert_eq!(
            allocation_thresholds(120, 0, 0, 1),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            allocation_trim_offsets(120, 0, 1, 1, 11),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            allocation_caps(120, 0, 1),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
    }

    #[test]
    fn allocation_reservations_follow_frame_and_channel_rules() {
        assert_eq!(
            reserve_allocation_bits(960, 0, 21, 1, true, 50),
            Ok(super::AllocationReservations {
                remaining_eighth_bits: 33,
                anti_collapse_eighth_bits: 8,
                skip_eighth_bits: 8,
                intensity_eighth_bits: 0,
                dual_stereo_eighth_bits: 0,
            })
        );
        assert_eq!(
            reserve_allocation_bits(960, 0, 21, 2, false, 100),
            Ok(super::AllocationReservations {
                remaining_eighth_bits: 48,
                anti_collapse_eighth_bits: 0,
                skip_eighth_bits: 8,
                intensity_eighth_bits: 35,
                dual_stereo_eighth_bits: 8,
            })
        );
        assert_eq!(
            reserve_allocation_bits(960, 0, 21, 2, false, 16),
            Ok(super::AllocationReservations {
                remaining_eighth_bits: 7,
                anti_collapse_eighth_bits: 0,
                skip_eighth_bits: 8,
                intensity_eighth_bits: 0,
                dual_stereo_eighth_bits: 0,
            })
        );
        assert_eq!(
            reserve_allocation_bits(120, 0, 21, 1, true, 40)
                .unwrap()
                .anti_collapse_eighth_bits,
            0
        );
    }

    #[test]
    fn allocation_search_chooses_highest_quality_within_budget() {
        let controls = AllocationControls {
            band_boosts: [0; 21],
            total_boost: 0,
            trim: 5,
        };
        let allocation = select_band_allocation(120, 0, 1, 1, &controls, 8)
            .unwrap()
            .unwrap();
        assert_eq!(allocation.quality_q64, 48);
        assert_eq!(allocation.eighth_bits[0], 8);
        assert_eq!(allocation.total_eighth_bits, 8);
        let below_floor = select_band_allocation(120, 0, 1, 1, &controls, 7)
            .unwrap()
            .unwrap();
        assert_eq!(below_floor.total_eighth_bits, 0);
    }

    #[test]
    fn band_reallocation_obeys_skip_flags_and_reclaims_skipped_band_bits() {
        let controls = AllocationControls {
            band_boosts: [0; 21],
            total_boost: 0,
            trim: 5,
        };
        let mut eighth_bits = [0; 21];
        eighth_bits[..3].copy_from_slice(&[8, 8, 8]);
        let allocation = super::BandAllocation {
            start_band: 0,
            end_band: 3,
            quality_q64: 0,
            eighth_bits,
            total_eighth_bits: 24,
        };
        let reservations = super::AllocationReservations {
            remaining_eighth_bits: 64,
            anti_collapse_eighth_bits: 0,
            skip_eighth_bits: 8,
            intensity_eighth_bits: 0,
            dual_stereo_eighth_bits: 0,
        };

        let mut skip_all = RangeDecoder::new(&[0; 8]);
        let skipped =
            reallocate_skipped_bands(&mut skip_all, 120, 1, &controls, reservations, allocation)
                .unwrap();
        assert_eq!(skipped.coded_end_band, 1);
        assert_eq!(&skipped.eighth_bits[1..3], &[8, 8]);

        let mut keep_all = RangeDecoder::new(&[0xff; 8]);
        let kept =
            reallocate_skipped_bands(&mut keep_all, 120, 1, &controls, reservations, allocation)
                .unwrap();
        assert_eq!(kept.coded_end_band, 3);
        assert!(kept.eighth_bits[..3].iter().all(|&bits| bits > 8));
    }

    #[test]
    fn band_reallocation_rejects_an_inconsistent_allocation_budget() {
        let controls = AllocationControls {
            band_boosts: [0; 21],
            total_boost: 0,
            trim: 5,
        };
        let mut range = RangeDecoder::new(&[0; 8]);
        let result = reallocate_skipped_bands(
            &mut range,
            120,
            1,
            &controls,
            super::AllocationReservations {
                remaining_eighth_bits: 64,
                anti_collapse_eighth_bits: 0,
                skip_eighth_bits: 8,
                intensity_eighth_bits: 0,
                dual_stereo_eighth_bits: 0,
            },
            super::BandAllocation {
                start_band: 0,
                end_band: 2,
                quality_q64: 0,
                eighth_bits: [0; 21],
                total_eighth_bits: 8,
            },
        );
        assert_eq!(result, Err(RangeError::CorruptFrame));
    }

    #[test]
    fn stereo_band_reallocation_keeps_intensity_and_dual_stereo_in_range() {
        let controls = AllocationControls {
            band_boosts: [0; 21],
            total_boost: 0,
            trim: 5,
        };
        let mut eighth_bits = [0; 21];
        eighth_bits[..3].copy_from_slice(&[16, 16, 16]);
        let allocation = super::BandAllocation {
            start_band: 0,
            end_band: 3,
            quality_q64: 0,
            eighth_bits,
            total_eighth_bits: 48,
        };
        let reservations = super::AllocationReservations {
            remaining_eighth_bits: 128,
            anti_collapse_eighth_bits: 0,
            skip_eighth_bits: 8,
            intensity_eighth_bits: super::LOG2_FRAC_EIGHTHS[2],
            dual_stereo_eighth_bits: 8,
        };

        for data in [[0; 8], [0xff; 8]] {
            let mut range = RangeDecoder::new(&data);
            let bands =
                reallocate_skipped_bands(&mut range, 120, 2, &controls, reservations, allocation)
                    .unwrap();
            assert!((allocation.start_band..=bands.coded_end_band).contains(&bands.intensity_band));
            assert!(bands.coded_end_band <= allocation.end_band);
        }
    }

    #[test]
    fn fine_shape_split_conserves_the_band_budget_for_all_frame_modes() {
        for (frame_samples_48k, channels) in [(120, 1), (240, 2), (480, 1), (960, 2)] {
            let source_bits = [256_u32; 21];
            let split = split_fine_shape_allocation(
                frame_samples_48k,
                channels,
                super::ReallocatedBands {
                    start_band: 0,
                    coded_end_band: 21,
                    eighth_bits: source_bits,
                    intensity_band: 10,
                    dual_stereo: false,
                },
            )
            .unwrap();
            let shape_total: u64 = split
                .shape_eighth_bits
                .iter()
                .map(|&bits| u64::from(bits))
                .sum();
            let fine_total: u64 = split
                .fine_bits
                .iter()
                .map(|&bits| u64::from(bits) * u64::from(channels) * 8)
                .sum();
            assert_eq!(
                shape_total + fine_total + u64::from(split.balance_eighth_bits),
                source_bits.iter().map(|&bits| u64::from(bits)).sum::<u64>()
            );
            assert!(split.fine_bits.iter().all(|&bits| bits <= 8));
        }
    }

    #[test]
    fn fine_shape_split_maps_skipped_band_floor_to_fine_energy() {
        let mut eighth_bits = [0; 21];
        eighth_bits[1] = 8;
        let split = split_fine_shape_allocation(
            120,
            1,
            super::ReallocatedBands {
                start_band: 0,
                coded_end_band: 1,
                eighth_bits,
                intensity_band: 0,
                dual_stereo: false,
            },
        )
        .unwrap();
        assert_eq!(split.fine_bits[1], 1);
        assert_eq!(split.shape_eighth_bits[1], 0);
    }

    #[test]
    fn fine_shape_split_applies_the_width_two_offset_and_priority_rule() {
        let mut eighth_bits = [0; 21];
        eighth_bits[8] = 64;
        let split = split_fine_shape_allocation(
            120,
            1,
            super::ReallocatedBands {
                start_band: 8,
                coded_end_band: 9,
                eighth_bits,
                intensity_band: 20,
                dual_stereo: false,
            },
        )
        .unwrap();
        assert_eq!(split.fine_bits[8], 2);
        assert_eq!(split.shape_eighth_bits[8], 48);
        assert!(!split.fine_priority[8]);
    }

    #[test]
    fn final_fine_energy_allocates_priority_zero_before_priority_one() {
        let mut fine_bits = [0; 21];
        fine_bits[1] = 1;
        let mut priority = [false; 21];
        priority[0] = true;
        let mut range = RangeDecoder::new(&[0, 1]);
        let finalised = decode_final_fine_energy(
            &mut range,
            &[0; 42],
            super::FineShapeAllocation {
                start_band: 0,
                coded_end_band: 2,
                shape_eighth_bits: [0; 21],
                fine_bits,
                fine_priority: priority,
                balance_eighth_bits: 0,
            },
            1,
            1,
        )
        .unwrap();
        assert_eq!(finalised.fine_bits[0], 0);
        assert_eq!(finalised.fine_bits[1], 2);
        assert_eq!(finalised.energies_q15[2], 1 << 12);
        assert_eq!(finalised.remaining_bits, 0);
    }

    #[test]
    fn final_fine_energy_waits_for_a_full_stereo_pair() {
        let mut fine_bits = [0; 21];
        fine_bits[0] = 1;
        let before = RangeDecoder::new(&[0, 1]);
        let before_tell = before.tell_frac();
        let mut range = before;
        let finalised = decode_final_fine_energy(
            &mut range,
            &[0; 42],
            super::FineShapeAllocation {
                start_band: 0,
                coded_end_band: 1,
                shape_eighth_bits: [0; 21],
                fine_bits,
                fine_priority: [false; 21],
                balance_eighth_bits: 0,
            },
            2,
            1,
        )
        .unwrap();
        assert_eq!(finalised.fine_bits[0], 1);
        assert_eq!(finalised.remaining_bits, 1);
        assert_eq!(range.tell_frac(), before_tell);
    }

    #[test]
    fn split_allocation_drives_coarse_and_final_fine_energy_decoding() {
        let mut eighth_bits = [0; 21];
        eighth_bits[8] = 64;
        let allocation = split_fine_shape_allocation(
            120,
            1,
            super::ReallocatedBands {
                start_band: 8,
                coded_end_band: 9,
                eighth_bits,
                intensity_band: 20,
                dual_stereo: false,
            },
        )
        .unwrap();
        let mut range = RangeDecoder::new(&[0, 1]);
        let energies =
            decode_fine_energies(&mut range, &[0; 42], &allocation.fine_bits, 9, 1).unwrap();
        assert_eq!(energies[16], -4_096);
        let finalised = decode_final_fine_energy(&mut range, &energies, allocation, 1, 1).unwrap();
        assert_eq!(finalised.energies_q15[16], -6_144);
        assert_eq!(finalised.fine_bits[8], 3);
        assert_eq!(finalised.remaining_bits, 0);
    }

    #[test]
    fn zero_range_stream_decodes_regular_short_frame_header() {
        let mut range = RangeDecoder::new(&[0; 8]);
        assert_eq!(
            decode_header(&mut range),
            Ok(FrameHeader {
                silence: false,
                postfilter: None,
                transient: false,
                intra: false,
            })
        );
    }

    #[test]
    fn high_range_stream_decodes_the_silent_frame_flag() {
        let mut range = RangeDecoder::new(&[0xff; 8]);
        assert_eq!(
            decode_header(&mut range),
            Ok(FrameHeader {
                silence: true,
                postfilter: None,
                transient: false,
                intra: false,
            })
        );
    }

    #[test]
    fn postfilter_pitch_stays_within_the_specified_range() {
        let mut found_postfilter = false;
        for first in 0..=u8::MAX {
            for second in 0..=u8::MAX {
                let mut bytes = [0; 8];
                bytes[0] = first;
                bytes[1] = second;
                let mut range = RangeDecoder::new(&bytes);
                if let Ok(FrameHeader {
                    postfilter: Some(postfilter),
                    ..
                }) = decode_header(&mut range)
                {
                    assert!((15..=1022).contains(&postfilter.period));
                    assert!(postfilter.gain < 8);
                    assert!(postfilter.tapset < 3);
                    found_postfilter = true;
                }
            }
        }
        assert!(found_postfilter);
    }

    #[test]
    fn coarse_energy_models_partition_the_range() {
        for models in ENERGY_LAPLACE_MODELS {
            for model in models {
                for band in 0..21 {
                    let zero_frequency = u16::from(model[band * 2]) << 7;
                    let decay = u16::from(model[band * 2 + 1]) << 6;
                    for value in [0, u32::from(zero_frequency) - 1, 16_384, 32_767] {
                        let (_, low, high) =
                            crate::range::laplace_interval(value, zero_frequency, decay).unwrap();
                        assert!(low <= value && value < high && high <= 32_768);
                    }
                }
            }
        }
    }

    #[test]
    fn coarse_energy_residuals_use_available_frame_budget() {
        let mut empty = RangeDecoder::new(&[]);
        let before = empty.tell_frac();
        let residuals = decode_coarse_energy_residuals(&mut empty, 120, false, 0, 1, 1, 0).unwrap();
        assert_eq!(residuals[0], -1);
        assert_eq!(empty.tell_frac(), before);

        let mut short = RangeDecoder::new(&[0; 2]);
        let residuals = decode_coarse_energy_residuals(&mut short, 120, false, 0, 1, 1, 1).unwrap();
        assert_eq!(residuals[0], 0);

        let mut full = RangeDecoder::new(&[0; 2]);
        let residuals = decode_coarse_energy_residuals(&mut full, 120, false, 0, 1, 1, 2).unwrap();
        assert_eq!(residuals[0], 0);
    }

    #[test]
    fn coarse_energy_rejects_invalid_layouts() {
        let mut range = RangeDecoder::new(&[0; 2]);
        assert_eq!(
            decode_coarse_energy_residuals(&mut range, 60, false, 0, 1, 1, 2),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            decode_coarse_energy_residuals(&mut range, 120, false, 0, 1, 3, 2),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            decode_coarse_energy_residuals(&mut range, 120, false, 4, 4, 1, 2),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
    }

    #[test]
    fn coarse_energy_prediction_matches_the_z_transform_recurrence() {
        let mut residuals = [0; 42];
        residuals[0] = 2;
        let previous = [0; 42];
        let intra = predict_coarse_energies(&residuals, &previous, 120, true, 2, 1).unwrap();
        assert_eq!(intra[0], 2 << 15);
        assert_eq!(intra[2], (2 << 15) - 9_830);

        let mut previous = [0; 42];
        previous[0] = 2 << 15;
        previous[2] = 4 << 15;
        residuals = [0; 42];
        let inter = predict_coarse_energies(&residuals, &previous, 120, false, 2, 1).unwrap();
        assert_eq!(inter[0], 58_880);
        assert_eq!(inter[2], 117_760);

        for (frame_samples, alpha) in [(120, 29_440), (240, 26_112), (480, 21_248), (960, 16_384)] {
            let mut previous = [0; 42];
            previous[0] = 1 << 15;
            residuals = [0; 42];
            let energies =
                predict_coarse_energies(&residuals, &previous, frame_samples, false, 1, 1).unwrap();
            assert_eq!(energies[0], alpha);
        }
    }

    #[test]
    fn coarse_energy_prediction_rejects_invalid_layouts() {
        assert_eq!(
            predict_coarse_energies(&[0; 42], &[0; 42], 120, false, 1, 0),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            predict_coarse_energies(&[0; 42], &[0; 42], 120, false, 22, 1),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
    }

    #[test]
    fn fine_energy_refinement_uses_centered_unsigned_steps() {
        let mut range = RangeDecoder::new(&[0, 0b0000_0101]);
        let mut coarse = [0; 42];
        coarse[0] = 1_000;
        coarse[2] = -500;
        let mut fine_bits = [0; 21];
        fine_bits[0] = 1;
        fine_bits[1] = 2;
        let energies = decode_fine_energies(&mut range, &coarse, &fine_bits, 2, 1).unwrap();
        assert_eq!(energies[0], 1_000 + 8_192);
        assert_eq!(energies[2], -500 + 4_096);
    }

    #[test]
    fn fine_energy_rejects_invalid_allocations_before_consuming_bits() {
        let mut range = RangeDecoder::new(&[0; 2]);
        let before = range.tell_frac();
        let mut fine_bits = [0; 21];
        fine_bits[0] = 9;
        assert_eq!(
            decode_fine_energies(&mut range, &[0; 42], &fine_bits, 1, 1),
            Err(RangeError::InvalidFineEnergyAllocation)
        );
        assert_eq!(range.tell_frac(), before);
    }

    #[test]
    fn allocation_controls_obey_caps_and_frame_budget() {
        let mut caps = [0; 21];
        caps[0] = 8;
        let mut boosted = RangeDecoder::new(&[0xff; 8]);
        let (boosts, total_boost) = decode_band_boosts(&mut boosted, 120, 0, 1, 8, &caps).unwrap();
        assert_eq!(boosts[0], 8);
        assert_eq!(total_boost, 8);

        let mut trim = RangeDecoder::new(&[0; 8]);
        assert_eq!(decode_allocation_trim(&mut trim, 8, 0), Ok(0));
        assert_eq!(decode_allocation_trim(&mut trim, 0, 0), Ok(5));
    }

    #[test]
    fn allocation_controls_reject_invalid_band_ranges() {
        let mut range = RangeDecoder::new(&[0; 8]);
        assert_eq!(
            decode_band_boosts(&mut range, 120, 1, 1, 8, &[0; 21]),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
    }

    #[test]
    fn allocation_controls_apply_caps_from_frame_mode() {
        let mut range = RangeDecoder::new(&[0xff; 8]);
        let controls = decode_allocation_controls(&mut range, 120, 0, 1, 1, 8).unwrap();
        assert_eq!(controls.band_boosts[0], 72);
        assert_eq!(controls.total_boost, 72);
        assert!(controls.trim <= 10);
    }
}
