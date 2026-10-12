use std::io::Cursor;

use wavpack::{WavPackBlockHeader, WavPackDecoder, visit_metadata_sub_blocks};
use zemer_core::{AudioDecoder, SampleFormat, Samples};

fn rechecksum_blocks(file: &mut [u8]) {
    let mut block_start = 0;
    while block_start < file.len() {
        let block_size = {
            let mut input = Cursor::new(&file[block_start..]);
            WavPackBlockHeader::read_from(&mut input)
                .unwrap()
                .unwrap()
                .block_size as usize
        };
        let block_end = block_start + block_size;
        let mut offset = block_start + 32;
        while offset < block_end {
            let id = file[offset];
            let large = id & 0x80 != 0;
            let header_size = if large { 4 } else { 2 };
            let words = if large {
                u32::from_le_bytes([file[offset + 1], file[offset + 2], file[offset + 3], 0])
            } else {
                u32::from(file[offset + 1])
            };
            let padded_size = usize::try_from(words * 2).unwrap();
            if id & 0x3f == 0x2f {
                let mut checksum = u32::MAX;
                for word in file[block_start..offset].chunks_exact(2) {
                    checksum = checksum
                        .wrapping_mul(3)
                        .wrapping_add(u32::from(u16::from_le_bytes([word[0], word[1]])));
                }
                let data_offset = offset + header_size;
                if padded_size == 4 {
                    file[data_offset..data_offset + 4].copy_from_slice(&checksum.to_le_bytes());
                } else {
                    let checksum = (checksum ^ (checksum >> 16)) as u16;
                    file[data_offset..data_offset + 2].copy_from_slice(&checksum.to_le_bytes());
                }
            }
            offset += header_size + padded_size;
        }
        block_start = block_end;
    }
}

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

#[test]
fn rejects_corrupted_wavpack_v5_block_checksum() {
    use wavpack::WavPackMetadataSubBlockHeader;

    let file = include_bytes!("fixtures/stereo16.wv");
    let mut input = Cursor::new(file.as_slice());
    let block = WavPackBlockHeader::read_from(&mut input).unwrap().unwrap();
    let mut remaining = block.payload_size();
    let mut checksum_offset = None;
    while let Some(sub_block) =
        WavPackMetadataSubBlockHeader::read_from(&mut input, &mut remaining).unwrap()
    {
        if sub_block.id == 0x2f {
            checksum_offset = Some(input.position() as usize);
            break;
        }
        input.set_position(input.position() + sub_block.padded_data_size);
    }
    let checksum_offset = checksum_offset.expect("WavPack v5 fixture contains a block checksum");

    for offset in [checksum_offset, 32 + 8] {
        let mut corrupted = file.to_vec();
        corrupted[offset] ^= 1;
        assert!(matches!(
            WavPackDecoder::new(Cursor::new(corrupted)),
            Err(zemer_core::AudioError::InvalidData(
                "WavPack block checksum mismatch"
            ))
        ));
    }

    let mut missing = file.to_vec();
    missing[checksum_offset - 2] = 0x2e;
    assert!(matches!(
        WavPackDecoder::new(Cursor::new(missing)),
        Err(zemer_core::AudioError::InvalidData(
            "WavPack block declares a missing checksum"
        ))
    ));
}

#[test]
fn decodes_reference_file_in_small_frame_batches() {
    let file = include_bytes!("fixtures/reference-5.9.0-mono.wv");
    let mut decoder = WavPackDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.spec().sample_rate(), 44_100);
    assert_eq!(decoder.spec().channels(), 1);

    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(3).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("WavPack decoder returned a non-integer sample buffer");
        };
        decoded.extend_from_slice(samples);
    }

    assert_eq!(
        decoded,
        [
            0, 1, -1, 32767, -32768, 1234, -1234, 42, -42, 0, 1000, -1000, 17, -17, 255, -255
        ]
    );
}

