use std::fmt;

const MAX_FRAME_BYTES: usize = 1275;
const MAX_PACKET_DURATION_48K: u16 = 5760;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Toc {
    pub configuration: u8,
    pub stereo: bool,
    pub frame_code: u8,
    pub frame_samples_48k: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Silk,
    Hybrid,
    Celt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bandwidth {
    Narrow,
    Medium,
    Wide,
    Superwide,
    Full,
}

impl Toc {
    fn parse(byte: u8) -> Self {
        let configuration = byte >> 3;
        let frame_samples_48k = match configuration {
            0..=11 => [480, 960, 1920, 2880][usize::from(configuration % 4)],
            12..=15 => [480, 960][usize::from(configuration % 2)],
            16..=31 => [120, 240, 480, 960][usize::from(configuration % 4)],
            _ => unreachable!("configuration is five bits"),
        };
        Self {
            configuration,
            stereo: byte & 0x04 != 0,
            frame_code: byte & 0x03,
            frame_samples_48k,
        }
    }

    pub const fn mode(self) -> Mode {
        match self.configuration {
            0..=11 => Mode::Silk,
            12..=15 => Mode::Hybrid,
            _ => Mode::Celt,
        }
    }

    pub const fn bandwidth(self) -> Bandwidth {
        match self.configuration {
            0..=3 | 16..=19 => Bandwidth::Narrow,
            4..=7 => Bandwidth::Medium,
            8..=11 | 20..=23 => Bandwidth::Wide,
            12..=13 | 24..=27 => Bandwidth::Superwide,
            _ => Bandwidth::Full,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpusPacket<'a> {
    toc: Toc,
    frames: Vec<&'a [u8]>,
}

impl<'a> OpusPacket<'a> {
    pub fn parse(packet: &'a [u8]) -> Result<Self, PacketError> {
        let toc_byte = *packet.first().ok_or(PacketError::Empty)?;
        let toc = Toc::parse(toc_byte);
        let mut frames = Vec::new();
        match toc.frame_code {
            0 => {
                frames
                    .try_reserve_exact(1)
                    .map_err(|_| PacketError::AllocationFailed)?;
                let frame = &packet[1..];
                check_frame_size(frame.len())?;
                frames.push(frame);
            }
            1 => {
                frames
                    .try_reserve_exact(2)
                    .map_err(|_| PacketError::AllocationFailed)?;
                let payload = &packet[1..];
                if payload.len() % 2 != 0 {
                    return Err(PacketError::InvalidFrameLayout);
                }
                let frame_bytes = payload.len() / 2;
                check_frame_size(frame_bytes)?;
                frames.push(&payload[..frame_bytes]);
                frames.push(&payload[frame_bytes..]);
            }
            2 => {
                frames
                    .try_reserve_exact(2)
                    .map_err(|_| PacketError::AllocationFailed)?;
                let mut cursor = 1;
                let first_bytes = read_frame_size(packet, &mut cursor)?;
                let remaining = packet.len() - cursor;
                if first_bytes > remaining {
                    return Err(PacketError::InvalidFrameLayout);
                }
                let second_bytes = remaining - first_bytes;
                check_frame_size(first_bytes)?;
                check_frame_size(second_bytes)?;
                frames.push(&packet[cursor..cursor + first_bytes]);
                frames.push(&packet[cursor + first_bytes..]);
            }
            3 => parse_code3(packet, toc, &mut frames)?,
            _ => unreachable!("frame code is two bits"),
        }
        let total_samples = toc
            .frame_samples_48k
            .checked_mul(u16::try_from(frames.len()).map_err(|_| PacketError::InvalidFrameCount)?)
            .ok_or(PacketError::DurationTooLong)?;
        if total_samples > MAX_PACKET_DURATION_48K {
            return Err(PacketError::DurationTooLong);
        }
        Ok(Self { toc, frames })
    }

    /// Parses one self-delimited Opus packet from the start of `packet`.
    /// Returns the packet and the number of bytes it consumed.
    pub fn parse_self_delimited(packet: &'a [u8]) -> Result<(Self, usize), PacketError> {
        let toc = Toc::parse(*packet.first().ok_or(PacketError::Empty)?);
        let mut cursor = 1;
        let mut frames = Vec::new();

        match toc.frame_code {
            0 => {
                frames
                    .try_reserve_exact(1)
                    .map_err(|_| PacketError::AllocationFailed)?;
                let size = read_frame_size(packet, &mut cursor)?;
                check_frame_size(size)?;
                let end = cursor
                    .checked_add(size)
                    .ok_or(PacketError::InvalidFrameLayout)?;
                frames.push(packet.get(cursor..end).ok_or(PacketError::Truncated)?);
                cursor = end;
            }
            1 => {
                let size = read_frame_size(packet, &mut cursor)?;
                check_frame_size(size)?;
                frames
                    .try_reserve_exact(2)
                    .map_err(|_| PacketError::AllocationFailed)?;
                for _ in 0..2 {
                    let end = cursor
                        .checked_add(size)
                        .ok_or(PacketError::InvalidFrameLayout)?;
                    frames.push(packet.get(cursor..end).ok_or(PacketError::Truncated)?);
                    cursor = end;
                }
            }
            2 => {
                let first_size = read_frame_size(packet, &mut cursor)?;
                let second_size = read_frame_size(packet, &mut cursor)?;
                check_frame_size(first_size)?;
                check_frame_size(second_size)?;
                frames
                    .try_reserve_exact(2)
                    .map_err(|_| PacketError::AllocationFailed)?;
                for size in [first_size, second_size] {
                    let end = cursor
                        .checked_add(size)
                        .ok_or(PacketError::InvalidFrameLayout)?;
                    frames.push(packet.get(cursor..end).ok_or(PacketError::Truncated)?);
                    cursor = end;
                }
            }
            3 => {
                let control = *packet.get(cursor).ok_or(PacketError::Truncated)?;
                cursor += 1;
                let frame_count = usize::from(control >> 2);
                if frame_count == 0 {
                    return Err(PacketError::InvalidFrameCount);
                }
                check_duration(toc, frame_count)?;
                let padding = if control & 0x02 != 0 {
                    read_padding_size(packet, &mut cursor)?
                } else {
                    0
                };
                frames
                    .try_reserve_exact(frame_count)
                    .map_err(|_| PacketError::AllocationFailed)?;

                let mut sizes = Vec::new();
                sizes
                    .try_reserve_exact(frame_count)
                    .map_err(|_| PacketError::AllocationFailed)?;
                if control & 0x01 == 0 {
                    let size = read_frame_size(packet, &mut cursor)?;
                    check_frame_size(size)?;
                    sizes.resize(frame_count, size);
                } else {
                    for _ in 0..frame_count {
                        let size = read_frame_size(packet, &mut cursor)?;
                        check_frame_size(size)?;
                        sizes.push(size);
                    }
                }

                for size in sizes {
                    let end = cursor
                        .checked_add(size)
                        .ok_or(PacketError::InvalidFrameLayout)?;
                    frames.push(packet.get(cursor..end).ok_or(PacketError::Truncated)?);
                    cursor = end;
                }
                cursor = cursor
                    .checked_add(padding)
                    .ok_or(PacketError::InvalidPadding)?;
                if cursor > packet.len() {
                    return Err(PacketError::Truncated);
                }
            }
            _ => unreachable!("frame code is two bits"),
        }

        check_duration(toc, frames.len())?;
        Ok((Self { toc, frames }, cursor))
    }

    pub const fn toc(&self) -> Toc {
        self.toc
    }

    pub fn frames(&self) -> &[&'a [u8]] {
        &self.frames
    }

    pub fn duration_samples_48k(&self) -> usize {
        usize::from(self.toc.frame_samples_48k) * self.frames.len()
    }
}

/// Splits an Ogg Opus audio packet into its constituent elementary streams.
/// The first `stream_count - 1` packets use RFC 6716 self-delimited framing.
pub fn parse_multistream_packet<'a>(
    packet: &'a [u8],
    stream_count: u8,
) -> Result<Vec<OpusPacket<'a>>, PacketError> {
    if stream_count == 0 {
        return Err(PacketError::InvalidFrameCount);
    }
    let mut streams = Vec::new();
    streams
        .try_reserve_exact(usize::from(stream_count))
        .map_err(|_| PacketError::AllocationFailed)?;
    let mut cursor = 0;
    for _ in 1..stream_count {
        let (stream, consumed) = OpusPacket::parse_self_delimited(&packet[cursor..])?;
        cursor = cursor
            .checked_add(consumed)
            .ok_or(PacketError::InvalidFrameLayout)?;
        streams.push(stream);
    }
    let final_stream = OpusPacket::parse(&packet[cursor..])?;
    if streams
        .first()
        .is_some_and(|first| first.duration_samples_48k() != final_stream.duration_samples_48k())
    {
        return Err(PacketError::DurationMismatch);
    }
    streams.push(final_stream);
    if streams
        .windows(2)
        .any(|pair| pair[0].duration_samples_48k() != pair[1].duration_samples_48k())
    {
        return Err(PacketError::DurationMismatch);
    }
    Ok(streams)
}

fn check_duration(toc: Toc, frame_count: usize) -> Result<(), PacketError> {
    let count = u16::try_from(frame_count).map_err(|_| PacketError::InvalidFrameCount)?;
    let duration = toc
        .frame_samples_48k
        .checked_mul(count)
        .ok_or(PacketError::DurationTooLong)?;
    if duration > MAX_PACKET_DURATION_48K {
        return Err(PacketError::DurationTooLong);
    }
    Ok(())
}

fn read_padding_size(packet: &[u8], cursor: &mut usize) -> Result<usize, PacketError> {
    let mut total = 0_usize;
    let mut value = usize::from(*packet.get(*cursor).ok_or(PacketError::Truncated)?);
    *cursor += 1;
    while value == 255 {
        total = total.checked_add(254).ok_or(PacketError::InvalidPadding)?;
        value = usize::from(*packet.get(*cursor).ok_or(PacketError::Truncated)?);
        *cursor += 1;
    }
    total.checked_add(value).ok_or(PacketError::InvalidPadding)
}

fn parse_code3<'a>(
    packet: &'a [u8],
    toc: Toc,
    frames: &mut Vec<&'a [u8]>,
) -> Result<(), PacketError> {
    let control = *packet.get(1).ok_or(PacketError::Truncated)?;
    let frame_count = usize::from(control >> 2);
    if frame_count == 0 {
        return Err(PacketError::InvalidFrameCount);
    }
    let total_samples = toc
        .frame_samples_48k
        .checked_mul(u16::try_from(frame_count).map_err(|_| PacketError::DurationTooLong)?)
        .ok_or(PacketError::DurationTooLong)?;
    if total_samples > MAX_PACKET_DURATION_48K {
        return Err(PacketError::DurationTooLong);
    }

    let mut cursor = 2;
    let padding_bytes = if control & 0x02 != 0 {
        read_padding_size(packet, &mut cursor)?
    } else {
        0
    };
    let data_end = packet
        .len()
        .checked_sub(padding_bytes)
        .filter(|end| *end >= cursor)
        .ok_or(PacketError::InvalidPadding)?;
    frames
        .try_reserve_exact(frame_count)
        .map_err(|_| PacketError::AllocationFailed)?;

    if control & 0x01 == 0 {
        let payload_bytes = data_end - cursor;
        if payload_bytes % frame_count != 0 {
            return Err(PacketError::InvalidFrameLayout);
        }
        let frame_bytes = payload_bytes / frame_count;
        check_frame_size(frame_bytes)?;
        for _ in 0..frame_count {
            frames.push(&packet[cursor..cursor + frame_bytes]);
            cursor += frame_bytes;
        }
    } else {
        let mut sizes = Vec::new();
        sizes
            .try_reserve_exact(frame_count)
            .map_err(|_| PacketError::AllocationFailed)?;
        let mut preceding_bytes = 0_usize;
        for _ in 0..frame_count - 1 {
            let frame_bytes = read_frame_size(&packet[..data_end], &mut cursor)?;
            check_frame_size(frame_bytes)?;
            preceding_bytes = preceding_bytes
                .checked_add(frame_bytes)
                .ok_or(PacketError::InvalidFrameLayout)?;
            sizes.push(frame_bytes);
        }
        let last_bytes = data_end
            .checked_sub(cursor)
            .and_then(|bytes| bytes.checked_sub(preceding_bytes))
            .ok_or(PacketError::InvalidFrameLayout)?;
        check_frame_size(last_bytes)?;
        sizes.push(last_bytes);
        for frame_bytes in sizes {
            let end = cursor + frame_bytes;
            frames.push(&packet[cursor..end]);
            cursor = end;
        }
    }
    Ok(())
}

