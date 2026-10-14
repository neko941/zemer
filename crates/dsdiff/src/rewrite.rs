use std::io::{Read, Seek, SeekFrom, Write};

use zemer_core::{AudioError, DsdDecoder, Metadata};

use crate::{
    DsdiffComment, DsdiffDecoder,
    decode::{Chunk, read_chunk_header},
    encode::{MAX_UNTIMED_COMMENTS, validate_comments},
    marker::MAX_TEXT_BYTES,
};

/// Rewrites text metadata from a complete metadata snapshot, preserving audio and unknown chunks.
pub fn rewrite_metadata<R: Read + Seek, W: Write + Seek>(
    mut input: R,
    output: &mut W,
    metadata: &Metadata,
) -> Result<(), AudioError> {
    let input_start = input.stream_position()?;
    let decoder = DsdiffDecoder::new(&mut input)?;
    let original = decoder.metadata().clone();
    let old_comments = decoder.comments().to_vec();
    let channels = usize::from(decoder.dsd_spec().channels());
    drop(decoder);

    validate_metadata(metadata, &original)?;
    let comments = comments_after_edit(metadata, &original, &old_comments, channels)?;
    let comments_changed = metadata.values("COMMENT") != original.values("COMMENT");
    let title = metadata.values("TITLE").first().map(String::as_str);
    let artist = metadata.values("ARTIST").first().map(String::as_str);
    let tags_changed = metadata.values("TITLE") != original.values("TITLE")
        || metadata.values("ARTIST") != original.values("ARTIST");
    let input_end = input.seek(SeekFrom::End(0))?;
    input.seek(SeekFrom::Start(input_start))?;
    let mut header = [0; 12];
    input.read_exact(&mut header)?;
    let form_size = u64::from_be_bytes(header[4..12].try_into().unwrap());
    let form_end = input_start
        .checked_add(12)
        .and_then(|position| position.checked_add(form_size))
        .filter(|&end| end == input_end)
        .ok_or(AudioError::InvalidFormat("invalid DSDIFF FORM size"))?;

    if output.stream_position()? != 0 || output.seek(SeekFrom::End(0))? != 0 {
        return Err(AudioError::InvalidData(
            "DSDIFF metadata rewrite requires an empty writer at position zero",
        ));
    }
    output.seek(SeekFrom::Start(0))?;
    output.write_all(b"FRM8\0\0\0\0\0\0\0\0DSD ")?;

    let mut saw_diin = false;
    let mut saw_comt = false;
    let mut position = input_start
        .checked_add(16)
        .ok_or(AudioError::InvalidData("DSDIFF chunk position overflow"))?;
    while position < form_end {
        input.seek(SeekFrom::Start(position))?;
        let chunk = read_chunk_header(&mut input, form_end)?;
        match &chunk.id {
            b"DIIN" => {
                saw_diin = true;
                if tags_changed {
                    rewrite_diin(&mut input, output, chunk, title, artist)?;
                } else {
                    copy_range(&mut input, output, position, chunk.padded_end)?;
                }
            }
            b"COMT" => {
                saw_comt = true;
                if comments_changed {
                    write_comments(output, &comments)?;
                } else {
                    copy_range(&mut input, output, position, chunk.padded_end)?;
                }
            }
            _ => copy_range(&mut input, output, position, chunk.padded_end)?,
        }
        position = chunk.padded_end;
    }
    if !saw_diin && (title.is_some() || artist.is_some()) {
        write_diin(output, title, artist)?;
    }
    if !saw_comt && comments_changed && !comments.is_empty() {
        write_comments(output, &comments)?;
    }

    let output_end = output.stream_position()?;
    let output_form_size = output_end
        .checked_sub(12)
        .ok_or(AudioError::InvalidData("DSDIFF output size underflow"))?;
    output.seek(SeekFrom::Start(4))?;
    output.write_all(&output_form_size.to_be_bytes())?;
    output.seek(SeekFrom::Start(output_end))?;
    output.flush()?;
    Ok(())
}

