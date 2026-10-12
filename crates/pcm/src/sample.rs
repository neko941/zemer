#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SampleFormat {
    PcmU8,
    PcmI16,
    PcmI24,
    PcmI32,
    Float32,
    Float64,
}

