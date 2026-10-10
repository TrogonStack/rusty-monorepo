//! Harness stdout read line by line, each line stamped with when it arrived.
//!
//! The bytes are kept exactly as the harness wrote them, since they become
//! `transcript.jsonl`; the stamps ride beside them so the spans a harness never
//! exported can be rebuilt with the times its events actually happened.

use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::time::SystemTime;

/// When one line of stdout finished arriving, and where it ends in the captured bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineArrival {
    end: usize,
    at: SystemTime,
}

/// The arrival time of every line in a captured stdout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StdoutTimeline(Vec<LineArrival>);

/// One line of stdout, without its terminator, and when it arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StampedLine<'a> {
    pub text: &'a [u8],
    pub at: SystemTime,
}

impl StdoutTimeline {
    /// Pairs each stamp with its line in `stdout`, the bytes this timeline was read with.
    pub fn lines<'a>(&'a self, stdout: &'a [u8]) -> impl Iterator<Item = StampedLine<'a>> + 'a {
        let mut start = 0;
        self.0.iter().filter_map(move |arrival| {
            let line = stdout.get(start..arrival.end)?;
            start = arrival.end;
            let text = line.strip_suffix(b"\n").unwrap_or(line);
            let text = text.strip_suffix(b"\r").unwrap_or(text);
            Some(StampedLine { text, at: arrival.at })
        })
    }

    /// Restamps lines the reader only got to after the harness ended, so output drained
    /// late is kept but cannot outlast the process that wrote it.
    pub(super) fn clamped_to(mut self, end: SystemTime) -> Self {
        for arrival in &mut self.0 {
            arrival.at = arrival.at.min(end);
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn stamped(stdout: &[u8], times: &[SystemTime]) -> Self {
        let mut arrivals = Vec::new();
        let mut end = 0;
        for (line, at) in stdout.split_inclusive(|byte| *byte == b'\n').zip(times) {
            end += line.len();
            arrivals.push(LineArrival { end, at: *at });
        }
        Self(arrivals)
    }
}

/// Reads `pipe` to its end, stamping each line as its terminator (or the end of the
/// stream) arrives.
pub(super) fn read_stamped<R: Read>(pipe: Option<R>) -> (Vec<u8>, StdoutTimeline) {
    let mut bytes = Vec::new();
    let mut arrivals = Vec::new();
    let Some(pipe) = pipe else {
        return (bytes, StdoutTimeline(arrivals));
    };
    let mut reader = BufReader::new(pipe);
    loop {
        match reader.read_until(b'\n', &mut bytes) {
            Ok(0) => break,
            Ok(_) => arrivals.push(LineArrival {
                end: bytes.len(),
                at: SystemTime::now(),
            }),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    if arrivals.last().map_or(0, |arrival| arrival.end) < bytes.len() {
        arrivals.push(LineArrival {
            end: bytes.len(),
            at: SystemTime::now(),
        });
    }
    (bytes, StdoutTimeline(arrivals))
}

#[cfg(test)]
mod tests {
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
}
