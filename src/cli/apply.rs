//! `braze-sync apply` — push local intent to Braze.
//!
//! Phase A10 wires Catalog Schema only (field add / field delete via the
//! A9 BrazeClient methods). The other resource kinds emit a "not yet
//! implemented (Phase B)" warning when selected via `--resource`.
//!
//! ## Safety chain
//!
//! Apply is the only command that mutates remote state, so it goes
//! through a strict order of checks. Each check fails closed:
//!
//! 1. Recompute the diff (= the apply plan) using the same code path as
//!    the [`super::diff`] command. They cannot disagree about what would
//!    be applied.
//! 2. Print the plan. The header line goes to stderr so the JSON output
//!    on stdout stays parseable for CI consumers.
//! 3. If `summary.changed_count() == 0` → "No changes" → exit 0.
//! 4. If `--confirm` is **not** set → "DRY RUN" → exit 0. Zero write
//!    calls reach Braze in this branch. Verified by integration tests
//!    that mount a `method("POST")` mock with `.expect(0)`.
//! 5. If `summary.destructive_count() > 0 && !args.allow_destructive` →
//!    return [`Error::DestructiveBlocked`] which `cli::exit_code_for`
//!    maps to exit code 6 per IMPLEMENTATION.md §7.1.
//! 6. Pre-validate the plan against v0.1.0's known unsupported
//!    operations (top-level catalog Added / Removed, field-level
//!    Modified). This runs **before any API call** so we can never
//!    leave Braze half-applied.
//! 7. Apply each change. The loop uses `?`, so the first failure aborts
//!    the rest — partial-apply is bad-by-default. Each call is logged
//!    via `tracing::info!` with structured fields per §2.3 #4.

use crate::braze::BrazeClient;
use crate::config::ResolvedConfig;
use crate::diff::catalog::CatalogSchemaDiff;
use crate::diff::{DiffOp, DiffSummary, ResourceDiff};
use crate::error::Error;
use crate::format::OutputFormat;
use crate::resource::{CatalogFieldType, ResourceKind};
use anyhow::{anyhow, Context as _};
use clap::Args;
use std::path::Path;

use super::diff::compute_catalog_schema_diffs;

#[derive(Args, Debug)]
pub struct ApplyArgs {
    /// Limit apply to a specific resource kind.
    #[arg(long, value_enum)]
    pub resource: Option<ResourceKind>,

    /// When `--resource` is given, optionally restrict to a single named
    /// resource. Requires `--resource`.
    #[arg(long, requires = "resource")]
    pub name: Option<String>,

    /// Actually apply changes. Without this, runs in dry-run mode and
    /// makes zero write calls to Braze. This is the default for safety.
    #[arg(long)]
    pub confirm: bool,

    /// Permit destructive operations (field deletes, etc.). Required in
    /// addition to `--confirm` for any change that would lose data on
    /// the Braze side.
    #[arg(long)]
    pub allow_destructive: bool,

    /// Archive orphan Content Blocks / Email Templates by prefixing the
    /// remote name with `[ARCHIVED-YYYY-MM-DD]`. Catalog Schema has no
    /// orphans, so this flag is parsed but inert in v0.1.0; it lights up
    /// in Phase B alongside the orphan-tracking resource kinds.
    #[arg(long)]
    pub archive_orphans: bool,
}

pub async fn run(
    args: &ApplyArgs,
    resolved: ResolvedConfig,
    config_dir: &Path,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let catalogs_root = config_dir.join(&resolved.resources.catalog_schema.path);

    let ResolvedConfig {
        api_endpoint,
        api_key,
        rate_limit_per_minute,
        ..
    } = resolved;
    let client = BrazeClient::new(api_endpoint, api_key, rate_limit_per_minute);

    let kinds: Vec<ResourceKind> = match args.resource {
        Some(k) => vec![k],
        None => ResourceKind::all().to_vec(),
    };

    // === Step 1: Compute the plan (= recomputed diff). ===
    let mut summary = DiffSummary::default();
    for kind in kinds {
        match kind {
            ResourceKind::CatalogSchema => {
                let diffs =
                    compute_catalog_schema_diffs(&client, &catalogs_root, args.name.as_deref())
                        .await
                        .context("computing catalog_schema plan")?;
                summary.diffs.extend(diffs);
            }
            other => {
                eprintln!(
                    "⚠ {}: not yet implemented in this binary (Phase B)",
                    other.as_str()
                );
            }
        }
    }

    // === Step 2: Print the plan. ===
    // Header on stderr, body on stdout — keeps JSON output on stdout
    // parseable for CI consumers.
    let mode_label = if args.confirm {
        "Plan:"
    } else {
        "Plan (dry-run, pass --confirm to apply):"
    };
    eprintln!("{mode_label}");
    print!("{}", format.formatter().format(&summary));

    // === Step 3: Empty plan — done. ===
    if summary.changed_count() == 0 {
        eprintln!("No changes to apply.");
        return Ok(());
    }

    // === Step 4: Dry-run mode — exit before any write call. ===
    if !args.confirm {
        eprintln!("DRY RUN — pass --confirm to apply these changes.");
        return Ok(());
    }

    // === Step 5: Destructive guard. ===
    if summary.destructive_count() > 0 && !args.allow_destructive {
        return Err(Error::DestructiveBlocked.into());
    }

    // === Step 6: Pre-validate. ===
    // Surface unsupported operations BEFORE making any API calls so we
    // never leave Braze half-applied.
    check_for_unsupported_ops(&summary)?;

    // === Step 7: Apply. ===
    let mut applied = 0;
    for diff in &summary.diffs {
        if let ResourceDiff::CatalogSchema(d) = diff {
            applied += apply_catalog_schema(&client, d).await?;
        }
        // Other ResourceDiff variants are Phase B; the warnings above
        // already told the user we're skipping them.
    }

    eprintln!("✓ Applied {applied} change(s).");
    Ok(())
}

