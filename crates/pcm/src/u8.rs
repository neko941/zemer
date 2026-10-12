use crate::{sample_conversion::quantize, PcmError};

pub(super) fn normalize(values: &[u8], output: &mut Vec<f64>) {
    output.extend(values.iter().map(|&value| (f64::from(value) - 128.0) / 128.0));
}

pub(super) fn encode(values: &[f64]) -> Result<Vec<u8>, PcmError> {
    values
        .iter()
        .map(|&value| quantize(value, 128.0, 128.0, 0.0, 255.0).map(|sample| sample as u8))
        .collect()
}
