use crate::G726Rate;

const MASK16: u32 = 0xffff;
const MASK15: u32 = 0x7fff;
const DQLN_40: [u16; 32] = [
    2048, 4030, 28, 104, 169, 224, 274, 318, 358, 395, 429, 459, 488, 514, 539, 566, 566, 539, 514,
    488, 459, 429, 395, 358, 318, 274, 224, 169, 104, 28, 4030, 2048,
];
const DQLN_32: [u16; 16] = [
    2048, 4, 135, 213, 273, 323, 373, 425, 425, 373, 323, 273, 213, 135, 4, 2048,
];
const DQLN_24: [u16; 8] = [2048, 135, 273, 373, 373, 273, 135, 2048];
const DQLN_16: [u16; 4] = [116, 365, 365, 116];

const WI_40: [u16; 16] = [
    14, 14, 24, 39, 40, 41, 58, 100, 141, 179, 219, 280, 358, 440, 529, 696,
];
const WI_32: [u16; 8] = [4084, 18, 41, 64, 112, 198, 355, 1122];
const WI_24: [u16; 4] = [4092, 30, 137, 582];
const WI_16: [u16; 2] = [4074, 439];

const FI_40: [u8; 16] = [0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 3, 4, 5, 6, 6];
const FI_32: [u8; 8] = [0, 0, 0, 1, 1, 1, 3, 7];
const FI_24: [u8; 4] = [0, 1, 2, 7];
const FI_16: [u8; 2] = [0, 7];

#[derive(Debug)]
pub(crate) struct DecoderState {
    rate: G726Rate,
    a: [u16; 2],
    b: [u16; 6],
    dq: [u16; 6],
    sr: [u16; 2],
    pk: [u8; 3],
    yu: u16,
    yl: u32,
    dms: u16,
    dml: u16,
    ap: u16,
    td: u8,
}

impl DecoderState {
    pub(crate) fn new(rate: G726Rate) -> Self {
        Self {
            rate,
            a: [0; 2],
            b: [0; 6],
            dq: [32; 6],
            sr: [32; 2],
            pk: [0; 3],
            yu: 544,
            yl: 34_816,
            dms: 0,
            dml: 0,
            ap: 0,
            td: 0,
        }
    }

    pub(crate) fn decode(&mut self, code: u8) -> i16 {
        self.decode_trace(code).0
    }

    pub(crate) fn encode(&mut self, sample: i16) -> u8 {
        let (se, _, y) = self.estimate();
        let sl = (sample >> 2) as u16;
        let se = if se & 0x4000 == 0 { se } else { se | 0x8000 };
        let difference = sl.wrapping_sub(se);
        let sign = (difference >> 15) as u8;
        let magnitude = if sign == 0 {
            difference & MASK15 as u16
        } else {
            difference.wrapping_neg() & MASK15 as u16
        };
        let logarithm = log2_q7(magnitude);
        let normalized = (u32::from(logarithm) + 4096 - (u32::from(y) >> 2)) & 4095;
        let index = quantizer_index(self.rate, sign, normalized as u16);
        let half = 1 << (self.rate.bits_per_sample() - 1);
        let code = if index < half {
            half | index
        } else {
            index - half
        };
        self.decode_trace(code);
        code
    }

