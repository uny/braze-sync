//! `braze-sync export` — pull current state from Braze into local files.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::config::{is_excluded, ResolvedConfig};
use crate::fs::{catalog_io, content_block_io, custom_attribute_io, email_template_io, tag_io};
use crate::resource::{
    ContentBlock, CustomAttribute, CustomAttributeRegistry, EmailTemplate, ResourceKind, Tag,
    TagRegistry,
};
use crate::values::has_placeholders;
use crate::values::templatize::{templatize_body, FieldKind};
use anyhow::Context as _;
use clap::Args;
use futures::stream::{StreamExt, TryStreamExt};
use regex_lite::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{selected_kinds, warn_if_name_excluded, FETCH_CONCURRENCY};

#[derive(Args, Debug, Default)]
pub struct ExportArgs {
    /// Limit export to a specific resource kind. Omit to export every
    /// enabled resource kind in turn.
    #[arg(long, value_enum)]
    pub resource: Option<ResourceKind>,

    /// When `--resource` is given, optionally restrict to a single named
    /// resource. Requires `--resource`.
    #[arg(long, requires = "resource")]
    pub name: Option<String>,

    /// Rebuild the Custom Attribute registry from the queried workspace
    /// alone, dropping entries that workspace does not have.
    ///
    /// Without this, `export` refreshes the entries Braze returned and
    /// keeps the rest: an attribute materializes on first `/users/track`
    /// traffic, so its absence from one workspace is not evidence the
    /// registry entry is wrong. Pass `--prune` when you do mean "the
    /// remote is the truth, rewrite the file" — and note that it is also
    /// the recovery path for a `registry.yaml` that no longer parses,
    /// because it is the one mode that does not read the existing file.
    ///
    /// Affects `custom_attribute` only; no other resource kind replaces
    /// local state on export.
    #[arg(long)]
    pub prune: bool,
}

pub async fn run(
    args: &ExportArgs,
    resolved: ResolvedConfig,
    config_dir: &Path,
) -> anyhow::Result<()> {
    let catalogs_root = config_dir.join(&resolved.resources.catalog_schema.path);
    let content_blocks_root = config_dir.join(&resolved.resources.content_block.path);
    let email_templates_root = config_dir.join(&resolved.resources.email_template.path);
    let custom_attributes_path = config_dir.join(&resolved.resources.custom_attribute.path);
    let tags_path = config_dir.join(&resolved.resources.tag.path);
    let client = BrazeClient::from_resolved(&resolved);
    let kinds = selected_kinds(args.resource, &resolved.resources);

    // `--prune` only means something for the single-file Custom Attribute
    // registry. Silently accepting it elsewhere would let an operator
    // believe a rebuild happened.
    if args.prune && !kinds.contains(&ResourceKind::CustomAttribute) {
        eprintln!(
            "⚠ --prune affects custom_attribute only; it has no effect on \
             the selected resource kind(s)"
        );
    }

    let mut total_written: usize = 0;
    for kind in kinds {
        // `custom_attribute` ignores `--name` (registry is a single file),
        // so skipping by exclude match before dispatching wouldn't fit —
        // handle it per-arm alongside the existing --name warning.
        if !matches!(kind, ResourceKind::CustomAttribute | ResourceKind::Tag)
            && warn_if_name_excluded(kind, args.name.as_deref(), resolved.excludes_for(kind))
        {
            continue;
        }
        match kind {
            ResourceKind::CatalogSchema => {
                let n = export_catalog_schemas(
                    &client,
                    &catalogs_root,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::CatalogSchema),
                )
                .await
                .context("exporting catalog_schema")?;
                eprintln!("✓ catalog_schema: exported {n} resource(s)");
                total_written += n;
            }
            ResourceKind::ContentBlock => {
                let n = export_content_blocks(
                    &client,
                    &content_blocks_root,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::ContentBlock),
                )
                .await
                .context("exporting content_block")?;
                eprintln!("✓ content_block: exported {n} resource(s)");
                total_written += n;
            }
            ResourceKind::EmailTemplate => {
                let n = export_email_templates(
                    &client,
                    &email_templates_root,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::EmailTemplate),
                )
                .await
                .context("exporting email_template")?;
                eprintln!("✓ email_template: exported {n} resource(s)");
                total_written += n;
            }
            ResourceKind::CustomAttribute => {
                if args.name.is_some() {
                    eprintln!(
                        "⚠ custom_attribute: --name is not supported for export \
                         (the registry is a single file); exporting all attributes"
                    );
                }
                let outcome = export_custom_attributes(
                    &client,
                    &custom_attributes_path,
                    resolved.excludes_for(ResourceKind::CustomAttribute),
                    args.prune,
                )
                .await
                .context("exporting custom_attribute")?;
                eprintln!("{}", outcome.summary_line());
                total_written += outcome.written();
            }
            ResourceKind::Tag => {
                if args.name.is_some() {
                    eprintln!(
                        "⚠ tag: --name is not supported for export \
                         (the registry is a single file); exporting all tags"
                    );
                }
                let n = export_tags(
                    config_dir,
                    &resolved,
                    &tags_path,
                    resolved.excludes_for(ResourceKind::Tag),
                )
                .context("exporting tag")?;
                eprintln!("✓ tag: exported {n} tag(s)");
                total_written += n;
            }
        }
    }

    eprintln!("done: {total_written} resource(s) written");
    Ok(())
}

