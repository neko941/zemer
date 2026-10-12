use std::{error::Error, fmt, io};

pub use zemer_pcm::{AudioSpec, PcmBuffer, PcmError, SampleFormat, Samples, convert_sample_format};
pub use zemer_metadata::Metadata;

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

impl From<PcmError> for AudioError {
    fn from(error: PcmError) -> Self {
        Self::InvalidData(error.0)
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
