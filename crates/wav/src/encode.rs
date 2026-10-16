use std::io::{Seek, SeekFrom, Write};

use crate::cset::{UTF8_CODE_PAGE, encode_info_value, write_utf8_chunk};
use crate::ima_adpcm::encode_block as encode_ima_block;
use crate::info::id_from_key;
use crate::{BextChunk, WavCueChunk, WavSamplerChunk};
use g711::{encode_alaw, encode_mulaw};
use ms_adpcm::{STANDARD_COEFFICIENTS, encode_block as encode_ms_block};
use zemer_core::{AudioEncoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

pub use g711::G711Encoding;

const MAX_INFO_VALUE: usize = 1024 * 1024;
const EXTENSIBLE_PCM_GUID: [u8; 16] = [
    1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
];
const EXTENSIBLE_FLOAT_GUID: [u8; 16] = [
    3, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WavContainer {
    Riff,
    Rf64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AdpcmEncoding {
    Ima,
    Microsoft,
}

pub struct WavWriter<W> {
    writer: W,
    spec: AudioSpec,
    bytes_per_sample: u16,
    data_bytes: u64,
    start: u64,
    data_size_offset: u64,
    fact_sample_count_offset: Option<u64>,
    ds64_body_offset: Option<u64>,
    riff_overhead: u64,
    metadata: Metadata,
    metadata_chunk_bytes: u64,
    cue: Option<WavCueChunk>,
    sampler: Option<WavSamplerChunk>,
    embedded_chunk_bytes: u64,
    g711: Option<G711Encoding>,
    adpcm: Option<AdpcmEncoding>,
    samples_per_block: u16,
    ima_pending: Vec<i16>,
    frames_written: u64,
    container: WavContainer,
    poisoned: bool,
    finished: bool,
}

impl<W: Write + Seek> WavWriter<W> {
    pub fn new(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_with_metadata(writer, spec, &Metadata::default())
    }

    pub fn new_with_metadata(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        Self::new_configured(writer, spec, metadata, None, None, WavContainer::Riff, None)
    }

    pub fn new_with_bext(writer: W, spec: AudioSpec, bext: &BextChunk) -> Result<Self, AudioError> {
        Self::new_with_metadata_and_bext(writer, spec, &Metadata::default(), bext)
    }

    pub fn new_with_metadata_and_bext(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
        bext: &BextChunk,
    ) -> Result<Self, AudioError> {
        Self::new_with_metadata_and_bext_and_container(
            writer,
            spec,
            metadata,
            bext,
            WavContainer::Riff,
        )
    }

    pub fn new_with_metadata_and_bext_and_container(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
        bext: &BextChunk,
        container: WavContainer,
    ) -> Result<Self, AudioError> {
        Self::new_configured(writer, spec, metadata, None, None, container, Some(bext))
    }

    pub fn new_with_container(
        writer: W,
        spec: AudioSpec,
        container: WavContainer,
    ) -> Result<Self, AudioError> {
        Self::new_with_metadata_and_container(writer, spec, &Metadata::default(), container)
    }

    pub fn new_with_metadata_and_container(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
        container: WavContainer,
    ) -> Result<Self, AudioError> {
        Self::new_configured(writer, spec, metadata, None, None, container, None)
    }

    pub fn new_g711(
        writer: W,
        spec: AudioSpec,
        encoding: G711Encoding,
    ) -> Result<Self, AudioError> {
        Self::new_g711_with_metadata(writer, spec, encoding, &Metadata::default())
    }

    pub fn new_g711_with_metadata(
        writer: W,
        spec: AudioSpec,
        encoding: G711Encoding,
        metadata: &Metadata,
    ) -> Result<Self, AudioError> {
        if spec.sample_format() != SampleFormat::PcmI16 {
            return Err(AudioError::InvalidData(
                "G.711 WAVE encoding requires signed 16-bit PCM input",
            ));
        }
        Self::new_configured(
            writer,
            spec,
            metadata,
            Some(encoding),
            None,
            WavContainer::Riff,
            None,
        )
    }

    pub fn new_g711_with_container(
        writer: W,
        spec: AudioSpec,
        encoding: G711Encoding,
        container: WavContainer,
    ) -> Result<Self, AudioError> {
        Self::new_g711_with_metadata_and_container(
            writer,
            spec,
            encoding,
            &Metadata::default(),
            container,
        )
    }

    pub fn new_g711_with_metadata_and_container(
        writer: W,
        spec: AudioSpec,
        encoding: G711Encoding,
        metadata: &Metadata,
        container: WavContainer,
    ) -> Result<Self, AudioError> {
        if spec.sample_format() != SampleFormat::PcmI16 {
            return Err(AudioError::InvalidData(
                "G.711 WAVE encoding requires signed 16-bit PCM input",
            ));
        }
        Self::new_configured(
            writer,
            spec,
            metadata,
            Some(encoding),
            None,
            container,
            None,
        )
    }

    pub fn new_ima_adpcm(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_ima_adpcm_with_metadata_and_container(
            writer,
            spec,
            &Metadata::default(),
            WavContainer::Riff,
        )
    }

    pub fn new_ima_adpcm_with_metadata_and_container(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
        container: WavContainer,
    ) -> Result<Self, AudioError> {
        Self::new_configured(
            writer,
            spec,
            metadata,
            None,
            Some(AdpcmEncoding::Ima),
            container,
            None,
        )
    }

    pub fn new_ms_adpcm(writer: W, spec: AudioSpec) -> Result<Self, AudioError> {
        Self::new_ms_adpcm_with_metadata_and_container(
            writer,
            spec,
            &Metadata::default(),
            WavContainer::Riff,
        )
    }

    pub fn new_ms_adpcm_with_metadata_and_container(
        writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
        container: WavContainer,
    ) -> Result<Self, AudioError> {
        Self::new_configured(
            writer,
            spec,
            metadata,
            None,
            Some(AdpcmEncoding::Microsoft),
            container,
            None,
        )
    }

    fn new_configured(
        mut writer: W,
        spec: AudioSpec,
        metadata: &Metadata,
        g711: Option<G711Encoding>,
        adpcm: Option<AdpcmEncoding>,
        container: WavContainer,
        bext: Option<&BextChunk>,
    ) -> Result<Self, AudioError> {
        let ima_adpcm = adpcm == Some(AdpcmEncoding::Ima);
        let ms_adpcm = adpcm == Some(AdpcmEncoding::Microsoft);
        let metadata_chunk_bytes = writer_metadata_size(metadata)?;
        let bext_chunk_bytes = if let Some(chunk) = bext {
            let size = chunk.chunk_size()?;
            u64::from(size) + 8 + u64::from(size & 1)
        } else {
            0
        };
        let start = writer.stream_position()?;
        if (ima_adpcm || ms_adpcm) && spec.sample_format() != SampleFormat::PcmI16 {
            return Err(AudioError::InvalidData(
                "WAVE ADPCM encoding requires signed 16-bit PCM",
            ));
        }
        if ms_adpcm && (!(1..=2).contains(&spec.channels()) || spec.speaker_mask().is_some()) {
            return Err(AudioError::InvalidData(
                "Microsoft ADPCM WAVE encoding requires mono or stereo PCM without a speaker mask",
            ));
        }
        let (legacy_tag, bits_per_sample) = if ima_adpcm || ms_adpcm {
            (if ms_adpcm { 0x0002_u16 } else { 0x0011_u16 }, 4_u16)
        } else {
            match (g711, spec.sample_format()) {
                (Some(G711Encoding::ALaw), SampleFormat::PcmI16) => (6_u16, 8_u16),
                (Some(G711Encoding::MuLaw), SampleFormat::PcmI16) => (7_u16, 8_u16),
                (Some(_), _) => {
                    return Err(AudioError::InvalidData(
                        "G.711 WAVE encoding requires signed 16-bit PCM input",
                    ));
                }
                (None, SampleFormat::PcmU8) => (1_u16, 8_u16),
                (None, SampleFormat::PcmI16) => (1, 16),
                (None, SampleFormat::PcmI24) => (1, 24),
                (None, SampleFormat::PcmI32) => (1, 32),
                (None, SampleFormat::Float32) => (3, 32),
                (None, SampleFormat::Float64) => (3, 64),
            }
        };
        let block_align = if ima_adpcm || ms_adpcm {
            if ima_adpcm {
                spec.channels()
                    .checked_mul(256)
                    .ok_or(AudioError::Unsupported(
                        "WAVE IMA ADPCM channel count exceeds the block alignment limit",
                    ))?
            } else if spec.channels() == 2 {
                512
            } else {
                256
            }
        } else {
            spec.channels()
                .checked_mul(bits_per_sample / 8)
                .ok_or(AudioError::InvalidData("WAVE block alignment overflow"))?
        };
        let samples_per_block = if ms_adpcm {
            ms_adpcm::samples_per_block(block_align, spec.channels())?
        } else if ima_adpcm {
            let channels = u32::from(spec.channels());
            u16::try_from(1 + (u32::from(block_align) - 4 * channels) * 2 / channels)
                .map_err(|_| AudioError::InvalidData("WAVE IMA ADPCM block is too large"))?
        } else {
            0
        };
        let extensible = spec.speaker_mask().is_some() || (ima_adpcm && spec.channels() > 2);
        let non_pcm = legacy_tag != 1;
        let riff_overhead = (if extensible { 60_u64 } else { 36_u64 })
            + if ms_adpcm {
                34
            } else if ima_adpcm && !extensible {
                4
            } else if non_pcm && !extensible {
                2
            } else {
                0
            }
            + if non_pcm { 12 } else { 0 }
            + if container == WavContainer::Rf64 {
                36
            } else {
                0
            };
        if riff_overhead + metadata_chunk_bytes + bext_chunk_bytes > u64::from(u32::MAX) {
            return Err(AudioError::Unsupported(
                "WAVE metadata exceeds the RIFF/WAVE size limit",
            ));
        }
        let byte_rate = if ima_adpcm || ms_adpcm {
            u64::from(spec.sample_rate()) * u64::from(block_align) / u64::from(samples_per_block)
        } else {
            u64::from(spec.sample_rate()) * u64::from(block_align)
        };
        if byte_rate > u64::from(u32::MAX) {
            return Err(AudioError::Unsupported(
                "WAVE byte rate exceeds the RIFF/WAVE limit",
            ));
        }
        let (riff_id, riff_size) = match container {
            WavContainer::Riff => (b"RIFF", 0_u32),
            WavContainer::Rf64 => (b"RF64", u32::MAX),
        };
        writer.write_all(riff_id)?;
        writer.write_all(&riff_size.to_le_bytes())?;
        writer.write_all(b"WAVE")?;
        let ds64_body_offset = if container == WavContainer::Rf64 {
            writer.write_all(b"ds64\x1c\0\0\0")?;
            let body_offset = writer.stream_position()?;
            writer.write_all(&[0; 24])?;
            writer.write_all(&0_u32.to_le_bytes())?;
            Some(body_offset)
        } else {
            None
        };
        if let Some(bext) = bext {
            let chunk_size = bext.chunk_size()?;
            writer.write_all(b"bext")?;
            writer.write_all(&chunk_size.to_le_bytes())?;
            bext.write(&mut writer)?;
            if chunk_size & 1 != 0 {
                writer.write_all(&[0])?;
            }
        }
        writer.write_all(b"fmt ")?;
        writer.write_all(
            &(if ms_adpcm {
                50_u32
            } else if extensible {
                40_u32
            } else if ima_adpcm {
                20_u32
            } else if non_pcm {
                18
            } else {
                16
            })
            .to_le_bytes(),
        )?;
        writer.write_all(&(if extensible { 0xfffe_u16 } else { legacy_tag }).to_le_bytes())?;
        writer.write_all(&spec.channels().to_le_bytes())?;
        writer.write_all(&spec.sample_rate().to_le_bytes())?;
        writer.write_all(&(byte_rate as u32).to_le_bytes())?;
        writer.write_all(&block_align.to_le_bytes())?;
        writer.write_all(&bits_per_sample.to_le_bytes())?;
        if extensible {
            writer.write_all(&22_u16.to_le_bytes())?;
            writer.write_all(
                &(if ima_adpcm {
                    samples_per_block
                } else {
                    bits_per_sample
                })
                .to_le_bytes(),
            )?;
            writer.write_all(&spec.speaker_mask().unwrap_or(0).to_le_bytes())?;
            writer.write_all(&subformat_guid(legacy_tag))?;
        } else if ms_adpcm {
            writer.write_all(&32_u16.to_le_bytes())?;
            writer.write_all(&samples_per_block.to_le_bytes())?;
            writer.write_all(&(STANDARD_COEFFICIENTS.len() as u16).to_le_bytes())?;
            for coefficient in STANDARD_COEFFICIENTS {
                writer.write_all(&coefficient.first.to_le_bytes())?;
                writer.write_all(&coefficient.second.to_le_bytes())?;
            }
        } else if ima_adpcm {
            writer.write_all(&2_u16.to_le_bytes())?;
            writer.write_all(&samples_per_block.to_le_bytes())?;
        } else if non_pcm {
            writer.write_all(&0_u16.to_le_bytes())?;
        }
        let fact_sample_count_offset = if non_pcm {
            writer.write_all(b"fact")?;
            writer.write_all(&4_u32.to_le_bytes())?;
            let offset = writer.stream_position()?;
            writer.write_all(&0_u32.to_le_bytes())?;
            Some(offset)
        } else {
            None
        };
        writer.write_all(b"data")?;
        let data_size_offset = writer.stream_position()?;
        writer.write_all(&if container == WavContainer::Rf64 {
            u32::MAX.to_le_bytes()
        } else {
            0_u32.to_le_bytes()
        })?;

        Ok(Self {
            writer,
            spec,
            bytes_per_sample: bits_per_sample / 8,
            data_bytes: 0,
            start,
            data_size_offset,
            fact_sample_count_offset,
            ds64_body_offset,
            riff_overhead: riff_overhead + bext_chunk_bytes,
            metadata: metadata.clone(),
            metadata_chunk_bytes,
            cue: None,
            sampler: None,
            embedded_chunk_bytes: 0,
            g711,
            adpcm,
            samples_per_block,
            ima_pending: Vec::new(),
            frames_written: 0,
            container,
            poisoned: false,
            finished: false,
        })
    }

    fn replace_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        let chunk_bytes = writer_metadata_size(metadata)?;
        let riff_size = self
            .riff_overhead
            .checked_add(self.data_bytes)
            .and_then(|size| size.checked_add(self.data_bytes & 1))
            .and_then(|size| size.checked_add(chunk_bytes))
            .and_then(|size| size.checked_add(self.embedded_chunk_bytes))
            .ok_or(AudioError::Unsupported("RIFF/WAVE output size overflow"))?;
        if self.container == WavContainer::Riff && riff_size > u64::from(u32::MAX) {
            return Err(AudioError::Unsupported(
                "WAVE metadata exceeds the RIFF/WAVE size limit",
            ));
        }
        self.metadata = metadata.clone();
        self.metadata_chunk_bytes = chunk_bytes;
        Ok(())
    }

    /// Adds or replaces a typed `cue ` chunk written after the audio data.
    pub fn set_cue(&mut self, cue: Option<WavCueChunk>) -> Result<(), AudioError> {
        self.ensure_metadata_writable()?;
        let cue_bytes = cue.as_ref().map(cue_chunk_bytes).transpose()?.unwrap_or(0);
        let sampler_bytes = self
            .sampler
            .as_ref()
            .map(sampler_chunk_bytes)
            .transpose()?
            .unwrap_or(0);
        self.replace_embedded_chunks(cue_bytes, sampler_bytes)?;
        self.cue = cue;
        Ok(())
    }

    /// Adds or replaces a typed `smpl` chunk written after the audio data.
    pub fn set_sampler(&mut self, sampler: Option<WavSamplerChunk>) -> Result<(), AudioError> {
        self.ensure_metadata_writable()?;
        let cue_bytes = self
            .cue
            .as_ref()
            .map(cue_chunk_bytes)
            .transpose()?
            .unwrap_or(0);
        let sampler_bytes = sampler
            .as_ref()
            .map(sampler_chunk_bytes)
            .transpose()?
            .unwrap_or(0);
        self.replace_embedded_chunks(cue_bytes, sampler_bytes)?;
        self.sampler = sampler;
        Ok(())
    }

    fn ensure_metadata_writable(&self) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData("WAVE writer is already finalized"));
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "WAVE writer is unusable after an earlier I/O failure",
            ));
        }
        Ok(())
    }

    fn replace_embedded_chunks(
        &mut self,
        cue_bytes: u64,
        sampler_bytes: u64,
    ) -> Result<(), AudioError> {
        let chunk_bytes = cue_bytes
            .checked_add(sampler_bytes)
            .ok_or(AudioError::Unsupported(
                "WAVE embedded metadata size overflow",
            ))?;
        let riff_size = self
            .riff_overhead
            .checked_add(self.data_bytes)
            .and_then(|size| size.checked_add(self.data_bytes & 1))
            .and_then(|size| size.checked_add(self.metadata_chunk_bytes))
            .and_then(|size| size.checked_add(chunk_bytes))
            .ok_or(AudioError::Unsupported("WAVE output size overflow"))?;
        if self.container == WavContainer::Riff && riff_size > u64::from(u32::MAX) {
            return Err(AudioError::Unsupported(
                "WAVE metadata exceeds the RIFF/WAVE size limit",
            ));
        }
        self.embedded_chunk_bytes = chunk_bytes;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W, AudioError> {
        self.finalize()?;
        Ok(self.writer)
    }

    fn finalize(&mut self) -> Result<(), AudioError> {
        if self.finished {
            return Ok(());
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "WAVE writer failed while writing sample data",
            ));
        }
        let result = self.finalize_output();
        if result.is_err() {
            self.poisoned = true;
        } else {
            self.finished = true;
        }
        result
    }

    fn finalize_output(&mut self) -> Result<(), AudioError> {
        if self.adpcm.is_some() && !self.ima_pending.is_empty() {
            let channels = usize::from(self.spec.channels());
            let start = self.ima_pending.len() - channels;
            let final_frame = self.ima_pending[start..].to_vec();
            let target = usize::from(self.samples_per_block) * channels;
            while self.ima_pending.len() < target {
                self.ima_pending.extend_from_slice(&final_frame);
            }
            self.write_adpcm_block()?;
        }
        if self.data_bytes & 1 != 0 {
            self.writer.write_all(&[0])?;
        }
        if self.metadata_chunk_bytes > 0 {
            write_utf8_chunk(&mut self.writer)?;
            write_info_chunk(&mut self.writer, &self.metadata, UTF8_CODE_PAGE)?;
        }
        if let Some(cue) = &self.cue {
            cue.write(&mut self.writer)?;
        }
        if let Some(sampler) = &self.sampler {
            sampler.write(&mut self.writer)?;
        }
        let end = self.writer.stream_position()?;
        let riff_size = end
            .checked_sub(self.start + 8)
            .ok_or(AudioError::InvalidData(
                "WAVE output position moved before RIFF data",
            ))?;
        let block_align = u64::from(self.spec.channels())
            .checked_mul(u64::from(self.bytes_per_sample))
            .ok_or(AudioError::InvalidData("WAVE block alignment overflow"))?;
        let sample_count = if self.adpcm.is_some() {
            self.frames_written
        } else {
            self.data_bytes / block_align
        };
        if self.container == WavContainer::Riff && riff_size > u64::from(u32::MAX) {
            return Err(AudioError::Unsupported(
                "RIFF/WAVE output larger than 4 GiB; select RF64 for large files",
            ));
        }
        if let Some(offset) = self.fact_sample_count_offset {
            self.writer.seek(SeekFrom::Start(offset))?;
            self.writer.write_all(
                &u32::try_from(sample_count)
                    .unwrap_or(u32::MAX)
                    .to_le_bytes(),
            )?;
        }
        match self.container {
            WavContainer::Riff => {
                self.writer.seek(SeekFrom::Start(self.start + 4))?;
                self.writer.write_all(&(riff_size as u32).to_le_bytes())?;
                self.writer.seek(SeekFrom::Start(self.data_size_offset))?;
                self.writer
                    .write_all(&(self.data_bytes as u32).to_le_bytes())?;
            }
            WavContainer::Rf64 => {
                let body = self.ds64_body_offset.ok_or(AudioError::InvalidData(
                    "RF64 writer is missing its ds64 size fields",
                ))?;
                self.writer.seek(SeekFrom::Start(body))?;
                self.writer.write_all(&riff_size.to_le_bytes())?;
                self.writer.write_all(&self.data_bytes.to_le_bytes())?;
                self.writer.write_all(&sample_count.to_le_bytes())?;
            }
        }
        self.writer.seek(SeekFrom::End(0))?;
        Ok(())
    }
}

