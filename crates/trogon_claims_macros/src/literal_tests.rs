use super::string_value;

#[test]
fn plain_literal_yields_its_contents() {
    assert_eq!(string_value("\"readme\""), Ok("readme".to_owned()));
}

#[test]
fn raw_literals_yield_their_contents() {
    assert_eq!(string_value("r\"a b\""), Ok("a b".to_owned()));
    assert_eq!(string_value("r#\"a\"b\"#"), Ok("a\"b".to_owned()));
}

#[test]
fn escapes_are_refused() {
    assert!(string_value("\"a\\x41\"").is_err());
}

#[test]
fn non_string_literals_are_refused() {
    assert!(string_value("42").is_err());
    assert!(string_value("b\"abc\"").is_err());
    assert!(string_value("'a'").is_err());
    assert!(string_value("c\"abc\"").is_err());
}
