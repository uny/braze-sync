//! Wiring layer between [`crate::values`] and the diff / apply pipeline.
//!
//! v0.15 split of responsibilities:
//! - lid / cb_id placeholders are resolved at apply/diff time from the
//!   remote body (see [`crate::values::braze_managed`]). The pre-flight
//!   gate intentionally does not validate them — they cannot be checked
//!   before the API call that fetches the remote body.
//! - custom / global placeholders are resolved from the per-env values
//!   yaml. The pre-flight gate aborts on any unresolved custom/global
//!   key before a single Braze write fires.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::ResolvedConfig;
use crate::error::{Error, Result};
use crate::resource::{ContentBlock, EmailTemplate};
use crate::values::braze_managed::prepare_field;
use crate::values::placeholder::{
    find_suspicious_placeholders, resolve_placeholders, LookupKey, PlaceholderType, ResolutionError,
};
use crate::values::schema::{default_values_path, ValuesFile};
use crate::values::templatize::FieldKind;

/// Where to look for the per-env values file. `values_file` config
/// override wins; otherwise default `values/<env>.yaml`.
pub fn values_file_path(config_dir: &Path, resolved: &ResolvedConfig) -> PathBuf {
    if let Some(custom) = &resolved.values_file {
        if custom.is_absolute() {
            custom.clone()
        } else {
            config_dir.join(custom)
        }
    } else {
        default_values_path(config_dir, &resolved.environment_name)
    }
}

/// Load the per-env values file, tolerating absence.
pub fn load_values_for_env(
    config_dir: &Path,
    resolved: &ResolvedConfig,
) -> Result<Option<ValuesFile>> {
    let path = values_file_path(config_dir, resolved);
    if !path.exists() {
        return Ok(None);
    }
    ValuesFile::load(&path).map(Some)
}

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