    fn decode_trace(&mut self, code: u8) -> (i16, u16, u16, u16) {
        let bits = self.rate.bits_per_sample();
        let (dqln, dqs) = reconst(self.rate, code);
        let (se, sez, y) = self.estimate();
        let dql = (u32::from(dqln) + (u32::from(y) >> 2)) & 4095;
        let dq = antilog(dql as u16, dqs);

        let sr = addb(dq, se);
        let (pk0, sigpk) = addc(dq, sez);
        let next_pk = [pk0, self.pk[0], self.pk[1]];

        let wi = function_w(self.rate, code);
        let fi = function_f(self.rate, code);
        let yut = filter_d(wi, y);
        let yup = limit_b(yut);
        let ylp = filter_e(yup, self.yl);
        let dms = filter_a(fi, self.dms);
        let dml = filter_b(fi, self.dml);
        let tdp = tone(limit_c(update_a2(next_pk, self.a, sigpk)));
        let ax = subtype_c(dms, dml, tdp, y);
        let app = filter_c(ax, self.ap);
        let tr = transition(self.td, self.yl, dq);
        let tdr = if tr == 0 { tdp } else { 0 };

        let a1t = update_a1(next_pk[0], next_pk[1], self.a[0], sigpk);
        let a2t = update_a2(next_pk, self.a, sigpk);
        let a2p = limit_c(a2t);
        let a1p = limit_d(a1t, a2p);
        self.a = [if tr != 0 { 0 } else { a1p }, if tr != 0 { 0 } else { a2p }];

        let mut next_b = [0; 6];
        for (index, value) in next_b.iter_mut().enumerate() {
            let sign = (dq >> 15) as u8;
            let delayed_sign = (self.dq[index] >> 10) as u8;
            *value = if tr != 0 {
                0
            } else {
                update_b(self.b[index], sign ^ delayed_sign, dq, bits)
            };
        }
        self.b = next_b;

        self.yu = yup;
        self.yl = ylp;
        self.dms = dms;
        self.dml = dml;
        self.ap = if tr != 0 { 256 } else { app };
        self.td = tdr;
        self.pk = next_pk;
        self.sr = [float_b(sr), self.sr[0]];
        self.dq = [
            float_a(dq),
            self.dq[0],
            self.dq[1],
            self.dq[2],
            self.dq[3],
            self.dq[4],
        ];

        (limo(sr) * 4, sr, se, y)
    }

    fn estimate(&self) -> (u16, u16, u16) {
        let al = if self.ap >= 256 { 64 } else { self.ap >> 2 };
        let y = mix(al, self.yu, self.yl);
        let mut sezi = 0_u32;
        for index in 0..6 {
            sezi = (sezi + u32::from(fmult(self.b[index], self.dq[index]))) & MASK16;
        }
        let sezi = sezi as u16;
        let sez = sezi >> 1;
        let wa2 = fmult(self.a[1], self.sr[1]);
        let wa1 = fmult(self.a[0], self.sr[0]);
        let se = (((u32::from(sezi) + u32::from(wa2)) & MASK16) + u32::from(wa1)) & MASK16;
        ((se >> 1) as u16, sez, y)
    }
}

fn log2_q7(magnitude: u16) -> u16 {
    if magnitude == 0 {
        return 0;
    }
    let exponent = 15 - magnitude.leading_zeros();
    let base = 1_u32 << exponent;
    let fraction = ((u32::from(magnitude) - base) << 7) / base;
    ((exponent << 7) + fraction) as u16
}

