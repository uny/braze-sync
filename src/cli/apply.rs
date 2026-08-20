//! `braze-sync apply` — the only command that mutates remote state.
//!
//! Recomputes the diff via the same code path as `diff`, prints it,
//! then short-circuits unless `--confirm` is set. Pre-validates
//! unsupported/destructive ops before any API call, then walks the
//! plan and aborts on the first write failure. Braze has no
//! cross-resource transaction, so a mid-loop failure can still leave
//! earlier writes applied — the pre-validation prevents *known-bad*
//! plans from firing any writes at all, but it does not promise
//! cross-write atomicity. Because that partial state is real and
//! nothing downstream flags it, an aborted run enumerates what it
//! applied, what failed, and what it never attempted (see
//! `report_partial_apply`), so the applied set is reported rather than
//! inferred from a second `diff`.

use crate::braze::BrazeClient;
use crate::config::{ApplyOrder, ResolvedConfig};
use crate::diff::catalog::CatalogSchemaDiff;
use crate::diff::content_block::{ContentBlockDiff, ContentBlockIdIndex};
use crate::diff::content_block_order::reorder_content_block_diffs_by_dependency;
use crate::diff::custom_attribute::CustomAttributeOp;
use crate::diff::email_template::{EmailTemplateDiff, EmailTemplateIdIndex};
use crate::diff::plan::{self, PlanFile};
use crate::diff::{DiffOp, DiffSummary, ResourceDiff};
use crate::error::Error;
use crate::format::{OutputFormat, TableFormatter};
use crate::resource::{CatalogField, ResourceKind};
use crate::values::{format_fallback_reports, gated_fallback_count, FallbackReport};
use anyhow::{anyhow, Context as _};
use clap::Args;
use std::path::{Path, PathBuf};

