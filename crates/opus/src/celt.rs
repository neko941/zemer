use crate::range::{RangeDecoder, RangeError};

const BAND_WIDTHS_2_5_MS: [u16; 21] = [
    1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 6, 6, 8, 12, 18, 22,
];

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
            let index =
                usize::from(band - start_band) * usize::from(channels) + usize::from(channel);
            residuals[index] = residual as i16;
        }
    }
    Ok(residuals)
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
        ENERGY_LAPLACE_MODELS, FrameHeader, band_edges, decode_coarse_energy_residuals,
        decode_header,
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
}
