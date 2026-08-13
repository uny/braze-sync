//! Output formatters for diff results.
//!
//! Two formatters are exposed:
//!
//! - [`TableFormatter`] — human-readable, multi-resource indented layout.
//!   v0.1.0 ships without ANSI colors; the global `--no-color` flag is
//!   therefore a no-op until a future cosmetic pass adds color.
//! - [`JsonFormatter`] — frozen v1 schema for CI consumption. The wire
//!   shape carries an explicit `version: 1` field so consumers can branch
//!   on a future schema bump.
//!
//! The wire types in [`json`] are deliberately separate from the domain
//! types in [`crate::resource`] / [`crate::diff`]. Refactoring a domain
//! type cannot accidentally change the public JSON contract.

pub mod json;
pub mod table;

use crate::diff::DiffSummary;
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
#[value(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    Table,
    Json,
}

/// Format a [`DiffSummary`] for display. Implementations are cheap
/// `Copy` values; [`TableFormatter`] carries the table-only display
/// knobs, [`JsonFormatter`] is a unit struct with none.
pub trait DiffFormatter {
    fn format(&self, summary: &DiffSummary) -> String;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TableFormatter {
    /// Suppress in-sync resources from the output. Only blocks whose
    /// entire body is `no drift` are dropped, so an unchanged resource
    /// that still carries an informational line stays visible.
    pub only_drift: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct JsonFormatter;

impl DiffFormatter for TableFormatter {
    fn format(&self, summary: &DiffSummary) -> String {
        table::render(summary, self.only_drift)
    }
}

impl DiffFormatter for JsonFormatter {
    fn format(&self, summary: &DiffSummary) -> String {
        json::render(summary)
    }
}

impl OutputFormat {
    /// Pick the formatter implementation for this format. `table`
    /// carries the table-only knobs; the JSON schema is frozen and
    /// ignores them, so `--format json` always emits every resource.
    pub fn formatter(self, table: TableFormatter) -> Box<dyn DiffFormatter> {
        match self {
            Self::Table => Box::new(table),
            Self::Json => Box::new(JsonFormatter),
        }
    }
}

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod snapshot_tests;