impl<W: Write + Seek> AudioEncoder for WavWriter<W> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData("WAVE writer is already finalized"));
        }
        self.replace_metadata(metadata)
    }

    fn finish(&mut self) -> Result<(), AudioError> {
        self.finalize()
    }

    fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        if self.finished {
            return Err(AudioError::InvalidData("WAVE writer is already finalized"));
        }
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "WAVE writer is unusable after an earlier I/O failure",
            ));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "PCM block does not match the WAVE writer format",
            ));
        }
        let channels = usize::from(self.spec.channels());
        let frames = (block.samples().len() / channels) as u64;
        let new_frames = self
            .frames_written
            .checked_add(frames)
            .ok_or(AudioError::Unsupported("WAVE sample count overflow"))?;
        if self.adpcm.is_some() {
            let Samples::I16(samples) = block.samples() else {
                return Err(AudioError::InvalidData(
                    "WAVE ADPCM requires signed 16-bit input",
                ));
            };
            let target = usize::from(self.samples_per_block) * channels;
            let mut offset = 0;
            while offset < samples.len() {
                let take = (target - self.ima_pending.len()).min(samples.len() - offset);
                self.ima_pending
                    .extend_from_slice(&samples[offset..offset + take]);
                offset += take;
                if self.ima_pending.len() == target {
                    if let Err(error) = self.write_adpcm_block() {
                        self.poisoned = true;
                        return Err(error);
                    }
                }
            }
            self.frames_written = new_frames;
            return Ok(());
        }
        let block_bytes = u64::try_from(block.samples().len())
            .map_err(|_| AudioError::Unsupported("sample buffer exceeds the RIFF/WAVE limit"))?
            .checked_mul(u64::from(self.bytes_per_sample))
            .ok_or(AudioError::Unsupported("sample buffer size overflow"))?;
        let new_size = self
            .data_bytes
            .checked_add(block_bytes)
            .ok_or(AudioError::Unsupported("RIFF/WAVE output size overflow"))?;
        let riff_size = self
            .riff_overhead
            .checked_add(new_size)
            .and_then(|size| size.checked_add(new_size & 1))
            .and_then(|size| size.checked_add(self.metadata_chunk_bytes))
            .and_then(|size| size.checked_add(self.embedded_chunk_bytes))
            .ok_or(AudioError::Unsupported("RIFF/WAVE output size overflow"))?;
        if self.container == WavContainer::Riff
            && (new_size > u64::from(u32::MAX) || riff_size > u64::from(u32::MAX))
        {
            return Err(AudioError::Unsupported(
                "RIFF/WAVE output larger than 4 GiB; select RF64 for large files",
            ));
        }

        let result = match self.g711 {
            Some(encoding) => write_g711_samples(&mut self.writer, block.samples(), encoding),
            None => write_sample_data(&mut self.writer, block.samples()),
        };
        if result.is_err() {
            self.poisoned = true;
            return result;
        }
        self.data_bytes = new_size;
        self.frames_written = new_frames;
        Ok(())
    }
}