fn read_frame_size(packet: &[u8], cursor: &mut usize) -> Result<usize, PacketError> {
    let first = usize::from(*packet.get(*cursor).ok_or(PacketError::Truncated)?);
    *cursor += 1;
    if first < 252 {
        return Ok(first);
    }
    let second = usize::from(*packet.get(*cursor).ok_or(PacketError::Truncated)?);
    *cursor += 1;
    first
        .checked_add(4 * second)
        .filter(|size| *size <= MAX_FRAME_BYTES)
        .ok_or(PacketError::InvalidFrameLayout)
}

fn check_frame_size(size: usize) -> Result<(), PacketError> {
    if size > MAX_FRAME_BYTES {
        return Err(PacketError::FrameTooLarge);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketError {
    Empty,
    Truncated,
    InvalidFrameLayout,
    InvalidPadding,
    FrameTooLarge,
    InvalidFrameCount,
    DurationTooLong,
    AllocationFailed,
    DurationMismatch,
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "empty Opus packet",
            Self::Truncated => "truncated Opus packet framing",
            Self::InvalidFrameLayout => "invalid Opus frame layout",
            Self::InvalidPadding => "invalid Opus packet padding",
            Self::FrameTooLarge => "Opus frame exceeds 1275 bytes",
            Self::InvalidFrameCount => "invalid Opus packet frame count",
            Self::DurationTooLong => "Opus packet duration exceeds 120 ms",
            Self::AllocationFailed => "unable to allocate Opus frame table",
            Self::DurationMismatch => "Opus multistream packet durations do not match",
        })
    }
}

