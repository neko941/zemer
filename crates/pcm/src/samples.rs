#[derive(Clone, Debug, PartialEq)]
pub enum Samples {
    U8(Vec<u8>),
    I16(Vec<i16>),
    I24(Vec<i32>),
    I32(Vec<i32>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

impl Samples {
    pub fn len(&self) -> usize {
        match self {
            Self::U8(values) => values.len(),
            Self::I16(values) => values.len(),
            Self::I24(values) | Self::I32(values) => values.len(),
            Self::F32(values) => values.len(),
            Self::F64(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn matches(&self, format: SampleFormat) -> bool {
        matches!(
            (self, format),
            (Self::U8(_), SampleFormat::PcmU8)
                | (Self::I16(_), SampleFormat::PcmI16)
                | (Self::I24(_), SampleFormat::PcmI24)
                | (Self::I32(_), SampleFormat::PcmI32)
                | (Self::F32(_), SampleFormat::Float32)
                | (Self::F64(_), SampleFormat::Float64)
        )
    }

    pub(crate) fn valid(&self) -> bool {
        match self {
            Self::I24(values) => values
                .iter()
                .all(|&sample| (-8_388_608..=8_388_607).contains(&sample)),
            _ => true,
        }
    }
}
