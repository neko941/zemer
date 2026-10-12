use std::io::Cursor;

use wavpack::{WavPackBlockHeader, visit_metadata_sub_blocks};

#[test]
fn reads_block_and_sub_blocks_from_wavpack_5_9_output() {
    let file = include_bytes!("fixtures/reference-5.9.0-mono.wv");
    let mut input = Cursor::new(file.as_slice());
    let block = WavPackBlockHeader::read_from(&mut input).unwrap().unwrap();

    assert_eq!(block.block_index(), 0);
    assert_eq!(block.version, 0x410);
    assert_eq!(block.total_samples(), 16);
    assert_eq!(block.block_samples, 16);
    assert!(block.block_size <= file.len() as u64);

    let mut has_audio_bitstream = false;
    visit_metadata_sub_blocks(&mut input, block.payload_size(), |sub_block, _| {
        has_audio_bitstream |= sub_block.id == 0x0a && sub_block.data_size > 0;
        Ok(())
    })
    .unwrap();

    assert!(has_audio_bitstream, "missing WavPack audio bitstream");
    assert_eq!(input.position(), block.block_size);
}
