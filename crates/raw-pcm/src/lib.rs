use std::io::{Read, Write};

use zemer_core::{
    AudioDecoder, AudioEncoder, AudioError, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples,
};

const MAX_BLOCK_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawPcmFormat {
    U8,
    I16Le,
    I16Be,
    I24Le,
    I24Be,
    I32Le,
    I32Be,
    F32Le,
    F32Be,
    F64Le,
    F64Be,
}

impl RawPcmFormat {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "raw_u8" => Self::U8,
            "raw_s16le" => Self::I16Le,
            "raw_s16be" => Self::I16Be,
            "raw_s24le" => Self::I24Le,
            "raw_s24be" => Self::I24Be,
            "raw_s32le" => Self::I32Le,
            "raw_s32be" => Self::I32Be,
            "raw_f32le" => Self::F32Le,
            "raw_f32be" => Self::F32Be,
            "raw_f64le" => Self::F64Le,
            "raw_f64be" => Self::F64Be,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::U8 => "raw_u8",
            Self::I16Le => "raw_s16le",
            Self::I16Be => "raw_s16be",
            Self::I24Le => "raw_s24le",
            Self::I24Be => "raw_s24be",
            Self::I32Le => "raw_s32le",
            Self::I32Be => "raw_s32be",
            Self::F32Le => "raw_f32le",
            Self::F32Be => "raw_f32be",
            Self::F64Le => "raw_f64le",
            Self::F64Be => "raw_f64be",
        }
    }

    pub fn sample_format(self) -> SampleFormat {
        match self {
            Self::U8 => SampleFormat::PcmU8,
            Self::I16Le | Self::I16Be => SampleFormat::PcmI16,
            Self::I24Le | Self::I24Be => SampleFormat::PcmI24,
            Self::I32Le | Self::I32Be => SampleFormat::PcmI32,
            Self::F32Le | Self::F32Be => SampleFormat::Float32,
            Self::F64Le | Self::F64Be => SampleFormat::Float64,
        }
    }

    fn bytes_per_sample(self) -> usize {
        match self.sample_format() {
            SampleFormat::PcmU8 => 1,
            SampleFormat::PcmI16 => 2,
            SampleFormat::PcmI24 => 3,
            SampleFormat::PcmI32 | SampleFormat::Float32 => 4,
            SampleFormat::Float64 => 8,
        }
    }

    fn little_endian(self) -> bool {
        matches!(
            self,
            Self::I16Le | Self::I24Le | Self::I32Le | Self::F32Le | Self::F64Le
        )
    }
}

pub struct RawPcmDecoder<R> {
    reader: R,
    spec: AudioSpec,
    format: RawPcmFormat,
    metadata: Metadata,
}

impl<R: Read> RawPcmDecoder<R> {
    pub fn new(
        reader: R,
        sample_rate: u32,
        channels: u16,
        format: RawPcmFormat,
    ) -> Result<Self, AudioError> {
        let spec = AudioSpec::new(sample_rate, channels, format.sample_format())?;
        Ok(Self {
            reader,
            spec,
            format,
            metadata: Metadata::default(),
        })
    }
}