use super::diff::{
    compute_catalog_schema_diffs, compute_content_block_plan, compute_custom_attribute_diffs,
    compute_email_template_plan, compute_tag_diffs,
};
use super::{selected_kinds, warn_if_name_excluded};

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

    /// Permit applying when the fallback gate fired: some field had an
    /// unmatched template placeholder and an unconsumed remote lid
    /// value at the same time (see the Notice block). Required in
    /// addition to `--confirm`.
    #[arg(long)]
    pub allow_fallback: bool,

    /// Load a plan file produced by `diff --plan-out=<path>` and refuse
    /// to apply if the freshly-computed plan does not match. Exits with
    /// code 7 on mismatch.
    #[arg(long, value_name = "PATH")]
    pub plan: Option<PathBuf>,
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
    let custom_attributes_path = config_dir.join(&resolved.resources.custom_attribute.path);
    let tags_path = config_dir.join(&resolved.resources.tag.path);
    let client = BrazeClient::from_resolved(&resolved);
    let kinds = selected_kinds(args.resource, &resolved.resources);

    // Reject scope/env mismatch before any Braze API call.
    let saved_plan = if let Some(path) = &args.plan {
        let plan = PlanFile::read_from(path)
            .with_context(|| format!("reading plan file {}", path.display()))?;
        check_plan_scope(&plan, &resolved.environment_name, args)?;
        warn_on_plan_metadata(&plan);
        Some(plan)
    } else {
        None
    };

    let mut summary = DiffSummary::default();
    let mut content_block_id_index: Option<ContentBlockIdIndex> = None;
    let mut email_template_id_index: Option<EmailTemplateIdIndex> = None;
    let mut fallback_reports: Vec<FallbackReport> = Vec::new();
    for kind in &kinds {
        let kind = *kind;
        if warn_if_name_excluded(kind, args.name.as_deref(), resolved.excludes_for(kind)) {
            continue;
        }
        match kind {
            ResourceKind::CatalogSchema => {
                let diffs = compute_catalog_schema_diffs(
                    &client,
                    &catalogs_root,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::CatalogSchema),
                )
                .await
                .context("computing catalog_schema plan")?;
                summary.diffs.extend(diffs);
            }
            ResourceKind::ContentBlock => {
                let (diffs, idx, reports) = compute_content_block_plan(
                    &client,
                    &content_blocks_root,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::ContentBlock),
                )
                .await
                .context("computing content_block plan")?;
                summary.diffs.extend(diffs);
                fallback_reports.extend(reports);
                content_block_id_index = Some(idx);
            }
            ResourceKind::EmailTemplate => {
                let (diffs, idx, reports) = compute_email_template_plan(
                    &client,
                    &email_templates_root,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::EmailTemplate),
                )
                .await
                .context("computing email_template plan")?;
                summary.diffs.extend(diffs);
                fallback_reports.extend(reports);
                email_template_id_index = Some(idx);
            }
            ResourceKind::CustomAttribute => {
                let diffs = compute_custom_attribute_diffs(
                    &client,
                    &custom_attributes_path,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::CustomAttribute),
                )
                .await
                .context("computing custom_attribute plan")?;
                summary.diffs.extend(diffs);
            }
            ResourceKind::Tag => {
                let diffs = compute_tag_diffs(
                    config_dir,
                    &resolved,
                    &tags_path,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::Tag),
                )
                .context("computing tag plan")?;
                summary.diffs.extend(diffs);
            }
        }
    }

    // Tag pre-flight: refuse to fire any mutation if a referenced tag is
    // missing from the registry. Without this, Braze rejects the first
    // tag-bearing create/update with HTTP 400 and the rest of the plan
    // halts mid-pipeline. Runs even when --kind is restricted to a non-tag
    // kind so tag drift cannot sneak past a per-kind apply.
    enforce_tag_preflight(config_dir, &resolved, &summary)?;

    // Targets must precede referrers because Braze validates
    // `{{content_blocks.${other}}}` includes at create time and rejects
    // forward references with an opaque HTTP 500. Done before
    // plan-print so the dry-run preview reflects actual write order.
    if matches!(
        resolved.resources.content_block.apply_order,
        ApplyOrder::Dependency
    ) {
        summary.diffs = reorder_content_block_diffs_by_dependency(std::mem::take(
            &mut summary.diffs,
        ))
        .map_err(|cycle| {
            anyhow!(
                "content_block dependency cycle detected\n       \
                         {cycle}\n       \
                         resolve by removing one of these references and try again"
            )
        })?;
    }

    let mode_label = if args.confirm {
        "Plan:"
    } else {
        "Plan (dry-run, pass --confirm to apply):"
    };
    eprintln!("{mode_label}");
    print!(
        "{}",
        format.formatter(TableFormatter::default()).format(&summary)
    );
    let fallback_block = format_fallback_reports(&fallback_reports);
    if !fallback_block.is_empty() {
        eprint!("{fallback_block}");
    }

    // Print the fresh plan first so the operator sees it even on mismatch.
    if let Some(saved) = &saved_plan {
        check_plan_ops(saved, &summary)?;
        eprintln!("✓ Plan matches saved plan ({} op(s)).", saved.ops.len());
    }

    if summary.actionable_count() == 0 {
        if summary.changed_count() > 0 {
            eprintln!(
                "No actionable changes to apply \
                 (informational drift above can be reconciled with `export`)."
            );
        } else {
            eprintln!("No changes to apply.");
        }
        return Ok(());
    }

    if !args.confirm {
        eprintln!("DRY RUN — pass --confirm to apply these changes.");
        return Ok(());
    }

    if summary.destructive_count() > 0 && !args.allow_destructive {
        return Err(Error::DestructiveBlocked.into());
    }

    let gated_count = gated_fallback_count(&fallback_reports);
    if gated_count > 0 && !args.allow_fallback {
        return Err(Error::FallbackGated { count: gated_count }.into());
    }

    check_for_unsupported_ops(&summary)?;

    let units = collect_write_units(&summary);
    let mut applied = 0;
    let mut done: Vec<String> = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        let label = unit.label();
        match unit
            .execute(
                &client,
                content_block_id_index.as_ref(),
                email_template_id_index.as_ref(),
            )
            .await
        {
            Ok(n) => {
                applied += n;
                eprintln!("  ✓ {label}");
                done.push(label);
            }
            Err(err) => {
                // Braze has no cross-resource transaction: everything in
                // `done` is already live. Enumerate it here rather than
                // making the operator re-derive it from a second `diff`.
                report_partial_apply(&done, &label, &err, &units[i + 1..], args.plan.is_some());
                return Err(err);
            }
        }
    }

    eprintln!("✓ Applied {applied} change(s).");
    Ok(())
}