impl<W: Write + Seek> WavWriter<W> {
    fn write_adpcm_block(&mut self) -> Result<(), AudioError> {
        let encoded = if self.adpcm == Some(AdpcmEncoding::Microsoft) {
            encode_ms_block(
                &self.ima_pending,
                self.spec.channels(),
                if self.spec.channels() == 2 { 512 } else { 256 },
            )?
        } else {
            encode_ima_block(&self.ima_pending, self.spec.channels())?
        };
        let new_size = self
            .data_bytes
            .checked_add(encoded.len() as u64)
            .ok_or(AudioError::Unsupported("RIFF/WAVE output size overflow"))?;
        let riff_size = self
            .riff_overhead
            .checked_add(new_size)
            .and_then(|size| size.checked_add(new_size & 1))
            .and_then(|size| size.checked_add(self.metadata_chunk_bytes))
            .and_then(|size| size.checked_add(self.embedded_chunk_bytes))
            .ok_or(AudioError::Unsupported("RIFF/WAVE output size overflow"))?;
        if self.container == WavContainer::Riff && riff_size > u64::from(u32::MAX) {
            return Err(AudioError::Unsupported(
                "RIFF/WAVE output larger than 4 GiB; select RF64 for large files",
            ));
        }
        self.writer.write_all(&encoded)?;
        self.data_bytes = new_size;
        self.ima_pending.clear();
        Ok(())
    }
}

