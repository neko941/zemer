use std::io::Read;

use ogg::{OggPacket, OggPacketReader, PAGE_FLAG_BOS};
use zemer_core::{AudioError, Metadata};

use crate::{VorbisCommentHeader, VorbisIdentificationHeader, VorbisSetupHeader};

pub(crate) struct TimeDomainBlock {
    pub(crate) channels: Vec<Vec<f64>>,
}

struct DecodedPacket<'a> {
    bits: crate::bitstream::BitReader<'a>,
    floors: Vec<crate::floor::FloorResult>,
    spectrum_bins: usize,
    mapping_index: usize,
    block_size: usize,
    previous_window_long: bool,
    next_window_long: bool,
    truncated: bool,
}

/// Reads and validates the three required Vorbis headers in one Ogg stream.
/// Audio packets remain available in order for codec synthesis.
pub struct VorbisStreamReader<R> {
    packets: OggPacketReader<R>,
    identification: VorbisIdentificationHeader,
    comments: VorbisCommentHeader,
    setup: VorbisSetupHeader,
}

impl<R: Read> VorbisStreamReader<R> {
    pub fn new(reader: R) -> Result<Self, AudioError> {
        Self::from_packets(OggPacketReader::new(reader))
    }

    fn from_packets(mut packets: OggPacketReader<R>) -> Result<Self, AudioError> {
        let identification_packet = next_header_packet(&mut packets)?;
        if !identification_packet.page_is_first
            || !identification_packet.starts_page
            || !identification_packet.ends_page
            || identification_packet.page_flags & PAGE_FLAG_BOS == 0
        {
            return Err(AudioError::InvalidFormat(
                "Vorbis identification packet must be alone on the first Ogg page",
            ));
        }
        let identification = VorbisIdentificationHeader::parse(&identification_packet.bytes)?;
        let comment_packet = next_header_packet(&mut packets)?;
        let comments = VorbisCommentHeader::parse_for_stream(&comment_packet.bytes)?;
        let setup_packet = next_header_packet(&mut packets)?;
        if !setup_packet.ends_page {
            return Err(AudioError::InvalidFormat(
                "Vorbis setup header must end its Ogg page",
            ));
        }
        let setup = VorbisSetupHeader::parse(&setup_packet.bytes, &identification)?;
        Ok(Self {
            packets,
            identification,
            comments,
            setup,
        })
    }

    pub(crate) fn next_stream(self) -> Result<Option<Self>, AudioError> {
        self.packets
            .next_stream()?
            .map(Self::from_packets)
            .transpose()
    }

    pub const fn identification(&self) -> &VorbisIdentificationHeader {
        &self.identification
    }

    pub const fn comments(&self) -> &VorbisCommentHeader {
        &self.comments
    }

    pub const fn setup(&self) -> &VorbisSetupHeader {
        &self.setup
    }

    pub fn metadata(&self) -> &Metadata {
        self.comments.metadata()
    }

    pub(crate) fn eos_granule_position(&self) -> Option<u64> {
        self.packets.eos_granule_position()
    }

    /// Decodes the floor curves from one audio packet without performing residue synthesis.
    pub fn decode_floor_curves(
        &self,
        packet: &OggPacket,
    ) -> Result<Vec<Option<Vec<f32>>>, AudioError> {
        let decoded = self.decode_packet_floors(packet)?;
        if decoded.truncated {
            let mut floors = Vec::new();
            floors
                .try_reserve_exact(usize::from(self.identification.channels()))
                .map_err(|_| AudioError::InvalidData("Vorbis floor allocation failed"))?;
            floors.resize_with(usize::from(self.identification.channels()), || None);
            return Ok(floors);
        }
        let mut floors = Vec::new();
        floors
            .try_reserve_exact(decoded.floors.len())
            .map_err(|_| AudioError::InvalidData("Vorbis floor allocation failed"))?;
        for floor in decoded.floors {
            floors.push(match floor {
                crate::floor::FloorResult::Unused => None,
                crate::floor::FloorResult::Curve(curve) => Some(curve),
                crate::floor::FloorResult::Truncated => None,
            });
        }
        Ok(floors)
    }