/// One write, in the order `apply` walks the plan.
///
/// Materialising the walk as a list (instead of writing straight from
/// the diff loop) is what lets a failure name the resources that were
/// never attempted: they are simply the tail of this vector.
///
/// **Invariant: one unit is exactly one Braze API call.** The failure
/// report enumerates units, so a unit that fanned out into several
/// calls could land some of them and still be reported wholesale as
/// "failed" — leaving writes that are live in Braze named nowhere. That
/// is why a catalog's per-field writes are separate units
/// ([`Self::CatalogField`]) rather than one catalog-sized unit.
enum WriteUnit<'a> {
    /// Whole-catalog create/delete. `POST /catalogs` carries the full
    /// schema and `DELETE /catalogs/{name}` drops it in one call, so
    /// either is a single unit. Field-level edits are never routed
    /// here — see [`Self::CatalogField`].
    Catalog(&'a CatalogSchemaDiff),
    /// One field add/delete on an existing catalog: one API call.
    CatalogField {
        catalog: &'a str,
        op: &'a DiffOp<CatalogField>,
    },
    ContentBlock(&'a ContentBlockDiff),
    EmailTemplate(&'a EmailTemplateDiff),
    /// Custom attribute deprecation toggles are one API call per
    /// direction, so a whole direction is a single unit.
    CustomAttributeBlocklist {
        names: Vec<&'a str>,
        blocklisted: bool,
    },
}

impl WriteUnit<'_> {
    fn label(&self) -> String {
        match self {
            Self::Catalog(d) => format!("catalog_schema '{}'", d.name),
            Self::CatalogField { catalog, op } => match op {
                DiffOp::Added(f) => format!("catalog_schema '{catalog}' field '{}' (add)", f.name),
                DiffOp::Removed(f) => {
                    format!("catalog_schema '{catalog}' field '{}' (delete)", f.name)
                }
                DiffOp::Modified { to, .. } => {
                    format!("catalog_schema '{catalog}' field '{}' (modify)", to.name)
                }
                DiffOp::Unchanged => format!("catalog_schema '{catalog}' field (unchanged)"),
            },
            Self::ContentBlock(d) => format!("content_block '{}'", d.name),
            Self::EmailTemplate(d) => format!("email_template '{}'", d.name),
            Self::CustomAttributeBlocklist { names, blocklisted } => format!(
                "custom_attribute {} ({})",
                if *blocklisted {
                    "deprecate"
                } else {
                    "reactivate"
                },
                names.join(", "),
            ),
        }
    }

    async fn execute(
        &self,
        client: &BrazeClient,
        content_block_id_index: Option<&ContentBlockIdIndex>,
        email_template_id_index: Option<&EmailTemplateIdIndex>,
    ) -> anyhow::Result<usize> {
        match self {
            Self::Catalog(d) => apply_catalog_schema(client, d).await,
            Self::CatalogField { catalog, op } => apply_catalog_field(client, catalog, op).await,
            Self::ContentBlock(d) => apply_content_block(client, d, content_block_id_index).await,
            Self::EmailTemplate(d) => {
                apply_email_template(client, d, email_template_id_index).await
            }
            Self::CustomAttributeBlocklist { names, blocklisted } => {
                apply_custom_attribute_blocklist(client, names, *blocklisted).await
            }
        }
    }
}

/// Flatten the plan into the writes `apply` will actually issue, in
/// write order. Diffs that fire no API call (orphans, unchanged,
/// informational tag drift) are left out so they can never show up as
/// "not attempted" in a failure report.
fn collect_write_units(summary: &DiffSummary) -> Vec<WriteUnit<'_>> {
    let mut units = Vec::new();
    let mut ca_deprecate: Vec<&str> = Vec::new();
    let mut ca_reactivate: Vec<&str> = Vec::new();
    for diff in &summary.diffs {
        match diff {
            ResourceDiff::CatalogSchema(d) => match &d.op {
                // One call carries the whole schema in either direction.
                DiffOp::Added(_) | DiffOp::Removed(_) => units.push(WriteUnit::Catalog(d)),
                // Field-level edits are one call each, so they are one
                // unit each — see the `WriteUnit` invariant.
                DiffOp::Modified { .. } | DiffOp::Unchanged => {
                    for op in d.field_diffs.iter().filter(|op| op.is_change()) {
                        units.push(WriteUnit::CatalogField {
                            catalog: &d.name,
                            op,
                        });
                    }
                }
            },
            ResourceDiff::ContentBlock(d) => {
                if !d.orphan && d.op.is_change() {
                    units.push(WriteUnit::ContentBlock(d));
                }
            }
            ResourceDiff::EmailTemplate(d) => {
                if !d.orphan && d.op.is_change() {
                    units.push(WriteUnit::EmailTemplate(d));
                }
            }
            ResourceDiff::CustomAttribute(d) => {
                if let CustomAttributeOp::DeprecationToggled { to, .. } = &d.op {
                    if *to {
                        ca_deprecate.push(&d.name);
                    } else {
                        ca_reactivate.push(&d.name);
                    }
                }
            }
            // Tags have no remote mutation API. The diff is informational;
            // pre-flight (above) is what protects apply from missing tags.
            ResourceDiff::Tag(_) => {}
        }
    }

    // Batched last, and by direction, to keep the blocklist calls at two
    // per run regardless of attribute count.
    for (names, blocklisted) in [(ca_deprecate, true), (ca_reactivate, false)] {
        if !names.is_empty() {
            units.push(WriteUnit::CustomAttributeBlocklist { names, blocklisted });
        }
    }
    units
}