fn subformat_guid(format_tag: u16) -> [u8; 16] {
    match format_tag {
        0x11 => [
            0x11, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
        ],
        1 => EXTENSIBLE_PCM_GUID,
        3 => EXTENSIBLE_FLOAT_GUID,
        6 => [
            6, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
        ],
        7 => [
            7, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
        ],
        _ => unreachable!("writer only selects known WAVE subformats"),
    }
}

fn cue_chunk_bytes(cue: &WavCueChunk) -> Result<u64, AudioError> {
    let size = cue.chunk_size()?;
    Ok(8 + u64::from(size) + u64::from(size & 1))
}

fn sampler_chunk_bytes(sampler: &WavSamplerChunk) -> Result<u64, AudioError> {
    let size = sampler.chunk_size()?;
    Ok(8 + u64::from(size) + u64::from(size & 1))
}

pub fn write_wav<W: Write + Seek>(writer: W, audio: &PcmBuffer) -> Result<W, AudioError> {
    let mut encoder = WavWriter::new(writer, audio.spec())?;
    encoder.write_block(audio)?;
    encoder.finish()
}

pub fn write_wav_with_metadata<W: Write + Seek>(
    writer: W,
    audio: &PcmBuffer,
    metadata: &Metadata,
) -> Result<W, AudioError> {
    let mut encoder = WavWriter::new_with_metadata(writer, audio.spec(), metadata)?;
    encoder.write_block(audio)?;
    encoder.finish()
}

