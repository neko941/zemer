use std::{fmt::Write as _, path::Path};

use super::{DsdReport, Report, iso_cli};
use wav::WavContainer;
use zemer_core::SampleFormat;

pub(super) fn print_report(report: &Report) {
    println!("format: {}", report.format.to_ascii_uppercase());
    if let Some(container) = report.container {
        println!("container: {}", container_name(container));
    }
    println!("sample_rate: {}", report.spec.sample_rate());
    println!("channels: {}", report.spec.channels());
    if let Some(mask) = report.spec.speaker_mask() {
        println!("speaker_mask: 0x{mask:08x}");
    }
    println!(
        "sample_format: {}",
        sample_format_name(report.spec.sample_format())
    );
    println!("frames: {}", report.frames);
    println!(
        "duration: {}/{} seconds",
        report.frames,
        report.spec.sample_rate()
    );
    for (key, values) in report.metadata.iter() {
        for value in values {
            println!("metadata.{key}: {value}");
        }
    }
}

pub(super) fn print_dsd_report(report: &DsdReport) {
    println!("format: {}", report.format);
    println!("dsd_bit_rate: {}", report.spec.bit_rate());
    println!("channels: {}", report.spec.channels());
    if let Some(mask) = report.spec.speaker_mask() {
        println!("speaker_mask: 0x{mask:08x}");
    }
    println!("octet_frames: {}", report.octet_frames);
    println!("dsd_samples_per_channel: {}", report.sample_count);
    println!(
        "duration: {}/{} seconds",
        report.sample_count,
        report.spec.bit_rate()
    );
    for (key, values) in report.metadata.iter() {
        for value in values {
            println!("metadata.{key}: {value}");
        }
    }
}

pub(super) fn dsd_report_json(report: &DsdReport) -> String {
    let mut json = format!(
        "{{\"format\":{},\"dsd_bit_rate\":{},\"channels\":{},\"speaker_mask\":{},\"octet_frames\":{},\"dsd_samples_per_channel\":{},\"duration_samples\":{{\"numerator\":{},\"denominator\":{}}},\"metadata\":{{",
        json_string(report.format),
        report.spec.bit_rate(),
        report.spec.channels(),
        report
            .spec
            .speaker_mask()
            .map_or_else(|| "null".to_owned(), |mask| mask.to_string()),
        report.octet_frames,
        report.sample_count,
        report.sample_count,
        report.spec.bit_rate(),
    );
    for (index, (key, values)) in report.metadata.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        let _ = write!(json, "{}:[", json_string(key));
        for (value_index, value) in values.iter().enumerate() {
            if value_index > 0 {
                json.push(',');
            }
            json.push_str(&json_string(value));
        }
        json.push(']');
    }
    json.push_str("}}");
    json
}

pub(super) fn dsd_conversion_json(report: &DsdReport, output: &Path) -> String {
    let mut json = dsd_report_json(report);
    json.insert_str(
        1,
        &format!("\"output\":{},", json_string(&output.display().to_string())),
    );
    json
}

pub(super) fn report_json(report: &Report) -> String {
    let mut json = format!(
        "{{\"format\":{},\"container\":{},\"sample_rate\":{},\"channels\":{},\"speaker_mask\":{},\"sample_format\":{},\"frames\":{},\"duration_frames\":{{\"numerator\":{},\"denominator\":{}}},\"metadata\":{{",
        json_string(report.format),
        report.container.map_or_else(
            || "null".to_owned(),
            |container| json_string(container_name(container))
        ),
        report.spec.sample_rate(),
        report.spec.channels(),
        report
            .spec
            .speaker_mask()
            .map_or_else(|| "null".to_owned(), |mask| mask.to_string()),
        json_string(sample_format_name(report.spec.sample_format())),
        report.frames,
        report.frames,
        report.spec.sample_rate(),
    );
    for (index, (key, values)) in report.metadata.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        let _ = write!(json, "{}:[", json_string(key));
        for (value_index, value) in values.iter().enumerate() {
            if value_index > 0 {
                json.push(',');
            }
            json.push_str(&json_string(value));
        }
        json.push(']');
    }
    json.push_str("}}");
    json
}

pub(super) fn iso_report_json(report: &iso_cli::IsoReport) -> String {
    let mut json = format!(
        "{{\"operation\":{},\"image\":{},\"volume_id\":{},\"entry_count\":{},\"entries\":[",
        json_string(&format!("iso_{}", report.action)),
        json_string(&report.image.display().to_string()),
        json_string(&report.volume_id),
        report.entries.len(),
    );
    for (index, entry) in report.entries.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        let _ = write!(
            json,
            "{{\"path\":{},\"type\":{},\"size\":{}}}",
            json_string(&entry.path),
            json_string(if entry.directory { "directory" } else { "file" }),
            entry.size,
        );
    }
    json.push(']');
    if let Some(output) = &report.output {
        let _ = write!(
            json,
            ",\"output\":{}",
            json_string(&output.display().to_string())
        );
    }
    json.push('}');
    json
}

pub(super) fn conversion_json(report: &Report, output: &Path) -> String {
    let mut json = report_json(report);
    json.insert_str(
        1,
        &format!("\"output\":{},", json_string(&output.display().to_string())),
    );
    if let Some(companion) = &report.companion_output {
        json.insert_str(
            1,
            &format!(
                "\"correction_output\":{},",
                json_string(&companion.display().to_string())
            ),
        );
    }
    if let Some(encoding) = report.output_encoding {
        json.insert_str(
            1,
            &format!("\"output_encoding\":{},", json_string(encoding)),
        );
    }
    if let Some(valid_bits) = report.output_valid_bits {
        json.insert_str(1, &format!("\"output_valid_bits\":{valid_bits},"));
    }
    json
}

pub(super) fn sample_format_name(format: SampleFormat) -> &'static str {
    match format {
        SampleFormat::PcmU8 => "pcm_u8",
        SampleFormat::PcmI16 => "pcm_i16",
        SampleFormat::PcmI24 => "pcm_i24",
        SampleFormat::PcmI32 => "pcm_i32",
        SampleFormat::Float32 => "float32",
        SampleFormat::Float64 => "float64",
    }
}

pub(super) fn container_name(container: WavContainer) -> &'static str {
    match container {
        WavContainer::Riff => "riff",
        WavContainer::Rf64 => "rf64",
    }
}

pub(super) fn json_string(value: &str) -> String {
    let mut json = String::with_capacity(value.len() + 2);
    json.push('"');
    for character in value.chars() {
        match character {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            '\u{0008}' => json.push_str("\\b"),
            '\u{000c}' => json.push_str("\\f"),
            control if control <= '\u{001f}' => {
                let _ = write!(json, "\\u{:04x}", control as u32);
            }
            other => json.push(other),
        }
    }
    json.push('"');
    json
}
