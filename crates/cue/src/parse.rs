use crate::{CUEFile, CUETimeStamp, CUETrack, TrackFlags};

pub(super) fn parse_cue(input: &str) -> Result<CUEFile, String> {
    let mut sheet = CUEFile::new();
    let mut current_file = None;
    let mut current_track = None;

    for (line_index, raw_line) in input.lines().enumerate() {
        let line_number = line_index + 1;
        let line = raw_line.trim().trim_start_matches('\u{feff}').trim();
        if line.is_empty() {
            continue;
        }
        let (command, rest) =
            split_command(line).ok_or_else(|| at_line(line_number, "expected a CUE command"))?;
        let command = command.to_ascii_uppercase();
        let invalid = |message: &str| at_line(line_number, message);

        match command.as_str() {
            "FILE" => {
                let (path, kind) = parse_file(rest).map_err(|message| invalid(&message))?;
                if !kind.eq_ignore_ascii_case("WAVE") {
                    return Err(invalid("only FILE ... WAVE is supported"));
                }
                if path.is_empty() {
                    return Err(invalid("CUE FILE path is empty"));
                }
                if sheet.files.len() == 99 {
                    return Err(invalid("CUE sheets cannot contain more than 99 files"));
                }
                current_file = Some(sheet.files.len());
                current_track = None;
                sheet.files.push(path);
            }
            "TRACK" => {
                let file_id = current_file.ok_or_else(|| invalid("TRACK appears before FILE"))?;
                let (number, kind) =
                    split_command(rest).ok_or_else(|| invalid("TRACK needs a number and type"))?;
                if !kind.eq_ignore_ascii_case("AUDIO") {
                    return Err(invalid("only TRACK ... AUDIO is supported"));
                }
                let number = number
                    .parse::<usize>()
                    .ok()
                    .filter(|number| (1..=99).contains(number))
                    .ok_or_else(|| invalid("CUE track number must be between 1 and 99"))?;
                if number != sheet.tracks.len() + 1 {
                    return Err(invalid("CUE track numbers must be sequential"));
                }
                sheet.tracks.push((file_id, CUETrack::new()));
                current_track = Some(sheet.tracks.len() - 1);
            }
            "TITLE" | "PERFORMER" | "SONGWRITER" => {
                let value = parse_text(rest).map_err(|message| invalid(&message))?;
                if let Some(track_index) = current_track {
                    let track = &mut sheet.tracks[track_index].1;
                    let slot = match command.as_str() {
                        "TITLE" => &mut track.title,
                        "PERFORMER" => &mut track.performer,
                        _ => &mut track.songwriter,
                    };
                    set_once(slot, value).map_err(|message| invalid(&message))?;
                } else {
                    let slot = match command.as_str() {
                        "TITLE" => &mut sheet.title,
                        "PERFORMER" => &mut sheet.performer,
                        _ => &mut sheet.songwriter,
                    };
                    set_once(slot, value).map_err(|message| invalid(&message))?;
                }
            }
            "CATALOG" | "CDTEXTFILE" => {
                if current_track.is_some() {
                    return Err(invalid("disc metadata appears inside a TRACK"));
                }
                let value = parse_text(rest).map_err(|message| invalid(&message))?;
                if command == "CATALOG"
                    && (value.len() != 13 || !value.bytes().all(|byte| byte.is_ascii_digit()))
                {
                    return Err(invalid("CATALOG must contain exactly 13 decimal digits"));
                }
                let slot = if command == "CATALOG" {
                    &mut sheet.catalog
                } else {
                    &mut sheet.text_file
                };
                set_once(slot, value).map_err(|message| invalid(&message))?;
            }
            "REM" => {
                if rest.is_empty() {
                    return Err(invalid("REM comment is empty"));
                }
                if let Some(track_index) = current_track {
                    sheet.tracks[track_index].1.comments.push(rest.to_owned());
                } else {
                    sheet.comments.push(rest.to_owned());
                }
            }
            "FLAGS" => {
                let track = current_track_mut(&mut sheet, current_track, line_number)?;
                if track.flags.is_some() {
                    return Err(invalid("duplicate FLAGS command"));
                }
                if rest.is_empty() {
                    return Err(invalid("FLAGS needs at least one flag"));
                }
                track.flags = Some(TrackFlags::parse(rest).map_err(|message| invalid(&message))?);
            }
            "ISRC" => {
                let track = current_track_mut(&mut sheet, current_track, line_number)?;
                let value = one_token(rest).ok_or_else(|| invalid("ISRC value is empty"))?;
                set_once(&mut track.isrc, value.to_owned()).map_err(|message| invalid(&message))?;
            }
            "PREGAP" | "POSTGAP" => {
                let track = current_track_mut(&mut sheet, current_track, line_number)?;
                let timestamp = parse_timestamp(rest).map_err(|message| invalid(&message))?;
                let slot = if command == "PREGAP" {
                    &mut track.pre_gap
                } else {
                    &mut track.post_gap
                };
                set_once(slot, timestamp).map_err(|message| invalid(&message))?;
            }
            "INDEX" => {
                let track = current_track_mut(&mut sheet, current_track, line_number)?;
                let (index, timestamp) = split_command(rest)
                    .ok_or_else(|| invalid("INDEX needs a number and timestamp"))?;
                let index = index
                    .parse::<u8>()
                    .ok()
                    .filter(|index| *index <= 99)
                    .ok_or_else(|| invalid("CUE index number must be between 0 and 99"))?;
                let timestamp = parse_timestamp(timestamp).map_err(|message| invalid(&message))?;
                if track
                    .indices
                    .last()
                    .is_some_and(|(previous, time)| index <= *previous || timestamp < *time)
                {
                    return Err(invalid("CUE indices must increase in number and time"));
                }
                track.indices.push((index, timestamp));
            }
            _ => return Err(invalid("unsupported CUE command")),
        }
    }

    if sheet.files.is_empty() || sheet.tracks.is_empty() {
        return Err("CUE sheet needs at least one WAVE file and AUDIO track".to_owned());
    }
    if sheet
        .tracks
        .iter()
        .any(|(_, track)| !track.indices.iter().any(|(index, _)| *index == 1))
    {
        return Err("every CUE AUDIO track needs INDEX 01".to_owned());
    }
    let mut files_with_tracks = vec![false; sheet.files.len()];
    for (file_id, _) in &sheet.tracks {
        files_with_tracks[*file_id] = true;
    }
    if files_with_tracks.contains(&false) {
        return Err("every CUE WAVE file needs at least one AUDIO track".to_owned());
    }
    Ok(sheet)
}

