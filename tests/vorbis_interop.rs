use std::{
    fs,
    fs::File,
    io::{self, Cursor, Read, Write},
    path::PathBuf,
};

use ogg::{NO_GRANULE_POSITION, OggPacketReader, OggPacketWriter};
use vorbis::{
    VorbisCommentHeader, VorbisDecoder, VorbisEncoder, VorbisIdentificationHeader,
    VorbisSetupHeader, VorbisStreamReader, rewrite_metadata,
};
use zemer_core::{
    AudioDecoder, AudioEncoder, AudioSpec, Metadata, PcmBuffer, SampleFormat, Samples,
};

fn generated_mono_vorbis() -> Vec<u8> {
    let spec = AudioSpec::new(48_000, 1, SampleFormat::Float32).unwrap();
    let samples: Vec<f32> = (0..20_000)
        .map(|frame| (std::f64::consts::TAU * 317.0 * frame as f64 / 48_000.0).sin() as f32 * 0.6)
        .collect();
    let mut bytes = Vec::new();
    {
        let mut encoder = VorbisEncoder::new(&mut bytes, spec).unwrap();
        for chunk in samples.chunks(997) {
            let block = PcmBuffer::new(spec, Samples::F32(chunk.to_vec())).unwrap();
            encoder.write_block(&block).unwrap();
        }
        encoder.finish().unwrap();
    }
    bytes
}

fn generated_stereo_vorbis() -> Vec<u8> {
    let spec = AudioSpec::new(48_000, 2, SampleFormat::Float32).unwrap();
    let samples: Vec<f32> = (0..20_000)
        .flat_map(|frame| {
            let phase = frame as f64 / 48_000.0;
            [317.0, 503.0]
                .map(|frequency| (std::f64::consts::TAU * frequency * phase).sin() as f32 * 0.6)
        })
        .collect();
    let mut bytes = Vec::new();
    {
        let mut encoder = VorbisEncoder::new(&mut bytes, spec).unwrap();
        for chunk in samples.chunks(997 * 2) {
            let block = PcmBuffer::new(spec, Samples::F32(chunk.to_vec())).unwrap();
            encoder.write_block(&block).unwrap();
        }
        encoder.finish().unwrap();
    }
    bytes
}

struct ShortInterruptedReader<'a> {
    input: Cursor<&'a [u8]>,
    interrupt_next: bool,
}

impl Read for ShortInterruptedReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.interrupt_next {
            self.interrupt_next = false;
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.interrupt_next = true;
        let limit = output.len().min(13);
        self.input.read(&mut output[..limit])
    }
}

fn rewrite_vorbis_terminal_page(input: &[u8], terminal_granule: Option<u64>) -> (Vec<u8>, u64) {
    let mut reader = OggPacketReader::new(Cursor::new(input));
    let mut packets = Vec::new();
    while let Some((packet, owns_granule)) = reader.next_packet_with_granule().unwrap() {
        let granule = if owns_granule {
            packet.page_granule_position
        } else {
            NO_GRANULE_POSITION
        };
        packets.push((packet.bytes, granule));
    }
    let last = packets.last_mut().unwrap();
    let original_granule = last.1;
    if let Some(granule) = terminal_granule {
        last.1 = granule;
    }

    let mut writer = OggPacketWriter::new(Vec::new(), 0x5a45_4d45);
    for (packet, granule) in packets {
        writer.write_packet(&packet, granule).unwrap();
    }
    writer.finish().unwrap();
    (writer.into_inner(), original_granule)
}

fn rewrite_vorbis_sample_rate(input: &[u8], sample_rate: u32) -> Vec<u8> {
    let mut reader = OggPacketReader::new(Cursor::new(input));
    let mut writer = OggPacketWriter::new(Vec::new(), 0x5a45_4d45);
    let mut first = true;
    while let Some((mut packet, owns_granule)) = reader.next_packet_with_granule().unwrap() {
        if first {
            packet.bytes[12..16].copy_from_slice(&sample_rate.to_le_bytes());
            first = false;
        }
        let granule = if owns_granule {
            packet.page_granule_position
        } else {
            NO_GRANULE_POSITION
        };
        writer.write_packet(&packet.bytes, granule).unwrap();
    }
    assert!(!first, "fixture contains the Vorbis identification packet");
    writer.finish().unwrap();
    writer.into_inner()
}

fn shift_audio_granules(input: &[u8], from_packet: usize, offset: i64) -> Vec<u8> {
    let mut reader = OggPacketReader::new(Cursor::new(input));
    let mut writer = OggPacketWriter::new(Vec::new(), 0x5a45_4d45);
    let mut packet_index = 0;
    while let Some((packet, owns_granule)) = reader.next_packet_with_granule().unwrap() {
        let mut granule = if owns_granule {
            packet.page_granule_position
        } else {
            NO_GRANULE_POSITION
        };
        if packet_index >= from_packet && owns_granule {
            granule = if offset < 0 {
                granule.checked_sub(offset.unsigned_abs()).unwrap()
            } else {
                granule.checked_add(offset as u64).unwrap()
            };
        }
        writer.write_packet(&packet.bytes, granule).unwrap();
        packet_index += 1;
    }
    writer.finish().unwrap();
    writer.into_inner()
}

fn start_third_audio_packet_on_second_packet_page(input: &[u8]) -> Vec<u8> {
    let mut pages = Vec::new();
    let mut offset = 0;
    while offset < input.len() {
        let segment_count = usize::from(input[offset + 26]);
        let lacing_end = offset + 27 + segment_count;
        let body_len: usize = input[offset + 27..lacing_end]
            .iter()
            .map(|&lace| usize::from(lace))
            .sum();
        let end = lacing_end + body_len;
        pages.push((offset, lacing_end, end));
        offset = end;
    }

    let (second_start, second_lacing_end, second_end) = pages[4];
    let (third_start, third_lacing_end, third_end) = pages[5];
    let mut second_page = input[second_start..second_end].to_vec();
    let second_lacing_end = second_lacing_end - second_start;
    second_page[26] += 1;
    second_page.insert(second_lacing_end, 255);
    second_page.extend_from_slice(&[0; 255]);
    second_page[22..26].fill(0);
    let checksum = ogg_crc_for_test(&second_page);
    second_page[22..26].copy_from_slice(&checksum.to_le_bytes());

    let mut third_page = input[third_start..third_end].to_vec();
    let third_lacing_end = third_lacing_end - third_start;
    third_page[5] |= ogg::PAGE_FLAG_CONTINUED;
    third_page[26] += 1;
    third_page.insert(third_lacing_end, 0);
    third_page[22..26].fill(0);
    let checksum = ogg_crc_for_test(&third_page);
    third_page[22..26].copy_from_slice(&checksum.to_le_bytes());

    [
        input[..second_start].to_vec(),
        second_page,
        third_page,
        input[third_end..].to_vec(),
    ]
    .concat()
}

fn append_reserved_packet_to_eos_page(input: &[u8]) -> Vec<u8> {
    let bytes = input.to_vec();
    let mut page_start = 0;
    let mut final_page_start = None;
    while page_start < bytes.len() {
        assert_eq!(&bytes[page_start..page_start + 4], b"OggS");
        let segment_count = usize::from(bytes[page_start + 26]);
        let lacing_start = page_start + 27;
        let body_start = lacing_start + segment_count;
        let body_size: usize = bytes[lacing_start..body_start]
            .iter()
            .map(|&lace| usize::from(lace))
            .sum();
        let page_end = body_start + body_size;
        if page_end == bytes.len() {
            final_page_start = Some(page_start);
        }
        page_start = page_end;
    }
    let page_start = final_page_start.expect("Ogg input contains a terminal page");
    let segment_count = usize::from(bytes[page_start + 26]);
    assert!(segment_count < u8::MAX as usize);
    let lacing_start = page_start + 27;
    let body_start = lacing_start + segment_count;
    assert_eq!(
        bytes[page_start + 5] & ogg::PAGE_FLAG_EOS,
        ogg::PAGE_FLAG_EOS
    );

    let mut page = Vec::with_capacity(bytes.len() + 2);
    page.extend_from_slice(&bytes[..body_start]);
    page.push(1); // One-byte terminal reserved packet.
    page.extend_from_slice(&bytes[body_start..]);
    page.push(1);
    page[page_start + 26] = (segment_count + 1) as u8;
    page[page_start + 22..page_start + 26].fill(0);
    let checksum = ogg_crc_for_test(&page[page_start..]);
    page[page_start + 22..page_start + 26].copy_from_slice(&checksum.to_le_bytes());
    page
}

