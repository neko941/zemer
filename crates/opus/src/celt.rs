use crate::postfilter::PostFilter;
use crate::pvq::{
    RecursivePvqAngleContext, SplitAngle, combine_mid_side_shapes, decode_codeword,
    normalize_pulses, pulses_for_allocation, split_stereo_shape_budget, spread_shape,
};
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
const CELT_OVERLAP_SAMPLES: usize = 120;

/// Builds the CELT low-overlap window for one long or short inverse MDCT.
///
/// The basic 240-sample Vorbis-derived window is zero-padded at both ends,
/// then extended with a flat center. This keeps the 120-sample overlap
/// power-complementary at the transform hop for every CELT frame size.
pub(crate) fn celt_low_overlap_window(transform_bins: usize) -> Result<Vec<f64>, RangeError> {
    if ![120, 240, 480, 960].contains(&transform_bins) {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }

    let output_len = transform_bins
        .checked_mul(2)
        .ok_or(RangeError::InvalidCeltEnergyLayout)?;
    let side_padding = (transform_bins - CELT_OVERLAP_SAMPLES) / 2;
    let flat_len = transform_bins - CELT_OVERLAP_SAMPLES;
    let mut window = vec![0.0; output_len];
    window[side_padding + CELT_OVERLAP_SAMPLES..side_padding + CELT_OVERLAP_SAMPLES + flat_len]
        .fill(1.0);
    for index in 0..CELT_OVERLAP_SAMPLES {
        let inner_angle =
            std::f64::consts::FRAC_PI_2 * (index as f64 + 0.5) / CELT_OVERLAP_SAMPLES as f64;
        let inner_sine = inner_angle.sin();
        let value = (std::f64::consts::FRAC_PI_2 * inner_sine * inner_sine).sin();
        window[side_padding + index] = value;
        window[output_len - side_padding - index - 1] = value;
    }
    Ok(window)
}

/// Applies CELT's half-scaled inverse MDCT and low-overlap window.
fn inverse_mdct_windowed(spectrum: &[f32]) -> Result<Vec<f32>, RangeError> {
    if spectrum.iter().any(|value| !value.is_finite()) {
        return Err(RangeError::CorruptFrame);
    }
    let window = celt_low_overlap_window(spectrum.len())?;
    let spectrum = spectrum
        .iter()
        .map(|&value| f64::from(value))
        .collect::<Vec<_>>();
    let unwindowed = zemer_core::transform::inverse_mdct(&spectrum)
        .map_err(|_| RangeError::InvalidCeltEnergyLayout)?;
    let output: Vec<_> = unwindowed
        .into_iter()
        .zip(window)
        .map(|(sample, window)| (sample * 0.5 * window) as f32)
        .collect();
    if output.iter().any(|value| !value.is_finite()) {
        return Err(RangeError::CorruptFrame);
    }
    Ok(output)
}

/// Stateful CELT inverse-MDCT synthesis with center-aligned weighted overlap-add.
#[derive(Default)]
pub(crate) struct CeltSynthesis {
    previous: Option<Vec<Vec<f32>>>,
    postfilter: crate::postfilter::CeltPostFilter,
}

impl CeltSynthesis {
    pub(crate) fn reset(&mut self) {
        self.previous = None;
        self.postfilter.reset();
    }

    /// Denormalizes one mono CELT band set and feeds its spectrum into synthesis.
    pub(crate) fn synthesize_mono_pvq(
        &mut self,
        frame_samples_48k: u16,
        allocation: FineShapeAllocation,
        shapes: &MonoPvqShapes,
        energies_q15: &[i32; 42],
    ) -> Result<Option<Vec<Vec<f32>>>, RangeError> {
        let spectrum =
            denormalize_mono_pvq_shapes(frame_samples_48k, allocation, shapes, energies_q15)?;
        self.synthesize(&[spectrum])
    }

    /// Synthesizes channel-major spectra. The first block primes the overlap state.
    pub(crate) fn synthesize(
        &mut self,
        spectra: &[Vec<f32>],
    ) -> Result<Option<Vec<Vec<f32>>>, RangeError> {
        if spectra.is_empty() {
            return Err(RangeError::InvalidCeltEnergyLayout);
        }
        let mut current = Vec::new();
        current
            .try_reserve_exact(spectra.len())
            .map_err(|_| RangeError::CorruptFrame)?;
        for spectrum in spectra {
            current.push(inverse_mdct_windowed(spectrum)?);
        }

        let Some(previous) = self.previous.as_ref() else {
            self.previous = Some(current);
            return Ok(None);
        };
        let Some(previous_size) = previous.first().map(Vec::len) else {
            return Err(RangeError::CorruptFrame);
        };
        let current_size = current[0].len();
        if previous.len() != current.len()
            || previous
                .iter()
                .any(|channel| channel.len() != previous_size)
            || current.iter().any(|channel| channel.len() != current_size)
        {
            return Err(RangeError::InvalidCeltEnergyLayout);
        }
        let output_frames = previous_size
            .checked_add(current_size)
            .ok_or(RangeError::InvalidCeltEnergyLayout)?
            / 4;
        let current_start = (previous_size as isize - current_size as isize) / 4;
        let mut output = Vec::new();
        output
            .try_reserve_exact(current.len())
            .map_err(|_| RangeError::CorruptFrame)?;
        for (previous_channel, current_channel) in previous.iter().zip(&current) {
            let mut channel = Vec::new();
            channel
                .try_reserve_exact(output_frames)
                .map_err(|_| RangeError::CorruptFrame)?;
            for frame in 0..output_frames {
                let previous_index = previous_size / 2 + frame;
                let current_index = frame as isize - current_start;
                let old = previous_channel.get(previous_index).copied().unwrap_or(0.0);
                let new = usize::try_from(current_index)
                    .ok()
                    .and_then(|index| current_channel.get(index).copied())
                    .unwrap_or(0.0);
                let sample = old + new;
                if !sample.is_finite() {
                    return Err(RangeError::CorruptFrame);
                }
                channel.push(sample);
            }
            output.push(channel);
        }
        self.previous = Some(current);
        Ok(Some(output))
    }

