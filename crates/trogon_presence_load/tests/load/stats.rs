//! Latency samples, outcome tallies and the plain text table the harness prints.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};

#[derive(Debug, Clone, Default)]
pub struct Samples(Vec<Duration>);

impl Samples {
    pub fn push(&mut self, sample: Duration) {
        self.0.push(sample);
    }

    pub fn merge(&mut self, other: Samples) {
        self.0.extend(other.0);
    }

    pub fn summary(&self) -> Summary {
        let mut sorted = self.0.clone();
        sorted.sort();
        Summary {
            count: sorted.len(),
            p50: percentile(&sorted, Quantile::P50),
            p95: percentile(&sorted, Quantile::P95),
            p99: percentile(&sorted, Quantile::P99),
            max: sorted.last().copied(),
        }
    }
}

impl FromIterator<Duration> for Samples {
    fn from_iter<I: IntoIterator<Item = Duration>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[derive(Debug, Clone, Copy)]
enum Quantile {
    P50,
    P95,
    P99,
}

impl Quantile {
    fn per_mille(self) -> usize {
        match self {
            Self::P50 => 500,
            Self::P95 => 950,
            Self::P99 => 990,
        }
    }
}

/// Nearest-rank percentile over an ascending slice.
fn percentile(sorted: &[Duration], quantile: Quantile) -> Option<Duration> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (sorted.len() * quantile.per_mille()).div_ceil(1000).max(1);
    sorted.get(rank - 1).copied()
}

#[derive(Debug, Clone, Copy)]
pub struct Summary {
    pub count: usize,
    pub p50: Option<Duration>,
    pub p95: Option<Duration>,
    pub p99: Option<Duration>,
    pub max: Option<Duration>,
}

pub fn millis(value: Option<Duration>) -> Value {
    value.map_or(Value::Null, |value| json!(round(value.as_secs_f64() * 1000.0)))
}

pub fn cell(value: Option<Duration>) -> String {
    value.map_or_else(
        || "-".to_owned(),
        |value| format!("{:.2}", value.as_secs_f64() * 1000.0),
    )
}

pub fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

impl Summary {
    pub fn to_json(self) -> Value {
        json!({
            "count": self.count,
            "p50_ms": millis(self.p50),
            "p95_ms": millis(self.p95),
            "p99_ms": millis(self.p99),
            "max_ms": millis(self.max),
        })
    }

    pub fn cells(self) -> Vec<String> {
        vec![
            self.count.to_string(),
            cell(self.p50),
            cell(self.p95),
            cell(self.p99),
            cell(self.max),
        ]
    }
}

#[derive(Debug, Clone, Default)]
pub struct Tally(BTreeMap<String, u64>);

impl Tally {
    pub fn add(&mut self, outcome: impl Into<String>, count: u64) {
        *self.0.entry(outcome.into()).or_default() += count;
    }

    pub fn bump(&mut self, outcome: impl Into<String>) {
        self.add(outcome, 1);
    }

    pub fn merge(&mut self, other: Tally) {
        for (outcome, count) in other.0 {
            self.add(outcome, count);
        }
    }

    pub fn get(&self, outcome: &str) -> u64 {
        self.0.get(outcome).copied().unwrap_or_default()
    }

    pub fn total(&self) -> u64 {
        self.0.values().sum()
    }

    pub fn share(&self, outcome: &str) -> f64 {
        match self.total() {
            0 => 0.0,
            total => self.get(outcome) as f64 / total as f64,
        }
    }

    pub fn to_json(&self) -> Value {
        json!(self.0)
    }

    pub fn render(&self) -> String {
        if self.0.is_empty() {
            return "-".to_owned();
        }
        self.0
            .iter()
            .map(|(outcome, count)| format!("{outcome}={count}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub struct Table {
    title: String,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(title: impl Into<String>, header: &[&str]) -> Self {
        Self {
            title: title.into(),
            header: header.iter().map(|column| (*column).to_owned()).collect(),
            rows: Vec::new(),
        }
    }

    pub fn row(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    pub fn render(&self) -> String {
        let mut widths: Vec<usize> = self.header.iter().map(String::len).collect();
        for row in &self.rows {
            for (index, value) in row.iter().enumerate() {
                if let Some(width) = widths.get_mut(index) {
                    *width = (*width).max(value.len());
                }
            }
        }
        let line = |row: &[String]| {
            let cells: Vec<String> = row
                .iter()
                .zip(&widths)
                .map(|(value, width)| format!("{value:<width$}"))
                .collect();
            format!("| {} |", cells.join(" | "))
        };
        let rule: Vec<String> = widths.iter().map(|width| "-".repeat(*width)).collect();
        let mut out = vec![
            format!("\n{}", self.title),
            line(&self.header),
            format!("| {} |", rule.join(" | ")),
        ];
        out.extend(self.rows.iter().map(|row| line(row)));
        out.join("\n")
    }
}
