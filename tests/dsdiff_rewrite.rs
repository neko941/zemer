use std::io::Cursor;

use dsdiff::{DsdiffDecoder, DsdiffEncoder, DsdiffMarker, rewrite_metadata};
use zemer_core::{DsdBuffer, DsdDecoder, DsdEncoder, DsdSpec, Metadata};

#[test]
fn rewrites_text_and_preserves_audio_unknown_chunks_and_typed_metadata() {
    let spec = DsdSpec::new(2_822_400, 2).unwrap();
    let audio = DsdBuffer::new(spec, vec![0x96, 0x69, 0x55, 0xaa, 0x0f, 0xf0]).unwrap();
    let marker = DsdiffMarker {
        hours: 0,
        minutes: 0,
        seconds: 0,
        samples: 12,
        offset: -3,
        kind: 0,
        channel: 0,
        track_flags: 0,
        text: "Keep marker".into(),
    };
    let mut metadata = Metadata::default();
    metadata.insert("TITLE", "Before");
    metadata.insert("ARTIST", "Artist");
    let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder.set_metadata(&metadata).unwrap();
    let comments = [
        dsdiff::DsdiffComment {
            year: 2025,
            month: 1,
            day: 2,
            hour: 3,
            minute: 4,
            kind: 1,
            reference: 1,
            text: "Typed".into(),
        },
        dsdiff::DsdiffComment {
            year: 2024,
            month: 5,
            day: 6,
            hour: 7,
            minute: 8,
            kind: 0,
            reference: 0,
            text: "Comment".into(),
        },
    ];
    encoder.set_comments(&comments).unwrap();
    encoder.set_markers(std::slice::from_ref(&marker)).unwrap();
    encoder.write_dsd_octets(&audio).unwrap();
    encoder.finish().unwrap();
    let mut source = encoder.into_inner().into_inner();

    let unknown_chunk = chunk(*b"JUNK", b"unknown payload");
    source.extend_from_slice(&unknown_chunk);
    let form_size = u64::from_be_bytes(source[4..12].try_into().unwrap());
    source[4..12].copy_from_slice(&(form_size + unknown_chunk.len() as u64).to_be_bytes());

    let original_audio = top_level_chunk(&source, *b"DSD ");
    let mut edited = DsdiffDecoder::new(Cursor::new(&source))
        .unwrap()
        .metadata()
        .clone();
    edited.remove("TITLE");
    edited.insert("TITLE", "After");
    edited.remove("COMMENT");
    edited.insert("COMMENT", "Updated comment");
    let mut output = Cursor::new(Vec::new());
    rewrite_metadata(Cursor::new(&source), &mut output, &edited).unwrap();
    let output = output.into_inner();

    assert_eq!(top_level_chunk(&output, *b"DSD "), original_audio);
    assert!(
        output
            .windows(unknown_chunk.len())
            .any(|w| w == unknown_chunk)
    );
    let mut decoder = DsdiffDecoder::new(Cursor::new(output)).unwrap();
    assert_eq!(decoder.metadata().values("TITLE"), ["After"]);
    assert_eq!(decoder.metadata().values("ARTIST"), ["Artist"]);
    assert_eq!(decoder.comments()[0], comments[0]);
    assert_eq!(decoder.comments()[1].year, 2024);
    assert_eq!(decoder.comments()[1].text, "Updated comment");
    assert_eq!(decoder.markers(), [marker]);
    let mut decoded = Vec::new();
    while let Some(block) = decoder.read_dsd_octets(1).unwrap() {
        decoded.extend_from_slice(block.bytes());
    }
    assert_eq!(decoded, audio.bytes());
}

#[test]
fn rejects_unimplemented_channel_layout_edits_before_writing() {
    let spec = DsdSpec::new(2_822_400, 1).unwrap();
    let mut encoder = DsdiffEncoder::new(Cursor::new(Vec::new()), spec).unwrap();
    encoder
        .write_dsd_octets(&DsdBuffer::new(spec, vec![0x55]).unwrap())
        .unwrap();
    encoder.finish().unwrap();
    let source = encoder.into_inner().into_inner();
    let mut metadata = Metadata::default();
    metadata.insert("DSDIFF_CHANNEL_ID", "C123");
    let mut output = Cursor::new(Vec::new());
    assert!(rewrite_metadata(Cursor::new(source), &mut output, &metadata).is_err());
    assert!(output.get_ref().is_empty());
}

#[test]
fn decodes_markers_from_a_dsdiff_15_layout_fixture() {
    let source = dsdiff_15_marker_fixture();
    let mut decoder = DsdiffDecoder::new(Cursor::new(source)).unwrap();
    assert_eq!(
        decoder.markers(),
        [DsdiffMarker {
            hours: 1,
            minutes: 2,
            seconds: 3,
            samples: 123,
            offset: -17,
            kind: 4,
            channel: 2,
            track_flags: 0,
            text: "Index 2".into(),
        }]
    );
    assert_eq!(
        decoder.read_dsd_octets(1).unwrap().unwrap().bytes(),
        [0x96, 0x69]
    );
    assert!(decoder.read_dsd_octets(1).unwrap().is_none());
}

fn dsdiff_15_marker_fixture() -> Vec<u8> {
    let mut sound_properties = b"SND ".to_vec();
    sound_properties.extend(chunk(*b"FS  ", &2_822_400_u32.to_be_bytes()));
    let mut channels = 2_u16.to_be_bytes().to_vec();
    channels.extend_from_slice(b"SLFTSRGT");
    sound_properties.extend(chunk(*b"CHNL", &channels));
    sound_properties.extend(chunk(*b"CMPR", b"DSD \x03DSD"));

    let mut marker = Vec::from(1_u16.to_be_bytes());
    marker.extend_from_slice(&[2, 3]);
    marker.extend_from_slice(&123_u32.to_be_bytes());
    marker.extend_from_slice(&(-17_i32).to_be_bytes());
    marker.extend_from_slice(&4_u16.to_be_bytes());
    marker.extend_from_slice(&2_u16.to_be_bytes());
    marker.extend_from_slice(&0_u16.to_be_bytes());
    marker.extend_from_slice(&7_u32.to_be_bytes());
    marker.extend_from_slice(b"Index 2");

    let mut body = b"DSD ".to_vec();
    body.extend(chunk(*b"FVER", &0x0105_0000_u32.to_be_bytes()));
    body.extend(chunk(*b"PROP", &sound_properties));
    body.extend(chunk(*b"DSD ", &[0x96, 0x69]));
    body.extend(chunk(*b"DIIN", &chunk(*b"MARK", &marker)));

    let mut file = b"FRM8".to_vec();
    file.extend_from_slice(&(body.len() as u64).to_be_bytes());
    file.extend(body);
    file
}

fn chunk(id: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = id.to_vec();
    bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    bytes.extend_from_slice(payload);
    if payload.len() & 1 != 0 {
        bytes.push(0);
    }
    bytes
}

fn top_level_chunk(file: &[u8], wanted: [u8; 4]) -> Vec<u8> {
    let end = 12 + u64::from_be_bytes(file[4..12].try_into().unwrap()) as usize;
    let mut position = 16;
    while position < end {
        let size =
            u64::from_be_bytes(file[position + 4..position + 12].try_into().unwrap()) as usize;
        let chunk_end = position + 12 + size + (size & 1);
        if file[position..position + 4] == wanted {
            return file[position..chunk_end].to_vec();
        }
        position = chunk_end;
    }
    panic!("missing top-level chunk {:?}", wanted);
}