pub(super) fn metadata_chunk_size(metadata: &Metadata) -> Result<u64, AudioError> {
    metadata_chunk_size_for_code_page(metadata, UTF8_CODE_PAGE)
}

pub(super) fn metadata_chunk_size_for_code_page(
    metadata: &Metadata,
    code_page: u16,
) -> Result<u64, AudioError> {
    let mut list_size = 4_u64;
    let mut values = 0_u64;
    for (key, entries) in metadata.iter() {
        id_from_key(key).ok_or(AudioError::Unsupported(
            "WAVE INFO cannot store this metadata key",
        ))?;
        for value in entries.iter().filter(|value| !value.is_empty()) {
            let bytes = encode_info_value(value, code_page)?;
            let size = bytes
                .len()
                .checked_add(1)
                .ok_or(AudioError::Unsupported("WAVE metadata value size overflow"))?;
            if size > MAX_INFO_VALUE {
                return Err(AudioError::Unsupported(
                    "WAVE metadata value exceeds the 1 MiB limit",
                ));
            }
            let padded = u64::try_from(size)
                .map_err(|_| AudioError::Unsupported("WAVE metadata value is too large"))?;
            list_size = list_size
                .checked_add(8 + padded + (padded & 1))
                .ok_or(AudioError::Unsupported("WAVE metadata size overflow"))?;
            values = values.checked_add(1).ok_or(AudioError::Unsupported(
                "WAVE metadata field count overflow",
            ))?;
        }
    }
    if values == 0 {
        return Ok(0);
    }
    let list_chunk_size = u32::try_from(list_size)
        .map_err(|_| AudioError::Unsupported("WAVE metadata exceeds the RIFF/WAVE size limit"))?;
    Ok(8 + u64::from(list_chunk_size) + u64::from(list_chunk_size & 1))
}

