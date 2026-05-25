//! braze-sync: GitOps CLI for managing Braze configuration as code.

pub mod braze;
pub mod cli;
pub mod config;
pub mod diff;
pub mod error;
pub mod format;
pub mod fs;
pub mod resource;
pub mod values;

pub use error::{Error, Result};
