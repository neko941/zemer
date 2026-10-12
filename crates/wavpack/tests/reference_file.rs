use std::io::Cursor;

use wavpack::{WavPackBlockHeader, WavPackDecoder, WavPackDsdDecoder, visit_metadata_sub_blocks};
use zemer_core::{AudioDecoder, DsdDecoder, SampleFormat, Samples};

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

fn first_metadata_data_offset(file: &[u8], wanted_id: u8) -> usize {
    let mut input = Cursor::new(file);
    let block = WavPackBlockHeader::read_from(&mut input).unwrap().unwrap();
    let mut remaining = block.payload_size();
    while let Some(sub_block) =
        wavpack::WavPackMetadataSubBlockHeader::read_from(&mut input, &mut remaining).unwrap()
    {
        let data_offset = input.position() as usize;
        if sub_block.id == wanted_id {
            return data_offset;
        }
        input.set_position(input.position() + sub_block.padded_data_size);
    }
    panic!("WavPack fixture lacks metadata block {wanted_id:#x}");
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
fn rejects_corrupted_wavpack_decoded_sample_checksum() {
    let mut corrupted = include_bytes!("fixtures/stereo16.wv").to_vec();
    corrupted[28] ^= 1;
    rechecksum_blocks(&mut corrupted);

    assert!(matches!(
        WavPackDecoder::new(Cursor::new(corrupted)),
        Err(zemer_core::AudioError::InvalidData(
            "WavPack decoded-sample checksum mismatch"
        ))
    ));
}

#[test]
fn rejects_corrupted_wvx_extended_checksum() {
    for file in [
        &include_bytes!("fixtures/mono32.wv")[..],
        &include_bytes!("fixtures/float32.wv")[..],
    ] {
        let mut corrupted = file.to_vec();
        let offset = first_metadata_data_offset(&corrupted, 0x0c);
        corrupted[offset] ^= 1;
        rechecksum_blocks(&mut corrupted);
        assert!(matches!(
            WavPackDecoder::new(Cursor::new(corrupted)),
            Err(zemer_core::AudioError::InvalidData(
                "WavPack WVX checksum mismatch"
            ))
        ));
    }
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
fn decodes_custom_sample_rate_from_metadata() {
    let expected = include_bytes!("fixtures/custom-rate-stereo.expected.pcm")
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    for file in [
        &include_bytes!("fixtures/custom-rate-stereo.wv")[..],
        &include_bytes!("fixtures/tagged-custom-rate-stereo.wv")[..],
    ] {
        let mut decoder = WavPackDecoder::new(Cursor::new(file)).unwrap();
        assert_eq!(decoder.spec().sample_rate(), 12_345);
        assert_eq!(decoder.spec().channels(), 2);
        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(29).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("WavPack decoder returned non-16-bit PCM");
            };
            decoded.extend_from_slice(samples);
        }
        assert_eq!(decoded, expected);
    }
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
fn expands_false_stereo_mono_payloads_to_two_output_channels() {
    let file = include_bytes!("fixtures/false-stereo.wv");
    let header = WavPackBlockHeader::read_from(&mut Cursor::new(file.as_slice()))
        .unwrap()
        .unwrap();
    assert_ne!(header.flags & (1 << 30), 0);

    let mut decoder = WavPackDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.spec().channels(), 2);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(37).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("WavPack decoder returned non-16-bit PCM");
        };
        decoded.extend_from_slice(samples);
    }
    let expected = include_bytes!("fixtures/false-stereo.expected.pcm")
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    assert_eq!(decoded, expected);
    assert!(decoded.chunks_exact(2).all(|channels| channels[0] == channels[1]));
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
fn decodes_other_multichannel_block_group_sizes() {
    let cases: [(u16, u32, &[u8], &[u8]); 4] = [
        (
            3,
            0x007,
            include_bytes!("fixtures/channels-3.wv"),
            include_bytes!("fixtures/channels-3.expected.pcm"),
        ),
        (
            4,
            0x033,
            include_bytes!("fixtures/channels-4.wv"),
            include_bytes!("fixtures/channels-4.expected.pcm"),
        ),
        (
            5,
            0x037,
            include_bytes!("fixtures/channels-5.wv"),
            include_bytes!("fixtures/channels-5.expected.pcm"),
        ),
        (
            7,
            0x70f,
            include_bytes!("fixtures/channels-7.wv"),
            include_bytes!("fixtures/channels-7.expected.pcm"),
        ),
    ];
    for (channels, speaker_mask, file, expected_bytes) in cases {
        let mut decoder = WavPackDecoder::new(Cursor::new(file)).unwrap();
        assert_eq!(decoder.spec().channels(), channels);
        assert_eq!(decoder.spec().speaker_mask(), Some(speaker_mask));
        assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);

        let mut decoded = Vec::new();
        while let Some(block) = decoder.read_frames(31).unwrap() {
            let Samples::I16(samples) = block.samples() else {
                panic!("WavPack decoder returned a non-16-bit sample buffer");
            };
            decoded.extend_from_slice(samples);
        }
        let expected = expected_bytes
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        assert_eq!(decoded, expected, "{channels}-channel output differs");
    }
}

