//! `braze-sync apply` — the only command that mutates remote state.
//!
//! Recomputes the diff via the same code path as `diff`, prints it,
//! then short-circuits unless `--confirm` is set. Pre-validates
//! unsupported/destructive ops before any API call, then walks the
//! plan and aborts on the first write failure. Braze has no
//! cross-resource transaction, so a mid-loop failure can still leave
//! earlier writes applied — the pre-validation prevents *known-bad*
//! plans from firing any writes at all, but it does not promise
//! cross-write atomicity.

use crate::braze::BrazeClient;
use crate::config::ResolvedConfig;
use crate::diff::catalog::CatalogSchemaDiff;
use crate::diff::content_block::{ContentBlockDiff, ContentBlockIdIndex};
use crate::diff::orphan;
use crate::diff::{DiffOp, DiffSummary, ResourceDiff};
use crate::error::Error;
use crate::format::OutputFormat;
use crate::resource::ResourceKind;
use anyhow::{anyhow, Context as _};
use clap::Args;
use std::path::Path;

use super::diff::{compute_catalog_schema_diffs, compute_content_block_plan};
use super::{selected_kinds, warn_unimplemented};

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
    /// remote name with `[ARCHIVED-YYYY-MM-DD]`. Inert for resource
    /// kinds that have no orphan concept (e.g. catalog schema).
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
    let content_blocks_root = config_dir.join(&resolved.resources.content_block.path);
    let client = BrazeClient::from_resolved(&resolved);
    let kinds = selected_kinds(args.resource, &resolved.resources);

    let mut summary = DiffSummary::default();
    let mut content_block_id_index: Option<ContentBlockIdIndex> = None;
    for kind in kinds {
        match kind {
            ResourceKind::CatalogSchema => {
                let diffs =
                    compute_catalog_schema_diffs(&client, &catalogs_root, args.name.as_deref())
                        .await
                        .context("computing catalog_schema plan")?;
                summary.diffs.extend(diffs);
            }
            ResourceKind::ContentBlock => {
                let (diffs, idx) =
                    compute_content_block_plan(&client, &content_blocks_root, args.name.as_deref())
                        .await
                        .context("computing content_block plan")?;
                summary.diffs.extend(diffs);
                content_block_id_index = Some(idx);
            }
            other => warn_unimplemented(other),
        }
    }

    let mode_label = if args.confirm {
        "Plan:"
    } else {
        "Plan (dry-run, pass --confirm to apply):"
    };
    eprintln!("{mode_label}");
    print!("{}", format.formatter().format(&summary));

    if summary.changed_count() == 0 {
        eprintln!("No changes to apply.");
        return Ok(());
    }

    if !args.confirm {
        eprintln!("DRY RUN — pass --confirm to apply these changes.");
        return Ok(());
    }

    if summary.destructive_count() > 0 && !args.allow_destructive {
        return Err(Error::DestructiveBlocked.into());
    }

    check_for_unsupported_ops(&summary)?;

    // One canonical archive timestamp per run, even across multiple
    // orphans. UTC (not Local) so two operators running the same archive
    // on the same wall-clock day from different timezones produce the
    // same `[ARCHIVED-YYYY-MM-DD]` prefix — determinism across a team
    // matters more than matching the operator's local calendar.
    let today = chrono::Utc::now().date_naive();

    let mut applied = 0;
    for diff in &summary.diffs {
        match diff {
            ResourceDiff::CatalogSchema(d) => {
                applied += apply_catalog_schema(&client, d).await?;
            }
            ResourceDiff::ContentBlock(d) => {
                applied += apply_content_block(
                    &client,
                    d,
                    content_block_id_index.as_ref(),
                    args.archive_orphans,
                    today,
                )
                .await?;
            }
            _ => {}
        }
    }

    eprintln!("✓ Applied {applied} change(s).");
    Ok(())
}