impl std::error::Error for PacketError {}

#[cfg(test)]
mod tests {
    use super::{Bandwidth, Mode, OpusPacket, PacketError, Toc, parse_multistream_packet};

    #[test]
    fn toc_configurations_select_mode_and_bandwidth() {
        let ranges = [
            (0..=3, Mode::Silk, Bandwidth::Narrow),
            (4..=7, Mode::Silk, Bandwidth::Medium),
            (8..=11, Mode::Silk, Bandwidth::Wide),
            (12..=13, Mode::Hybrid, Bandwidth::Superwide),
            (14..=15, Mode::Hybrid, Bandwidth::Full),
            (16..=19, Mode::Celt, Bandwidth::Narrow),
            (20..=23, Mode::Celt, Bandwidth::Wide),
            (24..=27, Mode::Celt, Bandwidth::Superwide),
            (28..=31, Mode::Celt, Bandwidth::Full),
        ];
        for (configurations, mode, bandwidth) in ranges {
            for configuration in configurations {
                let toc = Toc::parse(configuration << 3);
                assert_eq!((toc.mode(), toc.bandwidth()), (mode, bandwidth));
            }
        }
    }

    #[test]
    fn toc_configuration_maps_all_32_frame_durations() {
        let expected = [
            480, 960, 1920, 2880, 480, 960, 1920, 2880, 480, 960, 1920, 2880, 480, 960, 480, 960,
            120, 240, 480, 960, 120, 240, 480, 960, 120, 240, 480, 960, 120, 240, 480, 960,
        ];
        for (configuration, &samples) in expected.iter().enumerate() {
            let toc = Toc::parse((configuration as u8) << 3);
            assert_eq!(toc.frame_samples_48k, samples);
        }
    }

