//! braze-sync: GitOps CLI for managing Braze configuration as code.
//!
//! See `docs/local/IMPLEMENTATION.md` for the full design contract. The
//! public surface fills in incrementally over Phase A → Phase B per §13.

pub mod config;
pub mod diff;
pub mod error;
pub mod resource;

pub use error::{Error, Result};
