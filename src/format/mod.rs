//! Output formatting.
//!
//! Phase A6 stub: only the [`OutputFormat`] enum exists so the global
//! `--format` flag in [`crate::cli`] has a type to bind to. The actual
//! Table / JSON formatters and the `DiffFormatter` trait land in Phase A7
//! (see IMPLEMENTATION.md §13 / §6.6 / §12).

use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum OutputFormat {
    Table,
    Json,
}