impl<R: Read> AudioDecoder for RawPcmDecoder<R> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    fn read_frames(&mut self, max_frames: usize) -> Result<Option<PcmBuffer>, AudioError> {
        if max_frames == 0 {
            return Err(AudioError::InvalidData(
                "max_frames must be greater than zero",
            ));
        }
        let frame_bytes = self
            .format
            .bytes_per_sample()
            .checked_mul(usize::from(self.spec.channels()))
            .ok_or(AudioError::InvalidData("raw PCM frame size overflow"))?;
        let frames = max_frames.min((MAX_BLOCK_BYTES / frame_bytes).max(1));
        let byte_count = frames
            .checked_mul(frame_bytes)
            .ok_or(AudioError::InvalidData("raw PCM block size overflow"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|_| AudioError::InvalidData("raw PCM block allocation failed"))?;
        bytes.resize(byte_count, 0);
        let mut received = 0;
        while received < byte_count {
            match self.reader.read(&mut bytes[received..]) {
                Ok(0) => break,
                Ok(count) => received += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
        if received == 0 {
            return Ok(None);
        }
        if received % frame_bytes != 0 {
            return Err(AudioError::InvalidFormat(
                "raw PCM input ends in a partial sample frame",
            ));
        }
        bytes.truncate(received);
        decode_samples(self.format, &bytes)
            .and_then(|samples| PcmBuffer::new(self.spec, samples).map_err(Into::into))
            .map(Some)
    }
}

pub struct RawPcmEncoder<W> {
    writer: W,
    spec: AudioSpec,
    format: RawPcmFormat,
    poisoned: bool,
}

impl<W: Write> RawPcmEncoder<W> {
    pub fn new(writer: W, spec: AudioSpec, format: RawPcmFormat) -> Result<Self, AudioError> {
        if spec.sample_format() != format.sample_format() {
            return Err(AudioError::Unsupported(
                "raw PCM output format does not match the sample specification",
            ));
        }
        Ok(Self {
            writer,
            spec,
            format,
            poisoned: false,
        })
    }

    pub fn into_inner(self) -> W {
        self.writer
    }
}

impl<W: Write> AudioEncoder for RawPcmEncoder<W> {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn write_block(&mut self, block: &PcmBuffer) -> Result<(), AudioError> {
        if self.poisoned {
            return Err(AudioError::InvalidData(
                "raw PCM writer is unusable after an earlier write error",
            ));
        }
        if block.spec() != self.spec {
            return Err(AudioError::InvalidData(
                "PCM block does not match raw PCM writer format",
            ));
        }
        let byte_count = block
            .samples()
            .len()
            .checked_mul(self.format.bytes_per_sample())
            .filter(|&size| size <= MAX_BLOCK_BYTES)
            .ok_or(AudioError::InvalidData("raw PCM output block is too large"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|_| AudioError::InvalidData("raw PCM output allocation failed"))?;
        encode_samples(self.format, block.samples(), &mut bytes)?;
        if let Err(error) = self.writer.write_all(&bytes) {
            self.poisoned = true;
            return Err(error.into());
        }
        Ok(())
    }
}

fn decode_samples(format: RawPcmFormat, bytes: &[u8]) -> Result<Samples, AudioError> {
    Ok(match format {
        RawPcmFormat::U8 => Samples::U8(bytes.to_vec()),
        RawPcmFormat::I16Le | RawPcmFormat::I16Be => Samples::I16(
            bytes
                .chunks_exact(2)
                .map(|chunk| {
                    let pair = [chunk[0], chunk[1]];
                    if format.little_endian() {
                        i16::from_le_bytes(pair)
                    } else {
                        i16::from_be_bytes(pair)
                    }
                })
                .collect(),
        ),
        RawPcmFormat::I24Le | RawPcmFormat::I24Be => Samples::I24(
            bytes
                .chunks_exact(3)
                .map(|chunk| {
                    let ordered = if format.little_endian() {
                        [chunk[2], chunk[1], chunk[0]]
                    } else {
                        [chunk[0], chunk[1], chunk[2]]
                    };
                    let sign = if ordered[0] & 0x80 != 0 { 0xff } else { 0 };
                    i32::from_be_bytes([sign, ordered[0], ordered[1], ordered[2]])
                })
                .collect(),
        ),
        RawPcmFormat::I32Le | RawPcmFormat::I32Be => Samples::I32(
            bytes
                .chunks_exact(4)
                .map(|chunk| {
                    let array = chunk.try_into().unwrap();
                    if format.little_endian() {
                        i32::from_le_bytes(array)
                    } else {
                        i32::from_be_bytes(array)
                    }
                })
                .collect(),
        ),
        RawPcmFormat::F32Le | RawPcmFormat::F32Be => Samples::F32(
            bytes
                .chunks_exact(4)
                .map(|chunk| {
                    let array = chunk.try_into().unwrap();
                    f32::from_bits(if format.little_endian() {
                        u32::from_le_bytes(array)
                    } else {
                        u32::from_be_bytes(array)
                    })
                })
                .collect(),
        ),
        RawPcmFormat::F64Le | RawPcmFormat::F64Be => Samples::F64(
            bytes
                .chunks_exact(8)
                .map(|chunk| {
                    let array = chunk.try_into().unwrap();
                    f64::from_bits(if format.little_endian() {
                        u64::from_le_bytes(array)
                    } else {
                        u64::from_be_bytes(array)
                    })
                })
                .collect(),
        ),
    })
}

fn encode_samples(
    format: RawPcmFormat,
    samples: &Samples,
    output: &mut Vec<u8>,
) -> Result<(), AudioError> {
    match (format, samples) {
        (RawPcmFormat::U8, Samples::U8(values)) => output.extend_from_slice(values),
        (RawPcmFormat::I16Le, Samples::I16(values)) => {
            for value in values {
                output.extend_from_slice(&value.to_le_bytes());
            }
        }
        (RawPcmFormat::I16Be, Samples::I16(values)) => {
            for value in values {
                output.extend_from_slice(&value.to_be_bytes());
            }
        }
        (RawPcmFormat::I24Le, Samples::I24(values)) => {
            for value in values {
                output.extend_from_slice(&value.to_le_bytes()[..3]);
            }
        }
        (RawPcmFormat::I24Be, Samples::I24(values)) => {
            for value in values {
                output.extend_from_slice(&value.to_be_bytes()[1..]);
            }
        }
        (RawPcmFormat::I32Le, Samples::I32(values)) => {
            for value in values {
                output.extend_from_slice(&value.to_le_bytes());
            }
        }
        (RawPcmFormat::I32Be, Samples::I32(values)) => {
            for value in values {
                output.extend_from_slice(&value.to_be_bytes());
            }
        }
        (RawPcmFormat::F32Le, Samples::F32(values)) => {
            for value in values {
                output.extend_from_slice(&value.to_bits().to_le_bytes());
            }
        }
        (RawPcmFormat::F32Be, Samples::F32(values)) => {
            for value in values {
                output.extend_from_slice(&value.to_bits().to_be_bytes());
            }
        }
        (RawPcmFormat::F64Le, Samples::F64(values)) => {
            for value in values {
                output.extend_from_slice(&value.to_bits().to_le_bytes());
            }
        }
        (RawPcmFormat::F64Be, Samples::F64(values)) => {
            for value in values {
                output.extend_from_slice(&value.to_bits().to_be_bytes());
            }
        }
        _ => {
            return Err(AudioError::InvalidData(
                "PCM storage does not match raw PCM encoding",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use super::{RawPcmDecoder, RawPcmEncoder, RawPcmFormat};
    use zemer_core::{AudioDecoder, AudioEncoder, AudioSpec, PcmBuffer, SampleFormat, Samples};

    struct OneByteReader<R>(R);

    impl<R: Read> Read for OneByteReader<R> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let length = output.len().min(1);
            self.0.read(&mut output[..length])
        }
    }

    #[test]
    fn every_sample_format_round_trips_with_partial_reads() {
        let cases = [
            (
                RawPcmFormat::U8,
                SampleFormat::PcmU8,
                Samples::U8(vec![0, 128, 255, 17]),
            ),
            (
                RawPcmFormat::I16Le,
                SampleFormat::PcmI16,
                Samples::I16(vec![i16::MIN, 1, i16::MAX, -17]),
            ),
            (
                RawPcmFormat::I16Be,
                SampleFormat::PcmI16,
                Samples::I16(vec![i16::MIN, 1, i16::MAX, -17]),
            ),
            (
                RawPcmFormat::I24Le,
                SampleFormat::PcmI24,
                Samples::I24(vec![-8_388_608, 1, 8_388_607, -17]),
            ),
            (
                RawPcmFormat::I24Be,
                SampleFormat::PcmI24,
                Samples::I24(vec![-8_388_608, 1, 8_388_607, -17]),
            ),
            (
                RawPcmFormat::I32Le,
                SampleFormat::PcmI32,
                Samples::I32(vec![i32::MIN, 1, i32::MAX, -17]),
            ),
            (
                RawPcmFormat::I32Be,
                SampleFormat::PcmI32,
                Samples::I32(vec![i32::MIN, 1, i32::MAX, -17]),
            ),
            (
                RawPcmFormat::F32Le,
                SampleFormat::Float32,
                Samples::F32(vec![-1.0, 0.0, 0.5, 1.0]),
            ),
            (
                RawPcmFormat::F32Be,
                SampleFormat::Float32,
                Samples::F32(vec![-1.0, 0.0, 0.5, 1.0]),
            ),
            (
                RawPcmFormat::F64Le,
                SampleFormat::Float64,
                Samples::F64(vec![-1.0, 0.0, 0.5, 1.0]),
            ),
            (
                RawPcmFormat::F64Be,
                SampleFormat::Float64,
                Samples::F64(vec![-1.0, 0.0, 0.5, 1.0]),
            ),
        ];
        for (format, sample_format, samples) in cases {
            let spec = AudioSpec::new(48_000, 2, sample_format).unwrap();
            let input = PcmBuffer::new(spec, samples).unwrap();
            let mut encoder = RawPcmEncoder::new(Cursor::new(Vec::new()), spec, format).unwrap();
            encoder.write_block(&input).unwrap();
            let bytes = encoder.into_inner().into_inner();
            let mut decoder =
                RawPcmDecoder::new(OneByteReader(Cursor::new(bytes)), 48_000, 2, format).unwrap();
            let mut first_frame = input.clone();
            first_frame.truncate_frames(1).unwrap();
            assert_eq!(decoder.read_frames(1).unwrap().unwrap(), first_frame);
            assert_eq!(decoder.read_frames(8).unwrap().unwrap().frames(), 1);
            assert!(decoder.read_frames(1).unwrap().is_none());
        }
    }

    #[test]
    fn float_encoding_preserves_ieee_bits() {
        for (format, expected) in [
            (RawPcmFormat::F32Le, 0x8000_0000_u32.to_le_bytes().to_vec()),
            (RawPcmFormat::F32Be, 0x8000_0000_u32.to_be_bytes().to_vec()),
        ] {
            let spec = AudioSpec::new(48_000, 1, SampleFormat::Float32).unwrap();
            let input =
                PcmBuffer::new(spec, Samples::F32(vec![f32::from_bits(0x8000_0000)])).unwrap();
            let mut encoder = RawPcmEncoder::new(Cursor::new(Vec::new()), spec, format).unwrap();
            encoder.write_block(&input).unwrap();
            assert_eq!(encoder.into_inner().into_inner(), expected);
        }
    }

    #[test]
    fn rejects_incomplete_final_frames() {
        let mut decoder = RawPcmDecoder::new(
            Cursor::new(vec![0_u8, 1, 2]),
            48_000,
            2,
            RawPcmFormat::I16Le,
        )
        .unwrap();
        assert!(decoder.read_frames(8).is_err());
    }
}
