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

#[test]
fn wav_ima_adpcm_streams_mono_and_stereo_with_fact_trimming() {
    for channels in [1, 2] {
        let spec = AudioSpec::new(32_000, channels, SampleFormat::PcmI16).unwrap();
        let source: Vec<i16> = (0..257 * usize::from(channels))
            .map(|index| ((index * 97) as i32 - 12_000).clamp(-32_768, 32_767) as i16)
            .collect();
        let audio = PcmBuffer::new(spec, Samples::I16(source.clone())).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("title", "IMA block test");
        let mut writer = WavWriter::new_ima_adpcm_with_metadata_and_container(
            Cursor::new(Vec::new()),
            spec,
            &metadata,
            wav::WavContainer::Rf64,
        )
        .unwrap();
        let Samples::I16(audio_samples) = audio.samples() else {
            unreachable!();
        };
        for block in audio_samples.chunks(41 * usize::from(channels)) {
            writer
                .write_block(&PcmBuffer::new(spec, Samples::I16(block.to_vec())).unwrap())
                .unwrap();
        }
        let bytes = writer.finish().unwrap().into_inner();
        assert_eq!(&bytes[12..16], b"ds64");
        let rf64_header = 36;
        assert_eq!(
            u16::from_le_bytes(
                bytes[20 + rf64_header..22 + rf64_header]
                    .try_into()
                    .unwrap()
            ),
            0x0011
        );
        assert_eq!(
            u16::from_le_bytes(
                bytes[34 + rf64_header..36 + rf64_header]
                    .try_into()
                    .unwrap()
            ),
            4
        );
        assert_eq!(
            u32::from_le_bytes(
                bytes[48 + rf64_header..52 + rf64_header]
                    .try_into()
                    .unwrap()
            ),
            257
        );

        let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.spec(), spec);
        assert_eq!(decoder.metadata(), &metadata);
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(17).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("IMA ADPCM must decode to signed 16-bit PCM");
            };
            decoded.extend_from_slice(samples);
        }
        assert_eq!(decoded.len(), source.len());
        let max_error = source
            .iter()
            .zip(decoded)
            .map(|(left, right)| (i32::from(*left) - i32::from(right)).abs())
            .max()
            .unwrap();
        assert!(max_error < 3_000, "maximum IMA ADPCM error was {max_error}");
    }
}

#[test]
fn wav_ms_adpcm_streams_mono_and_stereo_with_fact_trimming() {
    for channels in [1, 2] {
        let spec = AudioSpec::new(32_000, channels, SampleFormat::PcmI16).unwrap();
        let source: Vec<i16> = (0..257 * usize::from(channels))
            .map(|index| ((index * 97) as i32 - 12_000).clamp(-32_768, 32_767) as i16)
            .collect();
        let audio = PcmBuffer::new(spec, Samples::I16(source.clone())).unwrap();
        let mut metadata = Metadata::default();
        metadata.insert("title", "Microsoft ADPCM block test");
        let mut writer = WavWriter::new_ms_adpcm_with_metadata_and_container(
            Cursor::new(Vec::new()),
            spec,
            &metadata,
            wav::WavContainer::Rf64,
        )
        .unwrap();
        let Samples::I16(audio_samples) = audio.samples() else {
            unreachable!();
        };
        for block in audio_samples.chunks(41 * usize::from(channels)) {
            writer
                .write_block(&PcmBuffer::new(spec, Samples::I16(block.to_vec())).unwrap())
                .unwrap();
        }
        let bytes = writer.finish().unwrap().into_inner();
        assert_eq!(
            u16::from_le_bytes(bytes[56..58].try_into().unwrap()),
            0x0002
        );
        assert_eq!(u16::from_le_bytes(bytes[70..72].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(bytes[114..118].try_into().unwrap()), 257);
        assert_eq!(
            u64::from_le_bytes(bytes[28..36].try_into().unwrap()),
            if channels == 1 { 256 } else { 512 }
        );

        let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.spec(), spec);
        assert_eq!(decoder.metadata(), &metadata);
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(17).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("Microsoft ADPCM must decode to signed 16-bit PCM");
            };
            decoded.extend_from_slice(samples);
        }
        assert_eq!(decoded.len(), source.len());
        let max_error = source
            .iter()
            .zip(decoded)
            .map(|(left, right)| (i32::from(*left) - i32::from(right)).abs())
            .max()
            .unwrap();
        assert!(
            max_error < 4_000,
            "maximum Microsoft ADPCM error was {max_error}"
        );
    }
}

#[test]
fn wav_ms_adpcm_accepts_bounded_custom_coefficients_and_requires_fact() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![100; 20])).unwrap();
    let mut writer = WavWriter::new_ms_adpcm(Cursor::new(Vec::new()), spec).unwrap();
    writer.write_block(&audio).unwrap();
    let mut bytes = writer.finish().unwrap().into_inner();

    bytes.splice(70..70, [0, 1, 0, 0]);
    let riff_size = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
    bytes[16..20].copy_from_slice(&54_u32.to_le_bytes());
    bytes[36..38].copy_from_slice(&36_u16.to_le_bytes());
    bytes[40..42].copy_from_slice(&8_u16.to_le_bytes());
    bytes[94] = 7;
    let mut decoder = WavDecoder::new(Cursor::new(bytes.clone())).unwrap();
    assert!(decoder.read_frames(1).unwrap().is_some());

    let fact = bytes
        .windows(4)
        .position(|window| window == b"fact")
        .unwrap();
    bytes.drain(fact..fact + 12);
    let riff_size = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
    assert!(WavDecoder::new(Cursor::new(bytes)).is_err());
}
