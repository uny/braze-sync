//! `braze-sync templatize` — one-shot migration from raw lid/cb_id
//! bodies to templated bodies + per-env values (RFC §2.7).
//!
//! Operates on local files only — no Braze API calls. Validates that
//! `--from-env` is declared in the config, walks every local
//! content_block and email_template, rewrites detected `lid` /
//! `cb_id` occurrences to `__BRAZESYNC.<type>.<key>__` placeholders,
//! and writes:
//!   - `values/<from-env>.yaml` with the actual lid / cb_id values
//!     observed in the local body (the canonical env);
//!   - `values/<other-env>.yaml` for every other env in the config,
//!     as a *skeleton* with the same key structure but `value: null`
//!     (RFC §2.7 step 6).
//!
//! Skeletons signal "needs `braze-sync export --env=<other-env>`" to
//! the per-env apply pre-flight — `value: null` survives shape
//! validation (§5 Edge cases) and abort is delegated to the unresolved
//! placeholder check.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _};
use clap::Args;

use crate::config::ConfigFile;
use crate::error::Error;
use crate::fs::{content_block_io, email_template_io};
use crate::resource::{ContentBlock, EmailTemplate};
use crate::values::schema::{
    CbIdEntry, ContentBlockValues, FieldValues, LidEntry, SUPPORTED_VERSION,
};
use crate::values::templatize::{templatize_body, DetectedEntry, FieldKind};
use crate::values::ValuesFile;

#[derive(Args, Debug)]
pub struct TemplatizeArgs {
    /// Canonical environment whose current body values are treated as
    /// truth and copied into `values/<from-env>.yaml`. All other envs
    /// declared in the config get a `value: null` skeleton file.
    #[arg(long, value_name = "ENV")]
    pub from_env: String,

    /// Preview-only mode. Walks the same code path but does not touch
    /// any file on disk. Prints a summary of what would change.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: &TemplatizeArgs, cfg: &ConfigFile, config_dir: &Path) -> anyhow::Result<()> {
    if !cfg.environments.contains_key(&args.from_env) {
        let known: Vec<&str> = cfg.environments.keys().map(String::as_str).collect();
        return Err(anyhow!(
            "unknown --from-env '{}'; declared envs: [{}]",
            args.from_env,
            known.join(", ")
        ));
    }

    let content_blocks_root = config_dir.join(&cfg.resources.content_block.path);
    let email_templates_root = config_dir.join(&cfg.resources.email_template.path);

    // If a values file for the canonical env already exists, load it as
    // the base so re-runs preserve operator-curated content (e.g.
    // `globals.custom`, entries for resources not touched this run,
    // and existing placeholder keys from earlier partial migrations).
    // Fresh runs start from an empty document.
    let canonical_path = values_path_for(config_dir, cfg, &args.from_env);
    let mut canonical = if canonical_path.exists() {
        ValuesFile::load(&canonical_path).with_context(|| {
            format!(
                "loading existing canonical values file {} before merge",
                canonical_path.display()
            )
        })?
    } else {
        ValuesFile {
            version: SUPPORTED_VERSION,
            ..Default::default()
        }
    };
    let mut summary = RunSummary::default();
    let mut content_block_rewrites: Vec<(PathBuf, ContentBlock)> = Vec::new();
    let mut email_template_rewrites: Vec<(PathBuf, EmailTemplate)> = Vec::new();

    if cfg.resources.content_block.enabled && content_blocks_root.exists() {
        let blocks = content_block_io::load_all_content_blocks(&content_blocks_root)
            .context("loading local content_blocks for templatize")?;
        for mut cb in blocks {
            let result = templatize_body(&cb.content, FieldKind::ContentBlock);
            if result.entries.is_empty() && cb.content.contains("__BRAZESYNC.") {
                summary.skipped.push(format!(
                    "content_block '{}' already templated — skipping",
                    cb.name
                ));
                continue;
            }
            if result.entries.is_empty() {
                // No placeholders, nothing to do.
                continue;
            }

            let entry = canonical.content_block.entry(cb.name.clone()).or_default();
            apply_entries_content_block(entry, &result.entries);
            for w in &result.warnings {
                summary
                    .warnings
                    .push(format!("content_block '{}': {w}", cb.name));
            }
            summary.touched_resources += 1;
            summary.lid_rewrites += count_lid(&result.entries);
            summary.cb_id_rewrites += count_cb_id(&result.entries);

            cb.content = result.new_body;
            let target = content_blocks_root.join(format!("{}.liquid", cb.name));
            content_block_rewrites.push((target, cb));
        }
    }

