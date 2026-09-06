# braze-sync

GitOps CLI for managing Braze configuration as code.

`braze-sync` lets you keep Braze workspace state in a Git repository and
synchronize it to Braze with the same workflow you'd use for
`terraform plan` / `kubectl diff` — including dry-run previews, drift
detection in CI, and an `--allow-destructive` gate that has to be
crossed explicitly before anything is dropped.

## Status: 5 resources + init + Braze-managed placeholder resolution

braze-sync manages Braze **configuration** as code. The five managed
resource kinds are:

- **Catalog Schema** (field definitions, types, constraints)
- **Content Block** (reusable Liquid fragments)
- **Email Template** (HTML/Liquid templates)
- **Custom Attribute** registry (definition-level; deprecation toggle)
- **Tag** registry (workspace tags referenced by other resources;
  derived from local frontmatter because Braze exposes no tag API)

**Out of scope**: runtime data like catalog **items**, user attribute
values, events, and campaigns. Those have their own systems of record;
use the Braze REST API or data pipelines directly. See
[docs/scope-boundaries.md](docs/scope-boundaries.md).

| Command | What it does |
|:---|:---|
| `braze-sync init` | Scaffolds a new workspace (config, directories, `.gitignore`) |
| `braze-sync export` | Pulls current Braze state into local files |
| `braze-sync diff` | Shows drift between local files and Braze |
| `braze-sync apply` | Applies local intent to Braze (dry-run by default) |
| `braze-sync validate` | Local-only structural and naming checks (no API call) |
| `braze-sync templatize` | One-shot migration: rewrite raw `lid` / `cb_id` to placeholders (see [docs/per-env-values.md](docs/per-env-values.md)) |

### Content Block specifics

Content Blocks live as `content_blocks/<name>.liquid` files: YAML
frontmatter (name, description, tags, state) followed by the Liquid
body. `braze-sync apply` can create new blocks and update existing
ones, but **the Braze API has no DELETE for content blocks**, so blocks
that exist in Braze but not in Git become *orphans* — `diff` and `apply`
report them but never mutate them. Retire an orphan by archiving it in
the Braze dashboard, or add it to `exclude_patterns` to keep it. The
data is never silently dropped. The same report-only policy applies to
email template orphans — see `docs/orphan-tracking.md`.

When a block body references another block via the Liquid include
syntax `{{content_blocks.${other} | id: '...'}}`, `apply` topologically
sorts so the referenced block is created before the referrer. Without
this, Braze rejects forward references at create time with an opaque
HTTP 500. Cycles abort the apply with a named-blocks error before any
write fires. Set `resources.content_block.apply_order: alphabetical` to
restore pre-v0.11 ordering.

## Install

**Pre-built binaries** (recommended):