    /// Synthesizes one CELT frame and post-filters its overlap-add PCM output.
    pub(crate) fn synthesize_with_postfilter(
        &mut self,
        spectra: &[Vec<f32>],
        postfilter: Option<PostFilter>,
    ) -> Result<Option<Vec<Vec<f32>>>, RangeError> {
        let mut output = self.synthesize(spectra)?;
        if let Some(samples) = output.as_mut() {
            self.postfilter.process(samples, postfilter)?;
        }
        Ok(output)
    }
}

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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MonoPvqShapes {
    pub(crate) vectors: [Vec<f32>; 21],
    pub(crate) pulses: [u32; 21],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TfResolution {
    pub(crate) selected_table: bool,
    pub(crate) changes: Vec<bool>,
    pub(crate) adjustments: Vec<i8>,
}

pub(crate) fn decode_tf_resolution(
    range: &mut RangeDecoder<'_>,
    frame_samples_48k: u16,
    transient: bool,
    start_band: u8,
    end_band: u8,
) -> Result<TfResolution, RangeError> {
    let lm = frame_lm(frame_samples_48k).ok_or(RangeError::InvalidCeltEnergyLayout)?;
    if start_band >= end_band || end_band > 21 {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }

    let first_symbol = if transient {
        range.decode_symbol(&[3, 1])?
    } else {
        range.decode_symbol(&[15, 1])?
    };
    let mut changes = Vec::with_capacity(usize::from(end_band - start_band));
    changes.push(first_symbol == 1);
    let logp = if transient { 4 } else { 5 };
    while changes.len() < usize::from(end_band - start_band) {
        let toggled = range.decode_bit_logp(logp)?;
        changes.push(*changes.last().unwrap() ^ toggled);
    }

    let table_needed = changes.iter().copied().any(|change| {
        tf_adjustment(lm, transient, false, change) != tf_adjustment(lm, transient, true, change)
    });
    let selected_table = table_needed && range.decode_bit_logp(1)?;
    let adjustments = changes
        .iter()
        .map(|&change| tf_adjustment(lm, transient, selected_table, change))
        .collect();
    Ok(TfResolution {
        selected_table,
        changes,
        adjustments,
    })
}

/// Applies an orthonormal Walsh-Hadamard transform to a CELT TF vector.
///
/// Time-resolution changes use sequency row ordering; frequency-resolution
/// changes use natural row ordering. Axis grouping remains the caller's job.
pub(crate) fn hadamard_tf_vector(values: &mut [f32], sequency: bool) -> Result<(), RangeError> {
    if values.len() < 2
        || values.len() > 128
        || !values.len().is_power_of_two()
        || values.iter().any(|value| !value.is_finite())
    {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }

    let mut group_width = 1;
    while group_width < values.len() {
        for group in values.chunks_exact_mut(group_width * 2) {
            for index in 0..group_width {
                let left = group[index];
                let right = group[index + group_width];
                group[index] = (left + right) * std::f32::consts::FRAC_1_SQRT_2;
                group[index + group_width] = (left - right) * std::f32::consts::FRAC_1_SQRT_2;
            }
        }
        group_width *= 2;
    }

    if sequency {
        let bits = values.len().trailing_zeros();
        let mut natural = [0.0; 128];
        natural[..values.len()].copy_from_slice(values);
        for (index, value) in values.iter_mut().enumerate() {
            let gray = index ^ (index >> 1);
            let natural_row = gray.reverse_bits() >> (usize::BITS - bits);
            *value = natural[natural_row];
        }
    }

    if values.iter().any(|value| !value.is_finite()) {
        return Err(RangeError::CorruptFrame);
    }
    Ok(())
}

/// Applies a Hadamard transform to repeated strided CELT TF axes.
pub(crate) fn hadamard_tf_axis(
    values: &mut [f32],
    axis_length: usize,
    stride: usize,
    sequency: bool,
) -> Result<(), RangeError> {
    let axis_span = axis_length
        .checked_mul(stride)
        .ok_or(RangeError::InvalidCeltEnergyLayout)?;
    if values.is_empty()
        || values.len() > 176
        || !(2..=128).contains(&axis_length)
        || !axis_length.is_power_of_two()
        || stride == 0
        || axis_span > values.len()
        || values.len() % axis_span != 0
    {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }

    let mut axis_storage = [0.0; 128];
    let axis = &mut axis_storage[..axis_length];
    for outer in (0..values.len()).step_by(axis_span) {
        for lane in 0..stride {
            for (index, value) in axis.iter_mut().enumerate() {
                *value = values[outer + index * stride + lane];
            }
            hadamard_tf_vector(axis, sequency)?;
            for (index, &value) in axis.iter().enumerate() {
                values[outer + index * stride + lane] = value;
            }
        }
    }
    Ok(())
}

/// Applies one decoded CELT TF adjustment to a block-major band vector.
///
/// Positive adjustments transform across time blocks in natural order.
/// Negative adjustments transform within each block in sequency order.
fn apply_tf_adjustment(values: &mut [f32], adjustment: i8, blocks: u8) -> Result<(), RangeError> {
    if values.is_empty()
        || values.len() > 176
        || !(1..=8).contains(&blocks)
        || !blocks.is_power_of_two()
        || values.len() % usize::from(blocks) != 0
        || !(-3..=3).contains(&adjustment)
    {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    if adjustment == 0 {
        return Ok(());
    }

    let axis_length = 1_usize << adjustment.unsigned_abs();
    let block_width = values.len() / usize::from(blocks);
    if adjustment > 0 {
        if axis_length > usize::from(blocks) || usize::from(blocks) % axis_length != 0 {
            return Err(RangeError::InvalidCeltEnergyLayout);
        }
        return hadamard_tf_axis(values, axis_length, block_width, false);
    }

    for block in values.chunks_exact_mut(block_width) {
        let complete = block.len() / axis_length * axis_length;
        // Non-power-of-two band widths can leave a short suffix outside the
        // Hadamard axis; preserve those bins unchanged.
        for vector in block[..complete].chunks_exact_mut(axis_length) {
            hadamard_tf_vector(vector, true)?;
        }
    }
    Ok(())
}

fn frame_lm(frame_samples_48k: u16) -> Option<u32> {
    match frame_samples_48k {
        120 | 240 | 480 | 960 => Some(u32::from(frame_samples_48k / 120).trailing_zeros()),
        _ => None,
    }
}

/// Derives the angle quantizer context for a recursive mono band split.
///
/// `depth` counts already-applied parent splits. The angle for the next split
/// uses the remaining transform depth after the parent dimensions are halved.
/// CELT's base-band log widths are stored in eighth-bit units, matching the
/// allocation arithmetic used by the recursive PVQ decoder.
pub(crate) fn recursive_pvq_angle_context(
    frame_samples_48k: u16,
    band: u8,
    depth: u8,
) -> Result<RecursivePvqAngleContext, RangeError> {
    let lm = frame_lm(frame_samples_48k).ok_or(RangeError::InvalidCeltEnergyLayout)?;
    if band >= BAND_LOG2_EIGHTHS.len() as u8 {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    let remaining_lm =
        i32::try_from(lm).map_err(|_| RangeError::InvalidCeltEnergyLayout)? - i32::from(depth) - 1;
    let pulse_cap = i32::from(BAND_LOG2_EIGHTHS[usize::from(band)]) + remaining_lm * 8;
    if pulse_cap < 0 {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    let pulse_cap_eighth_bits = pulse_cap as u32;
    let offset_eighth_bits = (pulse_cap_eighth_bits / 2) as i32 - 4;
    Ok(RecursivePvqAngleContext {
        pulse_cap_eighth_bits,
        offset_eighth_bits,
        time_split: false,
    })
}

fn tf_adjustment(lm: u32, transient: bool, select: bool, change: bool) -> i8 {
    let table = if transient {
        if select {
            [[0, -1], [1, -1], [1, -1], [1, -1]]
        } else {
            [[0, -1], [1, 0], [2, 0], [3, 0]]
        }
    } else if select {
        [[0, -1], [0, -2], [0, -3], [0, -3]]
    } else {
        [[0, -1], [0, -1], [0, -2], [0, -2]]
    };
    table[lm as usize][usize::from(change)]
}

pub(crate) fn decode_spread(range: &mut RangeDecoder<'_>) -> Result<u8, RangeError> {
    Ok(range.decode_symbol(&[7, 2, 21, 2])? as u8)
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

/// Decodes mono, unsplit PVQ shapes and applies spreading and TF adjustments.
///
/// Recursive splits, stereo, denormalization, and inverse-transform synthesis
/// remain separate stages.
pub(crate) fn decode_mono_pvq_shapes(
    range: &mut RangeDecoder<'_>,
    frame_samples_48k: u16,
    allocation: FineShapeAllocation,
    spread: u8,
    blocks: u8,
    tf_adjustments: &[i8; 21],
) -> Result<MonoPvqShapes, RangeError> {
    let edges = band_edges(frame_samples_48k).ok_or(RangeError::InvalidCeltEnergyLayout)?;
    let start_band = allocation.start_band;
    let end_band = allocation.coded_end_band;
    if start_band >= end_band || end_band > 21 {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }

    let mut shapes = std::array::from_fn(|_| Vec::new());
    let mut pulse_counts = [0; 21];
    let mut balance = i64::from(allocation.balance_eighth_bits);
    for band in start_band..end_band {
        let index = usize::from(band);
        let tell = i64::from(range.tell_frac());
        if band != start_band {
            balance -= tell;
        }
        let bands_remaining = i64::from((end_band - band).min(3));
        let compensated =
            i64::from(allocation.shape_eighth_bits[index]) + balance / bands_remaining;
        let budget = compensated.clamp(0, 16_383) as u32;
        let dimensions = usize::from(edges[index + 1] - edges[index]);
        if dimensions == 1 {
            let sign = if budget >= 8 {
                range.read_raw_bits(1)?
            } else {
                0
            };
            shapes[index] = vec![if sign == 0 { 1.0 } else { -1.0 }];
            pulse_counts[index] = 1;
        } else {
            let pulse_allocation = pulses_for_allocation(dimensions, budget)?;
            pulse_counts[index] = pulse_allocation.pulses;
            let pulses = if pulse_allocation.pulses == 0 {
                vec![0; dimensions]
            } else {
                decode_codeword(range, dimensions, pulse_allocation.pulses as usize)?
            };
            shapes[index] = normalize_pulses(&pulses)?;
        }
        spread_shape(&mut shapes[index], pulse_counts[index], spread, blocks)?;
        apply_tf_adjustment(&mut shapes[index], tf_adjustments[index], blocks)?;
        balance += i64::from(allocation.shape_eighth_bits[index]) + tell;
    }
    Ok(MonoPvqShapes {
        vectors: shapes,
        pulses: pulse_counts,
    })
}

/// Decodes a CELT stereo band from its normalized mid/side PVQ vectors.
///
/// The angle has already been decoded. Any unused codebook budget from the
/// first vector is transferred to the second, as required by CELT's stereo
/// allocation rule.
pub(crate) fn decode_mid_side_pvq_band(
    range: &mut RangeDecoder<'_>,
    dimensions: usize,
    remaining_eighth_bits: u32,
    angle: SplitAngle,
    spread: u8,
    blocks: u8,
    tf_adjustment: i8,
) -> Result<(Vec<f32>, Vec<f32>), RangeError> {
    if !(2..=176).contains(&dimensions)
        || angle.angle_q14 > 16_384
        || !angle.left_gain.is_finite()
        || !angle.right_gain.is_finite()
        || !(0.0..=1.0).contains(&angle.left_gain)
        || !(0.0..=1.0).contains(&angle.right_gain)
    {
        return Err(RangeError::InvalidPvqLayout);
    }
    if !(1..=8).contains(&blocks)
        || !blocks.is_power_of_two()
        || dimensions % usize::from(blocks) != 0
        || !(-3..=3).contains(&tf_adjustment)
        || (tf_adjustment > 0 && (1_usize << tf_adjustment as u32) > usize::from(blocks))
    {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    if spread > 3 {
        return Err(RangeError::InvalidPvqSpread);
    }

    let (mid_budget, side_budget) =
        split_stereo_shape_budget(remaining_eighth_bits, dimensions, angle.angle_q14)?;

    // In two dimensions, normalized mid and side shapes are perpendicular.
    // Only one PVQ vector plus a raw sign bit is needed; decoding two
    // independent vectors would waste bits and violate CELT's special case.
    if dimensions == 2 {
        let sign_cost = if angle.angle_q14 == 0 || angle.angle_q14 == 16_384 {
            0
        } else {
            8
        };
        if remaining_eighth_bits < sign_cost {
            return Ok((vec![0.0; dimensions], vec![0.0; dimensions]));
        }
        let mid_first = mid_budget >= side_budget;
        let (coded, _) = decode_stereo_shape(
            range,
            dimensions,
            remaining_eighth_bits - sign_cost,
            spread,
            blocks,
            tf_adjustment,
        )?;
        if sign_cost == 0 {
            let zero = vec![0.0; dimensions];
            return if mid_first {
                combine_mid_side_shapes(&coded, &zero, angle)
            } else {
                combine_mid_side_shapes(&zero, &coded, angle)
            };
        }
        let side_sign = range.read_raw_bits(1)?;
        let (mid, side) = if mid_first {
            let sign = if side_sign == 0 { 1.0 } else { -1.0 };
            (coded.clone(), vec![-coded[1] * sign, coded[0] * sign])
        } else {
            let sign = if side_sign == 0 { 1.0 } else { -1.0 };
            (vec![coded[1] * sign, -coded[0] * sign], coded)
        };
        return combine_mid_side_shapes(&mid, &side, angle);
    }

    let mid_first = mid_budget >= side_budget;
    let (first_budget, second_budget) = if mid_first {
        (mid_budget, side_budget)
    } else {
        (side_budget, mid_budget)
    };
    let (first, first_cost) = decode_stereo_shape(
        range,
        dimensions,
        first_budget,
        spread,
        blocks,
        tf_adjustment,
    )?;
    let second_budget = second_budget
        .checked_add(first_budget - u32::from(first_cost))
        .ok_or(RangeError::InvalidPvqLayout)?;
    let (second, _) = decode_stereo_shape(
        range,
        dimensions,
        second_budget,
        spread,
        blocks,
        tf_adjustment,
    )?;
    let (mid, side) = if mid_first {
        (first, second)
    } else {
        (second, first)
    };
    combine_mid_side_shapes(&mid, &side, angle)
}

fn decode_stereo_shape(
    range: &mut RangeDecoder<'_>,
    dimensions: usize,
    budget_eighth_bits: u32,
    spread: u8,
    blocks: u8,
    tf_adjustment: i8,
) -> Result<(Vec<f32>, u16), RangeError> {
    let allocation = pulses_for_allocation(dimensions, budget_eighth_bits)?;
    let pulses = allocation.pulses as usize;
    let codeword = if pulses == 0 {
        vec![0; dimensions]
    } else {
        decode_codeword(range, dimensions, pulses)?
    };
    let mut shape = normalize_pulses(&codeword)?;
    spread_shape(&mut shape, allocation.pulses, spread, blocks)?;
    apply_tf_adjustment(&mut shape, tf_adjustment, blocks)?;
    Ok((shape, allocation.codebook_eighth_bits))
}

/// Restores the decoded mono band gains on an otherwise zero CELT spectrum.
///
/// Energy values are base-2 log gains in Q15. The RFC 8251 energy ceiling is
/// applied before conversion so corrupt frames cannot overflow the spectrum.
pub(crate) fn denormalize_mono_pvq_shapes(
    frame_samples_48k: u16,
    allocation: FineShapeAllocation,
    shapes: &MonoPvqShapes,
    energies_q15: &[i32; 42],
) -> Result<Vec<f32>, RangeError> {
    let edges = band_edges(frame_samples_48k).ok_or(RangeError::InvalidCeltEnergyLayout)?;
    let start_band = allocation.start_band;
    let end_band = allocation.coded_end_band;
    if start_band >= end_band || end_band > 21 {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }

    let mut spectrum = vec![0.0; usize::from(frame_samples_48k)];
    for band in start_band..end_band {
        let index = usize::from(band);
        let start = usize::from(edges[index]);
        let end = usize::from(edges[index + 1]);
        let shape = &shapes.vectors[index];
        if shape.len() != end - start || shape.iter().any(|value| !value.is_finite()) {
            return Err(RangeError::InvalidPvqLayout);
        }

        let log_gain = (energies_q15[index * 2] as f32 / 32_768.0).min(32.0);
        let gain = log_gain.exp2();
        for (coefficient, &normalized) in spectrum[start..end].iter_mut().zip(shape) {
            *coefficient = normalized * gain;
        }
    }
    Ok(spectrum)
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
        AllocationControls, ENERGY_LAPLACE_MODELS, FineShapeAllocation, FrameHeader, MonoPvqShapes,
        PostFilter, allocation_caps, allocation_thresholds, allocation_trim_offsets,
        apply_tf_adjustment, band_edges, celt_low_overlap_window, decode_allocation_controls,
        decode_allocation_trim, decode_band_boosts, decode_coarse_energy_residuals,
        decode_final_fine_energy, decode_fine_energies, decode_header, decode_mid_side_pvq_band,
        decode_mono_pvq_shapes, decode_spread, decode_tf_resolution, denormalize_mono_pvq_shapes,
        hadamard_tf_axis, hadamard_tf_vector, inverse_mdct_windowed, predict_coarse_energies,
        reallocate_skipped_bands, recursive_pvq_angle_context, reserve_allocation_bits,
        select_band_allocation, split_fine_shape_allocation, static_allocation, tf_adjustment,
    };
    use crate::range::{RangeDecoder, RangeError};

    #[test]
    fn recursive_split_angle_context_tracks_band_width_and_remaining_lm() {
        assert_eq!(
            recursive_pvq_angle_context(120, 12, 0),
            Ok(crate::pvq::RecursivePvqAngleContext {
                pulse_cap_eighth_bits: 8,
                offset_eighth_bits: 0,
                time_split: false,
            })
        );
        for (depth, pulse_cap, offset) in [(0, 32, 12), (1, 24, 8), (2, 16, 4)] {
            assert_eq!(
                recursive_pvq_angle_context(960, 12, depth),
                Ok(crate::pvq::RecursivePvqAngleContext {
                    pulse_cap_eighth_bits: pulse_cap,
                    offset_eighth_bits: offset,
                    time_split: false,
                })
            );
        }
    }

    #[test]
    fn recursive_split_angle_context_rejects_invalid_band_or_impossible_depth() {
        assert_eq!(
            recursive_pvq_angle_context(120, 21, 0),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            recursive_pvq_angle_context(120, 0, 0),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            recursive_pvq_angle_context(60, 12, 0),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
    }

    #[test]
    fn low_overlap_window_has_power_complementary_edges_at_all_celt_sizes() {
        for transform_bins in [120, 240, 480, 960] {
            let window = celt_low_overlap_window(transform_bins).unwrap();
            assert_eq!(window.len(), transform_bins * 2);
            for index in 0..transform_bins {
                let paired = window[index + transform_bins];
                let energy = window[index] * window[index] + paired * paired;
                assert!(
                    (energy - 1.0).abs() < 1e-12,
                    "bins={transform_bins}, index={index}"
                );
            }
            let side_padding = (transform_bins - 120) / 2;
            let flat_start = side_padding + 120;
            let flat_end = flat_start + transform_bins - 120;
            assert!(window[..side_padding].iter().all(|&value| value == 0.0));
            assert!(
                window[flat_start..flat_end]
                    .iter()
                    .all(|&value| value == 1.0)
            );
            assert!(
                window[2 * transform_bins - side_padding..]
                    .iter()
                    .all(|&value| value == 0.0)
            );
            if side_padding == 0 {
                assert!(window[0] < 0.0001);
                assert!(window[119] > 0.9999);
            }
        }
    }

    #[test]
    fn low_overlap_window_transitions_are_power_complementary_across_sizes() {
        for previous_bins in [120, 240, 480, 960] {
            let previous = celt_low_overlap_window(previous_bins).unwrap();
            for current_bins in [120, 240, 480, 960] {
                let current = celt_low_overlap_window(current_bins).unwrap();
                let output_frames = (previous_bins + current_bins) / 2;
                let current_start = (previous_bins as isize - current_bins as isize) / 2;
                for frame in 0..output_frames {
                    let previous_index = previous_bins + frame;
                    let current_index = frame as isize - current_start;
                    let old = previous.get(previous_index).copied().unwrap_or(0.0);
                    let new = usize::try_from(current_index)
                        .ok()
                        .and_then(|index| current.get(index).copied())
                        .unwrap_or(0.0);
                    let energy = old * old + new * new;
                    assert!(
                        (energy - 1.0).abs() < 1e-12,
                        "previous={previous_bins}, current={current_bins}, frame={frame}"
                    );
                }
            }
        }
    }

    #[test]
    fn celt_synthesis_overlap_adds_all_frame_size_transitions() {
        use super::CeltSynthesis;

        for previous_bins in [120, 240, 480, 960] {
            for current_bins in [120, 240, 480, 960] {
                let mut synthesis = CeltSynthesis::default();
                assert!(
                    synthesis
                        .synthesize(&vec![vec![0.0; previous_bins]; 2])
                        .unwrap()
                        .is_none()
                );
                let output = synthesis
                    .synthesize(&vec![vec![0.0; current_bins]; 2])
                    .unwrap()
                    .unwrap();
                let expected_frames = (previous_bins + current_bins) / 2;
                assert_eq!(output.len(), 2);
                assert!(
                    output
                        .iter()
                        .all(|channel| channel.len() == expected_frames)
                );
                assert!(output.iter().flatten().all(|&sample| sample == 0.0));
            }
        }
    }

    #[test]
    fn celt_synthesis_applies_postfilter_after_overlap_add() {
        use super::CeltSynthesis;

        let params = PostFilter {
            period: 15,
            gain: 7,
            tapset: 2,
        };
        let spectrum = vec![1.0; 120];
        let mut plain = CeltSynthesis::default();
        let mut filtered = CeltSynthesis::default();
        assert!(
            plain
                .synthesize(std::slice::from_ref(&spectrum))
                .unwrap()
                .is_none()
        );
        assert!(
            filtered
                .synthesize_with_postfilter(std::slice::from_ref(&spectrum), Some(params))
                .unwrap()
                .is_none()
        );

        let plain = plain
            .synthesize(std::slice::from_ref(&spectrum))
            .unwrap()
            .unwrap();
        let filtered = filtered
            .synthesize_with_postfilter(std::slice::from_ref(&spectrum), Some(params))
            .unwrap()
            .unwrap();
        assert_eq!(plain[0].len(), filtered[0].len());
        assert_ne!(plain, filtered);
    }

    #[test]
    fn celt_synthesis_reset_and_rejects_channel_count_changes() {
        use super::CeltSynthesis;

        let mut synthesis = CeltSynthesis::default();
        assert!(synthesis.synthesize(&[vec![0.0; 120]]).unwrap().is_none());
        assert_eq!(
            synthesis.synthesize(&[vec![0.0; 120], vec![0.0; 120]]),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert!(synthesis.synthesize(&[vec![0.0; 120]]).unwrap().is_some());
        synthesis.reset();
        assert!(synthesis.synthesize(&[vec![0.0; 240]]).unwrap().is_none());
    }

    #[test]
    fn celt_mono_pvq_shapes_flow_through_denormalization_and_synthesis() {
        use super::CeltSynthesis;

        let allocation = FineShapeAllocation {
            start_band: 0,
            coded_end_band: 1,
            shape_eighth_bits: [0; 21],
            fine_bits: [0; 21],
            fine_priority: [false; 21],
            balance_eighth_bits: 0,
        };
        let mut vectors: [Vec<f32>; 21] = std::array::from_fn(|_| Vec::new());
        vectors[0] = vec![1.0];
        let shapes = MonoPvqShapes {
            vectors,
            pulses: [1; 21],
        };
        let energies = [0; 42];
        let mut synthesis = CeltSynthesis::default();
        assert!(
            synthesis
                .synthesize_mono_pvq(120, allocation, &shapes, &energies)
                .unwrap()
                .is_none()
        );
        let output = synthesis
            .synthesize_mono_pvq(120, allocation, &shapes, &energies)
            .unwrap()
            .unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].len(), 120);
        assert!(output[0].iter().all(|sample| sample.is_finite()));
        assert!(output[0].iter().any(|sample| *sample != 0.0));
    }

    #[test]
    fn invalid_mono_shape_does_not_consume_synthesis_history() {
        use super::CeltSynthesis;

        let allocation = FineShapeAllocation {
            start_band: 0,
            coded_end_band: 1,
            shape_eighth_bits: [0; 21],
            fine_bits: [0; 21],
            fine_priority: [false; 21],
            balance_eighth_bits: 0,
        };
        let mut valid_vectors: [Vec<f32>; 21] = std::array::from_fn(|_| Vec::new());
        valid_vectors[0] = vec![1.0];
        let valid = MonoPvqShapes {
            vectors: valid_vectors,
            pulses: [1; 21],
        };
        let mut invalid_vectors: [Vec<f32>; 21] = std::array::from_fn(|_| Vec::new());
        invalid_vectors[0] = vec![1.0, 0.0];
        let invalid = MonoPvqShapes {
            vectors: invalid_vectors,
            pulses: [1; 21],
        };
        let energies = [0; 42];
        let mut synthesis = CeltSynthesis::default();
        assert!(
            synthesis
                .synthesize_mono_pvq(120, allocation, &valid, &energies)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            synthesis.synthesize_mono_pvq(120, allocation, &invalid, &energies),
            Err(RangeError::InvalidPvqLayout)
        );
        assert!(
            synthesis
                .synthesize_mono_pvq(120, allocation, &valid, &energies)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn inverse_mdct_window_applies_half_scale_and_celt_lapping_shape() {
        for transform_bins in [120, 240, 480, 960] {
            let spectrum: Vec<f32> = (0..transform_bins)
                .map(|index| ((index * 19 % 41) as f32 - 20.0) / 29.0)
                .collect();
            let actual = inverse_mdct_windowed(&spectrum).unwrap();
            let spectrum_f64: Vec<_> = spectrum.iter().map(|&value| f64::from(value)).collect();
            let unwindowed = zemer_core::transform::inverse_mdct(&spectrum_f64).unwrap();
            let window = celt_low_overlap_window(transform_bins).unwrap();
            assert_eq!(actual.len(), transform_bins * 2);
            for ((&sample, &raw), &weight) in actual.iter().zip(&unwindowed).zip(&window) {
                let expected = (raw * 0.5 * weight) as f32;
                assert!((sample - expected).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn low_overlap_window_and_inverse_mdct_reject_invalid_inputs() {
        assert_eq!(
            celt_low_overlap_window(60),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            celt_low_overlap_window(1920),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            inverse_mdct_windowed(&[f32::NAN; 120]),
            Err(RangeError::CorruptFrame)
        );
        assert_eq!(
            inverse_mdct_windowed(&[0.0; 60]),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
    }

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
    fn mono_shape_decode_uses_frame_scaled_band_dimensions() {
        for (frame_samples, expected_dimensions) in [(120, 1), (240, 2), (480, 4), (960, 8)] {
            let allocation = FineShapeAllocation {
                start_band: 0,
                coded_end_band: 1,
                shape_eighth_bits: [64; 21],
                fine_bits: [0; 21],
                fine_priority: [false; 21],
                balance_eighth_bits: 0,
            };
            let mut range = RangeDecoder::new(&[0x5a; 32]);
            let shapes =
                decode_mono_pvq_shapes(&mut range, frame_samples, allocation, 0, 1, &[0; 21])
                    .unwrap();
            assert_eq!(shapes.vectors[0].len(), expected_dimensions);
            assert!(shapes.vectors[1..].iter().all(Vec::is_empty));
            let norm_squared = shapes.vectors[0]
                .iter()
                .map(|&value| f64::from(value) * f64::from(value))
                .sum::<f64>();
            assert!((norm_squared - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn mono_shape_denormalization_restores_q15_log_gain_and_caps_extremes() {
        let allocation = FineShapeAllocation {
            start_band: 8,
            coded_end_band: 9,
            shape_eighth_bits: [0; 21],
            fine_bits: [0; 21],
            fine_priority: [false; 21],
            balance_eighth_bits: 0,
        };
        let mut vectors = std::array::from_fn(|_| Vec::new());
        vectors[8] = vec![0.6, 0.8];
        let shapes = MonoPvqShapes {
            vectors,
            pulses: [0; 21],
        };
        let mut energies = [0; 42];
        energies[16] = 32_768;

        let spectrum = denormalize_mono_pvq_shapes(120, allocation, &shapes, &energies).unwrap();
        assert_eq!(spectrum.len(), 120);
        assert_eq!(spectrum[8..10], [1.2, 1.6]);
        assert!(spectrum[..8].iter().all(|&value| value == 0.0));
        assert!(spectrum[10..].iter().all(|&value| value == 0.0));

        energies[16] = 40 * 32_768;
        let capped = denormalize_mono_pvq_shapes(120, allocation, &shapes, &energies).unwrap();
        assert_eq!(capped[8], 0.6 * 2.0_f32.powi(32));
    }

    #[test]
    fn mono_shape_denormalization_rejects_malformed_band_shapes() {
        let allocation = FineShapeAllocation {
            start_band: 8,
            coded_end_band: 9,
            shape_eighth_bits: [0; 21],
            fine_bits: [0; 21],
            fine_priority: [false; 21],
            balance_eighth_bits: 0,
        };
        let mut vectors = std::array::from_fn(|_| Vec::new());
        vectors[8] = vec![1.0];
        let shapes = MonoPvqShapes {
            vectors,
            pulses: [0; 21],
        };
        assert_eq!(
            denormalize_mono_pvq_shapes(120, allocation, &shapes, &[0; 42]),
            Err(RangeError::InvalidPvqLayout)
        );
    }

    #[test]
    fn one_bin_mono_band_reads_its_raw_sign_bit() {
        let allocation = FineShapeAllocation {
            start_band: 0,
            coded_end_band: 1,
            shape_eighth_bits: [8; 21],
            fine_bits: [0; 21],
            fine_priority: [false; 21],
            balance_eighth_bits: 0,
        };
        for (payload, expected) in [(0, 1.0), (1, -1.0)] {
            let bytes = [0x00, payload];
            let mut range = RangeDecoder::new(&bytes);
            let shapes =
                decode_mono_pvq_shapes(&mut range, 120, allocation, 0, 1, &[0; 21]).unwrap();
            assert_eq!(shapes.vectors[0], vec![expected]);
            assert_eq!(shapes.pulses[0], 1);
        }
    }

    #[test]
    fn zero_pulse_band_does_not_decode_a_codeword() {
        let allocation = FineShapeAllocation {
            start_band: 8,
            coded_end_band: 9,
            shape_eighth_bits: [0; 21],
            fine_bits: [0; 21],
            fine_priority: [false; 21],
            balance_eighth_bits: 0,
        };
        let mut range = RangeDecoder::new(&[0x00, 0x00]);
        let before = range.tell_frac();
        let shapes = decode_mono_pvq_shapes(&mut range, 120, allocation, 0, 1, &[0; 21]).unwrap();
        assert_eq!(shapes.vectors[8], vec![0.0, 0.0]);
        assert_eq!(shapes.pulses[8], 0);
        assert_eq!(range.tell_frac(), before);
    }

    #[test]
    fn mono_shape_rebalances_unused_bits_into_the_final_band() {
        let mut shape_eighth_bits = [0; 21];
        shape_eighth_bits[12] = 39;
        shape_eighth_bits[13] = 26;
        let allocation = FineShapeAllocation {
            start_band: 12,
            coded_end_band: 14,
            shape_eighth_bits,
            fine_bits: [0; 21],
            fine_priority: [false; 21],
            balance_eighth_bits: 0,
        };
        let mut range = RangeDecoder::new(&[0x21; 128]);
        let shapes = decode_mono_pvq_shapes(&mut range, 120, allocation, 0, 1, &[0; 21]).unwrap();
        assert_eq!(shapes.pulses[12], 1);
        assert_eq!(shapes.pulses[13], 2);
    }

    #[test]
    fn two_dimensional_stereo_decodes_one_shape_and_a_raw_sign_bit() {
        let angle = crate::pvq::SplitAngle {
            resolution: 2,
            index: 1,
            angle_q14: 8_192,
            left_gain: std::f32::consts::FRAC_1_SQRT_2,
            right_gain: std::f32::consts::FRAC_1_SQRT_2,
        };
        assert_eq!(crate::pvq::pulses_for_allocation(2, 32).unwrap().pulses, 4);

        let bytes = [0x53; 32];
        let mut actual_range = RangeDecoder::new(&bytes);
        let (left, right) =
            decode_mid_side_pvq_band(&mut actual_range, 2, 40, angle, 0, 1, 0).unwrap();

        let mut expected_range = RangeDecoder::new(&bytes);
        crate::pvq::decode_codeword(&mut expected_range, 2, 4).unwrap();
        expected_range.read_raw_bits(1).unwrap();
        assert_eq!(actual_range.tell_frac(), expected_range.tell_frac());
        for channel in [left, right] {
            let norm_squared = channel
                .iter()
                .map(|&value| f64::from(value) * f64::from(value))
                .sum::<f64>();
            assert!((norm_squared - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn two_dimensional_stereo_endpoints_omit_the_unneeded_sign_bit() {
        for (angle_q14, left_gain, right_gain) in [(0, 1.0, 0.0), (16_384, 0.0, 1.0)] {
            let angle = crate::pvq::SplitAngle {
                resolution: 2,
                index: if angle_q14 == 0 { 0 } else { 2 },
                angle_q14,
                left_gain,
                right_gain,
            };
            let bytes = [0x53; 32];
            let mut actual_range = RangeDecoder::new(&bytes);
            let (left, right) =
                decode_mid_side_pvq_band(&mut actual_range, 2, 40, angle, 0, 1, 0).unwrap();

            let mut expected_range = RangeDecoder::new(&bytes);
            let allocation = crate::pvq::pulses_for_allocation(2, 40).unwrap();
            crate::pvq::decode_codeword(&mut expected_range, 2, allocation.pulses as usize)
                .unwrap();
            assert_eq!(actual_range.tell_frac(), expected_range.tell_frac());
            if angle_q14 == 0 {
                assert_eq!(left, right);
            } else {
                assert_eq!(left, right.iter().map(|value| -*value).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn stereo_shape_decode_keeps_unallocated_channels_zero_without_consuming_bits() {
        let angle = crate::pvq::SplitAngle {
            resolution: 2,
            index: 1,
            angle_q14: 8_192,
            left_gain: std::f32::consts::FRAC_1_SQRT_2,
            right_gain: std::f32::consts::FRAC_1_SQRT_2,
        };
        let mut range = RangeDecoder::new(&[0x53; 32]);
        let before = range.tell_frac();
        assert_eq!(
            decode_mid_side_pvq_band(&mut range, 2, 0, angle, 0, 1, 0),
            Ok((vec![0.0, 0.0], vec![0.0, 0.0]))
        );
        assert_eq!(range.tell_frac(), before);
    }

    #[test]
    fn stereo_shape_decode_codes_the_larger_side_vector_first() {
        let angle = crate::pvq::SplitAngle {
            resolution: 4,
            index: 3,
            angle_q14: 12_288,
            left_gain: 0.382_683_43,
            right_gain: 0.923_879_5,
        };
        let (mid_budget, side_budget) =
            crate::pvq::split_stereo_shape_budget(160, 4, angle.angle_q14).unwrap();
        assert!(side_budget > mid_budget);
        let first = crate::pvq::pulses_for_allocation(4, side_budget).unwrap();
        let second = crate::pvq::pulses_for_allocation(
            4,
            mid_budget + side_budget - u32::from(first.codebook_eighth_bits),
        )
        .unwrap();
        assert!(first.pulses > second.pulses);

        let bytes = [0x37; 32];
        let mut actual_range = RangeDecoder::new(&bytes);
        decode_mid_side_pvq_band(&mut actual_range, 4, 160, angle, 0, 1, 0).unwrap();
        let mut expected_range = RangeDecoder::new(&bytes);
        crate::pvq::decode_codeword(&mut expected_range, 4, first.pulses as usize).unwrap();
        crate::pvq::decode_codeword(&mut expected_range, 4, second.pulses as usize).unwrap();
        assert_eq!(actual_range.tell_frac(), expected_range.tell_frac());
    }

    #[test]
    fn mono_shape_decode_rejects_invalid_frame_layout_before_reading() {
        let allocation = FineShapeAllocation {
            start_band: 0,
            coded_end_band: 1,
            shape_eighth_bits: [64; 21],
            fine_bits: [0; 21],
            fine_priority: [false; 21],
            balance_eighth_bits: 0,
        };
        let mut range = RangeDecoder::new(&[0x5a; 32]);
        assert_eq!(
            decode_mono_pvq_shapes(&mut range, 60, allocation, 0, 1, &[0; 21]),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            decode_mono_pvq_shapes(
                &mut range,
                120,
                FineShapeAllocation {
                    start_band: 2,
                    ..allocation
                },
                0,
                1,
                &[0; 21],
            ),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
    }

    #[test]
    fn spread_symbol_uses_the_normative_four_value_distribution() {
        for bytes in [[0x00], [0x40], [0x80], [0xc0], [0xff]] {
            let mut range = RangeDecoder::new(&bytes);
            assert!(decode_spread(&mut range).unwrap() <= 3);
        }
    }

    #[test]
    fn tf_adjustment_tables_match_all_frame_modes() {
        let nontransient_select0 = [[0, -1], [0, -1], [0, -2], [0, -2]];
        let nontransient_select1 = [[0, -1], [0, -2], [0, -3], [0, -3]];
        let transient_select0 = [[0, -1], [1, 0], [2, 0], [3, 0]];
        let transient_select1 = [[0, -1], [1, -1], [1, -1], [1, -1]];
        for lm in 0..4 {
            for change in [false, true] {
                let column = usize::from(change);
                assert_eq!(
                    tf_adjustment(lm, false, false, change),
                    nontransient_select0[lm as usize][column]
                );
                assert_eq!(
                    tf_adjustment(lm, false, true, change),
                    nontransient_select1[lm as usize][column]
                );
                assert_eq!(
                    tf_adjustment(lm, true, false, change),
                    transient_select0[lm as usize][column]
                );
                assert_eq!(
                    tf_adjustment(lm, true, true, change),
                    transient_select1[lm as usize][column]
                );
            }
        }
    }

    #[test]
    fn tf_resolution_decodes_relative_band_changes() {
        let bytes = [0; 64];
        let mut range = RangeDecoder::new(&bytes);
        let decoded = decode_tf_resolution(&mut range, 480, true, 2, 7).unwrap();
        assert_eq!(decoded.changes, [false; 5]);
        assert_eq!(decoded.adjustments, [2; 5]);
        assert!(!decoded.selected_table);
    }

    #[test]
    fn hadamard_tf_vectors_use_orthonormal_natural_and_sequency_rows() {
        let mut natural = [1.0, 2.0, 3.0, 4.0];
        hadamard_tf_vector(&mut natural, false).unwrap();
        for (actual, expected) in natural.into_iter().zip([5.0, -1.0, -2.0, 0.0]) {
            assert!((actual - expected).abs() < 1e-6);
        }

        let mut sequency = [1.0, 2.0, 3.0, 4.0];
        hadamard_tf_vector(&mut sequency, true).unwrap();
        for (actual, expected) in sequency.into_iter().zip([5.0, -2.0, 0.0, -1.0]) {
            assert!((actual - expected).abs() < 1e-6);
        }
        let energy = sequency.iter().map(|value| value * value).sum::<f32>();
        assert_eq!(energy, 30.0);
    }

    #[test]
    fn hadamard_tf_vector_rejects_invalid_inputs() {
        assert_eq!(
            hadamard_tf_vector(&mut [], false),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            hadamard_tf_vector(&mut [1.0, 2.0, 3.0], false),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(
            hadamard_tf_vector(&mut [f32::NAN, 0.0], true),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
    }

    #[test]
    fn hadamard_tf_axis_transforms_strided_axes_independently() {
        let mut interleaved = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        hadamard_tf_axis(&mut interleaved, 2, 2, false).unwrap();
        let expected = [
            8.0_f32.sqrt(),
            18.0_f32.sqrt(),
            -2.0_f32.sqrt(),
            -2.0_f32.sqrt(),
            72.0_f32.sqrt(),
            98.0_f32.sqrt(),
            -2.0_f32.sqrt(),
            -2.0_f32.sqrt(),
        ];
        for (actual, expected) in interleaved.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn hadamard_tf_axis_rejects_invalid_geometry() {
        for (axis_length, stride) in [(1, 1), (3, 1), (2, 0), (4, 2)] {
            let mut values = [1.0, 2.0, 3.0, 4.0];
            assert_eq!(
                hadamard_tf_axis(&mut values, axis_length, stride, false),
                Err(RangeError::InvalidCeltEnergyLayout)
            );
        }
    }

    #[test]
    fn positive_tf_adjustment_transforms_across_short_blocks() {
        let mut values = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        apply_tf_adjustment(&mut values, 1, 2).unwrap();
        let expected = [
            18.0_f32.sqrt(),
            32.0_f32.sqrt(),
            50.0_f32.sqrt(),
            72.0_f32.sqrt(),
            -8.0_f32.sqrt(),
            -8.0_f32.sqrt(),
            -8.0_f32.sqrt(),
            -8.0_f32.sqrt(),
        ];
        for (actual, expected) in values.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn negative_tf_adjustment_transforms_each_block_in_sequency_order() {
        let mut values = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        apply_tf_adjustment(&mut values, -2, 2).unwrap();
        let expected = [5.0, -2.0, 0.0, -1.0, 13.0, -2.0, 0.0, -1.0];
        for (actual, expected) in values.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn negative_tf_adjustment_keeps_an_incomplete_tail_unchanged() {
        let mut values = [1.0, 2.0, 3.0];
        apply_tf_adjustment(&mut values, -1, 1).unwrap();
        assert!((values[0] - 3.0 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((values[1] + std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert_eq!(values[2], 3.0);
    }

    #[test]
    fn tf_adjustment_rejects_impossible_block_geometry_without_mutation() {
        let mut too_few_blocks = [1.0, 2.0];
        assert_eq!(
            apply_tf_adjustment(&mut too_few_blocks, 1, 1),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(too_few_blocks, [1.0, 2.0]);

        let mut non_power_of_two_blocks = [1.0; 6];
        assert_eq!(
            apply_tf_adjustment(&mut non_power_of_two_blocks, 0, 3),
            Err(RangeError::InvalidCeltEnergyLayout)
        );
        assert_eq!(non_power_of_two_blocks, [1.0; 6]);
    }

    #[test]
    fn mono_shape_decode_applies_the_per_band_tf_adjustment() {
        let allocation = FineShapeAllocation {
            start_band: 0,
            coded_end_band: 1,
            shape_eighth_bits: [64; 21],
            fine_bits: [0; 21],
            fine_priority: [false; 21],
            balance_eighth_bits: 0,
        };
        let bytes = [0x5a; 64];
        let mut plain_range = RangeDecoder::new(&bytes);
        let mut tf_range = RangeDecoder::new(&bytes);
        let plain =
            decode_mono_pvq_shapes(&mut plain_range, 480, allocation, 0, 2, &[0; 21]).unwrap();
        let mut adjustments = [0; 21];
        adjustments[0] = -1;
        let transformed =
            decode_mono_pvq_shapes(&mut tf_range, 480, allocation, 0, 2, &adjustments).unwrap();

        let mut expected = plain.vectors[0].clone();
        apply_tf_adjustment(&mut expected, -1, 2).unwrap();
        assert_eq!(transformed.vectors[0], expected);
        let transformed_norm = transformed.vectors[0]
            .iter()
            .map(|&value| f64::from(value) * f64::from(value))
            .sum::<f64>();
        assert!((transformed_norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn tf_select_is_omitted_when_the_frame_table_cannot_change_results() {
        let bytes = [0xa5; 64];
        let mut decoded_range = RangeDecoder::new(&bytes);
        let decoded = decode_tf_resolution(&mut decoded_range, 120, false, 0, 1).unwrap();
        assert!(!decoded.selected_table);
        let next_after_decode = decode_spread(&mut decoded_range).unwrap();

        let mut expected_range = RangeDecoder::new(&bytes);
        expected_range.decode_symbol(&[15, 1]).unwrap();
        let expected_next = decode_spread(&mut expected_range).unwrap();
        assert_eq!(next_after_decode, expected_next);
    }

    #[test]
    fn tf_select_is_consumed_when_the_frame_table_changes_results() {
        let bytes = [0; 64];
        let mut decoded_range = RangeDecoder::new(&bytes);
        let decoded = decode_tf_resolution(&mut decoded_range, 480, true, 0, 1).unwrap();
        assert!(!decoded.selected_table);
        let next_after_decode = decode_spread(&mut decoded_range).unwrap();

        let mut expected_range = RangeDecoder::new(&bytes);
        expected_range.decode_symbol(&[3, 1]).unwrap();
        assert!(!expected_range.decode_bit_logp(1).unwrap());
        let expected_next = decode_spread(&mut expected_range).unwrap();
        assert_eq!(next_after_decode, expected_next);
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
