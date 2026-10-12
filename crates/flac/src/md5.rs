//! Incremental MD5 for the checksum stored in FLAC STREAMINFO.

#[derive(Clone)]
pub(crate) struct Md5 {
    state: [u32; 4],
    length: u64,
    buffer: [u8; 64],
    buffered: usize,
}

impl Md5 {
    pub(crate) fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            length: 0,
            buffer: [0; 64],
            buffered: 0,
        }
    }

    pub(crate) fn update(&mut self, mut data: &[u8]) {
        if data.is_empty() {
            return;
        }
        self.length = self.length.wrapping_add(data.len() as u64);
        if self.buffered != 0 {
            let count = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + count].copy_from_slice(&data[..count]);
            self.buffered += count;
            data = &data[count..];
            if self.buffered == 64 {
                compress(&mut self.state, &self.buffer);
                self.buffered = 0;
            } else {
                return;
            }
        }

        let mut blocks = data.chunks_exact(64);
        for block in &mut blocks {
            compress(&mut self.state, block.try_into().unwrap());
        }
        let remainder = blocks.remainder();
        self.buffer[..remainder.len()].copy_from_slice(remainder);
        self.buffered = remainder.len();
    }

    pub(crate) fn finalize(mut self) -> [u8; 16] {
        let bit_length = self.length.wrapping_mul(8);
        self.buffer[self.buffered] = 0x80;
        self.buffered += 1;
        if self.buffered > 56 {
            self.buffer[self.buffered..].fill(0);
            compress(&mut self.state, &self.buffer);
            self.buffer = [0; 64];
        } else {
            self.buffer[self.buffered..56].fill(0);
        }
        self.buffer[56..].copy_from_slice(&bit_length.to_le_bytes());
        compress(&mut self.state, &self.buffer);

        let mut digest = [0; 16];
        for (word, bytes) in self.state.into_iter().zip(digest.chunks_exact_mut(4)) {
            bytes.copy_from_slice(&word.to_le_bytes());
        }
        digest
    }
}

fn compress(state: &mut [u32; 4], block: &[u8; 64]) {
    const SHIFT: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const TABLE: [u32; 64] = [
        0xd76a_a478,
        0xe8c7_b756,
        0x2420_70db,
        0xc1bd_ceee,
        0xf57c_0faf,
        0x4787_c62a,
        0xa830_4613,
        0xfd46_9501,
        0x6980_98d8,
        0x8b44_f7af,
        0xffff_5bb1,
        0x895c_d7be,
        0x6b90_1122,
        0xfd98_7193,
        0xa679_438e,
        0x49b4_0821,
        0xf61e_2562,
        0xc040_b340,
        0x265e_5a51,
        0xe9b6_c7aa,
        0xd62f_105d,
        0x0244_1453,
        0xd8a1_e681,
        0xe7d3_fbc8,
        0x21e1_cde6,
        0xc337_07d6,
        0xf4d5_0d87,
        0x455a_14ed,
        0xa9e3_e905,
        0xfcefa3f8,
        0x676f_02d9,
        0x8d2a_4c8a,
        0xfffa_3942,
        0x8771_f681,
        0x6d9d_6122,
        0xfde5_380c,
        0xa4be_ea44,
        0x4bde_cfa9,
        0xf6bb_4b60,
        0xbebf_bc70,
        0x289b_7ec6,
        0xeaa1_27fa,
        0xd4ef_3085,
        0x0488_1d05,
        0xd9d4_d039,
        0xe6db_99e5,
        0x1fa2_7cf8,
        0xc4ac_5665,
        0xf429_2244,
        0x432a_ff97,
        0xab94_23a7,
        0xfc93_a039,
        0x655b_59c3,
        0x8f0c_cc92,
        0xffef_f47d,
        0x8584_5dd1,
        0x6fa8_7e4f,
        0xfe2c_e6e0,
        0xa301_4314,
        0x4e08_11a1,
        0xf753_7e82,
        0xbd3a_f235,
        0x2ad7_d2bb,
        0xeb86_d391,
    ];

    let mut words = [0_u32; 16];
    for (word, bytes) in words.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_le_bytes(bytes.try_into().unwrap());
    }
    let [mut a, mut b, mut c, mut d] = *state;
    for index in 0..64 {
        let (function, word_index) = match index {
            0..=15 => ((b & c) | (!b & d), index),
            16..=31 => ((d & b) | (!d & c), (5 * index + 1) % 16),
            32..=47 => (b ^ c ^ d, (3 * index + 5) % 16),
            _ => (c ^ (b | !d), (7 * index) % 16),
        };
        let next = b.wrapping_add(
            a.wrapping_add(function)
                .wrapping_add(TABLE[index])
                .wrapping_add(words[word_index])
                .rotate_left(SHIFT[index]),
        );
        (a, b, c, d) = (d, next, b, c);
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

#[cfg(test)]
mod tests {
    use super::Md5;

    #[test]
    fn matches_rfc_1321_vectors_and_incremental_updates() {
        let vectors: &[(&[u8], [u8; 16])] = &[
            (
                b"",
                [
                    0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec,
                    0xf8, 0x42, 0x7e,
                ],
            ),
            (
                b"a",
                [
                    0x0c, 0xc1, 0x75, 0xb9, 0xc0, 0xf1, 0xb6, 0xa8, 0x31, 0xc3, 0x99, 0xe2, 0x69,
                    0x77, 0x26, 0x61,
                ],
            ),
            (
                b"abc",
                [
                    0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28,
                    0xe1, 0x7f, 0x72,
                ],
            ),
            (
                b"message digest",
                [
                    0xf9, 0x6b, 0x69, 0x7d, 0x7c, 0xb7, 0x93, 0x8d, 0x52, 0x5a, 0x2f, 0x31, 0xaa,
                    0xf1, 0x61, 0xd0,
                ],
            ),
            (
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                [
                    0x57, 0xed, 0xf4, 0xa2, 0x2b, 0xe3, 0xc9, 0x55, 0xac, 0x49, 0xda, 0x2e, 0x21,
                    0x07, 0xb6, 0x7a,
                ],
            ),
        ];
        for &(message, expected) in vectors {
            let mut digest = Md5::new();
            for byte in message {
                digest.update(std::slice::from_ref(byte));
            }
            assert_eq!(digest.finalize(), expected, "message {message:?}");
        }
    }
}
