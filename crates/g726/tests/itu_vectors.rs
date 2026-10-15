use g711::encode_mulaw;
use g726::{G726Decoder, G726Rate};

#[test]
fn matches_initial_mu_law_samples_of_official_16_kbit_vector() {
    let mut decoder = G726Decoder::new(G726Rate::Kbps16, 1).unwrap();
    let actual: Vec<u8> = [0, 1, 2, 1, 2, 1, 2, 1]
        .into_iter()
        .map(|code| encode_mulaw(decoder.decode_code(0, code).unwrap()))
        .collect();
    assert_eq!(actual, [0xfd, 0xf7, 0x76, 0xf5, 0x73, 0xf1, 0x6f, 0xed]);
}