Download from [GitHub Releases](https://github.com/uny/braze-sync/releases/latest)
for Linux (x86_64, aarch64), macOS (x86_64, Apple Silicon), and Windows (x86_64).

**Homebrew** (macOS / Linux):

```bash
brew install uny/tap/braze-sync
```

**cargo install** (requires Rust toolchain):

```bash
cargo install braze-sync
```

**Build from source:**

```bash
cargo install --path .
```

## Quick start

1. Scaffold a new workspace (config, directories, `.gitignore`):

   ```bash
   braze-sync init
   ```

   This writes a commented `braze-sync.config.yaml` (pointing at the
   EU default endpoint — edit if your instance is elsewhere) plus
   empty `catalogs/`, `content_blocks/`, `email_templates/`,
   `custom_attributes/`, and `tags/` directories. Safe to re-run:
   existing configs are kept unless `--force` is passed.

2. Set your Braze API key in an environment variable:

   ```bash
   export BRAZE_DEV_API_KEY="your-key-here"
   ```

3. Pull the current Braze state into the scaffolded layout:

   ```bash
   braze-sync export
   ```

   Or do steps 1 and 3 in one shot:

   ```bash
   braze-sync init --from-existing
   ```

4. Edit a resource (e.g. add a catalog field) and check the drift:

   ```bash
   braze-sync diff
   ```

5. Apply the change — dry-run first, then for real:

   ```bash
   braze-sync apply              # dry-run, makes zero write calls
   braze-sync apply --confirm    # actually applies
   ```

6. In CI, fail builds on drift or local validation issues:

   ```bash
   braze-sync validate               # exits 3 if any local file is invalid
   braze-sync diff --fail-on-drift   # exits 2 if Braze drifted from Git
   ```

   `validate` is local-only and **does not need an API key**, so it
   runs cleanly on fork PRs that don't have access to repository
   secrets.

## Safety by default

`braze-sync apply` is **dry-run by default**. You must pass `--confirm`
to write to Braze. Destructive operations (catalog deletes, field
deletes) require an additional `--allow-destructive` flag — `apply`
exits with code **6** if you try to drop a catalog or field without it.

```bash
braze-sync apply --confirm                     # add fields ok, deletes → exit 6
braze-sync apply --confirm --allow-destructive # catalog/field deletes permitted
```

`apply` is **not atomic across resources** — Braze exposes no
cross-resource transaction. Writes are issued one API call at a time and
the run aborts on the first failure, so a rejection mid-plan (e.g. Braze
refusing `update` on a drag-and-drop content block) leaves the earlier
writes live. The aborted run names them, so the applied set is reported
rather than inferred:

```text
✗ apply aborted — partial state left in Braze (no cross-resource rollback):
  applied (20):
    - content_block 'header_ja'
    …
  failed (may or may not have landed):
    - content_block 'promo_dnd': HTTP 400 Bad Request: {"message":"DND Content blocks are not allowed to be updated from the API."}
  not attempted (4):
    - content_block 'footer_ja'
    …
  → the applied writes above are live and are not rolled back.
  → a failed write is not proof that nothing changed: Braze can
    commit a write whose response never reaches the client.
    Run `braze-sync diff` to confirm the current remote state, then
    re-run `apply` with the same flags to pick up the changes that
    were not attempted.
```

Each line is one API call — a catalog's field writes are listed per
field — so nothing can land without being named. `applied` and `not
attempted` are certain; the **failed** write is not, because Braze can
commit a write whose response never reaches the client (every request
carries a timeout). That is why the report still points at `diff` to
confirm. If the *first* write fails there is no earlier write
to roll back, and the run says that instead of claiming a partial
state.

Re-run `apply` with the same flags after fixing (or excluding) the
offending resource to pick up the changes that were not attempted; the
`diff` is recomputed, so the writes that already landed are skipped.
**Exception:** a plan-locked run (`--plan`) must have its plan
regenerated with `diff --plan-out` first — the writes that landed moved
the remote away from what the plan recorded, so re-running
`apply --plan` as-is exits **7** (plan drift) without writing anything.

### What a plan file guarantees

`diff --plan-out` records, for every op that would overwrite a remote
body (`modify`, `destructive_delete`), a digest of the **remote** side as
`diff` observed it; an `add` records the remote's *absence* instead.
`deprecate` / `reactivate` record nothing — they write a boolean whose
expected prior value the op direction already states, so a remote toggle
removes the op from the fresh diff and is caught as op drift. `apply
--plan` re-fetches, and refuses to run if the op set changed *or* if any
of those remote preconditions no longer holds. So a plan authorizes
exactly this:

> Apply the *current* local intent, provided the remote preconditions
> the plan recorded still hold.

It deliberately does **not** freeze the change set: editing a local file
between plan and apply still applies, as long as the op shapes match.
Nor is it concurrency control — Braze's REST API offers no `If-Match`,
so the check happens against apply's own fetch and a lost update remains
possible in the window before the write.

The plan carries digests rather than payloads, so publishing it as a CI
artifact does not disclose content directly — but a digest is not
confidentiality. The projection is an unkeyed, deterministic hash whose
encoding is public in this repo, so anyone holding the artifact can
recompute a guess and confirm it. Resource *names* are in the plan in
cleartext regardless. Treat a plan file as you would any other build
artifact describing your Braze workspace, and do not publish plans that
touch sensitive resources to readers you would not otherwise trust.

API keys never live in the config file. The config only references the
*name* of the environment variable (`api_key_env`), and the key is
held in `secrecy::SecretString` from the moment it leaves the OS so
that `tracing` / `Debug` / panic messages cannot leak it.

## Limitations

These will be lifted across the v0.x → v1.0 milestones:

- **No field type changes.** Changing a field's type from `string` to
  `number` (or similar) is not auto-applied because the operation is
  data-losing on the field. Drop the field manually in Braze, then
  run `braze-sync apply` to re-add it with the new type.
- **No DELETE for content blocks or email templates — orphans are
  report-only.** Neither API exposes a DELETE endpoint, so resources
  that exist in Braze but not in Git become *orphans*. `diff` and
  `apply` report them (with an always-on "not present in Git" notice)
  but never mutate them. Braze also rejects renaming a content block
  after activation, and renaming an email template can break
  API-triggered sends that resolve it by name, so braze-sync does not
  auto-archive either kind. Retire an orphan by archiving it in the
  Braze dashboard, or add it to `exclude_patterns`. See
  `docs/orphan-tracking.md`.
- **Content block `state` is local-only and not observable.** The
  `state: active|draft` field in `content_blocks/<name>.liquid`
  frontmatter is a purely local authoring annotation. Braze's
  `/content_blocks/info` endpoint does not return state, so
  `braze-sync export` writes **no `state:` line** for any block
  fetched from Braze rather than defaulting to `active` and
  pretending it knows. If you want the annotation, add it to the
  file by hand after `export`. `apply` writes the field exactly
  once — when *creating* a new block — and never sends it on
  updates, so editing `state` on a block that already exists in
  Braze has no effect and the next `export` will strip it again.
  The diff layer also ignores the field to prevent an "infinite
  drift" loop (Braze has no DELETE, so a persistently-Modified
  Content Block is a trap).
