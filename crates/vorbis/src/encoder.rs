use std::{
    io::Write,
    sync::atomic::{AtomicU32, Ordering},
};

use ogg::OggPacketWriter;
use zemer_core::{AudioEncoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

use crate::rate_control::{PARTITION_SIZE, QUANTIZER_BITS, allocate_levels};
use crate::{
    bitstream::BitWriter,
    floor::{inverse_db, render_point},
    headers::encode_comment_header,
    mdct,
};

const BLOCK_SIZE: usize = 8192;
const HOP_SIZE: usize = BLOCK_SIZE / 2;
const SPECTRUM_BINS: usize = BLOCK_SIZE / 2;
const RESIDUE_PARTITION_SIZE: usize = 16;
const DEFAULT_RESIDUE_BITS: u8 = 12;
const MIN_RESIDUE_BITS: u8 = 8;
const MAX_RESIDUE_BITS: u8 = 16;
const FLOOR_PARTITIONS: usize = 31;
const TARGET_FLOOR_CODEBOOK: u8 = (QUANTIZER_BITS.len() + 1) as u8;
static SERIAL: AtomicU32 = AtomicU32::new(1);

/// Writes Ogg Vorbis using a clean-room fixed high-rate profile.
///
/// Input is interleaved Float32 PCM. The default profile uses 8192-sample
/// blocks and a 12-bit scalar residue codebook. It favors simple,
/// interoperable output over bitrate efficiency; it is not a psychoacoustic
/// quality encoder.
pub struct VorbisEncoder<W: Write> {
    writer: Option<OggPacketWriter<W>>,
    spec: AudioSpec,
    metadata: Metadata,
    pcm: Vec<f32>,
    base_frame: u64,
    total_frames: u64,
    next_center: u64,
    residue_bits: u8,
    target_bitrate: Option<u32>,
    initialized: bool,
    finished: bool,
    failed: bool,
}

impl<W: Write> VorbisEncoder<W> {
    pub fn new(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_with_residue_bits(writer, spec, DEFAULT_RESIDUE_BITS)
    }

    /// Creates an encoder with a scalar residue precision from 8 to 16 bits.
    ///
    /// Higher precision generally improves fidelity and increases packet size.
    /// This controls quantizer resolution only; it is not a psychoacoustic
    /// quality or target-bitrate setting.
    pub fn new_with_residue_bits(
        writer: W,
        spec: AudioSpec,
        residue_bits: u8,
    ) -> Result<Self, AudioError> {
        if spec.sample_format() != SampleFormat::Float32 {
            return Err(AudioError::Unsupported(
                "Vorbis encoder currently accepts Float32 PCM",
            ));
        }
        if !(MIN_RESIDUE_BITS..=MAX_RESIDUE_BITS).contains(&residue_bits) {
            return Err(AudioError::Unsupported(
                "Vorbis residue precision must be between 8 and 16 bits",
            ));
        }
        Ok(Self {
            writer: Some(OggPacketWriter::new(
                writer,
                SERIAL.fetch_add(1, Ordering::Relaxed),
            )),
            spec,
            metadata: Metadata::default(),
            pcm: Vec::new(),
            base_frame: 0,
            total_frames: 0,
            next_center: 0,
            residue_bits,
            target_bitrate: None,
            initialized: false,
            finished: false,
            failed: false,
        })
    }

    /// Creates an encoder that caps each audio packet to the byte budget for
    /// the requested nominal bitrate, including Ogg page overhead. Sparse
    /// inputs may use fewer bits, and stream headers make short-file average
    /// bitrate higher than this audio-packet target. Allocation greedily
    /// chooses the largest unweighted spectral squared-error reduction per
    /// bit; it is not a psychoacoustic quality mode.
    pub fn new_with_target_bitrate(
        writer: W,
        spec: AudioSpec,
        target_bitrate: u32,
    ) -> Result<Self, AudioError> {
        if spec.sample_format() != SampleFormat::Float32 {
            return Err(AudioError::Unsupported(
                "Vorbis encoder currently accepts Float32 PCM",
            ));
        }
        if target_bitrate == 0 || target_bitrate > i32::MAX as u32 {
            return Err(AudioError::Unsupported(
                "Vorbis target bitrate must fit a positive signed 32-bit value",
            ));
        }
        let packet_bytes = target_packet_bytes(target_bitrate, spec.sample_rate())?;
        let minimum_bits = target_packet_fixed_bits(usize::from(spec.channels()))?;
        let available_bits = packet_bytes
            .checked_mul(8)
            .and_then(|bits| bits.checked_sub(minimum_bits))
            .ok_or(AudioError::Unsupported(
                "Vorbis target bitrate is too low for the audio packet overhead",
            ))?;
        if available_bits == 0 {
            return Err(AudioError::Unsupported(
                "Vorbis target bitrate is too low for residue coding",
            ));
        }
        Ok(Self {
            writer: Some(OggPacketWriter::new(
                writer,
                SERIAL.fetch_add(1, Ordering::Relaxed),
            )),
            spec,
            metadata: Metadata::default(),
            pcm: Vec::new(),
            base_frame: 0,
            total_frames: 0,
            next_center: 0,
            residue_bits: DEFAULT_RESIDUE_BITS,
            target_bitrate: Some(target_bitrate),
            initialized: false,
            finished: false,
            failed: false,
        })
    }

    fn write_block_inner(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        if self.finished || self.failed {
            return Err(AudioError::InvalidData(
                "Vorbis encoder is finished or unusable",
            ));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "Vorbis encoder input specification changed",
            ));
        }
        let Samples::F32(samples) = block.samples() else {
            return Err(AudioError::InvalidData(
                "Vorbis encoder expected interleaved Float32 PCM",
            ));
        };
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(AudioError::InvalidData(
                "Vorbis encoder input contains a non-finite sample",
            ));
        }
        let total_samples = self
            .pcm
            .len()
            .checked_add(samples.len())
            .ok_or(AudioError::InvalidData("Vorbis PCM buffer size overflow"))?;
        self.pcm
            .try_reserve(total_samples - self.pcm.len())
            .map_err(|_| AudioError::InvalidData("Vorbis PCM buffer allocation failed"))?;
        self.pcm.extend_from_slice(samples);
        self.total_frames = self
            .total_frames
            .checked_add(block.frames() as u64)
            .ok_or(AudioError::InvalidData("Vorbis sample count overflow"))?;

        while self
            .next_center
            .checked_add(HOP_SIZE as u64)
            .is_some_and(|end| end <= self.total_frames)
        {
            self.encode_center(self.next_center)?;
            self.next_center = self
                .next_center
                .checked_add(HOP_SIZE as u64)
                .ok_or(AudioError::InvalidData("Vorbis frame position overflow"))?;
        }
        self.compact_pcm();
        Ok(())
    }

    fn finish_inner(&mut self) -> Result<(), AudioError> {
        if self.finished {
            return Ok(());
        }
        if self.failed {
            return Err(AudioError::InvalidData(
                "Vorbis encoder is unusable after an earlier failure",
            ));
        }
        if self.total_frames == 0 {
            return Err(AudioError::InvalidData(
                "cannot encode an empty Vorbis stream",
            ));
        }
        self.initialize()?;
        let final_center = self
            .total_frames
            .div_ceil(HOP_SIZE as u64)
            .checked_mul(HOP_SIZE as u64)
            .ok_or(AudioError::InvalidData(
                "Vorbis final frame position overflow",
            ))?;
        while self.next_center <= final_center {
            self.encode_center(self.next_center)?;
            self.next_center = self
                .next_center
                .checked_add(HOP_SIZE as u64)
                .ok_or(AudioError::InvalidData("Vorbis frame position overflow"))?;
        }
        self.writer
            .as_mut()
            .ok_or(AudioError::InvalidData("missing Vorbis Ogg writer"))?
            .finish()?;
        self.finished = true;
        Ok(())
    }

    fn initialize(&mut self) -> Result<(), AudioError> {
        if self.initialized {
            return Ok(());
        }
        let identification = identification_header(self.spec, self.target_bitrate)?;
        let comment = encode_comment_header("Zemer clean-room Vorbis", &self.metadata)?;
        let setup = if self.target_bitrate.is_some() {
            target_setup_header()?
        } else {
            setup_header(self.residue_bits)?
        };
        let writer = self
            .writer
            .as_mut()
            .ok_or(AudioError::InvalidData("missing Vorbis Ogg writer"))?;
        writer.write_packet(&identification, 0)?;
        writer.write_packet(&comment, 0)?;
        writer.write_packet(&setup, 0)?;
        self.initialized = true;
        Ok(())
    }

    fn encode_center(&mut self, center: u64) -> Result<(), AudioError> {
        self.initialize()?;
        let channels = usize::from(self.spec.channels());
        let mut spectra = Vec::new();
        spectra
            .try_reserve_exact(channels)
            .map_err(|_| AudioError::InvalidData("Vorbis channel allocation failed"))?;
        for channel in 0..channels {
            let mut samples = zeroed_f64(BLOCK_SIZE)?;
            let first_frame = center as i128 - HOP_SIZE as i128;
            for (offset, sample) in samples.iter_mut().enumerate() {
                let source_frame = first_frame + offset as i128;
                if source_frame >= 0 && source_frame < self.total_frames as i128 {
                    let source_frame = source_frame as u64;
                    if source_frame < self.base_frame {
                        return Err(AudioError::InvalidData(
                            "Vorbis encoder discarded overlapping PCM",
                        ));
                    }
                    let source_index = usize::try_from(source_frame - self.base_frame)
                        .ok()
                        .and_then(|frame| frame.checked_mul(channels))
                        .and_then(|index| index.checked_add(channel))
                        .ok_or(AudioError::InvalidData("Vorbis PCM offset overflow"))?;
                    *sample = f64::from(*self.pcm.get(source_index).ok_or(
                        AudioError::InvalidData("Vorbis encoder PCM window is incomplete"),
                    )?);
                }
            }
            mdct::apply_window(&mut samples, BLOCK_SIZE, true, true)?;
            spectra.push(zemer_core::transform::forward_mdct(&samples)?);
        }
        let packet = if let Some(target_bitrate) = self.target_bitrate {
            target_audio_packet(&spectra, target_bitrate, self.spec.sample_rate())?
        } else {
            audio_packet(&spectra, self.residue_bits)?
        };
        self.writer
            .as_mut()
            .ok_or(AudioError::InvalidData("missing Vorbis Ogg writer"))?
            .write_packet_owned(packet, center.min(self.total_frames))?;
        Ok(())
    }

    fn compact_pcm(&mut self) {
        let first_needed = self.next_center.saturating_sub(HOP_SIZE as u64);
        let discard_frames = first_needed.saturating_sub(self.base_frame);
        let discard_samples = usize::try_from(discard_frames)
            .ok()
            .and_then(|frames| frames.checked_mul(usize::from(self.spec.channels())))
            .unwrap_or(self.pcm.len())
            .min(self.pcm.len());
        if discard_samples > 0 {
            self.pcm.drain(..discard_samples);
            self.base_frame += (discard_samples / usize::from(self.spec.channels())) as u64;
        }
    }
}

