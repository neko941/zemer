use std::fmt::{self, Display, Formatter};

use crate::parse::parse_cue;

pub type FileID = usize;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CUEFile {
    pub files: Vec<String>,
    pub title: Option<String>,
    pub performer: Option<String>,
    pub catalog: Option<String>,
    pub text_file: Option<String>,
    pub songwriter: Option<String>,
    pub tracks: Vec<(usize, CUETrack)>,
    pub comments: Vec<String>,
}

impl CUEFile {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parse(input: &str) -> Result<Self, String> {
        if input.len() > crate::MAX_CUE_BYTES {
            return Err("CUE sheet exceeds the 16 MiB size limit".to_owned());
        }
        parse_cue(input)
    }

    pub fn to_cue_string(&self) -> Result<String, String> {
        if self.files.is_empty()
            || self.files.len() > 99
            || self.tracks.is_empty()
            || self.tracks.len() > 99
        {
            return Err("CUE output must have 1-99 tracks with valid file references".to_owned());
        }
        for value in self
            .files
            .iter()
            .chain(self.title.iter())
            .chain(self.performer.iter())
            .chain(self.songwriter.iter())
            .chain(self.text_file.iter())
            .chain(self.comments.iter())
        {
            validate_line_text(value)?;
        }
        if let Some(catalog) = &self.catalog {
            validate_line_text(catalog)?;
            if catalog.len() != 13 || !catalog.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("CUE CATALOG must contain exactly 13 decimal digits".to_owned());
            }
        }
        if self.files.iter().any(String::is_empty) {
            return Err("CUE FILE path is empty".to_owned());
        }
        let mut files_with_tracks = vec![false; self.files.len()];
        for (file_id, track) in &self.tracks {
            let Some(file_has_tracks) = files_with_tracks.get_mut(*file_id) else {
                return Err("CUE track references a missing FILE".to_owned());
            };
            *file_has_tracks = true;
            if !track.indices.iter().any(|(index, _)| *index == 1) {
                return Err("every CUE AUDIO track needs INDEX 01".to_owned());
            }
            track.validate()?;
        }
        if files_with_tracks.contains(&false) {
            return Err("every CUE WAVE file needs at least one AUDIO track".to_owned());
        }
        let (bytes, lines) = self.output_size()?;
        check_output_size(bytes, lines)?;
        Ok(self.to_string())
    }

    fn output_size(&self) -> Result<(usize, usize), String> {
        let mut bytes = 0;
        let mut lines = 0;
        for comment in &self.comments {
            add_output_line(&mut bytes, &mut lines, 4 + comment.len())?;
        }
        for (prefix, value) in [
            ("CATALOG ", self.catalog.as_deref()),
            ("CDTEXTFILE ", self.text_file.as_deref()),
            ("TITLE ", self.title.as_deref()),
            ("PERFORMER ", self.performer.as_deref()),
            ("SONGWRITER ", self.songwriter.as_deref()),
        ] {
            if let Some(value) = value {
                let value_len = if prefix == "CATALOG " {
                    value.len()
                } else {
                    quoted_len(value)?
                };
                add_output_line(&mut bytes, &mut lines, prefix.len() + value_len)?;
            }
        }
        for (file_id, path) in self.files.iter().enumerate() {
            add_output_line(
                &mut bytes,
                &mut lines,
                "FILE ".len() + quoted_len(path)? + " WAVE".len(),
            )?;
            for (_, track) in self
                .tracks
                .iter()
                .filter(|(track_file_id, _)| *track_file_id == file_id)
            {
                add_output_line(&mut bytes, &mut lines, "  TRACK 01 AUDIO".len())?;
                let (track_bytes, track_lines) = track.output_size()?;
                bytes = bytes
                    .checked_add(track_bytes)
                    .ok_or_else(|| "CUE output size overflow".to_owned())?;
                lines = lines
                    .checked_add(track_lines)
                    .ok_or_else(|| "CUE output line count overflow".to_owned())?;
            }
        }
        Ok((bytes, lines))
    }
}

impl Display for CUEFile {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut lines = Vec::new();
        lines.extend(self.comments.iter().map(|comment| format!("REM {comment}")));
        if let Some(catalog) = &self.catalog {
            lines.push(format!("CATALOG {catalog}"));
        }
        if let Some(text_file) = &self.text_file {
            lines.push(format!("CDTEXTFILE {}", render_text(text_file)));
        }
        if let Some(title) = &self.title {
            lines.push(format!("TITLE {}", render_text(title)));
        }
        if let Some(performer) = &self.performer {
            lines.push(format!("PERFORMER {}", render_text(performer)));
        }
        if let Some(songwriter) = &self.songwriter {
            lines.push(format!("SONGWRITER {}", render_text(songwriter)));
        }