fn validate_metadata(metadata: &Metadata, original: &Metadata) -> Result<(), AudioError> {
    for (key, values) in metadata.iter() {
        if !matches!(key, "TITLE" | "ARTIST" | "COMMENT" | "DSDIFF_CHANNEL_ID") {
            return Err(AudioError::Unsupported(
                "DSDIFF metadata editing supports title, artist, and comments",
            ));
        }
        if values.iter().any(|value| {
            value.len() > MAX_TEXT_BYTES || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        }) {
            return Err(AudioError::InvalidData(
                "DSDIFF text metadata must be printable ASCII",
            ));
        }
    }
    if metadata.values("TITLE").len() > 1 || metadata.values("ARTIST").len() > 1 {
        return Err(AudioError::Unsupported(
            "DSDIFF DIIN stores at most one title and artist",
        ));
    }
    if metadata.values("DSDIFF_CHANNEL_ID") != original.values("DSDIFF_CHANNEL_ID") {
        return Err(AudioError::Unsupported(
            "DSDIFF metadata editing cannot change channel IDs",
        ));
    }
    Ok(())
}

fn comments_after_edit(
    metadata: &Metadata,
    original: &Metadata,
    old_comments: &[DsdiffComment],
    channels: usize,
) -> Result<Vec<DsdiffComment>, AudioError> {
    let values = metadata.values("COMMENT");
    if values == original.values("COMMENT") {
        return Ok(old_comments.to_vec());
    }
    if values.len() > MAX_UNTIMED_COMMENTS {
        return Err(AudioError::InvalidData("too many DSDIFF comments"));
    }
    let mut comments = Vec::new();
    let capacity = values
        .len()
        .checked_add(old_comments.len())
        .ok_or(AudioError::InvalidData("DSDIFF comments are too large"))?;
    comments
        .try_reserve_exact(capacity)
        .map_err(|_| AudioError::InvalidData("DSDIFF comments are too large"))?;
    let mut new_general = values.iter();
    for old in old_comments {
        if old.kind == 0 {
            if let Some(text) = new_general.next() {
                let mut comment = old.clone();
                comment.text.clone_from(text);
                comments.push(comment);
            }
        } else {
            comments.push(old.clone());
        }
    }
    comments.extend(new_general.map(|text| DsdiffComment {
        year: 0,
        month: 0,
        day: 0,
        hour: 0,
        minute: 0,
        kind: 0,
        reference: 0,
        text: text.clone(),
    }));
    validate_comments(&comments, channels)?;
    Ok(comments)
}

fn rewrite_diin<R: Read + Seek, W: Write + Seek>(
    input: &mut R,
    output: &mut W,
    chunk: Chunk,
    title: Option<&str>,
    artist: Option<&str>,
) -> Result<(), AudioError> {
    let header_position = output.stream_position()?;
    write_chunk_header(output, *b"DIIN", 0)?;
    let payload_start = output.stream_position()?;
    let mut saw_title = false;
    let mut saw_artist = false;
    let mut position = chunk.payload_start;
    while position < chunk.end {
        input.seek(SeekFrom::Start(position))?;
        let inner = read_chunk_header(input, chunk.end)?;
        match &inner.id {
            b"DITI" if !saw_title => {
                saw_title = true;
                if let Some(title) = title {
                    write_text_chunk(output, *b"DITI", title)?;
                }
            }
            b"DIAR" if !saw_artist => {
                saw_artist = true;
                if let Some(artist) = artist {
                    write_text_chunk(output, *b"DIAR", artist)?;
                }
            }
            b"DITI" | b"DIAR" => {}
            _ => copy_range(input, output, position, inner.padded_end)?,
        }
        position = inner.padded_end;
    }
    if !saw_artist {
        if let Some(artist) = artist {
            write_text_chunk(output, *b"DIAR", artist)?;
        }
    }
    if !saw_title {
        if let Some(title) = title {
            write_text_chunk(output, *b"DITI", title)?;
        }
    }
    finish_chunk(output, header_position, payload_start)?;
    Ok(())
}

