use std::io::Cursor;

use wav::{
    BextChunk, WavContainer, WavDecoder, WavWriter, rewrite_metadata, rewrite_metadata_with_bext,
};
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

        let mut updated_bext = bext.clone();
        updated_bext.description = [0; 256];
        updated_bext.description[..10].copy_from_slice(b"Final mix ");
        updated_bext.coding_history = b"A=PCM\r\n".to_vec();
        let rewritten = rewrite_metadata_with_bext(
            Cursor::new(bytes.clone()),
            Cursor::new(Vec::new()),
            &metadata,
            &updated_bext,
        )
        .unwrap()
        .into_inner();
        let mut edited = WavDecoder::new(Cursor::new(rewritten)).unwrap();
        assert_eq!(edited.bext(), Some(&updated_bext));
        assert_eq!(edited.metadata().values("TITLE"), ["BWF test"]);
        assert_eq!(edited.read_frames(8).unwrap().unwrap(), audio);

        let mut short_bext = bytes;
        let bext_header = short_bext
            .windows(4)
            .position(|window| window == b"bext")
            .unwrap();
        short_bext[bext_header + 4..bext_header + 8].copy_from_slice(&601_u32.to_le_bytes());
        assert!(WavDecoder::new(Cursor::new(short_bext)).is_err());
    }
}

#[test]
fn broadcast_wave_rewriter_adds_bext_when_missing() {
    let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![4, -5])).unwrap();
    let mut writer = WavWriter::new(Cursor::new(Vec::new()), spec).unwrap();
    writer.write_block(&audio).unwrap();
    let original = writer.finish().unwrap().into_inner();
    let mut bext = BextChunk::default();
    bext.description[..6].copy_from_slice(b"Added ");

    let rewritten = rewrite_metadata_with_bext(
        Cursor::new(original),
        Cursor::new(Vec::new()),
        &Metadata::default(),
        &bext,
    )
    .unwrap()
    .into_inner();
    let mut decoder = WavDecoder::new(Cursor::new(rewritten)).unwrap();
    assert_eq!(decoder.bext(), Some(&bext));
    assert_eq!(decoder.read_frames(8).unwrap().unwrap(), audio);
}

#[test]
fn broadcast_wave_rewriter_updates_rf64_bext_table_entry() {
    let spec = AudioSpec::new(48_000, 1, SampleFormat::PcmI16).unwrap();
    let audio = PcmBuffer::new(spec, Samples::I16(vec![8, -9])).unwrap();
    let original_bext = BextChunk {
        coding_history: b"A=PCM\r\n".to_vec(),
        ..BextChunk::default()
    };
    let mut writer = WavWriter::new_with_metadata_and_bext_and_container(
        Cursor::new(Vec::new()),
        spec,
        &Metadata::default(),
        &original_bext,
        WavContainer::Rf64,
    )
    .unwrap();
    writer.write_block(&audio).unwrap();
    let mut original = writer.finish().unwrap().into_inner();

    let ds64_size = u32::from_le_bytes(original[16..20].try_into().unwrap());
    assert_eq!(ds64_size, 28);
    let ds64_end = 20 + ds64_size as usize;
    let bext_header = original.windows(4).rposition(|id| id == b"bext").unwrap();
    let old_bext_size = u32::from_le_bytes(
        original[bext_header + 4..bext_header + 8]
            .try_into()
            .unwrap(),
    );
    let old_riff_size = u64::from_le_bytes(original[20..28].try_into().unwrap());
    let mut table_entry = Vec::from(*b"bext");
    table_entry.extend_from_slice(&u64::from(old_bext_size).to_le_bytes());
    original.splice(ds64_end..ds64_end, table_entry);
    original[16..20].copy_from_slice(&40_u32.to_le_bytes());
    original[44..48].copy_from_slice(&1_u32.to_le_bytes());
    original[20..28].copy_from_slice(&(old_riff_size + 12).to_le_bytes());
    let bext_header = original.windows(4).rposition(|id| id == b"bext").unwrap();
    original[bext_header + 4..bext_header + 8].copy_from_slice(&u32::MAX.to_le_bytes());

    let updated_bext = BextChunk {
        coding_history: b"A=PCM,F=96000,W=24\r\nX".to_vec(),
        ..BextChunk::default()
    };
    let updated_size = 602 + u32::try_from(updated_bext.coding_history.len()).unwrap();
    let rewritten = rewrite_metadata_with_bext(
        Cursor::new(original),
        Cursor::new(Vec::new()),
        &Metadata::default(),
        &updated_bext,
    )
    .unwrap()
    .into_inner();
    let bext_header = rewritten.windows(4).rposition(|id| id == b"bext").unwrap();
    assert_eq!(
        &rewritten[bext_header + 4..bext_header + 8],
        &u32::MAX.to_le_bytes()
    );
    assert_eq!(
        u64::from_le_bytes(rewritten[ds64_end + 4..ds64_end + 12].try_into().unwrap()),
        u64::from(updated_size)
    );
    let mut decoder = WavDecoder::new(Cursor::new(rewritten)).unwrap();
    assert_eq!(decoder.bext(), Some(&updated_bext));
    assert_eq!(decoder.read_frames(8).unwrap().unwrap(), audio);
}
