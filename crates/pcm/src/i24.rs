use crate::{PcmError, conversion::quantize};

pub(super) fn normalize(values: &[i32], output: &mut Vec<f64>) {
    output.extend(values.iter().map(|&value| f64::from(value) / 8_388_608.0));
}

pub(super) fn encode(values: &[f64]) -> Result<Vec<i32>, PcmError> {
    values
        .iter()
        .map(|&value| quantize(value, 8_388_608.0, 0.0, -8_388_608.0, 8_388_607.0))
        .collect()
}