fn write_diin<W: Write + Seek>(
    output: &mut W,
    title: Option<&str>,
    artist: Option<&str>,
) -> Result<(), AudioError> {
    let header_position = output.stream_position()?;
    write_chunk_header(output, *b"DIIN", 0)?;
    let payload_start = output.stream_position()?;
    if let Some(artist) = artist {
        write_text_chunk(output, *b"DIAR", artist)?;
    }
    if let Some(title) = title {
        write_text_chunk(output, *b"DITI", title)?;
    }
    finish_chunk(output, header_position, payload_start)
}

fn write_text_chunk<W: Write>(output: &mut W, id: [u8; 4], text: &str) -> Result<(), AudioError> {
    let text_size = u32::try_from(text.len())
        .map_err(|_| AudioError::InvalidData("DSDIFF text is too large"))?;
    let chunk_size = u64::from(text_size) + 4;
    write_chunk_header(output, id, chunk_size)?;
    output.write_all(&text_size.to_be_bytes())?;
    output.write_all(text.as_bytes())?;
    if chunk_size & 1 != 0 {
        output.write_all(&[0])?;
    }
    Ok(())
}

fn write_comments<W: Write + Seek>(
    output: &mut W,
    comments: &[DsdiffComment],
) -> Result<(), AudioError> {
    if comments.is_empty() {
        return Ok(());
    }
    let payload_size = comments.iter().try_fold(2_u64, |size, comment| {
        size.checked_add(14)
            .and_then(|size| size.checked_add(comment.text.len() as u64))
            .and_then(|size| size.checked_add((comment.text.len() & 1) as u64))
            .ok_or(AudioError::InvalidData("DSDIFF comment size overflow"))
    })?;
    let count = u16::try_from(comments.len())
        .map_err(|_| AudioError::InvalidData("too many DSDIFF comments"))?;
    write_chunk_header(output, *b"COMT", payload_size)?;
    output.write_all(&count.to_be_bytes())?;
    for comment in comments {
        output.write_all(&comment.year.to_be_bytes())?;
        output.write_all(&[comment.month, comment.day, comment.hour, comment.minute])?;
        output.write_all(&comment.kind.to_be_bytes())?;
        output.write_all(&comment.reference.to_be_bytes())?;
        output.write_all(&(comment.text.len() as u32).to_be_bytes())?;
        output.write_all(comment.text.as_bytes())?;
        if comment.text.len() & 1 != 0 {
            output.write_all(&[0])?;
        }
    }
    if payload_size & 1 != 0 {
        output.write_all(&[0])?;
    }
    Ok(())
}

fn finish_chunk<W: Write + Seek>(
    output: &mut W,
    header_position: u64,
    payload_start: u64,
) -> Result<(), AudioError> {
    let end = output.stream_position()?;
    let size = end
        .checked_sub(payload_start)
        .ok_or(AudioError::InvalidData("DSDIFF chunk size underflow"))?;
    if size & 1 != 0 {
        output.write_all(&[0])?;
    }
    output.seek(SeekFrom::Start(header_position + 4))?;
    output.write_all(&size.to_be_bytes())?;
    output.seek(SeekFrom::End(0))?;
    Ok(())
}

fn write_chunk_header<W: Write>(output: &mut W, id: [u8; 4], size: u64) -> Result<(), AudioError> {
    output.write_all(&id)?;
    output.write_all(&size.to_be_bytes())?;
    Ok(())
}

fn copy_range<R: Read + Seek, W: Write>(
    input: &mut R,
    output: &mut W,
    start: u64,
    end: u64,
) -> Result<(), AudioError> {
    input.seek(SeekFrom::Start(start))?;
    let mut limited = input.take(end - start);
    let copied = std::io::copy(&mut limited, output)?;
    if copied != end - start {
        return Err(AudioError::InvalidFormat("truncated DSDIFF chunk"));
    }
    Ok(())
}
