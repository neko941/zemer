use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use dwvw::{ChannelEncoder, DwvwError};
use zemer_core::AudioError;

static NEXT_SPOOL_ID: AtomicU64 = AtomicU64::new(0);

pub(super) struct DwvwSpool {
    bit_depth: u8,
    channels: Vec<SampleSpool>,
    samples_per_channel: u64,
}

impl DwvwSpool {
    pub(super) fn new(channels: usize, bit_depth: u8) -> Result<Self, AudioError> {
        let mut spools = Vec::new();
        spools
            .try_reserve_exact(channels)
            .map_err(|_| AudioError::InvalidData("AIFC DWVW spool is too large"))?;
        for _ in 0..channels {
            spools.push(SampleSpool::new()?);
        }
        Ok(Self {
            bit_depth,
            channels: spools,
            samples_per_channel: 0,
        })
    }

    pub(super) fn write_interleaved(&mut self, samples: &[i32]) -> Result<(), AudioError> {
        let channels = self.channels.len();
        if channels == 0 || samples.len() % channels != 0 {
            return Err(AudioError::InvalidData("invalid AIFC DWVW sample block"));
        }
        let frames = samples.len() / channels;
        for (channel, spool) in self.channels.iter_mut().enumerate() {
            for frame in 0..frames {
                spool
                    .writer
                    .as_mut()
                    .ok_or(AudioError::InvalidData("AIFC DWVW spool is closed"))?
                    .write_all(&samples[frame * channels + channel].to_le_bytes())?;
            }
        }
        self.samples_per_channel = self
            .samples_per_channel
            .checked_add(frames as u64)
            .ok_or(AudioError::InvalidData("AIFC DWVW sample count overflow"))?;
        Ok(())
    }

    pub(super) fn finish_to(mut self, output: &mut impl Write) -> Result<(), AudioError> {
        for spool in &mut self.channels {
            let mut input = spool.reader()?;
            let mut encoder =
                ChannelEncoder::new(&mut *output, self.bit_depth).map_err(map_dwvw_error)?;
            let mut raw = [0; 8192 * 4];
            let mut remaining = self.samples_per_channel;
            while remaining != 0 {
                let count = remaining.min(8192) as usize;
                let size = count * 4;
                input.read_exact(&mut raw[..size])?;
                let mut samples = Vec::new();
                samples
                    .try_reserve_exact(count)
                    .map_err(|_| AudioError::InvalidData("AIFC DWVW block is too large"))?;
                samples.extend(
                    raw[..size]
                        .chunks_exact(4)
                        .map(|bytes| i32::from_le_bytes(bytes.try_into().unwrap())),
                );
                encoder.write_samples(&samples).map_err(map_dwvw_error)?;
                remaining -= count as u64;
            }
            let _ = encoder.finish().map_err(map_dwvw_error)?;
        }
        Ok(())
    }
}

struct SampleSpool {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
}

impl SampleSpool {
    fn new() -> Result<Self, AudioError> {
        for _ in 0..128 {
            let id = NEXT_SPOOL_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("zemer-dwvw-{}-{id}.tmp", std::process::id()));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        writer: Some(BufWriter::new(file)),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(AudioError::Io(error)),
            }
        }
        Err(AudioError::InvalidData(
            "could not create a unique AIFC DWVW spool",
        ))
    }

    fn reader(&mut self) -> Result<BufReader<File>, AudioError> {
        let writer = self
            .writer
            .take()
            .ok_or(AudioError::InvalidData("AIFC DWVW spool is closed"))?;
        let mut file = writer
            .into_inner()
            .map_err(|error| AudioError::Io(error.into_error()))?;
        file.seek(SeekFrom::Start(0))?;
        Ok(BufReader::new(file))
    }
}

impl Drop for SampleSpool {
    fn drop(&mut self) {
        self.writer.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) fn map_dwvw_error(error: DwvwError) -> AudioError {
    match error {
        DwvwError::Io(error) => AudioError::Io(error),
        DwvwError::InvalidBitDepth => AudioError::InvalidData("invalid AIFC DWVW bit depth"),
        DwvwError::InvalidSample => AudioError::InvalidData("AIFC DWVW sample is out of range"),
        DwvwError::Truncated => AudioError::InvalidData("truncated AIFC DWVW stream"),
        DwvwError::InvalidState => AudioError::InvalidData("invalid AIFC DWVW encoder state"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::SampleSpool;

    #[test]
    fn temporary_audio_spools_are_owner_only() {
        let spool = SampleSpool::new().unwrap();
        assert_eq!(
            std::fs::metadata(&spool.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