fn move_eos_to_nil_page(input: &[u8], granule: Option<u64>) -> (Vec<u8>, u64) {
    let mut bytes = input.to_vec();
    let mut page_start = 0;
    let mut final_page = None;
    while page_start < bytes.len() {
        assert_eq!(&bytes[page_start..page_start + 4], b"OggS");
        let segment_count = usize::from(bytes[page_start + 26]);
        let lacing_start = page_start + 27;
        let body_start = lacing_start + segment_count;
        let body_size: usize = bytes[lacing_start..body_start]
            .iter()
            .map(|&lace| usize::from(lace))
            .sum();
        let page_end = body_start + body_size;
        if page_end == bytes.len() {
            final_page = Some((page_start, body_start, segment_count));
        }
        page_start = page_end;
    }
    let (page_start, _, segment_count) = final_page.expect("Ogg input contains a terminal page");
    assert_eq!(
        bytes[page_start + 5] & ogg::PAGE_FLAG_EOS,
        ogg::PAGE_FLAG_EOS
    );
    let original_granule =
        u64::from_le_bytes(bytes[page_start + 6..page_start + 14].try_into().unwrap());
    let serial = u32::from_le_bytes(bytes[page_start + 14..page_start + 18].try_into().unwrap());
    let sequence = u32::from_le_bytes(bytes[page_start + 18..page_start + 22].try_into().unwrap());
    let selected_granule = granule.unwrap_or(original_granule);

    bytes[page_start + 5] &= !ogg::PAGE_FLAG_EOS;
    bytes[page_start + 22..page_start + 26].fill(0);
    let checksum = ogg_crc_for_test(&bytes[page_start..]);
    bytes[page_start + 22..page_start + 26].copy_from_slice(&checksum.to_le_bytes());

    let mut nil_page = vec![0; 27];
    nil_page[..4].copy_from_slice(b"OggS");
    nil_page[5] = ogg::PAGE_FLAG_EOS;
    nil_page[6..14].copy_from_slice(&selected_granule.to_le_bytes());
    nil_page[14..18].copy_from_slice(&serial.to_le_bytes());
    nil_page[18..22].copy_from_slice(&sequence.wrapping_add(1).to_le_bytes());
    nil_page[26] = 0;
    let checksum = ogg_crc_for_test(&nil_page);
    nil_page[22..26].copy_from_slice(&checksum.to_le_bytes());
    bytes.extend_from_slice(&nil_page);
    assert!(segment_count > 0);
    (bytes, original_granule)
}

fn reserial_ogg_stream(input: &[u8], serial: u32) -> Vec<u8> {
    let mut reader = OggPacketReader::new(Cursor::new(input));
    let mut writer = OggPacketWriter::new(Vec::new(), serial);
    while let Some((packet, owns_granule)) = reader.next_packet_with_granule().unwrap() {
        writer
            .write_packet(
                &packet.bytes,
                if owns_granule {
                    packet.page_granule_position
                } else {
                    NO_GRANULE_POSITION
                },
            )
            .unwrap();
    }
    writer.finish().unwrap();
    writer.into_inner()
}

fn truncate_audio_packet(input: &[u8], packet_index: usize, length: usize) -> Vec<u8> {
    let mut reader = OggPacketReader::new(Cursor::new(input));
    let mut packets = Vec::new();
    while let Some((packet, owns_granule)) = reader.next_packet_with_granule().unwrap() {
        let granule = if owns_granule {
            packet.page_granule_position
        } else {
            NO_GRANULE_POSITION
        };
        packets.push((packet.bytes, granule));
    }
    assert!(packet_index >= 3 && packet_index < packets.len());
    packets[packet_index].0.truncate(length);

    let mut writer = OggPacketWriter::new(Vec::new(), 0x5a45_4d45);
    for (packet, granule) in packets {
        writer.write_packet(&packet, granule).unwrap();
    }
    writer.finish().unwrap();
    writer.into_inner()
}