#[test]
fn decodes_unsigned_eight_bit_pcm() {
    let file = include_bytes!("fixtures/mono8.wv");
    let mut decoder = WavPackDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmU8);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(3).unwrap() {
        let Samples::U8(samples) = block.samples() else {
            panic!("WavPack decoder returned a non-8-bit sample buffer");
        };
        decoded.extend_from_slice(samples);
    }
    assert_eq!(decoded, [128, 129, 255, 0, 1, 126, 127, 0]);
}

#[test]
fn decodes_signed_twenty_four_bit_pcm() {
    let file = include_bytes!("fixtures/mono24.wv");
    let mut decoder = WavPackDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI24);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(3).unwrap() {
        let Samples::I24(samples) = block.samples() else {
            panic!("WavPack decoder returned a non-24-bit sample buffer");
        };
        decoded.extend_from_slice(samples);
    }
    assert_eq!(
        decoded,
        [-8_388_608, -1, 0, 1, 8_388_607, 123_456, -654_321, 42]
    );
}

#[test]
fn decodes_extended_signed_thirty_two_bit_pcm() {
    let file = include_bytes!("fixtures/mono32.wv");
    let mut decoder = WavPackDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI32);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(3).unwrap() {
        let Samples::I32(samples) = block.samples() else {
            panic!("WavPack decoder returned a non-32-bit sample buffer");
        };
        decoded.extend_from_slice(samples);
    }
    assert_eq!(
        decoded,
        [
            i32::MIN,
            i32::MIN + 1,
            -1,
            0,
            1,
            i32::MAX,
            123_456_789,
            -123_456_789,
            0x1234_5678,
            -0x1234_5678,
        ]
    );
}

#[test]
fn decodes_stereo_lossless_integer_pcm_at_all_sample_widths() {
    let left8 = [
        -128, -1, 0, 1, 127, 63, -64, 42, -42, 100, -100, 17, -17, 8, -8, 0,
    ];
    let right8 = [
        9, -9, 42, -42, 0, 100, -100, 64, -64, 1, -1, 127, -128, 7, -7, 0,
    ];
    let left16 = [
        0, 1, -1, 32767, -32768, 1234, -1234, 42, -42, 0, 1000, -1000, 17, -17, 255, -255,
    ];
    let right16 = [
        9, -9, 42, -42, 0, 100, -100, 200, -200, 1234, -1234, 1, -1, 16384, -16384, 777,
    ];
    let left24 = [
        -8_388_608, -1, 0, 1, 8_388_607, 123_456, -654_321, 42, -42, 4_000_000, -4_000_000, 17,
        -17, 255, -255, 0,
    ];
    let right24 = [
        9, -9, 42, -42, 0, 100, -100, 200, -200, 123_456, -123_456, 1, -1, 4_194_304, -4_194_304,
        777,
    ];
    let left32 = [
        i32::MIN,
        -1,
        0,
        1,
        i32::MAX,
        123_456_789,
        -123_456_789,
        42,
        -42,
        1_073_741_824,
        -1_073_741_824,
        17,
        -17,
        0x1234_5678,
        -0x1234_5678,
        0,
    ];
    let right32 = [
        9,
        -9,
        42,
        -42,
        0,
        100,
        -100,
        200,
        -200,
        123_456_789,
        -123_456_789,
        1,
        -1,
        1_073_741_823,
        -1_073_741_824,
        777,
    ];
    let cases = [
        (
            &include_bytes!("fixtures/stereo8.wv")[..],
            &left8,
            &right8,
            SampleFormat::PcmU8,
        ),
        (
            &include_bytes!("fixtures/stereo16.wv")[..],
            &left16,
            &right16,
            SampleFormat::PcmI16,
        ),
        (
            &include_bytes!("fixtures/stereo16-nojoint.wv")[..],
            &left16,
            &right16,
            SampleFormat::PcmI16,
        ),
        (
            &include_bytes!("fixtures/stereo24.wv")[..],
            &left24,
            &right24,
            SampleFormat::PcmI24,
        ),
        (
            &include_bytes!("fixtures/stereo32.wv")[..],
            &left32,
            &right32,
            SampleFormat::PcmI32,
        ),
    ];

    for (file, left, right, sample_format) in cases {
        let mut decoder = WavPackDecoder::new(Cursor::new(file)).unwrap();
        assert_eq!(decoder.spec().channels(), 2);
        assert_eq!(decoder.spec().sample_format(), sample_format);
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(3).unwrap() {
            match block.samples() {
                Samples::U8(samples) => {
                    decoded.extend(samples.iter().map(|sample| i32::from(*sample) - 128))
                }
                Samples::I16(samples) => {
                    decoded.extend(samples.iter().map(|sample| i32::from(*sample)))
                }
                Samples::I24(samples) | Samples::I32(samples) => decoded.extend_from_slice(samples),
                Samples::F32(_) | Samples::F64(_) => {
                    panic!("WavPack decoder returned float samples")
                }
            };
        }
        let expected = (0..128)
            .flat_map(|frame| [left[frame % 16], right[frame % 16]])
            .collect::<Vec<_>>();
        assert_eq!(
            decoded.len(),
            expected.len(),
            "sample count mismatch for {sample_format:?}"
        );
        if let Some((index, (actual, expected))) = decoded
            .iter()
            .zip(&expected)
            .enumerate()
            .find(|(_, (actual, expected))| actual != expected)
        {
            panic!(
                "{sample_format:?} mismatch at sample {index}: got {actual}, expected {expected}"
            );
        }
    }
}

