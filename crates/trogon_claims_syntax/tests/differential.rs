#![allow(clippy::unwrap_used, clippy::expect_used)]

use regex::Regex;
use trogon_claims_syntax::{
    validate_object_id, validate_object_type, validate_principal_kind, OBJECT_ID_MAX_BYTES, OBJECT_TYPE_MAX_BYTES,
    PRINCIPAL_KIND_MAX_BYTES,
};

const OBJECT_TYPE_PATTERN: &str = r"^([a-z][a-z0-9_]{1,61}[a-z0-9]/)*[a-z][a-z0-9_]{1,62}[a-z0-9]$";
const OBJECT_ID_PATTERN: &str = r"^(([a-zA-Z0-9/_|\-=+]{1,})|\*)$";
const RELATION_PATTERN: &str = r"^[a-z][a-z0-9_]{1,62}[a-z0-9]$";

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn string(&mut self, alphabet: &[char], max_len: usize) -> String {
        let len = self.below(max_len + 1);
        (0..len).map(|_| alphabet[self.below(alphabet.len())]).collect()
    }
}

fn type_oracle(value: &str) -> bool {
    Regex::new(OBJECT_TYPE_PATTERN).unwrap().is_match(value) && value.len() <= OBJECT_TYPE_MAX_BYTES
}

fn id_oracle(value: &str) -> bool {
    Regex::new(OBJECT_ID_PATTERN).unwrap().is_match(value) && value.len() <= OBJECT_ID_MAX_BYTES
}

fn kind_oracle(value: &str) -> bool {
    Regex::new(RELATION_PATTERN).unwrap().is_match(value) && value.len() <= PRINCIPAL_KIND_MAX_BYTES
}

fn object_id_oracle_wildcard_free(value: &str) -> bool {
    id_oracle(value) && value != "*"
}

const BOUNDARY: &[&str] = &[
    "",
    "*",
    "**",
    "a*b",
    "myapp//doc",
    "MyApp/document",
    "abc",
    "ab",
    "abc/",
    "/abc",
    "abc/abc",
    "abc_",
    "_abc",
    "ab_c",
    "a1c",
    "1bc",
    "123e4567-e89b-12d3-a456-426614174000",
    "a b",
    "a:b",
    "a.b",
    "a@b",
    "a~b",
    "a#b",
    "é",
    "abcé",
];

#[test]
fn boundary_cases_match_the_proto_patterns() {
    for case in BOUNDARY {
        assert_eq!(
            validate_object_id(case).is_ok(),
            object_id_oracle_wildcard_free(case),
            "object_id {case:?}"
        );
        assert_eq!(
            validate_object_type(case).is_ok(),
            type_oracle(case),
            "object_type {case:?}"
        );
        assert_eq!(
            validate_principal_kind(case).is_ok(),
            kind_oracle(case),
            "principal_kind {case:?}"
        );
    }
}

#[test]
fn length_limits_match_at_and_past_the_edge() {
    let segment = |n: usize| "a".repeat(n);
    for n in [2, 3, 62, 63, 64, 65] {
        let value = segment(n);
        assert_eq!(
            validate_principal_kind(&value).is_ok(),
            kind_oracle(&value),
            "kind len {n}"
        );
        assert_eq!(
            validate_object_type(&value).is_ok(),
            type_oracle(&value),
            "type len {n}"
        );
        let nested = format!("{}/{}", segment(n), segment(3));
        assert_eq!(
            validate_object_type(&nested).is_ok(),
            type_oracle(&nested),
            "inner len {n}"
        );
    }
    for n in [1023, 1024, 1025] {
        let value = "a".repeat(n);
        assert_eq!(validate_object_id(&value).is_ok(), id_oracle(&value), "id len {n}");
    }
    for n in [127, 128, 129] {
        let value = format!("{}/{}", "a".repeat(63), "b".repeat(n - 64));
        assert_eq!(
            validate_object_type(&value).is_ok(),
            type_oracle(&value),
            "type total {n}"
        );
    }
}

#[test]
fn random_strings_match_the_proto_patterns() {
    let alphabet: Vec<char> = "abcxyz019_/AZ-|=+*:.@~ #é".chars().collect();
    let type_alphabet: Vec<char> = "abcxyz019_/".chars().collect();
    let mut rng = Xorshift(0x9E37_79B9_7F4A_7C15);
    let id = Regex::new(OBJECT_ID_PATTERN).unwrap();
    let ty = Regex::new(OBJECT_TYPE_PATTERN).unwrap();
    let kind = Regex::new(RELATION_PATTERN).unwrap();
    for _ in 0..200_000 {
        let value = rng.string(&alphabet, 12);
        assert_eq!(
            validate_object_id(&value).is_ok(),
            id.is_match(&value) && value != "*",
            "object_id {value:?}"
        );
        assert_eq!(
            validate_object_type(&value).is_ok(),
            ty.is_match(&value),
            "object_type {value:?}"
        );
        assert_eq!(
            validate_principal_kind(&value).is_ok(),
            kind.is_match(&value),
            "kind {value:?}"
        );
        let value = rng.string(&type_alphabet, 12);
        assert_eq!(
            validate_object_type(&value).is_ok(),
            ty.is_match(&value),
            "object_type {value:?}"
        );
        assert_eq!(
            validate_principal_kind(&value).is_ok(),
            kind.is_match(&value),
            "kind {value:?}"
        );
    }
}