fn ogg_crc_for_test(page: &[u8]) -> u32 {
    let mut crc = 0_u32;
    for (index, &byte) in page.iter().enumerate() {
        let byte = if (22..26).contains(&index) { 0 } else { byte };
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn read_vorbis_to_end(bytes: &[u8]) -> Result<Vec<f32>, zemer_core::AudioError> {
    let mut decoder = VorbisDecoder::new(Cursor::new(bytes))?;
    let mut samples = Vec::new();
    while let Some(block) = decoder.read_frames(1024)? {
        let Samples::F32(block_samples) = block.samples() else {
            unreachable!("Vorbis decoder always returns Float32 samples");
        };
        samples.extend_from_slice(block_samples);
    }
    Ok(samples)
}

#[test]
fn decodes_from_short_reads_with_interrupted_io() {
    let bytes = generated_mono_vorbis();
    let expected = read_vorbis_to_end(&bytes).unwrap();
    let reader = ShortInterruptedReader {
        input: Cursor::new(&bytes),
        interrupt_next: true,
    };
    let mut decoder = VorbisDecoder::new(reader).unwrap();
    let mut actual = Vec::new();
    while let Some(block) = decoder.read_frames(733).unwrap() {
        let Samples::F32(samples) = block.samples() else {
            panic!("Vorbis decoder did not return Float32 PCM");
        };
        actual.extend_from_slice(samples);
    }
    assert_eq!(actual, expected);
}

fn ogg_page_ranges(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut pages = Vec::new();
    let mut page_start = 0;
    while page_start < bytes.len() {
        assert!(
            bytes.len() - page_start >= 27,
            "fixture has a complete page header"
        );
        assert_eq!(&bytes[page_start..page_start + 4], b"OggS");
        let segment_count = usize::from(bytes[page_start + 26]);
        let lacing_start = page_start + 27;
        let body_start = lacing_start + segment_count;
        assert!(
            body_start <= bytes.len(),
            "fixture has a complete lacing table"
        );
        let body_bytes: usize = bytes[lacing_start..body_start]
            .iter()
            .map(|&lace| usize::from(lace))
            .sum();
        let page_end = body_start + body_bytes;
        assert!(page_end <= bytes.len(), "fixture has a complete page body");
        pages.push((page_start, page_end));
        page_start = page_end;
    }
    pages
}

fn mutate_ogg_page(
    input: &[u8],
    page_index: usize,
    mutate: impl FnOnce(&mut [u8]) -> usize,
) -> Vec<u8> {
    let (page_start, page_end) = ogg_page_ranges(input)[page_index];
    let mut output = input.to_vec();
    let checksum_len = mutate(&mut output[page_start..page_end]);
    output[page_start + 22..page_start + 26].fill(0);
    let checksum = ogg_crc_for_test(&output[page_start..page_start + checksum_len]);
    output[page_start + 22..page_start + 26].copy_from_slice(&checksum.to_le_bytes());
    output
}

fn mutate_ogg_page_lacing(
    input: &[u8],
    page_index: usize,
    lacing_index: usize,
    delta: i16,
) -> Option<Vec<u8>> {
    let (page_start, page_end) = *ogg_page_ranges(input).get(page_index)?;
    let lacing_start = page_start + 27;
    let lacing_count = usize::from(input[page_start + 26]);
    if lacing_index >= lacing_count || !matches!(delta, -1 | 1) {
        return None;
    }
    let new_lacing = i16::from(input[lacing_start + lacing_index]) + delta;
    if !(0..=255).contains(&new_lacing) {
        return None;
    }

    let mut output = input.to_vec();
    output[lacing_start + lacing_index] = new_lacing as u8;
    let checksum_end = if delta > 0 {
        output.insert(page_end, 0);
        page_end + 1
    } else {
        output.remove(page_end - 1);
        page_end - 1
    };
    output[page_start + 22..page_start + 26].fill(0);
    let checksum = ogg_crc_for_test(&output[page_start..checksum_end]);
    output[page_start + 22..page_start + 26].copy_from_slice(&checksum.to_le_bytes());
    Some(output)
}

fn mutate_ogg_page_segment_count(input: &[u8], page_index: usize, delta: i16) -> Option<Vec<u8>> {
    let (page_start, page_end) = *ogg_page_ranges(input).get(page_index)?;
    let lacing_start = page_start + 27;
    let lacing_count = usize::from(input[page_start + 26]);
    if !matches!(delta, -1 | 1) {
        return None;
    }
    let new_count = i16::try_from(lacing_count).ok()? + delta;
    if !(0..=255).contains(&new_count) {
        return None;
    }

    let mut output = input.to_vec();
    output[page_start + 26] = new_count as u8;
    let checksum_end = if delta > 0 {
        let body_start = lacing_start + lacing_count;
        let first_body_byte = *input.get(body_start)?;
        let extra_bytes = usize::from(first_body_byte) + 1;
        output.splice(page_end..page_end, std::iter::repeat_n(0, extra_bytes));
        page_end + extra_bytes
    } else {
        if lacing_count == 0 {
            return None;
        }
        let last_lacing = usize::from(input[lacing_start + lacing_count - 1]);
        let new_page_end = page_end - last_lacing - 1;
        output.drain(new_page_end..page_end);
        new_page_end
    };
    output[page_start + 22..page_start + 26].fill(0);
    let checksum = ogg_crc_for_test(&output[page_start..checksum_end]);
    output[page_start + 22..page_start + 26].copy_from_slice(&checksum.to_le_bytes());
    Some(output)
}

fn representative_page_truncations(bytes: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    for (page_start, page_end) in ogg_page_ranges(bytes) {
        let segment_count = usize::from(bytes[page_start + 26]);
        let lacing_start = page_start + 27;
        let body_start = lacing_start + segment_count;

        for offset in [
            page_start + 1,
            page_start + 26,
            page_start + 27,
            body_start,
            page_end.saturating_sub(1),
            page_end,
        ] {
            if offset < bytes.len() {
                offsets.push(offset);
            }
        }
    }
    offsets.sort_unstable();
    offsets.dedup();
    offsets
}

#[test]
fn rejects_malformed_ogg_page_layouts_in_vorbis_streams() {
    let encoded = generated_stereo_vorbis();
    let spurious_continuation = mutate_ogg_page(&encoded, 0, |page| {
        page[5] |= ogg::PAGE_FLAG_CONTINUED;
        page.len()
    });
    assert!(read_vorbis_to_end(&spurious_continuation).is_err());

    let split = start_third_audio_packet_on_second_packet_page(&encoded);
    let mut reader = OggPacketReader::new(Cursor::new(&split));
    while reader.next_packet().unwrap().is_some() {}
    let missing_continuation = mutate_ogg_page(&split, 5, |page| {
        page[5] &= !ogg::PAGE_FLAG_CONTINUED;
        page.len()
    });
    assert!(read_vorbis_to_end(&missing_continuation).is_err());

    let missing_lacing = mutate_ogg_page(&encoded, 0, |page| {
        page[26] = 0;
        27
    });
    assert!(read_vorbis_to_end(&missing_lacing).is_err());
}

#[test]
fn real_libvorbis_setup_header_bit_mutations_never_panic() {
    let input = include_bytes!("fixtures/vorbis/libvorbis-44100-2.ogg");
    let mut reader = OggPacketReader::new(Cursor::new(input));
    let mut headers = Vec::new();
    for _ in 0..3 {
        let (packet, _) = reader.next_packet_with_granule().unwrap().unwrap();
        headers.push(packet.bytes);
    }
    assert!(headers[2].len() > 4000);
    let identification = VorbisIdentificationHeader::parse(&headers[0]).unwrap();

    for (byte_index, _) in headers[2].iter().enumerate() {
        for bit_index in 0..8 {
            let mut packet = headers[2].clone();
            packet[byte_index] ^= 1 << bit_index;
            let result =
                std::panic::catch_unwind(|| VorbisSetupHeader::parse(&packet, &identification));
            assert!(
                result.is_ok(),
                "Vorbis setup parser panicked after mutating bit {bit_index} of byte {byte_index}"
            );
        }
    }
}

#[test]
fn rejects_truncated_ogg_pages_through_the_public_decoder() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/vorbis/libvorbis-44100-2.ogg");
    let encoded = fs::read(fixture).unwrap();

    for end in representative_page_truncations(&encoded) {
        assert!(
            read_vorbis_to_end(&encoded[..end]).is_err(),
            "decoder accepted Ogg input truncated at byte {end}"
        );
    }
}

#[test]
fn rejects_every_truncation_of_the_vorbis_identification_page() {
    let spec = AudioSpec::new(44_100, 1, SampleFormat::Float32).unwrap();
    let audio = PcmBuffer::new(spec, Samples::F32(vec![0.25; 257])).unwrap();
    let mut encoded = Vec::new();
    {
        let mut encoder = VorbisEncoder::new(&mut encoded, spec).unwrap();
        encoder.write_block(&audio).unwrap();
        encoder.finish().unwrap();
    }
    let (_, page_end) = ogg_page_ranges(&encoded)[0];
    let page = &encoded[..page_end];
    assert!(
        page.len() < 4096,
        "identification page grew to {} bytes",
        page.len()
    );

    for end in 0..page.len() {
        let result =
            std::panic::catch_unwind(|| VorbisStreamReader::new(Cursor::new(&page[..end])));
        assert!(
            result.is_ok(),
            "Vorbis parser panicked on the identification page prefix ending at byte {end}"
        );
        assert!(
            result.unwrap().is_err(),
            "Vorbis parser accepted an incomplete stream ending at byte {end}"
        );
    }
}

#[test]
fn rejects_invalid_vorbis_terminal_granules() {
    let encoded = generated_mono_vorbis();
    let (_, valid_granule) = rewrite_vorbis_terminal_page(&encoded, None);

    for invalid_granule in [NO_GRANULE_POSITION, valid_granule + 100_000, 0] {
        let (malformed, _) = rewrite_vorbis_terminal_page(&encoded, Some(invalid_granule));
        assert!(
            read_vorbis_to_end(&malformed).is_err(),
            "decoder accepted invalid terminal granule {invalid_granule}"
        );
    }
}

#[test]
fn applies_initial_granule_offsets_to_the_start_of_pcm() {
    let encoded = generated_stereo_vorbis();
    let expected = read_vorbis_to_end(&encoded).unwrap();
    let stream = VorbisStreamReader::new(Cursor::new(&encoded)).unwrap();
    let [small_block, large_block] = stream.identification().block_sizes();
    assert_eq!(small_block, large_block);
    let first_chunk_frames = (small_block / 2) as u64;

    let mut packets = OggPacketReader::new(Cursor::new(&encoded));
    for packet_index in 0..=4 {
        let (packet, owns_granule) = packets.next_packet_with_granule().unwrap().unwrap();
        if packet_index == 4 {
            assert!(owns_granule);
            assert_eq!(packet.page_granule_position, first_chunk_frames);
        }
    }

    let trimmed = shift_audio_granules(&encoded, 4, -1);
    assert_eq!(read_vorbis_to_end(&trimmed).unwrap(), expected[2..]);

    let continued = start_third_audio_packet_on_second_packet_page(&trimmed);
    assert!(matches!(
        read_vorbis_to_end(&continued),
        Err(zemer_core::AudioError::InvalidFormat(
            "non-zero initial Vorbis granule requires a fresh page"
        ))
    ));

    let offset = shift_audio_granules(&encoded, 4, 7);
    assert_eq!(read_vorbis_to_end(&offset).unwrap(), expected);
}

#[test]
fn rejects_mismatched_intermediate_granule() {
    let encoded = generated_mono_vorbis();
    let malformed = shift_audio_granules(&encoded, 5, 1);
    assert!(matches!(
        read_vorbis_to_end(&malformed),
        Err(zemer_core::AudioError::InvalidFormat(
            "Vorbis page granule does not match decoded PCM"
        ))
    ));
}

#[test]
fn accepts_terminal_reserved_packet_and_keeps_prior_audio() {
    let encoded = generated_mono_vorbis();
    let expected = read_vorbis_to_end(&encoded).unwrap();
    let with_reserved_terminal = append_reserved_packet_to_eos_page(&encoded);
    let actual = read_vorbis_to_end(&with_reserved_terminal).unwrap();
    assert_eq!(actual.len(), expected.len());
    assert_eq!(actual, expected);
}

#[test]
fn decodes_and_rewrites_a_nil_terminal_eos_page() {
    let encoded = generated_mono_vorbis();
    let expected = read_vorbis_to_end(&encoded).unwrap();
    let (nil_eos, terminal_granule) = move_eos_to_nil_page(&encoded, None);
    assert_eq!(read_vorbis_to_end(&nil_eos).unwrap(), expected);

    for invalid in [NO_GRANULE_POSITION, terminal_granule + 100_000, 0] {
        let (malformed, _) = move_eos_to_nil_page(&encoded, Some(invalid));
        assert!(read_vorbis_to_end(&malformed).is_err());
    }

    let mut rewritten = Vec::new();
    rewrite_metadata(Cursor::new(&nil_eos), &mut rewritten, &Metadata::default()).unwrap();
    assert_eq!(read_vorbis_to_end(&rewritten).unwrap(), expected);
}

#[test]
fn chains_nil_eos_into_a_different_spec_and_rewrites_both_links() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    let first_source = fs::read(fixtures.join("libvorbis-8000-1.ogg")).unwrap();
    let second_source = fs::read(fixtures.join("libvorbis-44100-2.ogg")).unwrap();
    let (first, _) = move_eos_to_nil_page(&first_source, None);
    let serial = u32::from_le_bytes(first[14..18].try_into().unwrap()).wrapping_add(1);
    let second = reserial_ogg_stream(&second_source, serial);
    let mut chained = first;
    chained.extend_from_slice(&second);

    let first_oracle = fs::read(fixtures.join("libsndfile-8000-1.f32le")).unwrap();
    let second_oracle = fs::read(fixtures.join("libsndfile-44100-2.f32le")).unwrap();
    let first_expected: Vec<f32> = first_oracle
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    let second_expected: Vec<f32> = second_oracle
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();

    let mut decoder = VorbisDecoder::new(Cursor::new(&chained)).unwrap();
    let mut first_actual = Vec::new();
    while first_actual.len() < first_expected.len() {
        let remaining = first_expected.len() - first_actual.len();
        let block = decoder.read_frames(remaining.min(733)).unwrap().unwrap();
        assert_eq!(block.spec().sample_rate(), 8_000);
        assert_eq!(block.spec().channels(), 1);
        let Samples::F32(samples) = block.samples() else {
            unreachable!("Vorbis decoder always returns Float32 samples");
        };
        first_actual.extend_from_slice(samples);
    }
    assert_pcm_samples_close(&first_actual, &first_expected);

    let mut second_actual = Vec::new();
    while let Some(block) = decoder.read_frames(733).unwrap() {
        assert_eq!(block.spec().sample_rate(), 44_100);
        assert_eq!(block.spec().channels(), 2);
        let Samples::F32(samples) = block.samples() else {
            unreachable!("Vorbis decoder always returns Float32 samples");
        };
        second_actual.extend_from_slice(samples);
    }
    assert_pcm_samples_close(&second_actual, &second_expected);

    let before_links = ogg_links(&chained);
    let mut metadata = Metadata::default();
    metadata.insert("title", "First link title");
    let mut rewritten = Vec::new();
    rewrite_metadata(Cursor::new(&chained), &mut rewritten, &metadata).unwrap();
    let after_links = ogg_links(&rewritten);
    assert_eq!(after_links.len(), 2);
    assert_eq!(
        VorbisCommentHeader::parse(&after_links[0][1])
            .unwrap()
            .metadata()
            .values("title"),
        ["First link title"]
    );
    assert_eq!(after_links[1][1], before_links[1][1]);
    let mut expected = first_expected;
    expected.extend(second_expected);
    let actual = read_vorbis_to_end(&rewritten).unwrap();
    assert_pcm_samples_close(&actual, &expected);
}

