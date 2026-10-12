use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Write,
    ops::Range,
    path::{Path, PathBuf},
};

use cue::{CUEFile, CUETimeStamp, CUETrack, cue_frame, parse_wav, sample_frame};
use wav::{WavContainer, WavDecoder, WavWriter};
use zemer_core::AudioSpec;
use zemer_core::{AudioDecoder, AudioEncoder, AudioError, Metadata};

pub struct CueReport {
    pub action: &'static str,
    pub cue_path: PathBuf,
    pub tracks: usize,
}

struct TrackRange {
    file_id: usize,
    track: CUETrack,
    start_frame: u64,
    end_frame: u64,
    start_cue_frame: u64,
    end_cue_frame: u64,
    number: usize,
}

pub fn run(args: &[OsString]) -> Result<CueReport, String> {
    let args: Vec<_> = args
        .iter()
        .filter(|arg| arg.as_os_str() != "--json")
        .collect();
    match args.first().and_then(|arg| arg.to_str()) {
        Some("split") if args.len() == 3 => split(Path::new(&args[1]), Path::new(&args[2])),
        Some("create") if args.len() >= 3 => {
            let input = args[2..].iter().map(Path::new).collect::<Vec<_>>();
            create(Path::new(&args[1]), &input)
        }
        _ => Err("usage: zemer cue split [--json] <input.cue> <new-output-dir> | zemer cue create [--json] <output.cue> <track.wav>...".to_owned()),
    }
}

