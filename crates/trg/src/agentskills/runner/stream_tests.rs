use super::*;
use std::io::Cursor;
use std::time::Duration;

#[test]
fn keeps_every_byte_in_order_including_an_unterminated_tail() {
    let raw = b"{\"a\":1}\n\n{\"b\":2}\r\n\xff\xfe partial";
    let (bytes, timeline) = read_stamped(Some(Cursor::new(raw.to_vec())));
    assert_eq!(bytes, raw);

    let lines: Vec<&[u8]> = timeline.lines(&bytes).map(|line| line.text).collect();
    assert_eq!(
        lines,
        vec![
            b"{\"a\":1}".as_slice(),
            b"".as_slice(),
            b"{\"b\":2}".as_slice(),
            b"\xff\xfe partial".as_slice()
        ]
    );
}

#[test]
fn stamps_never_go_backwards() {
    let raw: Vec<u8> = (0..200).flat_map(|n| format!("line {n}\n").into_bytes()).collect();
    let (bytes, timeline) = read_stamped(Some(Cursor::new(raw.clone())));
    assert_eq!(bytes, raw);
    let stamps: Vec<SystemTime> = timeline.lines(&bytes).map(|line| line.at).collect();
    assert_eq!(stamps.len(), 200);
    assert!(stamps.windows(2).all(|pair| pair[0] <= pair[1]));
}

#[test]
fn a_watcher_sees_every_line_as_it_is_stamped() {
    let raw = b"one\ntwo\r\nthree";
    let mut seen: Vec<Vec<u8>> = Vec::new();
    let (bytes, timeline) = read_stamped_watching(Some(Cursor::new(raw.to_vec())), |line, _| {
        seen.push(line.to_vec());
    });

    assert_eq!(bytes, raw);
    assert_eq!(timeline.lines(&bytes).count(), 3);
    assert_eq!(seen, [b"one".to_vec(), b"two\r".to_vec(), b"three".to_vec()]);
}

#[test]
fn a_missing_pipe_reads_as_empty() {
    let (bytes, timeline) = read_stamped::<Cursor<Vec<u8>>>(None);
    assert!(bytes.is_empty());
    assert_eq!(timeline.lines(&bytes).count(), 0);
}

#[test]
fn clamping_restamps_lines_that_arrived_after_the_end() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let stdout = b"one\ntwo\nthree\n";
    let timeline = StdoutTimeline::stamped(
        stdout,
        &[base, base + Duration::from_secs(1), base + Duration::from_secs(2)],
    );

    let end = base + Duration::from_secs(1);
    let clamped = timeline.clamped_to(end);
    let lines: Vec<(&[u8], SystemTime)> = clamped.lines(stdout).map(|line| (line.text, line.at)).collect();
    assert_eq!(
        lines,
        vec![
            (b"one".as_slice(), base),
            (b"two".as_slice(), end),
            (b"three".as_slice(), end)
        ]
    );
}

#[test]
fn a_stamped_fixture_pairs_lines_with_the_times_given() {
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let stdout = b"one\ntwo\n";
    let timeline = StdoutTimeline::stamped(stdout, &[base, base + Duration::from_secs(1)]);
    let lines: Vec<StampedLine> = timeline.lines(stdout).collect();
    assert_eq!(lines[1].text, b"two");
    assert_eq!(lines[1].at, base + Duration::from_secs(1));
}