/// Emit what the run knows at the point it gives up: which writes
/// landed, which one failed, and which were never attempted.
///
/// Without this the operator cannot distinguish a run that wrote
/// nothing from one that wrote most of the plan — the writes that
/// landed are real and correct, so nothing downstream flags them.
///
/// Every claim here is scoped to what actually happened: a run whose
/// first write failed left no partial state at all, and saying it did
/// would send the operator hunting for writes that were never issued.
fn report_partial_apply(
    applied: &[String],
    failed: &str,
    err: &anyhow::Error,
    pending: &[WriteUnit],
    plan_locked: bool,
) {
    if applied.is_empty() {
        eprintln!("✗ apply aborted on the first write — nothing was applied:");
    } else {
        eprintln!("✗ apply aborted — partial state left in Braze (no cross-resource rollback):");
    }
    eprintln!("  applied ({}):", applied.len());
    for label in applied {
        eprintln!("    - {label}");
    }
    if applied.is_empty() {
        eprintln!("    (none — remote state is unchanged)");
    }
    eprintln!("  failed:");
    eprintln!("    - {failed}: {err:#}");
    eprintln!("  not attempted ({}):", pending.len());
    for unit in pending {
        eprintln!("    - {}", unit.label());
    }

    if applied.is_empty() {
        eprintln!("  → nothing to roll back. Fix the failure above, then re-run");
        eprintln!("    `apply` with the same flags.");
        return;
    }
    eprintln!("  → the applied writes above are live and are not rolled back.");
    eprintln!("    Run `braze-sync diff` to confirm the current remote state, then");
    eprintln!("    re-run `apply` with the same flags to pick up the changes that");
    eprintln!("    were not attempted.");
    if plan_locked {
        // The applied writes drop out of the fresh diff, so the saved
        // plan no longer matches and `check_plan_ops` would exit 7
        // before issuing a single write.
        eprintln!("    Because this run was plan-locked, regenerate the plan first");
        eprintln!("    (`diff --plan-out`) — re-running `apply --plan` as-is exits 7.");
    }
}

