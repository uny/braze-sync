//! Output formatters for diff results.
//!
//! Two formatters are exposed:
//!
//! - [`TableFormatter`] — human-readable, multi-resource indented layout
//!   matching IMPLEMENTATION.md §7.4. v0.1.0 ships without ANSI colors;
//!   the global `--no-color` flag is therefore a no-op until a future
//!   cosmetic pass adds color.
//! - [`JsonFormatter`] — frozen v1 schema for CI consumption (§12). The
//!   wire shape carries an explicit `version: 1` field so consumers can
//!   branch on a future schema bump.
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

/// Format a [`DiffSummary`] for display. Implementations are stateless
/// unit structs.
pub trait DiffFormatter {
    fn format(&self, summary: &DiffSummary) -> String;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TableFormatter;

#[derive(Debug, Default, Clone, Copy)]
pub struct JsonFormatter;

impl DiffFormatter for TableFormatter {
    fn format(&self, summary: &DiffSummary) -> String {
        table::render(summary)
    }
}

impl DiffFormatter for JsonFormatter {
    fn format(&self, summary: &DiffSummary) -> String {
        json::render(summary)
    }
}

impl OutputFormat {
    /// Pick the formatter implementation for this format.
    pub fn formatter(self) -> Box<dyn DiffFormatter> {
        match self {
            Self::Table => Box::new(TableFormatter),
            Self::Json => Box::new(JsonFormatter),
        }
    }
}

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod snapshot_tests;