#[test]
fn decodes_six_channel_lossless_pcm_with_speaker_mask() {
    let file = include_bytes!("fixtures/surround51.wv");
    let mut decoder = WavPackDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.spec().channels(), 6);
    assert_eq!(decoder.spec().speaker_mask(), Some(0x3f));
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);

    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(7).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("WavPack decoder returned a non-16-bit sample buffer");
        };
        decoded.extend_from_slice(samples);
    }
    let expected = (0..128)
        .flat_map(|frame| {
            (0..6).map(move |channel| (((frame * 37 + channel * 109) % 60_000) - 30_000) as i16)
        })
        .collect::<Vec<i16>>();
    assert_eq!(decoded, expected);
}

#[test]
fn decodes_eight_channel_lossless_pcm_with_speaker_mask() {
    let mut decoder = WavPackDecoder::new(Cursor::new(
        include_bytes!("fixtures/surround71.wv").as_slice(),
    ))
    .unwrap();
    assert_eq!(decoder.spec().channels(), 8);
    assert_eq!(decoder.spec().speaker_mask(), Some(0x63f));
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);

    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(997).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("WavPack decoder returned a non-16-bit sample buffer");
        };
        decoded.extend_from_slice(samples);
    }
    let expected = include_bytes!("fixtures/surround71.expected.pcm")
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    assert_eq!(decoded, expected);
}

#[test]
fn decodes_stereo_float32_losslessly_by_bit_pattern() {
    let file = include_bytes!("fixtures/float32.wv");
    let mut decoder = WavPackDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.spec().channels(), 2);
    assert_eq!(decoder.spec().sample_format(), SampleFormat::Float32);

    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(3).unwrap() {
        let Samples::F32(samples) = block.samples() else {
            panic!("WavPack decoder returned non-float samples");
        };
        decoded.extend(samples.iter().map(|sample| sample.to_bits()));
    }
    assert_eq!(
        decoded,
        [
            0x0000_0000,
            0x8000_0000,
            0x3f80_0000,
            0xbf80_0000,
            0x3f00_0000,
            0x4000_0000,
            0x0080_0000,
            0x7f7f_ffff,
            0x0000_0001,
            0x007f_ffff,
            0x7f80_0000,
            0xff80_0000,
            0x7fc1_2345,
            0xffc5_4321,
            0x7f81_2345,
            0x3eaa_aaab,
            0x3f12_3456,
            0xbf65_4321,
            0x0080_0001,
            0x8080_0001,
        ]
    );
}

