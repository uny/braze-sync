//! Wiring layer between [`crate::values`] and the diff / apply pipeline.

use crate::resource::{ContentBlock, EmailTemplate};
use crate::values::braze_managed::{prepare_field, LidFallback};
use crate::values::placeholder::ResolutionError;
use crate::values::templatize::FieldKind;

/// Drift-fallback summary for one resource (and field, for
/// email_template). Aggregated across the run and surfaced as a
/// "Notice" block after the diff / apply output so the operator can
/// see which links the local template introduced that the remote
/// body didn't carry.
#[derive(Debug, Clone)]
pub struct FallbackReport {
    pub resource_kind: &'static str,
    pub resource_name: String,
    pub field: Option<&'static str>,
    pub fallbacks: Vec<LidFallback>,
    /// See [`crate::values::braze_managed::PreparedTemplate::fallback_gated`].
    /// `diff` exits non-zero and `apply` requires `--allow-fallback` when
    /// any report in a run has this set.
    pub gated: bool,
}

/// One resource's worth of placeholder failures, ready to be folded
/// into a top-level error message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionFailure {
    pub resource_kind: &'static str,
    pub resource_name: String,
    /// `Some(field)` for email_template fields, `None` for content_block.
    pub field: Option<&'static str>,
    pub errors: Vec<ResolutionError>,
}

/// Resolve every `__BRAZESYNC__` in `cb.content`.
///
/// `remote` provides the live lid / cb_id values; pass `None` for new
/// resources (lid → URL slug, cb_id filter stripped — see
/// [`prepare_field`]).
pub fn resolve_content_block_with_remote(
    cb: &mut ContentBlock,
    remote: Option<&ContentBlock>,
) -> std::result::Result<Vec<FallbackReport>, ResolutionFailure> {
    if !needs_resolve(&cb.content) {
        return Ok(Vec::new());
    }
    let prep = prepare_field(
        &cb.content,
        remote.map(|r| r.content.as_str()),
        FieldKind::ContentBlock,
    );
    emit_prep_warnings("content_block", &cb.name, None, &prep.warnings);
    if !prep.errors.is_empty() {
        return Err(ResolutionFailure {
            resource_kind: "content_block",
            resource_name: cb.name.clone(),
            field: None,
            errors: prep.errors,
        });
    }
    cb.content = prep.body;
    let reports = if prep.fallbacks.is_empty() {
        Vec::new()
    } else {
        vec![FallbackReport {
            resource_kind: "content_block",
            resource_name: cb.name.clone(),
            field: None,
            fallbacks: prep.fallbacks,
            gated: prep.fallback_gated,
        }]
    };
    Ok(reports)
}

