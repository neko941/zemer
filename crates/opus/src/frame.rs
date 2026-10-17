//! CELT frame stages composed in normative range-coded order.

use crate::celt::{
    AllocationControls, AllocationReservations, BandAllocation, CeltSynthesis, FinalFineEnergy,
    FineShapeAllocation, FrameHeader, MonoPvqShapes, ReallocatedBands, TfResolution,
    apply_mono_anti_collapse, decode_allocation_controls, decode_coarse_energy_residuals,
    decode_final_fine_energy, decode_fine_energies, decode_header, decode_mono_pvq_shapes_bounded,
    decode_spread, decode_tf_resolution, denormalize_mono_pvq_shapes, predict_coarse_energies,
    reallocate_skipped_bands, reserve_allocation_bits, select_band_allocation,
    split_fine_shape_allocation,
};
use crate::range::{RangeDecoder, RangeError};

/// Header and coarse energy state produced by the first CELT frame stages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MonoCeltEnergyPrefix {
    pub(crate) header: FrameHeader,
    pub(crate) energies_q15: [i32; 42],
}

/// Mono CELT state decoded through fine energy, immediately before PVQ shapes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MonoCeltFramePrefix {
    pub(crate) header: FrameHeader,
    pub(crate) tf_resolution: TfResolution,
    pub(crate) spread: u8,
    pub(crate) controls: AllocationControls,
    pub(crate) reservations: AllocationReservations,
    pub(crate) band_allocation: Option<BandAllocation>,
    pub(crate) reallocated: Option<ReallocatedBands>,
    pub(crate) fine_shape: Option<FineShapeAllocation>,
    pub(crate) energies_q15: [i32; 42],
}

/// Final coarse-energy history carried between sequential mono CELT frames.
///
/// The caller resets this when the CELT decoder state is reset, and commits a
/// frame only after its final fine energy has been decoded successfully.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MonoCeltEnergyHistory {
    previous_final_energies_q15: [i32; 42],
    previous_previous_final_energies_q15: [i32; 42],
    anti_collapse_seed: u32,
}

impl Default for MonoCeltEnergyHistory {
    fn default() -> Self {
        Self {
            previous_final_energies_q15: [0; 42],
            previous_previous_final_energies_q15: [0; 42],
            anti_collapse_seed: 0,
        }
    }
}

impl MonoCeltEnergyHistory {
    pub(crate) fn reset(&mut self) {
        self.previous_final_energies_q15 = [0; 42];
        self.previous_previous_final_energies_q15 = [0; 42];
        self.anti_collapse_seed = 0;
    }

    pub(crate) fn decode_prefix(
        &self,
        range: &mut RangeDecoder<'_>,
        frame_samples_48k: u16,
        frame_bytes: usize,
        end_band: u8,
    ) -> Result<Option<MonoCeltFramePrefix>, RangeError> {
        decode_mono_frame_prefix(
            range,
            frame_samples_48k,
            frame_bytes,
            end_band,
            &self.previous_final_energies_q15,
        )
    }

    /// Decodes one supported active mono CELT frame and commits its energy
    /// history only after the frame tail succeeds. Silence and recursive PVQ
    /// splits are not yet handled by this path.
    pub(crate) fn decode_active_frame(
        &mut self,
        range: &mut RangeDecoder<'_>,
        frame_samples_48k: u16,
        frame_bytes: usize,
        end_band: u8,
        synthesis: &mut CeltSynthesis,
    ) -> Result<MonoCeltPcmFrame, RangeError> {
        let prefix = self
            .decode_prefix(range, frame_samples_48k, frame_bytes, end_band)?
            .ok_or(RangeError::UnsupportedCeltMode)?;
        let prior_energies = [
            self.previous_final_energies_q15,
            self.previous_previous_final_energies_q15,
        ];
        let frame = decode_mono_celt_frame(
            range,
            frame_samples_48k,
            frame_bytes,
            &prefix,
            synthesis,
            &prior_energies,
            self.anti_collapse_seed,
        )?;
        self.commit(&frame);
        Ok(frame)
    }

    pub(crate) fn commit(&mut self, frame: &MonoCeltPcmFrame) {
        self.previous_previous_final_energies_q15 = self.previous_final_energies_q15;
        self.previous_final_energies_q15 = frame.final_energy.energies_q15;
        self.anti_collapse_seed = frame.anti_collapse_seed;
    }
}

