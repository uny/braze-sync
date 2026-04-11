//! `braze-sync diff` — show drift between local files and Braze.
//!
//! Plan output goes to stdout (so `braze-sync diff > drift.txt` is
//! clean); warnings go to stderr. With `--fail-on-drift`, any drift
//! exits 2 so CI can gate on a clean tree.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::config::ResolvedConfig;
use crate::diff::catalog::diff_schema;
use crate::diff::content_block::{
    diff as diff_content_block, ContentBlockDiff, ContentBlockIdIndex,
};
use crate::diff::{DiffSummary, ResourceDiff};
use crate::error::Error;
use crate::format::OutputFormat;
use crate::fs::{catalog_io, content_block_io};
use crate::resource::{Catalog, ContentBlock, ResourceKind};
use anyhow::Context as _;
use clap::Args;
use futures::stream::{StreamExt, TryStreamExt};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{selected_kinds, warn_unimplemented, FETCH_CONCURRENCY};

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
            other => {
                warn_unimplemented(other);
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
