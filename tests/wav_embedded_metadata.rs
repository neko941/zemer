use std::io::Cursor;

use wav::{
    WavContainer, WavCueChunk, WavCuePoint, WavDecoder, WavSampleLoop, WavSamplerChunk,
    WavWriter, rewrite_metadata,
};
use zemer_core::{AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples};

#[test]
fn cue_and_sampler_chunks_round_trip_for_riff_and_rf64() {
    let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![1, -2, 300])).unwrap();
    let cue = WavCueChunk {
        points: vec![WavCuePoint {
            name: 7,
            position: 2,
            chunk_id: *b"data",
            chunk_start: 0,
            block_start: 0,
            sample_offset: 2,
        }],
    };
    let sampler = WavSamplerChunk {
        manufacturer: 1,
        product: 2,
        sample_period: 20_833,
        midi_unity_note: 60,
        midi_pitch_fraction: 0,
        smpte_format: 0,
        smpte_offset: 0,
        loops: vec![WavSampleLoop {
            identifier: 3,
            loop_type: 1,
            start: 0,
            end: 2,
            fraction: 0,
            play_count: 0,
        }],
        sampler_data: vec![9, 8, 7],
    };

    for container in [WavContainer::Riff, WavContainer::Rf64] {
        let mut writer = WavWriter::new_with_container(Cursor::new(Vec::new()), spec, container)
            .unwrap();
        writer.set_cue(Some(cue.clone())).unwrap();
        writer.set_sampler(Some(sampler.clone())).unwrap();
        writer.write_block(&audio).unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        let mut decoder = WavDecoder::new(Cursor::new(bytes.clone())).unwrap();
        assert_eq!(decoder.cue(), Some(&cue));
        assert_eq!(decoder.sampler(), Some(&sampler));
        assert_eq!(decoder.read_frames(8).unwrap().unwrap(), audio);
        assert!(decoder.read_frames(8).unwrap().is_none());

        let rewritten = rewrite_metadata(
            Cursor::new(bytes),
            Cursor::new(Vec::new()),
            &Metadata::default(),
        )
        .unwrap()
        .into_inner();
        let rewritten = WavDecoder::new(Cursor::new(rewritten)).unwrap();
        assert_eq!(rewritten.cue(), Some(&cue));
        assert_eq!(rewritten.sampler(), Some(&sampler));
    }
}

#[test]
fn malformed_embedded_chunk_counts_are_rejected() {
    let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![0])).unwrap();
    let cue = WavCueChunk {
        points: vec![WavCuePoint {
            name: 1,
            position: 0,
            chunk_id: *b"data",
            chunk_start: 0,
            block_start: 0,
            sample_offset: 0,
        }],
    };
    let mut writer = WavWriter::new(Cursor::new(Vec::new()), spec).unwrap();
    writer.set_cue(Some(cue)).unwrap();
    writer.write_block(&audio).unwrap();
    let mut bytes = writer.finish().unwrap().into_inner();

    let cue = bytes.windows(4).position(|part| part == b"cue ").unwrap();
    bytes[cue + 8..cue + 12].copy_from_slice(&2_u32.to_le_bytes());
    assert!(WavDecoder::new(Cursor::new(bytes.clone())).is_err());

    let sampler = WavSamplerChunk::default();
    let mut writer = WavWriter::new(Cursor::new(Vec::new()), spec).unwrap();
    writer.set_sampler(Some(sampler)).unwrap();
    writer.write_block(&audio).unwrap();
    let mut bytes = writer.finish().unwrap().into_inner();
    let sampler = bytes.windows(4).position(|part| part == b"smpl").unwrap();
    bytes[sampler + 36..sampler + 40].copy_from_slice(&1_u32.to_le_bytes());
    assert!(WavDecoder::new(Cursor::new(bytes)).is_err());
}