    /// Decodes residue vectors and applies inverse channel coupling before floor multiplication.
    pub fn decode_residue_vectors(&self, packet: &OggPacket) -> Result<Vec<Vec<f64>>, AudioError> {
        let mut decoded = self.decode_packet_floors(packet)?;
        if decoded.truncated {
            return zero_residue_channels(
                usize::from(self.identification.channels()),
                decoded.spectrum_bins,
            );
        }
        self.decode_packet_residues(&mut decoded)
    }

    /// Decodes one packet to per-channel frequency spectra before inverse MDCT synthesis.
    pub fn decode_spectrum(&self, packet: &OggPacket) -> Result<Vec<Vec<f64>>, AudioError> {
        let mut decoded = self.decode_packet_floors(packet)?;
        let channel_count = usize::from(self.identification.channels());
        if decoded.truncated {
            return zero_residue_channels(channel_count, decoded.spectrum_bins);
        }
        let mut spectra = self.decode_packet_residues(&mut decoded)?;
        let floors = std::mem::take(&mut decoded.floors);
        apply_floor_curves(&mut spectra, floors)?;
        Ok(spectra)
    }

    pub(crate) fn decode_time_block(
        &self,
        packet: &OggPacket,
    ) -> Result<TimeDomainBlock, AudioError> {
        let mut decoded = self.decode_packet_floors(packet)?;
        let mut spectra = if decoded.truncated {
            zero_residue_channels(
                usize::from(self.identification.channels()),
                decoded.spectrum_bins,
            )?
        } else {
            let mut spectra = self.decode_packet_residues(&mut decoded)?;
            let floors = std::mem::take(&mut decoded.floors);
            apply_floor_curves(&mut spectra, floors)?;
            spectra
        };
        let block_size = decoded.block_size;
        for channel in &mut spectra {
            let mut samples = crate::mdct::inverse_mdct(channel)?;
            crate::mdct::apply_window(
                &mut samples,
                self.identification.block_sizes()[0],
                decoded.previous_window_long,
                decoded.next_window_long,
            )?;
            *channel = samples;
        }
        if spectra
            .first()
            .is_some_and(|channel| channel.len() != block_size)
        {
            return Err(AudioError::InvalidData(
                "Vorbis inverse MDCT returned an unexpected block size",
            ));
        }
        Ok(TimeDomainBlock { channels: spectra })
    }

    fn decode_packet_residues(
        &self,
        decoded: &mut DecodedPacket<'_>,
    ) -> Result<Vec<Vec<f64>>, AudioError> {
        let channel_count = usize::from(self.identification.channels());
        let mut vectors = zero_residue_channels(channel_count, decoded.spectrum_bins)?;
        let mapping = self
            .setup
            .mappings
            .get(decoded.mapping_index)
            .ok_or(AudioError::InvalidFormat("invalid Vorbis audio mapping"))?;
        let mut no_residue = Vec::new();
        no_residue
            .try_reserve_exact(decoded.floors.len())
            .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
        no_residue.extend(
            decoded
                .floors
                .iter()
                .map(|floor| matches!(floor, crate::floor::FloorResult::Unused)),
        );
        for coupling in &mapping.coupling {
            let magnitude = usize::from(coupling.magnitude);
            let angle = usize::from(coupling.angle);
            if !no_residue[magnitude] || !no_residue[angle] {
                no_residue[magnitude] = false;
                no_residue[angle] = false;
            }
        }

        let mut channels = Vec::new();
        channels
            .try_reserve_exact(mapping.channel_mux.len())
            .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
        for (submap_index, submap) in mapping.submaps.iter().enumerate() {
            let residue = self
                .setup
                .residues
                .get(usize::from(submap.residue))
                .ok_or(AudioError::InvalidFormat("invalid Vorbis submap residue"))?;
            channels.clear();
            for (channel, &mapped) in mapping.channel_mux.iter().enumerate() {
                if usize::from(mapped) == submap_index {
                    channels.push(channel);
                }
            }
            if channels.is_empty() {
                continue;
            }
            let mut bundle = Vec::new();
            let mut skip = Vec::new();
            bundle
                .try_reserve_exact(channels.len())
                .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
            skip.try_reserve_exact(channels.len())
                .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
            for &channel in &channels {
                bundle.push(zeroed_residue_vector(decoded.spectrum_bins)?);
                skip.push(no_residue[channel]);
            }
            crate::residue::decode_residue(
                residue,
                &self.setup.codebooks,
                &mut decoded.bits,
                &mut bundle,
                &skip,
            )?;
            for (&channel, vector) in channels.iter().zip(bundle) {
                vectors[channel] = vector;
            }
        }
        crate::coupling::inverse_couple(&mut vectors, &mapping.coupling)?;
        Ok(vectors)
    }

