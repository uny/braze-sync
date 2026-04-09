//! Top-level error type for braze-sync.
//!
//! Library code returns `Result<T, Error>` defined here. The CLI layer
//! wraps this with `anyhow` and maps variants to the frozen exit codes
//! from IMPLEMENTATION.md §7.1.

use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Braze API error: {0}")]
    Api(#[from] crate::braze::error::BrazeApiError),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Missing environment variable: {0}")]
    MissingEnv(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parse error in {path}: {source}")]
    YamlParse {
        path: PathBuf,
        #[source]
        source: serde_yml::Error,
    },

    #[error("CSV parse error in {path}: {source}")]
    CsvParse {
        path: PathBuf,
        #[source]
        source: csv::Error,
    },

    #[error("Invalid file format in {path}: {message}")]
    InvalidFormat { path: PathBuf, message: String },

    #[error("Drift detected in {count} resource(s)")]
    DriftDetected { count: usize },

    #[error("Destructive change blocked: pass --allow-destructive to proceed")]
    DestructiveBlocked,

    #[error("Rate limit exhausted after {retries} retries")]
    RateLimitExhausted { retries: u32 },

    #[error("Catalog item field '{field}' is not in schema for catalog '{catalog}'")]
    CatalogItemSchemaMismatch { catalog: String, field: String },

    #[error(
        "Custom Attribute '{name}' cannot be created via API; created implicitly via /users/track"
    )]
    CustomAttributeCreateNotSupported { name: String },
}
