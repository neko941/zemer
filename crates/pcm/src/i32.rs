use crate::{PcmError, conversion::quantize};

pub(super) fn normalize(values: &[i32], output: &mut Vec<f64>) {
    output.extend(
        values
            .iter()
            .map(|&value| f64::from(value) / 2_147_483_648.0),
    );
}

pub(super) fn encode(values: &[f64]) -> Result<Vec<i32>, PcmError> {
    values
        .iter()
        .map(|&value| {
            quantize(
                value,
                2_147_483_648.0,
                0.0,
                -2_147_483_648.0,
                2_147_483_647.0,
            )
        })
        .collect()
}