fn quantizer_index(rate: G726Rate, sign: u8, dln: u16) -> u8 {
    match rate {
        G726Rate::Kbps16 => match (sign, dln) {
            (0, 261..=2047) => 3,
            (0, _) => 2,
            (1, 261..=2047) => 0,
            (1, _) => 1,
            _ => unreachable!(),
        },
        G726Rate::Kbps24 => match (sign, dln) {
            (0, 331..=2047) => 7,
            (0, 218..=330) => 6,
            (0, 8..=217) => 5,
            (0, _) => 3,
            (1, 331..=2047) => 0,
            (1, 218..=330) => 1,
            (1, 8..=217) => 2,
            (1, _) => 3,
            _ => unreachable!(),
        },
        G726Rate::Kbps32 => match (sign, dln) {
            (0, 2048..=3971) | (1, 2048..=3971) => 7,
            (0, 400..=2047) => 15,
            (0, 349..=399) => 14,
            (0, 300..=348) => 13,
            (0, 246..=299) => 12,
            (0, 178..=245) => 11,
            (0, 80..=177) => 10,
            (0, 3972..=4095) | (0, 0..=79) => 9,
            (1, 400..=2047) => 0,
            (1, 349..=399) => 1,
            (1, 300..=348) => 2,
            (1, 246..=299) => 3,
            (1, 178..=245) => 4,
            (1, 80..=177) => 5,
            (1, 3972..=4095) | (1, 0..=79) => 6,
            _ => unreachable!(),
        },
        G726Rate::Kbps40 => match (sign, dln) {
            (0, 2048..=3973) | (1, 2048..=3973) => 15,
            (0, 3974..=4079) => 17,
            (1, 3974..=4079) => 14,
            (0, 553..=2047) => 31,
            (0, 528..=552) => 30,
            (0, 502..=527) => 29,
            (0, 475..=501) => 28,
            (0, 445..=474) => 27,
            (0, 413..=444) => 26,
            (0, 378..=412) => 25,
            (0, 339..=377) => 24,
            (0, 298..=338) => 23,
            (0, 250..=297) => 22,
            (0, 198..=249) => 21,
            (0, 139..=197) => 20,
            (0, 68..=138) => 19,
            (0, _) => 18,
            (1, 553..=2047) => 0,
            (1, 528..=552) => 1,
            (1, 502..=527) => 2,
            (1, 475..=501) => 3,
            (1, 445..=474) => 4,
            (1, 413..=444) => 5,
            (1, 378..=412) => 6,
            (1, 339..=377) => 7,
            (1, 298..=338) => 8,
            (1, 250..=297) => 9,
            (1, 198..=249) => 10,
            (1, 139..=197) => 11,
            (1, 68..=138) => 12,
            (1, _) => 13,
            _ => unreachable!(),
        },
    }
}

fn reconst(rate: G726Rate, code: u8) -> (u16, u8) {
    let (table, width) = match rate {
        G726Rate::Kbps16 => (&DQLN_16[..], 2),
        G726Rate::Kbps24 => (&DQLN_24[..], 3),
        G726Rate::Kbps32 => (&DQLN_32[..], 4),
        G726Rate::Kbps40 => (&DQLN_40[..], 5),
    };
    let sign = code >> (width - 1);
    (table[usize::from(code)], sign)
}

fn limo(sr: u16) -> i16 {
    if (8192..32768).contains(&sr) {
        8191
    } else if (32768..57344).contains(&sr) {
        -8192
    } else {
        let value = (sr & 16383) << 2;
        (value as i16) >> 2
    }
}

fn antilog(dql: u16, sign: u8) -> u16 {
    let dql_sign = (dql >> 11) & 1;
    let exponent = u32::from((dql >> 7) & 15);
    let mantissa = u32::from(dql & 127);
    let magnitude = if dql_sign == 0 {
        let linear = (128 + mantissa) << 7;
        if exponent <= 14 {
            (linear >> (14 - exponent)) & MASK15
        } else {
            (linear << (exponent - 14)) & MASK15
        }
    } else {
        0
    };
    ((u32::from(sign) << 15) | magnitude) as u16
}

fn mix(al: u16, yu: u16, yl: u32) -> u16 {
    let difference = (u32::from(yu) + 16_384 - (yl >> 6)) & 16_383;
    let sign = difference >> 13;
    let magnitude = if sign == 0 {
        difference
    } else {
        (16_384 - difference) & 8191
    };
    let product = (magnitude * u32::from(al)) >> 6;
    let product = if sign == 0 {
        product
    } else {
        16_384_u32.wrapping_sub(product) & 16_383
    };
    (((yl >> 6) + product) & 8191) as u16
}

fn filter_d(wi: u16, y: u16) -> u16 {
    let difference = ((u32::from(wi) << 5) + 131_072 - u32::from(y)) & 131_071;
    let sign = difference >> 16;
    let extension = if sign == 0 {
        difference >> 5
    } else {
        (difference >> 5) + 4096
    };
    ((u32::from(y) + extension) & 8191) as u16
}