impl<W: Write> AudioEncoder for VorbisEncoder<W> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        let result = self.write_block_inner(block);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        let result = self.finish_inner();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if self.finished || self.failed || self.initialized {
            return Err(AudioError::InvalidData(
                "Vorbis metadata must be set before audio encoding starts",
            ));
        }
        self.metadata = metadata.clone();
        Ok(())
    }
}

fn identification_header(
    spec: AudioSpec,
    target_bitrate: Option<u32>,
) -> Result<Vec<u8>, AudioError> {
    let mut packet = Vec::new();
    packet
        .try_reserve_exact(30)
        .map_err(|_| AudioError::InvalidData("Vorbis header allocation failed"))?;
    packet.extend_from_slice(b"\x01vorbis");
    packet.extend_from_slice(&0_u32.to_le_bytes());
    packet.push(
        u8::try_from(spec.channels())
            .map_err(|_| AudioError::Unsupported("Vorbis supports at most 255 channels"))?,
    );
    packet.extend_from_slice(&spec.sample_rate().to_le_bytes());
    packet.extend_from_slice(&[0; 4]);
    packet.extend_from_slice(
        &target_bitrate
            .map(|bitrate| (bitrate as i32).to_le_bytes())
            .unwrap_or([0; 4]),
    );
    packet.extend_from_slice(&[0; 4]);
    packet.push(0xdd);
    packet.push(1);
    Ok(packet)
}

fn target_packet_bytes(target_bitrate: u32, sample_rate: u32) -> Result<usize, AudioError> {
    let bytes = u64::from(target_bitrate)
        .checked_mul(HOP_SIZE as u64)
        .ok_or(AudioError::InvalidData(
            "Vorbis target bitrate budget overflow",
        ))?
        / u64::from(sample_rate)
        / 8;
    let bytes = usize::try_from(bytes)
        .map_err(|_| AudioError::InvalidData("Vorbis target packet budget overflow"))?;
    let lacing_segments = bytes / 255 + 1;
    let pages = lacing_segments.div_ceil(255);
    let ogg_overhead = pages
        .checked_mul(27)
        .and_then(|headers| headers.checked_add(lacing_segments))
        .ok_or(AudioError::InvalidData("Vorbis Ogg overhead overflow"))?;
    bytes
        .checked_sub(ogg_overhead)
        .ok_or(AudioError::Unsupported(
            "Vorbis target bitrate is too low for Ogg page overhead",
        ))
}

fn target_packet_fixed_bits(channels: usize) -> Result<usize, AudioError> {
    const CLASSWORD_BITS: usize = 3;
    const FLOOR_BITS: usize = 1 + 16 + FLOOR_PARTITIONS * 8;
    let channel_overhead = channels
        .checked_mul(FLOOR_BITS + SPECTRUM_BINS / PARTITION_SIZE * CLASSWORD_BITS)
        .ok_or(AudioError::InvalidData("Vorbis packet overhead overflow"))?;
    channel_overhead
        .checked_add(3)
        .ok_or(AudioError::InvalidData("Vorbis packet overhead overflow"))
}

