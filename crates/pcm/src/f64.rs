pub(super) fn normalize(values: &[f64], output: &mut Vec<f64>) {
    output.extend_from_slice(values);
}

pub(super) fn encode(values: &[f64]) -> Vec<f64> {
    values.to_vec()
}
