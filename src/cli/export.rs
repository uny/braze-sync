//! `braze-sync export` — pull current state from Braze into local files.
//!
//! v0.1.0 supports Catalog Schema. The other resource kinds produce a
//! "not yet implemented" warning when selected.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::config::ResolvedConfig;
use crate::fs::catalog_io;
use crate::resource::ResourceKind;
use anyhow::Context as _;
use clap::Args;
use std::path::Path;

use super::{selected_kinds, warn_unimplemented};

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
            other => {
                warn_unimplemented(other);
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
            // get_catalog NotFound is informational, not a hard error —
            // export of a missing name simply writes nothing.
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