fn split_command(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let split = line.find(char::is_whitespace)?;
    Some((&line[..split], line[split..].trim()))
}

fn one_token(value: &str) -> Option<&str> {
    let mut words = value.split_ascii_whitespace();
    let word = words.next()?;
    words.next().is_none().then_some(word)
}

fn parse_file(input: &str) -> Result<(String, String), String> {
    let (path, rest) = if input.starts_with('"') || input.starts_with('\'') {
        parse_quoted(input)?
    } else {
        let split = input
            .find(char::is_whitespace)
            .ok_or_else(|| "FILE needs a path and type".to_owned())?;
        (input[..split].to_owned(), input[split..].trim())
    };
    let kind = one_token(rest).ok_or_else(|| "FILE needs exactly one type".to_owned())?;
    if rest.split_ascii_whitespace().count() != 1 {
        return Err("FILE needs exactly one type".to_owned());
    }
    Ok((path, kind.to_owned()))
}

fn parse_text(input: &str) -> Result<String, String> {
    if input.is_empty() {
        return Err("CUE text value is empty".to_owned());
    }
    if input.starts_with('"') || input.starts_with('\'') {
        let (value, rest) = parse_quoted(input)?;
        if !rest.trim().is_empty() {
            return Err("unexpected text after quoted value".to_owned());
        }
        Ok(value)
    } else {
        Ok(input.to_owned())
    }
}

fn parse_quoted(input: &str) -> Result<(String, &str), String> {
    let quote = input
        .chars()
        .next()
        .filter(|quote| matches!(quote, '"' | '\''))
        .ok_or_else(|| "expected a quoted value".to_owned())?;
    let mut output = String::new();
    let mut chars = input.char_indices().skip(1).peekable();
    while let Some((index, character)) = chars.next() {
        if character == '\\' {
            if chars.peek().is_some_and(|(_, next)| *next == quote) {
                let (_, escaped) = chars.next().unwrap();
                output.push(escaped);
            } else {
                output.push(character);
            }
        } else if character == quote {
            return Ok((output, &input[index + character.len_utf8()..]));
        } else {
            output.push(character);
        }
    }
    Err("unterminated quoted value".to_owned())
}

fn parse_timestamp(input: &str) -> Result<CUETimeStamp, String> {
    let mut parts = input.split(':');
    let minutes = parts
        .next()
        .ok_or_else(|| "invalid CUE timestamp".to_owned())?
        .parse::<u8>()
        .map_err(|_| "invalid CUE timestamp".to_owned())?;
    let seconds = parts
        .next()
        .ok_or_else(|| "invalid CUE timestamp".to_owned())?
        .parse::<u8>()
        .ok()
        .filter(|seconds| *seconds < 60)
        .ok_or_else(|| "invalid CUE timestamp".to_owned())?;
    let fractions = parts
        .next()
        .ok_or_else(|| "invalid CUE timestamp".to_owned())?
        .parse::<u8>()
        .ok()
        .filter(|fractions| *fractions < 75)
        .ok_or_else(|| "invalid CUE timestamp".to_owned())?;
    if parts.next().is_some() {
        return Err("invalid CUE timestamp".to_owned());
    }
    Ok(CUETimeStamp::new(minutes, seconds, fractions))
}

fn current_track_mut(
    sheet: &mut CUEFile,
    current_track: Option<usize>,
    line_number: usize,
) -> Result<&mut CUETrack, String> {
    current_track
        .and_then(|index| sheet.tracks.get_mut(index))
        .map(|(_, track)| track)
        .ok_or_else(|| at_line(line_number, "track command appears before TRACK"))
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), String> {
    if slot.replace(value).is_some() {
        Err("duplicate CUE metadata command".to_owned())
    } else {
        Ok(())
    }
}

fn at_line(line: usize, message: &str) -> String {
    format!("line {line}: {message}")
}