fn limit_b(yut: u16) -> u16 {
    let geul = ((u32::from(yut) + 11_264) & 16_383) >> 13;
    let gell = ((u32::from(yut) + 15_840) & 16_383) >> 13;
    if gell == 1 {
        544
    } else if geul == 0 {
        5120
    } else {
        yut
    }
}

fn filter_e(yup: u16, yl: u32) -> u32 {
    let difference = (u32::from(yup) + ((1_048_576 - yl) >> 6)) & 16_383;
    let extension = if difference >> 13 == 0 {
        difference
    } else {
        difference + 507_904
    };
    (yl + extension) & 524_287
}

fn function_w(rate: G726Rate, code: u8) -> u16 {
    let width = rate.bits_per_sample();
    let mask = (1_u8 << (width - 1)) - 1;
    let index = if code >> (width - 1) == 0 {
        code & mask
    } else {
        (!code) & mask
    };
    match rate {
        G726Rate::Kbps16 => WI_16[usize::from(index)],
        G726Rate::Kbps24 => WI_24[usize::from(index)],
        G726Rate::Kbps32 => WI_32[usize::from(index)],
        G726Rate::Kbps40 => WI_40[usize::from(index)],
    }
}

fn filter_a(fi: u8, dms: u16) -> u16 {
    let difference = ((u32::from(fi) << 9) + 8192 - u32::from(dms)) & 8191;
    let extension = if difference >> 12 == 0 {
        difference >> 5
    } else {
        (difference >> 5) + 3840
    };
    ((u32::from(dms) + extension) & 4095) as u16
}

fn filter_b(fi: u8, dml: u16) -> u16 {
    let difference = ((u32::from(fi) << 11) + 32_768 - u32::from(dml)) & 32_767;
    let extension = if difference >> 14 == 0 {
        difference >> 7
    } else {
        (difference >> 7) + 16_128
    };
    ((u32::from(dml) + extension) & 16_383) as u16
}

fn function_f(rate: G726Rate, code: u8) -> u8 {
    let width = rate.bits_per_sample();
    let mask = (1_u8 << (width - 1)) - 1;
    let index = if code >> (width - 1) == 0 {
        code & mask
    } else {
        (!code) & mask
    };
    match rate {
        G726Rate::Kbps16 => FI_16[usize::from(index)],
        G726Rate::Kbps24 => FI_24[usize::from(index)],
        G726Rate::Kbps32 => FI_32[usize::from(index)],
        G726Rate::Kbps40 => FI_40[usize::from(index)],
    }
}

fn filter_c(ax: u16, ap: u16) -> u16 {
    let difference = ((u32::from(ax) << 9) + 2048 - u32::from(ap)) & 2047;
    let extension = if difference >> 10 == 0 {
        difference >> 4
    } else {
        (difference >> 4) + 896
    };
    ((u32::from(ap) + extension) & 1023) as u16
}

fn subtype_c(dms: u16, dml: u16, tdp: u8, y: u16) -> u16 {
    let difference = ((u32::from(dms) << 2) + 32_768 - u32::from(dml)) & 32_767;
    let magnitude = if difference >> 14 == 0 {
        difference
    } else {
        (32_768 - difference) & 16_383
    };
    if y >= 1536 && magnitude < u32::from(dml >> 3) && tdp == 0 {
        0
    } else {
        1
    }
}

fn tone(a2p: u16) -> u8 {
    u8::from((32_768..53_760).contains(&a2p))
}

fn transition(td: u8, yl: u32, dq: u16) -> u8 {
    let magnitude = u32::from(dq & 32_767);
    let ylint = yl >> 15;
    let ylfrac = (yl >> 10) & 31;
    let threshold1 = (32 + ylfrac) << ylint;
    let threshold2 = if ylint > 9 { 31 << 10 } else { threshold1 };
    let dqthr = (threshold2 + (threshold2 >> 1)) >> 1;
    u8::from(magnitude > dqthr && td != 0)
}

