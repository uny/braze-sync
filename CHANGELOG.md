# Changelog

All notable changes to braze-sync are recorded here. The format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
versions follow [semver](https://semver.org/). Per IMPLEMENTATION.md
§0.2 the v0.x line is crates.io-published but tolerates breaking
changes; v1.0 freezes the public surface (CLI flags, config schema,
file formats, JSON output, exit codes) for the full v1.x line.

## [0.20.0] — 2026-08-20

### Changed

- **`apply` reports partial state on abort (#96).** A per-resource write
  failure (a Braze rejection, a permission error, a transient 4xx) still
  aborts the run — Braze has no cross-resource transaction — but the run
  now enumerates what it applied, what failed with the API error, and
  what it never attempted, plus the remediation. Previously a failed run
  printed only the error, so a run that wrote 20 of 25 resources and one
  that wrote none looked identical and the real remote state had to be
  re-derived from a second `diff`. Successful writes are also echoed as
  the walk proceeds.

  The enumeration is per **API call**, not per resource: a catalog's
  field writes are listed one field at a time, so a failure partway
  through a catalog still names the fields that landed. `applied` and
  `not attempted` are certain; the failed write is reported as
  possibly-landed, since Braze can commit a write whose response never
  reaches the client. A run whose first write fails reports
  that there is no earlier write to roll back rather than claiming a
  partial state, and a plan-locked run is told to regenerate
  its plan before re-running (the applied writes drop out of the fresh
  diff, so `apply --plan` would otherwise exit 7).

  **Output change:** the per-batch line `  ✓ deprecated N custom
  attribute(s)` / `  ✓ reactivated N custom attribute(s)` is replaced by
  the unified per-write line `  ✓ custom_attribute deprecate (a, b)`.
  Anything grepping stderr for the old wording needs updating; `--format
  json` output on stdout is unaffected. The success echo is likewise per
  API call: a catalog with three field additions emits three
  `  ✓ catalog_schema 'x' field '…' (add)` lines rather than one per
  resource.

### Security

- **`h2` bumped to 0.4.17** for RUSTSEC-2026-0258. Transitive dependency
  via `reqwest`; no braze-sync API surface is affected.

## [0.19.0] — 2026-08-17

### Added

- **Fallback gate.** `diff` now exits `8`, and `apply` requires
  `--allow-fallback` in addition to `--confirm`, when a `lid`
  placeholder resolves via a generated fallback slug under suspicious
  conditions: an unmatched template placeholder *and* an unconsumed
  remote `lid` value both present for the same field once exact
  matching completes. An ordinary new link — an unmatched placeholder
  with nothing left on the remote side to consume — does not gate,
  and dry-run (`apply` without `--confirm`) is unaffected. Fixes #93.

### Fixed

- **A content block include whose `${NAME}` held whitespace no longer
  breaks a neighboring `lid` anchor, and `templatize` no longer treats
  such an include as if nothing were wrong.** Three related gaps
  closed together:
  - The mask that stabilizes the anchor key required the captured
    `${NAME}` to carry no whitespace at all; when it did, the whole
    include stayed unmasked in the key, and a later Braze-side cb_id
    reassignment silently overwrote a neighboring `lid`'s live value
    with a generated fallback slug.
  - `templatize` now warns and leaves an include with an empty or
    whitespace-holding name untemplated, instead of silently
    mismanaging it — including a name differing only by a vertical tab,
    which previously appeared to templatize successfully and then
    deterministically failed to resolve later.
  - `templatize`'s warnings — including the new one above — now reach
    the operator even when a field has zero rewrites; they were
    previously dropped by an early skip.

  Fixes #85.

## [0.18.0] — 2026-08-14

### Added

- **`diff --only-drift`** omits in-sync resources from the table output.
  On a workspace of a few hundred resources the `✅ … / no drift` blocks
  are the overwhelming majority of the output — a reported measurement
  had 1141 of 1190 lines saying nothing — which buries the review
  material when CI posts a `diff` into a pull request comment, and
  pushes the comment toward GitHub's 65536-character limit. The flag
  suppresses only blocks whose entire body is `no drift`, so an in-sync
  resource that still carries an informational line (e.g. a Custom
  Attribute type mismatch) stays visible; the `Summary:` trailer and the
  always-on orphan report still account for every resource. Table output
  only — the frozen v1 JSON schema is unaffected. Fixes #81.

## [0.17.1] — 2026-08-13

### Fixed

- **`lid` anchor correlation no longer depends on how Braze formats the
  Liquid tag.** The anchor key that pairs a `__BRAZESYNC__` placeholder
  with its live `lid` in the remote body was a normalized string, and
  several spellings the Braze dashboard can produce did not normalize to
  the template's. When the key missed, the live `lid` was replaced by a
  generated fallback slug. Four such spellings are closed:
  - a Liquid-templated query separator, which left no literal `?` in the
    URL and so left the `lid` value itself inside the key — never equal
    across the two sides. Managed `| lid:` / `| id:` filters are now
    masked out of the key. Fixes #68.
  - in plaintext bodies, a URL run that stopped at the first quote inside
    a Liquid tag, making the key depend on formatting `templatize`
    rewrites. Runs now span whole `{{…}}` tags. Fixes #70.
  - a tag containing braces of its own — this repo's own
    `{{content_blocks.${NAME} | id: 'cbN'}}` include — which was not
    treated as one atom, so #70's symptoms survived for any URL built
    from an include. Fixes #73.
  - whitespace anywhere inside a Liquid tag other than immediately around
    the managed filter, so a dashboard reformat of `{{x|lid:'…'}}` into
    `{{ x | lid: '…' }}` keyed differently. Intra-tag whitespace is now
    normalized, except inside a quoted argument where it is part of the
    value. Fixes #77.

  **Operators should check links applied since v0.17.0.** While these
  bugs were live, an `apply` could overwrite a real Braze `lid` with a
  generated fallback slug. The run warned at the time — `lid: URL anchor
  '…' not found in remote body — using fallback value '…'` — but the
  slug is itself a valid `lid`, so the original value is not recoverable
  from the local template. This release stops it happening; it cannot
  undo an assignment already made. If those warnings appeared in a past
  run, verify the affected links in the Braze dashboard.

- **Mask sentinels no longer leak into operator output or into Braze.**
  The comparison-only placeholders used to collapse both sides of an
  anchor key could reach a warning shown to an operator, and could reach
  a fallback `lid` value POSTed to Braze. Fixes #71.

### Security

- **Bumped `anyhow` and `quinn-proto`** for two advisories that were
  failing the `audit` and `deny` jobs: unsoundness in `anyhow` 1.0.102's
  `Error::downcast_mut()`, and **RUSTSEC-2026-0185** (high) — remote
  memory exhaustion in `quinn-proto` 0.11.14 from unbounded out-of-order
  stream reassembly, reached transitively. Both fixes were on `main` but
  unreleased until now, so installs from crates.io and the Homebrew tap
  carried the affected versions.

## [0.17.0] — 2026-06-07

### Removed

- **`--archive-orphans` is gone** (on both `apply` and `diff`), along with
  its plan-scope field and plan-lock comparison. After verifying both
  affected resource kinds against the Braze API, the rename-based archival
  was non-functional for content blocks (Braze locks the name after
  activation) and unsafe for email templates: `POST /templates/email/update`
  renames unconditionally, `/templates/email/info` exposes no usage data,
  and API-triggered campaigns resolve templates by name at send time, so a
  rename could silently break live sends. The real prune path
  (`--allow-destructive` for catalogs, catalog fields, and custom-attribute
  deprecation) is unaffected. Fixes #65.

### Changed

- **Orphan reporting is now always on.** `diff` and `apply` print a
  read-only notice — "N Braze resource(s) not present in Git. Archive them
  in the Braze dashboard if intended, or add them to exclude_patterns to
  keep them." — across all resource kinds, instead of only when the removed
  `--archive-orphans` flag was passed. Retire an orphan manually in the
  Braze dashboard, or add it to `exclude_patterns`. Orphans still count as
  drift (so `diff --fail-on-drift` exits non-zero on them), but are no
  longer treated as actionable apply work: an orphan-only `apply` reports
  "no actionable changes" instead of a misleading "Applied 0 change(s)".

## [0.16.5] — 2026-05-28

### Fixed

- **`apply --archive-orphans` no longer aborts on orphan content blocks.**
  Braze rejects renaming a content block after activation (`HTTP 400:
  "Content Block name cannot be changed after activation."`), so the
  rename-based archival aborted the whole apply on the first activated
  orphan — and since Braze has no cross-resource transaction, earlier
  writes in the same run were left applied. Fixes #62.

### Changed

- **Content block orphans are now report-only.** `--archive-orphans`
  cannot rename content blocks (Braze locks the name after activation,
  and `/content_blocks/info` exposes no state field to tell draft from
  active ahead of time), so content block orphans are listed for manual
  removal in the Braze dashboard and `apply` exits 0 instead of issuing
  a rename Braze rejects. Email template orphan archival is unchanged —
  Braze allows post-activation renames there.

## [0.16.4] — 2026-05-28

### Fixed

- **`export` now templatizes new resources without a local template.**
  Previously `export` only reverse-templatized remote bodies when a local
  file with `__BRAZESYNC__` placeholders already existed; brand-new
  resources (no local file yet) were written with raw `lid`/`cb_id`
  values, causing spurious drift on subsequent diff cycles until a manual
  templatize. `templatize_body` is now applied unconditionally for new
  resources; the only opt-out is an existing local file that deliberately
  contains no placeholders. Fixes #59.

- **Email template preheader is now templatized when locally absent.**
  A local template with no `preheader` field previously skipped
  templatization of the remote preheader, leaving raw `lid` values on
  disk.

## [0.16.3] — 2026-05-28

### Fixed

- **`lid` regex now accepts digit-leading values.** The first character
  class in both `lid_match_re` and `lid_value_re` was widened from
  `[a-z]` to `[a-z0-9]` so Braze-generated `lid` values that start with a
  digit (e.g. `275ua26snuk7`) are matched correctly. The `__BRAZESYNC__`
  placeholder remains excluded, preserving idempotency. Fixes #56.

## [0.16.2] — 2026-05-27

### Fixed

- **`export` now captures Dashboard HTML edits on templated resources.**
  Previously, when a local file contained `__BRAZESYNC__` placeholders,
  `export` preserved the local body verbatim — silently discarding any
  surrounding HTML changes made in the Braze Dashboard. Now the remote
  body is reverse-templatized (lid / cb_id values rewritten back to
  `__BRAZESYNC__`) before saving, so Dashboard edits are captured while
  runtime-volatile identifiers do not produce spurious drift. Fixes #54.

- **`lid` regex broadened to match fallback slugs.** Both
  `lid_match_re` (templatize) and `lid_value_re` (correlation) now
  accept short names and underscores (`[a-z][a-z0-9_]*`), aligning with
  fallback slug generation introduced in v0.16.1.

- **Positional lid warning relaxed.** The resolver now only warns when
  the remote body has *more* lid values than local placeholders (extra
  values are dropped); fewer remote values is handled gracefully via
  the existing fallback mechanism without a warning.

## [0.16.1] — 2026-05-25

### Changed

- **Unmatched `lid` placeholders now resolve via fallback instead of
  aborting.** When the local template carries more `lid` placeholders
  than the remote body has `lid` values (e.g. a `footer` content
  block has 9 links in Prod structure but only 5 in Dev), the
  unmatched placeholders now resolve to the URL path-tail slug —
  identical to the new-resource path — and a warning is emitted per
  unmatched anchor. `diff` then surfaces a meaningful structural
  diff and `apply` POSTs the full template so the remote gains the
  missing links; Braze reassigns the lid value on first dashboard
  save. The subject/preheader positional resolver applies the same
  fallback when the remote yields fewer lid values than the
  template. The `ResolutionError::UnresolvedLid` variant is retained
  for the remaining failure mode — a lid placeholder with no URL
  anchor at all. Fixes #52.

- **`diff` and `apply` now print a "Notice" block summarizing any
  drift-fallback `lid` assignments**, scoped per resource (and field
  for email_template), with the URL anchor → fallback value mapping.
  Surfaces what would otherwise hide inside warning noise so an
  operator running blind `apply --confirm` still sees which links the
  local template introduced that the remote didn't carry.
  Brand-new-resource fallbacks are intentionally not listed (they're
  the expected path).

### Fixed

- **Fallback `lid` values can no longer collide with remote-resolved
  values.** Previously, if the remote happened to assign lid
  `'checkout'` to one URL and the local template introduced a new
  `/checkout` link, the fallback URL-slug `'checkout'` would land in
  the POSTed body alongside the real one — corrupting Braze's
  per-link analytics. The fallback generator now seeds its dedupe
  map with every remote lid value so collisions get the standard
  `_2`, `_3`, … suffix.

## [0.16.0] — 2026-05-25

### Changed (breaking)

- **Placeholder syntax simplified to anonymous `__BRAZESYNC__`**. The
  v0.15 form `__BRAZESYNC.<type>.<key>__` is removed. Type (lid vs
  cb_id) is inferred from the surrounding `| lid:` / `| id:` filter
  context, and the key is no longer needed (correlation has always
  been done by URL / `${NAME}` / positional — the key was an internal
  identifier with no role in matching).

  ```liquid
  # Before (v0.15)
  | lid: '__BRAZESYNC.lid.spring_sale__'
  | id:  '__BRAZESYNC.cb_id.promo_banner__'

  # After (v0.16)
  | lid: '__BRAZESYNC__'
  | id:  '__BRAZESYNC__'
  ```

  **Migration**: re-run `braze-sync templatize` on each workspace
  before upgrading. The old envelope is detected and surfaced as a
  fatal `RetiredNamespace` error so partial migrations fail loudly
  rather than ship broken templates. No `--modernize` / dual-parser
  compat path is provided.

- **New-resource lid fallback** now derives from the URL path tail
  (e.g. `https://example.com/spring-sale` → `spring_sale`) rather than
  from the placeholder key. URL-less placeholders fall back to
  positional `lid_1`, `lid_2`, …

- **`DuplicateLidKey` error removed.** Anonymous placeholders have no
  keys, so the per-key duplicate detection no longer applies.
  Ambiguous URL anchors still produce a positional-FIFO warning.

### Removed

- `Placeholder.key`, `PlaceholderType::parse`,
  `ResolutionError::UnknownKey`, `ResolutionError::DuplicateLidKey`,
  `LookupKey`, `resolve_placeholders`. Resolution is now offset-based
  and lives entirely in `crate::values::braze_managed::prepare_field`.

## [0.15.0] — 2026-05-25

### Changed

- **`lid` / `cb_id` placeholders are now resolved at apply/diff time from
  the live remote body**, not from `values/<env>.yaml`. The local Git
  body carries `__BRAZESYNC.lid.<key>__` / `__BRAZESYNC.cb_id.<key>__`;
  on every `apply` / `diff` braze-sync GETs the remote body and
  correlates by URL anchor (lid) / `${NAME}` (cb_id). Dashboard edits
  that reassign `lid` / `cb_id` are invisible to diff — only real
  template structure changes show up.
- New resources (not yet in Braze) use a controlled fallback: `lid`
  resolves to the placeholder key itself; the `| id: '__…__'` filter
  is stripped from `cb_id` so the POST carries the documented
  `{{content_blocks.${NAME}}}` form.
- **Subject / preheader `lid` placeholders are now resolved
  positionally** (Nth template placeholder ↔ Nth remote `| lid: '…'`
  value). Previously these failed at resolve time because the
  anchor-based correlator had no URL to match on. `templatize` now
  emits `subject_lid` / `preheader_lid` keys for these fields.
- **Same-URL ambiguity warning.** When a URL has multiple remote `lid`
  occurrences *and* multiple template placeholders, the resolver emits
  a warning so a dashboard-side link reorder cannot silently
  miscorrelate values.
- **Resolve-time diagnostics are now surfaced.** Warnings collected by
  the runtime resolver (URL anchor not found, cb_id filter stripped
  for a new resource, positional count mismatch) are written to stderr
  scoped by resource and field instead of being silently discarded.

### Removed

- **`values/<env>.yaml` is no longer read or written.** The v0.14
  per-env values mechanism — including `lid` / `cb_id` / `custom` /
  `global` entries, `globals.custom`, `environments.<env>.values_file`,
  the pre-flight values gate, and the `values_input_hashes` field on
  plan files — is gone. Existing values files can be deleted. The
  `__BRAZESYNC.custom.*__` / `__BRAZESYNC.global.*__` placeholder
  types are no longer recognized; use literal values until a future
  release reintroduces user-managed namespaces.
- `braze-sync export` no longer touches values files. Its only effect
  on placeholder-bearing resources is to keep the local body verbatim.
- `braze-sync templatize` no longer generates canonical / skeleton
  values files — it is now a pure body rewrite. The `--from-env` flag
  is removed (templatize is env-agnostic).
- `environments.<env>.values_file` config field is removed. Existing
  configs with this field still parse (unknown fields are now accepted
  on `EnvironmentConfig`) but the field has no effect.

## [0.14.3] — 2026-05-24

### Fixed

- **Resolver no longer rejects placeholders whose key ends in `_`.**
  When `templatize` produced a fallback key like `link_` (URL slug
  empty) the rendered envelope collapses to three consecutive
  underscores (`__BRAZESYNC.lid.link___`). The previous parser
  anchored on the *nearest* closing `__` and decided the key was
  `link`, leaving an orphan `_` and failing resolution with "key not
  in values". The parser now applies a narrowly-scoped recovery for
  the two v0.14.2 empty-slug fallback shapes — `lid.link_` and
  `cb_id.cb_` — extending the key by the one trailing `_` when the
  envelope is followed by a single `_` (not `__`). Recovery is gated
  on the exact `(type, key)` pair so hand-written bodies like
  `__BRAZESYNC.custom.foo___bar` are not silently mutated into
  key=`foo_`. Two adjacent placeholders separated by exactly `____`
  (e.g. `__BRAZESYNC.lid.foo____BRAZESYNC.lid.bar__`) still parse as
  two distinct placeholders.

### Changed

- **`templatize` no longer emits keys that end in `_`.** The empty-
  slug fallback now yields `link` (was `link_`) and the empty
  cb_id-slug fallback yields `cb` (was `cb_`); collision suffixes
  remain `_2`, `_3`, … so subsequent occurrences become `link_2`
  etc. with no trailing underscore. New templatize runs avoid the
  ambiguous `___` envelope shape entirely.

  Existing templates with `link_` placeholders continue to resolve
  thanks to the parser fix above. To migrate, either re-run
  `braze-sync templatize` on the affected resources (values yaml
  must be regenerated; dev values can be repopulated via `export`)
  or hand-rename `link_` → `link` (and `link__2` → `link_2`, etc.)
  in both the template body and the values file.

## [0.14.2] — 2026-05-24

### Fixed

- **`templatize`: anchor detection now covers VML, SVG, and other
  namespaced/custom elements.** v0.14.1 generalized the lid scan to
  see inside an enclosing tag's `href`, but it still required that
  tag to be `<a>`. Outlook-compatible email content blocks wrap their
  CTAs in VML (`<v:roundrect href="…">`), and SVG anchors use
  `<svg:a xlink:href="…">` — lids inside these still fell back to a
  sequential `link_N` key. The enclosing-tag scan now matches any
  element open tag and recognizes URL-bearing attributes (`href`,
  `src`, `action`) with or without a namespace prefix
  (`xlink:href`, `v:href`, …). The legacy `<a>`-specific prefix-scan
  fallback for the "lid as link text" pattern is unchanged. Re-run
  `braze-sync templatize` on affected resources to migrate any
  remaining `link_N` placeholders to URL-derived keys.

## [0.14.1] — 2026-05-24

### Fixed

- **`templatize`: anchor detection now sees lids inside `href`
  attribute values.** Braze's default HTML output puts the lid query
  parameter — and its `| lid: '…'` token — *inside* the anchor's
  `href` (e.g. `<a href="…/path/?lid={{${cblid} | lid: 'X'}}">`).
  The prefix-only scan couldn't see the closing quote of the
  enclosing tag and was silently falling back to sequential
  `link_N` keys for nearly every anchor in real Braze projects.
  Detection now first looks for an enclosing `<a …>` open tag and
  uses its `href` as the URL anchor; the legacy "lid sits between
  `<a>` and `</a>` as link text" pattern remains supported as a
  fallback. Existing `__BRAZESYNC.lid.link_N__` placeholders from
  v0.14.0 templatize runs are not rewritten automatically — re-run
  `braze-sync templatize` on affected resources to migrate them to
  URL-derived keys.

## [0.14.0] — 2026-05-24

### Added

- **Per-env values: template + values separation.** Resource bodies in
  Git become env-agnostic templates that reference per-env values via
  `__BRAZESYNC.<type>.<key>__` placeholders, resolved at apply time
  against `values/<env>.yaml`. `<type>` ∈ `lid` | `cb_id` | `custom` |
  `global`; the double-underscore-plus-dot envelope is verbatim-safe in
  Liquid, HTML, and JSON contexts. `lid` and `cb_id` are field-scoped
  on email_template (a per-occurrence ID can't span fields); `custom`
  is resource-scoped; `global` reads from a shared `globals.custom`
  namespace so values like `api_host` aren't duplicated across every
  resource. See [`docs/per-env-values.md`](docs/per-env-values.md).
- **`braze-sync templatize --from-env=<env>`.** One-shot migration
  that walks every local resource, rewrites raw `lid` literals and
  content_block include IDs into placeholders, writes the canonical
  env's `values/<env>.yaml`, and generates `value: null` skeleton
  files for every other configured environment. Idempotent on bodies
  that already contain placeholders; pair with `--dry-run` to preview.
- **Pre-flight values resolution.** `apply`, `diff`, and `export`
  resolve placeholders against `values/<env>.yaml` before any HTTP
  write. Failures are aggregated across every selected resource and
  reported in one shot (Terraform-style) — apply exits with **zero**
  Braze API calls if any placeholder is unresolved. A separate
  `WARN:` line surfaces envelope-shaped typos (`__BRAZSYNC.lid.foo__`,
  `__BRAZESYNC.url.foo__`) so they can't pass silently.
- **`export` correlation.** `export` no longer overwrites templated
  bodies; instead it refreshes the matching `values/<env>.yaml`
  entries, using HTML `<a href>` URLs, plaintext bare URLs, subject /
  preheader Liquid-identifier anchors, and content_block-include
  `${NAME}` syntactic anchors to keep key↔value pairing stable across
  commits. Orphan keys (no placeholder references them) are flagged
  with warnings rather than auto-deleted; ambiguous URL/anchor
  matches fall back to source order with a warning.
- **Plan-lock integration.** `diff --plan-out` now records a
  per-resource blake3 hash over the values subset that resource
  actually consumes; `apply --plan` recomputes it and exits **7**
  (`PlanDrift`) if the values file was edited for any plan-frozen
  resource between plan generation and apply. Body-only edits that
  don't change the placeholder set still pass — the v0.13 plan-lock
  tolerance for benign body edits is preserved. A `globals.custom`
  edit invalidates every consumer; regenerate the plan.
- **`environments.<env>.values_file` config field.** Optional path
  override; defaults to `values/<env_name>.yaml` relative to the
  config directory.

### Migration / breaking notes

- Repos with multi-env Braze workspaces and dashboard-edited `lid`
  values should run `braze-sync templatize` once before the first
  v0.14 apply, otherwise `apply --env=prod` will keep pushing the
  raw `lid` from Git over Prod's real value. The flow is documented
  in [`docs/per-env-values.md`](docs/per-env-values.md).
- Repos without any `__BRAZESYNC.` placeholder are unaffected — the
  resolver is a no-op when no template references it. The
  `feat-preserve-remote-patterns` proposal is superseded by this work.

## [0.13.0] — 2026-05-18

### Added

- **Plan/apply lock.** `diff --plan-out=<path>` writes a JSON plan file
  freezing the actionable op set (`{kind, name, op}` tuples plus the
  scope: environment, `--resource`, `--name`, `--archive-orphans`
  intent). `apply --plan=<path>` rejects scope mismatches before any
  Braze API call, then re-checks the freshly-computed ops against the
  saved set after reorder; any mismatch exits with new code **7**
  (`Error::PlanDrift`) and fires zero writes. Comparison is multiset
  equality on `(kind, name, op_type)` — payload-level edits between
  plan and apply are deliberately tolerated so benign content tweaks
  don't trip false positives (see `docs/local/feat-apply-plan-locking.md`
  §3.2). Plans older than 24 h or generated by a different braze-sync
  version produce stderr warnings but do not block apply. Motivation:
  prevents the failure mode where a stale `apply` overwrites dashboard
  edits accumulated since the PR plan was reviewed.

## [0.12.0] — 2026-05-16

### Added

- **Catalog schema deletion.** `apply` now drops a catalog from Braze
  when its directory is removed locally, via the synchronous
  `DELETE /catalogs/{name}` endpoint. The top-level delete short-
  circuits the per-field DELETE loop because one call drops the
  schema and all items at once. Among managed resources this was the
  last destructive endpoint Braze exposes that braze-sync had not yet
  wrapped — content_block and email_template have no DELETE API on
  Braze's side, and custom_attribute / tag have no per-entity delete
  surface at all. The existing destructive-op gate is reused, so
  `--allow-destructive` is required in addition to `--confirm`; a 404
  from Braze is surfaced as drift (`Http { status: 404, .. }`) rather
  than silently treated as success, matching `delete_catalog_field`.

## [0.11.0] — 2026-05-05

### Added

- **Topological apply order for content blocks.** When a local content
  block body references another via the Liquid include syntax
  `{{content_blocks.${other} | id: '...'}}`, `apply` now creates the
  target block before its referrer. Pre-v0.11 behavior sorted by name
  alphabetically and broke on a fresh workspace whenever a referrer
  sorted before its target — Braze validates the body at create time
  and rejects forward references with an opaque HTTP 500, halting the
  whole apply pass mid-pipeline. The new ordering pass parses
  references out of every actionable diff body, builds a
  `referrer → target` graph limited to blocks in the actionable set
  (references to already-present blocks are treated as no-op edges, so
  the referrer becomes a leaf), topologically sorts so targets precede
  referrers, and aborts before any HTTP write if it detects a cycle —
  surfacing the cycle path with named blocks so the operator can fix
  the loop instead of decoding a 500. The dry-run plan is emitted in
  the chosen order, so `apply` (without `--confirm`) previews the
  exact write sequence.
- **`apply_order` config field on `ResourceConfig`.** Default
  `dependency` (the new behavior). Set
  `resources.content_block.apply_order: alphabetical` to opt back into
  the v0.10 ordering — useful for repos that have built tooling around
  the old apply sequence and don't yet have inter-block references.
  The field is consulted only by content_block apply; setting it on
  other resource kinds is accepted but inert.

## [0.10.0] — 2026-05-04

### Added

- **`tag` resource kind, GitOps-only.** Workspace tags are now tracked
  as a first-class registry at `tags/registry.yaml`. Braze does not
  expose a public REST API for workspace tags (no list, create, update,
  or delete — verified against the Braze API documentation), so the
  registry is derived from local resource references rather than a
  remote pull. Four CLI integrations:
  - `export --resource tag` aggregates tags from local content_block
    and email_template frontmatter into the registry. Run regular
    `export` first to refresh the resource files, then `export tag` to
    rebuild the registry.
  - `validate --resource tag` cross-checks the registry against every
    tag referenced by local resources. Any tag referenced without a
    matching registry entry fails validation (exit 3) with an
    actionable message.
  - `diff --resource tag` reports the symmetric drift between the
    registry and observed references (`referenced_but_unregistered` /
    `registered_but_unreferenced`).
  - `apply` adds a tag pre-flight that runs on every kind: if a
    to-be-created/updated resource references a tag missing from the
    registry, apply aborts before issuing any write — with a list of
    missing tags and the resources that reference them. This prevents
    the cascading "Tags could not be found" 400s that previously
    halted apply at the first tagged content_block create on a fresh
    target environment.

  Disabled-by-default unless declared in the config. `braze-sync init`
  now scaffolds a `tags/` directory and declares the resource in the
  generated config. See `docs/local/feat-tag-management.md` for the
  design notes.

## [0.9.2] — 2026-05-03

### Fixed

- **Catalog creation no longer fails with `id-not-first-column` on
  freshly-exported workspaces.** `POST /catalogs` rejects bodies whose
  `fields[0].name` is not `id`, but exported `schema.yaml` files
  alphabetize fields, so any catalog whose id was not first
  alphabetically failed to create on a clean target environment.
  `Catalog::normalized()` now hoists the `id` field to position 0 and
  alphabetizes the rest; both the wire payload and the on-disk
  `schema.yaml` share this ordering, so export ↔ apply stays
  round-trip stable. (#26)

## [0.9.1] — 2026-05-01

### Fixed

- **`apply` no longer aborts on registry-only custom attributes.** A
  custom attribute present in the local registry but missing from
  Braze (`PresentInGitOnly`) is now treated as informational drift,
  matching how `diff` already reports it. Previously `apply --confirm`
  hard-errored with `Custom Attribute '...' cannot be created via
  API`, which blocked all other resource changes (content blocks,
  catalog schemas, email templates) in the same run. Braze has no
  creation endpoint for custom attributes — they materialize on the
  first `/users/track` call — so registry-only entries are an expected
  state, especially right after `export` from a higher environment.
  The plan-print still surfaces the `⚠ in Git registry but not in
  Braze (likely a typo)` warning, but the run no longer exits
  non-zero. (#24)

## [0.9.0] — 2026-04-27

### Added

- **Catalog creation in `apply`.** A new catalog directory committed to
  Git (`catalogs/<name>/schema.yaml`) is now created in Braze on
  `apply --confirm` via `POST /catalogs`, including its initial fields
  and `description`. Previously `apply` hard-errored and required the
  catalog to be created in the Braze dashboard first. Catalog
  **deletion** is still not supported — see the Limitations section in
  the README.

### Migration

- API keys used by `apply` need the `catalogs.create` permission in
  addition to the existing `catalogs.create_fields` /
  `catalogs.delete_fields`. CI keys that only had field-level
  permissions will now fail on the first new-catalog apply.

## [0.8.0] — 2026-04-19

### Breaking changes

- **Removed `catalog_items` support.** braze-sync now exclusively
  manages Braze configuration (schemas, content blocks, email
  templates, custom attribute registry). Catalog items are runtime
  data and are out of scope — see
  [`docs/scope-boundaries.md`](docs/scope-boundaries.md).
- **Removed the client-side rate limiter.** The `governor` dependency
  is gone; braze-sync now reacts to 429 + `Retry-After` instead of
  pre-throttling. The 429 retry loop uses a time budget + exponential
  backoff with full jitter, and honors `Retry-After` as integer
  seconds or HTTP-date.
- **Config hard-errors on removed keys.** Configs that still carry
  `defaults.rate_limit_per_minute`, `environments.<env>.rate_limit_per_minute`,
  or a `resources.catalog_items:` section will fail to load.

### Migration

**If you were syncing catalog items:** use the Braze REST API
(`/catalogs/{name}/items`) directly from your data pipeline, Cloud
Functions, or a dedicated ETL job. Remove the `catalog_items:` section
from `braze-sync.config.yaml`. Any `catalogs/*/items.csv` files on
disk are no longer read or written — delete them.

**If your config specified `rate_limit_per_minute`:** delete the key.
braze-sync no longer throttles proactively; Braze's own 429 signal is
the only pacing mechanism.

### Added

- `custom_attribute` now uses the Braze-verified wire schema
  (`attributes`/`name`/`status`) and follows RFC 5988 `Link: rel="next"`
  pagination through every page. Fixes `exported 0 attribute(s)` on
  workspaces where attributes carried suffixed type strings like
  `"String (Automatically Detected)"` or `status: "Blocklisted"`.
- `content_blocks/list` and `templates/email/list` now use offset
  pagination with `limit=1000` (the Braze documented max). Workspaces
  with more than 100 entries no longer hard-error on `diff`/`apply`.
- `CustomAttributeType::Object` / `ObjectArray` domain variants for
  the `Object` and `Object Array` types that Braze returns in practice.
- `exclude_patterns: [<regex>, ...]` on every resource config. Names
  matching any pattern are skipped by `export`, `diff`, `apply`, and
  `validate`, so Braze-reserved attributes (`_unset`), developer
  leftovers (`hoge`, `hack`), and legacy camelCase duplicates stop
  surfacing as drift. Patterns compile at config load time (bad regex
  → hard error before the command runs).
- New `docs/scope-boundaries.md` — canonical "configuration vs runtime
  data" reference.

## [0.7.0] — 2026-04-19

### Added

- Public documentation under `docs/` for configuration, CI integration,
  orphan tracking, and Custom Attribute registry mode. README now links
  to each page under "Further reading".
- `cargo-deny` wired into CI with a conservative `deny.toml`: permissive
  OSS license allow-list, wildcard-dependency ban, and
  allowed-registry enforcement. Complements the existing `cargo audit`
  job.
- Release artifacts are now signed with Sigstore cosign in keyless
  mode. Each `.tar.gz` / `.zip` ships alongside a `.cosign.bundle`
  verifiable against the release workflow's OIDC identity. See
  README → "Verifying release artifacts".
- Release workflow now updates `uny/homebrew-tap/Formula/braze-sync.rb`
  automatically on stable tags (pre-release tags like `vX.Y.Z-rc.N` are
  skipped). Authentication uses a dedicated GitHub App
  (`uny-release-bot`) with `Contents: Write` scoped to the tap repo,
  minting a short-lived installation token per run instead of a
  long-lived PAT.

### Changed

- Linux release builds switched from `*-unknown-linux-gnu` to
  `*-unknown-linux-musl` for fully static binaries. Matches the
  target list in IMPLEMENTATION.md §13 Phase C6 and removes the
  runtime glibc floor. rustls-only TLS means no openssl dependency
  to pull in.

## [0.6.0] — 2026-04-17

### Added

- `braze-sync init` — scaffolds a new workspace with a commented
  `braze-sync.config.yaml`, resource directories (`catalogs/`,
  `content_blocks/`, `email_templates/`, `custom_attributes/`), and
  `.gitignore` entries for `.env` files. Idempotent on directories and
  `.gitignore`; requires `--force` to overwrite an existing config.
- `braze-sync init --from-existing` — scaffolds then immediately runs
  `export` against the configured environment, populating the layout
  with current Braze state in a single command. Keeps an already-edited
  config rather than overwriting it, so it can be run after the
  operator has pointed the endpoint at their instance.

## [0.5.0] — Phase B4

Custom Attribute end-to-end (registry mode: read + diff + deprecation
toggle only; create is intentionally unsupported — see
IMPLEMENTATION.md §2.2).

## [0.4.0] — Phase B3

Catalog Items end-to-end with CSV streaming and blake3 content-hash
diffs.

## [0.3.0] — Phase B2

Email Template end-to-end with per-part diffs (subject / body_html /
body_plaintext / metadata) and orphan tracking (Braze has no DELETE).

## [0.2.1]

Catalog list pagination fail-closed; crates.io auto-publish CI.

## [0.2.0] — Phase B1

Content Block end-to-end with orphan tracking and `--archive-orphans`.

## [0.1.0] — Phase A

Catalog Schema end-to-end across all four core commands: `export`,
`diff`, `apply`, `validate`.
