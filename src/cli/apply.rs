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
use crate::diff::catalog::{CatalogItemsDiff, CatalogSchemaDiff};
use crate::diff::content_block::{ContentBlockDiff, ContentBlockIdIndex};
use crate::diff::email_template::{EmailTemplateDiff, EmailTemplateIdIndex};
use crate::diff::orphan;
use crate::diff::{DiffOp, DiffSummary, ResourceDiff};
use crate::error::Error;
use crate::format::OutputFormat;
use crate::resource::{CatalogItems, ResourceKind};
use anyhow::{anyhow, Context as _};
use clap::Args;
use futures::stream::{StreamExt, TryStreamExt};
use std::collections::BTreeMap;
use std::path::Path;

use super::diff::{
    compute_catalog_items_diffs, compute_catalog_schema_diffs, compute_content_block_plan,
    compute_email_template_plan,
};
use super::selected_kinds;

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
    let email_templates_root = config_dir.join(&resolved.resources.email_template.path);
    let client = BrazeClient::from_resolved(&resolved);
    let kinds = selected_kinds(args.resource, &resolved.resources);

    let mut summary = DiffSummary::default();
    let mut content_block_id_index: Option<ContentBlockIdIndex> = None;
    let mut email_template_id_index: Option<EmailTemplateIdIndex> = None;
    let mut catalog_items_local: Option<BTreeMap<String, CatalogItems>> = None;
    for kind in kinds {
        match kind {
            ResourceKind::CatalogSchema => {
                let diffs =
                    compute_catalog_schema_diffs(&client, &catalogs_root, args.name.as_deref())
                        .await
                        .context("computing catalog_schema plan")?;
                summary.diffs.extend(diffs);
            }
            ResourceKind::CatalogItems => {
                let (diffs, local_map) =
                    compute_catalog_items_diffs(
                        &client,
                        &catalogs_root,
                        args.name.as_deref(),
                        true,
                    )
                    .await
                    .context("computing catalog_items plan")?;
                summary.diffs.extend(diffs);
                catalog_items_local = Some(local_map);
            }
            ResourceKind::ContentBlock => {
                let (diffs, idx) =
                    compute_content_block_plan(&client, &content_blocks_root, args.name.as_deref())
                        .await
                        .context("computing content_block plan")?;
                summary.diffs.extend(diffs);
                content_block_id_index = Some(idx);
            }
            ResourceKind::EmailTemplate => {
                let (diffs, idx) = compute_email_template_plan(
                    &client,
                    &email_templates_root,
                    args.name.as_deref(),
                )
                .await
                .context("computing email_template plan")?;
                summary.diffs.extend(diffs);
                email_template_id_index = Some(idx);
            }
            ResourceKind::CustomAttribute => {
                tracing::debug!("custom_attribute apply not yet implemented");
            }
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

    let parallel_batches = resolved.resources.catalog_items.parallel_batches;
    let mut applied = 0;
    for diff in &summary.diffs {
        match diff {
            ResourceDiff::CatalogSchema(d) => {
                applied += apply_catalog_schema(&client, d).await?;
            }
            ResourceDiff::CatalogItems(d) => {
                let local_map = catalog_items_local.as_ref().ok_or_else(|| {
                    anyhow!("internal: catalog_items_local not populated before apply")
                })?;
                let local = local_map.get(&d.catalog_name).ok_or_else(|| {
                    anyhow!(
                        "internal: local items for catalog '{}' missing from items map",
                        d.catalog_name
                    )
                })?;
                applied +=
                    apply_catalog_items(&client, d, local, parallel_batches).await?;
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
            ResourceDiff::EmailTemplate(d) => {
                applied += apply_email_template(
                    &client,
                    d,
                    email_template_id_index.as_ref(),
                    args.archive_orphans,
                    today,
                )
                .await?;
            }
            ResourceDiff::CustomAttribute(_) => {}
        }
    }

    eprintln!("✓ Applied {applied} change(s).");
    Ok(())
}

/// Reject ops the API can't actually perform. Runs before any write
/// call so a statically-known-bad plan cannot fire a partial apply;
/// runtime write failures can still leave earlier writes in place.
///
/// ContentBlock diffs are deliberately not inspected here: every shape
/// the diff layer can produce (`Added` → create, `Modified` → update,
/// `orphan` → archive-or-noop) maps to a supported API call, so there
/// is nothing to statically reject. If a future diff shape is added
/// (e.g. content-type change with no in-place update), re-evaluate.
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

/// Batch size for catalog items upsert/delete (Braze limit).
const ITEMS_BATCH_SIZE: usize = 50;

fn items_progress_bar(total: u64, label: &str, color: &str) -> indicatif::ProgressBar {
    let pb = indicatif::ProgressBar::new(total);
    pb.set_style(
        indicatif::ProgressStyle::default_bar()
            .template(&format!(
                "{{spinner:.{color}}} [{{elapsed_precise}}] {{bar:40}} {{pos}}/{{len}} {label}"
            ))
            .unwrap(),
    );
    pb
}

/// Run a batch operation over `items` in chunks of [`ITEMS_BATCH_SIZE`],
/// fanning out up to `concurrency` in-flight requests. Returns the total
/// number of items processed.
async fn run_batched<T, F, Fut>(
    items: &[T],
    concurrency: usize,
    pb: &indicatif::ProgressBar,
    batch_fn: F,
) -> anyhow::Result<usize>
where
    T: Clone + Send + Sync + 'static,
    F: Fn(Vec<T>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let count = futures::stream::iter(items.chunks(ITEMS_BATCH_SIZE).map(|chunk| {
        let batch = chunk.to_vec();
        let batch_len = batch.len();
        let fut = batch_fn(batch);
        let pb = pb.clone();
        async move {
            fut.await?;
            pb.inc(batch_len as u64);
            Ok::<usize, anyhow::Error>(batch_len)
        }
    }))
    .buffer_unordered(concurrency)
    .try_fold(0usize, |acc, n| async move { Ok(acc + n) })
    .await?;

    pb.finish_and_clear();
    Ok(count)
}

async fn apply_catalog_items(
    client: &BrazeClient,
    d: &CatalogItemsDiff,
    local: &CatalogItems,
    parallel_batches: u32,
) -> anyhow::Result<usize> {
    if !d.has_changes() {
        return Ok(0);
    }

    let catalog_name = &d.catalog_name;
    let concurrency = (parallel_batches as usize).max(1);

    let upsert_ids: Vec<&str> = d
        .added_ids
        .iter()
        .chain(d.modified_ids.iter())
        .map(String::as_str)
        .collect();

    let mut upsert_count: usize = 0;
    if !upsert_ids.is_empty() {
        let rows = local.rows.as_ref().ok_or_else(|| {
            anyhow!(
                "internal: local items for catalog '{}' have no materialized rows",
                catalog_name
            )
        })?;
        let row_by_id: std::collections::HashMap<&str, &crate::resource::CatalogItemRow> =
            rows.iter().map(|r| (r.id.as_str(), r)).collect();

        let upsert_rows: Vec<crate::resource::CatalogItemRow> = upsert_ids
            .iter()
            .map(|&id| {
                (*row_by_id
                    .get(id)
                    .expect("item in diff but missing from local rows"))
                .clone()
            })
            .collect();

        let pb = items_progress_bar(upsert_rows.len() as u64, "items", "green");

        upsert_count = run_batched(&upsert_rows, concurrency, &pb, |batch| {
            let client = client.clone();
            let catalog_name = catalog_name.clone();
            async move {
                tracing::info!(
                    catalog = %catalog_name,
                    batch_size = batch.len(),
                    "upserting catalog items batch"
                );
                client
                    .upsert_catalog_items(&catalog_name, &batch)
                    .await
                    .with_context(|| {
                        format!("upserting items batch for catalog '{catalog_name}'")
                    })
            }
        })
        .await?;
    }

    let mut delete_count: usize = 0;
    if !d.removed_ids.is_empty() {
        let pb = items_progress_bar(d.removed_ids.len() as u64, "deletes", "red");

        delete_count = run_batched(&d.removed_ids, concurrency, &pb, |batch| {
            let client = client.clone();
            let catalog_name = catalog_name.clone();
            async move {
                tracing::info!(
                    catalog = %catalog_name,
                    batch_size = batch.len(),
                    "deleting catalog items batch"
                );
                client
                    .delete_catalog_items(&catalog_name, &batch)
                    .await
                    .with_context(|| {
                        format!("deleting items batch for catalog '{catalog_name}'")
                    })
            }
        })
        .await?;
    }

    Ok(upsert_count + delete_count)
}

async fn apply_email_template(
    client: &BrazeClient,
    d: &EmailTemplateDiff,
    id_index: Option<&EmailTemplateIdIndex>,
    archive_orphans: bool,
    today: chrono::NaiveDate,
) -> anyhow::Result<usize> {
    if d.orphan {
        if !archive_orphans {
            return Ok(0);
        }
        let id_index = id_index.ok_or_else(|| {
            anyhow!("internal: email_template id index missing for orphan apply path")
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
        let mut et = client
            .get_email_template(id)
            .await
            .with_context(|| format!("fetching email template '{}' for archive rename", d.name))?;
        et.name = archived;
        tracing::info!(
            email_template = %d.name,
            new_name = %et.name,
            "archiving orphan email template"
        );
        client.update_email_template(id, &et).await?;
        return Ok(1);
    }

    match &d.op {
        DiffOp::Added(et) => {
            tracing::info!(email_template = %et.name, "creating email template");
            let _ = client.create_email_template(et).await?;
            Ok(1)
        }
        DiffOp::Modified { to, .. } => {
            let id_index = id_index.ok_or_else(|| {
                anyhow!("internal: email_template id index missing for modified apply path")
            })?;
            let id = id_index.get(&to.name).ok_or_else(|| {
                anyhow!(
                    "internal: modified email template '{}' missing from id index",
                    to.name
                )
            })?;
            tracing::info!(email_template = %to.name, "updating email template");
            client.update_email_template(id, to).await?;
            Ok(1)
        }
        DiffOp::Removed(_) => {
            unreachable!("diff layer routes email template removals through orphan")
        }
        DiffOp::Unchanged => Ok(0),
    }
}