fn target_setup_header() -> Result<Vec<u8>, AudioError> {
    let mut bits = BitWriter::new();
    bits.write_bits((QUANTIZER_BITS.len() + 1) as u64, 8)?;
    write_target_classbook(&mut bits)?;
    for quantizer_bits in QUANTIZER_BITS {
        write_scalar_codebook(&mut bits, quantizer_bits)?;
    }
    write_floor_codebook(&mut bits)?;

    bits.write_bits(0, 6)?; // one time transform
    bits.write_bits(0, 16)?;

    bits.write_bits(0, 6)?; // one floor
    bits.write_bits(1, 16)?; // floor type 1
    write_floor1_setup(&mut bits, TARGET_FLOOR_CODEBOOK)?;

    bits.write_bits(0, 6)?; // one residue
    bits.write_bits(1, 16)?; // residue type 1
    bits.write_bits(0, 24)?; // begin
    bits.write_bits(SPECTRUM_BINS as u64, 24)?; // end
    bits.write_bits((RESIDUE_PARTITION_SIZE - 1) as u64, 24)?;
    bits.write_bits(QUANTIZER_BITS.len() as u64, 6)?; // one silent class plus each precision
    bits.write_bits(0, 8)?; // classbook 0
    for class in 0_u8..=QUANTIZER_BITS.len() as u8 {
        bits.write_bits(u64::from(class != 0), 3)?;
        bits.write_bit(false)?;
    }
    for class in 1_u8..=QUANTIZER_BITS.len() as u8 {
        bits.write_bits(u64::from(class), 8)?;
    }

    bits.write_bits(0, 6)?; // one mapping
    bits.write_bits(0, 16)?; // mapping type 0
    bits.write_bit(false)?; // one submap
    bits.write_bit(false)?; // no channel coupling
    bits.write_bits(0, 2)?; // reserved
    bits.write_bits(0, 8)?; // submap time configuration
    bits.write_bits(0, 8)?; // floor 0
    bits.write_bits(0, 8)?; // residue 0

    bits.write_bits(0, 6)?; // one mode
    bits.write_bit(true)?; // long block
    bits.write_bits(0, 16)?; // window type
    bits.write_bits(0, 16)?; // transform type
    bits.write_bits(0, 8)?; // mapping 0
    bits.write_bit(true)?; // framing

    let mut packet = Vec::new();
    let payload = bits.into_bytes();
    packet
        .try_reserve_exact(7 + payload.len())
        .map_err(|_| AudioError::InvalidData("Vorbis setup allocation failed"))?;
    packet.extend_from_slice(b"\x05vorbis");
    packet.extend_from_slice(&payload);
    Ok(packet)
}

fn write_target_classbook(bits: &mut BitWriter) -> Result<(), AudioError> {
    bits.write_bits(0x56_43_42, 24)?;
    bits.write_bits(1, 16)?;
    bits.write_bits(1 << 3, 24)?;
    bits.write_bit(true)?; // ordered codewords
    bits.write_bits(2, 5)?; // first codeword length is three bits
    bits.write_bits(1 << 3, 4)?;
    bits.write_bits(0, 4)?; // no lookup table
    Ok(())
}

fn write_floor_codebook(bits: &mut BitWriter) -> Result<(), AudioError> {
    const ENTRIES: u16 = 1 << 8;
    bits.write_bits(0x56_43_42, 24)?;
    bits.write_bits(1, 16)?;
    bits.write_bits(u64::from(ENTRIES), 24)?;
    bits.write_bit(true)?; // ordered codewords
    bits.write_bits(7, 5)?; // all codewords are eight bits
    bits.write_bits(u64::from(ENTRIES), 9)?;
    bits.write_bits(0, 4)?; // no vector lookup
    Ok(())
}

fn write_floor1_setup(bits: &mut BitWriter, codebook: u8) -> Result<(), AudioError> {
    bits.write_bits(FLOOR_PARTITIONS as u64, 5)?;
    for _ in 0..FLOOR_PARTITIONS {
        bits.write_bits(0, 4)?; // floor class 0
    }
    bits.write_bits(0, 3)?; // one value per partition
    bits.write_bits(0, 2)?; // no subclass selector
    bits.write_bits(u64::from(codebook + 1), 8)?;
    bits.write_bits(0, 2)?; // multiplier 1
    bits.write_bits(12, 4)?; // spectrum-bin range
    for x in floor1_points() {
        bits.write_bits(u64::from(x), 12)?;
    }
    Ok(())
}

fn write_floor1_packet(
    bits: &mut BitWriter,
    values: &[u8; FLOOR_PARTITIONS + 2],
) -> Result<(), AudioError> {
    let x_list = floor1_x_list();
    bits.write_bit(true)?; // floor present
    bits.write_bits(u64::from(values[0]), 8)?;
    bits.write_bits(u64::from(values[1]), 8)?;
    for index in 2..values.len() {
        let x = x_list[index];
        let low = (0..index)
            .filter(|&candidate| x_list[candidate] < x)
            .max_by_key(|&candidate| x_list[candidate])
            .ok_or(AudioError::InvalidData(
                "Vorbis floor low neighbor is missing",
            ))?;
        let high = (0..index)
            .filter(|&candidate| x_list[candidate] > x)
            .min_by_key(|&candidate| x_list[candidate])
            .ok_or(AudioError::InvalidData(
                "Vorbis floor high neighbor is missing",
            ))?;
        let predicted = render_point(
            x_list[low],
            u32::from(values[low]),
            x_list[high],
            u32::from(values[high]),
            x,
        );
        let residual = floor1_residual(values[index], predicted);
        bits.write_bits(u64::from(residual.reverse_bits()), 8)?;
    }
    Ok(())
}

const fn floor1_points() -> [u16; FLOOR_PARTITIONS] {
    let mut points = [0_u16; FLOOR_PARTITIONS];
    let mut index = 0;
    while index < FLOOR_PARTITIONS {
        let step = (index + 1) as u32;
        points[index] = (step * step * SPECTRUM_BINS as u32
            / ((FLOOR_PARTITIONS + 1) * (FLOOR_PARTITIONS + 1)) as u32)
            as u16;
        index += 1;
    }
    points
}

fn floor1_x_list() -> [u16; FLOOR_PARTITIONS + 2] {
    let mut x_list = [0_u16; FLOOR_PARTITIONS + 2];
    x_list[1] = SPECTRUM_BINS as u16;
    x_list[2..].copy_from_slice(&floor1_points());
    x_list
}

fn floor1_envelope(spectrum: &[f64]) -> Result<([u8; FLOOR_PARTITIONS + 2], Vec<f64>), AudioError> {
    let x_list = floor1_x_list();
    let mut values = [0_u8; FLOOR_PARTITIONS + 2];
    let mut order: [usize; FLOOR_PARTITIONS + 2] = std::array::from_fn(|index| index);
    order.sort_unstable_by_key(|&index| x_list[index]);
    for rank in 0..order.len() {
        let index = order[rank];
        let x = usize::from(x_list[index]);
        let start = if rank == 0 {
            0
        } else {
            (usize::from(x_list[order[rank - 1]]) + x) / 2
        };
        let end = if rank + 1 == order.len() {
            SPECTRUM_BINS
        } else {
            (x + usize::from(x_list[order[rank + 1]])).div_ceil(2)
        };
        let peak = spectrum[start.min(SPECTRUM_BINS)..end.min(SPECTRUM_BINS)]
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
        let amplitude = peak / 2.0;
        values[index] = if amplitude == 0.0 {
            0
        } else {
            (255.0 + (256.0 / 7.0) * amplitude.log10())
                .ceil()
                .clamp(0.0, 255.0) as u8
        };
    }

    let mut gains = floor1_gains(&x_list, &values)?;
    let max_normalized = spectrum
        .iter()
        .zip(&gains)
        .fold(0.0_f64, |peak, (&sample, &gain)| {
            peak.max(sample.abs() / gain)
        });
    if max_normalized > 2.0 {
        let lift =
            ((256.0 / 7.0 * (max_normalized / 2.0).log10()).ceil() + 1.0).clamp(0.0, 255.0) as u8;
        for value in &mut values {
            *value = value.saturating_add(lift);
        }
        gains = floor1_gains(&x_list, &values)?;
        if spectrum
            .iter()
            .zip(&gains)
            .any(|(&sample, &gain)| sample.abs() / gain > 2.0 + 1e-9)
        {
            values.fill(255);
            gains = floor1_gains(&x_list, &values)?;
        }
    }
    Ok((values, gains))
}

