use crate::PcmError;

pub(super) fn normalize(values: &[f32], output: &mut Vec<f64>) {
    output.extend(values.iter().map(|&value| f64::from(value)));
}

pub(super) fn encode(values: &[f64]) -> Result<Vec<f32>, PcmError> {
    values
        .iter()
        .map(|&sample| {
            let value = sample as f32;
            if sample.is_finite() && !value.is_finite() {
                return Err(PcmError("sample is outside the Float32 range"));
            }
            Ok(value)
        })
        .collect()
}