/// Reject ops the API can't actually perform. Runs before any write
/// call so a statically-known-bad plan cannot fire a partial apply;
/// runtime write failures can still leave earlier writes in place.
fn check_for_unsupported_ops(summary: &DiffSummary) -> anyhow::Result<()> {
    for diff in &summary.diffs {
        if let ResourceDiff::CatalogSchema(d) = diff {
            match &d.op {
                DiffOp::Added(_) => {
                    return Err(anyhow!(
                        "creating a new catalog '{}' is not supported by braze-sync; \
                         create the catalog in the Braze dashboard first, then run \
                         `braze-sync export` to populate the local schema",
                        d.name
                    ));
                }
                DiffOp::Removed(_) => {
                    return Err(anyhow!(
                        "deleting catalog '{}' (top-level) is not supported by braze-sync; \
                         only field-level changes can be applied",
                        d.name
                    ));
                }
                _ => {}
            }
            // Field type change would require delete-then-add, which is
            // data-losing on the field — refuse rather than silently drop.
            for fd in &d.field_diffs {
                if let DiffOp::Modified { from, to } = fd {
                    return Err(anyhow!(
                        "modifying field '{}' on catalog '{}' (type {} → {}) \
                         is not supported by braze-sync; the change would be \
                         data-losing on the field. Drop the field manually \
                         in the Braze dashboard and re-run `braze-sync apply`",
                        to.name,
                        d.name,
                        from.field_type.as_str(),
                        to.field_type.as_str(),
                    ));
                }
            }
        }
    }
    Ok(())
}

async fn apply_content_block(
    client: &BrazeClient,
    d: &ContentBlockDiff,
    id_index: Option<&ContentBlockIdIndex>,
    archive_orphans: bool,
    today: chrono::NaiveDate,
) -> anyhow::Result<usize> {
    // Orphans only mutate remote state when --archive-orphans is set;
    // otherwise the plan-print is the entire effect.
    if d.orphan {
        if !archive_orphans {
            return Ok(0);
        }
        let id_index = id_index.ok_or_else(|| {
            anyhow!("internal: content_block id index missing for orphan apply path")
        })?;
        let id = id_index.get(&d.name).ok_or_else(|| {
            anyhow!(
                "internal: orphan '{}' missing from id index — list/diff drift",
                d.name
            )
        })?;
        let archived = orphan::archive_name(today, &d.name);
        if archived == d.name {
            return Ok(0);
        }
        // Update endpoint requires the full body, not a partial. Safe
        // re: state — `get_content_block` defaults state to Active
        // (Braze /info has no state field) and `update_content_block`
        // omits state from the wire body, so this rename can never
        // toggle the remote state as a side effect. If either of those
        // invariants ever changes, the orphan path needs revisiting.
        let mut cb = client
            .get_content_block(id)
            .await
            .with_context(|| format!("fetching content block '{}' for archive rename", d.name))?;
        cb.name = archived;
        tracing::info!(
            content_block = %d.name,
            new_name = %cb.name,
            "archiving orphan content block"
        );
        client.update_content_block(id, &cb).await?;
        return Ok(1);
    }

    match &d.op {
        DiffOp::Added(cb) => {
            tracing::info!(content_block = %cb.name, "creating content block");
            let _ = client.create_content_block(cb).await?;
            Ok(1)
        }
        DiffOp::Modified { to, .. } => {
            let id_index = id_index.ok_or_else(|| {
                anyhow!("internal: content_block id index missing for modified apply path")
            })?;
            let id = id_index.get(&to.name).ok_or_else(|| {
                anyhow!(
                    "internal: modified content block '{}' missing from id index",
                    to.name
                )
            })?;
            tracing::info!(content_block = %to.name, "updating content block");
            client.update_content_block(id, to).await?;
            Ok(1)
        }
        // The diff layer routes remote-only blocks through the orphan
        // flag, never as a Removed op.
        DiffOp::Removed(_) => {
            unreachable!("diff layer routes content block removals through orphan")
        }
        DiffOp::Unchanged => Ok(0),
    }
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
                    field_type = f.field_type.as_str(),
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
