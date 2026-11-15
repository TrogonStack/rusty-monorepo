#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::DateTime;
use trogon_claims::{NumericDate, NumericDateError, Timestamp, TimestampError, UtcOffset};

const MIN: i64 = -62_167_219_200;
const MAX: i64 = 253_402_300_799;

fn numeric(text: &str) -> NumericDate {
    NumericDate::try_from_lexical(text).unwrap()
}

#[test]
fn integer_and_fractional_numeric_dates_keep_exact_text() {
    assert_eq!(
        (numeric("1700000000").secs(), numeric("1700000000").nanos()),
        (1_700_000_000, 0)
    );
    let fractional = numeric("1700000000.123456789");
    assert_eq!((fractional.secs(), fractional.nanos()), (1_700_000_000, 123_456_789));
    let exact = numeric("1700000000.1");
    assert_eq!(exact.nanos(), 100_000_000);
    let truncated = numeric("1.0000000009");
    assert_eq!(truncated.nanos(), 0);
}

#[test]
fn exponent_forms_shift_the_decimal_point() {
    assert_eq!(numeric("1.7e9").secs(), 1_700_000_000);
    assert_eq!(numeric("17E8").secs(), 1_700_000_000);
    let small = numeric("1700000000123e-3");
    assert_eq!((small.secs(), small.nanos()), (1_700_000_000, 123_000_000));
    assert_eq!(numeric("0e5").secs(), 0);
    assert_eq!(numeric("-0").secs(), 0);
}

#[test]
fn negative_fractions_floor_to_a_nonnegative_nano_part() {
    let before = numeric("-1.5");
    assert_eq!((before.secs(), before.nanos()), (-2, 500_000_000));
    let whole = numeric("-3");
    assert_eq!((whole.secs(), whole.nanos()), (-3, 0));
}

#[test]
fn malformed_numeric_dates_are_rejected() {
    assert_eq!(NumericDate::try_from_lexical(""), Err(NumericDateError::Empty));
    for text in [
        "abc", "+1", "01", ".5", "1.", "1e", "1e+", "--1", "1 ", "0x10", "NaN", "1.5.2",
    ] {
        assert_eq!(
            NumericDate::try_from_lexical(text),
            Err(NumericDateError::NotANumber),
            "{text}"
        );
    }
}

#[test]
fn out_of_range_numeric_dates_are_rejected() {
    for text in [
        "253402300800",
        "-62167219201",
        "1e13",
        "1e999999999999999999999",
        "9999999999999999999999",
    ] {
        assert_eq!(
            NumericDate::try_from_lexical(text),
            Err(NumericDateError::OutOfRange),
            "{text}"
        );
    }
    assert_eq!(numeric("253402300799").secs(), MAX);
    assert_eq!(numeric("-62167219200").secs(), MIN);
    assert_eq!(numeric("1e-999999999999999999999").secs(), 0);
}

#[test]
fn constructors_check_nanos_and_range() {
    assert_eq!(
        NumericDate::try_new(0, 1_000_000_000),
        Err(TimestampError::NanosOutOfRange { nanos: 1_000_000_000 })
    );
    assert_eq!(
        NumericDate::try_new(MAX + 1, 0),
        Err(TimestampError::SecondsOutOfRange { secs: MAX + 1 })
    );
    assert!(Timestamp::try_new(MAX, 0, UtcOffset::try_from_minutes(60).unwrap()).is_err());
    assert!(Timestamp::try_new(MIN, 0, UtcOffset::try_from_minutes(-60).unwrap()).is_err());
    assert!(UtcOffset::try_from_minutes(1440).is_err());
    assert!(UtcOffset::try_from_minutes(-1440).is_err());
}

#[test]
fn numeric_date_converts_to_a_utc_timestamp() {
    let timestamp = numeric("1700000000.5").to_timestamp();
    assert_eq!(timestamp.offset(), UtcOffset::UTC);
    assert_eq!(timestamp.to_string(), "2023-11-14T22:13:20.5Z");
}

#[test]
fn timestamps_compare_by_instant_not_by_offset() {
    let utc = Timestamp::try_new(1_700_000_000, 0, UtcOffset::UTC).unwrap();
    let plus_two = Timestamp::try_new(1_700_000_000, 0, UtcOffset::try_from_minutes(120).unwrap()).unwrap();
    assert_eq!(utc, plus_two);
    assert_ne!(utc.to_string(), plus_two.to_string());
    assert_eq!(plus_two.to_string(), "2023-11-15T00:13:20+02:00");
    let later = Timestamp::try_new(1_700_000_001, 0, UtcOffset::UTC).unwrap();
    assert!(utc < later);
}

#[test]
fn formatting_matches_chrono_across_the_whole_range() {
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut cases = vec![MIN, MAX, 0, -1, 951_782_400, 951_868_799, 4_107_542_400];
    for _ in 0..100_000 {
        cases.push(MIN + (next() % (MAX - MIN + 1) as u64) as i64);
    }
    for secs in cases {
        let nanos = (next() % 1_000_000_000) as u32;
        let minutes = (next() % 2879) as i32 - 1439;
        let offset = UtcOffset::try_from_minutes(minutes).unwrap();
        let Ok(timestamp) = Timestamp::try_new(secs, nanos, offset) else {
            continue;
        };
        let local = DateTime::from_timestamp(secs + offset.seconds(), nanos)
            .unwrap()
            .naive_utc();
        let mut expected = local.format("%Y-%m-%dT%H:%M:%S").to_string();
        if nanos != 0 {
            let fraction = format!("{nanos:09}");
            expected.push('.');
            expected.push_str(fraction.trim_end_matches('0'));
        }
        expected.push_str(&offset.to_string());
        assert_eq!(timestamp.to_string(), expected, "secs={secs} minutes={minutes}");
    }
}

#[test]
fn offsets_render_as_rfc3339() {
    assert_eq!(UtcOffset::UTC.to_string(), "Z");
    assert_eq!(UtcOffset::try_from_minutes(-330).unwrap().to_string(), "-05:30");
    assert_eq!(UtcOffset::try_from_minutes(1439).unwrap().to_string(), "+23:59");
}
