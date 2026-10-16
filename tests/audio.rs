use std::io::Cursor;

use wav::{WavDecoder, WavWriter, rewrite_metadata, write_wav, write_wav_with_metadata};
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
fn wav_writer_declares_utf8_for_unicode_info_metadata() {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![0])).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "東京");
    let bytes = write_wav_with_metadata(Cursor::new(Vec::new()), &audio, &metadata)
        .unwrap()
        .into_inner();
    let cset = bytes
        .windows(4)
        .position(|window| window == b"CSET")
        .unwrap();
    assert_eq!(
        u32::from_le_bytes(bytes[cset + 4..cset + 8].try_into().unwrap()),
        8
    );
    assert_eq!(
        u16::from_le_bytes(bytes[cset + 8..cset + 10].try_into().unwrap()),
        65001
    );
    assert_eq!(
        WavDecoder::new(Cursor::new(bytes)).unwrap().metadata(),
        &metadata
    );
}

#[test]
fn wav_info_obeys_cset_and_rewrites_in_the_declared_code_page() {
    let original = wav_with_info(
        Some(1252),
        &[(*b"INAM", b"Caf\xe9\0"), (*b"IART", b"M\xe9dia\0")],
    );
    let decoder = WavDecoder::new(Cursor::new(original.clone())).unwrap();
    assert_eq!(decoder.metadata().values("title"), ["Café"]);
    assert_eq!(decoder.metadata().values("artist"), ["Média"]);

    let mut metadata = decoder.metadata().clone();
    metadata.remove("title");
    metadata.insert("title", "Crème brûlée");
    metadata.insert("comment", "démo");
    let rewritten = rewrite_metadata(
        Cursor::new(original.clone()),
        Cursor::new(Vec::new()),
        &metadata,
    )
    .unwrap()
    .into_inner();
    let rewritten_decoder = WavDecoder::new(Cursor::new(rewritten.clone())).unwrap();
    assert_eq!(rewritten_decoder.metadata(), &metadata);
    assert!(
        rewritten
            .windows(13)
            .any(|window| window == b"Cr\xe8me br\xfbl\xe9e\0")
    );
    assert!(
        rewritten
            .windows(14)
            .any(|window| window == b"IART\x06\0\0\0M\xe9dia\0")
    );

    metadata.remove("title");
    metadata.insert("title", "東京");
    assert!(rewrite_metadata(Cursor::new(original), Cursor::new(Vec::new()), &metadata).is_err());
}

#[test]
fn wav_info_without_cset_uses_the_riff_default_code_page() {
    let bytes = wav_with_info(None, &[(*b"INAM", b"Caf\xe9\0")]);
    let decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.metadata().values("title"), ["Café"]);
}

#[test]
fn wav_info_distinguishes_latin1_default_from_windows_1252() {
    let latin1 = wav_with_info(None, &[(*b"INAM", b"\x80\0")]);
    let windows = wav_with_info(Some(1252), &[(*b"INAM", b"\x80\0")]);
    assert_eq!(
        WavDecoder::new(Cursor::new(latin1))
            .unwrap()
            .metadata()
            .values("title"),
        ["\u{80}"]
    );
    assert_eq!(
        WavDecoder::new(Cursor::new(windows))
            .unwrap()
            .metadata()
            .values("title"),
        ["€"]
    );
}

fn wav_with_info(code_page: Option<u16>, entries: &[([u8; 4], &[u8])]) -> Vec<u8> {
    let spec = AudioSpec::new(8_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![0])).unwrap();
    let mut bytes = write_wav(Cursor::new(Vec::new()), &audio)
        .unwrap()
        .into_inner();
    let mut info = b"INFO".to_vec();
    for (id, value) in entries {
        info.extend_from_slice(id);
        info.extend_from_slice(&(value.len() as u32).to_le_bytes());
        info.extend_from_slice(value);
        if value.len() & 1 != 0 {
            info.push(0);
        }
    }
    append_wav_chunk(&mut bytes, *b"LIST", &info);
    if let Some(code_page) = code_page {
        let mut cset = Vec::from(code_page.to_le_bytes());
        cset.extend_from_slice(&1_u16.to_le_bytes());
        cset.extend_from_slice(&9_u16.to_le_bytes());
        cset.extend_from_slice(&1_u16.to_le_bytes());
        append_wav_chunk(&mut bytes, *b"CSET", &cset);
    }
    let riff_size = u32::try_from(bytes.len() - 8).unwrap();
    bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
    bytes
}