        let mut number = 1;
        for (file_id, path) in self.files.iter().enumerate() {
            lines.push(format!("FILE {} WAVE", render_text(path)));
            for (_, track) in self
                .tracks
                .iter()
                .filter(|(track_file_id, _)| *track_file_id == file_id)
            {
                lines.push(format!("  TRACK {number:02} AUDIO"));
                number += 1;
                lines.extend(track.to_string().lines().map(str::to_owned));
            }
        }
        f.write_str(&lines.join("\n"))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CUETrack {
    pub title: Option<String>,
    pub performer: Option<String>,
    pub songwriter: Option<String>,
    pub flags: Option<TrackFlags>,
    pub isrc: Option<String>,
    pub pre_gap: Option<CUETimeStamp>,
    pub post_gap: Option<CUETimeStamp>,
    pub indices: Vec<(u8, CUETimeStamp)>,
    pub comments: Vec<String>,
}

impl CUETrack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn to_cue_string(&self) -> Result<String, String> {
        self.validate()?;
        let (bytes, lines) = self.output_size()?;
        check_output_size(bytes, lines)?;
        Ok(self.to_string())
    }

    fn output_size(&self) -> Result<(usize, usize), String> {
        let mut bytes = 0;
        let mut lines = 0;
        for (prefix, value) in [
            ("    TITLE ", self.title.as_deref()),
            ("    PERFORMER ", self.performer.as_deref()),
            ("    SONGWRITER ", self.songwriter.as_deref()),
        ] {
            if let Some(value) = value {
                add_output_line(&mut bytes, &mut lines, prefix.len() + quoted_len(value)?)?;
            }
        }
        if let Some(flags) = self.flags.filter(|flags| !flags.is_empty()) {
            add_output_line(
                &mut bytes,
                &mut lines,
                "    FLAGS ".len() + flags.to_string().len(),
            )?;
        }
        if let Some(isrc) = &self.isrc {
            add_output_line(&mut bytes, &mut lines, "    ISRC ".len() + isrc.len())?;
        }
        if let Some(timestamp) = self.pre_gap {
            add_output_line(
                &mut bytes,
                &mut lines,
                "    PREGAP ".len() + timestamp.to_string().len(),
            )?;
        }
        for (index, timestamp) in &self.indices {
            let index_len = format!("{index:02}").len();
            add_output_line(
                &mut bytes,
                &mut lines,
                "    INDEX ".len() + index_len + 1 + timestamp.to_string().len(),
            )?;
        }
        if let Some(timestamp) = self.post_gap {
            add_output_line(
                &mut bytes,
                &mut lines,
                "    POSTGAP ".len() + timestamp.to_string().len(),
            )?;
        }
        for comment in &self.comments {
            add_output_line(&mut bytes, &mut lines, "    REM ".len() + comment.len())?;
        }
        Ok((bytes, lines))
    }

    fn validate(&self) -> Result<(), String> {
        for value in self
            .title
            .iter()
            .chain(self.performer.iter())
            .chain(self.songwriter.iter())
            .chain(self.comments.iter())
        {
            validate_line_text(value)?;
        }
        if let Some(isrc) = &self.isrc {
            validate_line_text(isrc)?;
            if isrc.chars().any(char::is_whitespace) {
                return Err("CUE ISRC must be a single token".to_owned());
            }
        }
        let mut previous = None;
        for (index, timestamp) in &self.indices {
            crate::cue_frame(*timestamp)?;
            if *index > 99
                || previous.is_some_and(|(previous_index, previous_time)| {
                    *index <= previous_index || *timestamp < previous_time
                })
            {
                return Err("CUE indices must increase in number and time".to_owned());
            }
            previous = Some((*index, *timestamp));
        }
        for timestamp in self.pre_gap.iter().chain(self.post_gap.iter()) {
            crate::cue_frame(*timestamp)?;
        }
        Ok(())
    }
}