fn limit_c(a2t: u16) -> u16 {
    if (32_768..=53_248).contains(&a2t) {
        53_248
    } else if (12_288..=32_767).contains(&a2t) {
        12_288
    } else {
        a2t
    }
}

fn limit_d(a1t: u16, a2p: u16) -> u16 {
    let upper = (15_360 + 65_536 - u32::from(a2p)) & MASK16;
    let lower = (u32::from(a2p) + 65_536 - 15_360) & MASK16;
    if a1t >= 32_768 && u32::from(a1t) <= lower {
        lower as u16
    } else if u32::from(a1t) >= upper && a1t <= 32_767 {
        upper as u16
    } else {
        a1t
    }
}

fn update_a1(pk0: u8, pk1: u8, a1: u16, sigpk: u8) -> u16 {
    let pks = pk0 ^ pk1;
    let uga = if sigpk != 0 {
        0
    } else if pks == 0 {
        192
    } else {
        65_344
    };
    let sign = a1 >> 15;
    let leak = if sign == 0 {
        (65_536 - (u32::from(a1) >> 8)) & MASK16
    } else {
        (65_536 - ((u32::from(a1) >> 8) + 65_280)) & MASK16
    };
    ((u32::from(a1) + ((uga + leak) & MASK16)) & MASK16) as u16
}

fn update_a2(pk: [u8; 3], a: [u16; 2], sigpk: u8) -> u16 {
    let pks1 = pk[0] ^ pk[1];
    let pks2 = pk[0] ^ pk[2];
    let uga2a = if pks2 == 0 { 16_384 } else { 114_688 };
    let a1 = a[0];
    let fa1 = if a1 >> 15 == 0 {
        u32::from(a1.min(8191)) << 2
    } else if a1 >= 57_345 {
        (u32::from(a1) << 2) & 131_071
    } else {
        24_577 << 2
    };
    let fa = if pks1 != 0 {
        fa1
    } else {
        (131_072 - fa1) & 131_071
    };
    let uga2b = (uga2a + fa) & 131_071;
    let uga2 = if sigpk != 0 {
        0
    } else if uga2b >> 16 == 0 {
        uga2b >> 7
    } else {
        (uga2b >> 7) + 64_512
    };
    let a2 = a[1];
    let leak = if a2 >> 15 == 0 {
        (65_536 - (u32::from(a2) >> 7)) & MASK16
    } else {
        (65_536 - ((u32::from(a2) >> 7) + 65_024)) & MASK16
    };
    ((u32::from(a2) + ((uga2 + leak) & MASK16)) & MASK16) as u16
}

fn update_b(b: u16, un: u8, dq: u16, bits: u8) -> u16 {
    let magnitude = u32::from(dq & 32_767);
    let uga = if magnitude == 0 {
        0
    } else if un == 0 {
        128
    } else {
        65_408
    };
    let sign = b >> 15;
    let (leak_shift, negative_bias) = if bits == 5 { (9, 65_408) } else { (8, 65_280) };
    let leak = if sign == 0 {
        (65_536 - (u32::from(b) >> leak_shift)) & MASK16
    } else {
        (65_536 - ((u32::from(b) >> leak_shift) + negative_bias)) & MASK16
    };
    ((u32::from(b) + ((uga + leak) & MASK16)) & MASK16) as u16
}

fn float_a(dq: u16) -> u16 {
    float_code(dq >> 15, u32::from(dq & 32_767))
}

fn float_b(sr: u16) -> u16 {
    let sign = sr >> 15;
    let magnitude = if sign == 0 {
        u32::from(sr)
    } else {
        (65_536 - u32::from(sr)) & MASK15
    };
    float_code(sign, magnitude)
}

