//! Per-env values + Braze-managed runtime resolution.
//!
//! v0.15 model: template + values separation where `lid` / `cb_id` are
//! resolved at apply/diff time from the remote body (see
//! [`braze_managed`]), and only user-managed `custom` / `global`
//! entries live in `values/<env>.yaml`.
//!
//! ## Module shape
//!
//! - [`schema`]: `values/<env>.yaml` deserialization. v0.15 carries
//!   only `custom` / `global` namespaces; legacy `lid:` / `cb_id:`
//!   sections parse-silent-drop until the operator runs
//!   `braze-sync values cleanup`.
//! - [`placeholder`]: extract and resolve `__BRAZESYNC.<type>.<key>__`
//!   tokens in a body string. The resolver itself stays
//!   resource-shape-agnostic.
//! - [`braze_managed`]: produce the lid / cb_id half of the resolution
//!   lookup at apply/diff time from a freshly-fetched remote body
//!   (or, for brand-new resources, from a controlled fallback).
//! - [`templatize`]: detect raw lid / cb_id literals and rewrite them
//!   to `__BRAZESYNC.*__` placeholders. Local-only migration helper.
//! - [`integration`]: composes the above into the diff/apply pipeline.

pub mod braze_managed;
pub mod correlation;
pub mod integration;
pub mod placeholder;
pub mod schema;
pub mod templatize;

pub use braze_managed::{prepare_field, PreparedTemplate};
pub use correlation::{
    extract_cb_id_values, extract_html_lid_values, extract_plaintext_lid_values, normalize_url,
    slug_for_cb_id, slug_for_lid, CbIdCorrelation, LidCorrelation,
};

pub use integration::{
    compute_values_input_hashes, format_failures, load_values_for_env, preflight_values,
    resolve_content_block_with_remote, resolve_email_template_with_remote, values_file_path,
    PreflightArgs, ResolutionFailure,
};

pub use placeholder::{
    extract_placeholders, find_suspicious_placeholders, resolve_placeholders, LookupKey,
    Placeholder, PlaceholderType, ResolutionError,
};
pub use schema::{
    default_values_path, ContentBlockValues, CustomEntry, EmailTemplateValues, Globals, ValuesFile,
    SUPPORTED_VERSION,
};
