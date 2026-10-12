use std::io::Cursor;

use wavpack::{WavPackDecoder, WavPackDsdDecoder, WavPackDsdEncoder, WavPackEncoder};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioSpec, DsdBuffer, DsdDecoder, DsdEncoder, DsdSpec, PcmBuffer,
    SampleFormat, Samples,
};

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
