//! Wiring layer between [`crate::values`] (Phase 1) and the diff /
//! apply pipeline.
//!
//! Responsibilities:
//! - Resolve the per-env values file path from config (`values_file`
//!   override or default `values/<env>.yaml`).
//! - Build the per-resource flat lookup table from the structured
//!   [`ValuesFile`] per RFC §2.2 namespace rules.
//! - Aggregate placeholder failures across resources so the caller can
//!   abort with a single grouped report (RFC §2.4 / §3 Q7).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::ResolvedConfig;
use crate::error::{Error, Result};
use crate::resource::{ContentBlock, EmailTemplate};
use crate::values::placeholder::{
    find_suspicious_placeholders, resolve_placeholders, LookupKey, PlaceholderType, ResolutionError,
};
use crate::values::schema::{default_values_path, ValuesFile};

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
///
/// Missing file → `Ok(None)`. Unresolved placeholders later (against an
/// empty fallback) will surface as `ResolutionFailure`s with a hint to
/// create the file (see [`format_failures`]).
///
/// Present-but-malformed → propagates `Error::YamlParse` /
/// `Error::InvalidFormat` (RFC §5 Edge cases — abort early, before any
/// API call).
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

/// Resolve every `__BRAZESYNC.*__` in `cb.content` against `values`.
/// content_block bodies are single-field, so all `lid` / `cb_id` /
/// `custom` namespaces live at the resource root (RFC §2.2).
pub fn resolve_content_block_in_place(
    cb: &mut ContentBlock,
    values: Option<&ValuesFile>,
) -> std::result::Result<(), ResolutionFailure> {
    if !body_has_placeholders(&cb.content) {
        return Ok(());
    }
    let lookup = build_content_block_lookup(&cb.name, values);
    match resolve_placeholders(&cb.content, &lookup) {
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

/// Resolve placeholders across every Liquid-bearing field of `et`. Each
/// field has its own per-field `lid` / `cb_id` namespace; `custom` is
/// resource-scoped (RFC §2.2). Failures are aggregated *per field* so a
/// single email_template can surface multiple `ResolutionFailure`s.
pub fn resolve_email_template_in_place(
    et: &mut EmailTemplate,
    values: Option<&ValuesFile>,
) -> std::result::Result<(), Vec<ResolutionFailure>> {
    // Snapshot field refs so the borrow of `et` doesn't conflict with
    // the per-field mut writes below. The macro keeps it readable.
    let mut failures: Vec<ResolutionFailure> = Vec::new();

    macro_rules! resolve_field {
        ($field_name:expr, $accessor:expr) => {{
            let body: &str = $accessor;
            if body_has_placeholders(body) {
                let lookup = build_email_template_lookup(&et.name, $field_name, values);
                match resolve_placeholders(body, &lookup) {
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

    let new_subject = resolve_field!("subject", et.subject.as_str());
    let new_body_html = resolve_field!("body_html", et.body_html.as_str());
    let new_body_plaintext = resolve_field!("body_plaintext", et.body_plaintext.as_str());
    let new_preheader = match et.preheader.as_deref() {
        Some(s) => resolve_field!("preheader", s),
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

/// Quick filter to avoid building a lookup map for bodies that have no
/// placeholders. The strict extractor would also be empty in that case
/// but a single substring scan is cheaper than parsing tokens.
fn body_has_placeholders(body: &str) -> bool {
    body.contains("__BRAZESYNC.")
}

/// Build the `(type, key) -> value` lookup for one content_block. Pulls
/// from `globals.custom` for `global`, and from `content_block.<name>.*`
/// for the other three namespaces.
fn build_content_block_lookup(
    name: &str,
    values: Option<&ValuesFile>,
) -> BTreeMap<LookupKey, String> {
    let mut out = BTreeMap::new();
    let Some(vf) = values else {
        return out;
    };
    insert_globals(&mut out, vf);
    if let Some(cb) = vf.content_block.get(name) {
        for (k, e) in &cb.lid {
            if let Some(v) = &e.value {
                out.insert((PlaceholderType::Lid, k.clone()), v.clone());
            }
        }
        for (k, e) in &cb.cb_id {
            if let Some(v) = &e.value {
                out.insert((PlaceholderType::CbId, k.clone()), v.clone());
            }
        }
        for (k, e) in &cb.custom {
            if let Some(v) = &e.value {
                out.insert((PlaceholderType::Custom, k.clone()), v.clone());
            }
        }
    }
    out
}

/// Build the lookup for one email_template field. `custom` is taken
/// from the resource root (shared across fields); `lid` / `cb_id` from
/// the field-specific namespace (RFC §2.2).
fn build_email_template_lookup(
    name: &str,
    field: &str,
    values: Option<&ValuesFile>,
) -> BTreeMap<LookupKey, String> {
    let mut out = BTreeMap::new();
    let Some(vf) = values else {
        return out;
    };
    insert_globals(&mut out, vf);
    let Some(et) = vf.email_template.get(name) else {
        return out;
    };
    for (k, e) in &et.custom {
        if let Some(v) = &e.value {
            out.insert((PlaceholderType::Custom, k.clone()), v.clone());
        }
    }
    let field_values = match field {
        "subject" => &et.subject,
        "preheader" => &et.preheader,
        "body_html" => &et.body_html,
        "body_plaintext" => &et.body_plaintext,
        // Other fields can't contain placeholders by construction.
        _ => return out,
    };
    for (k, e) in &field_values.lid {
        if let Some(v) = &e.value {
            out.insert((PlaceholderType::Lid, k.clone()), v.clone());
        }
    }
    for (k, e) in &field_values.cb_id {
        if let Some(v) = &e.value {
            out.insert((PlaceholderType::CbId, k.clone()), v.clone());
        }
    }
    out
}

/// RFC §2.3: surface envelope-shaped tokens that don't match the strict
/// placeholder grammar as warnings, so typos like `__BRAZSYNC.lid.foo__`
/// (missing E) or unknown types like `__BRAZESYNC.url.foo__` don't pass
/// through silently. These wouldn't trigger the unresolved-key abort
/// because the strict extractor returns nothing for them.
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

fn insert_globals(out: &mut BTreeMap<LookupKey, String>, vf: &ValuesFile) {
    for (k, e) in &vf.globals.custom {
        if let Some(v) = &e.value {
            out.insert((PlaceholderType::Global, k.clone()), v.clone());
        }
    }
}

/// Bundle of inputs the pre-flight needs from the entry layer. Kept as
/// a struct (rather than 10 positional args) to keep clippy happy and
/// to make future additions (e.g. a new kind that grows placeholders)
/// non-breaking at call sites.
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
/// `__BRAZESYNC.*__` placeholders resolve against the loaded values
/// file. Returns the loaded `ValuesFile` (or `None` if absent and no
/// placeholders are used) on success; on any failure across any kind,
/// aborts with an aggregated [`format_failures`] error.
///
/// This is the "pre-flight" gate the apply / diff entry points call
/// before any Braze API request (RFC §2.4 / §3 Q7).
pub fn preflight_values(args: PreflightArgs<'_>) -> Result<Option<ValuesFile>> {
    use crate::resource::ResourceKind;

    // Skip values-file IO entirely when no placeholder-bearing kind is
    // selected. Otherwise a malformed `values/<env>.yaml` would abort
    // unrelated commands like `apply --resource tag` even though tag /
    // catalog_schema / custom_attribute never consume the values file.
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
        for mut cb in locals {
            warn_suspicious(
                "content_block",
                &cb.name,
                None,
                find_suspicious_placeholders(&cb.content),
            );
            if let Err(f) = resolve_content_block_in_place(&mut cb, values.as_ref()) {
                failures.push(f);
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
        for mut t in locals {
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
            if let Err(per_field_failures) =
                resolve_email_template_in_place(&mut t, values.as_ref())
            {
                failures.extend(per_field_failures);
            }
        }
    }

    if !failures.is_empty() {
        return Err(format_failures(&failures, &values_path, values_loaded));
    }

    Ok(values)
}

/// Compute per-resource "consumed values" hashes for plan-lock
/// integrity checking (RFC §4 Phase 6).
///
/// For each placeholder-bearing local resource we extract the
/// `(type, key)` pairs from the templated body, look up each in the
/// values file using the same namespace rules as
/// `resolve_*_in_place`, build a stable `BTreeMap<String, String>`
/// of consumed entries, serialize via `serde_json`, and blake3-hash
/// the resulting bytes.
///
/// Returned map keys are `"<kind>/<name>"` so the plan file's JSON
/// representation has stable, grep-able keys; values are blake3 hex
/// digests (64 chars).
///
/// blake3 vs the RFC's "SHA-256": both are 32-byte cryptographic
/// digests, both deterministic, both already collision-resistant for
/// this scale. blake3 is already a transitive dep (used by catalog
/// items diffing), so we keep the dep surface minimal and document
/// the choice here.
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
    let lookup = build_content_block_lookup(&cb.name, values);
    let mut consumed: BTreeMap<String, String> = BTreeMap::new();
    for ph in crate::values::placeholder::extract_placeholders(&cb.content) {
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
    // Field-prefix the key so two fields that reference the same
    // (type, key) but resolve to different per-field values don't
    // collapse to a single BTreeMap entry. The hash must distinguish
    // them — apply later resolves field-scoped, so plan and apply
    // need the same view.
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
        let lookup = build_email_template_lookup(&et.name, field_name, values);
        for ph in crate::values::placeholder::extract_placeholders(body) {
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
    // BTreeMap iteration is sorted, so serde_json::to_vec produces a
    // stable byte stream. We accept the unwrap because BTreeMap of
    // primitives can't fail serialization.
    let bytes =
        serde_json::to_vec(consumed).expect("BTreeMap<String, String> serialization is infallible");
    blake3::hash(&bytes).to_hex().to_string()
}

/// Format aggregated failures into a single human-readable error.
/// Adds the "values file missing" hint when `values_loaded == false` so
/// the operator immediately knows where to look.
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
            "\nResolve by adding the missing keys to {} or running `braze-sync export --env=<env>`.",
            values_path.display(),
        ));
    } else {
        msg.push_str(&format!(
            "\nNo values file was loaded at {}. Create it (or set environments.<env>.values_file in your config), \
             then add the missing keys or run `braze-sync export --env=<env>` to populate them.",
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
    fn no_placeholders_skips_resolution_even_without_values() {
        let mut block = cb("plain", "<p>hi there</p>");
        resolve_content_block_in_place(&mut block, None).unwrap();
        assert_eq!(block.content, "<p>hi there</p>");
    }

    #[test]
    fn resolves_content_block_lid_custom_global() {
        let v = values_yaml(
            r#"
version: 1
globals:
  custom:
    host:
      value: api-prod.example.com
content_block:
  promo:
    lid:
      cta:
        value: ai8kexrxcp03
        url: https://example.com/cta
    custom:
      variant:
        value: A
"#,
        );
        let mut block = cb(
            "promo",
            "host=__BRAZESYNC.global.host__ variant=__BRAZESYNC.custom.variant__ \
             lid=__BRAZESYNC.lid.cta__",
        );
        resolve_content_block_in_place(&mut block, Some(&v)).unwrap();
        assert_eq!(
            block.content,
            "host=api-prod.example.com variant=A lid=ai8kexrxcp03"
        );
    }

    #[test]
    fn missing_values_file_aggregates_failures() {
        let mut block = cb("promo", "__BRAZESYNC.lid.cta__");
        let err = resolve_content_block_in_place(&mut block, None).unwrap_err();
        assert_eq!(err.resource_kind, "content_block");
        assert_eq!(err.resource_name, "promo");
        assert_eq!(err.errors.len(), 1);
    }

    #[test]
    fn email_template_field_scoped_lid_namespaces() {
        let v = values_yaml(
            r#"
version: 1
email_template:
  welcome:
    custom:
      seg:
        value: seg_prod
    subject:
      lid:
        s_lid:
          value: lidsubject01
          anchor: "{{promo}}"
    body_html:
      lid:
        h_lid:
          value: lidhtml01001
          url: https://example.com/cta
"#,
        );
        let mut t = et("welcome");
        t.subject = "x=__BRAZESYNC.lid.s_lid__ seg=__BRAZESYNC.custom.seg__".into();
        t.body_html = "<a>__BRAZESYNC.lid.h_lid__</a>".into();
        resolve_email_template_in_place(&mut t, Some(&v)).unwrap();
        assert_eq!(t.subject, "x=lidsubject01 seg=seg_prod");
        assert_eq!(t.body_html, "<a>lidhtml01001</a>");
    }

    #[test]
    fn email_template_lid_in_wrong_field_fails() {
        // subject.lid.s_lid is declared but body_html references the
        // same key — should fail because field-scoped namespace
        // doesn't cross.
        let v = values_yaml(
            r#"
version: 1
email_template:
  welcome:
    subject:
      lid:
        s_lid:
          value: lidsubject01
          anchor: "{{promo}}"
"#,
        );
        let mut t = et("welcome");
        t.body_html = "__BRAZESYNC.lid.s_lid__".into();
        let err = resolve_email_template_in_place(&mut t, Some(&v)).unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].field, Some("body_html"));
    }

    #[test]
    fn email_template_aggregates_failures_across_fields() {
        let mut t = et("welcome");
        t.subject = "__BRAZESYNC.lid.x__".into();
        t.body_html = "__BRAZESYNC.lid.y__".into();
        let err = resolve_email_template_in_place(&mut t, None).unwrap_err();
        assert_eq!(err.len(), 2);
        let fields: Vec<_> = err.iter().filter_map(|f| f.field).collect();
        assert!(fields.contains(&"subject"));
        assert!(fields.contains(&"body_html"));
    }

    #[test]
    fn format_failures_mentions_values_path_when_missing() {
        let failures = vec![ResolutionFailure {
            resource_kind: "content_block",
            resource_name: "promo".into(),
            field: None,
            errors: vec![ResolutionError::UnknownKey {
                ty: PlaceholderType::Lid,
                key: "cta".into(),
                start: 0,
            }],
        }];
        let err = format_failures(&failures, Path::new("/x/values/prod.yaml"), false);
        let msg = err.to_string();
        assert!(msg.contains("content_block 'promo'"));
        assert!(msg.contains("__BRAZESYNC.lid.cta__"));
        assert!(msg.contains("No values file was loaded"));
    }

    #[test]
    fn format_failures_omits_missing_hint_when_loaded() {
        let failures = vec![ResolutionFailure {
            resource_kind: "content_block",
            resource_name: "promo".into(),
            field: None,
            errors: vec![ResolutionError::UnknownKey {
                ty: PlaceholderType::Lid,
                key: "cta".into(),
                start: 0,
            }],
        }];
        let err = format_failures(&failures, Path::new("/x/values/prod.yaml"), true);
        let msg = err.to_string();
        assert!(msg.contains("Resolve by adding"));
        assert!(!msg.contains("No values file was loaded"));
    }
}