async fn export_catalog_schemas(
    client: &BrazeClient,
    catalogs_root: &Path,
    name_filter: Option<&str>,
    excludes: &[Regex],
) -> anyhow::Result<usize> {
    let catalogs = match name_filter {
        Some(name) => match client.get_catalog(name).await {
            Ok(c) => vec![c],
            // Missing remote is informational, not a hard error.
            Err(BrazeApiError::NotFound { .. }) => {
                eprintln!("⚠ catalog_schema: '{name}' not found in Braze");
                Vec::new()
            }
            Err(e) => return Err(e.into()),
        },
        None => client.list_catalogs().await?,
    };

    let filtered: Vec<_> = catalogs
        .into_iter()
        .filter(|c| !is_excluded(&c.name, excludes))
        .collect();
    let count = filtered.len();
    for cat in filtered {
        catalog_io::save_schema(catalogs_root, &cat)?;
    }
    Ok(count)
}

/// Lists first to discover ids, then fetches `/info` per block. With
/// `--name`, the list still happens (to translate name → id) but only
/// the matching block's body is fetched.
///
/// Remote bodies are reverse-templatized (raw lid / cb_id values
/// rewritten back to `__BRAZESYNC__`) before being written to disk,
/// so Dashboard HTML edits are captured while runtime-volatile lid /
/// cb_id values do not produce spurious drift. This applies to both
/// new resources (no local file yet) and existing ones whose local
/// template already contains placeholders. The only case where
/// templatization is skipped is when a local file exists but
/// deliberately contains no placeholders.
/// v0.15: no values-file writeback — lid / cb_id are resolved from
/// the remote body at apply/diff time.
async fn export_content_blocks(
    client: &BrazeClient,
    content_blocks_root: &Path,
    name_filter: Option<&str>,
    excludes: &[Regex],
) -> anyhow::Result<usize> {
    let summaries = client.list_content_blocks().await?;
    let targets: Vec<_> = summaries
        .into_iter()
        .filter(|s| name_filter.is_none_or(|n| s.name == n))
        .filter(|s| !is_excluded(&s.name, excludes))
        .collect();

    if targets.is_empty() {
        if let Some(name) = name_filter {
            eprintln!("⚠ content_block: '{name}' not found in Braze");
        }
        return Ok(0);
    }

    let blocks: Vec<ContentBlock> = futures::stream::iter(targets.iter().map(|s| {
        let name = s.name.as_str();
        let id = s.content_block_id.as_str();
        async move {
            client
                .get_content_block(id)
                .await
                .with_context(|| format!("fetching content block '{name}'"))
        }
    }))
    .buffer_unordered(FETCH_CONCURRENCY)
    .try_collect()
    .await?;

    for remote in &blocks {
        let local_path = content_blocks_root.join(format!("{}.liquid", remote.name));
        let local = if local_path.exists() {
            Some(content_block_io::read_content_block_file(&local_path)?)
        } else {
            None
        };
        let mut to_save = remote.clone();
        let should_templatize = local.as_ref().is_none_or(|l| has_placeholders(&l.content));
        if should_templatize {
            to_save.content = templatize_body(&remote.content, FieldKind::ContentBlock).new_body;
        }
        content_block_io::save_content_block(content_blocks_root, &to_save)?;
    }
    Ok(blocks.len())
}