fn writer_metadata_size(metadata: &Metadata) -> Result<u64, AudioError> {
    if !metadata.pictures().is_empty() {
        return Err(AudioError::Unsupported(
            "WAVE writer does not support embedded pictures",
        ));
    }
    let size = metadata_chunk_size(metadata)?;
    if size == 0 {
        Ok(0)
    } else {
        size.checked_add(16)
            .ok_or(AudioError::Unsupported("WAVE metadata size overflow"))
    }
}

pub(super) fn write_info_chunk<W: Write>(
    writer: &mut W,
    metadata: &Metadata,
    code_page: u16,
) -> Result<(), AudioError> {
    let chunk_size = metadata_chunk_size_for_code_page(metadata, code_page)? - 8;
    writer.write_all(b"LIST")?;
    writer.write_all(&(chunk_size as u32).to_le_bytes())?;
    writer.write_all(b"INFO")?;
    write_info_subchunks(writer, metadata, code_page)?;
    if chunk_size & 1 != 0 {
        writer.write_all(&[0])?;
    }
    Ok(())
}

pub(super) fn write_info_subchunks<W: Write>(
    writer: &mut W,
    metadata: &Metadata,
    code_page: u16,
) -> Result<(), AudioError> {
    for (key, entries) in metadata.iter() {
        let id = id_from_key(key).ok_or(AudioError::Unsupported(
            "WAVE INFO cannot store this metadata key",
        ))?;
        for value in entries.iter().filter(|value| !value.is_empty()) {
            let bytes = encode_info_value(value, code_page)?;
            let size = u32::try_from(bytes.len() + 1)
                .map_err(|_| AudioError::Unsupported("WAVE metadata value is too large"))?;
            writer.write_all(&id)?;
            writer.write_all(&size.to_le_bytes())?;
            writer.write_all(&bytes)?;
            writer.write_all(&[0])?;
            if size & 1 != 0 {
                writer.write_all(&[0])?;
            }
        }
    }
    Ok(())
}

fn write_sample_data<W: Write>(writer: &mut W, samples: &Samples) -> Result<(), AudioError> {
    match samples {
        Samples::U8(values) => writer.write_all(values)?,
        Samples::I16(values) => {
            write_samples(writer, values.iter().map(|value| value.to_le_bytes()))?
        }
        Samples::I24(values) => write_samples(
            writer,
            values.iter().map(|value| {
                let bytes = value.to_le_bytes();
                [bytes[0], bytes[1], bytes[2]]
            }),
        )?,
        Samples::I32(values) => {
            write_samples(writer, values.iter().map(|value| value.to_le_bytes()))?
        }
        Samples::F32(values) => {
            write_samples(writer, values.iter().map(|value| value.to_le_bytes()))?
        }
        Samples::F64(values) => {
            write_samples(writer, values.iter().map(|value| value.to_le_bytes()))?
        }
    }
    Ok(())
}

fn write_g711_samples<W: Write>(
    writer: &mut W,
    samples: &Samples,
    encoding: G711Encoding,
) -> Result<(), AudioError> {
    let Samples::I16(values) = samples else {
        return Err(AudioError::InvalidData(
            "G.711 WAVE encoding requires signed 16-bit PCM input",
        ));
    };
    const BUFFER_SIZE: usize = 64 * 1024;
    let mut buffer = Vec::with_capacity(BUFFER_SIZE);
    for &sample in values {
        if buffer.len() == BUFFER_SIZE {
            writer.write_all(&buffer)?;
            buffer.clear();
        }
        buffer.push(match encoding {
            G711Encoding::ALaw => encode_alaw(sample),
            G711Encoding::MuLaw => encode_mulaw(sample),
        });
    }
    writer.write_all(&buffer)?;
    Ok(())
}