/// Walk the plan and reject anything v0.1.0 cannot actually do. Runs
/// before any API call so a partial apply is impossible.
fn check_for_unsupported_ops(summary: &DiffSummary) -> anyhow::Result<()> {
    for diff in &summary.diffs {
        if let ResourceDiff::CatalogSchema(d) = diff {
            // Top-level catalog Added/Removed: not supported in v0.1.0.
            // The §8.3 endpoint table only lists field-level POST/DELETE,
            // not whole-catalog create/delete.
            match &d.op {
                DiffOp::Added(_) => {
                    return Err(anyhow!(
                        "creating a new catalog '{}' is not supported in v0.1.0; \
                         create the catalog in the Braze dashboard first, then run \
                         `braze-sync export` to populate the local schema",
                        d.name
                    ));
                }
                DiffOp::Removed(_) => {
                    return Err(anyhow!(
                        "deleting catalog '{}' (top-level) is not supported in v0.1.0; \
                         only field-level changes can be applied",
                        d.name
                    ));
                }
                _ => {}
            }
            // Field-level Modified (type change): not supported. Auto
            // delete-then-add is data-losing on the changed field, which
            // we refuse to do silently. Document a manual workaround.
            for fd in &d.field_diffs {
                if let DiffOp::Modified { from, to } = fd {
                    return Err(anyhow!(
                        "modifying field '{}' on catalog '{}' (type {} → {}) \
                         is not supported in v0.1.0; the change would be \
                         data-losing on the field. Drop the field manually \
                         in the Braze dashboard and re-run `braze-sync apply`",
                        to.name,
                        d.name,
                        type_str(from.field_type),
                        type_str(to.field_type),
                    ));
                }
            }
        }
    }
    Ok(())
}

async fn apply_catalog_schema(
    client: &BrazeClient,
    d: &CatalogSchemaDiff,
) -> anyhow::Result<usize> {
    let mut count = 0;
    for fd in &d.field_diffs {
        match fd {
            DiffOp::Added(f) => {
                tracing::info!(
                    catalog = %d.name,
                    field = %f.name,
                    field_type = type_str(f.field_type),
                    "adding catalog field"
                );
                client.add_catalog_field(&d.name, f).await?;
                count += 1;
            }
            DiffOp::Removed(f) => {
                tracing::info!(
                    catalog = %d.name,
                    field = %f.name,
                    "deleting catalog field"
                );
                client.delete_catalog_field(&d.name, &f.name).await?;
                count += 1;
            }
            DiffOp::Modified { .. } => {
                // Already rejected by check_for_unsupported_ops above.
                // Defensive in case the validate step is ever bypassed.
                return Err(anyhow!(
                    "internal: Modified field op should have been rejected \
                     by check_for_unsupported_ops"
                ));
            }
            DiffOp::Unchanged => {}
        }
    }
    Ok(count)
}

fn type_str(t: CatalogFieldType) -> &'static str {
    // Duplicates the helper in format/table.rs and format/json.rs.
    // Refactoring to a `CatalogFieldType::as_str()` method is queued
    // for the Architecture Review Gate after Phase A.
    match t {
        CatalogFieldType::String => "string",
        CatalogFieldType::Number => "number",
        CatalogFieldType::Boolean => "boolean",
        CatalogFieldType::Time => "time",
        CatalogFieldType::Object => "object",
        CatalogFieldType::Array => "array",
    }
}