    fn decode_packet_floors<'a>(
        &self,
        packet: &'a OggPacket,
    ) -> Result<DecodedPacket<'a>, AudioError> {
        let mut bits = crate::bitstream::BitReader::new(&packet.bytes);
        if bits.read_bit()? {
            return Err(AudioError::InvalidFormat(
                "invalid Vorbis audio packet type",
            ));
        }
        let mode_bits = integer_log(self.setup.modes.len().saturating_sub(1));
        let mode_number = bits.read_bits(mode_bits)? as usize;
        let mode = self
            .setup
            .modes
            .get(mode_number)
            .ok_or(AudioError::InvalidFormat("invalid Vorbis audio mode"))?;
        let (previous_window_long, next_window_long) = if mode.long_block {
            (bits.read_bit()?, bits.read_bit()?)
        } else {
            (false, false)
        };
        let block_size = self.identification.block_sizes()[usize::from(mode.long_block)];
        let mapping = self
            .setup
            .mappings
            .get(usize::from(mode.mapping))
            .ok_or(AudioError::InvalidFormat("invalid Vorbis audio mapping"))?;
        let channels = usize::from(self.identification.channels());
        let mut floors = Vec::new();
        floors
            .try_reserve_exact(channels)
            .map_err(|_| AudioError::InvalidData("Vorbis floor allocation failed"))?;
        for channel in 0..channels {
            let submap_index = usize::from(mapping.channel_mux[channel]);
            let submap = mapping
                .submaps
                .get(submap_index)
                .ok_or(AudioError::InvalidFormat("invalid Vorbis channel submap"))?;
            let floor = self
                .setup
                .floors
                .get(usize::from(submap.floor))
                .ok_or(AudioError::InvalidFormat("invalid Vorbis channel floor"))?;
            let result = match floor {
                crate::VorbisFloor::Type0(floor) => crate::floor::decode_floor0(
                    floor,
                    &self.setup.codebooks,
                    &mut bits,
                    block_size / 2,
                )?,
                crate::VorbisFloor::Type1(floor) => crate::floor::decode_floor1(
                    floor,
                    &self.setup.codebooks,
                    &mut bits,
                    block_size / 2,
                )?,
            };
            match result {
                crate::floor::FloorResult::Unused => floors.push(crate::floor::FloorResult::Unused),
                crate::floor::FloorResult::Curve(curve) => {
                    floors.push(crate::floor::FloorResult::Curve(curve))
                }
                crate::floor::FloorResult::Truncated => {
                    return Ok(DecodedPacket {
                        bits,
                        floors,
                        spectrum_bins: block_size / 2,
                        mapping_index: usize::from(mode.mapping),
                        block_size,
                        previous_window_long,
                        next_window_long,
                        truncated: true,
                    });
                }
            }
        }
        Ok(DecodedPacket {
            bits,
            floors,
            spectrum_bins: block_size / 2,
            mapping_index: usize::from(mode.mapping),
            block_size,
            previous_window_long,
            next_window_long,
            truncated: false,
        })
    }

    /// Returns the next audio packet, skipping reserved non-audio packet types.
    pub fn next_audio_packet(&mut self) -> Result<Option<OggPacket>, AudioError> {
        self.next_audio_packet_with_granule()
            .map(|packet| packet.map(|(packet, _)| packet))
    }

    pub(crate) fn next_audio_packet_with_granule(
        &mut self,
    ) -> Result<Option<(OggPacket, bool)>, AudioError> {
        while let Some((packet, owns_granule)) = self.next_packet_with_granule()? {
            let Some(first_byte) = packet.bytes.first() else {
                return Err(AudioError::InvalidFormat("empty Vorbis audio packet"));
            };
            if first_byte & 1 != 0 {
                continue;
            }
            return Ok(Some((packet, owns_granule)));
        }
        Ok(None)
    }

    pub(crate) fn next_packet_with_granule(
        &mut self,
    ) -> Result<Option<(OggPacket, bool)>, AudioError> {
        self.packets.next_packet_with_granule().map_err(Into::into)
    }
}

