use std::io::Cursor;

use wav::{WavDecoder, WavWriter, write_wav_with_metadata};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples, transcode,
};

#[test]
fn metadata_survives_wav_write_and_streaming_transcode() {
    let audio = PcmBuffer::new(
        AudioSpec::new(44_100, 1, SampleFormat::PcmI16).unwrap(),
        Samples::I16(vec![-2, 0, 2]),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "Track");
    metadata.insert("artist", "First");
    metadata.insert("artist", "Second");
    let input = write_wav_with_metadata(Cursor::new(Vec::new()), &audio, &metadata)
        .unwrap()
        .into_inner();

    let mut decoder = WavDecoder::new(Cursor::new(input)).unwrap();
    let mut encoder = WavWriter::new(Cursor::new(Vec::new()), audio.spec()).unwrap();
    assert_eq!(transcode(&mut decoder, &mut encoder, 2).unwrap(), 3);
    assert!(encoder.write_block(&audio).is_err());
    let output = encoder.finish().unwrap().into_inner();

    let decoder = WavDecoder::new(Cursor::new(output)).unwrap();
    assert_eq!(decoder.metadata(), &metadata);
}

#[test]
fn wav_writer_rejects_metadata_it_cannot_represent() {
    let audio = PcmBuffer::new(
        AudioSpec::new(44_100, 1, SampleFormat::PcmU8).unwrap(),
        Samples::U8(vec![128]),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("composer", "Unknown mapping");
    assert!(
        WavWriter::new_with_metadata(Cursor::new(Vec::new()), audio.spec(), &metadata).is_err()
    );
}
