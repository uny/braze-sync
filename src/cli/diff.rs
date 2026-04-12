//! `braze-sync diff` — show drift between local files and Braze.
//!
//! Plan output goes to stdout (so `braze-sync diff > drift.txt` is
//! clean); warnings go to stderr. With `--fail-on-drift`, any drift
//! exits 2 so CI can gate on a clean tree.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::config::ResolvedConfig;
use crate::diff::catalog::{diff_items, diff_schema};
use crate::diff::content_block::{
    diff as diff_content_block, ContentBlockDiff, ContentBlockIdIndex,
};
use crate::diff::email_template::{
    diff as diff_email_template, EmailTemplateDiff, EmailTemplateIdIndex,
};
use crate::diff::{DiffSummary, ResourceDiff};
use crate::error::Error;
use crate::format::OutputFormat;
use crate::fs::{catalog_io, content_block_io, email_template_io};
use crate::resource::{Catalog, CatalogItems, ContentBlock, EmailTemplate, ResourceKind};
use anyhow::Context as _;
use clap::Args;
use futures::stream::{StreamExt, TryStreamExt};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{selected_kinds, FETCH_CONCURRENCY};

#[derive(Args, Debug)]
pub struct DiffArgs {
    /// Limit diff to a specific resource kind.
    #[arg(long, value_enum)]
    pub resource: Option<ResourceKind>,

    /// When `--resource` is given, optionally restrict to a single named
    /// resource. Requires `--resource`.
    #[arg(long, requires = "resource")]
    pub name: Option<String>,

    /// Exit with code 2 if any drift is detected. Intended for CI gates.
    #[arg(long)]
    pub fail_on_drift: bool,
}

pub async fn run(
    args: &DiffArgs,
    resolved: ResolvedConfig,
    config_dir: &Path,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let catalogs_root = config_dir.join(&resolved.resources.catalog_schema.path);
    let content_blocks_root = config_dir.join(&resolved.resources.content_block.path);
    let email_templates_root = config_dir.join(&resolved.resources.email_template.path);
    let client = BrazeClient::from_resolved(&resolved);
    let kinds = selected_kinds(args.resource, &resolved.resources);

    let mut summary = DiffSummary::default();
    for kind in kinds {
        match kind {
            ResourceKind::CatalogSchema => {
                let diffs =
                    compute_catalog_schema_diffs(&client, &catalogs_root, args.name.as_deref())
                        .await
                        .context("computing catalog_schema diff")?;
                summary.diffs.extend(diffs);
            }
            ResourceKind::ContentBlock => {
                let (diffs, _idx) =
                    compute_content_block_plan(&client, &content_blocks_root, args.name.as_deref())
                        .await
                        .context("computing content_block diff")?;
                summary.diffs.extend(diffs);
            }
            ResourceKind::CatalogItems => {
                let (diffs, _map) =
                    compute_catalog_items_diffs(&client, &catalogs_root, args.name.as_deref())
                        .await
                        .context("computing catalog_items diff")?;
                summary.diffs.extend(diffs);
            }
            ResourceKind::EmailTemplate => {
                let (diffs, _idx) = compute_email_template_plan(
                    &client,
                    &email_templates_root,
                    args.name.as_deref(),
                )
                .await
                .context("computing email_template diff")?;
                summary.diffs.extend(diffs);
            }
            ResourceKind::CustomAttribute => {
                tracing::debug!("custom_attribute diff not yet implemented");
            }
        }
    }

    let formatted = format.formatter().format(&summary);
    print!("{formatted}");

    if args.fail_on_drift && summary.changed_count() > 0 {
        return Err(Error::DriftDetected {
            count: summary.changed_count(),
        }
        .into());
    }

    Ok(())
}

/// Shared by `apply` so the printed plan and the executed plan cannot
/// disagree.
pub(crate) async fn compute_catalog_schema_diffs(
    client: &BrazeClient,
    catalogs_root: &Path,
    name_filter: Option<&str>,
) -> anyhow::Result<Vec<ResourceDiff>> {
    let mut local = catalog_io::load_all_schemas(catalogs_root)?;
    if let Some(name) = name_filter {
        local.retain(|c| c.name == name);
    }

    let remote: Vec<Catalog> = match name_filter {
        Some(name) => match client.get_catalog(name).await {
            Ok(c) => vec![c],
            // NotFound on a filtered fetch just means "no remote"; the
            // local entry surfaces as Added rather than as an error.
            Err(BrazeApiError::NotFound { .. }) => Vec::new(),
            Err(e) => return Err(e.into()),
        },
        None => client.list_catalogs().await?,
    };

    let local_by_name: BTreeMap<&str, &Catalog> =
        local.iter().map(|c| (c.name.as_str(), c)).collect();
    let remote_by_name: BTreeMap<&str, &Catalog> =
        remote.iter().map(|c| (c.name.as_str(), c)).collect();

    let mut all_names: BTreeSet<&str> = BTreeSet::new();
    all_names.extend(local_by_name.keys().copied());
    all_names.extend(remote_by_name.keys().copied());

    let mut diffs = Vec::new();
    for name in all_names {
        let l = local_by_name.get(name).copied();
        let r = remote_by_name.get(name).copied();
        if let Some(d) = diff_schema(l, r) {
            diffs.push(ResourceDiff::CatalogSchema(d));
        }
    }

    Ok(diffs)
}