/// Decoded mono CELT frame data after PVQ, final energy, and synthesis.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MonoCeltPcmFrame {
    pub(crate) shapes: MonoPvqShapes,
    pub(crate) final_energy: FinalFineEnergy,
    pub(crate) spectrum: Vec<f32>,
    pub(crate) samples: Option<Vec<f32>>,
    pub(crate) final_range: u32,
    anti_collapse_seed: u32,
}

/// Decodes the CELT header and mono coarse energy in bitstream order.
///
/// The caller supplies the previous frame's finalized energies. This stage
/// does not update that history: CELT energy prediction is based on the final
/// fine-energy values, which are decoded later in the frame.
pub(crate) fn decode_mono_energy_prefix(
    range: &mut RangeDecoder<'_>,
    frame_samples_48k: u16,
    frame_bytes: usize,
    end_band: u8,
    previous_final_energies_q15: &[i32; 42],
) -> Result<MonoCeltEnergyPrefix, RangeError> {
    if crate::celt::band_edges(frame_samples_48k).is_none()
        || frame_bytes == 0
        || end_band == 0
        || end_band > 21
    {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }

    let header = decode_header(range)?;
    if header.silence {
        return Ok(MonoCeltEnergyPrefix {
            header,
            energies_q15: [0; 42],
        });
    }

    let residuals = decode_coarse_energy_residuals(
        range,
        frame_samples_48k,
        header.intra,
        0,
        end_band,
        1,
        frame_bytes,
    )?;
    let energies_q15 = predict_coarse_energies(
        &residuals,
        previous_final_energies_q15,
        frame_samples_48k,
        header.intra,
        end_band,
        1,
    )?;
    Ok(MonoCeltEnergyPrefix {
        header,
        energies_q15,
    })
}

/// Decodes the mono CELT frame prefix through fine energy, before PVQ residuals.
///
/// `None` represents a silence frame, which terminates CELT side-information
/// decoding after the header. Low-budget active frames can carry a zero-bit
/// allocation and still proceed through the allocation stages.
pub(crate) fn decode_mono_frame_prefix(
    range: &mut RangeDecoder<'_>,
    frame_samples_48k: u16,
    frame_bytes: usize,
    end_band: u8,
    previous_final_energies_q15: &[i32; 42],
) -> Result<Option<MonoCeltFramePrefix>, RangeError> {
    let energy = decode_mono_energy_prefix(
        range,
        frame_samples_48k,
        frame_bytes,
        end_band,
        previous_final_energies_q15,
    )?;
    if energy.header.silence {
        return Ok(None);
    }

    let tf_resolution = decode_tf_resolution(
        range,
        frame_samples_48k,
        energy.header.transient,
        0,
        end_band,
    )?;
    let spread = decode_spread(range)?;
    let controls =
        decode_allocation_controls(range, frame_samples_48k, 0, end_band, 1, frame_bytes)?;
    let remaining_eighth_bits = frame_bytes
        .saturating_mul(64)
        .saturating_sub(range.tell_frac() as usize)
        .try_into()
        .map_err(|_| RangeError::CorruptFrame)?;
    let reservations = reserve_allocation_bits(
        frame_samples_48k,
        0,
        end_band,
        1,
        energy.header.transient,
        remaining_eighth_bits,
    )?;
    let band_allocation = select_band_allocation(
        frame_samples_48k,
        0,
        end_band,
        1,
        &controls,
        reservations.remaining_eighth_bits,
    )?;
    let (reallocated, fine_shape, energies_q15) = if let Some(allocation) = band_allocation {
        let reallocated = reallocate_skipped_bands(
            range,
            frame_samples_48k,
            1,
            &controls,
            reservations,
            allocation,
        )?;
        let fine_shape = split_fine_shape_allocation(frame_samples_48k, 1, reallocated)?;
        let energies_q15 = decode_fine_energies(
            range,
            &energy.energies_q15,
            &fine_shape.fine_bits,
            end_band,
            1,
        )?;
        (Some(reallocated), Some(fine_shape), energies_q15)
    } else {
        (None, None, energy.energies_q15)
    };

    Ok(Some(MonoCeltFramePrefix {
        header: energy.header,
        tf_resolution,
        spread,
        controls,
        reservations,
        band_allocation,
        reallocated,
        fine_shape,
        energies_q15,
    }))
}

