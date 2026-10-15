use std::io::Cursor;

use wav::{BextChunk, WavContainer, WavDecoder, WavWriter, rewrite_metadata};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples,
};

#[test]
fn broadcast_wave_chunk_round_trips_with_audio_and_info_tags() {
    let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![1, -2, 300])).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "BWF test");

    let mut bext = BextChunk::default();
    bext.description[..8].copy_from_slice(b"Transfer");
    bext.originator[..5].copy_from_slice(b"Zemer");
    bext.time_reference = 0x1_0000_0002;
    bext.version = 2;
    bext.reserved[..4].copy_from_slice(&[1, 2, 3, 4]);
    bext.coding_history = b"A=PCM,F=48000,W=16\r\nA".to_vec();

    for container in [WavContainer::Riff, WavContainer::Rf64] {
        let mut writer = WavWriter::new_with_metadata_and_bext_and_container(
            Cursor::new(Vec::new()),
            spec,
            &metadata,
            &bext,
            container,
        )
        .unwrap();
        writer.write_block(&audio).unwrap();
        let bytes = writer.finish().unwrap().into_inner();

        let mut decoder = WavDecoder::new(Cursor::new(bytes.clone())).unwrap();
        assert_eq!(decoder.bext(), Some(&bext));
        assert_eq!(decoder.metadata().values("TITLE"), ["BWF test"]);
        assert_eq!(decoder.read_frames(8).unwrap().unwrap(), audio);
        assert!(decoder.read_frames(8).unwrap().is_none());

        let mut edited_metadata = Metadata::default();
        edited_metadata.insert("TITLE", "Edited");
        let rewritten = rewrite_metadata(
            Cursor::new(bytes.clone()),
            Cursor::new(Vec::new()),
            &edited_metadata,
        )
        .unwrap()
        .into_inner();
        let edited = WavDecoder::new(Cursor::new(rewritten)).unwrap();
        assert_eq!(edited.bext(), Some(&bext));
        assert_eq!(edited.metadata().values("TITLE"), ["Edited"]);

        let mut short_bext = bytes;
        let bext_header = short_bext
            .windows(4)
            .position(|window| window == b"bext")
            .unwrap();
        short_bext[bext_header + 4..bext_header + 8].copy_from_slice(&601_u32.to_le_bytes());
        assert!(WavDecoder::new(Cursor::new(short_bext)).is_err());
    }
}
