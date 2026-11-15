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
        let selected_items = stdout.split_inclusive(|byte| *byte == b'\n').zip(times);
        for (line, at) in selected_items {
            end += line.len();
            arrivals.push(LineArrival { end, at: *at });
        }
        Self(arrivals)
    }
}

/// Reads `pipe` to its end, stamping each line as its terminator (or the end of the
/// stream) arrives.
pub(super) fn read_stamped<R: Read>(pipe: Option<R>) -> (Vec<u8>, StdoutTimeline) {
    read_stamped_watching(pipe, |_, _| {})
}

/// Reads `pipe` like [`read_stamped`], handing `on_line` each line, without its
/// terminator, as soon as it arrives.
pub(super) fn read_stamped_watching<R: Read>(
    pipe: Option<R>,
    mut on_line: impl FnMut(&[u8], SystemTime),
) -> (Vec<u8>, StdoutTimeline) {
    let mut bytes = Vec::new();
    let mut arrivals = Vec::new();
    let Some(pipe) = pipe else {
        return (bytes, StdoutTimeline(arrivals));
    };
    let mut reader = BufReader::new(pipe);
    let mut arrived = |bytes: &[u8], arrivals: &mut Vec<LineArrival>| {
        let start = arrivals.last().map_or(0, |arrival: &LineArrival| arrival.end);
        let at = SystemTime::now();
        arrivals.push(LineArrival { end: bytes.len(), at });
        let line = &bytes[start..];
        on_line(line.strip_suffix(b"\n").unwrap_or(line), at);
    };
    loop {
        match reader.read_until(b'\n', &mut bytes) {
            Ok(0) => break,
            Ok(_) => arrived(&bytes, &mut arrivals),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    if arrivals.last().map_or(0, |arrival| arrival.end) < bytes.len() {
        arrived(&bytes, &mut arrivals);
    }
    (bytes, StdoutTimeline(arrivals))
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
