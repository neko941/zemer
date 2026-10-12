use std::{collections::BTreeMap, error::Error, fmt, io};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleFormat {
    PcmU8,
    PcmI16,
    PcmI24,
    PcmI32,
    Float32,
    Float64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioSpec {
    sample_rate: u32,
    channels: u16,
    sample_format: SampleFormat,
    speaker_mask: Option<u32>,
}

impl AudioSpec {
    pub fn new(
        sample_rate: u32,
        channels: u16,
        sample_format: SampleFormat,
    ) -> Result<Self, AudioError> {
        Self::new_with_speaker_mask(sample_rate, channels, sample_format, None)
    }

    pub fn new_with_speaker_mask(
        sample_rate: u32,
        channels: u16,
        sample_format: SampleFormat,
        speaker_mask: Option<u32>,
    ) -> Result<Self, AudioError> {
        if sample_rate == 0 || channels == 0 {
            return Err(AudioError::InvalidData(
                "sample rate and channel count must be non-zero",
            ));
        }
        Ok(Self {
            sample_rate,
            channels,
            sample_format,
            speaker_mask,
        })
    }

    pub fn sample_rate(self) -> u32 {
        self.sample_rate
    }
    pub fn channels(self) -> u16 {
        self.channels
    }
    pub fn sample_format(self) -> SampleFormat {
        self.sample_format
    }
    pub fn speaker_mask(self) -> Option<u32> {
        self.speaker_mask
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Samples {
    U8(Vec<u8>),
    I16(Vec<i16>),
    I24(Vec<i32>),
    I32(Vec<i32>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

impl Samples {
    pub fn len(&self) -> usize {
        match self {
            Self::U8(values) => values.len(),
            Self::I16(values) => values.len(),
            Self::I24(values) | Self::I32(values) => values.len(),
            Self::F32(values) => values.len(),
            Self::F64(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn matches(&self, format: SampleFormat) -> bool {
        matches!(
            (self, format),
            (Self::U8(_), SampleFormat::PcmU8)
                | (Self::I16(_), SampleFormat::PcmI16)
                | (Self::I24(_), SampleFormat::PcmI24)
                | (Self::I32(_), SampleFormat::PcmI32)
                | (Self::F32(_), SampleFormat::Float32)
                | (Self::F64(_), SampleFormat::Float64)
        )
    }

    fn valid(&self) -> bool {
        match self {
            Self::I24(values) => values
                .iter()
                .all(|&sample| (-8_388_608..=8_388_607).contains(&sample)),
            _ => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PcmBuffer {
    spec: AudioSpec,
    samples: Samples,
}

impl PcmBuffer {
    pub fn new(spec: AudioSpec, samples: Samples) -> Result<Self, AudioError> {
        if !samples.matches(spec.sample_format) {
            return Err(AudioError::InvalidData(
                "sample storage does not match the declared sample format",
            ));
        }
        if samples.len() % usize::from(spec.channels) != 0 {
            return Err(AudioError::InvalidData(
                "sample count is not a whole number of frames",
            ));
        }
        if !samples.valid() {
            return Err(AudioError::InvalidData(
                "24-bit PCM sample is outside the signed 24-bit range",
            ));
        }
        Ok(Self { spec, samples })
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.spec.channels)
    }

    pub fn spec(&self) -> AudioSpec {
        self.spec
    }
    pub fn samples(&self) -> &Samples {
        &self.samples
    }
}

/// Converts sample values without changing the sample rate or channel count.
///
/// Integer output rounds to the nearest representable value. Float samples
/// outside `[-1.0, 1.0]` cannot be converted to integer PCM.
pub fn convert_sample_format(
    audio: &PcmBuffer,
    target: SampleFormat,
) -> Result<PcmBuffer, AudioError> {
    if audio.spec.sample_format == target {
        return Ok(audio.clone());
    }

    let samples = normalized_samples(&audio.samples)?;
    let converted = match target {
        SampleFormat::PcmU8 => Samples::U8(
            samples
                .iter()
                .map(|&sample| quantize(sample, 128.0, 128.0, 0.0, 255.0))
                .map(|sample| sample.map(|value| value as u8))
                .collect::<Result<_, _>>()?,
        ),
        SampleFormat::PcmI16 => Samples::I16(
            samples
                .iter()
                .map(|&sample| quantize(sample, 32_768.0, 0.0, -32_768.0, 32_767.0))
                .map(|sample| sample.map(|value| value as i16))
                .collect::<Result<_, _>>()?,
        ),
        SampleFormat::PcmI24 => Samples::I24(
            samples
                .iter()
                .map(|&sample| quantize(sample, 8_388_608.0, 0.0, -8_388_608.0, 8_388_607.0))
                .collect::<Result<_, _>>()?,
        ),
        SampleFormat::PcmI32 => Samples::I32(
            samples
                .iter()
                .map(|&sample| {
                    quantize(
                        sample,
                        2_147_483_648.0,
                        0.0,
                        -2_147_483_648.0,
                        2_147_483_647.0,
                    )
                })
                .collect::<Result<_, _>>()?,
        ),
        SampleFormat::Float32 => {
            let values = samples
                .into_iter()
                .map(|sample| {
                    let value = sample as f32;
                    if sample.is_finite() && !value.is_finite() {
                        return Err(AudioError::InvalidData(
                            "sample is outside the Float32 range",
                        ));
                    }
                    Ok(value)
                })
                .collect::<Result<_, _>>()?;
            Samples::F32(values)
        }
        SampleFormat::Float64 => Samples::F64(samples),
    };
    let spec = AudioSpec::new_with_speaker_mask(
        audio.spec.sample_rate,
        audio.spec.channels,
        target,
        audio.spec.speaker_mask,
    )?;
    PcmBuffer::new(spec, converted)
}

fn normalized_samples(samples: &Samples) -> Result<Vec<f64>, AudioError> {
    let mut normalized = Vec::new();
    normalized
        .try_reserve_exact(samples.len())
        .map_err(|_| AudioError::InvalidData("audio block is too large"))?;
    match samples {
        Samples::U8(values) => {
            normalized.extend(values.iter().map(|&v| (f64::from(v) - 128.0) / 128.0))
        }
        Samples::I16(values) => normalized.extend(values.iter().map(|&v| f64::from(v) / 32_768.0)),
        Samples::I24(values) => {
            normalized.extend(values.iter().map(|&v| f64::from(v) / 8_388_608.0))
        }
        Samples::I32(values) => {
            normalized.extend(values.iter().map(|&v| f64::from(v) / 2_147_483_648.0))
        }
        Samples::F32(values) => normalized.extend(values.iter().map(|&v| f64::from(v))),
        Samples::F64(values) => normalized.extend_from_slice(values),
    }
    Ok(normalized)
}

fn quantize(
    sample: f64,
    scale: f64,
    offset: f64,
    minimum: f64,
    maximum: f64,
) -> Result<i32, AudioError> {
    if !sample.is_finite() || !(-1.0..=1.0).contains(&sample) {
        return Err(AudioError::InvalidData(
            "float samples outside [-1.0, 1.0] cannot be converted to integer PCM",
        ));
    }
    Ok((sample * scale + offset).round().clamp(minimum, maximum) as i32)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Metadata {
    tags: BTreeMap<String, Vec<String>>,
}

impl Metadata {
    pub fn insert(&mut self, key: impl AsRef<str>, value: impl Into<String>) {
        self.tags
            .entry(key.as_ref().trim().to_ascii_uppercase())
            .or_default()
            .push(value.into());
    }

    pub fn values(&self, key: &str) -> &[String] {
        self.tags
            .get(&key.trim().to_ascii_uppercase())
            .map_or(&[], Vec::as_slice)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.tags
            .iter()
            .map(|(key, values)| (key.as_str(), values.as_slice()))
    }
}

pub trait AudioDecoder {
    fn spec(&self) -> AudioSpec;
    fn metadata(&self) -> &Metadata;
    fn read_frames(&mut self, max_frames: usize) -> Result<Option<PcmBuffer>, AudioError>;
}

pub trait AudioEncoder {
    fn spec(&self) -> AudioSpec;
    fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError>;

    fn set_metadata(&mut self, metadata: &Metadata) -> Result<(), AudioError> {
        if metadata.iter().next().is_some() {
            return Err(AudioError::Unsupported(
                "encoder does not support audio metadata",
            ));
        }
        Ok(())
    }
}

pub fn transcode<D: AudioDecoder, E: AudioEncoder>(
    decoder: &mut D,
    encoder: &mut E,
    block_frames: usize,
) -> Result<u64, AudioError> {
    if block_frames == 0 {
        return Err(AudioError::InvalidData(
            "block_frames must be greater than zero",
        ));
    }
    if decoder.spec() != encoder.spec() {
        return Err(AudioError::InvalidData(
            "decoder and encoder PCM specifications differ",
        ));
    }
    encoder.set_metadata(decoder.metadata())?;
    let mut frames = 0_u64;
    while let Some(block) = decoder.read_frames(block_frames)? {
        encoder.write_block(&block)?;
        frames = frames
            .checked_add(block.frames() as u64)
            .ok_or(AudioError::InvalidData("transcoded frame count overflow"))?;
    }
    Ok(frames)
}

#[derive(Debug)]
pub enum AudioError {
    Io(io::Error),
    InvalidFormat(&'static str),
    InvalidData(&'static str),
    Unsupported(&'static str),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::InvalidFormat(message) => write!(f, "invalid audio format: {message}"),
            Self::InvalidData(message) => write!(f, "invalid audio data: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported audio format: {message}"),
        }
    }
}

impl Error for AudioError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for AudioError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_buffer_rejects_mismatched_storage_and_partial_frames() {
        let spec = AudioSpec::new(48_000, 2, SampleFormat::PcmI16).unwrap();
        assert!(PcmBuffer::new(spec, Samples::I32(vec![0, 0])).is_err());
        assert!(PcmBuffer::new(spec, Samples::I16(vec![0])).is_err());
    }

    #[test]
    fn metadata_keys_are_case_insensitive_and_allow_repeated_values() {
        let mut metadata = Metadata::default();
        metadata.insert("artist", "One");
        metadata.insert("ARTIST", "Two");
        assert_eq!(metadata.values("Artist"), ["One", "Two"]);
    }

    #[test]
    fn sample_format_conversion_preserves_full_scale_and_channel_shape() {
        let source = PcmBuffer::new(
            AudioSpec::new(44_100, 2, SampleFormat::Float32).unwrap(),
            Samples::F32(vec![-1.0, -0.5, 0.0, 0.5, 1.0, 0.25]),
        )
        .unwrap();
        let converted = convert_sample_format(&source, SampleFormat::PcmI16).unwrap();
        assert_eq!(converted.spec().sample_rate(), 44_100);
        assert_eq!(converted.spec().channels(), 2);
        assert_eq!(
            converted.samples(),
            &Samples::I16(vec![i16::MIN, -16_384, 0, 16_384, i16::MAX, 8_192])
        );
    }

    #[test]
    fn sample_format_conversion_preserves_channel_mask() {
        let spec =
            AudioSpec::new_with_speaker_mask(48_000, 4, SampleFormat::PcmI16, Some(0x33)).unwrap();
        let audio = PcmBuffer::new(spec, Samples::I16(vec![0, 0, 0, 0])).unwrap();
        let converted = convert_sample_format(&audio, SampleFormat::Float32).unwrap();
        assert_eq!(converted.spec().speaker_mask(), Some(0x33));
    }

    #[test]
    fn sample_format_conversion_maps_unsigned_pcm_and_rejects_clipping() {
        let source = PcmBuffer::new(
            AudioSpec::new(8_000, 1, SampleFormat::PcmU8).unwrap(),
            Samples::U8(vec![0, 128, 255]),
        )
        .unwrap();
        let converted = convert_sample_format(&source, SampleFormat::Float64).unwrap();
        assert_eq!(
            converted.samples(),
            &Samples::F64(vec![-1.0, 0.0, 127.0 / 128.0])
        );

        let over_range = PcmBuffer::new(
            AudioSpec::new(8_000, 1, SampleFormat::Float32).unwrap(),
            Samples::F32(vec![1.01]),
        )
        .unwrap();
        assert!(convert_sample_format(&over_range, SampleFormat::PcmI16).is_err());
    }

    #[test]
    fn sample_format_conversion_rejects_non_finite_pcm_and_float_overflow() {
        let non_finite = PcmBuffer::new(
            AudioSpec::new(8_000, 1, SampleFormat::Float64).unwrap(),
            Samples::F64(vec![f64::NAN]),
        )
        .unwrap();
        assert!(convert_sample_format(&non_finite, SampleFormat::PcmI24).is_err());

        let overflow = PcmBuffer::new(
            AudioSpec::new(8_000, 1, SampleFormat::Float64).unwrap(),
            Samples::F64(vec![f64::MAX]),
        )
        .unwrap();
        assert!(convert_sample_format(&overflow, SampleFormat::Float32).is_err());
    }
}