fn write_samples<W, const N: usize>(
    writer: &mut W,
    values: impl Iterator<Item = [u8; N]>,
) -> Result<(), AudioError>
where
    W: Write,
{
    const BUFFER_SIZE: usize = 64 * 1024;
    let mut buffer = Vec::with_capacity(BUFFER_SIZE);
    for value in values {
        if buffer.len() + N > BUFFER_SIZE {
            writer.write_all(&buffer)?;
            buffer.clear();
        }
        buffer.extend_from_slice(&value);
    }
    writer.write_all(&buffer)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io::{self, Cursor, Read, Seek, SeekFrom, Write},
    };

    use zemer_core::{
        AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, Picture, SampleFormat, Samples,
    };

    use super::{G711Encoding, WavContainer, WavWriter};
    use g711::{encode_alaw, encode_mulaw};

    #[test]
    fn constructor_rejects_pictures_it_cannot_store() {
        let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
        let mut metadata = Metadata::default();
        metadata.add_picture(Picture::new(3, "image/png", "cover", 0, 0, 0, 0, [1]));
        assert!(WavWriter::new_with_metadata(Cursor::new(Vec::new()), spec, &metadata).is_err());
    }

    #[derive(Default)]
    struct SparseWriter {
        bytes: BTreeMap<u64, u8>,
        position: u64,
        len: u64,
    }

    impl Write for SparseWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            for (offset, byte) in bytes.iter().enumerate() {
                self.bytes.insert(self.position + offset as u64, *byte);
            }
            self.position += bytes.len() as u64;
            self.len = self.len.max(self.position);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Read for SparseWriter {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.position >= self.len {
                return Ok(0);
            }
            let count = bytes.len().min((self.len - self.position) as usize);
            for (offset, byte) in bytes[..count].iter_mut().enumerate() {
                *byte = self
                    .bytes
                    .get(&(self.position + offset as u64))
                    .copied()
                    .unwrap_or(0);
            }
            self.position += count as u64;
            Ok(count)
        }
    }

    impl Seek for SparseWriter {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            let next = match from {
                SeekFrom::Start(position) => i128::from(position),
                SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
                SeekFrom::End(offset) => i128::from(self.len) + i128::from(offset),
            };
            if !(0..=i128::from(u64::MAX)).contains(&next) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid seek"));
            }
            self.position = next as u64;
            Ok(self.position)
        }
    }

    #[test]
    fn g711_encoders_match_standard_endpoint_codewords() {
        let samples = [i16::MIN, -1, 0, 1, i16::MAX];
        assert_eq!(samples.map(encode_alaw), [0x2a, 0x55, 0xd5, 0xd5, 0xaa]);
        assert_eq!(samples.map(encode_mulaw), [0x00, 0x7f, 0xff, 0xff, 0x80]);
    }

    #[test]
    fn writer_and_reader_round_trip_all_registered_info_fields() {
        let fields = [
            ("ARCHIVAL_LOCATION", *b"IARL"),
            ("ARTIST", *b"IART"),
            ("COMMISSIONED", *b"ICMS"),
            ("COMMENT", *b"ICMT"),
            ("COPYRIGHT", *b"ICOP"),
            ("DATE", *b"ICRD"),
            ("CROPPED", *b"ICRP"),
            ("DIMENSIONS", *b"IDIM"),
            ("DPI", *b"IDPI"),
            ("ENGINEER", *b"IENG"),
            ("GENRE", *b"IGNR"),
            ("KEYWORDS", *b"IKEY"),
            ("LIGHTNESS", *b"ILGT"),
            ("MEDIUM", *b"IMED"),
            ("TITLE", *b"INAM"),
            ("PALETTE_SETTING", *b"IPLT"),
            ("ALBUM", *b"IPRD"),
            ("SUBJECT", *b"ISBJ"),
            ("SOFTWARE", *b"ISFT"),
            ("SHARPNESS", *b"ISHP"),
            ("SOURCE", *b"ISRC"),
            ("SOURCE_FORM", *b"ISRF"),
            ("TECHNICIAN", *b"ITCH"),
        ];
        let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
        let mut metadata = Metadata::default();
        for (key, _) in fields {
            metadata.insert(key, format!("{key} value"));
        }
        metadata.insert("ENGINEER", "second engineer");

        let mut writer =
            WavWriter::new_with_metadata(Cursor::new(Vec::new()), spec, &metadata).unwrap();
        writer
            .write_block(&PcmBuffer::new(spec, Samples::I16(vec![123])).unwrap())
            .unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        for (_, id) in fields {
            assert!(bytes.windows(4).any(|window| window == id));
        }
        let decoder = crate::WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.metadata(), &metadata);
    }

    #[test]
    fn g711_writer_emits_the_selected_wave_encoding_and_metadata() {
        let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
        let audio = PcmBuffer::new(spec, Samples::I16(vec![i16::MIN, 0, i16::MAX])).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("title", "Companded");

        for (encoding, tag, expected) in [
            (G711Encoding::ALaw, 6_u16, &[0x2a, 0xd5, 0xaa][..]),
            (G711Encoding::MuLaw, 7_u16, &[0x00, 0xff, 0x80][..]),
        ] {
            let mut writer = WavWriter::new_g711_with_metadata(
                Cursor::new(Vec::new()),
                spec,
                encoding,
                &metadata,
            )
            .unwrap();
            writer.write_block(&audio).unwrap();
            let bytes = writer.finish().unwrap().into_inner();
            assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), tag);
            assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 18);
            assert_eq!(u16::from_le_bytes(bytes[36..38].try_into().unwrap()), 0);
            assert_eq!(&bytes[38..42], b"fact");
            assert_eq!(u32::from_le_bytes(bytes[46..50].try_into().unwrap()), 3);
            assert_eq!(u16::from_le_bytes(bytes[32..34].try_into().unwrap()), 1);
            assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 8);
            assert_eq!(&bytes[58..61], expected);

            let mut decoder = crate::WavDecoder::new(Cursor::new(bytes)).unwrap();
            assert_eq!(decoder.spec(), spec);
            assert_eq!(decoder.metadata(), &metadata);
            assert_eq!(decoder.read_frames(3).unwrap().unwrap().frames(), 3);
        }
    }

    #[test]
    fn float_writer_emits_fact_with_per_channel_frame_count() {
        for (format, samples) in [
            (
                SampleFormat::Float32,
                Samples::F32(vec![0.25, -0.5, 1.0, 0.0]),
            ),
            (
                SampleFormat::Float64,
                Samples::F64(vec![0.25, -0.5, 1.0, 0.0]),
            ),
        ] {
            let spec = AudioSpec::new(48_000, 2, format).unwrap();
            let audio = PcmBuffer::new(spec, samples).unwrap();
            let mut writer = WavWriter::new(Cursor::new(Vec::new()), spec).unwrap();
            assert_eq!(writer.riff_overhead, 50);
            writer.write_block(&audio).unwrap();
            let bytes = writer.finish().unwrap().into_inner();

            assert_eq!(&bytes[12..16], b"fmt ");
            assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 18);
            assert_eq!(u16::from_le_bytes(bytes[36..38].try_into().unwrap()), 0);
            assert_eq!(&bytes[38..42], b"fact");
            assert_eq!(u32::from_le_bytes(bytes[46..50].try_into().unwrap()), 2);
            assert_eq!(&bytes[50..54], b"data");

            let mut decoder = crate::WavDecoder::new(Cursor::new(bytes)).unwrap();
            assert_eq!(decoder.spec(), spec);
            assert_eq!(decoder.read_frames(2).unwrap().unwrap(), audio);
        }
    }

    #[test]
    fn extensible_float_writer_emits_fact_chunk() {
        let spec =
            AudioSpec::new_with_speaker_mask(48_000, 2, SampleFormat::Float32, Some(3)).unwrap();
        let audio = PcmBuffer::new(spec, Samples::F32(vec![0.25, -0.5])).unwrap();
        let mut writer = WavWriter::new(Cursor::new(Vec::new()), spec).unwrap();
        assert_eq!(writer.riff_overhead, 72);
        writer.write_block(&audio).unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        assert_eq!(&bytes[60..64], b"fact");
        assert_eq!(u32::from_le_bytes(bytes[68..72].try_into().unwrap()), 1);
        let mut decoder = crate::WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.read_frames(1).unwrap().unwrap(), audio);
    }

    #[test]
    fn g711_writer_requires_signed_16_bit_pcm_input() {
        let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI24).unwrap();
        assert!(WavWriter::new_g711(Cursor::new(Vec::new()), spec, G711Encoding::ALaw).is_err());
    }

    #[test]
    fn extensible_wave_preserves_channel_mask_through_write_and_read() {
        let mask = 0x33;
        let spec =
            AudioSpec::new_with_speaker_mask(48_000, 4, SampleFormat::PcmI16, Some(mask)).unwrap();
        let audio = PcmBuffer::new(spec, Samples::I16(vec![1, 2, 3, 4])).unwrap();
        let bytes = super::write_wav(Cursor::new(Vec::new()), &audio)
            .unwrap()
            .into_inner();

        assert_eq!(
            u16::from_le_bytes(bytes[20..22].try_into().unwrap()),
            0xfffe
        );
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 40);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), mask);
        assert_eq!(
            &bytes[44..60],
            &[
                1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71
            ]
        );
        let mut decoder = crate::WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.spec(), spec);
        assert_eq!(
            decoder.read_frames(1).unwrap().unwrap().samples(),
            &Samples::I16(vec![1, 2, 3, 4])
        );
    }

    #[test]
    fn rf64_writer_and_reader_round_trip_and_patch_ds64_sizes() {
        let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
        let audio = PcmBuffer::new(spec, Samples::I16(vec![-123, 456])).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("title", "RF64 test");
        let mut writer = WavWriter::new_with_metadata_and_container(
            Cursor::new(Vec::new()),
            spec,
            &metadata,
            WavContainer::Rf64,
        )
        .unwrap();
        writer.write_block(&audio).unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        assert_eq!(&bytes[..4], b"RF64");
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            u32::MAX
        );
        assert_eq!(&bytes[12..16], b"ds64");
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 28);
        assert_eq!(
            u64::from_le_bytes(bytes[20..28].try_into().unwrap()),
            (bytes.len() - 8) as u64
        );
        assert_eq!(u64::from_le_bytes(bytes[28..36].try_into().unwrap()), 4);
        assert_eq!(u64::from_le_bytes(bytes[36..44].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[44..48].try_into().unwrap()), 0);
        assert_eq!(
            u32::from_le_bytes(bytes[76..80].try_into().unwrap()),
            u32::MAX
        );

        let mut decoder = crate::WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.container(), WavContainer::Rf64);
        assert_eq!(decoder.metadata(), &metadata);
        assert_eq!(decoder.read_frames(2).unwrap().unwrap(), audio);
        assert!(decoder.read_frames(1).unwrap().is_none());
    }

    #[test]
    fn rf64_writer_patches_sizes_above_four_gib_without_allocating_audio() {
        let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmU8).unwrap();
        let mut writer =
            WavWriter::new_with_container(SparseWriter::default(), spec, WavContainer::Rf64)
                .unwrap();
        let data_bytes = u64::from(u32::MAX) + 1;
        let data_start = writer.data_size_offset + 4;
        writer.data_bytes = data_bytes;
        writer
            .writer
            .seek(SeekFrom::Start(data_start + data_bytes - 1))
            .unwrap();
        writer.writer.write_all(&[0]).unwrap();

        let output = writer.finish().unwrap();
        let read_u64 = |offset| {
            let bytes: [u8; 8] = (0_u64..8)
                .map(|index| *output.bytes.get(&(offset + index)).unwrap())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap();
            u64::from_le_bytes(bytes)
        };
        assert_eq!(read_u64(20), 72 + data_bytes);
        assert_eq!(read_u64(28), data_bytes);
        assert_eq!(read_u64(36), data_bytes);
        assert_eq!(
            (0..4)
                .map(|i| *output.bytes.get(&(76 + i)).unwrap())
                .collect::<Vec<_>>(),
            u32::MAX.to_le_bytes()
        );
        let mut decoder = crate::WavDecoder::new(output).unwrap();
        assert_eq!(decoder.container(), WavContainer::Rf64);
        assert_eq!(
            decoder.read_frames(2).unwrap().unwrap().samples(),
            &Samples::U8(vec![0, 0])
        );
    }

    #[test]
    fn rf64_float_fact_uses_sentinel_when_frame_count_exceeds_u32() {
        let spec = AudioSpec::new(48_000, 1, SampleFormat::Float32).unwrap();
        let mut writer =
            WavWriter::new_with_container(SparseWriter::default(), spec, WavContainer::Rf64)
                .unwrap();
        assert_eq!(writer.riff_overhead, 86);
        let data_bytes = (u64::from(u32::MAX) + 1) * 4;
        let data_start = writer.data_size_offset + 4;
        writer.data_bytes = data_bytes;
        writer
            .writer
            .seek(SeekFrom::Start(data_start + data_bytes - 1))
            .unwrap();
        writer.writer.write_all(&[0]).unwrap();

        let mut output = writer.finish().unwrap();
        output.seek(SeekFrom::Start(82)).unwrap();
        let mut fact = [0; 4];
        output.read_exact(&mut fact).unwrap();
        assert_eq!(u32::from_le_bytes(fact), u32::MAX);
        output.seek(SeekFrom::Start(36)).unwrap();
        let mut ds64_sample_count = [0; 8];
        output.read_exact(&mut ds64_sample_count).unwrap();
        assert_eq!(
            u64::from_le_bytes(ds64_sample_count),
            u64::from(u32::MAX) + 1
        );
    }
}
