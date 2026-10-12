/// A DSDIFF `COMT` comment record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DsdiffComment {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub kind: u16,
    pub reference: u16,
    pub text: String,
}
