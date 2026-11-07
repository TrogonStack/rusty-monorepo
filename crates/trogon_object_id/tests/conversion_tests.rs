use trogon_object_id::{IntegerText, ParseError, Text, MAX_VALUE_BYTES};

#[test]
fn text_conversions_validate_and_preserve_owned_storage() {
    let owned = String::from("hello_🦀");
    let pointer = owned.as_ptr();
    let text = Text::try_from(owned).expect("valid owned text");
    assert_eq!(text.as_ref(), "hello_🦀", "text keeps its spelling");
    let recovered = String::from(text);
    assert_eq!(
        recovered.as_ptr(),
        pointer,
        "infallible conversion returns the allocation"
    );
    assert_eq!(
        Text::try_from("hello_🦀"),
        "hello_🦀".parse(),
        "borrowed and FromStr paths agree"
    );
    for (raw, expected) in [
        (String::new(), ParseError::EmptyValue),
        ("x".repeat(MAX_VALUE_BYTES + 1), ParseError::ValueTooLong),
        (String::from("line\nfeed"), ParseError::ControlCharacter),
    ] {
        assert_eq!(Text::try_from(raw.as_str()), Err(expected), "borrowed text is checked");
        assert_eq!(Text::try_from(raw), Err(expected), "owned text is checked");
    }
}

#[test]
fn integer_conversions_preserve_syntax_and_allocation() {
    let owned = String::from("+00042");
    let pointer = owned.as_ptr();
    let integer = IntegerText::try_from(owned).expect("signed decimal text");
    assert_eq!(integer.as_ref(), "+00042", "numeric spelling is retained");
    let text = Text::from(integer);
    assert_eq!(text.as_str().as_ptr(), pointer, "infallible widening keeps storage");
    let integer = IntegerText::try_from(text).expect("checked text can be narrowed");
    assert_eq!(integer.as_str().as_ptr(), pointer, "fallible narrowing keeps storage");
    let recovered = String::from(integer);
    assert_eq!(recovered.as_ptr(), pointer, "owned extraction keeps storage");
    for raw in ["0", "-42", "+00042"] {
        assert_eq!(
            IntegerText::try_from(raw),
            raw.parse(),
            "TryFrom and FromStr accept identical decimal syntax"
        );
    }
    for (raw, expected) in [
        (String::new(), ParseError::EmptyValue),
        ("9".repeat(MAX_VALUE_BYTES + 1), ParseError::ValueTooLong),
        (String::from("1\n"), ParseError::ControlCharacter),
        (String::from("+"), ParseError::InvalidValue),
        (String::from("１２"), ParseError::InvalidValue),
        (String::from("1.0"), ParseError::InvalidValue),
    ] {
        assert_eq!(
            IntegerText::try_from(raw.as_str()),
            Err(expected),
            "borrowed integer is checked"
        );
        assert_eq!(IntegerText::try_from(raw), Err(expected), "owned integer is checked");
    }
    assert_eq!(
        IntegerText::try_from(Text::try_from("bad").expect("valid general text")),
        Err(ParseError::InvalidValue),
        "valid text does not automatically establish the integer rule"
    );
}

#[cfg(feature = "uuid")]
#[test]
fn uuid_text_conversions_keep_case_and_owned_storage() {
    use trogon_object_id::UuidText;
    let raw = "550E8400-E29B-41D4-A716-446655440000";
    let owned = raw.to_owned();
    let pointer = owned.as_ptr();
    let uuid = UuidText::try_from(owned).expect("hyphenated UUID text");
    assert_eq!(uuid.as_ref(), raw, "text UUID preserves capitalization");
    let text = Text::from(uuid);
    assert_eq!(text.as_str().as_ptr(), pointer, "text widening keeps allocation");
    let uuid = UuidText::try_from(text).expect("valid text narrows to UUID text");
    let recovered = String::from(uuid);
    assert_eq!(recovered.as_ptr(), pointer, "UUID text extraction keeps allocation");
    assert_eq!(UuidText::try_from(raw), raw.parse(), "TryFrom and FromStr agree");
    for (raw, expected) in [
        (String::new(), ParseError::EmptyValue),
        ("x".repeat(MAX_VALUE_BYTES + 1), ParseError::ValueTooLong),
        (String::from("abc\n"), ParseError::ControlCharacter),
        (
            String::from("550e8400e29b41d4a716446655440000"),
            ParseError::InvalidValue,
        ),
        (
            String::from("urn:uuid:550e8400-e29b-41d4-a716-446655440000"),
            ParseError::InvalidValue,
        ),
        (
            String::from("550e8400-e29b-41d4-a716-44665544000z"),
            ParseError::InvalidValue,
        ),
    ] {
        assert_eq!(
            UuidText::try_from(raw.as_str()),
            Err(expected),
            "borrowed UUID text is checked"
        );
        assert_eq!(UuidText::try_from(raw), Err(expected), "owned UUID text is checked");
    }
    assert_eq!(
        UuidText::try_from(Text::try_from("bad").expect("valid general text")),
        Err(ParseError::InvalidValue),
        "text narrowing still checks UUID syntax"
    );
}