/// Decodes and synthesizes the unsplit mono CELT frame tail.
///
/// Recursive PVQ splitting remains unsupported. The caller retains synthesis
/// and anti-collapse state across frames.
pub(crate) fn decode_mono_celt_frame(
    range: &mut RangeDecoder<'_>,
    frame_samples_48k: u16,
    frame_bytes: usize,
    prefix: &MonoCeltFramePrefix,
    synthesis: &mut CeltSynthesis,
    previous_energies_q15: &[[i32; 42]; 2],
    anti_collapse_seed: u32,
) -> Result<MonoCeltPcmFrame, RangeError> {
    let allocation = prefix.fine_shape.ok_or(RangeError::UnsupportedCeltMode)?;
    if prefix.band_allocation.is_none() || prefix.reallocated.is_none() {
        return Err(RangeError::UnsupportedCeltMode);
    }

    let mut tf_adjustments = [0; 21];
    let adjustment_count = prefix.tf_resolution.adjustments.len();
    tf_adjustments[..adjustment_count].copy_from_slice(&prefix.tf_resolution.adjustments);
    let frame_eighth_bits = frame_bytes
        .saturating_mul(64)
        .try_into()
        .map_err(|_| RangeError::CorruptFrame)?;
    let blocks = if prefix.header.transient {
        u8::try_from(frame_samples_48k / 120).map_err(|_| RangeError::InvalidCeltEnergyLayout)?
    } else {
        1
    };
    if frame_samples_48k % 120 != 0 || ![1, 2, 4, 8].contains(&blocks) {
        return Err(RangeError::InvalidCeltEnergyLayout);
    }
    let mut shapes = decode_mono_pvq_shapes_bounded(
        range,
        frame_samples_48k,
        allocation,
        prefix.spread,
        blocks,
        &tf_adjustments,
        frame_eighth_bits,
    )?;
    let anti_collapse_on =
        prefix.reservations.anti_collapse_eighth_bits > 0 && range.read_raw_bits(1)? != 0;
    let remaining_bits = frame_bytes
        .saturating_mul(8)
        .saturating_sub(range.tell() as usize)
        .try_into()
        .map_err(|_| RangeError::CorruptFrame)?;
    let final_energy =
        decode_final_fine_energy(range, &prefix.energies_q15, allocation, 1, remaining_bits)?;
    let anti_collapse_seed = if anti_collapse_on {
        apply_mono_anti_collapse(
            &mut shapes,
            frame_samples_48k,
            allocation,
            &final_energy.energies_q15,
            previous_energies_q15,
            anti_collapse_seed,
        )?
    } else {
        anti_collapse_seed
    };
    let spectrum = denormalize_mono_pvq_shapes(
        frame_samples_48k,
        allocation,
        &shapes,
        &final_energy.energies_q15,
    )?;
    let synthesis_result = if prefix.header.transient {
        synthesis.synthesize_transient_with_postfilter(
            std::slice::from_ref(&spectrum),
            usize::from(blocks),
            prefix.header.postfilter,
        )?
    } else {
        synthesis
            .synthesize_with_postfilter(std::slice::from_ref(&spectrum), prefix.header.postfilter)?
    };
    let samples = match synthesis_result {
        Some(mut channels) => Some(channels.pop().ok_or(RangeError::CorruptFrame)?),
        None => None,
    };
    Ok(MonoCeltPcmFrame {
        shapes,
        final_energy,
        spectrum,
        samples,
        final_range: range.final_range(),
        anti_collapse_seed,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        MonoCeltEnergyHistory, MonoCeltFramePrefix, decode_mono_energy_prefix,
        decode_mono_frame_prefix,
    };
    use crate::celt::{
        FrameHeader, decode_allocation_controls, decode_coarse_energy_residuals,
        decode_fine_energies, decode_header, decode_spread, decode_tf_resolution,
        predict_coarse_energies, reallocate_skipped_bands, reserve_allocation_bits,
        select_band_allocation, split_fine_shape_allocation,
    };
    use crate::range::{RangeDecoder, RangeError};

    #[test]
    #[ignore = "requires the external RFC 8251 opus_newvectors corpus"]
    fn rfc8251_silent_celt_packets_match_final_range() {
        use crate::packet::{Mode, OpusPacket};
        use std::{env, fs, path::Path};

        let directory = env::var_os("ZEMER_OPUS_TESTVECTORS").unwrap();
        let (mut silent, mut matched) = (0, 0);
        for entry in fs::read_dir(Path::new(&directory)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "bit") {
                continue;
            }
            let bytes = fs::read(&path).unwrap();
            let mut cursor = 0;
            while cursor + 8 <= bytes.len() {
                let packet_bytes =
                    u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
                let expected =
                    u32::from_be_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap());
                let end = cursor + 8 + packet_bytes;
                if end > bytes.len() {
                    break;
                }
                let Ok(packet) = OpusPacket::parse(&bytes[cursor + 8..end]) else {
                    cursor = end;
                    continue;
                };
                let toc = packet.toc();
                if toc.mode() == Mode::Celt && toc.frame_code == 0 {
                    let frame = packet.frames()[0];
                    let mut range = RangeDecoder::new(frame);
                    if decode_header(&mut range).unwrap().silence {
                        silent += 1;
                        if range.final_range() == expected {
                            matched += 1;
                        } else {
                            eprintln!(
                                "{} packet={cursor} actual={:#010x} expected={expected:#010x}",
                                path.file_name().unwrap().to_string_lossy(),
                                range.final_range()
                            );
                        }
                    }
                }
                cursor = end;
            }
        }
        eprintln!("silent CELT packets: {matched}/{silent} final ranges matched");
        assert!(silent > 0);
        assert_eq!(matched, silent);
    }

    #[test]
    fn silence_header_stops_before_energy_and_clears_the_current_envelope() {
        let packet = [0xff; 8];
        let previous = [12_345; 42];
        let mut actual_range = RangeDecoder::new(&packet);
        let actual =
            decode_mono_energy_prefix(&mut actual_range, 120, packet.len(), 21, &previous).unwrap();

        let mut header_only_range = RangeDecoder::new(&packet);
        let header = decode_header(&mut header_only_range).unwrap();
        assert!(header.silence);
        assert_eq!(actual.header, header);
        assert_eq!(actual.energies_q15, [0; 42]);
        assert_eq!(actual_range.final_range(), header_only_range.final_range());
    }

    #[test]
    fn regular_frame_decodes_header_then_mono_coarse_energy() {
        let packet = [0; 64];
        let previous = [0; 42];
        let mut actual_range = RangeDecoder::new(&packet);
        let actual =
            decode_mono_energy_prefix(&mut actual_range, 120, packet.len(), 1, &previous).unwrap();
        assert_eq!(
            actual.header,
            FrameHeader {
                silence: false,
                postfilter: None,
                transient: false,
                intra: false,
            }
        );

        let mut expected_range = RangeDecoder::new(&packet);
        let header = decode_header(&mut expected_range).unwrap();
        let residuals = decode_coarse_energy_residuals(
            &mut expected_range,
            120,
            header.intra,
            0,
            1,
            1,
            packet.len(),
        )
        .unwrap();
        let energies =
            predict_coarse_energies(&residuals, &previous, 120, header.intra, 1, 1).unwrap();
        assert_eq!(actual.energies_q15, energies);
        assert_eq!(actual_range.final_range(), expected_range.final_range());
    }

    #[test]
    fn regular_frame_composes_mono_stages_through_fine_energy() {
        let packet = [0; 256];
        let previous = [0; 42];
        let mut actual_range = RangeDecoder::new(&packet);
        let actual = decode_mono_frame_prefix(&mut actual_range, 120, packet.len(), 1, &previous)
            .unwrap()
            .unwrap();

        let mut expected_range = RangeDecoder::new(&packet);
        let energy =
            decode_mono_energy_prefix(&mut expected_range, 120, packet.len(), 1, &previous)
                .unwrap();
        let tf_resolution = decode_tf_resolution(&mut expected_range, 120, false, 0, 1).unwrap();
        let spread = decode_spread(&mut expected_range).unwrap();
        let controls =
            decode_allocation_controls(&mut expected_range, 120, 0, 1, 1, packet.len()).unwrap();
        let remaining_eighth_bits =
            (packet.len() * 64).saturating_sub(expected_range.tell_frac() as usize) as u32;
        let reservations =
            reserve_allocation_bits(120, 0, 1, 1, false, remaining_eighth_bits).unwrap();
        let band_allocation =
            select_band_allocation(120, 0, 1, 1, &controls, reservations.remaining_eighth_bits)
                .unwrap();
        let (reallocated, fine_shape, energies_q15) = if let Some(allocation) = band_allocation {
            let reallocated = reallocate_skipped_bands(
                &mut expected_range,
                120,
                1,
                &controls,
                reservations,
                allocation,
            )
            .unwrap();
            let fine_shape = split_fine_shape_allocation(120, 1, reallocated).unwrap();
            let energies_q15 = decode_fine_energies(
                &mut expected_range,
                &energy.energies_q15,
                &fine_shape.fine_bits,
                1,
                1,
            )
            .unwrap();
            (Some(reallocated), Some(fine_shape), energies_q15)
        } else {
            (None, None, energy.energies_q15)
        };
        let expected = MonoCeltFramePrefix {
            header: energy.header,
            tf_resolution,
            spread,
            controls,
            reservations,
            band_allocation,
            reallocated,
            fine_shape,
            energies_q15,
        };
        assert_eq!(actual, expected);
        assert_eq!(actual_range.final_range(), expected_range.final_range());
    }

    #[test]
    fn silence_frame_prefix_stops_after_the_header() {
        let packet = [0xff; 64];
        let mut actual_range = RangeDecoder::new(&packet);
        assert_eq!(
            decode_mono_frame_prefix(&mut actual_range, 120, packet.len(), 21, &[0; 42]).unwrap(),
            None
        );

        let mut header_only_range = RangeDecoder::new(&packet);
        decode_header(&mut header_only_range).unwrap();
        assert_eq!(actual_range.final_range(), header_only_range.final_range());
    }

    #[test]
    fn active_low_budget_frame_preserves_zero_bit_allocation() {
        let packet = [0; 1];
        let mut range = RangeDecoder::new(&packet);
        let decoded = decode_mono_frame_prefix(&mut range, 120, packet.len(), 21, &[0; 42])
            .unwrap()
            .unwrap();
        assert!(!decoded.header.silence);
        assert_eq!(
            decoded
                .band_allocation
                .map(|allocation| allocation.total_eighth_bits),
            Some(0)
        );
        assert!(decoded.reallocated.is_some());
        assert!(decoded.fine_shape.is_some());
    }

    #[test]
    fn mono_frame_decodes_pvq_final_energy_and_stateful_pcm() {
        let packet = [0; 256];
        let mut synthesis = crate::celt::CeltSynthesis::default();
        let mut energy_history = MonoCeltEnergyHistory::default();

        for frame_index in 0..2 {
            let mut range = RangeDecoder::new(&packet);
            let frame = energy_history
                .decode_active_frame(&mut range, 120, packet.len(), 1, &mut synthesis)
                .unwrap();

            assert_eq!(frame.spectrum.len(), 120);
            assert!(frame.spectrum.iter().all(|sample| sample.is_finite()));
            assert_eq!(frame.final_range, range.final_range());
            if frame_index == 0 {
                assert_eq!(frame.samples, None);
            } else {
                assert_eq!(frame.samples.as_ref().map(Vec::len), Some(120));
            }
            assert_eq!(
                energy_history.previous_final_energies_q15,
                frame.final_energy.energies_q15
            );
        }

        let mut reset_history = energy_history;
        reset_history.reset();
        assert_eq!(reset_history, MonoCeltEnergyHistory::default());
    }

    #[test]
    fn invalid_frame_geometry_is_rejected_before_consuming_header_bits() {
        let packet = [0; 8];
        for (frame_samples, frame_bytes, end_band) in
            [(60, 8, 1), (120, 0, 1), (120, 8, 0), (120, 8, 22)]
        {
            let mut range = RangeDecoder::new(&packet);
            let before = range.final_range();
            assert_eq!(
                decode_mono_energy_prefix(
                    &mut range,
                    frame_samples,
                    frame_bytes,
                    end_band,
                    &[0; 42],
                ),
                Err(RangeError::InvalidCeltEnergyLayout)
            );
            assert_eq!(range.final_range(), before);
        }
    }

    #[test]
    #[ignore = "requires the external RFC 8251 opus_newvectors corpus"]
    fn rfc8251_active_celt_vector07_matches_final_range() {
        use crate::packet::OpusPacket;
        use std::{env, fs, path::Path};
        let root =
            Path::new(&env::var_os("ZEMER_OPUS_TESTVECTORS").unwrap()).join("testvector07.bit");
        let bytes = fs::read(root).unwrap();
        let cursor = 263;
        let length = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        let expected = u32::from_be_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap());
        let packet = OpusPacket::parse(&bytes[cursor + 8..cursor + 8 + length]).unwrap();
        assert_eq!(packet.toc().configuration, 31);
        let frame = packet.frames()[0];
        let mut range = RangeDecoder::new(frame);
        let prefix = decode_mono_frame_prefix(&mut range, 960, frame.len(), 21, &[0; 42])
            .unwrap()
            .unwrap();

        assert_eq!(
            prefix.fine_shape.unwrap().fine_bits,
            [
                3, 2, 2, 2, 2, 2, 1, 1, 2, 2, 2, 1, 2, 2, 1, 1, 0, 0, 0, 0, 0
            ]
        );
        assert_eq!(range.tell_frac(), 801);
        let mut synthesis = crate::celt::CeltSynthesis::default();
        let decoded = super::decode_mono_celt_frame(
            &mut range,
            960,
            frame.len(),
            &prefix,
            &mut synthesis,
            &[[0; 42]; 2],
            0,
        )
        .unwrap();
        assert_eq!(decoded.final_range, expected);
    }

    #[test]
    #[ignore = "requires the external RFC 8251 opus_newvectors corpus"]
    fn rfc8251_vector07_transient_frame_matches_range_and_pcm() {
        use crate::packet::OpusPacket;
        use std::{env, fs, path::Path};
        let root =
            Path::new(&env::var_os("ZEMER_OPUS_TESTVECTORS").unwrap()).join("testvector07.bit");
        let bytes = fs::read(root).unwrap();
        let cursor = 0;
        let length = u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        let expected_packet_range =
            u32::from_be_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap());
        assert_eq!(expected_packet_range, 0x2c33_ee00);
        let packet = OpusPacket::parse(&bytes[cursor + 8..cursor + 8 + length]).unwrap();
        let frame = packet.frames()[0];
        let mut range = RangeDecoder::new(frame);
        let prefix = decode_mono_frame_prefix(&mut range, 960, frame.len(), 21, &[0; 42])
            .unwrap()
            .unwrap();

        assert!(prefix.header.transient);
        let reallocated = prefix.reallocated.unwrap();
        assert_eq!(reallocated.coded_end_band, 15);
        assert_eq!(
            &reallocated.eighth_bits[..15],
            &[
                175, 164, 154, 141, 128, 120, 112, 106, 198, 182, 166, 150, 260, 210, 167
            ]
        );
        let fine = prefix.fine_shape.unwrap();
        assert_eq!(
            fine.fine_bits,
            [
                2, 2, 2, 2, 2, 2, 1, 1, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 0, 0, 0
            ]
        );
        assert_eq!(range.tell_frac(), 790);
        assert_eq!(range.final_range(), 0x0a3f_ee00);
        let mut history = MonoCeltEnergyHistory::default();
        let mut synthesis = crate::celt::CeltSynthesis::default();
        assert!(synthesis.synthesize(&[vec![0.0; 960]]).unwrap().is_none());
        let mut decode_range = RangeDecoder::new(frame);
        let decoded = history
            .decode_active_frame(&mut decode_range, 960, frame.len(), 21, &mut synthesis)
            .unwrap();
        assert_eq!(decoded.final_range, expected_packet_range);
        assert!(decoded.shapes.collapse_masks.iter().any(|&mask| mask != 0));
        let samples = decoded.samples.unwrap();
        assert_eq!(samples.len(), 960);
        assert!(samples.iter().all(|sample| sample.is_finite()));
        assert!(samples.iter().any(|&sample| sample != 0.0));
        let pcm = fs::read(
            Path::new(&env::var_os("ZEMER_OPUS_TESTVECTORS").unwrap()).join("testvector07.dec"),
        )
        .unwrap();
        let reference: Vec<_> = pcm
            .chunks_exact(4)
            .take(960)
            .map(|frame| i16::from_le_bytes([frame[0], frame[1]]))
            .collect();
        let normalized_errors: Vec<_> = samples
            .iter()
            .zip(&reference)
            .map(|(&actual, &expected)| (f64::from(actual) * 32_768.0 - f64::from(expected)).abs())
            .collect();
        let normalized_rms = (normalized_errors
            .iter()
            .map(|error| error.powi(2))
            .sum::<f64>()
            / normalized_errors.len() as f64)
            .sqrt();
        let normalized_peak = normalized_errors.iter().copied().fold(0.0_f64, f64::max);
        assert!(normalized_rms < 10.0, "RMS error: {normalized_rms}");
        assert!(normalized_peak < 32.0, "peak error: {normalized_peak}");
    }
}