fn float_code(sign: u16, magnitude: u32) -> u16 {
    let exponent = if magnitude == 0 {
        0
    } else {
        (31 - magnitude.leading_zeros()) + 1
    };
    let mantissa = if magnitude == 0 {
        32
    } else {
        (magnitude << 6) >> exponent
    };
    ((u32::from(sign) << 10) | (exponent << 6) | (mantissa & 63)) as u16
}

fn fmult(coefficient: u16, sample: u16) -> u16 {
    let sign = coefficient >> 15;
    let magnitude = if sign == 0 {
        u32::from(coefficient) >> 2
    } else {
        (16_384 - (u32::from(coefficient) >> 2)) & 8191
    };
    let exponent = if magnitude == 0 {
        0
    } else {
        (31 - magnitude.leading_zeros()) + 1
    };
    let mantissa = if magnitude == 0 {
        32
    } else {
        (magnitude << 6) >> exponent
    };
    let sample_sign = sample >> 10;
    let sample_exponent = (sample >> 6) & 15;
    let sample_mantissa = sample & 63;
    let product_sign = sample_sign ^ sign;
    let product_exponent = u32::from(sample_exponent + exponent as u16);
    let product_mantissa = (u32::from(sample_mantissa) * (mantissa & 63) + 48) >> 4;
    let product_magnitude = if product_exponent <= 26 {
        (product_mantissa << 7) >> (26 - product_exponent)
    } else {
        ((product_mantissa << 7) << (product_exponent - 26)) & MASK15
    };
    let value = if product_sign == 0 {
        product_magnitude
    } else {
        (65_536 - product_magnitude) & MASK16
    };
    value as u16
}

fn addb(dq: u16, se: u16) -> u16 {
    let magnitude = u32::from(dq & 32_767);
    let difference = if dq >> 15 == 0 {
        magnitude
    } else {
        (65_536 - magnitude) & MASK16
    };
    let estimate = if se >> 14 == 0 {
        u32::from(se)
    } else {
        32_768 + u32::from(se)
    };
    ((difference + estimate) & MASK16) as u16
}

fn addc(dq: u16, sez: u16) -> (u8, u8) {
    let magnitude = u32::from(dq & 32_767);
    let difference = if dq >> 15 == 0 {
        magnitude
    } else {
        (65_536 - magnitude) & MASK16
    };
    let estimate = if sez >> 14 == 0 {
        u32::from(sez)
    } else {
        32_768 + u32::from(sez)
    };
    let sum = (difference + estimate) & MASK16;
    ((sum >> 15) as u8, u8::from(sum == 0))
}

#[cfg(test)]
mod sync_tests {
    use super::{DecoderState, G726Rate, limo, quantizer_index};
    use g711::{G711Encoding, decode_alaw, decode_mulaw, encode_alaw, encode_mulaw};
    use std::{env, fs};

    #[test]
    fn annex_a_limiter_clamps_and_sign_extends_14_bit_output() {
        for (input, expected) in [
            (0x1fff, 8191),
            (0x2000, 8191),
            (0x7fff, 8191),
            (0x8000, -8192),
            (0xdfff, -8192),
            (0xe000, -8192),
            (0xe001, -8191),
            (0xffff, -1),
        ] {
            assert_eq!(limo(input), expected, "SR={input:#06x}");
        }
    }

    fn encode(sample: i16, law: G711Encoding) -> u8 {
        match law {
            G711Encoding::ALaw => encode_alaw(sample),
            G711Encoding::MuLaw => encode_mulaw(sample),
        }
    }

    fn decode(code: u8, law: G711Encoding) -> i16 {
        match law {
            G711Encoding::ALaw => decode_alaw(code),
            G711Encoding::MuLaw => decode_mulaw(code),
        }
    }

