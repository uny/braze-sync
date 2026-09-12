//! `braze-sync export` — pull current state from Braze into local files.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::config::{is_excluded, ResolvedConfig};
use crate::fs::{catalog_io, content_block_io, custom_attribute_io, email_template_io, tag_io};
use crate::resource::{
    ContentBlock, CustomAttribute, CustomAttributeRegistry, EmailTemplate, ResourceKind, Tag,
    TagRegistry,
};
use crate::values::has_placeholders;
use crate::values::templatize::{templatize_body, FieldKind};
use anyhow::Context as _;
use clap::Args;
use futures::stream::{StreamExt, TryStreamExt};
use regex_lite::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{selected_kinds, warn_if_name_excluded, FETCH_CONCURRENCY};

#[derive(Args, Debug, Default)]
pub struct ExportArgs {
    /// Limit export to a specific resource kind. Omit to export every
    /// enabled resource kind in turn.
    #[arg(long, value_enum)]
    pub resource: Option<ResourceKind>,

    /// When `--resource` is given, optionally restrict to a single named
    /// resource. Requires `--resource`.
    #[arg(long, requires = "resource")]
    pub name: Option<String>,

    /// Rebuild the Custom Attribute registry from the queried workspace
    /// alone, dropping entries that workspace does not have.
    ///
    /// Without this, `export` refreshes the entries Braze returned and
    /// keeps the rest: an attribute materializes on first `/users/track`
    /// traffic, so its absence from one workspace is not evidence the
    /// registry entry is wrong. Pass `--prune` when you do mean "the
    /// remote is the truth, rewrite the file".
    ///
    /// Entries matching `exclude_patterns` are kept even here — excluded
    /// means the remote is not consulted about them, so the remote's
    /// silence is not evidence either. The one exception is the corrupt-
    /// file recovery below: identifying an excluded entry means reading
    /// the file, so a file that will not load takes them with it, and
    /// the summary line says so.
    ///
    /// `--prune` is also the recovery path for a `registry.yaml` that no
    /// longer parses: it is the one mode that tolerates a parse error.
    /// It still reads the file when it can, to report how many entries
    /// it dropped — so when the read fails it reports that the count is
    /// unknown rather than reporting zero.
    ///
    /// Affects `custom_attribute` only. `tags/registry.yaml` is
    /// single-file too, but it is rebuilt from the tags local resources
    /// reference rather than from a remote list, so no other kind has
    /// state a workspace's silence can remove.
    ///
    /// Conflicts with `--name`. `export` ignores `--name` for
    /// `custom_attribute` (the registry is a single file), so the two
    /// together would read as "prune this one entry" and do the exact
    /// opposite — rebuild the whole file.
    #[arg(long, conflicts_with = "name")]
    pub prune: bool,
}