    if cfg.resources.email_template.enabled && email_templates_root.exists() {
        let templates = email_template_io::load_all_email_templates(&email_templates_root)
            .context("loading local email_templates for templatize")?;
        for mut et in templates {
            let already_templated = et.subject.contains("__BRAZESYNC.")
                || et.body_html.contains("__BRAZESYNC.")
                || et.body_plaintext.contains("__BRAZESYNC.")
                || et
                    .preheader
                    .as_deref()
                    .is_some_and(|p| p.contains("__BRAZESYNC."));

            let subject_r = templatize_body(&et.subject, FieldKind::EmailSubject);
            let body_html_r = templatize_body(&et.body_html, FieldKind::EmailHtmlBody);
            let body_plain_r = templatize_body(&et.body_plaintext, FieldKind::EmailPlainBody);
            let preheader_r = et
                .preheader
                .as_ref()
                .map(|p| templatize_body(p, FieldKind::EmailPreheader));

            let any_rewrite = !(subject_r.entries.is_empty()
                && body_html_r.entries.is_empty()
                && body_plain_r.entries.is_empty()
                && preheader_r.as_ref().is_none_or(|r| r.entries.is_empty()));

            if !any_rewrite {
                if already_templated {
                    summary.skipped.push(format!(
                        "email_template '{}' already templated — skipping",
                        et.name
                    ));
                }
                continue;
            }

            let entry = canonical.email_template.entry(et.name.clone()).or_default();
            apply_entries_email_template_field(
                &mut entry.subject,
                &subject_r.entries,
                &mut summary.warnings,
                &et.name,
                "subject",
                &subject_r.warnings,
            );
            apply_entries_email_template_field(
                &mut entry.body_html,
                &body_html_r.entries,
                &mut summary.warnings,
                &et.name,
                "body_html",
                &body_html_r.warnings,
            );
            apply_entries_email_template_field(
                &mut entry.body_plaintext,
                &body_plain_r.entries,
                &mut summary.warnings,
                &et.name,
                "body_plaintext",
                &body_plain_r.warnings,
            );
            if let Some(r) = preheader_r.as_ref() {
                apply_entries_email_template_field(
                    &mut entry.preheader,
                    &r.entries,
                    &mut summary.warnings,
                    &et.name,
                    "preheader",
                    &r.warnings,
                );
            }

            summary.touched_resources += 1;
            summary.lid_rewrites += count_lid(&subject_r.entries)
                + count_lid(&body_html_r.entries)
                + count_lid(&body_plain_r.entries)
                + preheader_r
                    .as_ref()
                    .map(|r| count_lid(&r.entries))
                    .unwrap_or(0);
            summary.cb_id_rewrites += count_cb_id(&subject_r.entries)
                + count_cb_id(&body_html_r.entries)
                + count_cb_id(&body_plain_r.entries)
                + preheader_r
                    .as_ref()
                    .map(|r| count_cb_id(&r.entries))
                    .unwrap_or(0);

            et.subject = subject_r.new_body;
            et.body_html = body_html_r.new_body;
            et.body_plaintext = body_plain_r.new_body;
            if let Some(r) = preheader_r {
                et.preheader = Some(r.new_body);
            }
            email_template_rewrites.push((email_templates_root.join(&et.name), et));
        }
    }

    // Build skeleton file contents for every non-canonical env. Same
    // key structure, same correlation metadata, but `value: null` so
    // apply pre-flight aborts until export populates it.
    let mut skeleton_paths: Vec<(String, PathBuf, ValuesFile)> = Vec::new();
    for env_name in cfg.environments.keys() {
        if env_name == &args.from_env {
            continue;
        }
        let skeleton = canonical.skeleton_clone();
        let path = values_path_for(config_dir, cfg, env_name);
        skeleton_paths.push((env_name.clone(), path, skeleton));
    }