#[test]
fn conceals_truncated_audio_packets_and_keeps_the_granule_sample_count() {
    let encoded = generated_mono_vorbis();
    let mut reference = OggPacketReader::new(Cursor::new(&encoded));
    for _ in 0..3 {
        reference.next_packet().unwrap().unwrap();
    }
    let first_audio = reference.next_packet().unwrap().unwrap();
    let lengths = [1, first_audio.bytes.len() / 2, first_audio.bytes.len() - 1];

    for length in lengths {
        let malformed = truncate_audio_packet(&encoded, 3, length);
        let decoded = read_vorbis_to_end(&malformed).unwrap();
        assert_eq!(decoded.len(), 20_000, "truncated packet length {length}");
        assert!(
            decoded.iter().all(|sample| sample.is_finite()),
            "truncated packet length {length} produced non-finite PCM"
        );
    }
}

#[test]
fn handles_truncated_real_audio_packets_at_each_stream_position() {
    let input = include_bytes!("fixtures/vorbis/libvorbis-44100-2.ogg");
    let mut reader = OggPacketReader::new(Cursor::new(input));
    let mut packet_lengths = Vec::new();
    while let Some(packet) = reader.next_packet().unwrap() {
        packet_lengths.push(packet.bytes.len());
    }
    assert!(
        packet_lengths.len() > 5,
        "fixture has first, middle, and last audio packets"
    );
    let last_packet = packet_lengths.len() - 1;
    let middle_packet = 3 + (last_packet - 3) / 2;

    for packet_index in [3, middle_packet, last_packet] {
        let packet_length = packet_lengths[packet_index];
        assert!(
            packet_length > 2,
            "audio packet can be truncated at multiple offsets"
        );
        for length in [1, packet_length / 2, packet_length - 1] {
            let malformed = truncate_audio_packet(input, packet_index, length);
            let result = std::panic::catch_unwind(|| read_vorbis_to_end(&malformed));
            assert!(
                result.is_ok(),
                "decoder panicked on truncated packet {packet_index} at length {length}"
            );
            if let Ok(decoded) = result.unwrap() {
                assert_eq!(decoded.len(), 12_347 * 2);
                assert!(decoded.iter().all(|sample| sample.is_finite()));
            }
        }
    }
}

#[test]
fn decoder_handles_mutations_in_first_middle_and_last_real_audio_packets() {
    let input = include_bytes!("fixtures/vorbis/libvorbis-44100-2.ogg");
    let mut reader = OggPacketReader::new(Cursor::new(input));
    let mut packets = Vec::new();
    while let Some((packet, owns_granule)) = reader.next_packet_with_granule().unwrap() {
        let granule = if owns_granule {
            packet.page_granule_position
        } else {
            NO_GRANULE_POSITION
        };
        packets.push((packet.bytes, granule));
    }
    assert!(packets.len() > 3, "fixture contains an audio packet");
    let first_packet_bits = packets[3].0.len() * 8;
    assert!(first_packet_bits > 0, "fixture audio packet is non-empty");

    for bit_index in 0..first_packet_bits {
        assert_packet_bit_mutation_is_safe(&packets, 3, bit_index);
    }

    let last_packet = packets.len() - 1;
    assert!(last_packet > 3, "fixture contains multiple audio packets");
    let middle_packet = 3 + (last_packet - 3) / 2;
    assert!(middle_packet > 3 && middle_packet < last_packet);
    for packet_index in [middle_packet, last_packet] {
        let packet_bits = packets[packet_index].0.len() * 8;
        assert!(packet_bits > 0, "audio packet is non-empty");
        let mutation_count = packet_bits.min(64);
        for sample in 0..mutation_count {
            let bit_index = sample * packet_bits / mutation_count;
            assert_packet_bit_mutation_is_safe(&packets, packet_index, bit_index);
        }
    }
}

#[test]
fn decoder_handles_deterministic_multi_bit_audio_packet_mutations() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    let files = [
        "libvorbis-8000-1.ogg",
        "libvorbis-44100-2.ogg",
        "libvorbis-32000-3.ogg",
        "libvorbis-48000-6.ogg",
        "libvorbis-96000-8.ogg",
        "libsndfile-192000-2.ogg",
        "oggenc-22050-4.ogg",
        "oggenc-44100-5.ogg",
        "rusty-vorbis-0_1_1-44100-2.ogg",
    ];
    for (file_index, filename) in files.into_iter().enumerate() {
        let input = fs::read(fixtures.join(filename)).unwrap();
        let mut reader = OggPacketReader::new(Cursor::new(input));
        let mut packets = Vec::new();
        while let Some((packet, owns_granule)) = reader.next_packet_with_granule().unwrap() {
            let granule = if owns_granule {
                packet.page_granule_position
            } else {
                NO_GRANULE_POSITION
            };
            packets.push((packet.bytes, granule));
        }
        assert!(packets.len() > 3, "{filename} contains audio packets");

        for case in 0..16_u64 {
            let mut mutated = packets.clone();
            let mut state = case ^ (file_index as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            for _ in 0..8 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let packet_index = 3 + state as usize % (mutated.len() - 3);
                let packet_bits = mutated[packet_index].0.len() * 8;
                assert!(packet_bits > 0, "{filename} audio packets are non-empty");
                let bit_index = (state >> 32) as usize % packet_bits;
                mutated[packet_index].0[bit_index / 8] ^= 1 << (bit_index % 8);
            }

            let mut writer = OggPacketWriter::new(Vec::new(), 0x5a45_4d45);
            for (packet, granule) in &mutated {
                writer.write_packet(packet, *granule).unwrap();
            }
            writer.finish().unwrap();
            let stream = writer.into_inner();
            let result = std::panic::catch_unwind(|| {
                if let Ok(decoded) = read_vorbis_to_end(&stream) {
                    assert!(decoded.iter().all(|sample| sample.is_finite()));
                }
            });
            assert!(
                result.is_ok(),
                "decoder panicked after multi-bit audio packet mutations in {filename}, case {case}"
            );
        }
    }
}

