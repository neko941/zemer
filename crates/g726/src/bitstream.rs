pub(crate) fn read_code(bytes: &[u8], bit_offset: usize, width: u8) -> u8 {
    let mut value = 0;
    for bit in 0..usize::from(width) {
        let position = bit_offset + bit;
        value = (value << 1) | ((bytes[position / 8] >> (7 - position % 8)) & 1);
    }
    value
}

pub(crate) fn read_code_lsb(bytes: &[u8], bit_offset: usize, width: u8) -> u8 {
    let mut value = 0;
    for bit in 0..usize::from(width) {
        let position = bit_offset + bit;
        value |= ((bytes[position / 8] >> (position % 8)) & 1) << bit;
    }
    value
}

#[cfg(test)]
pub(crate) fn pack_codes(codes: &[u8], width: u8) -> Vec<u8> {
    let mut bytes = vec![0; (codes.len() * usize::from(width)).div_ceil(8)];
    for (index, &code) in codes.iter().enumerate() {
        for bit in 0..usize::from(width) {
            let source = (code >> (usize::from(width) - bit - 1)) & 1;
            let position = index * usize::from(width) + bit;
            bytes[position / 8] |= source << (7 - position % 8);
        }
    }
    bytes
}

#[cfg(test)]
pub(crate) fn pack_codes_lsb(codes: &[u8], width: u8) -> Vec<u8> {
    let mut bytes = vec![0; (codes.len() * usize::from(width)).div_ceil(8)];
    for (index, &code) in codes.iter().enumerate() {
        for bit in 0..usize::from(width) {
            let position = index * usize::from(width) + bit;
            bytes[position / 8] |= ((code >> bit) & 1) << (position % 8);
        }
    }
    bytes
}