fn split(cue_path: &Path, output_dir: &Path) -> Result<CueReport, String> {
    let bytes = fs::read(cue_path).map_err(|error| format!("{}: {error}", cue_path.display()))?;
    let sheet = parse_wav(&bytes)?;
    let parent = cue_path.parent().unwrap_or_else(|| Path::new("."));
    let mut file_info = Vec::with_capacity(sheet.files.len());
    for name in &sheet.files {
        let path = parent.join(name.replace('\\', "/"));
        let (spec, frames, _) = wav_info(&path)?;
        file_info.push((path, spec, frames));
    }

    let mut ranges = Vec::with_capacity(sheet.tracks.len());
    let mut number = 0;
    for (file_id, (_, spec, file_frames)) in file_info.iter().enumerate() {
        let tracks = sheet
            .tracks
            .iter()
            .filter_map(|(track_file_id, track)| (*track_file_id == file_id).then_some(track))
            .collect::<Vec<_>>();
        for (position, track) in tracks.iter().enumerate() {
            number += 1;
            let start_cue_frame = if position == 0 {
                0
            } else {
                cue_frame(
                    index(track, 0)
                        .or_else(|| index(track, 1))
                        .ok_or_else(|| format!("track {number} is missing INDEX 01"))?,
                )
                .map_err(str::to_owned)?
            };
            let end_cue_frame = if let Some(next_track) = tracks.get(position + 1) {
                cue_frame(
                    index(next_track, 0)
                        .or_else(|| index(next_track, 1))
                        .ok_or_else(|| format!("track {} is missing INDEX 01", number + 1))?,
                )
                .map_err(str::to_owned)?
            } else {
                u64::MAX
            };
            let start_frame = if position == 0 {
                0
            } else {
                sample_frame(
                    timestamp_from_cue_frame(start_cue_frame)?,
                    spec.sample_rate(),
                )
                .map_err(str::to_owned)?
            };
            let end_frame = if position + 1 == tracks.len() {
                *file_frames
            } else {
                sample_frame(timestamp_from_cue_frame(end_cue_frame)?, spec.sample_rate())
                    .map_err(str::to_owned)?
            };
            if start_frame >= end_frame || end_frame > *file_frames {
                return Err(format!(
                    "track {number} has an empty, reversed, or out-of-file range"
                ));
            }
            ranges.push(TrackRange {
                file_id,
                track: (**track).clone(),
                start_frame,
                end_frame,
                start_cue_frame,
                end_cue_frame,
                number,
            });
        }
    }

    fs::create_dir(output_dir).map_err(|error| format!("{}: {error}", output_dir.display()))?;
    let cue_out = output_dir.join("tracks.cue");
    let mut created = Vec::new();
    let result = (|| {
        let mut output_sheet = CUEFile::new();
        output_sheet.title = sheet.title.clone();
        output_sheet.performer = sheet.performer.clone();
        output_sheet.songwriter = sheet.songwriter.clone();
        output_sheet.catalog = sheet.catalog.clone();
        output_sheet.text_file = sheet.text_file.clone();
        output_sheet.comments = sheet.comments.clone();
        let mut active_file = None;
        let mut decoder = None;
        let mut source_position = 0_u64;
        for range in &ranges {
            if active_file != Some(range.file_id) {
                let file = File::open(&file_info[range.file_id].0).map_err(|error| {
                    format!("{}: {error}", file_info[range.file_id].0.display())
                })?;
                decoder = Some(
                    WavDecoder::new(file)
                        .map_err(|error| audio_error(error, &file_info[range.file_id].0))?,
                );
                active_file = Some(range.file_id);
                source_position = 0;
            }
            let title = track_title(&range.track, range.number);
            let filename = format!("{:02} - {}.wav", range.number, safe_filename(&title));
            let output_path = output_dir.join(&filename);
            let mut metadata = Metadata::default();
            metadata.insert("TITLE", title);
            if let Some(performer) = &range.track.performer {
                metadata.insert("ARTIST", performer.clone());
            } else if let Some(performer) = &sheet.performer {
                metadata.insert("ARTIST", performer.clone());
            }
            copy_wav_range(
                decoder.as_mut().unwrap(),
                &file_info[range.file_id].0,
                &output_path,
                file_info[range.file_id].1,
                source_position..range.start_frame,
                range.start_frame..range.end_frame,
                &metadata,
            )?;
            source_position = range.end_frame;
            created.push(output_path);
            output_sheet.files.push(filename);
            let mut track = range.track.clone();
            track.indices = range
                .track
                .indices
                .iter()
                .filter_map(|(id, timestamp)| {
                    let source = cue_frame(*timestamp).ok()?;
                    (source >= range.start_cue_frame && source < range.end_cue_frame).then(
                        || {
                            timestamp_from_cue_frame(source - range.start_cue_frame)
                                .ok()
                                .map(|relative| (*id, relative))
                        },
                    )?
                })
                .collect();
            if !track.indices.iter().any(|(id, _)| *id == 1) {
                return Err(format!(
                    "track {} INDEX 01 falls before its split range",
                    range.number
                ));
            }
            output_sheet
                .tracks
                .push((output_sheet.files.len() - 1, track));
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&cue_out)
            .map_err(|error| format!("{}: {error}", cue_out.display()))?;
        created.push(cue_out.clone());
        output
            .write_all(output_sheet.to_string().as_bytes())
            .map_err(|error| format!("{}: {error}", cue_out.display()))?;
        Ok(())
    })();
    if let Err(error) = result {
        for path in created {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_dir(output_dir);
        return Err(error);
    }
    Ok(CueReport {
        action: "split",
        cue_path: cue_out,
        tracks: ranges.len(),
    })
}

fn create(output: &Path, inputs: &[&Path]) -> Result<CueReport, String> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let output_parent =
        fs::canonicalize(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let mut sheet = CUEFile::new();
    sheet.title = output
        .file_stem()
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    let mut artist = None;
    for input in inputs {
        let (spec, metadata) = wav_metadata(input)?;
        let name = relative_file(&output_parent, input)?;
        sheet.files.push(name);
        if artist.is_none() {
            artist = metadata.values("ARTIST").first().cloned();
        }
        let mut track = CUETrack::new();
        track.title = metadata.values("TITLE").first().cloned().or_else(|| {
            input
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned)
        });
        track.performer = metadata.values("ARTIST").first().cloned();
        track.indices.push((1, CUETimeStamp::new(0, 0, 0)));
        if spec.sample_rate() == 0 {
            return Err(format!("{} has an invalid sample rate", input.display()));
        }
        sheet.tracks.push((sheet.files.len() - 1, track));
    }
    sheet.performer = artist;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|error| format!("{}: {error}", output.display()))?;
    if let Err(error) = file.write_all(sheet.to_string().as_bytes()) {
        let _ = fs::remove_file(output);
        return Err(format!("{}: {error}", output.display()));
    }
    Ok(CueReport {
        action: "create",
        cue_path: output.to_owned(),
        tracks: inputs.len(),
    })
}

