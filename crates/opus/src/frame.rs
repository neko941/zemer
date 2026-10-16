//! CELT frame stages composed in normative range-coded order.

use crate::celt::{
    FrameHeader, decode_coarse_energy_residuals, decode_header, predict_coarse_energies,
};
use crate::range::{RangeDecoder, RangeError};

/// Header and coarse energy state produced by the first CELT frame stages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MonoCeltEnergyPrefix {
    pub(crate) header: FrameHeader,
    pub(crate) energies_q15: [i32; 42],
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

#[cfg(test)]
mod tests {
    use super::decode_mono_energy_prefix;
    use crate::celt::{
        FrameHeader, decode_coarse_energy_residuals, decode_header, predict_coarse_energies,
    };
    use crate::range::{RangeDecoder, RangeError};

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
}