/// Reject ops the API can't actually perform. Runs before any write
/// call so a statically-known-bad plan cannot fire a partial apply;
/// runtime write failures can still leave earlier writes in place.
///
/// ContentBlock diffs are deliberately not inspected here: every shape
/// the diff layer can produce (`Added` → create, `Modified` → update,
/// `orphan` → report-only no-op) maps to a supported API call, so there
/// is nothing to statically reject. If a future diff shape is added
/// (e.g. content-type change with no in-place update), re-evaluate.
/// Block apply when any resource that would be mutated references a tag
/// not declared in `tags/registry.yaml`. Braze has no public REST API for
/// workspace tags, so we cannot create them; the only reliable way to
/// avoid the cascading "Tags could not be found" failure (a single tagged
/// content_block create that 400s blocks every queued create after it) is
/// to refuse the apply entirely until the operator either (a) adds the
/// tag to the registry after creating it in the Braze dashboard, or
/// (b) removes the tag from the offending resource.
///
/// No-op when the `tag` resource is disabled in config — this is opt-in
/// safety, not a forced gate.
fn enforce_tag_preflight(
    config_dir: &Path,
    resolved: &ResolvedConfig,
    summary: &DiffSummary,
) -> anyhow::Result<()> {
    if !resolved.resources.is_enabled(ResourceKind::Tag) {
        return Ok(());
    }
    let tags_path = config_dir.join(&resolved.resources.tag.path);
    let registry = match crate::fs::tag_io::load_registry(&tags_path)? {
        Some(r) => r,
        // No registry file → operator hasn't opted in to tag tracking yet;
        // skip the gate rather than blocking on a config-only state.
        None => return Ok(()),
    };
    let excludes = resolved.excludes_for(ResourceKind::Tag);

    let mut missing: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for diff in &summary.diffs {
        // Only inspect resources that would actually fire a write.
        let (resource_label, tags) = match diff {
            ResourceDiff::ContentBlock(d) => match &d.op {
                DiffOp::Added(cb) => (format!("content_block '{}'", cb.name), &cb.tags),
                DiffOp::Modified { to, .. } => (format!("content_block '{}'", to.name), &to.tags),
                _ => continue,
            },
            ResourceDiff::EmailTemplate(d) => match &d.op {
                DiffOp::Added(et) => (format!("email_template '{}'", et.name), &et.tags),
                DiffOp::Modified { to, .. } => (format!("email_template '{}'", to.name), &to.tags),
                _ => continue,
            },
            _ => continue,
        };
        for t in tags {
            if crate::config::is_excluded(t, excludes) {
                continue;
            }
            if !registry.contains(t) {
                missing
                    .entry(t.clone())
                    .or_default()
                    .push(resource_label.clone());
            }
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    let mut msg = String::from(
        "tag pre-flight: refusing to apply — the following tags are referenced \
         by resources that would be created/updated, but are not declared in \
         tags/registry.yaml:\n",
    );
    for (tag, refs) in &missing {
        msg.push_str(&format!("  • '{tag}' — referenced by:\n"));
        for r in refs {
            msg.push_str(&format!("      - {r}\n"));
        }
    }
    msg.push_str(
        "\nFix: create each missing tag in the Braze dashboard \
         (Settings → Tags), then add it to tags/registry.yaml and re-run apply. \
         Braze does not expose a tag creation API.",
    );
    Err(anyhow!(msg))
}

/// Reject obvious plan-vs-CLI mismatches before doing any remote work.
fn check_plan_scope(plan: &PlanFile, environment: &str, args: &ApplyArgs) -> anyhow::Result<()> {
    if plan.version != plan::CURRENT_PLAN_VERSION {
        return Err(anyhow!(
            "plan file version {} is not supported by this binary \
             (expected {}). Regenerate with `diff --plan-out`.",
            plan.version,
            plan::CURRENT_PLAN_VERSION,
        ));
    }
    if plan.scope.environment != environment {
        eprintln!(
            "✗ plan drift: plan was generated for environment '{}' \
             but apply targets '{}'",
            plan.scope.environment, environment,
        );
        return Err(Error::PlanDrift.into());
    }
    if plan.scope.resource != args.resource {
        eprintln!(
            "✗ plan drift: plan scope --resource={:?} but apply --resource={:?}",
            plan.scope.resource.map(|k| k.as_str()),
            args.resource.map(|k| k.as_str()),
        );
        return Err(Error::PlanDrift.into());
    }
    if plan.scope.name != args.name {
        eprintln!(
            "✗ plan drift: plan scope --name={:?} but apply --name={:?}",
            plan.scope.name, args.name,
        );
        return Err(Error::PlanDrift.into());
    }
    Ok(())
}

fn warn_on_plan_metadata(plan: &PlanFile) {
    let current = env!("CARGO_PKG_VERSION");
    if plan.braze_sync_version != current {
        eprintln!(
            "⚠ plan was generated by braze-sync {} (current: {}). \
             Proceeding — pass `diff --plan-out` again if op semantics changed.",
            plan.braze_sync_version, current,
        );
    }
    let age = chrono::Utc::now().signed_duration_since(plan.generated_at);
    if age > plan::STALE_PLAN_WARN_THRESHOLD {
        eprintln!(
            "⚠ plan is {} hour(s) old — Braze may have drifted; consider \
             regenerating with `diff --plan-out`.",
            age.num_hours(),
        );
    }
}

/// Compare the saved plan's ops against the freshly-computed summary.
/// Returns `Error::PlanDrift` on any (kind, name, op_type) mismatch.
fn check_plan_ops(saved: &PlanFile, fresh: &DiffSummary) -> anyhow::Result<()> {
    let fresh_ops = plan::collect_ops(fresh);
    let diff = saved.diff_ops(&fresh_ops);
    if diff.is_match() {
        return Ok(());
    }
    eprintln!("✗ plan drift: saved plan and fresh plan differ");
    if !diff.missing.is_empty() {
        eprintln!("  ops in saved plan but not in fresh plan (resolved or absorbed remotely):");
        for op in &diff.missing {
            eprintln!("    - {} '{}' [{:?}]", op.kind.as_str(), op.name, op.op);
        }
    }
    if !diff.extra.is_empty() {
        eprintln!("  ops in fresh plan but not in saved plan (new drift since plan):");
        for op in &diff.extra {
            eprintln!("    - {} '{}' [{:?}]", op.kind.as_str(), op.name, op.op);
        }
    }
    eprintln!("  → regenerate with `diff --plan-out` and review before re-applying");
    Err(Error::PlanDrift.into())
}

fn check_for_unsupported_ops(summary: &DiffSummary) -> anyhow::Result<()> {
    for diff in &summary.diffs {
        if let ResourceDiff::CatalogSchema(d) = diff {
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
) -> anyhow::Result<usize> {
    // Content block orphans are report-only: Braze has no DELETE endpoint
    // and rejects renaming a content block after activation, so apply
    // cannot mutate them. They surface in the always-on orphan report in
    // the diff output instead. See docs/orphan-tracking.md.
    if d.orphan {
        return Ok(0);
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

/// Create or delete a whole catalog. Both are a single API call:
/// `POST /catalogs` carries the full schema (so no per-field POSTs are
/// needed for a create) and `DELETE /catalogs/{name}` drops the schema
/// and all items at once (so per-field DELETEs would be redundant, and
/// would 404 once the catalog is gone).
///
/// Field-level edits on an existing catalog do not come here — they are
/// one [`WriteUnit::CatalogField`] each, applied by
/// [`apply_catalog_field`].
async fn apply_catalog_schema(
    client: &BrazeClient,
    d: &CatalogSchemaDiff,
) -> anyhow::Result<usize> {
    match &d.op {
        DiffOp::Added(cat) => {
            tracing::info!(catalog = %d.name, "creating new catalog");
            client
                .create_catalog(cat)
                .await
                .with_context(|| format!("creating catalog '{}'", d.name))?;
            Ok(1)
        }
        DiffOp::Removed(_) => {
            tracing::info!(catalog = %d.name, "deleting catalog");
            client
                .delete_catalog(&d.name)
                .await
                .with_context(|| format!("deleting catalog '{}'", d.name))?;
            Ok(1)
        }
        DiffOp::Modified { .. } | DiffOp::Unchanged => Err(anyhow!(
            "internal: catalog '{}' routed to the whole-catalog write \
             path without a create/delete op",
            d.name,
        )),
    }
}

/// Apply one field diff on an existing catalog — exactly one API call,
/// which is what lets the failure report name precisely which fields
/// landed and which were never attempted.
async fn apply_catalog_field(
    client: &BrazeClient,
    catalog: &str,
    op: &DiffOp<CatalogField>,
) -> anyhow::Result<usize> {
    match op {
        DiffOp::Added(f) => {
            tracing::info!(
                catalog = %catalog,
                field = %f.name,
                field_type = f.field_type.as_str(),
                "adding catalog field"
            );
            client.add_catalog_field(catalog, f).await?;
            Ok(1)
        }
        DiffOp::Removed(f) => {
            tracing::info!(
                catalog = %catalog,
                field = %f.name,
                "deleting catalog field"
            );
            client.delete_catalog_field(catalog, &f.name).await?;
            Ok(1)
        }
        DiffOp::Modified { .. } => Err(anyhow!(
            "internal: Modified field op should have been rejected \
             by check_for_unsupported_ops"
        )),
        DiffOp::Unchanged => Ok(0),
    }
}

async fn apply_email_template(
    client: &BrazeClient,
    d: &EmailTemplateDiff,
    id_index: Option<&EmailTemplateIdIndex>,
) -> anyhow::Result<usize> {
    // Email template orphans are report-only. Braze exposes no DELETE
    // endpoint and no usage/inclusion data, so we cannot prove a template
    // is unused before mutating it; renaming an API-referenced template
    // breaks send-time name resolution. They surface in the diff output for
    // manual dashboard archival instead. See docs/orphan-tracking.md.
    if d.orphan {
        return Ok(0);
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

/// Toggle one direction of custom attribute deprecation. Callers batch
/// by direction so a run issues at most two blocklist calls regardless
/// of attribute count; the per-unit success line is printed by the
/// caller's walk.
async fn apply_custom_attribute_blocklist(
    client: &BrazeClient,
    names: &[&str],
    blocklisted: bool,
) -> anyhow::Result<usize> {
    let verb = if blocklisted {
        "deprecating"
    } else {
        "reactivating"
    };
    tracing::info!(attributes = ?names, "{verb} custom attributes");
    client
        .set_custom_attribute_blocklist(names, blocklisted)
        .await
        .with_context(|| format!("{verb} custom attributes"))?;
    Ok(names.len())
}
