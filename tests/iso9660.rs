use std::{
    fs,
    io::Cursor,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use iso9660::{IsoImage, LOGICAL_BLOCK_SIZE};

static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

fn put_u16(bytes: &mut [u8], value: u16) {
    bytes[..2].copy_from_slice(&value.to_le_bytes());
    bytes[2..].copy_from_slice(&value.to_be_bytes());
}

fn put_u32(bytes: &mut [u8], value: u32) {
    bytes[..4].copy_from_slice(&value.to_le_bytes());
    bytes[4..].copy_from_slice(&value.to_be_bytes());
}

fn record(name: &[u8], extent: u32, size: u32, flags: u8) -> Vec<u8> {
    let mut bytes = vec![0; 33 + name.len() + usize::from(name.len() % 2 == 0)];
    bytes[0] = bytes.len() as u8;
    put_u32(&mut bytes[2..10], extent);
    put_u32(&mut bytes[10..18], size);
    bytes[25] = flags;
    put_u16(&mut bytes[28..32], 1);
    bytes[32] = name.len() as u8;
    bytes[33..33 + name.len()].copy_from_slice(name);
    bytes
}

fn sample_iso(joliet: bool) -> Vec<u8> {
    let block = LOGICAL_BLOCK_SIZE as usize;
    let mut bytes = vec![0; block * 24];
    let pvd = &mut bytes[block * 16..block * 17];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    pvd[40..49].copy_from_slice(b"TEST DISC");
    put_u32(&mut pvd[80..88], 24);
    put_u16(&mut pvd[128..132], LOGICAL_BLOCK_SIZE as u16);
    let root = record(&[0], 18, block as u32, 2);
    pvd[156..156 + root.len()].copy_from_slice(&root);
    if joliet {
        let svd = &mut bytes[block * 17..block * 18];
        svd[0] = 2;
        svd[1..6].copy_from_slice(b"CD001");
        svd[6] = 1;
        let volume_id: Vec<u8> = "音楽".encode_utf16().flat_map(u16::to_be_bytes).collect();
        svd[40..40 + volume_id.len()].copy_from_slice(&volume_id);
        svd[88..91].copy_from_slice(b"%/E");
        put_u32(&mut svd[80..88], 24);
        put_u16(&mut svd[128..132], LOGICAL_BLOCK_SIZE as u16);
        let joliet_root = record(&[0], 19, block as u32, 2);
        svd[156..156 + joliet_root.len()].copy_from_slice(&joliet_root);
    }
    let end_block = if joliet { 18 } else { 17 };
    let terminator = block * end_block;
    bytes[terminator] = 255;
    bytes[terminator + 1..terminator + 6].copy_from_slice(b"CD001");
    bytes[terminator + 6] = 1;
    let root_block = if joliet { 19 } else { 18 };
    let file_block = root_block + 1;
    let directory = block * root_block;
    let current = record(&[0], root_block as u32, block as u32, 2);
    let parent = record(&[1], root_block as u32, block as u32, 2);
    let filename: Vec<u8> = if joliet {
        "écho.txt;1"
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect()
    } else {
        b"TEST.TXT;1".to_vec()
    };
    let file = record(&filename, file_block as u32, 4, 0);
    let mut offset = directory;
    for value in [&current, &parent, &file] {
        bytes[offset..offset + value.len()].copy_from_slice(value);
        offset += value.len();
    }
    bytes[block * file_block..block * file_block + 4].copy_from_slice(b"data");
    bytes
}

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "zemer-iso-{name}-{}-{}",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn inspects_and_extracts_a_primary_volume_from_the_cli() {
    let image = IsoImage::new(Cursor::new(sample_iso(false))).unwrap();
    assert_eq!(image.volume_id(), "TEST DISC");
    assert_eq!(image.entries()[0].name(), "TEST.TXT");

    let image_path = temp_path("sample.iso");
    let output_dir = temp_path("extracted");
    fs::write(&image_path, sample_iso(false)).unwrap();
    let cli = env!("CARGO_BIN_EXE_zemer_test_cli");

    let inspect = Command::new(cli)
        .args(["iso", "inspect", "--json"])
        .arg(&image_path)
        .output()
        .unwrap();
    assert!(
        inspect.status.success(),
        "{}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let report = String::from_utf8(inspect.stdout).unwrap();
    assert!(report.contains("\"volume_id\":\"TEST DISC\""));
    assert!(report.contains("\"path\":\"TEST.TXT\""));

    let extract = Command::new(cli)
        .args(["iso", "extract"])
        .arg(&image_path)
        .arg(&output_dir)
        .output()
        .unwrap();
    assert!(
        extract.status.success(),
        "{}",
        String::from_utf8_lossy(&extract.stderr)
    );
    assert_eq!(fs::read(output_dir.join("TEST.TXT")).unwrap(), b"data");

    fs::remove_file(image_path).unwrap();
    fs::remove_dir_all(output_dir).unwrap();
}

#[test]
fn reads_joliet_names_and_copies_the_file() {
    let mut image = IsoImage::new(Cursor::new(sample_iso(true))).unwrap();
    assert_eq!(image.volume_id(), "音楽");
    let file = image
        .entries()
        .iter()
        .find(|entry| !entry.is_directory())
        .unwrap();
    assert_eq!(file.name(), "écho.txt");
    let file = file.clone();
    let mut contents = Vec::new();
    image.copy_file(&file, &mut contents).unwrap();
    assert_eq!(contents, b"data");
}

#[test]
fn copy_file_rejects_an_entry_from_another_image() {
    let first = IsoImage::new(Cursor::new(sample_iso(false))).unwrap();
    let foreign = first.entries()[0].clone();
    let mut second_bytes = sample_iso(false);
    let file_record = 18 * LOGICAL_BLOCK_SIZE as usize + 68;
    second_bytes[file_record + 33] = b'B';
    let mut second = IsoImage::new(Cursor::new(second_bytes)).unwrap();

    assert!(matches!(
        second.copy_file(&foreign, &mut Vec::new()),
        Err(iso9660::IsoError::Invalid(
            "entry does not belong to this image"
        ))
    ));
}

#[test]
fn rejects_disagreeing_dual_endian_fields() {
    let mut bytes = sample_iso(false);
    bytes[16 * LOGICAL_BLOCK_SIZE as usize + 84] ^= 1;
    assert!(matches!(
        IsoImage::new(Cursor::new(bytes)),
        Err(iso9660::IsoError::Invalid(_))
    ));
}

#[test]
fn rejects_unsafe_paths_before_creating_the_output_directory() {
    let mut bytes = sample_iso(false);
    let root = 18 * LOGICAL_BLOCK_SIZE as usize;
    let next_record = usize::from(bytes[root]) + usize::from(bytes[root + 34]);
    let unsafe_record = record(b"../ESCAPE.TXT;1", 19, 4, 0);
    bytes[root + next_record..root + next_record + unsafe_record.len()]
        .copy_from_slice(&unsafe_record);
    let mut image = IsoImage::new(Cursor::new(bytes)).unwrap();
    let output = temp_path("unsafe");
    assert!(matches!(
        image.extract_to(&output),
        Err(iso9660::IsoError::Unsupported(_))
    ));
    assert!(!output.exists());
}

#[test]
fn copies_consecutive_multi_extent_files_in_order() {
    let mut bytes = sample_iso(false);
    let root = 18 * LOGICAL_BLOCK_SIZE as usize;
    let file_start = root + usize::from(bytes[root]) + usize::from(bytes[root + 34]);
    let first = record(b"TEST.TXT;1", 19, 4, 0x80);
    let last = record(b"TEST.TXT;1", 20, 3, 0);
    bytes[file_start..file_start + first.len()].copy_from_slice(&first);
    bytes[file_start + first.len()..file_start + first.len() + last.len()].copy_from_slice(&last);
    bytes[20 * LOGICAL_BLOCK_SIZE as usize..20 * LOGICAL_BLOCK_SIZE as usize + 3]
        .copy_from_slice(b"end");
    let mut image = IsoImage::new(Cursor::new(bytes)).unwrap();
    let file = image
        .entries()
        .iter()
        .find(|entry| !entry.is_directory())
        .unwrap()
        .clone();
    assert_eq!(file.len(), 7);
    let mut contents = Vec::new();
    image.copy_file(&file, &mut contents).unwrap();
    assert_eq!(contents, b"dataend");
}
