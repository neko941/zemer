use std::io::Write;

use zemer_core::AudioError;

/// The fixed-size AES channel-status payload from the AIFC `AESD` chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AiffAudioRecording {
    pub channel_status: [u8; 24],
}

pub(super) fn parse_audio_recording(bytes: &[u8]) -> Result<AiffAudioRecording, AudioError> {
    let channel_status = bytes
        .try_into()
        .map_err(|_| AudioError::InvalidFormat("AIFF AESD chunk must be 24 bytes"))?;
    Ok(AiffAudioRecording { channel_status })
}

pub(super) fn write_audio_recording_chunk(
    writer: &mut impl Write,
    recording: &AiffAudioRecording,
) -> Result<(), AudioError> {
    writer.write_all(b"AESD\0\0\0\x18")?;
    writer.write_all(&recording.channel_status)?;
    Ok(())
}