#[test]
fn decoder_handles_valid_crc_mutations_in_real_ogg_page_headers() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    for filename in ["libvorbis-44100-2.ogg", "rusty-vorbis-0_1_1-44100-2.ogg"] {
        let input = fs::read(fixtures.join(filename)).unwrap();
        let page_count = ogg_page_ranges(&input).len();
        assert!(
            page_count > 2,
            "{filename} has first, middle, and final pages"
        );
        for page_index in [0, page_count / 2, page_count - 1] {
            for byte_index in 0..22 {
                for bit_index in 0..u8::BITS {
                    let mutated = mutate_ogg_page(&input, page_index, |page| {
                        page[byte_index] ^= 1 << bit_index;
                        page.len()
                    });
                    let result = std::panic::catch_unwind(|| read_vorbis_to_end(&mutated));
                    assert!(
                        result.is_ok(),
                        "decoder panicked on {filename} page {page_index} header byte {byte_index} bit {bit_index}"
                    );
                    if let Ok(Ok(samples)) = result {
                        assert!(
                            samples.iter().all(|sample| sample.is_finite()),
                            "{filename} page {page_index} header mutation produced non-finite PCM"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn decoder_handles_valid_crc_mutations_in_real_ogg_lacing_tables() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    let mut mutations = 0;
    for filename in ["libvorbis-44100-2.ogg", "rusty-vorbis-0_1_1-44100-2.ogg"] {
        let mutations_before_file = mutations;
        let input = fs::read(fixtures.join(filename)).unwrap();
        let pages = ogg_page_ranges(&input);
        assert!(
            pages.len() > 2,
            "{filename} has first, middle, and final pages"
        );
        for page_index in [0, pages.len() / 2, pages.len() - 1] {
            let (page_start, _) = pages[page_index];
            let lacing_count = usize::from(input[page_start + 26]);
            for lacing_index in 0..lacing_count {
                for delta in [-1_i16, 1] {
                    let Some(mutated) =
                        mutate_ogg_page_lacing(&input, page_index, lacing_index, delta)
                    else {
                        continue;
                    };
                    let result = std::panic::catch_unwind(|| read_vorbis_to_end(&mutated));
                    assert!(
                        result.is_ok(),
                        "decoder panicked on {filename} page {page_index} lacing entry {lacing_index} delta {delta}"
                    );
                    if let Ok(Ok(samples)) = result {
                        assert!(
                            samples.iter().all(|sample| sample.is_finite()),
                            "{filename} page {page_index} lacing mutation produced non-finite PCM"
                        );
                    }
                    mutations += 1;
                }
            }
        }
        assert!(
            mutations > mutations_before_file,
            "{filename} contains mutable lacing entries"
        );
    }
    assert!(mutations > 0, "fixtures contain mutable lacing entries");
}

#[test]
fn decoder_handles_valid_crc_mutations_in_real_ogg_segment_counts() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    let mut mutations = 0;
    for filename in ["libvorbis-44100-2.ogg", "rusty-vorbis-0_1_1-44100-2.ogg"] {
        let mutations_before_file = mutations;
        let input = fs::read(fixtures.join(filename)).unwrap();
        let pages = ogg_page_ranges(&input);
        assert!(
            pages.len() > 2,
            "{filename} has first, middle, and final pages"
        );
        for page_index in [0, pages.len() / 2, pages.len() - 1] {
            for delta in [-1_i16, 1] {
                let Some(mutated) = mutate_ogg_page_segment_count(&input, page_index, delta) else {
                    continue;
                };
                let result = std::panic::catch_unwind(|| read_vorbis_to_end(&mutated));
                assert!(
                    result.is_ok(),
                    "decoder panicked on {filename} page {page_index} segment count delta {delta}"
                );
                if let Ok(Ok(samples)) = result {
                    assert!(
                        samples.iter().all(|sample| sample.is_finite()),
                        "{filename} page {page_index} segment-count mutation produced non-finite PCM"
                    );
                }
                mutations += 1;
            }
        }
        assert!(
            mutations > mutations_before_file,
            "{filename} has a mutable real page segment count"
        );
    }
    assert!(mutations > 0, "fixtures have mutable page segment counts");
}

#[test]
fn decoder_handles_sampled_audio_packet_mutations_across_libvorbis_profiles() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    for (sample_rate, channels) in [(8_000, 1), (32_000, 3), (48_000, 6), (96_000, 8)] {
        let input =
            fs::read(fixtures.join(format!("libvorbis-{sample_rate}-{channels}.ogg"))).unwrap();
        let mut reader = OggPacketReader::new(Cursor::new(input));
        let mut packets = Vec::new();
        while let Some((packet, owns_granule)) = reader.next_packet_with_granule().unwrap() {
            let granule = if owns_granule {
                packet.page_granule_position
            } else {
                NO_GRANULE_POSITION
            };
            packets.push((packet.bytes, granule));
        }
        let last_packet = packets.len() - 1;
        let middle_packet = 3 + (last_packet - 3) / 2;
        for packet_index in [3, middle_packet, last_packet] {
            let packet_bits = packets[packet_index].0.len() * 8;
            assert!(packet_bits > 0, "audio packet is non-empty");
            for fraction in [0, 1, 2] {
                let bit_index = fraction * packet_bits / 3;
                assert_packet_bit_mutation_is_safe(&packets, packet_index, bit_index);
            }
        }
    }
}

#[test]
fn decoder_handles_sampled_audio_packet_mutations_from_independent_encoders() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    for filename in [
        "rusty-vorbis-0_1_1-44100-2.ogg",
        "wikimedia-cc0-432hz.ogg",
        "oxideav-vorbis-0_0_12-48000-6.ogg",
    ] {
        let input = fs::read(fixtures.join(filename)).unwrap();
        let mut reader = OggPacketReader::new(Cursor::new(input));
        let mut packets = Vec::new();
        while let Some((packet, owns_granule)) = reader.next_packet_with_granule().unwrap() {
            let granule = if owns_granule {
                packet.page_granule_position
            } else {
                NO_GRANULE_POSITION
            };
            packets.push((packet.bytes, granule));
        }
        assert!(packets.len() > 4, "{filename} has multiple audio packets");
        let last_packet = packets.len() - 1;
        let middle_packet = 3 + (last_packet - 3) / 2;
        for packet_index in [3, middle_packet, last_packet] {
            let packet_bits = packets[packet_index].0.len() * 8;
            assert!(packet_bits > 0, "audio packet is non-empty");
            for fraction in [0, 1, 2] {
                let bit_index = fraction * packet_bits / 3;
                assert_packet_bit_mutation_is_safe(&packets, packet_index, bit_index);
            }
        }
    }
}

fn assert_packet_bit_mutation_is_safe(packets: &[(Vec<u8>, u64)], packet_index: usize, bit: usize) {
    let mut mutated = packets.to_vec();
    mutated[packet_index].0[bit / 8] ^= 1 << (bit % 8);
    let mut writer = OggPacketWriter::new(Vec::new(), 0x5a45_4d45);
    for (packet, granule) in &mutated {
        writer.write_packet(packet, *granule).unwrap();
    }
    writer.finish().unwrap();
    let stream = writer.into_inner();
    let result = std::panic::catch_unwind(|| {
        if let Ok(decoded) = read_vorbis_to_end(&stream) {
            assert!(decoded.iter().all(|sample| sample.is_finite()));
        }
    });
    assert!(
        result.is_ok(),
        "Vorbis decoder panicked after flipping bit {bit} of byte {} in packet {packet_index}",
        bit / 8
    );
}

#[test]
fn decodes_libvorbis_profiles_against_libsndfile_pcm_oracles() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    for (sample_rate, channels) in [
        (8_000, 1),
        (44_100, 2),
        (32_000, 3),
        (48_000, 6),
        (96_000, 8),
    ] {
        let input = fixtures.join(format!("libvorbis-{sample_rate}-{channels}.ogg"));
        let oracle = fixtures.join(format!("libsndfile-{sample_rate}-{channels}.f32le"));
        compare_external_pcm(&input, &oracle, Some((sample_rate, channels, 12_347)));
    }
}

#[test]
fn decodes_vorbis_at_nonzero_sample_rate_boundaries() {
    let input = include_bytes!("fixtures/vorbis/libvorbis-44100-2.ogg");
    let oracle = include_bytes!("fixtures/vorbis/libsndfile-44100-2.f32le");
    let expected: Vec<f32> = oracle
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();

    assert!(VorbisDecoder::new(Cursor::new(rewrite_vorbis_sample_rate(input, 0))).is_err());

    // Vorbis requires a nonzero sample rate but sets no upper bound.
    for sample_rate in [1, u32::MAX] {
        let bytes = rewrite_vorbis_sample_rate(input, sample_rate);
        let mut decoder = VorbisDecoder::new(Cursor::new(bytes)).unwrap();
        assert_eq!(decoder.spec().sample_rate(), sample_rate);
        let mut actual = Vec::new();
        while let Some(block) = decoder.read_frames(733).unwrap() {
            let Samples::F32(samples) = block.samples() else {
                panic!("Vorbis decoder did not return float32 PCM");
            };
            actual.extend_from_slice(samples);
        }
        assert_pcm_samples_close(&actual, &expected);
    }
}

#[test]
fn decodes_192khz_stereo_libsndfile_stream_against_ffmpeg_oracle() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    compare_external_pcm(
        &fixtures.join("libsndfile-192000-2.ogg"),
        &fixtures.join("ffmpeg-native-192000-2.f32le"),
        Some((192_000, 2, 12_347)),
    );
}