#[test]
fn decodes_hybrid_lossy_pcm_against_official_wvunpack_output() {
    for (encoded, reference, channels) in [
        (
            &include_bytes!("fixtures/hybrid-mono.wv")[..],
            &include_bytes!("fixtures/hybrid-mono.expected.pcm")[..],
            1,
        ),
        (
            &include_bytes!("fixtures/hybrid-nojoint-unshaped.wv")[..],
            &include_bytes!("fixtures/hybrid-nojoint-unshaped.expected.pcm")[..],
            2,
        ),
        (
            &include_bytes!("fixtures/hybrid-joint-unshaped.wv")[..],
            &include_bytes!("fixtures/hybrid-joint-unshaped.expected.pcm")[..],
            2,
        ),
    ] {
        assert_eq!(reference.len() % 2, 0);
        let expected = reference
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();

        let mut decoder = WavPackDecoder::new(Cursor::new(encoded)).unwrap();
        assert_eq!(decoder.spec().channels(), channels);
        assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(257).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("WavPack hybrid decoder returned the wrong sample type");
            };
            decoded.extend_from_slice(samples);
        }
        if decoded != expected {
            let index = decoded
                .iter()
                .zip(&expected)
                .position(|(decoded, expected)| decoded != expected)
                .unwrap_or(decoded.len().min(expected.len()));
            panic!(
                "hybrid PCM differs at sample {index}: decoded {:?}, expected {:?}",
                &decoded[index.saturating_sub(4)..(index + 8).min(decoded.len())],
                &expected[index.saturating_sub(4)..(index + 8).min(expected.len())]
            );
        }
    }
}

#[test]
fn decodes_noise_shaped_hybrid_lossy_pcm_against_official_wvunpack_output() {
    let mut decoder = WavPackDecoder::new(Cursor::new(
        include_bytes!("fixtures/hybrid-shaped.wv").as_slice(),
    ))
    .unwrap();
    let reference = include_bytes!("fixtures/hybrid-shaped.expected.pcm");
    let expected = reference
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(257).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("noise-shaped WavPack decoder returned a non-16-bit sample buffer");
        };
        decoded.extend_from_slice(samples);
    }
    assert_eq!(decoded, expected);
}

#[test]
fn restores_noise_shaped_hybrid_lossless_pcm_against_official_wvunpack_output() {
    let mut decoder = WavPackDecoder::new_with_correction(
        Cursor::new(include_bytes!("fixtures/hybrid-shaped-lossless.wv").as_slice()),
        Cursor::new(include_bytes!("fixtures/hybrid-shaped-lossless.wvc").as_slice()),
    )
    .unwrap();
    assert_eq!(decoder.spec().channels(), 2);
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);

    let reference = include_bytes!("fixtures/hybrid-shaped-lossless.expected.pcm");
    let expected = reference
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(257).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("noise-shaped WavPack correction returned a non-16-bit sample buffer");
        };
        decoded.extend_from_slice(samples);
    }
    assert_eq!(decoded.len(), expected.len());
    for (index, (decoded, expected)) in decoded.iter().zip(&expected).enumerate() {
        assert_eq!(decoded, expected, "shaped hybrid sample differs at {index}");
    }
}

#[test]
fn rejects_hybrid_correction_with_cross_channel_decorrelation() {
    let mut audio = include_bytes!("fixtures/hybrid-shaped-lossless.wv").to_vec();
    let mut correction = include_bytes!("fixtures/hybrid-shaped-lossless.wvc").to_vec();
    for block in [&mut audio, &mut correction] {
        let flags = u32::from_le_bytes(block[24..28].try_into().unwrap()) | (1 << 5);
        block[24..28].copy_from_slice(&flags.to_le_bytes());
        rechecksum_blocks(block);
    }
    assert!(matches!(
        WavPackDecoder::new_with_correction(Cursor::new(audio), Cursor::new(correction)),
        Err(zemer_core::AudioError::Unsupported(
            "WavPack correction with cross-channel decorrelation is not supported yet"
        ))
    ));
}

