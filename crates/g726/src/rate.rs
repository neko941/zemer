#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum G726Rate {
    Kbps16,
    Kbps24,
    Kbps32,
    Kbps40,
}

impl G726Rate {
    pub const fn bits_per_sample(self) -> u8 {
        match self {
            Self::Kbps16 => 2,
            Self::Kbps24 => 3,
            Self::Kbps32 => 4,
            Self::Kbps40 => 5,
        }
    }
}
