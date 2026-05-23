//! `braze-sync export` — pull current state from Braze into local files.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::config::{is_excluded, ResolvedConfig};
use crate::fs::{catalog_io, content_block_io, custom_attribute_io, email_template_io, tag_io};
use crate::resource::{
    ContentBlock, CustomAttributeRegistry, EmailTemplate, ResourceKind, Tag, TagRegistry,
};
use crate::values::{
    extract_placeholders, refresh_content_block_values, refresh_email_template_values,
    values_file_path, ExportUpdates, ValuesFile,
};
use anyhow::Context as _;
use clap::Args;
use futures::stream::{StreamExt, TryStreamExt};
use regex_lite::Regex;
use std::collections::BTreeSet;
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

    // Load existing values file so per-resource refresh can extend it
    // in place. Absent file is fine — it will only be written back if
    // at least one resource has placeholders.
    let values_path = values_file_path(config_dir, &resolved);
    // Only the placeholder-bearing kinds consume the values file. Loading
    // it unconditionally would abort exports of unrelated kinds (tag,
    // catalog_schema, custom_attribute) whenever a pre-existing values
    // file is malformed — see preflight_values in integration.rs for the
    // matching gate.
    let needs_values = kinds
        .iter()
        .any(|k| matches!(k, ResourceKind::ContentBlock | ResourceKind::EmailTemplate));
    let mut values = if needs_values && values_path.exists() {
        ValuesFile::load(&values_path)?
    } else {
        ValuesFile {
            version: crate::values::schema::SUPPORTED_VERSION,
            ..Default::default()
        }
    };
    let mut all_updates = ExportUpdates::default();

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
                let (n, updates) = export_content_blocks(
                    &client,
                    &content_blocks_root,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::ContentBlock),
                    &mut values,
                )
                .await
                .context("exporting content_block")?;
                all_updates.merge(updates);
                eprintln!("✓ content_block: exported {n} resource(s)");
                total_written += n;
            }
            ResourceKind::EmailTemplate => {
                let (n, updates) = export_email_templates(
                    &client,
                    &email_templates_root,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::EmailTemplate),
                    &mut values,
                )
                .await
                .context("exporting email_template")?;
                all_updates.merge(updates);
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
                let n = export_custom_attributes(
                    &client,
                    &custom_attributes_path,
                    resolved.excludes_for(ResourceKind::CustomAttribute),
                )
                .await
                .context("exporting custom_attribute")?;
                eprintln!("✓ custom_attribute: exported {n} attribute(s)");
                total_written += n;
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

    // Write the values file back if any resource contributed updates.
    // Empty maps still serialize cleanly, so we guard on "did we
    // actually touch anything" to avoid creating an empty file for
    // workspaces that don't use placeholders at all.
    if all_updates.lid_updates + all_updates.cb_id_updates > 0 {
        values.save(&values_path)?;
        eprintln!(
            "✓ values: refreshed {} lid + {} cb_id entry(ies) in {}",
            all_updates.lid_updates,
            all_updates.cb_id_updates,
            values_path.display()
        );
    }
    for w in &all_updates.orphan_warnings {
        eprintln!("⚠ {w}");
    }
    for w in &all_updates.missing_entry_warnings {
        eprintln!("⚠ {w}");
    }
    for w in &all_updates.ambiguity_warnings {
        eprintln!("⚠ {w}");
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
/// When a local template with `__BRAZESYNC.*__` placeholders exists for
/// a given name, this function preserves the local body (the
/// templatized form is the source of truth) and instead refreshes the
/// per-env values entries from the remote body (RFC §2.5 "既存リソース").
async fn export_content_blocks(
    client: &BrazeClient,
    content_blocks_root: &Path,
    name_filter: Option<&str>,
    excludes: &[Regex],
    values: &mut ValuesFile,
) -> anyhow::Result<(usize, ExportUpdates)> {
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
        return Ok((0, ExportUpdates::default()));
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

    let mut updates = ExportUpdates::default();
    for remote in &blocks {
        let local_path = content_blocks_root.join(format!("{}.liquid", remote.name));
        let local = if local_path.exists() {
            Some(content_block_io::read_content_block_file(&local_path)?)
        } else {
            None
        };

        let mut to_save = remote.clone();
        if let Some(local) = local.as_ref() {
            // Strict parse — a substring check would match documentation
            // comments that happen to mention the placeholder convention
            // and silently suppress remote-body updates for non-templated
            // resources.
            if !extract_placeholders(&local.content).is_empty() {
                let report = refresh_content_block_values(local, remote, values);
                updates.merge(report);
                // Source of truth for templated bodies is local — write
                // it back unchanged so placeholders survive the round trip.
                to_save.content = local.content.clone();
            }
        }
        content_block_io::save_content_block(content_blocks_root, &to_save)?;
    }
    Ok((blocks.len(), updates))
}

/// Same list-then-fetch pattern as content blocks. Per-field placeholder
/// preservation: each of `subject`, `body_html`, `body_plaintext`,
/// `preheader` is independently checked for `__BRAZESYNC.*__` content
/// and, when present, kept from the local template instead of being
/// overwritten by the remote body.
async fn export_email_templates(
    client: &BrazeClient,
    email_templates_root: &Path,
    name_filter: Option<&str>,
    excludes: &[Regex],
    values: &mut ValuesFile,
) -> anyhow::Result<(usize, ExportUpdates)> {
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
        return Ok((0, ExportUpdates::default()));
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

    let mut updates = ExportUpdates::default();
    for remote in &templates {
        let local_dir = email_templates_root.join(&remote.name);
        let local = if local_dir.is_dir() {
            Some(email_template_io::read_email_template_dir(&local_dir)?)
        } else {
            None
        };

        let mut to_save = remote.clone();
        if let Some(local) = local.as_ref() {
            // Strict parse per field — see content_block path for the
            // same rationale (substring match is too eager).
            let subject_has = !extract_placeholders(&local.subject).is_empty();
            let body_html_has = !extract_placeholders(&local.body_html).is_empty();
            let body_plain_has = !extract_placeholders(&local.body_plaintext).is_empty();
            let preheader_has = local
                .preheader
                .as_deref()
                .is_some_and(|p| !extract_placeholders(p).is_empty());
            if subject_has || body_html_has || body_plain_has || preheader_has {
                let report = refresh_email_template_values(local, remote, values);
                updates.merge(report);
                // Preserve per-field local body when it contains a
                // placeholder. Fields without placeholders take the
                // remote value so non-templated drift still round-trips.
                if subject_has {
                    to_save.subject = local.subject.clone();
                }
                if body_html_has {
                    to_save.body_html = local.body_html.clone();
                }
                if body_plain_has {
                    to_save.body_plaintext = local.body_plaintext.clone();
                }
                if preheader_has {
                    to_save.preheader = local.preheader.clone();
                }
            }
        }
        email_template_io::save_email_template(email_templates_root, &to_save)?;
    }
    Ok((templates.len(), updates))
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

async fn export_custom_attributes(
    client: &BrazeClient,
    registry_path: &Path,
    excludes: &[Regex],
) -> anyhow::Result<usize> {
    let attrs: Vec<_> = client
        .list_custom_attributes()
        .await?
        .into_iter()
        .filter(|a| !is_excluded(&a.name, excludes))
        .collect();
    let count = attrs.len();
    let registry = CustomAttributeRegistry { attributes: attrs };
    custom_attribute_io::save_registry(registry_path, &registry)?;
    Ok(count)
}