fn append_wav_chunk(bytes: &mut Vec<u8>, id: [u8; 4], body: &[u8]) {
    bytes.extend_from_slice(&id);
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(body);
    if body.len() & 1 != 0 {
        bytes.push(0);
    }
}

#[test]
fn wav_writer_uses_id3_for_non_info_metadata() {
    let audio = PcmBuffer::new(
        AudioSpec::new(44_100, 1, SampleFormat::PcmU8).unwrap(),
        Samples::U8(vec![128]),
    )
    .unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("composer", "Unknown mapping");
    let mut writer =
        WavWriter::new_with_metadata(Cursor::new(Vec::new()), audio.spec(), &metadata).unwrap();
    writer.write_block(&audio).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.metadata().values("COMPOSER"), ["Unknown mapping"]);
    assert_eq!(decoder.read_frames(1).unwrap().unwrap(), audio);
}

#[test]
fn wav_ima_adpcm_streams_one_through_eight_channels_with_fact_trimming() {
    for channels in 1..=8 {
        let spec = AudioSpec::new(32_000, channels, SampleFormat::PcmI16).unwrap();
        let source: Vec<i16> = (0..257 * usize::from(channels))
            .map(|index| ((index * 31) as i32 - 4_000).clamp(-32_768, 32_767) as i16)
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
        let fmt = bytes.windows(4).position(|chunk| chunk == b"fmt ").unwrap();
        assert_eq!(
            u16::from_le_bytes(bytes[fmt + 8..fmt + 10].try_into().unwrap()),
            if channels > 2 { 0xfffe } else { 0x0011 }
        );
        assert_eq!(
            u16::from_le_bytes(bytes[fmt + 22..fmt + 24].try_into().unwrap()),
            4
        );
        assert_eq!(
            u16::from_le_bytes(bytes[fmt + 26..fmt + 28].try_into().unwrap()),
            505
        );
        let fact = bytes.windows(4).position(|chunk| chunk == b"fact").unwrap();
        assert_eq!(
            u32::from_le_bytes(bytes[fact + 8..fact + 12].try_into().unwrap()),
            257
        );
        assert_eq!(
            u16::from_le_bytes(bytes[fmt + 20..fmt + 22].try_into().unwrap()),
            256 * channels
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
fn wav_ima_adpcm_writer_checks_block_alignment_channel_limit() {
    let maximum_spec = AudioSpec::new(32_000, 255, SampleFormat::PcmI16).unwrap();
    let writer = WavWriter::new_ima_adpcm(Cursor::new(Vec::new()), maximum_spec).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.spec(), maximum_spec);
    assert!(decoder.read_frames(1).unwrap().is_none());

    let too_many_channels = AudioSpec::new(32_000, 256, SampleFormat::PcmI16).unwrap();
    assert!(WavWriter::new_ima_adpcm(Cursor::new(Vec::new()), too_many_channels).is_err());
}

#[test]
fn wav_ima_adpcm_extensible_preserves_speaker_mask() {
    let spec =
        AudioSpec::new_with_speaker_mask(48_000, 3, SampleFormat::PcmI16, Some(0x7)).unwrap();
    let mut writer = WavWriter::new_ima_adpcm(Cursor::new(Vec::new()), spec).unwrap();
    writer
        .write_block(&PcmBuffer::new(spec, Samples::I16(vec![0; 27])).unwrap())
        .unwrap();
    let bytes = writer.finish().unwrap().into_inner();

    let mut decoder = WavDecoder::new(Cursor::new(bytes)).unwrap();
    assert_eq!(decoder.spec(), spec);
    assert_eq!(decoder.read_frames(9).unwrap().unwrap().frames(), 9);
    assert!(decoder.read_frames(1).unwrap().is_none());
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