/// Resolve every `__BRAZESYNC.*__` in `cb.content`. `remote` provides
/// live lid / cb_id values; pass `None` for new resources (fallback
/// kicks in for both types — see [`prepare_field`]).
pub fn resolve_content_block_with_remote(
    cb: &mut ContentBlock,
    remote: Option<&ContentBlock>,
    values: Option<&ValuesFile>,
) -> std::result::Result<(), ResolutionFailure> {
    if !body_has_placeholders(&cb.content) {
        return Ok(());
    }
    let mut lookup = build_user_managed_lookup_cb(&cb.name, values);
    let prep = prepare_field(
        &cb.content,
        remote.map(|r| r.content.as_str()),
        FieldKind::ContentBlock,
    );
    lookup.extend(prep.additions);
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
/// `remote` is the freshly-fetched template body — `None` means new
/// resource (cb_id filter strip + lid fallback applies).
pub fn resolve_email_template_with_remote(
    et: &mut EmailTemplate,
    remote: Option<&EmailTemplate>,
    values: Option<&ValuesFile>,
) -> std::result::Result<(), Vec<ResolutionFailure>> {
    let mut failures: Vec<ResolutionFailure> = Vec::new();
    let base_lookup = build_user_managed_lookup_et(&et.name, values);

    macro_rules! resolve_field {
        ($field_name:expr, $field_kind:expr, $accessor:expr, $remote_accessor:expr) => {{
            let body: &str = $accessor;
            if body_has_placeholders(body) {
                let mut lookup = base_lookup.clone();
                let prep = prepare_field(body, $remote_accessor, $field_kind);
                lookup.extend(prep.additions);
                match resolve_placeholders(&prep.body, &lookup) {
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

/// Build the `(type, key) -> value` lookup for one content_block's
/// **user-managed** namespaces only (custom / global). lid / cb_id are
/// merged in separately from the remote body.
fn build_user_managed_lookup_cb(
    name: &str,
    values: Option<&ValuesFile>,
) -> BTreeMap<LookupKey, String> {
    let mut out = BTreeMap::new();
    let Some(vf) = values else {
        return out;
    };
    insert_globals(&mut out, vf);
    if let Some(cb) = vf.content_block.get(name) {
        for (k, e) in &cb.custom {
            if let Some(v) = &e.value {
                out.insert((PlaceholderType::Custom, k.clone()), v.clone());
            }
        }
    }
    out
}

/// Build the user-managed lookup for one email_template. `custom`
/// lives at the resource root and is shared across fields.
fn build_user_managed_lookup_et(
    name: &str,
    values: Option<&ValuesFile>,
) -> BTreeMap<LookupKey, String> {
    let mut out = BTreeMap::new();
    let Some(vf) = values else {
        return out;
    };
    insert_globals(&mut out, vf);
    if let Some(et) = vf.email_template.get(name) {
        for (k, e) in &et.custom {
            if let Some(v) = &e.value {
                out.insert((PlaceholderType::Custom, k.clone()), v.clone());
            }
        }
    }
    out
}

fn insert_globals(out: &mut BTreeMap<LookupKey, String>, vf: &ValuesFile) {
    for (k, e) in &vf.globals.custom {
        if let Some(v) = &e.value {
            out.insert((PlaceholderType::Global, k.clone()), v.clone());
        }
    }
}

/// Surface envelope-shaped tokens that don't match the strict
/// placeholder grammar as warnings.
fn warn_suspicious(kind: &str, name: &str, field: Option<&str>, suspicious: Vec<String>) {
    if suspicious.is_empty() {
        return;
    }
    let scope = match field {
        Some(f) => format!("{kind} '{name}' ({f})"),
        None => format!("{kind} '{name}'"),
    };
    for s in suspicious {
        eprintln!(
            "WARN: {scope}: suspicious placeholder `{s}` — strict form is \
             __BRAZESYNC.<lid|cb_id|custom|global>.<key>__"
        );
    }
}

/// Bundle of inputs the pre-flight needs from the entry layer.
pub struct PreflightArgs<'a> {
    pub config_dir: &'a Path,
    pub resolved: &'a ResolvedConfig,
    pub content_blocks_root: &'a Path,
    pub email_templates_root: &'a Path,
    pub kinds: &'a [crate::resource::ResourceKind],
    pub cb_name_filter: Option<&'a str>,
    pub et_name_filter: Option<&'a str>,
    pub cb_excludes: &'a [regex_lite::Regex],
    pub et_excludes: &'a [regex_lite::Regex],
}

/// Walk every selected kind's local files and validate that all
/// **user-managed** (`custom` / `global`) placeholders resolve against
/// the loaded values file. lid / cb_id are intentionally skipped —
/// they resolve from the remote body at plan compute time and any
/// failure surfaces in `compute_*_plan` with full context.
pub fn preflight_values(args: PreflightArgs<'_>) -> Result<Option<ValuesFile>> {
    use crate::resource::ResourceKind;

    let has_cb = args.kinds.contains(&ResourceKind::ContentBlock);
    let has_et = args.kinds.contains(&ResourceKind::EmailTemplate);
    if !has_cb && !has_et {
        return Ok(None);
    }

    let values_path = values_file_path(args.config_dir, args.resolved);
    let values = load_values_for_env(args.config_dir, args.resolved)?;
    let values_loaded = values.is_some();

    let mut failures: Vec<ResolutionFailure> = Vec::new();

    if has_cb && args.content_blocks_root.exists() {
        let mut locals =
            crate::fs::content_block_io::load_all_content_blocks(args.content_blocks_root)
                .map_err(|e| Error::Config(format!("loading content_block locals: {e}")))?;
        if let Some(name) = args.cb_name_filter {
            locals.retain(|c| c.name == name);
        }
        locals.retain(|c| !crate::config::is_excluded(&c.name, args.cb_excludes));
        for cb in &locals {
            warn_suspicious(
                "content_block",
                &cb.name,
                None,
                find_suspicious_placeholders(&cb.content),
            );
            if let Some(failure) =
                preflight_user_managed_cb(cb, values.as_ref(), &mut Vec::<&str>::new())
            {
                failures.push(failure);
            }
        }
    }

    if has_et && args.email_templates_root.exists() {
        let mut locals =
            crate::fs::email_template_io::load_all_email_templates(args.email_templates_root)
                .map_err(|e| Error::Config(format!("loading email_template locals: {e}")))?;
        if let Some(name) = args.et_name_filter {
            locals.retain(|t| t.name == name);
        }
        locals.retain(|t| !crate::config::is_excluded(&t.name, args.et_excludes));
        for t in &locals {
            for (field, body) in [
                ("subject", t.subject.as_str()),
                ("body_html", t.body_html.as_str()),
                ("body_plaintext", t.body_plaintext.as_str()),
                ("preheader", t.preheader.as_deref().unwrap_or("")),
            ] {
                warn_suspicious(
                    "email_template",
                    &t.name,
                    Some(field),
                    find_suspicious_placeholders(body),
                );
            }
            failures.extend(preflight_user_managed_et(t, values.as_ref()));
        }
    }

    if !failures.is_empty() {
        return Err(format_failures(&failures, &values_path, values_loaded));
    }

    Ok(values)
}

/// Pre-flight check that every `custom` / `global` placeholder
/// referenced by `cb.content` resolves against `values`. Returns
/// `Some(failure)` carrying only the user-managed unresolved keys.
fn preflight_user_managed_cb(
    cb: &ContentBlock,
    values: Option<&ValuesFile>,
    _ignored: &mut Vec<&str>,
) -> Option<ResolutionFailure> {
    if !body_has_placeholders(&cb.content) {
        return None;
    }
    let lookup = build_user_managed_lookup_cb(&cb.name, values);
    let errors = unresolved_user_managed(&cb.content, &lookup);
    if errors.is_empty() {
        None
    } else {
        Some(ResolutionFailure {
            resource_kind: "content_block",
            resource_name: cb.name.clone(),
            field: None,
            errors,
        })
    }
}

fn preflight_user_managed_et(
    et: &EmailTemplate,
    values: Option<&ValuesFile>,
) -> Vec<ResolutionFailure> {
    let mut out = Vec::new();
    let lookup = build_user_managed_lookup_et(&et.name, values);
    for (field, body) in [
        ("subject", et.subject.as_str()),
        ("body_html", et.body_html.as_str()),
        ("body_plaintext", et.body_plaintext.as_str()),
        ("preheader", et.preheader.as_deref().unwrap_or("")),
    ] {
        if !body_has_placeholders(body) {
            continue;
        }
        let errors = unresolved_user_managed(body, &lookup);
        if !errors.is_empty() {
            out.push(ResolutionFailure {
                resource_kind: "email_template",
                resource_name: et.name.clone(),
                field: Some(field),
                errors,
            });
        }
    }
    out
}

/// Return `ResolutionError::UnknownKey` entries for every
/// **custom / global** placeholder in `body` whose key is not in
/// `lookup`. lid / cb_id placeholders are ignored — they are validated
/// later against the remote body.
fn unresolved_user_managed(
    body: &str,
    lookup: &BTreeMap<LookupKey, String>,
) -> Vec<ResolutionError> {
    let mut errors = Vec::new();
    for ph in crate::values::placeholder::extract_placeholders(body) {
        match ph.ty {
            PlaceholderType::Custom | PlaceholderType::Global => {
                let key = (ph.ty, ph.key.clone());
                if !lookup.contains_key(&key) {
                    errors.push(ResolutionError::UnknownKey {
                        ty: ph.ty,
                        key: ph.key,
                        start: ph.start,
                    });
                }
            }
            PlaceholderType::Lid | PlaceholderType::CbId => {}
        }
    }
    errors
}

/// Compute per-resource "consumed values" hashes for plan-lock
/// integrity checking.
///
/// v0.15 model: only `custom` / `global` consumption contributes to
/// the hash. lid / cb_id values come from the remote body and are
/// expected to differ between plan and apply (dashboard edits are
/// normal); the plan-lock catches values yaml edits, not Braze drift.
pub fn compute_values_input_hashes(
    args: PreflightArgs<'_>,
    values: Option<&ValuesFile>,
) -> Result<BTreeMap<String, String>> {
    use crate::resource::ResourceKind;

    let has_cb = args.kinds.contains(&ResourceKind::ContentBlock);
    let has_et = args.kinds.contains(&ResourceKind::EmailTemplate);
    if !has_cb && !has_et {
        return Ok(BTreeMap::new());
    }

    let mut hashes: BTreeMap<String, String> = BTreeMap::new();

    if has_cb && args.content_blocks_root.exists() {
        let mut locals =
            crate::fs::content_block_io::load_all_content_blocks(args.content_blocks_root)
                .map_err(|e| Error::Config(format!("loading content_block locals: {e}")))?;
        if let Some(name) = args.cb_name_filter {
            locals.retain(|c| c.name == name);
        }
        locals.retain(|c| !crate::config::is_excluded(&c.name, args.cb_excludes));
        for cb in locals {
            if !body_has_placeholders(&cb.content) {
                continue;
            }
            let consumed = consumed_for_content_block(&cb, values);
            if consumed.is_empty() {
                continue;
            }
            let key = format!("content_block/{}", cb.name);
            hashes.insert(key, hash_consumed_map(&consumed));
        }
    }

    if has_et && args.email_templates_root.exists() {
        let mut locals =
            crate::fs::email_template_io::load_all_email_templates(args.email_templates_root)
                .map_err(|e| Error::Config(format!("loading email_template locals: {e}")))?;
        if let Some(name) = args.et_name_filter {
            locals.retain(|t| t.name == name);
        }
        locals.retain(|t| !crate::config::is_excluded(&t.name, args.et_excludes));
        for et in locals {
            let any_ph = body_has_placeholders(&et.subject)
                || body_has_placeholders(&et.body_html)
                || body_has_placeholders(&et.body_plaintext)
                || et.preheader.as_deref().is_some_and(body_has_placeholders);
            if !any_ph {
                continue;
            }
            let consumed = consumed_for_email_template(&et, values);
            if consumed.is_empty() {
                continue;
            }
            let key = format!("email_template/{}", et.name);
            hashes.insert(key, hash_consumed_map(&consumed));
        }
    }

    Ok(hashes)
}

fn consumed_for_content_block(
    cb: &crate::resource::ContentBlock,
    values: Option<&ValuesFile>,
) -> BTreeMap<String, String> {
    let lookup = build_user_managed_lookup_cb(&cb.name, values);
    let mut consumed: BTreeMap<String, String> = BTreeMap::new();
    for ph in crate::values::placeholder::extract_placeholders(&cb.content) {
        if !matches!(ph.ty, PlaceholderType::Custom | PlaceholderType::Global) {
            continue;
        }
        let lk = (ph.ty, ph.key.clone());
        if let Some(v) = lookup.get(&lk) {
            consumed.insert(format!("{}.{}", ph.ty.as_str(), ph.key), v.clone());
        }
    }
    consumed
}

fn consumed_for_email_template(
    et: &crate::resource::EmailTemplate,
    values: Option<&ValuesFile>,
) -> BTreeMap<String, String> {
    let lookup = build_user_managed_lookup_et(&et.name, values);
    let mut consumed: BTreeMap<String, String> = BTreeMap::new();
    for (field_name, body) in [
        ("subject", et.subject.as_str()),
        ("body_html", et.body_html.as_str()),
        ("body_plaintext", et.body_plaintext.as_str()),
        ("preheader", et.preheader.as_deref().unwrap_or("")),
    ] {
        if !body_has_placeholders(body) {
            continue;
        }
        for ph in crate::values::placeholder::extract_placeholders(body) {
            if !matches!(ph.ty, PlaceholderType::Custom | PlaceholderType::Global) {
                continue;
            }
            let lk = (ph.ty, ph.key.clone());
            if let Some(v) = lookup.get(&lk) {
                consumed.insert(
                    format!("{field_name}.{}.{}", ph.ty.as_str(), ph.key),
                    v.clone(),
                );
            }
        }
    }
    consumed
}

fn hash_consumed_map(consumed: &BTreeMap<String, String>) -> String {
    let bytes =
        serde_json::to_vec(consumed).expect("BTreeMap<String, String> serialization is infallible");
    blake3::hash(&bytes).to_hex().to_string()
}

/// Format aggregated failures into a single human-readable error.
pub fn format_failures(
    failures: &[ResolutionFailure],
    values_path: &Path,
    values_loaded: bool,
) -> Error {
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
                        "    - offset {}: __BRAZESYNC.{}.{}__ (key not in values)\n",
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
    if values_loaded {
        msg.push_str(&format!(
            "\nResolve by adding the missing keys to {}.",
            values_path.display(),
        ));
    } else {
        msg.push_str(&format!(
            "\nNo values file was loaded at {}. Create it (or set environments.<env>.values_file in your config) \
             and add the missing custom/global entries.",
            values_path.display(),
        ));
    }
    Error::Config(msg)
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

    fn values_yaml(s: &str) -> ValuesFile {
        serde_norway::from_str(s).expect("test yaml parses")
    }

    #[test]
    fn no_placeholders_skips_resolution() {
        let mut block = cb("plain", "<p>hi there</p>");
        resolve_content_block_with_remote(&mut block, None, None).unwrap();
        assert_eq!(block.content, "<p>hi there</p>");
    }

    #[test]
    fn content_block_resolves_lid_from_remote_and_custom_from_values() {
        let v = values_yaml(
            r#"
version: 1
globals:
  custom:
    host:
      value: api-prod.example.com
content_block:
  promo:
    custom:
      variant:
        value: A
"#,
        );
        let mut block = cb(
            "promo",
            "host=__BRAZESYNC.global.host__ variant=__BRAZESYNC.custom.variant__ \
             <a href=\"https://x.com/cta\">__BRAZESYNC.lid.cta__</a>",
        );
        let remote = cb(
            "promo",
            "ignored {{x | lid: 'newlidvalue1'}} ignored \
             <a href=\"https://x.com/cta\">{{x | lid: 'newlidvalue1'}}</a>",
        );
        resolve_content_block_with_remote(&mut block, Some(&remote), Some(&v)).unwrap();
        assert!(block.content.contains("host=api-prod.example.com"));
        assert!(block.content.contains("variant=A"));
        assert!(block.content.contains(">newlidvalue1<"));
    }

    #[test]
    fn missing_custom_aborts_with_failure() {
        let mut block = cb("promo", "__BRAZESYNC.custom.unknown__");
        let err = resolve_content_block_with_remote(&mut block, None, None).unwrap_err();
        assert_eq!(err.resource_kind, "content_block");
        assert_eq!(err.errors.len(), 1);
    }

    #[test]
    fn new_resource_lid_fallback_uses_placeholder_key() {
        let mut block = cb(
            "promo",
            r#"<a href="https://x.com/spring">__BRAZESYNC.lid.spring_sale__</a>"#,
        );
        resolve_content_block_with_remote(&mut block, None, None).unwrap();
        assert!(block.content.contains(">spring_sale<"));
    }

    #[test]
    fn new_resource_cb_id_filter_is_stripped() {
        let mut block = cb(
            "page",
            "{{content_blocks.${promo} | id: '__BRAZESYNC.cb_id.promo__'}}",
        );
        resolve_content_block_with_remote(&mut block, None, None).unwrap();
        assert_eq!(block.content, "{{content_blocks.${promo}}}");
    }

    #[test]
    fn email_template_field_resolution_per_field() {
        let v = values_yaml(
            r#"
version: 1
email_template:
  welcome:
    custom:
      seg:
        value: seg_prod
"#,
        );
        let mut t = et("welcome");
        t.subject = "seg=__BRAZESYNC.custom.seg__".into();
        t.body_html =
            r#"<a href="https://x.com/cta">__BRAZESYNC.lid.cta__</a>"#.into();
        let mut remote = et("welcome");
        remote.body_html =
            r#"<a href="https://x.com/cta">{{x | lid: 'newhtmllidx'}}</a>"#.into();
        resolve_email_template_with_remote(&mut t, Some(&remote), Some(&v)).unwrap();
        assert_eq!(t.subject, "seg=seg_prod");
        assert!(t.body_html.contains(">newhtmllidx<"));
    }

    #[test]
    fn preflight_only_checks_user_managed() {
        // lid placeholder with no values entry — preflight should NOT fail,
        // because lid resolves at plan compute time from the remote body.
        let mut block = cb("promo", "<a href=\"https://x.com/cta\">__BRAZESYNC.lid.cta__</a>");
        let failure = preflight_user_managed_cb(&block, None, &mut Vec::new());
        assert!(failure.is_none(), "lid must not be checked at preflight");

        // custom placeholder with no values entry — preflight MUST fail.
        block.content = "__BRAZESYNC.custom.required_thing__".into();
        let failure = preflight_user_managed_cb(&block, None, &mut Vec::new());
        assert!(failure.is_some(), "custom must be checked at preflight");
    }

    #[test]
    fn format_failures_mentions_values_path_when_missing() {
        let failures = vec![ResolutionFailure {
            resource_kind: "content_block",
            resource_name: "promo".into(),
            field: None,
            errors: vec![ResolutionError::UnknownKey {
                ty: PlaceholderType::Custom,
                key: "host".into(),
                start: 0,
            }],
        }];
        let err = format_failures(&failures, Path::new("/x/values/prod.yaml"), false);
        let msg = err.to_string();
        assert!(msg.contains("content_block 'promo'"));
        assert!(msg.contains("__BRAZESYNC.custom.host__"));
        assert!(msg.contains("No values file was loaded"));
    }
}
