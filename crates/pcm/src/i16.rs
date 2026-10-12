use crate::{PcmError, sample_conversion::quantize};

pub(super) fn normalize(values: &[i16], output: &mut Vec<f64>) {
    output.extend(values.iter().map(|&value| f64::from(value) / 32_768.0));
}

pub(super) fn encode(values: &[f64]) -> Result<Vec<i16>, PcmError> {
    values
        .iter()
        .map(|&value| {
            quantize(value, 32_768.0, 0.0, -32_768.0, 32_767.0).map(|sample| sample as i16)
        })
        .collect()
}