/// Resolve placeholders across every Liquid-bearing field of `et`.
pub fn resolve_email_template_with_remote(
    et: &mut EmailTemplate,
    remote: Option<&EmailTemplate>,
) -> std::result::Result<Vec<FallbackReport>, Vec<ResolutionFailure>> {
    let mut failures: Vec<ResolutionFailure> = Vec::new();
    let mut reports: Vec<FallbackReport> = Vec::new();

    macro_rules! resolve_field {
        ($field_name:expr, $field_kind:expr, $accessor:expr, $remote_accessor:expr) => {{
            let body: &str = $accessor;
            if needs_resolve(body) {
                let prep = prepare_field(body, $remote_accessor, $field_kind);
                emit_prep_warnings(
                    "email_template",
                    &et.name,
                    Some($field_name),
                    &prep.warnings,
                );
                if !prep.errors.is_empty() {
                    failures.push(ResolutionFailure {
                        resource_kind: "email_template",
                        resource_name: et.name.clone(),
                        field: Some($field_name),
                        errors: prep.errors,
                    });
                    None
                } else {
                    if !prep.fallbacks.is_empty() {
                        reports.push(FallbackReport {
                            resource_kind: "email_template",
                            resource_name: et.name.clone(),
                            field: Some($field_name),
                            fallbacks: prep.fallbacks,
                            gated: prep.fallback_gated,
                        });
                    }
                    Some(prep.body)
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
    Ok(reports)
}

/// Human-readable summary block for collected drift fallbacks.
/// Empty input → empty string (so callers can append unconditionally).
pub fn format_fallback_reports(reports: &[FallbackReport]) -> String {
    if reports.is_empty() {
        return String::new();
    }
    let total: usize = reports.iter().map(|r| r.fallbacks.len()).sum();
    let gated_total: usize = reports
        .iter()
        .filter(|r| r.gated)
        .map(|r| r.fallbacks.len())
        .sum();
    let mut out = String::new();
    out.push_str(&format!(
        "\nNotice: {total} link(s) resolved with fallback lid values \
         (Braze will assign the final lid on first dashboard save):\n"
    ));
    for r in reports {
        let scope = match r.field {
            Some(f) => format!("  {} '{}' ({})", r.resource_kind, r.resource_name, f),
            None => format!("  {} '{}'", r.resource_kind, r.resource_name),
        };
        out.push_str(&scope);
        if r.gated {
            out.push_str(
                " ⚠ GATED — unmatched placeholder(s) and unconsumed remote lid \
                 value(s) both present; may indicate a link merge/reorder, not a \
                 plain new link",
            );
        }
        out.push('\n');
        for fb in &r.fallbacks {
            match &fb.anchor {
                Some(url) => out.push_str(&format!("    - {url} → '{}'\n", fb.value)),
                None => out.push_str(&format!(
                    "    - (no URL anchor — positional) → '{}'\n",
                    fb.value
                )),
            }
        }
    }
    if gated_total > 0 {
        out.push_str(&format!(
            "\n{gated_total} of the above are gated (see ⚠ above): `diff` exits \
             non-zero; `apply` requires `--allow-fallback` in addition to \
             `--confirm`.\n"
        ));
    }
    out
}

/// Cheap pre-filter: a body needs resolution if it carries the strict
/// token *or* a retired-namespace token we need to surface as an error.
fn needs_resolve(body: &str) -> bool {
    body.contains("__BRAZESYNC") || body.contains("__BRAZSYNC")
}

fn emit_prep_warnings(
    kind: &'static str,
    name: &str,
    field: Option<&'static str>,
    warnings: &[String],
) {
    if warnings.is_empty() {
        return;
    }
    let scope = match field {
        Some(f) => format!("{kind} '{name}' ({f})"),
        None => format!("{kind} '{name}'"),
    };
    for w in warnings {
        eprintln!("warning: {scope}: {w}");
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
                ResolutionError::UnresolvedLid { start, anchor } => {
                    let where_ = anchor
                        .as_deref()
                        .map(|u| format!("URL '{u}'"))
                        .unwrap_or_else(|| "no URL anchor".to_string());
                    msg.push_str(&format!(
                        "    - offset {start}: lid `__BRAZESYNC__` ({where_}) — no anchor match in remote body\n",
                    ));
                }
                ResolutionError::UnresolvedCbId { start, name } => {
                    let n = name.as_deref().unwrap_or("<unknown>");
                    msg.push_str(&format!(
                        "    - offset {start}: cb_id `__BRAZESYNC__` (`${{{n}}}`) — no `${{{n}}}` include in remote body\n",
                    ));
                }
                ResolutionError::UnknownContext { start } => {
                    msg.push_str(&format!(
                        "    - offset {start}: `__BRAZESYNC__` outside `| lid:` / `| id:` argument — cannot infer type\n",
                    ));
                }
                ResolutionError::RetiredNamespace { token } => {
                    msg.push_str(&format!(
                        "    - {token}: retired placeholder syntax \
                         (v0.15 `__BRAZESYNC.<type>.<key>__` was removed in v0.16; \
                         re-run `braze-sync templatize` to regenerate)\n",
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
        let reports = resolve_content_block_with_remote(&mut block, None).unwrap();
        assert_eq!(block.content, "<p>hi there</p>");
        assert!(reports.is_empty());
    }

    #[test]
    fn content_block_resolves_lid_from_remote() {
        let mut block = cb(
            "promo",
            r#"<a href="https://x.com/cta">{{x | lid: '__BRAZESYNC__'}}</a>"#,
        );
        let remote = cb(
            "promo",
            r#"<a href="https://x.com/cta">{{x | lid: 'newlidvalue1'}}</a>"#,
        );
        let reports = resolve_content_block_with_remote(&mut block, Some(&remote)).unwrap();
        assert!(block.content.contains("'newlidvalue1'"));
        assert!(reports.is_empty());
    }

    #[test]
    fn new_resource_lid_uses_url_slug() {
        let mut block = cb(
            "promo",
            r#"<a href="https://x.com/spring-sale">{{x | lid: '__BRAZESYNC__'}}</a>"#,
        );
        let reports = resolve_content_block_with_remote(&mut block, None).unwrap();
        assert!(
            block.content.contains("'spring_sale'"),
            "got: {}",
            block.content
        );
        assert!(reports.is_empty());
    }

    #[test]
    fn new_resource_cb_id_filter_is_stripped() {
        let mut block = cb("page", "{{content_blocks.${promo} | id: '__BRAZESYNC__'}}");
        let reports = resolve_content_block_with_remote(&mut block, None).unwrap();
        assert_eq!(block.content, "{{content_blocks.${promo}}}");
        assert!(reports.is_empty());
    }

    #[test]
    fn email_template_resolves_per_field() {
        let mut t = et("welcome");
        t.body_html = r#"<a href="https://x.com/cta">{{x | lid: '__BRAZESYNC__'}}</a>"#.into();
        let mut remote = et("welcome");
        remote.body_html = r#"<a href="https://x.com/cta">{{x | lid: 'newhtmllidx'}}</a>"#.into();
        let reports = resolve_email_template_with_remote(&mut t, Some(&remote)).unwrap();
        assert!(t.body_html.contains("'newhtmllidx'"));
        assert!(reports.is_empty());
    }

    #[test]
    fn missing_remote_anchor_falls_back_to_slug() {
        let mut block = cb(
            "promo",
            r#"<a href="https://x.com/cta">{{x | lid: '__BRAZESYNC__'}}</a>"#,
        );
        let remote = cb("promo", "<p>no anchor here</p>");
        let reports = resolve_content_block_with_remote(&mut block, Some(&remote)).unwrap();
        assert!(block.content.contains("'cta'"), "got: {}", block.content);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].fallbacks.len(), 1);
        assert_eq!(
            reports[0].fallbacks[0].anchor.as_deref(),
            Some("https://x.com/cta")
        );
        assert_eq!(reports[0].fallbacks[0].value, "cta");
    }

    #[test]
    fn missing_remote_anchor_report_is_not_gated() {
        let mut block = cb(
            "promo",
            r#"<a href="https://x.com/cta">{{x | lid: '__BRAZESYNC__'}}</a>"#,
        );
        let remote = cb("promo", "<p>no anchor here</p>");
        let reports = resolve_content_block_with_remote(&mut block, Some(&remote)).unwrap();
        assert_eq!(reports.len(), 1);
        assert!(!reports[0].gated);
    }

    #[test]
    fn placeholder_miss_and_remote_leftover_report_is_gated() {
        let mut block = cb(
            "promo",
            r#"<a href="https://x.com/new-page">{{x | lid: '__BRAZESYNC__'}}</a>"#,
        );
        let remote = cb(
            "promo",
            r#"<a href="https://x.com/old-page">{{x | lid: 'liveeeeeeee1'}}</a>"#,
        );
        let reports = resolve_content_block_with_remote(&mut block, Some(&remote)).unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].gated);
    }

    #[test]
    fn format_fallback_reports_marks_gated_entries() {
        let reports = vec![FallbackReport {
            resource_kind: "content_block",
            resource_name: "promo".into(),
            field: None,
            fallbacks: vec![LidFallback {
                anchor: Some("https://x.com/new-page".into()),
                value: "new_page".into(),
            }],
            gated: true,
        }];
        let out = format_fallback_reports(&reports);
        assert!(out.contains("GATED"), "got: {out}");
        assert!(
            out.contains("--allow-fallback"),
            "expected the apply hint, got: {out}"
        );
    }

    #[test]
    fn format_fallback_reports_omits_gate_hint_when_nothing_gated() {
        let reports = vec![FallbackReport {
            resource_kind: "content_block",
            resource_name: "promo".into(),
            field: None,
            fallbacks: vec![LidFallback {
                anchor: Some("https://x.com/cta".into()),
                value: "cta".into(),
            }],
            gated: false,
        }];
        let out = format_fallback_reports(&reports);
        assert!(!out.contains("GATED"), "got: {out}");
        assert!(!out.contains("--allow-fallback"), "got: {out}");
    }

    #[test]
    fn subject_lid_resolves_positionally_from_remote() {
        let mut t = et("promo");
        t.subject = "Spring sale {{x | lid: '__BRAZESYNC__'}}".into();
        let mut remote = et("promo");
        remote.subject = "Spring sale {{x | lid: 'subjectlid1'}}".into();
        let reports = resolve_email_template_with_remote(&mut t, Some(&remote)).unwrap();
        assert!(t.subject.contains("'subjectlid1'"));
        assert!(reports.is_empty());
    }

    #[test]
    fn retired_v015_envelope_is_fatal() {
        let mut block = cb("legacy", "hello __BRAZESYNC.lid.foo__ world");
        let err = resolve_content_block_with_remote(&mut block, None).unwrap_err();
        assert!(err
            .errors
            .iter()
            .any(|e| matches!(e, ResolutionError::RetiredNamespace { .. })));
    }

    #[test]
    fn typo_suffixed_token_is_detected() {
        let mut block = cb("typo", "hello __BRAZESYNCTEST__ world");
        let err = resolve_content_block_with_remote(&mut block, None).unwrap_err();
        assert!(
            !err.errors.is_empty(),
            "typo-suffixed token must not pass silently"
        );
    }
}