/// Same list-then-fetch pattern as content blocks. Per-field reverse-
/// templatize: each of `subject`, `body_html`, `body_plaintext`,
/// `preheader` is templatized for new resources (no local dir), when
/// the corresponding local field already contains placeholders, or
/// when the local field is absent (preheader not yet saved locally).
async fn export_email_templates(
    client: &BrazeClient,
    email_templates_root: &Path,
    name_filter: Option<&str>,
    excludes: &[Regex],
) -> anyhow::Result<usize> {
    let summaries = client.list_email_templates().await?;
    let targets: Vec<_> = summaries
        .into_iter()
        .filter(|s| name_filter.is_none_or(|n| s.name == n))
        .filter(|s| !is_excluded(&s.name, excludes))
        .collect();

    if targets.is_empty() {
        if let Some(name) = name_filter {
            eprintln!("⚠ email_template: '{name}' not found in Braze");
        }
        return Ok(0);
    }

    let templates: Vec<EmailTemplate> = futures::stream::iter(targets.iter().map(|s| {
        let name = s.name.as_str();
        let id = s.email_template_id.as_str();
        async move {
            client
                .get_email_template(id)
                .await
                .with_context(|| format!("fetching email template '{name}'"))
        }
    }))
    .buffer_unordered(FETCH_CONCURRENCY)
    .try_collect()
    .await?;

    for remote in &templates {
        let local_dir = email_templates_root.join(&remote.name);
        let local = if local_dir.is_dir() {
            Some(email_template_io::read_email_template_dir(&local_dir)?)
        } else {
            None
        };
        let mut to_save = remote.clone();
        let subject_templ = local.as_ref().is_none_or(|l| has_placeholders(&l.subject));
        let body_html_templ = local
            .as_ref()
            .is_none_or(|l| has_placeholders(&l.body_html));
        let body_plain_templ = local
            .as_ref()
            .is_none_or(|l| has_placeholders(&l.body_plaintext));
        let preheader_templ = local
            .as_ref()
            .is_none_or(|l| l.preheader.as_deref().is_none_or(has_placeholders));
        if subject_templ {
            to_save.subject = templatize_body(&remote.subject, FieldKind::EmailSubject).new_body;
        }
        if body_html_templ {
            to_save.body_html =
                templatize_body(&remote.body_html, FieldKind::EmailHtmlBody).new_body;
        }
        if body_plain_templ {
            to_save.body_plaintext =
                templatize_body(&remote.body_plaintext, FieldKind::EmailPlainBody).new_body;
        }
        if preheader_templ {
            to_save.preheader = remote
                .preheader
                .as_deref()
                .map(|p| templatize_body(p, FieldKind::EmailPreheader).new_body);
        }
        email_template_io::save_email_template(email_templates_root, &to_save)?;
    }
    Ok(templates.len())
}

/// Aggregate tag names from local content_block + email_template files.
///
/// Braze does not expose a public REST API for workspace tags, so the
/// registry is derived from the local Git state instead of a remote
/// list. Operators are expected to run regular `export` first (to refresh
/// content_block / email_template files), then `export tag` to rebuild
/// the registry from the freshly-synced frontmatter. Tags found here are
/// the union of every `tags:` array on every local resource, minus
/// `tag.exclude_patterns`.
fn export_tags(
    config_dir: &Path,
    resolved: &ResolvedConfig,
    registry_path: &Path,
    excludes: &[Regex],
) -> anyhow::Result<usize> {
    let referenced = collect_local_tag_references(config_dir, resolved)?;
    let tags: Vec<Tag> = referenced
        .into_iter()
        .filter(|name| !is_excluded(name, excludes))
        .map(|name| Tag {
            name,
            description: None,
        })
        .collect();
    let count = tags.len();
    let registry = TagRegistry { tags };
    tag_io::save_registry(registry_path, &registry)?;
    Ok(count)
}

/// Walk every local resource directory the config knows about and
/// collect the union of `tags:` referenced on the resources. Used by
/// both `export` (to rebuild registry) and `apply`/`validate`
/// (to cross-check the registry against actual usage).
pub(crate) fn collect_local_tag_references(
    config_dir: &Path,
    resolved: &ResolvedConfig,
) -> anyhow::Result<BTreeSet<String>> {
    let mut tags: BTreeSet<String> = BTreeSet::new();

    if resolved.resources.content_block.enabled {
        let root = config_dir.join(&resolved.resources.content_block.path);
        let blocks = content_block_io::load_all_content_blocks(&root)
            .context("loading local content_blocks for tag aggregation")?;
        for cb in &blocks {
            for t in &cb.tags {
                tags.insert(t.clone());
            }
        }
    }

    if resolved.resources.email_template.enabled {
        let root = config_dir.join(&resolved.resources.email_template.path);
        let templates = crate::fs::email_template_io::load_all_email_templates(&root)
            .context("loading local email_templates for tag aggregation")?;
        for et in &templates {
            for t in &et.tags {
                tags.insert(t.clone());
            }
        }
    }

    Ok(tags)
}