fn floor1_gains(
    x_list: &[u16; FLOOR_PARTITIONS + 2],
    values: &[u8; FLOOR_PARTITIONS + 2],
) -> Result<Vec<f64>, AudioError> {
    let mut gains = Vec::new();
    gains
        .try_reserve_exact(SPECTRUM_BINS)
        .map_err(|_| AudioError::InvalidData("Vorbis floor gain allocation failed"))?;
    let mut order: [usize; FLOOR_PARTITIONS + 2] = std::array::from_fn(|index| index);
    order.sort_unstable_by_key(|&index| x_list[index]);
    for pair in order.windows(2) {
        let x0 = x_list[pair[0]];
        let x1 = x_list[pair[1]];
        let y0 = u32::from(values[pair[0]]);
        let y1 = u32::from(values[pair[1]]);
        for x in usize::from(x0)..usize::from(x1).min(SPECTRUM_BINS) {
            let y = render_point(x0, y0, x1, y1, x as u16);
            gains.push(f64::from(inverse_db(y)?));
        }
    }
    if gains.len() != SPECTRUM_BINS {
        return Err(AudioError::InvalidData("invalid Vorbis floor point range"));
    }
    Ok(gains)
}

fn floor1_residual(value: u8, predicted: u32) -> u8 {
    let value = u32::from(value);
    let high_room = 256 - predicted;
    let low_room = predicted;
    let room = high_room.min(low_room) * 2;
    match value.cmp(&predicted) {
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => {
            let difference = value - predicted;
            let coded = difference * 2;
            if coded < room {
                coded as u8
            } else if high_room > low_room {
                value as u8
            } else {
                (255 - value) as u8
            }
        }
        std::cmp::Ordering::Less => {
            let difference = predicted - value;
            let coded = difference * 2 - 1;
            if coded < room {
                coded as u8
            } else if high_room > low_room {
                value as u8
            } else {
                (255 - value) as u8
            }
        }
    }
}

fn target_audio_packet(
    spectra: &[Vec<f64>],
    target_bitrate: u32,
    sample_rate: u32,
) -> Result<Vec<u8>, AudioError> {
    let max_packet_bytes = target_packet_bytes(target_bitrate, sample_rate)?;
    let fixed_bits = target_packet_fixed_bits(spectra.len())?;
    let residue_bit_budget = max_packet_bytes
        .checked_mul(8)
        .and_then(|bits| bits.checked_sub(fixed_bits))
        .ok_or(AudioError::Unsupported(
            "Vorbis target bitrate is too low for the audio packet overhead",
        ))?;

    let mut floors = Vec::new();
    floors
        .try_reserve_exact(spectra.len())
        .map_err(|_| AudioError::InvalidData("Vorbis channel allocation failed"))?;
    let mut distortions = Vec::new();
    let partition_count = SPECTRUM_BINS / PARTITION_SIZE;
    let count = spectra
        .len()
        .checked_mul(partition_count)
        .ok_or(AudioError::InvalidData("Vorbis partition count overflow"))?;
    distortions
        .try_reserve_exact(count)
        .map_err(|_| AudioError::InvalidData("Vorbis distortion allocation failed"))?;

    for spectrum in spectra {
        if spectrum.len() != SPECTRUM_BINS {
            return Err(AudioError::InvalidData("invalid Vorbis spectrum size"));
        }
        let peak = spectrum
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
        if !peak.is_finite() || peak > 2.0 {
            return Err(AudioError::InvalidData(
                "Vorbis scalar encoder cannot represent this spectrum peak",
            ));
        }
        let floor_model = floor1_envelope(spectrum)?;
        let gains = &floor_model.1;
        for (partition_index, partition) in spectrum.chunks_exact(PARTITION_SIZE).enumerate() {
            let mut error = [0.0_f64; QUANTIZER_BITS.len() + 1];
            for (offset, &sample) in partition.iter().enumerate() {
                let gain = gains[partition_index * PARTITION_SIZE + offset];
                error[0] += sample * sample;
                for (level, &quantizer_bits) in QUANTIZER_BITS.iter().enumerate() {
                    let steps = f64::from(1_u32 << quantizer_bits);
                    let quantized = ((sample / gain + 2.0) * steps / 4.0)
                        .round()
                        .clamp(0.0, steps - 1.0);
                    let reconstructed = gain * (-2.0 + quantized * 4.0 / steps);
                    let difference = sample - reconstructed;
                    error[level + 1] += difference * difference;
                }
            }
            distortions.push(error);
        }
        floors.push(floor_model);
    }

    let levels = allocate_levels(&distortions, residue_bit_budget)?;
    let mut bits = BitWriter::new();
    bits.write_bit(false)?; // audio packet
    bits.write_bit(true)?; // previous block is long
    bits.write_bit(true)?; // next block is long
    for (floor_values, _) in &floors {
        write_floor1_packet(&mut bits, floor_values)?;
    }
    for partition in 0..partition_count {
        for channel in 0..spectra.len() {
            let level = levels[channel * partition_count + partition];
            let classword_bits = 3;
            let codeword = level.reverse_bits() >> (u8::BITS as usize - classword_bits);
            bits.write_bits(u64::from(codeword), classword_bits)?;
        }
        for (channel, spectrum) in spectra.iter().enumerate() {
            let level = levels[channel * partition_count + partition];
            if level == 0 {
                continue;
            }
            let quantizer_bits = QUANTIZER_BITS[usize::from(level - 1)];
            let codeword_width = usize::from(quantizer_bits);
            let steps = f64::from(1_u32 << quantizer_bits);
            let gains = &floors[channel].1;
            let start = partition * PARTITION_SIZE;
            for (offset, &sample) in spectrum[start..start + PARTITION_SIZE].iter().enumerate() {
                let gain = gains[start + offset];
                let index = ((sample / gain + 2.0) * steps / 4.0)
                    .round()
                    .clamp(0.0, steps - 1.0) as u16;
                let codeword = index.reverse_bits() >> (u16::BITS as usize - codeword_width);
                bits.write_bits(u64::from(codeword), codeword_width)?;
            }
        }
    }
    let packet = bits.into_bytes();
    if packet.len() > max_packet_bytes {
        return Err(AudioError::InvalidData(
            "Vorbis rate allocation exceeded its packet budget",
        ));
    }
    Ok(packet)
}

