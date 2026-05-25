//! Braze-managed placeholder resolution (`__BRAZESYNC.lid.…__`,
//! `__BRAZESYNC.cb_id.…__`).
//!
//! v0.15 model: all placeholders are owned by Braze. Values are
//! resolved at apply/diff time from the freshly-fetched remote body
//! via URL / `${NAME}` anchor correlation. There is no values yaml —
//! the local Git body carries the template; Braze carries the values.
//!
//! ## Module shape
//!
//! - [`placeholder`]: extract / resolve `__BRAZESYNC.<type>.<key>__`
//!   tokens. Resource-shape-agnostic; takes a flat
//!   `(type, key) -> value` lookup.
//! - [`braze_managed`]: build the lookup at apply/diff time by
//!   correlating the templatized body against the remote body. Also
//!   handles the new-resource fallback (lid key→value, cb_id filter
//!   strip).
//! - [`templatize`]: detect raw `| lid: 'X'` / `{{content_blocks.${NAME} | id: 'cbN'}}`
//!   literals and rewrite to placeholders. Local-only migration helper.
//! - [`correlation`]: low-level extractors used by both `braze_managed`
//!   (resolve path) and `templatize` (rewrite path).
//! - [`integration`]: thin facade that the diff/apply pipeline calls.

pub mod braze_managed;
pub mod correlation;
pub mod integration;
pub mod placeholder;
pub mod templatize;

pub use braze_managed::{prepare_field, PreparedTemplate};
pub use correlation::{
    extract_cb_id_values, extract_html_lid_values, extract_plaintext_lid_values, normalize_url,
    slug_for_cb_id, slug_for_lid, CbIdCorrelation, LidCorrelation,
};
pub use integration::{
    format_failures, resolve_content_block_with_remote, resolve_email_template_with_remote,
    ResolutionFailure,
};
pub use placeholder::{
    extract_placeholders, find_suspicious_placeholders, resolve_placeholders, LookupKey,
    Placeholder, PlaceholderType, ResolutionError,
};
