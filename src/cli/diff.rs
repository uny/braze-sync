//! `braze-sync diff` — show drift between local files and Braze.
//!
//! v0.1.0 supports Catalog Schema only. The other resource kinds emit a
//! "not yet implemented" warning.
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
use crate::diff::{DiffSummary, ResourceDiff};
use crate::error::Error;
use crate::format::OutputFormat;
use crate::fs::catalog_io;
use crate::resource::{Catalog, ResourceKind};
use anyhow::Context as _;
use clap::Args;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{selected_kinds, warn_unimplemented};

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