#[test]
fn decodes_oggenc_four_and_five_channel_profiles_against_libsndfile_oracles() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    for (sample_rate, channels, frames) in [(22_050, 4, 18_211), (44_100, 5, 30_017)] {
        let stem = format!("{sample_rate}-{channels}");
        let input = fixtures.join(format!("oggenc-{stem}.ogg"));
        let oracle = fixtures.join(format!("libsndfile-{stem}.f32le"));
        compare_external_pcm(&input, &oracle, Some((sample_rate, channels, frames)));
    }
}

#[test]
fn decodes_wikimedia_cc0_vorbis_recording_against_libsndfile_oracle() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    compare_external_pcm(
        &fixtures.join("wikimedia-cc0-example.ogg"),
        &fixtures.join("wikimedia-cc0-example.f32le"),
        Some((44_100, 2, 163_328)),
    );
}

#[test]
fn decodes_wikimedia_cc0_legacy_vorbis_recording_against_libsndfile_oracle() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    compare_external_pcm(
        &fixtures.join("wikimedia-cc0-432hz.ogg"),
        &fixtures.join("wikimedia-cc0-432hz.f32le"),
        Some((44_100, 2, 356_352)),
    );
}

#[test]
fn decodes_rusty_vorbis_encoder_output_against_libsndfile_oracle() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    compare_external_pcm(
        &fixtures.join("rusty-vorbis-0_1_1-44100-2.ogg"),
        &fixtures.join("rusty-vorbis-0_1_1-44100-2.f32le"),
        Some((44_100, 2, 12_347)),
    );
}

#[test]
fn decodes_oxideav_vorbis_encoder_output_against_libsndfile_oracle() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    compare_external_pcm(
        &fixtures.join("oxideav-vorbis-0_0_12-44100-2.ogg"),
        &fixtures.join("oxideav-vorbis-0_0_12-44100-2.f32le"),
        Some((44_100, 2, 12_347)),
    );
}

#[test]
fn decodes_oxideav_six_channel_vorbis_output_against_libsndfile_oracle() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vorbis");
    compare_external_pcm(
        &fixtures.join("oxideav-vorbis-0_0_12-48000-6.ogg"),
        &fixtures.join("oxideav-vorbis-0_0_12-48000-6.f32le"),
        Some((48_000, 6, 12_347)),
    );
}

#[test]
#[ignore = "writes a generated Vorbis file to ZEMER_VORBIS_ENCODED_OUTPUT for an external decoder oracle"]
fn encodes_reference_decoder_input_file() {
    let output = PathBuf::from(std::env::var_os("ZEMER_VORBIS_ENCODED_OUTPUT").unwrap());
    let spec = AudioSpec::new(44_100, 2, SampleFormat::Float32).unwrap();
    let frame_count = 12_347_usize;
    let input: Vec<f32> = (0..frame_count)
        .flat_map(|frame| {
            let phase = std::f64::consts::TAU * 440.0 * frame as f64 / 44_100.0;
            [(phase.sin() * 0.7) as f32, (phase.cos() * 0.3) as f32]
        })
        .collect();
    let mut file = File::create(output).unwrap();
    let mut encoder = VorbisEncoder::new(&mut file, spec).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "Zemer encoder oracle tone");
    encoder.set_metadata(&metadata).unwrap();
    for chunk in input.chunks(2 * 1703) {
        let block = PcmBuffer::new(spec, Samples::F32(chunk.to_vec())).unwrap();
        encoder.write_block(&block).unwrap();
    }
    encoder.finish().unwrap();
}

#[test]
#[ignore = "writes mono/multichannel Vorbis files named PREFIX-CHANNELS.ogg for independent decoding; set ZEMER_VORBIS_ENCODED_PROFILE_PREFIX"]
fn encodes_reference_decoder_channel_profiles() {
    let prefix = std::env::var_os("ZEMER_VORBIS_ENCODED_PROFILE_PREFIX").unwrap();
    let prefix = PathBuf::from(prefix);
    for (sample_rate, channels) in [
        (8_000, 1),
        (44_100, 2),
        (32_000, 3),
        (44_100, 4),
        (44_100, 5),
        (48_000, 6),
        (48_000, 7),
        (96_000, 8),
    ] {
        let path = prefix.with_file_name(format!(
            "{}-{channels}.ogg",
            prefix.file_name().unwrap().to_string_lossy()
        ));
        let spec = AudioSpec::new(sample_rate, channels, SampleFormat::Float32).unwrap();
        let frame_count = 8_193_usize;
        let mono: Vec<f32> = (0..frame_count)
            .map(|frame| {
                (std::f64::consts::TAU * 440.0 * frame as f64 / f64::from(sample_rate)).sin() as f32
                    * 0.4
            })
            .collect();
        let input: Vec<f32> = mono
            .iter()
            .flat_map(|&sample| std::iter::repeat_n(sample, usize::from(channels)))
            .collect();
        let mut file = File::create(path).unwrap();
        let mut encoder = VorbisEncoder::new(&mut file, spec).unwrap();
        for chunk in input.chunks(usize::from(channels) * 1_337) {
            let block = PcmBuffer::new(spec, Samples::F32(chunk.to_vec())).unwrap();
            encoder.write_block(&block).unwrap();
        }
        encoder.finish().unwrap();

        let mut decoder = VorbisDecoder::new(
            File::open(prefix.with_file_name(format!(
                "{}-{channels}.ogg",
                prefix.file_name().unwrap().to_string_lossy()
            )))
            .unwrap(),
        )
        .unwrap();
        let mut actual = Vec::new();
        while let Some(block) = decoder.read_frames(733).unwrap() {
            let Samples::F32(samples) = block.samples() else {
                panic!("Vorbis decoder did not return float32 PCM");
            };
            actual.extend_from_slice(samples);
        }
        assert_eq!(actual.len(), input.len());
        let max_error = actual
            .iter()
            .zip(&input)
            .map(|(actual, expected)| (actual - expected).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            max_error < 0.1,
            "{channels} channels at {sample_rate} Hz: maximum encoder round-trip error {max_error}"
        );
    }
}

#[test]
#[ignore = "writes target-bitrate Vorbis profiles and matching Float32 inputs for external decoder/rate analysis; set ZEMER_VORBIS_RATE_PROFILE_PREFIX"]
fn encodes_target_bitrate_profiles_for_external_validation() {
    let prefix = PathBuf::from(std::env::var_os("ZEMER_VORBIS_RATE_PROFILE_PREFIX").unwrap());
    for (sample_rate, channels, bitrate_kbps) in [
        (32_000, 1, 32_u32),
        (44_100, 2, 96),
        (48_000, 3, 128),
        (48_000, 4, 192),
        (48_000, 5, 256),
        (48_000, 6, 256),
        (48_000, 6, 320),
        (48_000, 7, 384),
        (96_000, 8, 512),
        (192_000, 2, 384),
    ] {
        let spec = AudioSpec::new(sample_rate, channels, SampleFormat::Float32).unwrap();
        let frame_count = sample_rate as usize * 2;
        let mut input = Vec::with_capacity(frame_count * usize::from(channels));
        for frame in 0..frame_count {
            for channel in 0..usize::from(channels) {
                let base_hz = 220.0 + channel as f64 * 73.0;
                let phase = std::f64::consts::TAU * base_hz * frame as f64 / f64::from(sample_rate);
                let harmonic =
                    std::f64::consts::TAU * (base_hz * 2.7) * frame as f64 / f64::from(sample_rate);
                let transient = if frame % (sample_rate as usize / 8) < 96 {
                    0.12
                } else {
                    0.0
                };
                let seed = (frame as u32)
                    .wrapping_mul(1_664_525)
                    .wrapping_add((channel as u32).wrapping_mul(1_013_904_223));
                let noise = ((seed >> 8) as i16 as f32) / 32_768.0 * 0.015;
                input.push((phase.sin() * 0.32 + harmonic.sin() * 0.11) as f32 + transient + noise);
            }
        }

        let stem = format!("{sample_rate}-{channels}-{bitrate_kbps}");
        let prefix_name = prefix.file_name().unwrap().to_string_lossy();
        let pcm_path = prefix.with_file_name(format!("{prefix_name}-{stem}.f32le"));
        let encoded_path = prefix.with_file_name(format!("{prefix_name}-{stem}.ogg"));
        let mut pcm_file = File::create(pcm_path).unwrap();
        for sample in &input {
            pcm_file.write_all(&sample.to_le_bytes()).unwrap();
        }

        let mut encoded_file = File::create(encoded_path).unwrap();
        let mut encoder =
            VorbisEncoder::new_with_target_bitrate(&mut encoded_file, spec, bitrate_kbps * 1000)
                .unwrap();
        for chunk in input.chunks(usize::from(channels) * 4093) {
            encoder
                .write_block(&PcmBuffer::new(spec, Samples::F32(chunk.to_vec())).unwrap())
                .unwrap();
        }
        encoder.finish().unwrap();
    }
}

