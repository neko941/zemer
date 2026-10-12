use crate::{AudioSpec, PcmBuffer, PcmError, SampleFormat, Samples, f32, f64, i16, i24, i32, u8};

/// Converts sample values without changing the sample rate or channel count.
///
/// Integer output rounds to the nearest representable value. Float samples
/// outside `[-1.0, 1.0]` cannot be converted to integer PCM.
pub fn convert_sample_format(
    audio: &PcmBuffer,
    target: SampleFormat,
) -> Result<PcmBuffer, PcmError> {
    if audio.spec().sample_format() == target {
        return Ok(audio.clone());
    }

    let mut samples = Vec::new();
    samples
        .try_reserve_exact(audio.samples().len())
        .map_err(|_| PcmError("audio block is too large"))?;
    normalize(audio.samples(), &mut samples);
    let converted = encode(&samples, target)?;
    let spec = AudioSpec::new_with_speaker_mask(
        audio.spec().sample_rate(),
        audio.spec().channels(),
        target,
        audio.spec().speaker_mask(),
    )?;
    PcmBuffer::new(spec, converted)
}

fn normalize(samples: &Samples, output: &mut Vec<f64>) {
    match samples {
        Samples::U8(values) => u8::normalize(values, output),
        Samples::I16(values) => i16::normalize(values, output),
        Samples::I24(values) => i24::normalize(values, output),
        Samples::I32(values) => i32::normalize(values, output),
        Samples::F32(values) => f32::normalize(values, output),
        Samples::F64(values) => f64::normalize(values, output),
    }
}

fn encode(samples: &[f64], format: SampleFormat) -> Result<Samples, PcmError> {
    match format {
        SampleFormat::PcmU8 => Ok(Samples::U8(u8::encode(samples)?)),
        SampleFormat::PcmI16 => Ok(Samples::I16(i16::encode(samples)?)),
        SampleFormat::PcmI24 => Ok(Samples::I24(i24::encode(samples)?)),
        SampleFormat::PcmI32 => Ok(Samples::I32(i32::encode(samples)?)),
        SampleFormat::Float32 => Ok(Samples::F32(f32::encode(samples)?)),
        SampleFormat::Float64 => Ok(Samples::F64(f64::encode(samples))),
    }
}

pub(crate) fn quantize(
    sample: f64,
    scale: f64,
    offset: f64,
    minimum: f64,
    maximum: f64,
) -> Result<i32, PcmError> {
    if !sample.is_finite() || !(-1.0..=1.0).contains(&sample) {
        return Err(PcmError(
            "float samples outside [-1.0, 1.0] cannot be converted to integer PCM",
        ));
    }
    Ok((sample * scale + offset).round().clamp(minimum, maximum) as i32)
}

#[cfg(test)]
mod tests {
    use crate::{AudioSpec, PcmBuffer, SampleFormat, Samples, convert_sample_format};

    #[test]
    fn every_sample_variant_round_trips_through_float64() {
        let cases = [
            (SampleFormat::PcmU8, Samples::U8(vec![0, 128, 255])),
            (
                SampleFormat::PcmI16,
                Samples::I16(vec![i16::MIN, 0, i16::MAX]),
            ),
            (
                SampleFormat::PcmI24,
                Samples::I24(vec![-8_388_608, 0, 8_388_607]),
            ),
            (
                SampleFormat::PcmI32,
                Samples::I32(vec![i32::MIN, 0, i32::MAX]),
            ),
            (
                SampleFormat::Float32,
                Samples::F32(vec![-1.0, 0.0, 0.25, 1.0]),
            ),
            (
                SampleFormat::Float64,
                Samples::F64(vec![-1.0, 0.0, 0.25, 1.0]),
            ),
        ];

        for (format, samples) in cases {
            let audio =
                PcmBuffer::new(AudioSpec::new(48_000, 1, format).unwrap(), samples).unwrap();
            let float = convert_sample_format(&audio, SampleFormat::Float64).unwrap();
            assert_eq!(convert_sample_format(&float, format).unwrap(), audio);
        }
    }
}
