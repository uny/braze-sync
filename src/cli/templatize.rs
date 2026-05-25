//! `braze-sync templatize` — one-shot local rewrite of raw lid/cb_id
//! literals to `__BRAZESYNC.*__` placeholders. No Braze API calls.

use std::path::Path;

use anyhow::Context as _;
use clap::Args;

use crate::config::ConfigFile;
use crate::fs::{content_block_io, email_template_io};
use crate::values::templatize::{templatize_body, FieldKind};

#[derive(Args, Debug)]
pub struct TemplatizeArgs {
    /// Preview-only mode. Walks the same code path but does not touch
    /// any file on disk. Prints a summary of what would change.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: &TemplatizeArgs, cfg: &ConfigFile, config_dir: &Path) -> anyhow::Result<()> {
    let content_blocks_root = config_dir.join(&cfg.resources.content_block.path);
    let email_templates_root = config_dir.join(&cfg.resources.email_template.path);

    let mut summary = RunSummary::default();
    let mut content_block_rewrites: Vec<(std::path::PathBuf, crate::resource::ContentBlock)> =
        Vec::new();
    let mut email_template_rewrites: Vec<crate::resource::EmailTemplate> = Vec::new();

    if cfg.resources.content_block.enabled && content_blocks_root.exists() {
        let blocks = content_block_io::load_all_content_blocks(&content_blocks_root)
            .context("loading local content_blocks for templatize")?;
        for mut cb in blocks {
            let result = templatize_body(&cb.content, FieldKind::ContentBlock);
            if result.lid_rewrites + result.cb_id_rewrites == 0 {
                if cb.content.contains("__BRAZESYNC.") {
                    summary.skipped.push(format!(
                        "content_block '{}' already templated — skipping",
                        cb.name
                    ));
                }
                continue;
            }
            for w in &result.warnings {
                summary
                    .warnings
                    .push(format!("content_block '{}': {w}", cb.name));
            }
            summary.touched_resources += 1;
            summary.lid_rewrites += result.lid_rewrites;
            summary.cb_id_rewrites += result.cb_id_rewrites;
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

            let total_rewrites = subject_r.lid_rewrites
                + subject_r.cb_id_rewrites
                + body_html_r.lid_rewrites
                + body_html_r.cb_id_rewrites
                + body_plain_r.lid_rewrites
                + body_plain_r.cb_id_rewrites
                + preheader_r
                    .as_ref()
                    .map(|r| r.lid_rewrites + r.cb_id_rewrites)
                    .unwrap_or(0);
            if total_rewrites == 0 {
                if already_templated {
                    summary.skipped.push(format!(
                        "email_template '{}' already templated — skipping",
                        et.name
                    ));
                }
                continue;
            }

            for (field, warnings) in [
                ("subject", &subject_r.warnings),
                ("body_html", &body_html_r.warnings),
                ("body_plaintext", &body_plain_r.warnings),
            ] {
                for w in warnings {
                    summary
                        .warnings
                        .push(format!("email_template '{}' ({field}): {w}", et.name));
                }
            }
            if let Some(r) = preheader_r.as_ref() {
                for w in &r.warnings {
                    summary
                        .warnings
                        .push(format!("email_template '{}' (preheader): {w}", et.name));
                }
            }

            summary.touched_resources += 1;
            summary.lid_rewrites += subject_r.lid_rewrites
                + body_html_r.lid_rewrites
                + body_plain_r.lid_rewrites
                + preheader_r.as_ref().map(|r| r.lid_rewrites).unwrap_or(0);
            summary.cb_id_rewrites += subject_r.cb_id_rewrites
                + body_html_r.cb_id_rewrites
                + body_plain_r.cb_id_rewrites
                + preheader_r.as_ref().map(|r| r.cb_id_rewrites).unwrap_or(0);

            et.subject = subject_r.new_body;
            et.body_html = body_html_r.new_body;
            et.body_plaintext = body_plain_r.new_body;
            if let Some(r) = preheader_r {
                et.preheader = Some(r.new_body);
            }
            email_template_rewrites.push(et);
        }
    }

    eprintln!("templatize summary:");
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
        eprintln!(
            "(dry-run) would rewrite {} resource file(s) in place",
            content_block_rewrites.len() + email_template_rewrites.len()
        );
        return Ok(());
    }

    for (path, cb) in &content_block_rewrites {
        content_block_io::save_content_block(path.parent().unwrap_or_else(|| Path::new(".")), cb)?;
    }
    for et in &email_template_rewrites {
        email_template_io::save_email_template(&email_templates_root, et)?;
    }
    eprintln!(
        "✓ templatize: rewrote {} resource file(s)",
        content_block_rewrites.len() + email_template_rewrites.len()
    );
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