#[test]
#[ignore = "writes 8/10/12/14/16-bit scalar residue profiles for external decoder checks; set ZEMER_VORBIS_RESIDUE_PROFILE_PREFIX"]
fn encodes_reference_decoder_residue_profiles() {
    let prefix = PathBuf::from(std::env::var_os("ZEMER_VORBIS_RESIDUE_PROFILE_PREFIX").unwrap());
    let spec = AudioSpec::new(44_100, 2, SampleFormat::Float32).unwrap();
    let input: Vec<f32> = (0..12_347)
        .flat_map(|frame| {
            let phase = std::f64::consts::TAU * 440.0 * frame as f64 / 44_100.0;
            [(phase.sin() * 0.7) as f32, (phase.cos() * 0.3) as f32]
        })
        .collect();
    for residue_bits in [8, 10, 12, 14, 16] {
        let path = prefix.with_file_name(format!(
            "{}-{residue_bits}.ogg",
            prefix.file_name().unwrap().to_string_lossy()
        ));
        let mut file = File::create(path).unwrap();
        let mut encoder =
            VorbisEncoder::new_with_residue_bits(&mut file, spec, residue_bits).unwrap();
        for chunk in input.chunks(2 * 1703) {
            let block = PcmBuffer::new(spec, Samples::F32(chunk.to_vec())).unwrap();
            encoder.write_block(&block).unwrap();
        }
        encoder.finish().unwrap();
    }
}

#[test]
#[ignore = "requires an external FFmpeg/libvorbis Ogg reference file in ZEMER_VORBIS_REFERENCE"]
fn parses_external_ogg_vorbis_headers_and_audio_packets() {
    let path = PathBuf::from(std::env::var_os("ZEMER_VORBIS_REFERENCE").unwrap());
    let mut stream = VorbisStreamReader::new(File::open(path).unwrap()).unwrap();
    assert_eq!(stream.identification().channels(), 2);
    assert_eq!(stream.identification().sample_rate(), 44_100);
    assert_eq!(stream.metadata().values("TITLE"), ["Zemer reference tone"]);

    let mut packets = 0;
    let mut decoded_curves = 0;
    while let Some(packet) = stream.next_audio_packet().unwrap() {
        let curves = stream.decode_floor_curves(&packet).unwrap();
        let residues = stream.decode_residue_vectors(&packet).unwrap();
        let spectra = stream.decode_spectrum(&packet).unwrap();
        assert_eq!(curves.len(), 2);
        assert_eq!(residues.len(), 2);
        assert_eq!(spectra.len(), 2);
        assert!(
            curves.iter().flatten().all(|curve| {
                curve.len() <= 4096 && curve.iter().all(|sample| sample.is_finite())
            })
        );
        assert!(residues.iter().all(|vector| {
            vector.len() <= 4096 && vector.iter().all(|sample| sample.is_finite())
        }));
        assert!(spectra.iter().all(|vector| {
            vector.len() <= 4096 && vector.iter().all(|sample| sample.is_finite())
        }));
        decoded_curves += curves.iter().filter(|curve| curve.is_some()).count();
        packets += 1;
    }
    assert!(packets > 0);
    assert!(decoded_curves > 0);
}

#[test]
#[ignore = "requires an external Ogg Vorbis file and f32le PCM oracle"]
fn decodes_external_ogg_vorbis_against_float_pcm_oracle() {
    let input = PathBuf::from(std::env::var_os("ZEMER_VORBIS_REFERENCE").unwrap());
    let oracle = PathBuf::from(std::env::var_os("ZEMER_VORBIS_PCM_ORACLE").unwrap());
    compare_external_pcm(&input, &oracle, None);
}

#[test]
#[ignore = "requires a tab-separated external Ogg Vorbis/PCM oracle manifest"]
fn decodes_external_ogg_vorbis_profiles_against_float_pcm_oracles() {
    let manifest = PathBuf::from(std::env::var_os("ZEMER_VORBIS_CORPUS").unwrap());
    let manifest = fs::read_to_string(manifest).unwrap();
    let mut profiles = 0;
    for (line_number, line) in manifest.lines().enumerate() {
        let columns: Vec<_> = line.split('\t').collect();
        assert_eq!(
            columns.len(),
            5,
            "invalid profile manifest line {}",
            line_number + 1
        );
        let input = PathBuf::from(columns[0]);
        let oracle = PathBuf::from(columns[1]);
        let sample_rate = columns[2].parse().unwrap();
        let channels = columns[3].parse().unwrap();
        let frames = columns[4].parse().unwrap();
        assert!(frames > 0, "profile {} is empty", line_number + 1);
        compare_external_pcm(&input, &oracle, Some((sample_rate, channels, frames)));
        profiles += 1;
    }
    assert!(profiles > 0, "Vorbis PCM oracle manifest has no profiles");
}

#[test]
#[ignore = "requires malformed Xiph Vorbis vectors outside the repository"]
fn rejects_malformed_external_xiph_vectors() {
    for variable in [
        "ZEMER_VORBIS_ONE_ENTRY_TRUNCATED",
        "ZEMER_VORBIS_SINGLEMAP_MISSING_EOS",
        "ZEMER_VORBIS_UNUSED_MODE",
        "ZEMER_VORBIS_COMBUSTION_PAGE_SHARING",
        "ZEMER_VORBIS_MAKE_PAGE_SHARING",
    ] {
        let input = PathBuf::from(std::env::var_os(variable).unwrap());
        let bytes = fs::read(input).unwrap();
        assert!(read_vorbis_to_end(&bytes).is_err(), "{variable}");
    }
}

#[test]
#[ignore = "requires an external chained Ogg Vorbis stream and per-link PCM manifest"]
fn decodes_chained_ogg_vorbis_streams_with_different_specs() {
    let input = PathBuf::from(std::env::var_os("ZEMER_VORBIS_CHAIN").unwrap());
    let manifest = PathBuf::from(std::env::var_os("ZEMER_VORBIS_CHAIN_CORPUS").unwrap());
    let manifest = fs::read_to_string(manifest).unwrap();
    let mut decoder = VorbisDecoder::new(File::open(input).unwrap()).unwrap();
    let mut links = 0;
    for (line_number, line) in manifest.lines().enumerate() {
        let columns: Vec<_> = line.split('\t').collect();
        assert_eq!(
            columns.len(),
            4,
            "invalid link manifest line {}",
            line_number + 1
        );
        let oracle = fs::read(columns[0]).unwrap();
        assert_eq!(oracle.len() % 4, 0);
        let expected: Vec<f32> = oracle
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        let sample_rate = columns[1].parse().unwrap();
        let channels: u16 = columns[2].parse().unwrap();
        let frames: usize = columns[3].parse().unwrap();
        assert!(frames > 0, "link {} is empty", line_number + 1);
        assert_eq!(expected.len(), frames * usize::from(channels));

        let mut actual = Vec::with_capacity(expected.len());
        while actual.len() < expected.len() {
            let remaining_frames = (expected.len() - actual.len()) / usize::from(channels);
            let block = decoder
                .read_frames(remaining_frames.min(733))
                .unwrap()
                .unwrap_or_else(|| panic!("link {} ended early", line_number + 1));
            assert_eq!(block.spec().sample_rate(), sample_rate);
            assert_eq!(block.spec().channels(), channels);
            let Samples::F32(samples) = block.samples() else {
                panic!("Vorbis decoder did not return float32 PCM");
            };
            actual.extend_from_slice(samples);
        }
        assert_pcm_samples_close(&actual, &expected);
        links += 1;
    }
    assert!(links > 0, "Vorbis chain manifest has no links");
    assert!(decoder.read_frames(733).unwrap().is_none());
}

