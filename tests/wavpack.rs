use std::io::{self, Cursor, Seek, SeekFrom, Write};

use wavpack::{WavPackDecoder, WavPackDsdDecoder, WavPackDsdEncoder, WavPackEncoder};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioSpec, DsdBuffer, DsdDecoder, DsdEncoder, DsdSpec, PcmBuffer,
    SampleFormat, Samples,
};

struct FailOnceWriter {
    inner: Cursor<Vec<u8>>,
    bytes_before_failure: Option<usize>,
}

impl Write for FailOnceWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(remaining) = self.bytes_before_failure {
            if remaining == 0 {
                self.bytes_before_failure = None;
                return Err(io::Error::other("injected write failure"));
            }
            if bytes.len() > remaining {
                let written = self.inner.write(&bytes[..remaining])?;
                self.bytes_before_failure = Some(0);
                return Ok(written);
            }
            self.bytes_before_failure = Some(remaining - bytes.len());
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Seek for FailOnceWriter {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

#[test]
fn dsd_wavpack_encoder_round_trips_raw_octets_and_bit_rate() {
    let spec = DsdSpec::new(2_822_400, 2).unwrap();
    let samples = vec![0x80, 0x7f, 0x55, 0xaa, 0xff, 0x00];
    let source = DsdBuffer::new(spec, samples.clone()).unwrap();
    let mut encoder = WavPackDsdEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_dsd_octets(&source).unwrap();
    encoder.finish().unwrap();

    let bytes = encoder.into_inner().into_inner();
    let mut decoder = WavPackDsdDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.dsd_spec(), spec);
    assert_eq!(
        decoder.read_dsd_octets(16).unwrap().unwrap().bytes(),
        samples
    );
    assert!(decoder.read_dsd_octets(16).unwrap().is_none());
}

#[test]
fn dsd_wavpack_encoder_round_trips_multichannel_octets_and_speaker_mask() {
    for channels in [3, 6] {
        let speaker_mask = (1_u32 << channels) - 1;
        let spec = DsdSpec::new_with_speaker_mask(2_822_400, channels, Some(speaker_mask)).unwrap();
        let samples = (0..usize::from(channels) * 3)
            .map(|value| (value * 13 + 7) as u8)
            .collect::<Vec<_>>();
        let source = DsdBuffer::new(spec, samples.clone()).unwrap();
        let mut encoder = WavPackDsdEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_dsd_octets(&source).unwrap();
        encoder.finish().unwrap();

        let bytes = encoder.into_inner().into_inner();
        let mut decoder = WavPackDsdDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.dsd_spec(), spec);
        assert_eq!(
            decoder.read_dsd_octets(16).unwrap().unwrap().bytes(),
            samples
        );
        assert!(decoder.read_dsd_octets(16).unwrap().is_none());
    }
}

#[test]
fn wavpack_encoder_round_trips_multichannel_pcm_and_speaker_mask() {
    let spec =
        AudioSpec::new_with_speaker_mask(48_000, 3, SampleFormat::PcmI16, Some(0x7)).unwrap();
    let source = PcmBuffer::new(spec, Samples::I16(vec![1, 10, 100, -2, -20, -200])).unwrap();
    let mut encoder = WavPackEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_block(&source).unwrap();
    encoder.finish().unwrap();

    let bytes = encoder.into_inner().into_inner();
    let mut decoder = WavPackDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.spec(), spec);
    assert_eq!(decoder.read_frames(16).unwrap().unwrap(), source);
    assert!(decoder.read_frames(16).unwrap().is_none());
}

#[test]
fn wavpack_encoder_preserves_float32_bits_and_custom_rate() {
    let expected = [
        0x0000_0000,
        0x8000_0000,
        0x0000_0001,
        0x007f_ffff,
        0x3f80_0000,
        0xbf80_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7fc1_2345,
        0xff80_0001,
        0x7f7f_ffff,
        0xffc5_4321,
    ];
    for channels in [2, 3] {
        let spec = if channels == 2 {
            AudioSpec::new(123_456, channels, SampleFormat::Float32).unwrap()
        } else {
            AudioSpec::new_with_speaker_mask(123_456, channels, SampleFormat::Float32, Some(0x7))
                .unwrap()
        };
        let source =
            PcmBuffer::new(spec, Samples::F32(expected.map(f32::from_bits).to_vec())).unwrap();
        let mut encoder = WavPackEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
        encoder.write_block(&source).unwrap();
        encoder.finish().unwrap();

        let bytes = encoder.into_inner().into_inner();
        let mut decoder = WavPackDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.spec(), spec);
        let Samples::F32(decoded) = decoder.read_frames(16).unwrap().unwrap().samples().clone()
        else {
            panic!("WavPack float encoder decoded to a non-float format");
        };
        assert_eq!(
            decoded
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(decoder.read_frames(16).unwrap().is_none());
    }
}

#[test]
fn wavpack_encoder_splits_float32_at_the_block_boundary() {
    let spec = AudioSpec::new(48_000, 2, SampleFormat::Float32).unwrap();
    let frame_count = 32_769;
    let pattern = [0x0000_0000, 0x8000_0000, 0x3f80_0000, 0x7fc1_2345];
    let expected = (0..frame_count * 2)
        .map(|index| pattern[index % pattern.len()])
        .collect::<Vec<_>>();
    let source = PcmBuffer::new(
        spec,
        Samples::F32(expected.iter().copied().map(f32::from_bits).collect()),
    )
    .unwrap();
    let mut encoder = WavPackEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.write_block(&source).unwrap();
    encoder.finish().unwrap();

    let bytes = encoder.into_inner().into_inner();
    let mut decoder = WavPackDecoder::new(Cursor::new(bytes)).unwrap();
    let mut actual = Vec::with_capacity(expected.len());
    while let Some(block) = decoder.read_frames(8192).unwrap() {
        let Samples::F32(samples) = block.samples() else {
            panic!("WavPack float encoder decoded to a non-float format");
        };
        actual.extend(samples.iter().map(|sample| sample.to_bits()));
    }
    assert_eq!(actual, expected);
}

#[test]
fn pcm_and_dsd_writers_cannot_finish_after_a_partial_write_failure() {
    let writer = FailOnceWriter {
        inner: Cursor::new(Vec::new()),
        bytes_before_failure: Some(16),
    };
    let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
    let mut encoder = WavPackEncoder::new(writer, spec).unwrap();
    encoder
        .write_block(&PcmBuffer::new(spec, Samples::I16(vec![123])).unwrap())
        .unwrap();
    assert!(encoder.finish().is_err());
    assert!(encoder.finish().is_err());

    let writer = FailOnceWriter {
        inner: Cursor::new(Vec::new()),
        bytes_before_failure: Some(16),
    };
    let spec = DsdSpec::new(2_822_400, 1).unwrap();
    let mut encoder = WavPackDsdEncoder::new(writer, spec).unwrap();
    encoder
        .write_dsd_octets(&DsdBuffer::new(spec, vec![0x80]).unwrap())
        .unwrap();
    assert!(encoder.finish().is_err());
    assert!(encoder.finish().is_err());
}