fn wav_info(path: &Path) -> Result<(AudioSpec, u64, Metadata), String> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut decoder = WavDecoder::new(file).map_err(|error| audio_error(error, path))?;
    let spec = decoder.spec();
    let metadata = decoder.metadata().clone();
    let mut frames = 0_u64;
    while let Some(block) = decoder
        .read_frames(8192)
        .map_err(|error| audio_error(error, path))?
    {
        frames = frames
            .checked_add(block.frames() as u64)
            .ok_or("WAV frame count overflow")?;
    }
    Ok((spec, frames, metadata))
}

fn wav_metadata(path: &Path) -> Result<(AudioSpec, Metadata), String> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let decoder = WavDecoder::new(file).map_err(|error| audio_error(error, path))?;
    Ok((decoder.spec(), decoder.metadata().clone()))
}

fn copy_wav_range(
    decoder: &mut WavDecoder<File>,
    source: &Path,
    output: &Path,
    spec: AudioSpec,
    skipped_range: Range<u64>,
    copy_range: Range<u64>,
    metadata: &Metadata,
) -> Result<(), String> {
    if decoder.spec() != spec
        || skipped_range.start > skipped_range.end
        || skipped_range.end != copy_range.start
        || copy_range.start >= copy_range.end
    {
        return Err(format!(
            "{} has incompatible or overlapping track ranges",
            source.display()
        ));
    }
    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|error| format!("{}: {error}", output.display()))?;
    let result = (|| {
        let mut encoder = WavWriter::new_with_metadata_and_container(
            output_file,
            spec,
            metadata,
            WavContainer::Riff,
        )
        .map_err(|error| audio_error(error, output))?;
        let mut skipped = skipped_range.start;
        while skipped < skipped_range.end {
            let count = (skipped_range.end - skipped).min(8192) as usize;
            let block = decoder
                .read_frames(count)
                .map_err(|error| audio_error(error, source))?
                .ok_or_else(|| format!("{} ended before the track start", source.display()))?;
            skipped += block.frames() as u64;
        }
        let mut copied = copy_range.start;
        while copied < copy_range.end {
            let count = (copy_range.end - copied).min(8192) as usize;
            let block = decoder
                .read_frames(count)
                .map_err(|error| audio_error(error, source))?
                .ok_or_else(|| format!("{} ended before the track end", source.display()))?;
            copied += block.frames() as u64;
            encoder
                .write_block(&block)
                .map_err(|error| audio_error(error, output))?;
        }
        encoder
            .finish()
            .map_err(|error| audio_error(error, output))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(output);
    }
    result
}

fn index(track: &CUETrack, wanted: u8) -> Option<CUETimeStamp> {
    track
        .indices
        .iter()
        .find(|(number, _)| *number == wanted)
        .map(|(_, time)| *time)
}

fn timestamp_from_cue_frame(frame: u64) -> Result<CUETimeStamp, String> {
    let minutes = frame / (75 * 60);
    if minutes > u64::from(u8::MAX) {
        return Err("CUE timestamp exceeds the supported minute range".to_owned());
    }
    Ok(CUETimeStamp::new(
        minutes as u8,
        ((frame / 75) % 60) as u8,
        (frame % 75) as u8,
    ))
}

fn track_title(track: &CUETrack, number: usize) -> String {
    track
        .title
        .clone()
        .unwrap_or_else(|| format!("Track {number:02}"))
}

fn safe_filename(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_end_matches('.');
    if name.is_empty() {
        "Track".to_owned()
    } else {
        name.to_owned()
    }
}

fn relative_file(base: &Path, input: &Path) -> Result<String, String> {
    let absolute =
        fs::canonicalize(input).map_err(|error| format!("{}: {error}", input.display()))?;
    let base_parts = base.components().collect::<Vec<_>>();
    let file_parts = absolute.components().collect::<Vec<_>>();
    let common = base_parts
        .iter()
        .zip(&file_parts)
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 {
        return Err(format!(
            "{} is on a different volume from the CUE output",
            input.display()
        ));
    }
    let mut parts = vec!["..".to_owned(); base_parts.len() - common];
    for part in &file_parts[common..] {
        parts.push(
            part.as_os_str()
                .to_str()
                .ok_or_else(|| format!("{} is not valid UTF-8", input.display()))?
                .to_owned(),
        );
    }
    Ok(parts.join("/"))
}

fn audio_error(error: AudioError, path: &Path) -> String {
    format!("{}: {error}", path.display())
}