/// Compute the per-content-block diff plan plus a name → id index for
/// the apply path. Returning both keeps the second half of `apply` from
/// having to refetch `/content_blocks/list`.
pub(crate) async fn compute_content_block_plan(
    client: &BrazeClient,
    content_blocks_root: &Path,
    name_filter: Option<&str>,
) -> anyhow::Result<(Vec<ResourceDiff>, ContentBlockIdIndex)> {
    let mut local = content_block_io::load_all_content_blocks(content_blocks_root)?;
    if let Some(name) = name_filter {
        local.retain(|c| c.name == name);
    }

    let mut summaries = client.list_content_blocks().await?;
    if let Some(name) = name_filter {
        summaries.retain(|s| s.name == name);
    }

    let id_index: ContentBlockIdIndex = summaries
        .into_iter()
        .map(|s| (s.name, s.content_block_id))
        .collect();

    let local_by_name: BTreeMap<&str, &ContentBlock> =
        local.iter().map(|c| (c.name.as_str(), c)).collect();

    // Only names present on both sides need a /info fetch. Fan them out
    // in parallel; the BrazeClient's rate limiter still governs RPM.
    let shared_names: Vec<&str> = id_index
        .keys()
        .map(String::as_str)
        .filter(|n| local_by_name.contains_key(n))
        .collect();
    let fetched: BTreeMap<String, ContentBlock> =
        futures::stream::iter(shared_names.iter().map(|name| {
            let id = id_index
                .get(*name)
                .expect("id_index built from the same summaries set");
            async move {
                client
                    .get_content_block(id)
                    .await
                    .map(|cb| (name.to_string(), cb))
                    .with_context(|| format!("fetching content block '{name}'"))
            }
        }))
        .buffer_unordered(FETCH_CONCURRENCY)
        .try_collect()
        .await?;

    let mut all_names: BTreeSet<&str> = BTreeSet::new();
    all_names.extend(local_by_name.keys().copied());
    all_names.extend(id_index.keys().map(String::as_str));

    let mut diffs = Vec::new();
    for name in all_names {
        let local_cb = local_by_name.get(name).copied();
        let remote_cb = fetched.get(name);
        let remote_present = id_index.contains_key(name);
        // Spell out only the legal triples. `fetched` carries only names
        // present on BOTH sides, and `try_collect` aborts on the first
        // /info failure, so a shared name always lands in `fetched`. The
        // previous `(Some, None, _)` arm accepted `remote_present == true`
        // and would have routed a partial-fetch shared name through
        // `Added`, silently creating a duplicate in Braze on apply.
        let diff_result = match (local_cb, remote_cb, remote_present) {
            (Some(l), Some(r), true) => diff_content_block(Some(l), Some(r)),
            (Some(l), None, false) => diff_content_block(Some(l), None),
            (None, None, true) => Some(ContentBlockDiff::orphan(name)),
            _ => unreachable!(
                "content_block diff invariant violated for '{name}': \
                 local={} remote={} remote_present={remote_present}",
                local_cb.is_some(),
                remote_cb.is_some(),
            ),
        };
        if let Some(d) = diff_result {
            diffs.push(ResourceDiff::ContentBlock(d));
        }
    }

    Ok((diffs, id_index))
}