    #[test]
    fn parses_all_four_frame_packing_codes() {
        let code0 = OpusPacket::parse(&[0x80, 1, 2]).unwrap();
        assert_eq!(code0.frames, [&[1, 2][..]]);

        let code1 = OpusPacket::parse(&[0x81, 1, 2, 3, 4]).unwrap();
        assert_eq!(code1.frames, [&[1, 2][..], &[3, 4][..]]);

        let code2 = OpusPacket::parse(&[0x82, 2, 1, 2, 3]).unwrap();
        assert_eq!(code2.frames, [&[1, 2][..], &[3][..]]);

        let code3_cbr = OpusPacket::parse(&[0x83, 8, 1, 2, 3, 4]).unwrap();
        assert_eq!(code3_cbr.frames, [&[1, 2][..], &[3, 4][..]]);

        let code3_vbr = OpusPacket::parse(&[0x83, 9, 2, 1, 2, 3]).unwrap();
        assert_eq!(code3_vbr.frames, [&[1, 2][..], &[3][..]]);
    }

    #[test]
    fn parses_self_delimited_frame_packing_codes_and_consumed_lengths() {
        let (code0, consumed) = OpusPacket::parse_self_delimited(&[0x80, 2, 10, 11, 99]).unwrap();
        assert_eq!(code0.frames(), &[&[10, 11][..]]);
        assert_eq!(consumed, 4);

        let (code1, consumed) = OpusPacket::parse_self_delimited(&[0x81, 1, 10, 11, 99]).unwrap();
        assert_eq!(code1.frames(), &[&[10][..], &[11][..]]);
        assert_eq!(consumed, 4);

        let (code2, consumed) =
            OpusPacket::parse_self_delimited(&[0x82, 1, 2, 10, 11, 12, 99]).unwrap();
        assert_eq!(code2.frames(), &[&[10][..], &[11, 12][..]]);
        assert_eq!(consumed, 6);

        let (code3_cbr, consumed) =
            OpusPacket::parse_self_delimited(&[0x83, 8, 2, 10, 11, 12, 13, 99]).unwrap();
        assert_eq!(code3_cbr.frames(), &[&[10, 11][..], &[12, 13][..]]);
        assert_eq!(consumed, 7);

        let (code3_vbr, consumed) =
            OpusPacket::parse_self_delimited(&[0x83, 9, 1, 2, 10, 11, 12, 99]).unwrap();
        assert_eq!(code3_vbr.frames(), &[&[10][..], &[11, 12][..]]);
        assert_eq!(consumed, 7);
    }

