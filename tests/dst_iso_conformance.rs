use std::{
    fs,
    path::{Path, PathBuf},
};

use dst::decode_frame;

const FRAMES_TO_CHECK: usize = 10;

#[test]
#[ignore = "requires official ISO DST bitstreams and reference MP4s in ZEMER_DST_*_DIR"]
fn decodes_official_dst_vectors_bit_exactly() {
    check_official_vectors(FRAMES_TO_CHECK);
}

#[test]
#[ignore = "runs full ISO DST sequences; requires official media in ZEMER_DST_*_DIR"]
fn decodes_complete_official_dst_vectors_bit_exactly() {
    check_official_vectors(usize::MAX);
}

fn check_official_vectors(frame_limit: usize) {
    let compressed = PathBuf::from(
        std::env::var_os("ZEMER_DST_COMPRESSED_DIR")
            .expect("set ZEMER_DST_COMPRESSED_DIR to the ISO compressedMp4 directory"),
    );
    let reference = PathBuf::from(
        std::env::var_os("ZEMER_DST_REFERENCE_DIR")
            .expect("set ZEMER_DST_REFERENCE_DIR to the ISO referencesMp4 directory"),
    );
    let mut vectors = Vec::new();
    for rate in ["064", "128", "256"] {
        let frame_bytes = match rate {
            "064" => 4_704,
            "128" => 9_408,
            "256" => 18_816,
            _ => unreachable!(),
        };
        for channels in 1..=6 {
            vectors.push((
                format!("DST_00_{channels:02}_{rate}.mp4"),
                channels,
                frame_bytes,
            ));
        }
        for family in [1, 2] {
            for channels in [2, 5, 6] {
                vectors.push((
                    format!("DST_{family:02}_{channels:02}_{rate}.mp4"),
                    channels,
                    frame_bytes,
                ));
            }
        }
    }
    for family in [3, 4, 6] {
        for channels in [2, 5, 6] {
            vectors.push((
                format!("DST_{family:02}_{channels:02}_064.mp4"),
                channels,
                4_704,
            ));
        }
    }
    vectors.push(("DST_00_64_064.mp4".into(), 64, 4_704));
    if let Some(name) = std::env::var_os("ZEMER_DST_VECTOR") {
        let name = name.to_string_lossy();
        vectors.retain(|(vector, _, _)| vector == name.as_ref());
        assert!(!vectors.is_empty(), "unknown ISO DST vector: {name}");
    }

    for (name, channels, frame_bytes) in vectors {
        let coded = mp4_samples(&compressed.join(&name), frame_limit);
        let expected = mp4_samples(&reference.join(&name), frame_limit);
        assert!(
            coded.len() >= FRAMES_TO_CHECK,
            "enough source frames for {name}"
        );
        assert_eq!(coded.len(), expected.len(), "sample count for {name}");
        for (index, (input, expected)) in coded.iter().zip(expected).enumerate() {
            let planar = decode_frame(input, channels, frame_bytes)
                .unwrap_or_else(|error| panic!("{name} frame {index}: {error}"));
            let mut interleaved = Vec::with_capacity(planar.len());
            for byte in 0..frame_bytes {
                for channel in 0..channels {
                    interleaved.push(planar[channel * frame_bytes + byte]);
                }
            }
            assert_eq!(
                interleaved, expected,
                "bit-exact reference for {name} frame {index}"
            );
        }
    }
}

fn mp4_samples(path: &Path, limit: usize) -> Vec<Vec<u8>> {
    let bytes = fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let stsz = find_box(&bytes, 0, bytes.len(), *b"stsz").expect("MP4 stsz box");
    let sample_size = be_u32(&bytes[stsz + 4..stsz + 8]) as usize;
    let sample_count = be_u32(&bytes[stsz + 8..stsz + 12]) as usize;
    let sizes = if sample_size == 0 {
        (0..sample_count)
            .map(|index| {
                let start = stsz + 12 + index * 4;
                be_u32(&bytes[start..start + 4]) as usize
            })
            .collect::<Vec<_>>()
    } else {
        vec![sample_size; sample_count]
    };

    let (offset_box, wide_offsets) = find_box(&bytes, 0, bytes.len(), *b"stco")
        .map(|offset| (offset, false))
        .or_else(|| find_box(&bytes, 0, bytes.len(), *b"co64").map(|offset| (offset, true)))
        .expect("MP4 stco/co64 box");
    let chunk_count = be_u32(&bytes[offset_box + 4..offset_box + 8]) as usize;
    assert_eq!(
        chunk_count,
        sample_count,
        "one sample per chunk in {}",
        path.display()
    );
    let offsets = (0..chunk_count)
        .map(|index| {
            let width = if wide_offsets { 8 } else { 4 };
            let start = offset_box + 8 + index * width;
            if wide_offsets {
                usize::try_from(u64::from_be_bytes(
                    bytes[start..start + 8].try_into().unwrap(),
                ))
                .unwrap()
            } else {
                be_u32(&bytes[start..start + 4]) as usize
            }
        })
        .collect::<Vec<_>>();

    offsets
        .into_iter()
        .zip(sizes)
        .take(limit)
        .map(|(offset, size)| bytes[offset..offset + size].to_vec())
        .collect()
}

fn find_box(bytes: &[u8], start: usize, end: usize, target: [u8; 4]) -> Option<usize> {
    let mut position = start;
    while position.checked_add(8)? <= end {
        let size = be_u32(bytes.get(position..position + 4)?) as usize;
        let kind: [u8; 4] = bytes.get(position + 4..position + 8)?.try_into().ok()?;
        let (header, size) = if size == 1 {
            (
                16,
                usize::try_from(u64::from_be_bytes(
                    bytes.get(position + 8..position + 16)?.try_into().ok()?,
                ))
                .ok()?,
            )
        } else if size == 0 {
            (8, end - position)
        } else {
            (8, size)
        };
        let box_end = position.checked_add(size)?;
        if size < header || box_end > end {
            return None;
        }
        if kind == target {
            return Some(position + header);
        }
        if matches!(
            kind,
            [b'm', b'o', b'o', b'v']
                | [b't', b'r', b'a', b'k']
                | [b'm', b'd', b'i', b'a']
                | [b'm', b'i', b'n', b'f']
                | [b's', b't', b'b', b'l']
        ) {
            if let Some(found) = find_box(bytes, position + header, box_end, target) {
                return Some(found);
            }
        }
        position = box_end;
    }
    None
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap())
}