    // Emit summary to stderr regardless of dry-run.
    eprintln!("templatize summary (--from-env={}):", args.from_env);
    eprintln!(
        "  • touched {} resource(s); {} lid + {} cb_id rewrite(s)",
        summary.touched_resources, summary.lid_rewrites, summary.cb_id_rewrites
    );
    for s in &summary.skipped {
        eprintln!("  • {s}");
    }
    for w in &summary.warnings {
        eprintln!("  ⚠ {w}");
    }

    if summary.touched_resources == 0 {
        eprintln!("nothing to templatize.");
        return Ok(());
    }

    if args.dry_run {
        eprintln!("(dry-run) would write:");
        eprintln!("  • {}", canonical_path.display());
        for (env, path, _) in &skeleton_paths {
            eprintln!("  • {} (skeleton for env '{}')", path.display(), env);
        }
        eprintln!(
            "  • {} resource file(s) rewritten in place",
            content_block_rewrites.len() + email_template_rewrites.len()
        );
        return Ok(());
    }

    // Write the canonical values file first — apply / diff pre-flight
    // depends on it, and a failed rewrite later still leaves a usable
    // values file for the from-env.
    canonical.save(&canonical_path)?;
    let mut written_skeletons: Vec<(String, PathBuf)> = Vec::new();
    for (env, path, skeleton) in &skeleton_paths {
        // Refuse to overwrite an existing skeleton: a user may have
        // already exported real values for that env.
        if path.exists() {
            eprintln!(
                "  • skipping skeleton for env '{}': {} already exists",
                env,
                path.display()
            );
            continue;
        }
        skeleton.save(path)?;
        written_skeletons.push((env.clone(), path.clone()));
    }
    for (path, cb) in &content_block_rewrites {
        content_block_io::save_content_block(path.parent().unwrap_or_else(|| Path::new(".")), cb)?;
    }
    for (_, et) in &email_template_rewrites {
        email_template_io::save_email_template(&email_templates_root, et)?;
    }

    eprintln!("✓ templatize: wrote {}", canonical_path.display());
    for (env, path) in &written_skeletons {
        eprintln!(
            "✓ templatize: wrote {} (skeleton for '{}')",
            path.display(),
            env
        );
    }
    Ok(())
}

#[derive(Default)]
struct RunSummary {
    touched_resources: usize,
    lid_rewrites: usize,
    cb_id_rewrites: usize,
    skipped: Vec<String>,
    warnings: Vec<String>,
}

fn count_lid(entries: &[DetectedEntry]) -> usize {
    entries
        .iter()
        .filter(|e| matches!(e, DetectedEntry::Lid { .. }))
        .count()
}
fn count_cb_id(entries: &[DetectedEntry]) -> usize {
    entries
        .iter()
        .filter(|e| matches!(e, DetectedEntry::CbId { .. }))
        .count()
}

fn apply_entries_content_block(cb_values: &mut ContentBlockValues, entries: &[DetectedEntry]) {
    for entry in entries {
        match entry {
            DetectedEntry::Lid { key, value, url } => {
                cb_values.lid.insert(
                    key.clone(),
                    LidEntry {
                        value: Some(value.clone()),
                        url: url.clone(),
                        anchor: None,
                    },
                );
            }
            DetectedEntry::CbId { key, value, .. } => {
                cb_values.cb_id.insert(
                    key.clone(),
                    CbIdEntry {
                        value: Some(value.clone()),
                    },
                );
            }
        }
    }
}

fn apply_entries_email_template_field(
    field: &mut FieldValues,
    entries: &[DetectedEntry],
    out_warnings: &mut Vec<String>,
    et_name: &str,
    field_name: &str,
    field_warnings: &[String],
) {
    for entry in entries {
        match entry {
            DetectedEntry::Lid { key, value, url } => {
                field.lid.insert(
                    key.clone(),
                    LidEntry {
                        value: Some(value.clone()),
                        url: url.clone(),
                        anchor: None,
                    },
                );
            }
            DetectedEntry::CbId { key, value, .. } => {
                field.cb_id.insert(
                    key.clone(),
                    CbIdEntry {
                        value: Some(value.clone()),
                    },
                );
            }
        }
    }
    for w in field_warnings {
        out_warnings.push(format!("email_template '{et_name}' ({field_name}): {w}"));
    }
}