/// Same pattern as `compute_content_block_plan` — list first, fan-out
/// /info fetches for shared names, then diff.
pub(crate) async fn compute_email_template_plan(
    client: &BrazeClient,
    email_templates_root: &Path,
    name_filter: Option<&str>,
) -> anyhow::Result<(Vec<ResourceDiff>, EmailTemplateIdIndex)> {
    let mut local = email_template_io::load_all_email_templates(email_templates_root)?;
    if let Some(name) = name_filter {
        local.retain(|t| t.name == name);
    }

    let mut summaries = client.list_email_templates().await?;
    if let Some(name) = name_filter {
        summaries.retain(|s| s.name == name);
    }

    let id_index: EmailTemplateIdIndex = summaries
        .into_iter()
        .map(|s| (s.name, s.email_template_id))
        .collect();

    let local_by_name: BTreeMap<&str, &EmailTemplate> =
        local.iter().map(|t| (t.name.as_str(), t)).collect();

    let shared_names: Vec<&str> = id_index
        .keys()
        .map(String::as_str)
        .filter(|n| local_by_name.contains_key(n))
        .collect();
    let fetched: BTreeMap<String, EmailTemplate> =
        futures::stream::iter(shared_names.iter().map(|name| {
            let id = id_index
                .get(*name)
                .expect("id_index built from the same summaries set");
            async move {
                client
                    .get_email_template(id)
                    .await
                    .map(|et| (name.to_string(), et))
                    .with_context(|| format!("fetching email template '{name}'"))
            }
        }))
        .buffer_unordered(FETCH_CONCURRENCY)
        .try_collect()
        .await?;

    let mut all_names: BTreeSet<&str> = BTreeSet::new();
    all_names.extend(local_by_name.keys().copied());
    all_names.extend(id_index.keys().map(String::as_str));

    let mut diffs = Vec::new();
    for name in all_names {
        let local_et = local_by_name.get(name).copied();
        let remote_et = fetched.get(name);
        let remote_present = id_index.contains_key(name);
        let diff_result = match (local_et, remote_et, remote_present) {
            (Some(l), Some(r), true) => diff_email_template(Some(l), Some(r)),
            (Some(l), None, false) => diff_email_template(Some(l), None),
            (None, None, true) => Some(EmailTemplateDiff::orphan(name)),
            _ => unreachable!(
                "email_template diff invariant violated for '{name}': \
                 local={} remote={} remote_present={remote_present}",
                local_et.is_some(),
                remote_et.is_some(),
            ),
        };
        if let Some(d) = diff_result {
            diffs.push(ResourceDiff::EmailTemplate(d));
        }
    }

    Ok((diffs, id_index))
}

/// Resolve catalog names from a name filter: with `--name`, returns just
/// that name; without, discovers all catalog names via `list_catalogs`.
pub(crate) async fn resolve_catalog_names(
    client: &BrazeClient,
    name_filter: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    match name_filter {
        Some(name) => Ok(vec![name.to_string()]),
        None => {
            let catalogs = client.list_catalogs().await?;
            Ok(catalogs.into_iter().map(|c| c.name).collect())
        }
    }
}

/// Compute catalog items diffs. Returns the diff results plus a map
/// from catalog_name → local `CatalogItems` (with rows) so the apply
/// path can read rows without reloading the CSV.
pub(crate) async fn compute_catalog_items_diffs(
    client: &BrazeClient,
    catalogs_root: &Path,
    name_filter: Option<&str>,
) -> anyhow::Result<(Vec<ResourceDiff>, BTreeMap<String, CatalogItems>)> {
    let local_map: BTreeMap<String, CatalogItems> = match name_filter {
        Some(name) => {
            let items_path = catalogs_root.join(name).join("items.csv");
            if items_path.is_file() {
                let ci = catalog_io::load_items(&items_path)?;
                BTreeMap::from([(ci.catalog_name.clone(), ci)])
            } else {
                BTreeMap::new()
            }
        }
        None => catalog_io::load_all_items(catalogs_root)?
            .into_iter()
            .map(|ci| (ci.catalog_name.clone(), ci))
            .collect(),
    };

    let remote_catalog_names = resolve_catalog_names(client, name_filter).await?;

    // Fetch remote items for each catalog that exists locally OR remotely.
    let mut all_names: BTreeSet<String> = BTreeSet::new();
    all_names.extend(local_map.keys().cloned());
    all_names.extend(remote_catalog_names);

    // Fan out remote item fetches in parallel. Hash rows inside the
    // closure so full row data is dropped immediately after each fetch,
    // rather than all catalogs' rows living in memory simultaneously.
    let fetched: BTreeMap<String, Option<BTreeMap<String, String>>> =
        futures::stream::iter(all_names.iter().map(|name| {
            let client = client.clone();
            let name = name.clone();
            async move {
                match client.list_catalog_items(&name).await {
                    Ok(rows) => {
                        let hashes = rows
                            .iter()
                            .map(|r| (r.id.clone(), r.content_hash()))
                            .collect();
                        Ok((name, Some(hashes)))
                    }
                    Err(BrazeApiError::NotFound { .. }) => Ok((name, None)),
                    Err(e) => Err(e),
                }
            }
        }))
        .buffer_unordered(FETCH_CONCURRENCY)
        .try_collect()
        .await?;

    let empty_hashes = BTreeMap::new();

    let mut diffs = Vec::new();
    for name in &all_names {
        let local_hashes = local_map
            .get(name)
            .map(|ci| &ci.item_hashes)
            .unwrap_or(&empty_hashes);

        let remote_hashes = fetched
            .get(name)
            .and_then(|opt| opt.as_ref())
            .unwrap_or(&empty_hashes);

        let d = diff_items(name, local_hashes, remote_hashes);
        if d.has_changes() {
            diffs.push(ResourceDiff::CatalogItems(d));
        }
    }

    Ok((diffs, local_map))
}
