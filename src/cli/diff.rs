//! `braze-sync diff` — show drift between local files and Braze.
//!
//! v0.2.0 supports Catalog Schema and Content Block. The other resource
//! kinds emit a "not yet implemented" warning.
//!
//! Output goes to **stdout** so scripts can `braze-sync diff > drift.txt`
//! cleanly. Status warnings go to stderr. The formatter is chosen by the
//! global `--format` flag (default: `table`).
//!
//! With `--fail-on-drift`, a non-empty `summary.changed_count()` makes the
//! command exit with code 2 (`Error::DriftDetected`) so CI pipelines can
//! gate merges on a clean tree.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::config::ResolvedConfig;
use crate::diff::catalog::diff_schema;
use crate::diff::content_block::diff as diff_content_block;
use crate::diff::{DiffSummary, ResourceDiff};
use crate::error::Error;
use crate::format::OutputFormat;
use crate::fs::{catalog_io, content_block_io};
use crate::resource::{Catalog, ContentBlock, ResourceKind};
use anyhow::Context as _;
use clap::Args;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{selected_kinds, warn_unimplemented};

/// Name → Braze content_block_id index returned alongside the content
/// block diff plan. Apply uses it to translate per-name diff entries
/// into the `content_block_id` the update endpoint requires.
pub(crate) type ContentBlockIdIndex = BTreeMap<String, String>;

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

    // Render formatted output to stdout. Formatters return strings ending
    // with one newline, so a plain `print!` is enough.
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

/// Compute the per-catalog-schema diff between local files and Braze.
///
/// `pub(crate)` so [`crate::cli::apply`] can reuse the exact same plan
/// computation that the diff command displays — apply is "compute the
/// plan and then execute it", so they MUST agree on what the plan is.
pub(crate) async fn compute_catalog_schema_diffs(
    client: &BrazeClient,
    catalogs_root: &Path,
    name_filter: Option<&str>,
) -> anyhow::Result<Vec<ResourceDiff>> {
    // Local: load all on-disk schemas, then optionally restrict by name.
    let mut local = catalog_io::load_all_schemas(catalogs_root)?;
    if let Some(name) = name_filter {
        local.retain(|c| c.name == name);
    }

    // Remote: when filtering, hit the cheaper get-by-name endpoint.
    let remote: Vec<Catalog> = match name_filter {
        Some(name) => match client.get_catalog(name).await {
            Ok(c) => vec![c],
            // NotFound on the filtered get-by-name call means the remote
            // simply doesn't have it; treat as "no remote" so the local
            // shows up as Added (a normal diff entry, not an error).
            Err(BrazeApiError::NotFound { .. }) => Vec::new(),
            Err(e) => return Err(e.into()),
        },
        None => client.list_catalogs().await?,
    };

    // Index by name and compute diffs over the union, in deterministic
    // (sorted) order.
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
///
/// API call shape (worst case `--name` not set):
/// 1. one `GET /content_blocks/list`
/// 2. for each name present in BOTH local and remote, one
///    `GET /content_blocks/info?content_block_id=...` to fetch the
///    body. Local-only names produce no API calls (we already have the
///    body); remote-only names also produce no API calls (orphans only
///    need their identity to report).
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

    // Build the name → id index up front; apply uses it later.
    let id_index: ContentBlockIdIndex = summaries
        .iter()
        .map(|s| (s.name.clone(), s.content_block_id.clone()))
        .collect();

    let local_by_name: BTreeMap<&str, &ContentBlock> =
        local.iter().map(|c| (c.name.as_str(), c)).collect();
    let remote_names: BTreeSet<&str> = summaries.iter().map(|s| s.name.as_str()).collect();

    // Only fetch /info for names that exist on BOTH sides — those are
    // the only candidates for a body comparison. Local-only goes to
    // Added (no fetch needed), remote-only goes to orphan (no body
    // needed).
    let mut all_names: BTreeSet<&str> = BTreeSet::new();
    all_names.extend(local_by_name.keys().copied());
    all_names.extend(remote_names.iter().copied());

    let mut diffs = Vec::new();
    for name in all_names {
        let local_cb = local_by_name.get(name).copied();
        let remote_cb: Option<ContentBlock> = if remote_names.contains(name) && local_cb.is_some() {
            let id = id_index
                .get(name)
                .expect("id_index built from the same summaries set");
            Some(client.get_content_block(id).await?)
        } else {
            None
        };
        let remote_ref = remote_cb.as_ref();
        // Three cases:
        // 1. local + remote present → call diff with both
        // 2. local only → call diff with local only (Added)
        // 3. remote only → orphan; manufacture a stub remote with just
        //    the name so the diff function can flag it
        let diff_result = match (local_cb, remote_ref, remote_names.contains(name)) {
            (Some(l), Some(r), _) => diff_content_block(Some(l), Some(r)),
            (Some(l), None, _) => diff_content_block(Some(l), None),
            (None, _, true) => {
                let stub = ContentBlock {
                    name: name.to_string(),
                    description: None,
                    content: String::new(),
                    tags: vec![],
                    state: crate::resource::ContentBlockState::Active,
                };
                diff_content_block(None, Some(&stub))
            }
            (None, _, false) => None,
        };
        if let Some(d) = diff_result {
            diffs.push(ResourceDiff::ContentBlock(d));
        }
    }

    Ok((diffs, id_index))
}