fn values_path_for(config_dir: &Path, cfg: &ConfigFile, env_name: &str) -> PathBuf {
    if let Some(env) = cfg.environments.get(env_name) {
        if let Some(custom) = &env.values_file {
            if custom.is_absolute() {
                return custom.clone();
            }
            return config_dir.join(custom);
        }
    }
    crate::values::schema::default_values_path(config_dir, env_name)
}

/// Build a `value: null` skeleton with the same key + correlation
/// metadata as `self`. Used by templatize to pre-populate per-env
/// files for non-canonical envs (RFC §2.7 step 6).
impl ValuesFile {
    pub fn skeleton_clone(&self) -> ValuesFile {
        let mut out = ValuesFile {
            version: self.version,
            ..Default::default()
        };
        // globals.custom keeps its keys (user-managed) but values blank.
        for k in self.globals.custom.keys() {
            out.globals.custom.insert(
                k.clone(),
                crate::values::schema::CustomEntry { value: None },
            );
        }
        for (name, src) in &self.content_block {
            let dst = out.content_block.entry(name.clone()).or_default();
            for (k, e) in &src.lid {
                dst.lid.insert(
                    k.clone(),
                    LidEntry {
                        value: None,
                        url: e.url.clone(),
                        anchor: e.anchor.clone(),
                    },
                );
            }
            for k in src.cb_id.keys() {
                dst.cb_id.insert(k.clone(), CbIdEntry { value: None });
            }
            for k in src.custom.keys() {
                dst.custom.insert(
                    k.clone(),
                    crate::values::schema::CustomEntry { value: None },
                );
            }
        }
        for (name, src) in &self.email_template {
            let dst = out.email_template.entry(name.clone()).or_default();
            for k in src.custom.keys() {
                dst.custom.insert(
                    k.clone(),
                    crate::values::schema::CustomEntry { value: None },
                );
            }
            skeleton_field(&src.subject, &mut dst.subject);
            skeleton_field(&src.preheader, &mut dst.preheader);
            skeleton_field(&src.body_html, &mut dst.body_html);
            skeleton_field(&src.body_plaintext, &mut dst.body_plaintext);
        }
        out
    }
}

fn skeleton_field(src: &FieldValues, dst: &mut FieldValues) {
    for (k, e) in &src.lid {
        dst.lid.insert(
            k.clone(),
            LidEntry {
                value: None,
                url: e.url.clone(),
                anchor: e.anchor.clone(),
            },
        );
    }
    for k in src.cb_id.keys() {
        dst.cb_id.insert(k.clone(), CbIdEntry { value: None });
    }
}

#[allow(dead_code)]
fn _used_imports() {
    // Suppress unused-import warning for helpers that are only
    // referenced indirectly through trait impls in tests/cli flows.
    let _ = std::mem::size_of::<BTreeMap<String, ()>>();
    let _ = std::mem::size_of::<Error>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_clone_preserves_keys_and_clears_values() {
        let mut canonical = ValuesFile {
            version: 1,
            ..Default::default()
        };
        let cb = canonical
            .content_block
            .entry("promo".to_string())
            .or_default();
        cb.lid.insert(
            "cta".to_string(),
            LidEntry {
                value: Some("ai8kexrxcp03".into()),
                url: Some("https://example.com/cta".into()),
                anchor: None,
            },
        );
        cb.cb_id.insert(
            "shared".to_string(),
            CbIdEntry {
                value: Some("cb42".into()),
            },
        );

        let skel = canonical.skeleton_clone();
        let cb = &skel.content_block["promo"];
        assert!(cb.lid["cta"].value.is_none());
        assert_eq!(
            cb.lid["cta"].url.as_deref(),
            Some("https://example.com/cta")
        );
        assert!(cb.cb_id["shared"].value.is_none());
    }
}
