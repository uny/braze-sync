//! Per-env values: template + values separation for Braze resources.
//!
//! Implements RFC `docs/local/feat-per-env-values.md` Phase 1: schema +
//! placeholder resolver. Phase 2 (apply integration), Phase 3 (export),
//! Phase 4 (diff), Phase 6 (plan-lock) wire this module into the
//! existing CLI surface.
//!
//! ## Module shape
//!
//! - [`schema`]: `values/<env>.yaml` deserialization and built-in shape
//!   validation (lid format, cb_id format, key naming).
//! - [`placeholder`]: extract and resolve `__BRAZESYNC.<type>.<key>__`
//!   tokens in a body string. Resolution takes a flat `(type, key) -> value`
//!   lookup so it stays resource-shape-agnostic.

pub mod correlation;
pub mod exporter;
pub mod integration;
pub mod placeholder;
pub mod schema;

pub use correlation::{
    extract_cb_id_values, extract_html_lid_values, extract_plaintext_lid_values, normalize_url,
    slug_for_cb_id, slug_for_lid, CbIdCorrelation, LidCorrelation,
};
pub use exporter::{
    refresh_content_block_values, refresh_email_template_values, ExportUpdates,
};

pub use integration::{
    format_failures, load_values_for_env, preflight_values, resolve_content_block_in_place,
    resolve_email_template_in_place, values_file_path, PreflightArgs, ResolutionFailure,
};

pub use placeholder::{
    extract_placeholders, find_suspicious_placeholders, resolve_placeholders, LookupKey,
    Placeholder, PlaceholderType, ResolutionError,
};
pub use schema::{
    default_values_path, CbIdEntry, ContentBlockValues, CustomEntry, EmailTemplateValues,
    FieldValues, Globals, LidEntry, ValuesFile, SUPPORTED_VERSION,
};