fn setup_header(residue_bits: u8) -> Result<Vec<u8>, AudioError> {
    let mut bits = BitWriter::new();
    bits.write_bits(2, 8)?; // three codebooks
    write_classbook(&mut bits)?;
    write_scalar_codebook(&mut bits, residue_bits)?;
    write_floor_codebook(&mut bits)?;

    bits.write_bits(0, 6)?; // one time transform
    bits.write_bits(0, 16)?;

    bits.write_bits(0, 6)?; // one floor
    bits.write_bits(1, 16)?; // floor type 1
    write_floor1_setup(&mut bits, 2)?;

    bits.write_bits(0, 6)?; // one residue
    bits.write_bits(1, 16)?; // residue type 1
    bits.write_bits(0, 24)?; // begin
    bits.write_bits(SPECTRUM_BINS as u64, 24)?; // end
    bits.write_bits((RESIDUE_PARTITION_SIZE - 1) as u64, 24)?;
    bits.write_bits(1, 6)?; // two classifications: silent and coded
    bits.write_bits(0, 8)?; // classbook 0 (two one-bit entries)
    bits.write_bits(0, 3)?; // silent classification has no residue stage
    bits.write_bit(false)?;
    bits.write_bits(1, 3)?; // coded classification uses pass 0
    bits.write_bit(false)?;
    bits.write_bits(1, 8)?;

    bits.write_bits(0, 6)?; // one mapping
    bits.write_bits(0, 16)?; // mapping type 0
    bits.write_bit(false)?; // one submap
    bits.write_bit(false)?; // no channel coupling
    bits.write_bits(0, 2)?; // reserved
    bits.write_bits(0, 8)?; // submap time configuration
    bits.write_bits(0, 8)?; // floor 0
    bits.write_bits(0, 8)?; // residue 0

    bits.write_bits(0, 6)?; // one mode
    bits.write_bit(true)?; // long block
    bits.write_bits(0, 16)?; // window type
    bits.write_bits(0, 16)?; // transform type
    bits.write_bits(0, 8)?; // mapping 0
    bits.write_bit(true)?; // framing

    let mut packet = Vec::new();
    let payload = bits.into_bytes();
    packet
        .try_reserve_exact(7 + payload.len())
        .map_err(|_| AudioError::InvalidData("Vorbis setup allocation failed"))?;
    packet.extend_from_slice(b"\x05vorbis");
    packet.extend_from_slice(&payload);
    Ok(packet)
}

fn write_classbook(bits: &mut BitWriter) -> Result<(), AudioError> {
    bits.write_bits(0x56_43_42, 24)?;
    bits.write_bits(1, 16)?;
    bits.write_bits(2, 24)?;
    bits.write_bit(false)?;
    bits.write_bit(false)?;
    bits.write_bits(0, 5)?; // first one-bit codeword
    bits.write_bits(0, 5)?; // second one-bit codeword
    bits.write_bits(0, 4)?; // no lookup table
    Ok(())
}

fn write_scalar_codebook(bits: &mut BitWriter, residue_bits: u8) -> Result<(), AudioError> {
    let entries = 1_u32 << residue_bits;
    bits.write_bits(0x56_43_42, 24)?;
    bits.write_bits(1, 16)?;
    bits.write_bits(u64::from(entries), 24)?;
    bits.write_bit(true)?; // ordered codeword lengths
    bits.write_bits(u64::from(residue_bits - 1), 5)?;
    bits.write_bits(
        u64::from(entries),
        (u32::BITS - entries.leading_zeros()) as usize,
    )?;
    bits.write_bits(1, 4)?; // lookup type 1 is equivalent for a one-dimensional scalar book
    bits.write_bits(pack_float32(-2.0) as u64, 32)?;
    bits.write_bits(pack_float32(4.0 / f64::from(entries)) as u64, 32)?;
    bits.write_bits(u64::from(residue_bits - 1), 4)?;
    bits.write_bit(false)?; // no sequence accumulation
    for value in 0..entries {
        bits.write_bits(u64::from(value), usize::from(residue_bits))?;
    }
    Ok(())
}

fn audio_packet(spectra: &[Vec<f64>], residue_bits: u8) -> Result<Vec<u8>, AudioError> {
    let mut bits = BitWriter::new();
    bits.write_bit(false)?; // audio packet
    bits.write_bit(true)?; // previous block is long
    bits.write_bit(true)?; // next block is long

    let mut floors = Vec::new();
    floors
        .try_reserve_exact(spectra.len())
        .map_err(|_| AudioError::InvalidData("Vorbis channel allocation failed"))?;
    let range = 2.0;
    for spectrum in spectra {
        if spectrum.len() != SPECTRUM_BINS {
            return Err(AudioError::InvalidData("invalid Vorbis spectrum size"));
        }
        let peak = spectrum
            .iter()
            .fold(0.0_f64, |peak, sample| peak.max(sample.abs()));
        if !peak.is_finite() || peak > range {
            return Err(AudioError::InvalidData(
                "Vorbis scalar encoder cannot represent this spectrum peak",
            ));
        }
        floors.push(floor1_envelope(spectrum)?);
    }

    for (floor_values, _) in &floors {
        write_floor1_packet(&mut bits, floor_values)?;
    }
    let quantizer_steps = f64::from(1_u32 << residue_bits);
    let codeword_width = usize::from(residue_bits);
    let zero_index = (quantizer_steps as u32 / 2) as u16;
    let mut quantized_channels = Vec::new();
    quantized_channels
        .try_reserve_exact(spectra.len())
        .map_err(|_| AudioError::InvalidData("Vorbis channel allocation failed"))?;
    for (spectrum, (_, gains)) in spectra.iter().zip(&floors) {
        let mut quantized = Vec::new();
        quantized
            .try_reserve_exact(SPECTRUM_BINS)
            .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
        for (index, &sample) in spectrum.iter().enumerate() {
            let gain = gains[index];
            let index = ((sample / gain + range) * quantizer_steps / (range * 2.0))
                .round()
                .clamp(0.0, quantizer_steps - 1.0) as u16;
            quantized.push(index);
        }
        quantized_channels.push(quantized);
    }

    let mut active_channels = Vec::new();
    active_channels
        .try_reserve_exact(spectra.len())
        .map_err(|_| AudioError::InvalidData("Vorbis channel allocation failed"))?;
    for start in (0..SPECTRUM_BINS).step_by(RESIDUE_PARTITION_SIZE) {
        let end = start + RESIDUE_PARTITION_SIZE;
        active_channels.clear();
        for channel in &quantized_channels {
            let active = channel[start..end].iter().any(|&index| index != zero_index);
            bits.write_bit(active)?; // classbook entry 0 means silent
            active_channels.push(active);
        }
        for (channel, &active) in quantized_channels.iter().zip(&active_channels) {
            if active {
                for &index in &channel[start..end] {
                    let codeword = index.reverse_bits() >> (u16::BITS as usize - codeword_width);
                    bits.write_bits(u64::from(codeword), codeword_width)?;
                }
            }
        }
    }
    Ok(bits.into_bytes())
}