#[test]
fn restores_hybrid_lossless_pcm_with_correction_file() {
    let correction = include_bytes!("fixtures/hybrid-lossless.wvc").to_vec();
    let mut correction_reader = Cursor::new(correction.as_slice());
    let mut decoder = WavPackDecoder::new_with_correction(
        Cursor::new(include_bytes!("fixtures/hybrid-lossless.wv").as_slice()),
        &mut correction_reader,
    )
    .unwrap();
    assert_eq!(decoder.spec().channels(), 2);
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);

    let reference = include_bytes!("fixtures/hybrid-lossless.expected.pcm");
    let expected = reference
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(257).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("WavPack corrected stream returned the wrong sample type");
        };
        decoded.extend_from_slice(samples);
    }
    assert_eq!(decoded, expected);
}

#[test]
fn rejects_truncated_hybrid_correction_file() {
    let correction = include_bytes!("fixtures/hybrid-lossless.wvc");
    assert!(
        WavPackDecoder::new_with_correction(
            Cursor::new(include_bytes!("fixtures/hybrid-lossless.wv").as_slice()),
            Cursor::new(&correction[..31]),
        )
        .is_err()
    );
}

#[test]
fn decodes_consecutive_six_channel_block_groups() {
    let file = include_bytes!("fixtures/surround51-twogroups.wv");
    let mut decoder = WavPackDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.spec().channels(), 6);
    assert_eq!(decoder.spec().speaker_mask(), Some(0x3f));

    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(997).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("WavPack decoder returned a non-16-bit sample buffer");
        };
        decoded.extend_from_slice(samples);
    }
    let expected = (0..11_026)
        .flat_map(|frame| {
            (0..6).map(move |channel| (((frame * 37 + channel * 109) % 60_000) - 30_000) as i16)
        })
        .collect::<Vec<i16>>();
    assert_eq!(decoded, expected);
}

#[test]
fn rejects_multichannel_block_group_without_final_block() {
    let file = include_bytes!("fixtures/surround51.wv");
    let mut corrupted = file.to_vec();
    let mut input = Cursor::new(file.as_slice());
    let mut final_flags_offset = None;
    while let Some(header) = WavPackBlockHeader::read_from(&mut input).unwrap() {
        let start = input.position() as usize - 32;
        if header.flags & (1 << 12) != 0 {
            final_flags_offset = Some(start + 24);
        }
        input.set_position((start as u64) + header.block_size);
    }
    let offset = final_flags_offset.expect("fixture has a final multichannel block");
    let flags = u32::from_le_bytes(corrupted[offset..offset + 4].try_into().unwrap());
    corrupted[offset..offset + 4].copy_from_slice(&(flags & !(1 << 12)).to_le_bytes());
    rechecksum_blocks(&mut corrupted);
    assert!(WavPackDecoder::new(Cursor::new(corrupted)).is_err());
}

#[test]
fn rejects_extended_integer_blocks_without_required_metadata() {
    use wavpack::WavPackMetadataSubBlockHeader;

    fn hide_metadata(file: &[u8], id: u8) -> Vec<u8> {
        let mut result = file.to_vec();
        let mut input = Cursor::new(file);
        let block = WavPackBlockHeader::read_from(&mut input).unwrap().unwrap();
        let mut remaining = block.payload_size();
        while remaining != 0 {
            let header_offset = input.position() as usize;
            let sub_block = WavPackMetadataSubBlockHeader::read_from(&mut input, &mut remaining)
                .unwrap()
                .unwrap();
            if sub_block.id == id {
                result[header_offset] = 0x2d;
            }
            input.set_position(input.position() + sub_block.padded_data_size);
        }
        rechecksum_blocks(&mut result);
        result
    }

    let file = include_bytes!("fixtures/mono32.wv");
    assert!(WavPackDecoder::new(Cursor::new(hide_metadata(file, 0x09))).is_err());
    assert!(WavPackDecoder::new(Cursor::new(hide_metadata(file, 0x0c))).is_err());
}
