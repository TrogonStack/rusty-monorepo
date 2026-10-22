use std::fmt;

use serde::Serialize;
use trogon_presence::{BucketReport, UnixMillis};

use super::{
    ApplyReport, ConfigReport, CountReport, ExpireReport, InspectReport, LeaseBucketCheck, LeaseState, ShardsReport,
};
use crate::drain::DrainReply;

const COLUMN_GAP: &str = "  ";
const EMPTY_CELL: &str = "-";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Table,
    Json,
}

impl OutputFormat {
    pub fn json_if(json: bool) -> Self {
        if json {
            Self::Json
        } else {
            Self::Table
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    headers: Vec<&'static str>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(headers: Vec<&'static str>) -> Self {
        Self {
            headers,
            rows: Vec::new(),
        }
    }

    pub fn row(mut self, cells: Vec<String>) -> Self {
        self.rows.push(cells);
        self
    }

    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }
}

impl fmt::Display for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut widths: Vec<usize> = self.headers.iter().map(|header| header.chars().count()).collect();
        for row in &self.rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(cell.chars().count());
            }
        }
        let line = |cells: Vec<&str>| {
            let padded: Vec<String> = cells
                .iter()
                .zip(&widths)
                .map(|(cell, width)| format!("{cell:<width$}", width = *width))
                .collect();
            padded.join(COLUMN_GAP).trim_end().to_owned()
        };
        writeln!(f, "{}", line(self.headers.clone()))?;
        for row in &self.rows {
            writeln!(f, "{}", line(row.iter().map(String::as_str).collect()))?;
        }
        Ok(())
    }
}

pub trait Report: Serialize {
    fn table(&self) -> Table;
}

pub fn render<R: Report>(report: &R, format: OutputFormat) -> Result<String, serde_json::Error> {
    match format {
        OutputFormat::Table => Ok(report.table().to_string()),
        OutputFormat::Json => serde_json::to_string_pretty(report).map(|json| format!("{json}\n")),
    }
}

fn optional(value: Option<impl fmt::Display>) -> String {
    value.map_or_else(|| EMPTY_CELL.to_owned(), |value| value.to_string())
}

fn bucket_rows(table: Table, report: &BucketReport) -> Table {
    report.checks.iter().fold(table, |table, check| {
        table.row(vec![
            report.stream.clone(),
            check.field.to_string(),
            check.status.to_string(),
            check.severity.to_string(),
        ])
    })
}

impl Report for ApplyReport {
    fn table(&self) -> Table {
        let action = |action: super::ApplyAction| match action {
            super::ApplyAction::Created => "created".to_owned(),
            super::ApplyAction::Verified => "verified".to_owned(),
        };
        Table::new(vec!["bucket", "action"])
            .row(vec![self.data.bucket.to_string(), action(self.data.action)])
            .row(vec![self.lease.bucket.to_string(), action(self.lease.action)])
    }
}

impl Report for ConfigReport {
    fn table(&self) -> Table {
        let table = bucket_rows(Table::new(vec!["stream", "field", "status", "severity"]), &self.data);
        match &self.lease {
            LeaseBucketCheck::Checked(report) => bucket_rows(table, report),
            LeaseBucketCheck::Missing { bucket } => table.row(vec![
                bucket.stream_name(),
                "existence".to_owned(),
                "missing".to_owned(),
                "hard".to_owned(),
            ]),
        }
    }
}

fn expiry(expires_at: Option<UnixMillis>, now: UnixMillis) -> String {
    match expires_at {
        None => "never".to_owned(),
        Some(at) if at <= now => "expired".to_owned(),
        Some(at) => format!("in {}ms", at.get() - now.get()),
    }
}

impl Report for InspectReport {
    fn table(&self) -> Table {
        let headers = vec![
            "topic",
            "key",
            "holder",
            "phx_ref",
            "phx_ref_prev",
            "meta",
            "revision",
            "expires",
        ];
        self.entries.iter().fold(Table::new(headers), |table, entry| {
            table.row(vec![
                entry.topic.to_string(),
                entry.key.to_string(),
                entry.holder.to_string(),
                entry.phx_ref.to_string(),
                optional(entry.phx_ref_prev.as_ref()),
                serde_json::to_string(&entry.meta).unwrap_or_default(),
                entry.revision.to_string(),
                expiry(entry.expires_at, self.now),
            ])
        })
    }
}

impl Report for CountReport {
    fn table(&self) -> Table {
        Table::new(vec!["topic", "keys", "metas"]).row(vec![
            self.topic.to_string(),
            self.keys.to_string(),
            self.metas.to_string(),
        ])
    }
}

fn lease_cells(state: LeaseState) -> [String; 3] {
    match state {
        LeaseState::Free => ["free".to_owned(), EMPTY_CELL.to_owned(), EMPTY_CELL.to_owned()],
        LeaseState::Held {
            owner,
            revision,
            current_generation,
        } => [
            owner.to_string(),
            revision.to_string(),
            if current_generation { "held" } else { "stale" }.to_owned(),
        ],
    }
}

impl Report for ShardsReport {
    fn table(&self) -> Table {
        let headers = vec![
            "shard",
            "view_owner",
            "view_rev",
            "view_state",
            "writer_owner",
            "writer_rev",
            "writer_state",
        ];
        self.shards.iter().fold(Table::new(headers), |table, shard| {
            let [view_owner, view_rev, view_state] = lease_cells(shard.view);
            let [writer_owner, writer_rev, writer_state] = lease_cells(shard.writer);
            table.row(vec![
                shard.shard.as_str().to_owned(),
                view_owner,
                view_rev,
                view_state,
                writer_owner,
                writer_rev,
                writer_state,
            ])
        })
    }
}

impl Report for ExpireReport {
    fn table(&self) -> Table {
        let headers = vec!["holder", "key", "topic", "lifetime", "status", "mutation_seq"];
        let mut table = Table::new(headers);
        for key in &self.keys {
            for entry in &key.entries {
                let status = match entry.status {
                    super::ExpiryStatus::Released => "released",
                    super::ExpiryStatus::Gone => "gone",
                };
                table = table.row(vec![
                    self.holder.to_string(),
                    key.key.to_string(),
                    entry.topic.to_string(),
                    entry.lifetime.to_string(),
                    status.to_owned(),
                    optional(entry.mutation_seq),
                ]);
            }
        }
        table
    }
}

impl Report for DrainReply {
    fn table(&self) -> Table {
        Table::new(vec![
            "instance",
            "released_views",
            "released_writers",
            "already_draining",
        ])
        .row(vec![
            self.instance.to_string(),
            self.released_views.to_string(),
            self.released_writers.to_string(),
            self.already_draining.to_string(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_pad_columns_to_the_widest_cell() {
        let table = Table::new(vec!["a", "bb"])
            .row(vec!["xyz".to_owned(), "1".to_owned()])
            .row(vec!["q".to_owned(), "22".to_owned()]);
        assert_eq!(table.to_string(), "a    bb\nxyz  1\nq    22\n");
    }

    #[test]
    fn json_flag_selects_the_machine_format() {
        assert_eq!(OutputFormat::json_if(true), OutputFormat::Json);
        assert_eq!(OutputFormat::json_if(false), OutputFormat::Table);
    }
}