    fn vector(name: &str) -> Vec<u8> {
        let root = env::var("ZEMER_G726_VECTOR_DIR")
            .expect("set ZEMER_G726_VECTOR_DIR to extracted ITU vectors");
        let path = format!("{root}/{name}");
        let text = fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
        let hex: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
            .collect();
        let (checksum, data) = bytes.split_last().unwrap();
        assert_eq!(
            data.iter().map(|&byte| u64::from(byte)).sum::<u64>() % 255,
            u64::from(*checksum)
        );
        data.to_vec()
    }

    fn check_vector(
        rate: G726Rate,
        law: G711Encoding,
        initialization: Option<&str>,
        input_name: &str,
        output_name: &str,
    ) {
        let codes = vector(input_name);
        let expected = vector(output_name);
        let mut decoder = DecoderState::new(rate);
        if let Some(initialization) = initialization {
            for code in vector(initialization) {
                decoder.decode_trace(code);
            }
        }
        let actual: Vec<u8> = codes
            .iter()
            .copied()
            .map(|code| {
                let trace = decoder.decode_trace(code);
                sync(rate, law, code, trace.1, trace.2, trace.3)
            })
            .collect();
        assert_eq!(
            actual.len(),
            expected.len(),
            "{input_name} → {output_name} length"
        );
        let mismatches: Vec<_> = actual
            .iter()
            .zip(&expected)
            .enumerate()
            .filter_map(|(index, (&actual, &expected))| {
                (actual != expected).then_some((index, codes[index], actual, expected))
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "{input_name} → {output_name}: {:?}",
            &mismatches[..mismatches.len().min(8)]
        );
    }

    fn id(rate: G726Rate, ds: u8, dln: u16) -> u8 {
        quantizer_index(rate, ds, dln)
    }

    fn sync(rate: G726Rate, law: G711Encoding, code: u8, sr: u16, se: u16, y: u16) -> u8 {
        let output = (sr as i16).saturating_mul(4);
        let sp = if sr == 0x8000 {
            // COMPRESS converts -32768 to zero in its 15-bit signed-magnitude field.
            match law {
                G711Encoding::ALaw => 0x55,
                G711Encoding::MuLaw => 0x7f,
            }
        } else {
            encode(output, law)
        };
        let sample = decode(sp, law);
        let magnitude = i32::from(sample).unsigned_abs() >> 2;
        let slx = if sp & 0x80 != 0 {
            magnitude as u16
        } else {
            (16_384 - magnitude) as u16 & 16_383
        };
        let sli = if slx >> 13 == 0 {
            u32::from(slx)
        } else {
            49_152 + u32::from(slx)
        };
        let sei = if se >> 14 == 0 {
            u32::from(se)
        } else {
            32_768 + u32::from(se)
        };
        let dx = (sli + 65_536 - sei) & 65_535;
        let ds = (dx >> 15) as u8;
        let dqm = if ds == 0 { dx } else { (65_536 - dx) & 32_767 };
        let exponent = if dqm == 0 {
            0
        } else {
            31 - dqm.leading_zeros()
        };
        let mantissa = ((dqm << 7) >> exponent) & 127;
        let dlx = (exponent << 7) + mantissa;
        let dlnx = (dlx + 4096 - (u32::from(y) >> 2)) & 4095;
        let decision = id(rate, ds, dlnx as u16);
        let width = rate.bits_per_sample();
        let sign = code >> (width - 1);
        let input_index = if sign == 0 {
            code + (1 << (width - 1))
        } else {
            code & ((1 << (width - 1)) - 1)
        };
        let target = if decision < input_index {
            1
        } else if decision > input_index {
            -1
        } else {
            return sp;
        };
        let zero_code = if target > 0 { 0x7f } else { 0xff };
        (0..=u8::MAX)
            .filter(|&candidate| {
                let level = decode(candidate, law);
                (target > 0 && level > sample) || (target < 0 && level < sample)
            })
            .min_by_key(|&candidate| {
                (
                    decode(candidate, law).abs_diff(sample),
                    u8::from(candidate != zero_code),
                )
            })
            .unwrap_or(sp)
    }

    #[test]
    #[ignore = "requires externally extracted official ITU-T G.726 Appendix II vectors"]
    fn synchronous_adjustment_matches_official_reset_overload_and_i_input_vectors() {
        for (rate, rate_dir) in [
            (G726Rate::Kbps16, "16"),
            (G726Rate::Kbps24, "24"),
            (G726Rate::Kbps32, "32"),
            (G726Rate::Kbps40, "40"),
        ] {
            let cases = [
                ("mu", "mu", "RN", "FM", "FM", G711Encoding::MuLaw),
                ("mu", "mu", "RV", "FM", "FM", G711Encoding::MuLaw),
                ("a", "a", "RN", "FA", "FA", G711Encoding::ALaw),
                ("a", "a", "RV", "FA", "FA", G711Encoding::ALaw),
                ("mu", "mu", "RN", "FM", "FC", G711Encoding::ALaw),
                ("mu", "mu", "RV", "FM", "FC", G711Encoding::ALaw),
                ("a", "a", "RN", "FA", "FX", G711Encoding::MuLaw),
                ("a", "a", "RV", "FA", "FX", G711Encoding::MuLaw),
                ("INPUT", "mu", "RI", "I", "FM", G711Encoding::MuLaw),
                ("INPUT", "a", "RI", "I", "FA", G711Encoding::ALaw),
            ];
            for (input_dir, output_dir, stem, input_law, output_law, law) in cases {
                let input_name = if input_dir == "INPUT" {
                    format!("INPUT/I{rate_dir}")
                } else {
                    format!("{input_dir}/{rate_dir}/{stem}{rate_dir}{input_law}.I")
                };
                let output_name = format!("{output_dir}/{rate_dir}/{stem}{rate_dir}{output_law}.O");
                check_vector(rate, law, None, &input_name, &output_name);
            }
        }
    }

    #[test]
    #[ignore = "requires externally extracted official ITU-T G.726 Appendix II vectors"]
    fn synchronous_adjustment_matches_official_homing_vectors() {
        for (rate, rate_dir) in [
            (G726Rate::Kbps16, "16"),
            (G726Rate::Kbps24, "24"),
            (G726Rate::Kbps32, "32"),
            (G726Rate::Kbps40, "40"),
        ] {
            let cases = [
                ("mu", "mu", "M", "HN", "FM", "FM", G711Encoding::MuLaw),
                ("mu", "mu", "M", "HV", "FM", "FM", G711Encoding::MuLaw),
                ("a", "a", "A", "HN", "FA", "FA", G711Encoding::ALaw),
                ("a", "a", "A", "HV", "FA", "FA", G711Encoding::ALaw),
                ("mu", "mu", "A", "HN", "FM", "FC", G711Encoding::ALaw),
                ("mu", "mu", "A", "HV", "FM", "FC", G711Encoding::ALaw),
                ("a", "a", "M", "HN", "FA", "FX", G711Encoding::MuLaw),
                ("a", "a", "M", "HV", "FA", "FX", G711Encoding::MuLaw),
                ("INPUT", "mu", "M", "HI", "I", "FM", G711Encoding::MuLaw),
                ("INPUT", "a", "A", "HI", "I", "FA", G711Encoding::ALaw),
            ];
            for (input_dir, output_dir, init_law, stem, input_law, output_law, law) in cases {
                let input_name = if input_dir == "INPUT" {
                    format!("INPUT/I{rate_dir}")
                } else {
                    format!("{input_dir}/{rate_dir}/{stem}{rate_dir}{input_law}.I")
                };
                let output_name = format!("{output_dir}/{rate_dir}/{stem}{rate_dir}{output_law}.O");
                let initialization_dir = if init_law == "M" { "mu" } else { "a" };
                let initialization =
                    format!("{initialization_dir}/{rate_dir}/I_INI_{rate_dir}.{init_law}");
                check_vector(rate, law, Some(&initialization), &input_name, &output_name);
            }
        }
    }
}