/// What one Custom Attribute registry export did to the file.
///
/// `export` is the only writer of `registry.yaml`, and the registry is
/// the only resource stored as a single file — so it is the only kind
/// where a write can *remove* local state. The counts are reported
/// separately because a single "N attributes written" count cannot say
/// whether entries disappeared.
struct RegistryExport {
    /// Entries taken from the Braze response.
    refreshed: usize,
    /// Entries kept because the queried workspace does not have them.
    /// Zero under `--prune`.
    kept: usize,
    /// Entries dropped because the queried workspace does not have them.
    /// Non-zero only under `--prune`.
    removed: usize,
}

impl RegistryExport {
    /// Entries actually in the file afterwards.
    fn written(&self) -> usize {
        self.refreshed + self.kept
    }

    fn summary_line(&self) -> String {
        if self.removed > 0 {
            format!(
                "✓ custom_attribute: refreshed {} from Braze, removed {} \
                 registry entr{} this workspace does not have (--prune)",
                self.refreshed,
                self.removed,
                if self.removed == 1 { "y" } else { "ies" },
            )
        } else if self.kept > 0 {
            format!(
                "✓ custom_attribute: refreshed {} from Braze, kept {} \
                 registry-only entr{}",
                self.refreshed,
                self.kept,
                if self.kept == 1 { "y" } else { "ies" },
            )
        } else {
            format!(
                "✓ custom_attribute: exported {} attribute(s)",
                self.refreshed
            )
        }
    }
}

/// Refresh the Custom Attribute registry from Braze.
///
/// Unlike every other resource kind, the registry is a single file, so a
/// write here decides membership for the whole set rather than for one
/// resource. Directory-backed kinds preserve a local-only resource for
/// free — `export` simply never writes that file — and this function
/// matches that behaviour explicitly: entries the queried workspace does
/// not return are kept, not dropped.
///
/// That is not a courtesy. Braze has no create-attribute endpoint; an
/// attribute exists once `/users/track` has carried it. A registry entry
/// missing from one workspace therefore means "no traffic yet here", not
/// "wrong entry" — and a registry shared by environments that point at
/// different workspaces has such entries by construction. Dropping them
/// would also make the `type mismatch: … (run export to update)` hint
/// `diff` prints a destructive instruction.
///
/// Entries Braze *did* return are overwritten wholesale, which is what
/// keeps that hint's promise: a stale `type` (or `description`, or
/// `deprecated`) is corrected here. Only membership is preserved.
///
/// `--prune` restores the "the remote is the truth" rebuild.
async fn export_custom_attributes(
    client: &BrazeClient,
    registry_path: &Path,
    excludes: &[Regex],
    prune: bool,
) -> anyhow::Result<RegistryExport> {
    let remote: Vec<CustomAttribute> = client
        .list_custom_attributes()
        .await?
        .into_iter()
        .filter(|a| !is_excluded(&a.name, excludes))
        .collect();

    // `--prune` must not fail on an unparseable registry: it is the
    // documented way to replace one, so reading it can only be
    // best-effort there — the load feeds the "removed N" count and
    // nothing else. Without `--prune` the error propagates: merging
    // requires knowing what is in the file, and silently falling back to
    // a full replacement would restore the data loss this function
    // exists to prevent, on precisely the input (a one-character YAML
    // slip) where it is least expected.
    let local = if prune {
        custom_attribute_io::load_registry(registry_path).unwrap_or(None)
    } else {
        custom_attribute_io::load_registry(registry_path)?
    };

    let remote_names: BTreeSet<&str> = remote.iter().map(|a| a.name.as_str()).collect();

    // Collapse duplicate local names last-wins, matching
    // `diff::custom_attribute::diff`. Disagreeing would leave the two
    // commands unable to converge: whichever entry `export` kept, `diff`
    // would keep reporting the other one as drift.
    let local_only: BTreeMap<&str, &CustomAttribute> = local
        .iter()
        .flat_map(|r| r.attributes.iter())
        .filter(|a| !remote_names.contains(a.name.as_str()))
        .map(|a| (a.name.as_str(), a))
        .collect();

    let refreshed = remote.len();
    let local_only_count = local_only.len();

    let mut attributes = remote;
    if !prune {
        attributes.extend(local_only.into_values().cloned());
    }

    // `save_registry` normalizes (sorts by name), so the merged order
    // does not leak into the file.
    let registry = CustomAttributeRegistry { attributes };
    custom_attribute_io::save_registry(registry_path, &registry)?;

    Ok(RegistryExport {
        refreshed,
        kept: if prune { 0 } else { local_only_count },
        removed: if prune { local_only_count } else { 0 },
    })
}
