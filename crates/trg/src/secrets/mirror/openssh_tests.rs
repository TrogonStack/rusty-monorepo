use super::*;
use aws_lc_rs::rand;

fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&u32::try_from(bytes.len()).expect("length").to_be_bytes());
    out.extend_from_slice(bytes);
}

/// An OpenSSH container around a freshly generated Ed25519 key.
fn ed25519_container(cipher: &[u8], tamper_public: bool) -> (String, [u8; 32]) {
    let mut seed = [0u8; 32];
    rand::fill(&mut seed).expect("randomness");
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed).expect("seed");
    let mut public = pair.public_key().as_ref().to_vec();

    let mut blob = Vec::new();
    put_string(&mut blob, SSH_TYPE_ED25519);
    put_string(&mut blob, &public);

    if tamper_public {
        public[0] ^= 0x01;
    }
    let mut pair_bytes = seed.to_vec();
    pair_bytes.extend_from_slice(&public);

    let mut private = Vec::new();
    private.extend_from_slice(&7u32.to_be_bytes());
    private.extend_from_slice(&7u32.to_be_bytes());
    put_string(&mut private, SSH_TYPE_ED25519);
    put_string(&mut private, &public);
    put_string(&mut private, &pair_bytes);
    put_string(&mut private, b"fixture");
    for pad in 1..=7u8 {
        if private.len() % 8 == 0 {
            break;
        }
        private.push(pad);
    }

    let mut raw = OPENSSH_MAGIC.to_vec();
    put_string(&mut raw, cipher);
    put_string(&mut raw, b"none");
    put_string(&mut raw, b"");
    raw.extend_from_slice(&1u32.to_be_bytes());
    put_string(&mut raw, &blob);
    put_string(&mut raw, &private);

    let encoded = BASE64.encode(&raw);
    let mut text = format!("{OPENSSH_FENCE_BEGIN}\n");
    for line in encoded.as_bytes().chunks(70) {
        text.push_str(std::str::from_utf8(line).expect("ascii"));
        text.push('\n');
    }
    text.push_str(OPENSSH_FENCE_END);
    text.push('\n');
    (text, seed)
}

#[test]
fn an_ed25519_key_becomes_pkcs8_with_its_public_half() {
    let (text, seed) = ed25519_container(OPENSSH_CIPHER_NONE, false);
    let key = parse(&text).expect("parse");
    assert_eq!(key.transit_type(), "ed25519");

    let mut expected = ED25519_PKCS8_PREFIX.to_vec();
    expected.extend_from_slice(&seed);
    assert_eq!(key.pkcs8(), expected.as_slice());
    let again = Ed25519KeyPair::from_pkcs8_maybe_unchecked(key.pkcs8());
    assert!(again.is_ok(), "aws-lc-rs must read back the document it was given");
}

#[test]
fn text_that_is_not_an_openssh_key_is_refused() {
    assert_eq!(parse("hunter2").err(), Some(SourceKeyError::NotOpenssh));
    assert_eq!(
        parse("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n").err(),
        Some(SourceKeyError::NotOpenssh)
    );
}

#[test]
fn a_passphrase_protected_key_is_refused() {
    let (text, _) = ed25519_container(b"aes256-ctr", false);
    assert_eq!(parse(&text).err(), Some(SourceKeyError::Encrypted));
}

#[test]
fn a_key_whose_halves_disagree_is_refused() {
    let (text, _) = ed25519_container(OPENSSH_CIPHER_NONE, true);
    assert_eq!(parse(&text).err(), Some(SourceKeyError::Inconsistent));
}

#[test]
fn modulo_matches_integer_arithmetic() {
    for (value, modulus) in [(1_000_003u64, 997u64), (u64::MAX, 65_537), (5, 7), (7, 7), (0, 3)] {
        let got = modulo(&value.to_be_bytes(), &modulus.to_be_bytes());
        let want = (value % modulus).to_be_bytes();
        assert_eq!(strip(&got), strip(&want), "{value} mod {modulus}");
    }
}

#[test]
fn minus_one_borrows_across_bytes() {
    assert_eq!(minus_one(&[0x01, 0x00]), vec![0x00, 0xff]);
    assert_eq!(minus_one(&[0x00, 0x05]), vec![0x04]);
}

#[test]
fn an_error_never_carries_key_bytes() {
    let (text, seed) = ed25519_container(OPENSSH_CIPHER_NONE, true);
    let message = parse(&text).expect_err("refused").to_string();
    assert!(!message.contains(&BASE64.encode(seed)));
}