    #[test]
    fn self_delimited_code3_consumes_declared_padding() {
        let (packet, consumed) =
            OpusPacket::parse_self_delimited(&[0x83, 10, 2, 1, 10, 11, 12, 13, 99]).unwrap();
        assert_eq!(packet.frames(), &[&[10][..], &[11][..]]);
        assert_eq!(consumed, 8);
    }

    #[test]
    fn splits_multistream_packet_and_checks_equal_durations() {
        let streams = parse_multistream_packet(&[0x80, 1, 10, 0x80, 11], 2).unwrap();
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].frames(), &[&[10][..]]);
        assert_eq!(streams[1].frames(), &[&[11][..]]);

        assert_eq!(
            parse_multistream_packet(&[0x80, 1, 10, 0x88, 11], 2),
            Err(PacketError::DurationMismatch)
        );
    }

    #[test]
    fn rejects_truncated_self_delimited_and_empty_multistream_packets() {
        assert_eq!(
            OpusPacket::parse_self_delimited(&[0x80, 2, 1]).unwrap_err(),
            PacketError::Truncated
        );
        assert_eq!(
            parse_multistream_packet(&[0x80], 2).unwrap_err(),
            PacketError::Truncated
        );
        assert_eq!(
            parse_multistream_packet(&[0x80], 0).unwrap_err(),
            PacketError::InvalidFrameCount
        );
    }

    #[test]
    fn parses_extended_frame_lengths_and_opus_padding() {
        let mut packet = vec![0x82, 252, 0];
        packet.extend(vec![7; 252]);
        packet.push(9);
        let parsed = OpusPacket::parse(&packet).unwrap();
        assert_eq!(parsed.frames[0].len(), 252);
        assert_eq!(parsed.frames[1], [9]);

        let padded = OpusPacket::parse(&[0x83, 6, 2, 8, 9, 0xaa, 0xbb]).unwrap();
        assert_eq!(padded.frames, [&[8, 9][..]]);

        let mut long_padding = vec![0x83, 6, 255, 255, 2, 0x42];
        long_padding.extend([0xcc; 510]);
        let padded = OpusPacket::parse(&long_padding).unwrap();
        assert_eq!(padded.frames, [&[0x42][..]]);
    }

    #[test]
    fn enforces_the_1275_byte_implicit_frame_limit() {
        let mut packet = vec![0x80];
        packet.extend([0x55; 1275]);
        assert_eq!(OpusPacket::parse(&packet).unwrap().frames[0].len(), 1275);
        packet.push(0);
        assert_eq!(OpusPacket::parse(&packet), Err(PacketError::FrameTooLarge));
    }

    #[test]
    fn rejects_malformed_lengths_counts_durations_and_padding() {
        for invalid in [
            &[][..],
            &[0x81, 1][..],
            &[0x82][..],
            &[0x82, 252][..],
            &[0x83, 0][..],
            &[0x83, 1][..],
            &[0x83, 2, 5][..],
            &[0x83, 9, 3, 1, 2][..],
            &[0x83, 0xF9][..],
        ] {
            assert!(OpusPacket::parse(invalid).is_err(), "{invalid:?}");
        }
        assert_eq!(
            OpusPacket::parse(&[0x82, 3, 1]).unwrap_err(),
            PacketError::InvalidFrameLayout
        );
    }

    #[test]
    fn packet_code_duration_is_bounded_to_120_ms() {
        let mut packet = vec![0xf3, 49 << 2];
        packet.extend([0; 49]);
        assert_eq!(
            OpusPacket::parse(&packet),
            Err(PacketError::DurationTooLong)
        );
        assert_eq!(
            OpusPacket::parse(&[0x83, 8, 0, 0]),
            Ok(OpusPacket {
                toc: Toc::parse(0x83),
                frames: vec![&[0][..], &[0][..]],
            })
        );
    }
}
