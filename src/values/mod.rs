//! Anonymous Braze-managed placeholder resolution (`__BRAZESYNC__`).
//!
//! Resolved at apply/diff time from the freshly-fetched remote body
//! via URL / `${NAME}` / positional anchor correlation.

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
    extract_placeholders, find_suspicious_placeholders, has_placeholders, Placeholder,
    PlaceholderType, ResolutionError, TOKEN,
};