fn pack_float32(value: f64) -> u32 {
    if value == 0.0 {
        return 0;
    }
    let sign = u32::from(value.is_sign_negative()) << 31;
    let absolute = value.abs();
    let mut exponent = absolute.log2().floor() as i32 + 768;
    let mut mantissa = (absolute / 2.0_f64.powi(exponent - 788)).round() as u32;
    if mantissa >= (1 << 21) {
        mantissa >>= 1;
        exponent += 1;
    }
    sign | ((exponent as u32) << 21) | mantissa
}

fn zeroed_f64(length: usize) -> Result<Vec<f64>, AudioError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| AudioError::InvalidData("Vorbis PCM window allocation failed"))?;
    values.resize(length, 0.0);
    Ok(values)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ogg::{OggPacketReader, OggPacketWriter};
    use zemer_core::{
        AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, Picture, SampleFormat, Samples,
    };

    use crate::rate_control::QUANTIZER_BITS;
    use crate::{VorbisDecoder, VorbisStreamReader, bitstream::BitReader};

    use crate::{VorbisIdentificationHeader, VorbisSetupHeader};

    use super::{
        BLOCK_SIZE, DEFAULT_RESIDUE_BITS, FLOOR_PARTITIONS, HOP_SIZE, SPECTRUM_BINS,
        TARGET_FLOOR_CODEBOOK, VorbisEncoder, audio_packet, floor1_envelope, identification_header,
        setup_header, target_setup_header,
    };

    #[test]
    fn encoder_headers_form_a_valid_vorbis_setup() {
        let spec = AudioSpec::new(44_100, 2, SampleFormat::Float32).unwrap();
        let identification =
            VorbisIdentificationHeader::parse(&identification_header(spec, None).unwrap()).unwrap();
        let setup = setup_header(DEFAULT_RESIDUE_BITS).unwrap();
        let parsed = VorbisSetupHeader::parse(&setup, &identification).unwrap();

        assert_eq!(identification.channels(), 2);
        assert_eq!(identification.sample_rate(), 44_100);
        assert_eq!(identification.block_sizes(), [8192, 8192]);
        assert_eq!(parsed.codebooks.len(), 3);
        assert_eq!(parsed.codebooks[0].entries, 2);
        assert_eq!(parsed.codebooks[1].entries, 1 << DEFAULT_RESIDUE_BITS);
        assert_eq!(parsed.floors.len(), 1);
        let crate::VorbisFloor::Type1(floor) = &parsed.floors[0] else {
            panic!("encoder setup does not use floor 1")
        };
        assert_eq!(floor.x_list.len(), FLOOR_PARTITIONS + 2);
        assert_eq!(parsed.residues.len(), 1);
        assert_eq!(parsed.mappings[0].channel_mux, [0, 0]);
        let mut vector = [0.0];
        parsed.codebooks[1].lookup_vector(0, &mut vector).unwrap();
        assert!((vector[0] + 2.0).abs() < 0.00001);
        parsed.codebooks[1]
            .lookup_vector(1 << (DEFAULT_RESIDUE_BITS - 1), &mut vector)
            .unwrap();
        assert!(vector[0].abs() < 0.00001);
        parsed.codebooks[1]
            .lookup_vector((1 << DEFAULT_RESIDUE_BITS) - 1, &mut vector)
            .unwrap();
        assert!((vector[0] - (2.0 - 4.0 / f64::from(1 << DEFAULT_RESIDUE_BITS))).abs() < 0.0001);
        for entry in [0_u16, 1, 2, 127, 128, 2048, 4095] {
            let reversed =
                entry.reverse_bits() >> (u16::BITS as usize - usize::from(DEFAULT_RESIDUE_BITS));
            let encoded = [reversed as u8, (reversed >> 8) as u8];
            let mut bits = BitReader::new(&encoded);
            assert_eq!(
                parsed.codebooks[1].huffman.decode_entry(&mut bits).unwrap(),
                u32::from(entry)
            );
        }
    }

    #[test]
    fn residue_precision_profiles_produce_complete_codebooks() {
        let spec = AudioSpec::new(48_000, 1, SampleFormat::Float32).unwrap();
        let identification =
            VorbisIdentificationHeader::parse(&identification_header(spec, None).unwrap()).unwrap();
        for residue_bits in [8, 10, 12, 14, 16] {
            let setup = setup_header(residue_bits).unwrap();
            let parsed = VorbisSetupHeader::parse(&setup, &identification).unwrap();
            let codebook = &parsed.codebooks[1];
            assert_eq!(codebook.entries, 1_u32 << residue_bits);
            assert!(
                codebook
                    .codeword_lengths
                    .iter()
                    .all(|&length| length == residue_bits)
            );
            let mut vector = [0.0];
            codebook.lookup_vector(0, &mut vector).unwrap();
            assert_eq!(vector[0], -2.0);
            codebook
                .lookup_vector(codebook.entries - 1, &mut vector)
                .unwrap();
            assert!(vector[0] < 2.0 && vector[0] > 1.9);
        }
    }

    #[test]
    fn encoder_rejects_residue_precision_outside_its_documented_range() {
        let spec = AudioSpec::new(48_000, 1, SampleFormat::Float32).unwrap();
        assert!(VorbisEncoder::new_with_residue_bits(Vec::new(), spec, 7).is_err());
        assert!(VorbisEncoder::new_with_residue_bits(Vec::new(), spec, 17).is_err());
    }

    #[test]
    fn target_bitrate_header_and_setup_use_the_adaptive_residue_profile() {
        let spec = AudioSpec::new(48_000, 2, SampleFormat::Float32).unwrap();
        let identification =
            VorbisIdentificationHeader::parse(&identification_header(spec, Some(128_000)).unwrap())
                .unwrap();
        let setup =
            VorbisSetupHeader::parse(&target_setup_header().unwrap(), &identification).unwrap();
        assert_eq!(identification.bitrate_nominal(), Some(128_000));
        assert_eq!(setup.codebooks.len(), QUANTIZER_BITS.len() + 2);
        assert_eq!(setup.codebooks[0].entries, 1 << 3);
        assert_eq!(
            setup.codebooks[1..=QUANTIZER_BITS.len()]
                .iter()
                .map(|codebook| codebook.entries)
                .collect::<Vec<_>>(),
            [4, 16, 256, 4096]
        );
        assert!(
            setup.codebooks[1..=QUANTIZER_BITS.len()]
                .iter()
                .all(|codebook| codebook.lookup.kind == 1)
        );
        assert_eq!(setup.codebooks[TARGET_FLOOR_CODEBOOK as usize].entries, 256);
        assert_eq!(
            setup.codebooks[TARGET_FLOOR_CODEBOOK as usize].lookup.kind,
            0
        );
        let crate::VorbisFloor::Type1(floor) = &setup.floors[0] else {
            panic!("target profile did not declare floor 1")
        };
        assert_eq!(floor.x_list.len(), FLOOR_PARTITIONS + 2);
        assert_eq!(
            floor.classes[0].subclass_books,
            [Some(TARGET_FLOOR_CODEBOOK)]
        );
        assert_eq!(setup.residues[0].cascades.len(), QUANTIZER_BITS.len() + 1);
    }

    #[test]
    fn target_bitrate_encoder_produces_a_decodable_rate_limited_stream() {
        let spec = AudioSpec::new(48_000, 2, SampleFormat::Float32).unwrap();
        let frame_count = 16_384_usize;
        let input: Vec<f32> = (0..frame_count)
            .flat_map(|frame| {
                let seed = (frame as u32)
                    .wrapping_mul(1_664_525)
                    .wrapping_add(1_013_904_223);
                let left = ((seed >> 8) as i16 as f32) / 32_768.0 * 0.25;
                let right = (left * 0.71)
                    + (std::f64::consts::TAU * 997.0 * frame as f64 / 48_000.0).sin() as f32 * 0.04;
                [left, right]
            })
            .collect();
        let mut sizes = Vec::new();
        let mut errors = Vec::new();
        for target_bitrate in [64_000, 128_000, 192_000] {
            let mut output = Vec::new();
            {
                let mut encoder =
                    VorbisEncoder::new_with_target_bitrate(&mut output, spec, target_bitrate)
                        .unwrap();
                encoder
                    .write_block(&PcmBuffer::new(spec, Samples::F32(input.clone())).unwrap())
                    .unwrap();
                encoder.finish().unwrap();
            }
            let mut decoder = VorbisDecoder::new(Cursor::new(&output)).unwrap();
            let mut decoded = Vec::new();
            while let Some(block) = decoder.read_frames(997).unwrap() {
                let Samples::F32(samples) = block.samples() else {
                    panic!("Vorbis decoder did not return Float32 PCM");
                };
                decoded.extend_from_slice(samples);
            }
            assert_eq!(decoded.len(), input.len());
            let squared_error = input
                .iter()
                .zip(&decoded)
                .map(|(source, decoded)| f64::from(*source - *decoded).powi(2))
                .sum::<f64>();
            sizes.push(output.len());
            errors.push(squared_error);
        }
        assert!(
            errors[1] < errors[0],
            "squared errors: {errors:?}; stream sizes: {sizes:?}"
        );
        assert!(
            errors[2] < errors[1],
            "squared errors: {errors:?}; stream sizes: {sizes:?}"
        );
        assert!(
            sizes[0] < sizes[1] && sizes[1] < sizes[2],
            "stream sizes: {sizes:?}"
        );
    }

    #[test]
    fn fixed_encoder_round_trips_the_255_channel_limit() {
        let spec = AudioSpec::new(8_000, u8::MAX.into(), SampleFormat::Float32).unwrap();
        let input = vec![0.0; usize::from(spec.channels())];
        let mut output = Vec::new();
        {
            let mut encoder = VorbisEncoder::new(&mut output, spec).unwrap();
            encoder
                .write_block(&PcmBuffer::new(spec, Samples::F32(input)).unwrap())
                .unwrap();
            encoder.finish().unwrap();
        }

        let mut decoder = VorbisDecoder::new(Cursor::new(output)).unwrap();
        assert_eq!(decoder.spec().channels(), u8::MAX.into());
        let mut decoded_frames = 0;
        while let Some(block) = decoder.read_frames(1).unwrap() {
            assert_eq!(block.spec().channels(), u8::MAX.into());
            decoded_frames += block.frames();
        }
        assert_eq!(decoded_frames, 1);
    }

    #[test]
    fn target_bitrate_audio_pages_stay_within_the_requested_budget() {
        let frame_count = 8193_usize;
        for (sample_rate, channels, target_bitrate) in [
            (32_000, 1, 32_000),
            (44_100, 2, 96_000),
            (48_000, 3, 128_000),
            (48_000, 4, 192_000),
            (48_000, 5, 256_000),
            (48_000, 6, 320_000),
            (48_000, 7, 384_000),
            (96_000, 8, 512_000),
            (192_000, 2, 384_000),
        ] {
            let spec = AudioSpec::new(sample_rate, channels, SampleFormat::Float32).unwrap();
            let input: Vec<f32> = (0..frame_count)
                .flat_map(|frame| {
                    (0..channels).map(move |channel| {
                        let frequency = 220.0 + f64::from(channel) * 73.0;
                        (std::f64::consts::TAU * frequency * frame as f64 / f64::from(sample_rate))
                            .sin() as f32
                            * 0.5
                    })
                })
                .collect();
            let mut output = Vec::new();
            {
                let mut encoder =
                    VorbisEncoder::new_with_target_bitrate(&mut output, spec, target_bitrate)
                        .unwrap();
                encoder
                    .write_block(&PcmBuffer::new(spec, Samples::F32(input)).unwrap())
                    .unwrap();
                encoder.finish().unwrap();
            }

            let page_budget =
                (u64::from(target_bitrate) * HOP_SIZE as u64 / u64::from(sample_rate) / 8) as usize;
            let mut reader = OggPacketReader::new(Cursor::new(output));
            let mut packet_count = 0;
            while let Some(packet) = reader.next_packet().unwrap() {
                packet_count += 1;
                if packet_count <= 3 {
                    continue;
                }
                assert!(packet.starts_page && packet.ends_page);
                let page_bytes = 27 + packet.page_segment_count + packet.bytes.len();
                assert!(
                    page_bytes <= page_budget,
                    "{channels} channels at {sample_rate} Hz, {target_bitrate} bit/s: page uses {page_bytes} of {page_budget} bytes"
                );
            }
            assert!(packet_count > 3, "profile emitted audio packets");
        }
    }

    #[test]
    fn rejects_target_bitrates_that_cannot_fit_packet_overhead() {
        let spec = AudioSpec::new(48_000, 2, SampleFormat::Float32).unwrap();
        assert!(VorbisEncoder::new_with_target_bitrate(Vec::new(), spec, 0).is_err());
        assert!(VorbisEncoder::new_with_target_bitrate(Vec::new(), spec, 8_000).is_err());
    }

    #[test]
    fn configurable_residue_precision_round_trips_audio_and_changes_size() {
        let spec = AudioSpec::new(48_000, 1, SampleFormat::Float32).unwrap();
        let input: Vec<f32> = (0..8193)
            .map(|frame| {
                (std::f64::consts::TAU * 440.0 * frame as f64 / 48_000.0).sin() as f32 * 0.7
            })
            .collect();
        let mut errors = Vec::new();
        let mut sizes = Vec::new();
        for residue_bits in [8, 12, 16] {
            let mut output = Vec::new();
            {
                let mut encoder =
                    VorbisEncoder::new_with_residue_bits(&mut output, spec, residue_bits).unwrap();
                let block = PcmBuffer::new(spec, Samples::F32(input.clone())).unwrap();
                encoder.write_block(&block).unwrap();
                encoder.finish().unwrap();
            }
            let mut decoder = VorbisDecoder::new(Cursor::new(&output)).unwrap();
            let mut decoded = Vec::new();
            while let Some(block) = decoder.read_frames(997).unwrap() {
                let Samples::F32(samples) = block.samples() else {
                    panic!("Vorbis decoder did not return Float32 PCM");
                };
                decoded.extend_from_slice(samples);
            }
            assert_eq!(decoded.len(), input.len());
            errors.push(
                input
                    .iter()
                    .zip(decoded)
                    .map(|(source, decoded)| (source - decoded).abs())
                    .fold(0.0_f32, f32::max),
            );
            sizes.push(output.len());
        }
        assert!(errors[2] < errors[0], "errors by precision: {errors:?}");
        assert!(
            sizes[0] < sizes[1] && sizes[1] < sizes[2],
            "sizes: {sizes:?}"
        );
    }

    #[test]
    fn omits_residue_partitions_that_quantize_to_zero() {
        let zero_spectrum = vec![0.0; SPECTRUM_BINS];
        let mut one_bin_spectrum = zero_spectrum.clone();
        one_bin_spectrum[3] = 0.5;
        let silent_packet =
            audio_packet(std::slice::from_ref(&zero_spectrum), DEFAULT_RESIDUE_BITS).unwrap();
        let active_packet = audio_packet(&[one_bin_spectrum], DEFAULT_RESIDUE_BITS).unwrap();

        assert!(
            silent_packet.len() < active_packet.len(),
            "silent packet {} bytes, active packet {} bytes",
            silent_packet.len(),
            active_packet.len()
        );
    }

    #[test]
    fn encoder_writes_a_decodable_stream_with_exact_sample_count_and_tags() {
        let spec = AudioSpec::new(44_100, 2, SampleFormat::Float32).unwrap();
        let frame_count = 12_347_usize;
        let input: Vec<f32> = (0..frame_count)
            .flat_map(|frame| {
                let phase = std::f64::consts::TAU * 440.0 * frame as f64 / 44_100.0;
                [(phase.sin() * 0.7) as f32, (phase.cos() * 0.3) as f32]
            })
            .collect();
        let picture = Picture::new(3, "image/jpeg", "front", 1, 1, 24, 0, [1, 2, 3]);
        let mut output = Vec::new();
        {
            let mut encoder = VorbisEncoder::new(&mut output, spec).unwrap();
            let mut metadata = Metadata::default();
            metadata.insert("title", "Encoder regression");
            metadata.add_picture(picture.clone());
            encoder.set_metadata(&metadata).unwrap();
            for chunk in input.chunks(2 * 1703) {
                let chunk = PcmBuffer::new(spec, Samples::F32(chunk.to_vec())).unwrap();
                encoder.write_block(&chunk).unwrap();
            }
            encoder.finish().unwrap();
        }

        let mut decoder = VorbisDecoder::new(Cursor::new(output.clone())).unwrap();
        assert_eq!(decoder.metadata().values("title"), ["Encoder regression"]);
        assert_eq!(
            decoder.metadata().pictures(),
            std::slice::from_ref(&picture)
        );
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(997).unwrap() {
            let Samples::F32(samples) = block.samples() else {
                panic!("Vorbis decoder did not return Float32 PCM");
            };
            decoded.extend_from_slice(samples);
        }
        assert_eq!(decoded.len(), input.len());
        let (max_index, max_error) = input
            .iter()
            .zip(&decoded)
            .enumerate()
            .map(|(index, (expected, actual))| (index, (expected - actual).abs()))
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .unwrap();
        assert!(
            max_error < 0.1,
            "maximum encode/decode error {max_error} at sample {max_index}: input {}, output {}; input {:?}, output {:?}",
            input[max_index],
            decoded[max_index],
            &input[..input.len().min(16)],
            &decoded[..decoded.len().min(16)]
        );

        let mut rewritten = Vec::new();
        let mut replacement = Metadata::default();
        replacement.insert("title", "Rewritten with picture");
        replacement.add_picture(picture.clone());
        crate::rewrite_metadata(Cursor::new(output), &mut rewritten, &replacement).unwrap();
        let decoder = VorbisDecoder::new(Cursor::new(rewritten)).unwrap();
        assert_eq!(
            decoder.metadata().values("title"),
            ["Rewritten with picture"]
        );
        assert_eq!(decoder.metadata().pictures(), [picture]);
    }

    #[test]
    fn encoder_preserves_short_sample_counts_at_overlap_boundaries() {
        let spec = AudioSpec::new(44_100, 1, SampleFormat::Float32).unwrap();
        let frame_counts = [
            1,
            HOP_SIZE - 1,
            HOP_SIZE,
            HOP_SIZE + 1,
            HOP_SIZE * 2 - 1,
            HOP_SIZE * 2,
            HOP_SIZE * 2 + 1,
        ];

        for target_bitrate in [None, Some(64_000)] {
            for frame_count in frame_counts {
                let input: Vec<f32> = (0..frame_count)
                    .map(|frame| {
                        (std::f64::consts::TAU * 997.0 * frame as f64 / 44_100.0).sin() as f32 * 0.5
                    })
                    .collect();
                let mut output = Vec::new();
                {
                    let mut encoder = match target_bitrate {
                        Some(bitrate) => {
                            VorbisEncoder::new_with_target_bitrate(&mut output, spec, bitrate)
                                .unwrap()
                        }
                        None => VorbisEncoder::new(&mut output, spec).unwrap(),
                    };
                    encoder
                        .write_block(&PcmBuffer::new(spec, Samples::F32(input)).unwrap())
                        .unwrap();
                    encoder.finish().unwrap();
                }

                let mut decoder = VorbisDecoder::new(Cursor::new(output)).unwrap();
                let mut decoded_frames = 0;
                while let Some(block) = decoder.read_frames(257).unwrap() {
                    decoded_frames += block.frames();
                }
                assert_eq!(
                    decoded_frames, frame_count,
                    "target bitrate {target_bitrate:?}"
                );
            }
        }
    }

    #[test]
    fn audio_packet_recovers_its_quantized_spectral_values() {
        let spec = AudioSpec::new(44_100, 2, SampleFormat::Float32).unwrap();
        let spectra: Vec<Vec<f64>> = (0..2)
            .map(|channel| {
                (0..BLOCK_SIZE / 2)
                    .map(|index| {
                        let angle = index as f64 * 0.17;
                        if channel == 0 {
                            0.13 * angle.sin()
                        } else {
                            0.09 * angle.cos()
                        }
                    })
                    .collect()
            })
            .collect();
        let packet = audio_packet(&spectra, DEFAULT_RESIDUE_BITS).unwrap();
        let identification = identification_header(spec, None).unwrap();
        let comment = crate::headers::encode_comment_header("test", &Metadata::default()).unwrap();
        let setup = setup_header(DEFAULT_RESIDUE_BITS).unwrap();
        let mut writer = OggPacketWriter::new(Cursor::new(Vec::new()), 29);
        writer.write_packet(&identification, 0).unwrap();
        writer.write_packet(&comment, 0).unwrap();
        writer.write_packet(&setup, 0).unwrap();
        writer.write_packet(&packet, HOP_SIZE as u64).unwrap();
        writer.finish().unwrap();
        let mut stream =
            VorbisStreamReader::new(Cursor::new(writer.into_inner().into_inner())).unwrap();
        let audio = stream.next_audio_packet().unwrap().unwrap();
        let decoded = stream.decode_spectrum(&audio).unwrap();

        for (actual_spectrum, spectrum) in decoded.iter().zip(&spectra) {
            let (_, gains) = floor1_envelope(spectrum).unwrap();
            let range = 2.0;
            let quantizer_steps = f64::from(1_u32 << DEFAULT_RESIDUE_BITS);
            for (index, (actual, source)) in actual_spectrum.iter().zip(spectrum).enumerate() {
                let gain = gains[index];
                let quantized = ((*source / gain + range) * quantizer_steps / (range * 2.0))
                    .round()
                    .clamp(0.0, quantizer_steps - 1.0);
                let expected = gain * (-range + quantized * (range * 2.0) / quantizer_steps);
                assert!((actual - expected).abs() < 0.00001);
            }
        }
    }
}