impl Display for CUETrack {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut lines = Vec::new();
        if let Some(title) = &self.title {
            lines.push(format!("    TITLE {}", render_text(title)));
        }
        if let Some(performer) = &self.performer {
            lines.push(format!("    PERFORMER {}", render_text(performer)));
        }
        if let Some(songwriter) = &self.songwriter {
            lines.push(format!("    SONGWRITER {}", render_text(songwriter)));
        }
        if let Some(flags) = self.flags.filter(|flags| !flags.is_empty()) {
            lines.push(format!("    FLAGS {flags}"));
        }
        if let Some(isrc) = &self.isrc {
            lines.push(format!("    ISRC {isrc}"));
        }
        if let Some(pre_gap) = self.pre_gap {
            lines.push(format!("    PREGAP {pre_gap}"));
        }
        lines.extend(
            self.indices
                .iter()
                .map(|(index, timestamp)| format!("    INDEX {index:02} {timestamp}")),
        );
        if let Some(post_gap) = self.post_gap {
            lines.push(format!("    POSTGAP {post_gap}"));
        }
        lines.extend(
            self.comments
                .iter()
                .map(|comment| format!("    REM {comment}")),
        );
        f.write_str(&lines.join("\n"))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TrackFlags(u8);

impl TrackFlags {
    pub const DCP: Self = Self(1);
    pub const CH4: Self = Self(2);
    pub const PRE: Self = Self(4);
    pub const SCMS: Self = Self(8);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        let mut flags = Self::empty();
        for token in value.split_ascii_whitespace() {
            flags |= match token.to_ascii_uppercase().as_str() {
                "DCP" => Self::DCP,
                "4CH" => Self::CH4,
                "PRE" => Self::PRE,
                "SCMS" => Self::SCMS,
                _ => return Err(format!("unknown CUE track flag: {token}")),
            };
        }
        Ok(flags)
    }
}

impl From<&str> for TrackFlags {
    fn from(value: &str) -> Self {
        let mut flags = Self::empty();
        for token in value.split_ascii_whitespace() {
            flags |= match token.to_ascii_uppercase().as_str() {
                "DCP" => Self::DCP,
                "4CH" => Self::CH4,
                "PRE" => Self::PRE,
                "SCMS" => Self::SCMS,
                _ => Self::empty(),
            };
        }
        flags
    }
}

impl std::ops::BitOr for TrackFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for TrackFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl Display for TrackFlags {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut flags = Vec::new();
        if self.contains(Self::DCP) {
            flags.push("DCP");
        }
        if self.contains(Self::CH4) {
            flags.push("4CH");
        }
        if self.contains(Self::PRE) {
            flags.push("PRE");
        }
        if self.contains(Self::SCMS) {
            flags.push("SCMS");
        }
        f.write_str(&flags.join(" "))
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct CUETimeStamp {
    minutes: u8,
    seconds: u8,
    fractions: u8,
}

impl CUETimeStamp {
    pub const fn new(minutes: u8, seconds: u8, fractions: u8) -> Self {
        Self {
            minutes,
            seconds,
            fractions,
        }
    }

    pub const fn minutes(self) -> u8 {
        self.minutes
    }

    pub const fn seconds(self) -> u8 {
        self.seconds
    }

    pub const fn fractions(self) -> u8 {
        self.fractions
    }
}

impl Display for CUETimeStamp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}",
            self.minutes, self.seconds, self.fractions
        )
    }
}

fn render_text(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn quoted_len(value: &str) -> Result<usize, String> {
    value
        .len()
        .checked_add(value.bytes().filter(|byte| *byte == b'"').count())
        .and_then(|length| length.checked_add(2))
        .ok_or_else(|| "CUE output size overflow".to_owned())
}

fn add_output_line(bytes: &mut usize, lines: &mut usize, line_bytes: usize) -> Result<(), String> {
    *bytes = bytes
        .checked_add(line_bytes)
        .ok_or_else(|| "CUE output size overflow".to_owned())?;
    *lines = lines
        .checked_add(1)
        .ok_or_else(|| "CUE output line count overflow".to_owned())?;
    Ok(())
}

fn check_output_size(bytes: usize, lines: usize) -> Result<(), String> {
    let size = bytes
        .checked_add(lines.saturating_sub(1))
        .ok_or_else(|| "CUE output size overflow".to_owned())?;
    if size > crate::MAX_CUE_BYTES {
        return Err("CUE output exceeds the 16 MiB size limit".to_owned());
    }
    Ok(())
}

fn validate_line_text(value: &str) -> Result<(), String> {
    if value
        .chars()
        .any(|character| matches!(character, '\r' | '\n' | '\0'))
    {
        return Err("CUE values cannot contain line breaks or NUL bytes".to_owned());
    }
    Ok(())
}