pub async fn run(
    args: &ExportArgs,
    resolved: ResolvedConfig,
    config_dir: &Path,
) -> anyhow::Result<()> {
    let catalogs_root = config_dir.join(&resolved.resources.catalog_schema.path);
    let content_blocks_root = config_dir.join(&resolved.resources.content_block.path);
    let email_templates_root = config_dir.join(&resolved.resources.email_template.path);
    let custom_attributes_path = config_dir.join(&resolved.resources.custom_attribute.path);
    let tags_path = config_dir.join(&resolved.resources.tag.path);
    let client = BrazeClient::from_resolved(&resolved);
    let kinds = selected_kinds(args.resource, &resolved.resources);

    // `--prune` only means something for the single-file Custom Attribute
    // registry. Silently accepting it elsewhere would let an operator
    // believe a rebuild happened.
    //
    // Keyed off `--resource`, not off `kinds`: when the operator named
    // `custom_attribute` and it is disabled in config, `selected_kinds`
    // has already said so and returned nothing. Saying "it has no effect
    // on the selected resource kind(s)" on top of that would name the
    // wrong cause — they picked the right kind; it is switched off.
    if args.prune
        && !kinds.contains(&ResourceKind::CustomAttribute)
        && args.resource != Some(ResourceKind::CustomAttribute)
    {
        eprintln!(
            "⚠ --prune affects custom_attribute only; it has no effect on \
             the selected resource kind(s)"
        );
    }

    let mut total_written: usize = 0;
    for kind in kinds {
        // `custom_attribute` ignores `--name` (registry is a single file),
        // so skipping by exclude match before dispatching wouldn't fit —
        // handle it per-arm alongside the existing --name warning.
        if !matches!(kind, ResourceKind::CustomAttribute | ResourceKind::Tag)
            && warn_if_name_excluded(kind, args.name.as_deref(), resolved.excludes_for(kind))
        {
            continue;
        }
        match kind {
            ResourceKind::CatalogSchema => {
                let n = export_catalog_schemas(
                    &client,
                    &catalogs_root,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::CatalogSchema),
                )
                .await
                .context("exporting catalog_schema")?;
                eprintln!("✓ catalog_schema: exported {n} resource(s)");
                total_written += n;
            }
            ResourceKind::ContentBlock => {
                let n = export_content_blocks(
                    &client,
                    &content_blocks_root,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::ContentBlock),
                )
                .await
                .context("exporting content_block")?;
                eprintln!("✓ content_block: exported {n} resource(s)");
                total_written += n;
            }
            ResourceKind::EmailTemplate => {
                let n = export_email_templates(
                    &client,
                    &email_templates_root,
                    args.name.as_deref(),
                    resolved.excludes_for(ResourceKind::EmailTemplate),
                )
                .await
                .context("exporting email_template")?;
                eprintln!("✓ email_template: exported {n} resource(s)");
                total_written += n;
            }
            ResourceKind::CustomAttribute => {
                if args.name.is_some() {
                    eprintln!(
                        "⚠ custom_attribute: --name is not supported for export \
                         (the registry is a single file); exporting all attributes"
                    );
                }
                let outcome = export_custom_attributes(
                    &client,
                    &custom_attributes_path,
                    resolved.excludes_for(ResourceKind::CustomAttribute),
                    args.prune,
                )
                .await
                .context("exporting custom_attribute")?;
                eprintln!("{}", outcome.summary_line());
                total_written += outcome.written();
            }
            ResourceKind::Tag => {
                if args.name.is_some() {
                    eprintln!(
                        "⚠ tag: --name is not supported for export \
                         (the registry is a single file); exporting all tags"
                    );
                }
                let n = export_tags(
                    config_dir,
                    &resolved,
                    &tags_path,
                    resolved.excludes_for(ResourceKind::Tag),
                )
                .context("exporting tag")?;
                eprintln!("✓ tag: exported {n} tag(s)");
                total_written += n;
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
    excludes: &[Regex],
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

    let filtered: Vec<_> = catalogs
        .into_iter()
        .filter(|c| !is_excluded(&c.name, excludes))
        .collect();
    let count = filtered.len();
    for cat in filtered {
        catalog_io::save_schema(catalogs_root, &cat)?;
    }
    Ok(count)
}

/// Lists first to discover ids, then fetches `/info` per block. With
/// `--name`, the list still happens (to translate name → id) but only
/// the matching block's body is fetched.
///
/// Remote bodies are reverse-templatized (raw lid / cb_id values
/// rewritten back to `__BRAZESYNC__`) before being written to disk,
/// so Dashboard HTML edits are captured while runtime-volatile lid /
/// cb_id values do not produce spurious drift. This applies to both
/// new resources (no local file yet) and existing ones whose local
/// template already contains placeholders. The only case where
/// templatization is skipped is when a local file exists but
/// deliberately contains no placeholders.
/// v0.15: no values-file writeback — lid / cb_id are resolved from
/// the remote body at apply/diff time.
async fn export_content_blocks(
    client: &BrazeClient,
    content_blocks_root: &Path,
    name_filter: Option<&str>,
    excludes: &[Regex],
) -> anyhow::Result<usize> {
    let summaries = client.list_content_blocks().await?;
    let targets: Vec<_> = summaries
        .into_iter()
        .filter(|s| name_filter.is_none_or(|n| s.name == n))
        .filter(|s| !is_excluded(&s.name, excludes))
        .collect();

    if targets.is_empty() {
        if let Some(name) = name_filter {
            eprintln!("⚠ content_block: '{name}' not found in Braze");
        }
        return Ok(0);
    }

    let blocks: Vec<ContentBlock> = futures::stream::iter(targets.iter().map(|s| {
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

    for remote in &blocks {
        let local_path = content_blocks_root.join(format!("{}.liquid", remote.name));
        let local = if local_path.exists() {
            Some(content_block_io::read_content_block_file(&local_path)?)
        } else {
            None
        };
        let mut to_save = remote.clone();
        let should_templatize = local.as_ref().is_none_or(|l| has_placeholders(&l.content));
        if should_templatize {
            to_save.content = templatize_body(&remote.content, FieldKind::ContentBlock).new_body;
        }
        content_block_io::save_content_block(content_blocks_root, &to_save)?;
    }
    Ok(blocks.len())
}

/// Same list-then-fetch pattern as content blocks. Per-field reverse-
/// templatize: each of `subject`, `body_html`, `body_plaintext`,
/// `preheader` is templatized for new resources (no local dir), when
/// the corresponding local field already contains placeholders, or
/// when the local field is absent (preheader not yet saved locally).
async fn export_email_templates(
    client: &BrazeClient,
    email_templates_root: &Path,
    name_filter: Option<&str>,
    excludes: &[Regex],
) -> anyhow::Result<usize> {
    let summaries = client.list_email_templates().await?;
    let targets: Vec<_> = summaries
        .into_iter()
        .filter(|s| name_filter.is_none_or(|n| s.name == n))
        .filter(|s| !is_excluded(&s.name, excludes))
        .collect();

    if targets.is_empty() {
        if let Some(name) = name_filter {
            eprintln!("⚠ email_template: '{name}' not found in Braze");
        }
        return Ok(0);
    }

    let templates: Vec<EmailTemplate> = futures::stream::iter(targets.iter().map(|s| {
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

    for remote in &templates {
        let local_dir = email_templates_root.join(&remote.name);
        let local = if local_dir.is_dir() {
            Some(email_template_io::read_email_template_dir(&local_dir)?)
        } else {
            None
        };
        let mut to_save = remote.clone();
        let subject_templ = local.as_ref().is_none_or(|l| has_placeholders(&l.subject));
        let body_html_templ = local
            .as_ref()
            .is_none_or(|l| has_placeholders(&l.body_html));
        let body_plain_templ = local
            .as_ref()
            .is_none_or(|l| has_placeholders(&l.body_plaintext));
        let preheader_templ = local
            .as_ref()
            .is_none_or(|l| l.preheader.as_deref().is_none_or(has_placeholders));
        if subject_templ {
            to_save.subject = templatize_body(&remote.subject, FieldKind::EmailSubject).new_body;
        }
        if body_html_templ {
            to_save.body_html =
                templatize_body(&remote.body_html, FieldKind::EmailHtmlBody).new_body;
        }
        if body_plain_templ {
            to_save.body_plaintext =
                templatize_body(&remote.body_plaintext, FieldKind::EmailPlainBody).new_body;
        }
        if preheader_templ {
            to_save.preheader = remote
                .preheader
                .as_deref()
                .map(|p| templatize_body(p, FieldKind::EmailPreheader).new_body);
        }
        email_template_io::save_email_template(email_templates_root, &to_save)?;
    }
    Ok(templates.len())
}

/// Aggregate tag names from local content_block + email_template files.
///
/// Braze does not expose a public REST API for workspace tags, so the
/// registry is derived from the local Git state instead of a remote
/// list. Operators are expected to run regular `export` first (to refresh
/// content_block / email_template files), then `export tag` to rebuild
/// the registry from the freshly-synced frontmatter. Tags found here are
/// the union of every `tags:` array on every local resource, minus
/// `tag.exclude_patterns`.
fn export_tags(
    config_dir: &Path,
    resolved: &ResolvedConfig,
    registry_path: &Path,
    excludes: &[Regex],
) -> anyhow::Result<usize> {
    let referenced = collect_local_tag_references(config_dir, resolved)?;
    let tags: Vec<Tag> = referenced
        .into_iter()
        .filter(|name| !is_excluded(name, excludes))
        .map(|name| Tag {
            name,
            description: None,
        })
        .collect();
    let count = tags.len();
    let registry = TagRegistry { tags };
    tag_io::save_registry(registry_path, &registry)?;
    Ok(count)
}

/// Walk every local resource directory the config knows about and
/// collect the union of `tags:` referenced on the resources. Used by
/// both `export` (to rebuild registry) and `apply`/`validate`
/// (to cross-check the registry against actual usage).
pub(crate) fn collect_local_tag_references(
    config_dir: &Path,
    resolved: &ResolvedConfig,
) -> anyhow::Result<BTreeSet<String>> {
    let mut tags: BTreeSet<String> = BTreeSet::new();

    if resolved.resources.content_block.enabled {
        let root = config_dir.join(&resolved.resources.content_block.path);
        let blocks = content_block_io::load_all_content_blocks(&root)
            .context("loading local content_blocks for tag aggregation")?;
        for cb in &blocks {
            for t in &cb.tags {
                tags.insert(t.clone());
            }
        }
    }

    if resolved.resources.email_template.enabled {
        let root = config_dir.join(&resolved.resources.email_template.path);
        let templates = crate::fs::email_template_io::load_all_email_templates(&root)
            .context("loading local email_templates for tag aggregation")?;
        for et in &templates {
            for t in &et.tags {
                tags.insert(t.clone());
            }
        }
    }

    Ok(tags)
}

/// What one Custom Attribute registry export did to the file.
///
/// `export` is the only writer of `registry.yaml`, and it is the only
/// kind whose export can drop an entry the *remote* did not speak to —
/// `tags/registry.yaml` is single-file too, but it is rebuilt from local
/// resource frontmatter, so nothing there turns on what Braze returned.
/// The counts are reported separately because a single "N attributes
/// written" count cannot say whether entries disappeared.
struct RegistryExport {
    /// Entries taken from the Braze response.
    refreshed: usize,
    /// Entries kept because the queried workspace does not have them.
    /// Zero under `--prune`.
    kept_registry_only: usize,
    /// Entries kept because they match `exclude_patterns`. Kept under
    /// `--prune` too: "managed out of band" is not a statement about
    /// which workspace happens to hold them.
    kept_excluded: usize,
    /// Entries dropped because the queried workspace does not have them.
    /// `Some(0)` outside `--prune`. `None` means the file `--prune`
    /// replaced was corrupt, so the number is genuinely unknown — which
    /// has to be said rather than reported as zero.
    removed: Option<usize>,
    /// Surplus entries dropped because two or more shared a name. Counted
    /// in *entries*, not names — this is the "how many lines vanished"
    /// number, whereas the warning that names them counts names.
    ///
    /// Reported separately from `removed`, and never folded into it:
    /// these went because the file disagreed with itself, not because a
    /// workspace was silent about them. Folding them in would also make
    /// plain `export` — which reports `removed: Some(0)` — claim it
    /// removed nothing on a run that did.
    collapsed_duplicates: usize,
}

impl RegistryExport {
    /// Entries actually in the file afterwards.
    fn written(&self) -> usize {
        self.refreshed + self.kept_registry_only + self.kept_excluded
    }

    fn summary_line(&self) -> String {
        let mut parts = vec![format!("refreshed {} from Braze", self.refreshed)];
        match self.removed {
            // A corrupt file that `--prune` replaced. How much it
            // replaced cannot be established, and reporting 0 would be
            // the same silent destructive write this function was changed
            // to stop making. (A file that could not be *read* does not
            // reach here at all — that aborts.)
            // The excluded clause is not a detail: everywhere else
            // `--prune` promises to keep excluded entries, and this is
            // the one path where it cannot — reading the file is what
            // would have identified them. Saying only "unknown" would
            // leave the operator holding a promise that quietly did not
            // apply to the run they just made.
            None => parts.push(
                "replaced a corrupt registry, entries removed: unknown \
                 (any excluded entries it held could not be kept)"
                    .into(),
            ),
            Some(n) if n > 0 => parts.push(format!(
                "removed {n} registry entr{} this workspace does not have",
                plural_y(n)
            )),
            Some(_) => {}
        }
        if self.kept_registry_only > 0 {
            parts.push(format!(
                "kept {} registry-only entr{}",
                self.kept_registry_only,
                plural_y(self.kept_registry_only)
            ));
        }
        if self.kept_excluded > 0 {
            parts.push(format!(
                "kept {} excluded entr{}",
                self.kept_excluded,
                plural_y(self.kept_excluded)
            ));
        }
        if self.collapsed_duplicates > 0 {
            // Named separately from `removed` so that a plain `export`
            // — which removes nothing on the remote's account — cannot
            // report a run that deleted lines as if it had deleted none.
            parts.push(format!(
                "dropped {} duplicate entr{}",
                self.collapsed_duplicates,
                plural_y(self.collapsed_duplicates)
            ));
        }
        format!("✓ custom_attribute: {}", parts.join(", "))
    }
}

/// Whether a failed registry load means "the file's contents are
/// corrupt" rather than "the file could not be reached".
///
/// Only the first is something `--prune` is asked to recover from. A
/// permission fault, or a path `read_to_string` refuses (a directory),
/// is not a registry anyone asked to replace, and treating it as one
/// would be a silent destructive write. (A missing file is neither:
/// `load_registry` maps `NotFound` to `Ok(None)` before this is
/// consulted.)
///
/// "Not a file" is not the boundary, and saying so would overclaim: a
/// FIFO at the registry path does not error here, it blocks in
/// `read_to_string` until something opens the other end. The boundary
/// is what the read *returns*.
fn is_corrupt_content(e: &crate::error::Error) -> bool {
    match e {
        crate::error::Error::YamlParse { .. } => true,
        crate::error::Error::Io(io) => io.kind() == std::io::ErrorKind::InvalidData,
        _ => false,
    }
}

fn plural_y(n: usize) -> &'static str {
    if n == 1 {
        "y"
    } else {
        "ies"
    }
}

/// Refresh the Custom Attribute registry from Braze.
///
/// Unlike every other resource kind, the registry is a single file, so a
/// write here decides membership for the whole set rather than for one
/// resource. Directory-backed kinds preserve a local-only resource for
/// free — `export` simply never writes that file — and this function
/// matches that behaviour explicitly: entries the queried workspace does
/// not return are kept, not dropped.
///
/// That is not a courtesy. Braze has no create-attribute endpoint; an
/// attribute exists once `/users/track` has carried it. A registry entry
/// missing from one workspace therefore means "no traffic yet here", not
/// "wrong entry" — and a registry shared by environments that point at
/// different workspaces has such entries by construction. Dropping them
/// would also make the `type mismatch: … (run export to update)` hint
/// `diff` prints a destructive instruction.
///
/// Entries Braze *did* return are overwritten wholesale, which is what
/// keeps that hint's promise: a stale `type` (or `description`, or
/// `deprecated`) is corrected here. Only membership is preserved.
///
/// `--prune` restores the "the remote is the truth" rebuild — for the
/// entries whose truth the remote actually speaks to. An entry matching
/// `exclude_patterns` is kept either way: excluded means the remote is
/// not consulted about it, so the remote's silence says nothing.
async fn export_custom_attributes(
    client: &BrazeClient,
    registry_path: &Path,
    excludes: &[Regex],
    prune: bool,
) -> anyhow::Result<RegistryExport> {
    // Classification needs the *unfiltered* names: an excluded attribute
    // Braze returned is still an attribute Braze returned. Filtering
    // before this point is what made `--prune` delete an excluded entry
    // while reporting that the workspace did not have it.
    let remote_all = client.list_custom_attributes().await?;
    let remote_names: BTreeSet<String> = remote_all.iter().map(|a| a.name.clone()).collect();
    let remote: Vec<CustomAttribute> = remote_all
        .into_iter()
        .filter(|a| !is_excluded(&a.name, excludes))
        .collect();

    // `--prune` must not fail on an unparseable registry: replacing one
    // is what the flag is for, so a parse error there is expected input
    // rather than an error — but the entry count it replaced is then
    // unknowable, and `None` says so. Every *other* error still
    // propagates, `--prune` included: a registry that cannot be read
    // because of a permission or I/O fault is not a registry anyone
    // asked to replace, and swallowing that would be a silent
    // destructive write of exactly the kind this function exists to
    // prevent. Without `--prune` even the parse error propagates, since
    // merging cannot proceed without knowing what is in the file.
    let mut count_unknown = false;
    let local = match custom_attribute_io::load_registry(registry_path) {
        Ok(local) => local,
        // Content corruption: bad YAML syntax, valid YAML of the wrong
        // shape, or bytes that are not UTF-8 at all — `read_to_string`
        // rejects those before the parser ever sees them, so they arrive
        // as `Io(InvalidData)` rather than `YamlParse`.
        Err(e) if prune && is_corrupt_content(&e) => {
            count_unknown = true;
            None
        }
        Err(e) => return Err(e.into()),
    };

    // Three buckets, keyed so duplicate local names collapse last-wins,
    // matching `diff::custom_attribute::diff`. Disagreeing would leave
    // the two commands unable to converge: whichever entry `export`
    // kept, `diff` would keep reporting the other one as drift.
    let mut excluded: BTreeMap<&str, &CustomAttribute> = BTreeMap::new();
    let mut registry_only: BTreeMap<&str, &CustomAttribute> = BTreeMap::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut duplicated: BTreeSet<&str> = BTreeSet::new();
    let mut collapsed_duplicates: usize = 0;
    for a in local.iter().flat_map(|r| r.attributes.iter()) {
        let name = a.name.as_str();
        if !seen.insert(name) {
            // Two units, deliberately. The warning names *names*: three
            // entries for one name is one duplicated name, and a count
            // of 2 there would be read as two names. The summary line
            // counts *entries*, because that is how many lines the write
            // is about to delete.
            duplicated.insert(name);
            collapsed_duplicates += 1;
        }
        if is_excluded(name, excludes) {
            excluded.insert(name, a);
        } else if !remote_names.contains(name) {
            registry_only.insert(name, a);
        }
    }
    if !duplicated.is_empty() {
        // `diff` warns on the same input; staying silent here would make
        // the command that actually drops the losing entry the quiet one.
        //
        // The wording deliberately differs from `diff`'s "last entry
        // wins". That is true on `diff`'s side, but not always here: if
        // the duplicated name is one Braze returned and it is not
        // excluded, neither local entry survives — the remote value
        // overwrites both. What holds in every case is that one entry
        // per name is written.
        //
        // `eprintln!`, not `tracing::warn!` as in `diff`, and the names
        // spelled out rather than counted: this is the one line standing
        // between an operator and a deleted entry, and a `tracing` line
        // is silenced by whatever `RUST_LOG` their shell happens to
        // carry. The names have to be here because `validate` cannot be
        // the fallback — it skips excluded names before its own
        // duplicate check, so for exactly the out-of-band entries this
        // deletion hurts most, it reports nothing.
        eprintln!(
            "⚠ custom_attribute: duplicate name(s) in the local registry \
             — only one entry per name is written, the rest are dropped: {}",
            duplicated.iter().copied().collect::<Vec<_>>().join(", ")
        );
    }

    let refreshed = remote.len();
    let kept_excluded = excluded.len();
    let registry_only_count = registry_only.len();

    let mut attributes = remote;
    attributes.extend(excluded.into_values().cloned());
    if !prune {
        attributes.extend(registry_only.into_values().cloned());
    }

    // `save_registry` normalizes (sorts by name), so the merged order
    // does not leak into the file.
    let registry = CustomAttributeRegistry { attributes };
    custom_attribute_io::save_registry(registry_path, &registry)?;

    Ok(RegistryExport {
        refreshed,
        kept_registry_only: if prune { 0 } else { registry_only_count },
        kept_excluded,
        removed: if !prune {
            Some(0)
        } else if count_unknown {
            None
        } else {
            Some(registry_only_count)
        },
        collapsed_duplicates,
    })
}
