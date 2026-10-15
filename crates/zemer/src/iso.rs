use std::{
    ffi::OsString,
    fs::File,
    path::{Path, PathBuf},
};

use iso9660::IsoImage;

pub struct IsoReport {
    pub action: &'static str,
    pub image: PathBuf,
    pub volume_id: String,
    pub entries: Vec<IsoEntryReport>,
    pub output: Option<PathBuf>,
}

pub struct IsoEntryReport {
    pub path: String,
    pub directory: bool,
    pub size: u64,
}

pub fn run(args: &[OsString]) -> Result<IsoReport, String> {
    let args = args
        .iter()
        .filter(|argument| argument.as_os_str() != "--json")
        .collect::<Vec<_>>();
    match args.first().and_then(|argument| argument.to_str()) {
        Some("inspect") if args.len() == 2 => inspect(Path::new(&args[1])),
        Some("extract") if args.len() == 3 => {
            extract(Path::new(&args[1]), Path::new(&args[2]))
        }
        _ => Err("usage: zemer iso inspect [--json] <image.iso> | zemer iso extract [--json] <image.iso> <new-output-dir>".to_owned()),
    }
}

fn inspect(path: &Path) -> Result<IsoReport, String> {
    let file = open_image(path)?;
    let image = IsoImage::new(file).map_err(|error| format!("{}: {error}", path.display()))?;
    let volume_id = image.volume_id().to_owned();
    let entries = image
        .entries()
        .iter()
        .map(|entry| IsoEntryReport {
            path: entry.path().join("/"),
            directory: entry.is_directory(),
            size: entry.len(),
        })
        .collect();
    Ok(IsoReport {
        action: "inspect",
        image: path.to_owned(),
        volume_id,
        entries,
        output: None,
    })
}

fn extract(image_path: &Path, output: &Path) -> Result<IsoReport, String> {
    let file = open_image(image_path)?;
    let mut image =
        IsoImage::new(file).map_err(|error| format!("{}: {error}", image_path.display()))?;
    let volume_id = image.volume_id().to_owned();
    let entries = image
        .entries()
        .iter()
        .map(|entry| IsoEntryReport {
            path: entry.path().join("/"),
            directory: entry.is_directory(),
            size: entry.len(),
        })
        .collect();
    image
        .extract_to(output)
        .map_err(|error| format!("{}: {error}", output.display()))?;
    Ok(IsoReport {
        action: "extract",
        image: image_path.to_owned(),
        volume_id,
        entries,
        output: Some(output.to_owned()),
    })
}

fn open_image(path: &Path) -> Result<File, String> {
    File::open(path).map_err(|error| format!("{}: {error}", path.display()))
}