#[test]
#[ignore = "requires an external Ogg Vorbis file and f32le PCM oracle"]
fn rewrites_comments_without_changing_audio_packets_or_decoded_samples() {
    let input = PathBuf::from(std::env::var_os("ZEMER_VORBIS_REFERENCE").unwrap());
    let oracle = PathBuf::from(std::env::var_os("ZEMER_VORBIS_PCM_ORACLE").unwrap());
    let original = fs::read(&input).unwrap();
    let mut metadata = Metadata::default();
    metadata.insert("title", "Rewritten title");
    metadata.insert("artist", "Zemer");
    let mut rewritten = Vec::new();
    rewrite_metadata(Cursor::new(&original), &mut rewritten, &metadata).unwrap();

    let mut original_stream = VorbisStreamReader::new(Cursor::new(&original)).unwrap();
    let mut rewritten_stream = VorbisStreamReader::new(Cursor::new(&rewritten)).unwrap();
    assert_eq!(
        rewritten_stream.metadata().values("title"),
        ["Rewritten title"]
    );
    assert_eq!(rewritten_stream.metadata().values("artist"), ["Zemer"]);
    assert_eq!(
        rewritten_stream.comments().vendor(),
        original_stream.comments().vendor()
    );
    let original_packets: Vec<_> =
        std::iter::from_fn(|| original_stream.next_audio_packet().transpose())
            .map(Result::unwrap)
            .map(|packet| packet.bytes)
            .collect();
    let rewritten_packets: Vec<_> =
        std::iter::from_fn(|| rewritten_stream.next_audio_packet().transpose())
            .map(Result::unwrap)
            .map(|packet| packet.bytes)
            .collect();
    assert!(!original_packets.is_empty());
    assert_eq!(rewritten_packets, original_packets);

    let expected_bytes = fs::read(oracle).unwrap();
    assert_eq!(expected_bytes.len() % 4, 0);
    let expected: Vec<f32> = expected_bytes
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();
    let mut decoder = VorbisDecoder::new(Cursor::new(rewritten)).unwrap();
    let mut actual = Vec::new();
    while let Some(block) = decoder.read_frames(733).unwrap() {
        let Samples::F32(samples) = block.samples() else {
            panic!("Vorbis decoder did not return float32 PCM");
        };
        actual.extend_from_slice(samples);
    }
    assert_pcm_samples_close(&actual, &expected);
}

#[test]
#[ignore = "requires an external chained Ogg Vorbis stream"]
fn rewrites_only_the_first_link_in_a_chained_stream() {
    let input = PathBuf::from(std::env::var_os("ZEMER_VORBIS_CHAIN").unwrap());
    let original = fs::read(input).unwrap();
    let original_links = ogg_links(&original);
    assert!(original_links.len() > 1);

    let mut metadata = Metadata::default();
    metadata.insert("title", "First link title");
    let mut rewritten = Vec::new();
    rewrite_metadata(Cursor::new(&original), &mut rewritten, &metadata).unwrap();
    let rewritten_links = ogg_links(&rewritten);
    assert_eq!(rewritten_links.len(), original_links.len());

    for (index, (original, rewritten)) in original_links.iter().zip(&rewritten_links).enumerate() {
        assert_eq!(rewritten.len(), original.len());
        if index == 0 {
            let comments = VorbisCommentHeader::parse(&rewritten[1]).unwrap();
            assert_eq!(comments.metadata().values("title"), ["First link title"]);
            assert_eq!(
                comments.vendor(),
                VorbisCommentHeader::parse(&original[1]).unwrap().vendor()
            );
            assert_eq!(&rewritten[..1], &original[..1]);
            assert_eq!(&rewritten[2..], &original[2..]);
        } else {
            assert_eq!(rewritten, original, "later link {index} changed");
        }
    }
}

#[test]
fn rewrites_vorbis_comments_spanning_ogg_pages() {
    let spec = AudioSpec::new(44_100, 1, SampleFormat::Float32).unwrap();
    let audio = PcmBuffer::new(spec, Samples::F32(vec![0.25; 257])).unwrap();
    let long_comment = "lyrics ".repeat(10_000);
    let mut metadata = Metadata::default();
    metadata.insert("LYRICS", long_comment.clone());
    let mut original = Vec::new();
    {
        let mut encoder = VorbisEncoder::new(&mut original, spec).unwrap();
        encoder.set_metadata(&metadata).unwrap();
        encoder.write_block(&audio).unwrap();
        encoder.finish().unwrap();
    }

    let mut packets = OggPacketReader::new(Cursor::new(&original));
    let _identification = packets.next_packet().unwrap().unwrap();
    let comment = packets.next_packet().unwrap().unwrap();
    assert!(comment.bytes.len() > 65_025);
    assert_ne!(comment.page_flags & ogg::PAGE_FLAG_CONTINUED, 0);
    assert_eq!(
        VorbisCommentHeader::parse(&comment.bytes)
            .unwrap()
            .metadata()
            .values("LYRICS"),
        [long_comment]
    );

    let mut updated = Metadata::default();
    updated.insert("LYRICS", "Edited lyrics");
    let mut rewritten = Vec::new();
    rewrite_metadata(Cursor::new(&original), &mut rewritten, &updated).unwrap();
    let original_packets = ogg_links(&original);
    let rewritten_packets = ogg_links(&rewritten);
    assert_eq!(&rewritten_packets[0][..1], &original_packets[0][..1]);
    assert_eq!(&rewritten_packets[0][2..], &original_packets[0][2..]);
    let mut decoder = VorbisDecoder::new(Cursor::new(rewritten)).unwrap();
    assert_eq!(decoder.metadata().values("LYRICS"), ["Edited lyrics"]);
    let mut frames = 0;
    while let Some(block) = decoder.read_frames(128).unwrap() {
        frames += block.frames();
    }
    assert_eq!(frames, 257);
}

fn ogg_links(bytes: &[u8]) -> Vec<Vec<Vec<u8>>> {
    let mut stream = OggPacketReader::new(Cursor::new(bytes));
    let mut links = Vec::new();
    loop {
        let mut packets = Vec::new();
        while let Some(packet) = stream.next_packet().unwrap() {
            packets.push(packet.bytes);
        }
        links.push(packets);
        match stream.next_stream().unwrap() {
            Some(next) => stream = next,
            None => return links,
        }
    }
}

fn compare_external_pcm(
    input: &std::path::Path,
    oracle: &std::path::Path,
    expected_profile: Option<(u32, u16, usize)>,
) {
    let expected_bytes = fs::read(oracle).unwrap();
    assert_eq!(expected_bytes.len() % 4, 0);
    let expected: Vec<f32> = expected_bytes
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
        .collect();

    let mut decoder = VorbisDecoder::new(File::open(input).unwrap()).unwrap_or_else(|error| {
        panic!(
            "could not initialize Vorbis decoder for {}: {error}",
            input.display()
        )
    });
    let spec = decoder.spec();
    if let Some((sample_rate, channels, frames)) = expected_profile {
        assert_eq!(spec.sample_rate(), sample_rate);
        assert_eq!(spec.channels(), channels);
        assert_eq!(expected.len(), frames * usize::from(channels));
    }
    let mut actual = Vec::new();
    while let Some(block) = decoder.read_frames(733).unwrap_or_else(|error| {
        panic!(
            "Vorbis decode failed for {} after {} samples: {error}",
            input.display(),
            actual.len()
        )
    }) {
        let Samples::F32(samples) = block.samples() else {
            panic!("Vorbis decoder did not return float32 PCM");
        };
        actual.extend_from_slice(samples);
    }
    assert_eq!(
        actual.len(),
        expected.len(),
        "decoded sample count differs for {}",
        input.display()
    );
    assert_eq!(actual.len() % usize::from(spec.channels()), 0);
    if let Some((_, channels, frames)) = expected_profile {
        assert_eq!(actual.len(), frames * usize::from(channels));
    }
    assert_pcm_samples_close(&actual, &expected);
}

fn assert_pcm_samples_close(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len(), "decoded sample count differs");
    let (max_index, max_error) = actual
        .iter()
        .zip(expected)
        .enumerate()
        .map(|(index, (actual, expected))| (index, (actual - expected).abs()))
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .unwrap_or((0, 0.0));
    assert!(
        max_error < 0.002,
        "maximum PCM error {max_error} at sample {max_index}: got {}, expected {}; first actual {:?}; first expected {:?}",
        actual[max_index],
        expected[max_index],
        &actual[..actual.len().min(8)],
        &expected[..expected.len().min(8)],
    );
}
