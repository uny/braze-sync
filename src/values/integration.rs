//! Wiring layer between [`crate::values`] and the diff / apply pipeline.
//!
//! v0.15 model: all `__BRAZESYNC.*__` placeholders are Braze-managed
//! (`lid` / `cb_id`) and resolved at apply/diff time from the freshly-
//! fetched remote body via URL / `${NAME}` anchor correlation. There
//! is no values yaml, no pre-flight gate, no values_input_hash —
//! resolution failures surface at compute-plan time with full context.

use std::collections::BTreeMap;

use crate::resource::{ContentBlock, EmailTemplate};
use crate::values::braze_managed::prepare_field;
use crate::values::placeholder::{
    find_suspicious_placeholders, resolve_placeholders, LookupKey, ResolutionError,
};
use crate::values::templatize::FieldKind;

/// One resource's worth of placeholder failures, ready to be folded into
/// a top-level error message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionFailure {
    pub resource_kind: &'static str,
    pub resource_name: String,
    /// `Some(field)` for email_template fields, `None` for content_block.
    pub field: Option<&'static str>,
    pub errors: Vec<ResolutionError>,
}

/// Resolve every `__BRAZESYNC.*__` in `cb.content`.
///
/// `remote` provides the live lid / cb_id values; pass `None` for new
/// resources (lid falls back to the placeholder key, cb_id filter is
/// stripped — see [`prepare_field`]).
pub fn resolve_content_block_with_remote(
    cb: &mut ContentBlock,
    remote: Option<&ContentBlock>,
) -> std::result::Result<(), ResolutionFailure> {
    warn_suspicious("content_block", &cb.name, None, &cb.content);
    if !body_has_placeholders(&cb.content) {
        return Ok(());
    }
    let prep = prepare_field(
        &cb.content,
        remote.map(|r| r.content.as_str()),
        FieldKind::ContentBlock,
    );
    let lookup: BTreeMap<LookupKey, String> = prep.additions;
    match resolve_placeholders(&prep.body, &lookup) {
        Ok(resolved) => {
            cb.content = resolved;
            Ok(())
        }
        Err(errors) => Err(ResolutionFailure {
            resource_kind: "content_block",
            resource_name: cb.name.clone(),
            field: None,
            errors,
        }),
    }
}

/// Resolve placeholders across every Liquid-bearing field of `et`.
pub fn resolve_email_template_with_remote(
    et: &mut EmailTemplate,
    remote: Option<&EmailTemplate>,
) -> std::result::Result<(), Vec<ResolutionFailure>> {
    let mut failures: Vec<ResolutionFailure> = Vec::new();

    macro_rules! resolve_field {
        ($field_name:expr, $field_kind:expr, $accessor:expr, $remote_accessor:expr) => {{
            let body: &str = $accessor;
            warn_suspicious("email_template", &et.name, Some($field_name), body);
            if body_has_placeholders(body) {
                let prep = prepare_field(body, $remote_accessor, $field_kind);
                match resolve_placeholders(&prep.body, &prep.additions) {
                    Ok(resolved) => Some(resolved),
                    Err(errors) => {
                        failures.push(ResolutionFailure {
                            resource_kind: "email_template",
                            resource_name: et.name.clone(),
                            field: Some($field_name),
                            errors,
                        });
                        None
                    }
                }
            } else {
                None
            }
        }};
    }

    let new_subject = resolve_field!(
        "subject",
        FieldKind::EmailSubject,
        et.subject.as_str(),
        remote.map(|r| r.subject.as_str())
    );
    let new_body_html = resolve_field!(
        "body_html",
        FieldKind::EmailHtmlBody,
        et.body_html.as_str(),
        remote.map(|r| r.body_html.as_str())
    );
    let new_body_plaintext = resolve_field!(
        "body_plaintext",
        FieldKind::EmailPlainBody,
        et.body_plaintext.as_str(),
        remote.map(|r| r.body_plaintext.as_str())
    );
    let new_preheader = match et.preheader.as_deref() {
        Some(s) => resolve_field!(
            "preheader",
            FieldKind::EmailPreheader,
            s,
            remote.and_then(|r| r.preheader.as_deref())
        ),
        None => None,
    };

    if !failures.is_empty() {
        return Err(failures);
    }

    if let Some(v) = new_subject {
        et.subject = v;
    }
    if let Some(v) = new_body_html {
        et.body_html = v;
    }
    if let Some(v) = new_body_plaintext {
        et.body_plaintext = v;
    }
    if let Some(v) = new_preheader {
        et.preheader = Some(v);
    }
    Ok(())
}

fn body_has_placeholders(body: &str) -> bool {
    body.contains("__BRAZESYNC.")
}

