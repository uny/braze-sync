//! `braze-sync export` — pull current state from Braze into local files.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::config::ResolvedConfig;
use crate::fs::{catalog_io, content_block_io, email_template_io};
use crate::resource::ResourceKind;
use anyhow::Context as _;
use clap::Args;
use futures::stream::{StreamExt, TryStreamExt};
use std::path::Path;

use super::diff::resolve_catalog_names;
use super::{selected_kinds, FETCH_CONCURRENCY};

#[derive(Args, Debug)]
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
    let client = BrazeClient::from_resolved(&resolved);
    let kinds = selected_kinds(args.resource, &resolved.resources);

    let mut total_written: usize = 0;
    for kind in kinds {
        match kind {
            ResourceKind::CatalogSchema => {
                let n = export_catalog_schemas(&client, &catalogs_root, args.name.as_deref())
                    .await
                    .context("exporting catalog_schema")?;
                eprintln!("✓ catalog_schema: exported {n} resource(s)");
                total_written += n;
            }
            ResourceKind::CatalogItems => {
                let n = export_catalog_items(&client, &catalogs_root, args.name.as_deref())
                    .await
                    .context("exporting catalog_items")?;
                eprintln!("✓ catalog_items: exported {n} catalog(s)");
                total_written += n;
            }
            ResourceKind::ContentBlock => {
                let n = export_content_blocks(&client, &content_blocks_root, args.name.as_deref())
                    .await
                    .context("exporting content_block")?;
                eprintln!("✓ content_block: exported {n} resource(s)");
                total_written += n;
            }
            ResourceKind::EmailTemplate => {
                let n =
                    export_email_templates(&client, &email_templates_root, args.name.as_deref())
                        .await
                        .context("exporting email_template")?;
                eprintln!("✓ email_template: exported {n} resource(s)");
                total_written += n;
            }
            ResourceKind::CustomAttribute => {
                tracing::debug!("custom_attribute export not yet implemented");
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

    let count = catalogs.len();
    for cat in catalogs {
        catalog_io::save_schema(catalogs_root, &cat)?;
    }
    Ok(count)
}

/// Lists first to discover ids, then fetches `/info` per block. With
/// `--name`, the list still happens (to translate name → id) but only
/// the matching block's body is fetched.
async fn export_content_blocks(
    client: &BrazeClient,
    content_blocks_root: &Path,
    name_filter: Option<&str>,
) -> anyhow::Result<usize> {
    let summaries = client.list_content_blocks().await?;
    let targets: Vec<_> = match name_filter {
        Some(name) => summaries.into_iter().filter(|s| s.name == name).collect(),
        None => summaries,
    };

    if targets.is_empty() {
        if let Some(name) = name_filter {
            eprintln!("⚠ content_block: '{name}' not found in Braze");
        }
        return Ok(0);
    }

    let blocks: Vec<crate::resource::ContentBlock> =
        futures::stream::iter(targets.iter().map(|s| {
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

    for cb in &blocks {
        content_block_io::save_content_block(content_blocks_root, cb)?;
    }
    Ok(blocks.len())
}

/// Same list-then-fetch pattern as content blocks.
async fn export_email_templates(
    client: &BrazeClient,
    email_templates_root: &Path,
    name_filter: Option<&str>,
) -> anyhow::Result<usize> {
    let summaries = client.list_email_templates().await?;
    let targets: Vec<_> = match name_filter {
        Some(name) => summaries.into_iter().filter(|s| s.name == name).collect(),
        None => summaries,
    };

    if targets.is_empty() {
        if let Some(name) = name_filter {
            eprintln!("⚠ email_template: '{name}' not found in Braze");
        }
        return Ok(0);
    }

    let templates: Vec<crate::resource::EmailTemplate> =
        futures::stream::iter(targets.iter().map(|s| {
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

    for et in &templates {
        email_template_io::save_email_template(email_templates_root, et)?;
    }
    Ok(templates.len())
}

/// Export catalog items. Discovers catalogs via `list_catalogs` (to get
/// names), then fetches items per catalog in parallel. With `--name`,
/// fetches items for that single catalog only.
async fn export_catalog_items(
    client: &BrazeClient,
    catalogs_root: &Path,
    name_filter: Option<&str>,
) -> anyhow::Result<usize> {
    let catalog_names = resolve_catalog_names(client, name_filter).await?;

    let mut stream = futures::stream::iter(catalog_names.into_iter().map(|name| {
        let client = client.clone();
        async move {
            match client.list_catalog_items(&name).await {
                Ok(items) => Ok(Some((name, items))),
                Err(BrazeApiError::NotFound { .. }) => {
                    eprintln!("⚠ catalog_items: catalog '{name}' not found in Braze");
                    Ok(None)
                }
                Err(e) => Err(e),
            }
        }
    }))
    .buffer_unordered(FETCH_CONCURRENCY);

    let mut count = 0;
    while let Some(result) = stream.next().await {
        if let Some((name, items)) = result? {
            catalog_io::save_items(catalogs_root, &name, &items)?;
            count += 1;
        }
    }
    Ok(count)
}