fn zeroed_residue_vector(length: usize) -> Result<Vec<f64>, AudioError> {
    let mut vector = Vec::new();
    vector
        .try_reserve_exact(length)
        .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
    vector.resize(length, 0.0);
    Ok(vector)
}

fn zero_residue_channels(channels: usize, vector_size: usize) -> Result<Vec<Vec<f64>>, AudioError> {
    let mut vectors = Vec::new();
    vectors
        .try_reserve_exact(channels)
        .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
    for _ in 0..channels {
        vectors.push(zeroed_residue_vector(vector_size)?);
    }
    Ok(vectors)
}

fn apply_floor_curves(
    spectra: &mut [Vec<f64>],
    floors: Vec<crate::floor::FloorResult>,
) -> Result<(), AudioError> {
    if spectra.len() != floors.len() {
        return Err(AudioError::InvalidData(
            "Vorbis floor and residue channel counts differ",
        ));
    }
    for (spectrum, floor) in spectra.iter_mut().zip(floors) {
        match floor {
            crate::floor::FloorResult::Unused | crate::floor::FloorResult::Truncated => {
                spectrum.fill(0.0);
            }
            crate::floor::FloorResult::Curve(floor) => {
                if spectrum.len() != floor.len() {
                    return Err(AudioError::InvalidData(
                        "Vorbis floor and residue vector sizes differ",
                    ));
                }
                for (value, gain) in spectrum.iter_mut().zip(floor) {
                    *value *= f64::from(gain);
                    if !value.is_finite() {
                        return Err(AudioError::InvalidData(
                            "non-finite Vorbis floor/residue product",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn next_header_packet<R: Read>(packets: &mut OggPacketReader<R>) -> Result<OggPacket, AudioError> {
    packets.next_packet()?.ok_or(AudioError::InvalidFormat(
        "missing required Vorbis header packet",
    ))
}

fn integer_log(value: usize) -> u32 {
    usize::BITS - value.leading_zeros()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ogg::OggPacketWriter;
    use zemer_core::{AudioDecoder, AudioError};

    use super::{VorbisStreamReader, apply_floor_curves};
    use crate::floor::FloorResult;

    struct BitWriter(Vec<bool>);

    impl BitWriter {
        fn new() -> Self {
            Self(Vec::new())
        }

        fn write(&mut self, value: u64, width: usize) {
            self.0.extend((0..width).map(|bit| value & (1 << bit) != 0));
        }

        fn finish(self) -> Vec<u8> {
            let mut packet = b"\x05vorbis".to_vec();
            let mut payload = vec![0; self.0.len().div_ceil(8)];
            for (offset, bit) in self.0.into_iter().enumerate() {
                if bit {
                    payload[offset / 8] |= 1 << (offset % 8);
                }
            }
            packet.extend(payload);
            packet
        }
    }

    fn headers() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let mut identification = vec![0; 30];
        identification[..7].copy_from_slice(b"\x01vorbis");
        identification[11] = 2;
        identification[12..16].copy_from_slice(&44_100_u32.to_le_bytes());
        identification[28] = 0x66;
        identification[29] = 1;

        let mut comments = b"\x03vorbis".to_vec();
        comments.extend_from_slice(&5_u32.to_le_bytes());
        comments.extend_from_slice(b"zemer");
        comments.extend_from_slice(&1_u32.to_le_bytes());
        comments.extend_from_slice(&10_u32.to_le_bytes());
        comments.extend_from_slice(b"TITLE=Song");
        comments.push(1);

        let mut setup = BitWriter::new();
        setup.write(0, 8); // One codebook.
        setup.write(0x56_43_42, 24);
        setup.write(1, 16);
        setup.write(2, 24);
        setup.write(0, 1); // Unordered lengths.
        setup.write(0, 1); // Dense codebook.
        setup.write(0, 5);
        setup.write(0, 5);
        setup.write(0, 4); // No lookup table.
        setup.write(0, 6); // One time transform.
        setup.write(0, 16);
        setup.write(0, 6); // One floor.
        setup.write(1, 16); // Floor type 1.
        setup.write(0, 5); // No partitions.
        setup.write(0, 2); // Multiplier 1.
        setup.write(0, 4); // Range bits 0.
        setup.write(0, 6); // One residue.
        setup.write(0, 16); // Residue type 0.
        setup.write(0, 24); // Begin.
        setup.write(0, 24); // End.
        setup.write(0, 24); // Partition size 1.
        setup.write(0, 6); // One classification.
        setup.write(0, 8); // Classbook 0.
        setup.write(0, 3); // No residue stages.
        setup.write(0, 1); // No high cascade bits.
        setup.write(0, 6); // One mapping.
        setup.write(0, 16); // Mapping type 0.
        setup.write(0, 1); // One submap.
        setup.write(0, 1); // No coupling.
        setup.write(0, 2); // Reserved.
        setup.write(0, 8); // Submap time placeholder.
        setup.write(0, 8); // Floor 0.
        setup.write(0, 8); // Residue 0.
        setup.write(0, 6); // One mode.
        setup.write(0, 1); // Short block.
        setup.write(0, 16); // Window type.
        setup.write(0, 16); // Transform type.
        setup.write(0, 8); // Mapping 0.
        setup.write(1, 1); // Framing.
        (identification, comments, setup.finish())
    }

    fn ogg_stream(comment_packet: Vec<u8>) -> Vec<u8> {
        let (identification, _, setup) = headers();
        let mut writer = OggPacketWriter::new(Cursor::new(Vec::new()), 9);
        writer.write_packet(&identification, 0).unwrap();
        writer.write_packet(&comment_packet, 0).unwrap();
        writer.write_packet(&setup, 0).unwrap();
        writer.write_packet(&[0, 0], 0).unwrap();
        writer.finish().unwrap();
        writer.into_inner().into_inner()
    }

    fn merge_pages(bytes: &[u8], first_page: usize) -> Vec<u8> {
        let mut pages = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let segment_count = usize::from(bytes[offset + 26]);
            let lacing_end = offset + 27 + segment_count;
            let body_len: usize = bytes[offset + 27..lacing_end]
                .iter()
                .map(|&length| usize::from(length))
                .sum();
            let end = lacing_end + body_len;
            pages.push((offset, lacing_end, end));
            offset = end;
        }

        let (start, lacing_end, left_end) = pages[first_page];
        let (right_start, right_lacing_end, right_end) = pages[first_page + 1];
        assert_eq!(right_end, bytes.len());
        let left = &bytes[start..left_end];
        let right = &bytes[right_start..right_end];
        let mut merged = left[..27].to_vec();
        merged[5] = right[5];
        merged[6..14].copy_from_slice(&right[6..14]);
        merged[26] = (usize::from(left[26]) + usize::from(right[26])) as u8;
        merged.extend_from_slice(&bytes[start + 27..lacing_end]);
        merged.extend_from_slice(&bytes[right_start + 27..right_lacing_end]);
        merged.extend_from_slice(&bytes[lacing_end..left_end]);
        merged.extend_from_slice(&bytes[right_lacing_end..right_end]);
        merged[22..26].fill(0);
        let crc = ogg_crc_for_test(&merged);
        merged[22..26].copy_from_slice(&crc.to_le_bytes());

        [bytes[..start].to_vec(), merged].concat()
    }

    fn ogg_crc_for_test(bytes: &[u8]) -> u32 {
        let mut crc = 0_u32;
        for &byte in bytes {
            crc ^= u32::from(byte) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 {
                    (crc << 1) ^ 0x04c1_1db7
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    #[test]
    fn validates_headers_and_exposes_ordered_audio_packets() {
        let (_, comments, _) = headers();
        let mut stream = VorbisStreamReader::new(Cursor::new(ogg_stream(comments))).unwrap();
        assert_eq!(stream.identification().channels(), 2);
        assert_eq!(stream.identification().sample_rate(), 44_100);
        assert_eq!(stream.metadata().values("TITLE"), ["Song"]);
        assert_eq!(stream.next_audio_packet().unwrap().unwrap().bytes, [0, 0]);
        assert!(stream.next_audio_packet().unwrap().is_none());
    }

    #[test]
    fn rejects_audio_on_the_page_that_completes_the_setup_header() {
        let (_, comments, _) = headers();
        let bytes = merge_pages(&ogg_stream(comments), 2);
        assert!(matches!(
            VorbisStreamReader::new(Cursor::new(bytes)),
            Err(AudioError::InvalidFormat(
                "Vorbis setup header must end its Ogg page"
            ))
        ));
    }

    #[test]
    fn decodes_the_vorbis_maximum_of_255_channels() {
        let (mut identification, comments, setup) = headers();
        identification[11] = u8::MAX;
        let mut writer = OggPacketWriter::new(Cursor::new(Vec::new()), 11);
        writer.write_packet(&identification, 0).unwrap();
        writer.write_packet(&comments, 0).unwrap();
        writer.write_packet(&setup, 0).unwrap();
        // One packet-type bit followed by one unused-floor bit per channel.
        writer.write_packet(&[0; 32], 0).unwrap();
        writer.finish().unwrap();

        let mut decoder =
            crate::VorbisDecoder::new(Cursor::new(writer.into_inner().into_inner())).unwrap();
        assert_eq!(decoder.spec().channels(), u8::MAX.into());
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn rejects_misordered_headers_and_skips_reserved_packet_types() {
        let (identification, _, _) = headers();
        let reader = VorbisStreamReader::new(Cursor::new(ogg_stream(identification)));
        assert!(matches!(reader, Err(AudioError::InvalidFormat(_))));

        let (_, comments, _) = headers();
        let mut writer = OggPacketWriter::new(Cursor::new(Vec::new()), 10);
        let (identification, _, setup) = headers();
        writer.write_packet(&identification, 0).unwrap();
        writer.write_packet(&comments, 0).unwrap();
        writer.write_packet(&setup, 0).unwrap();
        writer.write_packet(b"\x03vorbis", 0).unwrap();
        writer.write_packet(&[0, 0], 0).unwrap();
        writer.finish().unwrap();
        let mut reader =
            VorbisStreamReader::new(Cursor::new(writer.into_inner().into_inner())).unwrap();
        assert_eq!(reader.next_audio_packet().unwrap().unwrap().bytes, [0, 0]);
    }

    #[test]
    fn multiplies_residue_by_floor_and_zeros_unused_channels() {
        let mut spectra = vec![vec![1.0, -4.0], vec![8.0, 5.0]];
        apply_floor_curves(
            &mut spectra,
            vec![FloorResult::Curve(vec![2.0, 0.5]), FloorResult::Unused],
        )
        .unwrap();
        assert_eq!(spectra, [[2.0, -2.0], [0.0, 0.0]]);
    }
}