fn warn_suspicious(kind: &str, name: &str, field: Option<&str>, body: &str) {
    let suspects = find_suspicious_placeholders(body);
    for s in &suspects {
        let scope = match field {
            Some(f) => format!("{kind} '{name}' ({f})"),
            None => format!("{kind} '{name}'"),
        };
        eprintln!("warning: {scope}: suspicious placeholder-like token {s}");
    }
}

/// Format aggregated failures into a single human-readable error.
pub fn format_failures(failures: &[ResolutionFailure]) -> crate::error::Error {
    let mut msg = String::new();
    msg.push_str(&format!(
        "Cannot continue: {} placeholder resolution failure(s)\n",
        failures.iter().map(|f| f.errors.len()).sum::<usize>(),
    ));
    for f in failures {
        let scope = match f.field {
            Some(field) => format!("  {} '{}' ({}):", f.resource_kind, f.resource_name, field),
            None => format!("  {} '{}':", f.resource_kind, f.resource_name),
        };
        msg.push_str(&scope);
        msg.push('\n');
        for e in &f.errors {
            match e {
                ResolutionError::UnknownKey { ty, key, start } => {
                    msg.push_str(&format!(
                        "    - offset {}: __BRAZESYNC.{}.{}__ (no anchor match in remote body)\n",
                        start,
                        ty.as_str(),
                        key,
                    ));
                }
                ResolutionError::DuplicateLidKey { key, occurrences } => {
                    let offsets = occurrences
                        .iter()
                        .map(|o| o.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    msg.push_str(&format!(
                        "    - __BRAZESYNC.lid.{key}__ referenced {} times (offsets {offsets}); \
                         lid IDs are per-click-context — use a distinct key per occurrence\n",
                        occurrences.len(),
                    ));
                }
            }
        }
    }
    crate::error::Error::Config(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::content_block::ContentBlockState;

    fn cb(name: &str, content: &str) -> ContentBlock {
        ContentBlock {
            name: name.into(),
            description: None,
            content: content.into(),
            tags: Vec::new(),
            state: ContentBlockState::Active,
        }
    }

    fn et(name: &str) -> EmailTemplate {
        EmailTemplate {
            name: name.into(),
            subject: String::new(),
            body_html: String::new(),
            body_plaintext: String::new(),
            description: None,
            preheader: None,
            should_inline_css: None,
            tags: Vec::new(),
        }
    }

    #[test]
    fn no_placeholders_skips_resolution() {
        let mut block = cb("plain", "<p>hi there</p>");
        resolve_content_block_with_remote(&mut block, None).unwrap();
        assert_eq!(block.content, "<p>hi there</p>");
    }

    #[test]
    fn content_block_resolves_lid_from_remote() {
        let mut block = cb(
            "promo",
            r#"<a href="https://x.com/cta">__BRAZESYNC.lid.cta__</a>"#,
        );
        let remote = cb(
            "promo",
            r#"<a href="https://x.com/cta">{{x | lid: 'newlidvalue1'}}</a>"#,
        );
        resolve_content_block_with_remote(&mut block, Some(&remote)).unwrap();
        assert!(block.content.contains(">newlidvalue1<"));
    }

    #[test]
    fn new_resource_lid_fallback_uses_placeholder_key() {
        let mut block = cb(
            "promo",
            r#"<a href="https://x.com/spring">__BRAZESYNC.lid.spring_sale__</a>"#,
        );
        resolve_content_block_with_remote(&mut block, None).unwrap();
        assert!(block.content.contains(">spring_sale<"));
    }

    #[test]
    fn new_resource_cb_id_filter_is_stripped() {
        let mut block = cb(
            "page",
            "{{content_blocks.${promo} | id: '__BRAZESYNC.cb_id.promo__'}}",
        );
        resolve_content_block_with_remote(&mut block, None).unwrap();
        assert_eq!(block.content, "{{content_blocks.${promo}}}");
    }

    #[test]
    fn email_template_resolves_per_field() {
        let mut t = et("welcome");
        t.body_html = r#"<a href="https://x.com/cta">__BRAZESYNC.lid.cta__</a>"#.into();
        let mut remote = et("welcome");
        remote.body_html =
            r#"<a href="https://x.com/cta">{{x | lid: 'newhtmllidx'}}</a>"#.into();
        resolve_email_template_with_remote(&mut t, Some(&remote)).unwrap();
        assert!(t.body_html.contains(">newhtmllidx<"));
    }

    #[test]
    fn missing_remote_anchor_surfaces_as_failure() {
        let mut block = cb(
            "promo",
            r#"<a href="https://x.com/cta">__BRAZESYNC.lid.cta__</a>"#,
        );
        let remote = cb("promo", "<p>no anchor here</p>");
        let err = resolve_content_block_with_remote(&mut block, Some(&remote)).unwrap_err();
        assert_eq!(err.errors.len(), 1);
    }
}