- **`--no-color` only affects tracing output.** Table and diff output
  do not currently emit ANSI colors, so the flag only suppresses
  ANSI escapes from the tracing subscriber on stderr.

## Exit codes

These are **frozen at v1.0**: scripts and CI configs can rely on them
across all v1.x releases.

| Code | Meaning |
|:---:|:---|
| `0` | Success |
| `1` | General error |
| `2` | Drift detected (`diff --fail-on-drift`) |
| `3` | Config / argument error (or `validate` issues) |
| `4` | Authentication failed (invalid API key) |
| `5` | Rate limit retries exhausted |
| `6` | Destructive change blocked (pass `--allow-destructive`) |
| `7` | Plan/apply mismatch (`apply --plan`: op set differs, or the remote moved since the plan) |
| `8` | Fallback gate (unmatched placeholder + unconsumed remote lid). Unlike `2`, `diff` has no opt-in flag for this — it always exits `8` when the gate fires; `apply` requires `--allow-fallback` |

## Output formats

The global `--format` flag picks between human-readable and
machine-readable output for `diff` and `apply`:

```bash
braze-sync diff --format table   # default — emoji + indented text
braze-sync diff --format json    # frozen v1 schema with `version: 1`
```

The JSON shape is **frozen at v1.0** with an explicit `version: 1`
field on the root. Future schema bumps will increment `version`, so
CI consumers can branch on it.

On a workspace with hundreds of resources, the table is mostly
`no drift`. `diff --only-drift` keeps just the blocks a reviewer has to
read — useful when CI posts the output into a pull request comment:

```bash
braze-sync diff --only-drift
```

```text
📝 Content Block: promo
   ~ content changed (+5 -3)

✅ Custom Attribute: visit_count
   no drift
   ℹ type mismatch: local number vs Braze string (run export to update)

Summary: 1 changed, 384 in sync, 0 orphan, 0 destructive
```

Nothing is hidden that a reviewer needs: the `Summary:` trailer still
counts every resource, and an in-sync resource that carries an
informational line (like the type mismatch above) is kept. The flag is
table-only — `--format json` always emits every resource.

## Verifying release artifacts

Release archives from [GitHub Releases](https://github.com/uny/braze-sync/releases)
are signed with [Sigstore cosign](https://github.com/sigstore/cosign)
in keyless mode — the signing identity is the release workflow itself,
not a long-lived key. Each `.tar.gz` / `.zip` ships with a `.cosign.bundle`
carrying the signature and Fulcio certificate. To verify, download both
and run:

```bash
cosign verify-blob \
  --bundle braze-sync-<target>.tar.gz.cosign.bundle \
  --certificate-identity 'https://github.com/uny/braze-sync/.github/workflows/release.yml@refs/tags/v<version>' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' \
  braze-sync-<target>.tar.gz
```

A successful run prints `Verified OK`. Any mismatch — tampering,
wrong repo, or a build from a different workflow — fails. The
SHA-256 digests (`.sha256`) are still published for consumers that
only need a content hash.

## Further reading

- [Configuration reference](docs/configuration.md) — every field in `braze-sync.config.yaml`.
- [CI integration](docs/integration.md) — drift detection and apply-on-merge workflows.
- [Orphan tracking](docs/orphan-tracking.md) — how Content Blocks and Email Templates are handled when Braze has no DELETE.
- [Custom Attribute registry mode](docs/registry-mode.md) — why attributes work differently and what `apply` actually does.
- [Braze-managed placeholders](docs/per-env-values.md) — how `lid` / `cb_id` are templatized in Git and resolved at apply/diff time from the live remote body.

## License

[MIT](LICENSE)