#[test]
fn decodes_six_channel_lossless_pcm24_and_pcm32() {
    let mut pcm24 = WavPackDecoder::new(Cursor::new(
        include_bytes!("fixtures/surround51-24.wv").as_slice(),
    ))
    .unwrap();
    assert_eq!(pcm24.spec().sample_format(), SampleFormat::PcmI24);
    assert_eq!(pcm24.spec().channels(), 6);
    assert_eq!(pcm24.spec().speaker_mask(), Some(0x003f));
    let mut decoded24 = Vec::new();
    while let Some(block) = pcm24.read_frames(13).unwrap() {
        let Samples::I24(samples) = block.samples() else {
            panic!("WavPack decoder returned non-24-bit samples");
        };
        decoded24.extend_from_slice(samples);
    }
    let expected24 = include_bytes!("fixtures/surround51-24.expected.pcm")
        .chunks_exact(3)
        .map(|bytes| {
            let value =
                i32::from(bytes[0]) | (i32::from(bytes[1]) << 8) | (i32::from(bytes[2]) << 16);
            if value & 0x0080_0000 != 0 {
                value | !0x00ff_ffff
            } else {
                value
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(decoded24, expected24);

    let mut pcm32 = WavPackDecoder::new(Cursor::new(
        include_bytes!("fixtures/surround51-32.wv").as_slice(),
    ))
    .unwrap();
    assert_eq!(pcm32.spec().sample_format(), SampleFormat::PcmI32);
    assert_eq!(pcm32.spec().channels(), 6);
    assert_eq!(pcm32.spec().speaker_mask(), Some(0x003f));
    let mut decoded32 = Vec::new();
    while let Some(block) = pcm32.read_frames(13).unwrap() {
        let Samples::I32(samples) = block.samples() else {
            panic!("WavPack decoder returned non-32-bit samples");
        };
        decoded32.extend_from_slice(samples);
    }
    let expected32 = include_bytes!("fixtures/surround51-32.expected.pcm")
        .chunks_exact(4)
        .map(|bytes| i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect::<Vec<_>>();
    assert_eq!(decoded32, expected32);
}

#[test]
fn restores_multichannel_hybrid_pcm_with_correction_file() {
    let mut decoder = WavPackDecoder::new_with_correction(
        Cursor::new(include_bytes!("fixtures/channels-3-hybrid.wv").as_slice()),
        Cursor::new(include_bytes!("fixtures/channels-3-hybrid.wvc").as_slice()),
    )
    .unwrap();
    assert_eq!(decoder.spec().channels(), 3);
    assert_eq!(decoder.spec().speaker_mask(), Some(0x007));

    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(31).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("WavPack correction decoder returned a non-16-bit buffer");
        };
        decoded.extend_from_slice(samples);
    }
    let expected = include_bytes!("fixtures/channels-3-hybrid.expected.pcm")
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    assert_eq!(decoded, expected);
}

#[test]
fn restores_hybrid_lossless_pcm24_and_pcm32_with_correction_files() {
    let mut decoder = WavPackDecoder::new_with_correction(
        Cursor::new(include_bytes!("fixtures/hybrid-24-stereo.wv").as_slice()),
        Cursor::new(include_bytes!("fixtures/hybrid-24-stereo.wvc").as_slice()),
    )
    .unwrap();
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI24);
    let mut decoded24 = Vec::new();
    while let Some(block) = decoder.read_frames(31).unwrap() {
        let Samples::I24(samples) = block.samples() else {
            panic!("WavPack decoder returned a non-24-bit buffer");
        };
        decoded24.extend_from_slice(samples);
    }
    let expected24 = include_bytes!("fixtures/hybrid-24-stereo.expected.pcm")
        .chunks_exact(3)
        .map(|bytes| {
            let value =
                i32::from(bytes[0]) | (i32::from(bytes[1]) << 8) | (i32::from(bytes[2]) << 16);
            if value & 0x0080_0000 != 0 {
                value | !0x00ff_ffff
            } else {
                value
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(decoded24, expected24);

    let mut decoder = WavPackDecoder::new_with_correction(
        Cursor::new(include_bytes!("fixtures/hybrid-32-stereo.wv").as_slice()),
        Cursor::new(include_bytes!("fixtures/hybrid-32-stereo.wvc").as_slice()),
    )
    .unwrap();
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI32);
    let mut decoded32 = Vec::new();
    while let Some(block) = decoder.read_frames(31).unwrap() {
        let Samples::I32(samples) = block.samples() else {
            panic!("WavPack decoder returned a non-32-bit buffer");
        };
        decoded32.extend_from_slice(samples);
    }
    let expected32 = include_bytes!("fixtures/hybrid-32-stereo.expected.pcm")
        .chunks_exact(4)
        .map(|bytes| i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect::<Vec<_>>();
    assert_eq!(decoded32, expected32);
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
fn decodes_hybrid_float_with_and_without_correction_file() {
    let lossy_file = include_bytes!("fixtures/hybrid-float-stereo.wv");
    let mut lossy = WavPackDecoder::new(Cursor::new(lossy_file.as_slice())).unwrap();
    assert_eq!(lossy.spec().sample_format(), SampleFormat::Float32);
    let mut lossy_samples = Vec::new();
    while let Some(block) = lossy.read_frames(37).unwrap() {
        let Samples::F32(samples) = block.samples() else {
            panic!("WavPack decoder returned non-float samples");
        };
        lossy_samples.extend_from_slice(samples);
    }
    assert_eq!(lossy_samples.len(), 1024);
    assert!(lossy_samples.iter().all(|sample| sample.is_finite()));

    let mut lossless = WavPackDecoder::new_with_correction(
        Cursor::new(lossy_file.as_slice()),
        Cursor::new(include_bytes!("fixtures/hybrid-float-stereo.wvc").as_slice()),
    )
    .unwrap();
    let mut restored_bits = Vec::new();
    while let Some(block) = lossless.read_frames(37).unwrap() {
        let Samples::F32(samples) = block.samples() else {
            panic!("WavPack correction decoder returned non-float samples");
        };
        restored_bits.extend(samples.iter().map(|sample| sample.to_bits()));
    }
    let expected_bits = include_bytes!("fixtures/hybrid-float-stereo.expected.f32le")
        .chunks_exact(4)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect::<Vec<_>>();
    assert_eq!(restored_bits, expected_bits);
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
fn restores_cross_channel_shaped_hybrid_lossless_pcm() {
    let audio = include_bytes!("fixtures/hybrid-cross-channel-lossless.wv");
    let correction = include_bytes!("fixtures/hybrid-cross-channel-lossless.wvc");
    let flags = u32::from_le_bytes(audio[24..28].try_into().unwrap());
    assert_ne!(
        flags & (1 << 5),
        0,
        "fixture must use cross-channel decorrelation"
    );
    assert_ne!(flags & (1 << 6), 0, "fixture must use noise shaping");

    let mut decoder = WavPackDecoder::new_with_correction(
        Cursor::new(audio.as_slice()),
        Cursor::new(correction.as_slice()),
    )
    .unwrap();
    assert_eq!(decoder.spec().channels(), 2);
    assert_eq!(decoder.spec().sample_format(), SampleFormat::PcmI16);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_frames(257).unwrap() {
        let Samples::I16(samples) = block.samples() else {
            panic!("cross-channel WavPack decoder returned the wrong sample type");
        };
        decoded.extend_from_slice(samples);
    }
    let expected = include_bytes!("fixtures/hybrid-cross-channel-lossless.expected.pcm")
        .chunks_exact(2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    assert_eq!(decoded, expected);
}

#[test]
fn decodes_raw_native_dsd_octets_and_verifies_dsd_checksum() {
    let file = include_bytes!("fixtures/dsd-direct-stereo.wv");
    let mut decoder = WavPackDsdDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.dsd_spec().bit_rate(), 2_822_400);
    assert_eq!(decoder.dsd_spec().channels(), 2);

    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_dsd_octets(257).unwrap() {
        decoded.extend_from_slice(block.bytes());
    }
    assert_eq!(
        decoded,
        include_bytes!("fixtures/dsd-direct-stereo.expected.dsd")
    );
    assert!(WavPackDecoder::new(Cursor::new(file.as_slice())).is_err());

    let mut corrupted = file.to_vec();
    corrupted[28] ^= 1;
    rechecksum_blocks(&mut corrupted);
    assert!(matches!(
        WavPackDsdDecoder::new(Cursor::new(corrupted)),
        Err(zemer_core::AudioError::InvalidData(
            "WavPack DSD checksum mismatch"
        ))
    ));
}

#[test]
fn decodes_fast_native_dsd_octets_from_official_encoder() {
    let file = include_bytes!("fixtures/dsd-fast-stereo.wv");
    let mut decoder = WavPackDsdDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.dsd_spec().bit_rate(), 2_822_400);
    assert_eq!(decoder.dsd_spec().channels(), 2);

    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_dsd_octets(257).unwrap() {
        decoded.extend_from_slice(block.bytes());
    }
    assert_eq!(
        decoded,
        include_bytes!("fixtures/dsd-fast-stereo.expected.dsd")
    );
}

#[test]
fn decodes_adaptive_native_dsd_octets_from_official_encoder() {
    let file = include_bytes!("fixtures/dsd-adaptive-stereo.wv");
    let mut decoder = WavPackDsdDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.dsd_spec().bit_rate(), 2_822_400);
    assert_eq!(decoder.dsd_spec().channels(), 2);

    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_dsd_octets(257).unwrap() {
        decoded.extend_from_slice(block.bytes());
    }
    assert_eq!(
        decoded,
        include_bytes!("fixtures/dsd-adaptive-stereo.expected.dsd")
    );
}

#[test]
fn decodes_six_channel_adaptive_native_dsd() {
    let file = include_bytes!("fixtures/dsd-adaptive-51.wv");
    let mut decoder = WavPackDsdDecoder::new(Cursor::new(file.as_slice())).unwrap();
    assert_eq!(decoder.dsd_spec().bit_rate(), 2_822_400);
    assert_eq!(decoder.dsd_spec().channels(), 6);

    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_dsd_octets(127).unwrap() {
        decoded.extend_from_slice(block.bytes());
    }
    assert_eq!(
        decoded,
        include_bytes!("fixtures/dsd-adaptive-51.expected.dsd")
    );
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
