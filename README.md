# braze-sync

GitOps CLI for managing Braze configuration as code.

`braze-sync` lets you keep Braze workspace state in a Git repository and
synchronize it to Braze with the same workflow you'd use for
`terraform plan` / `kubectl diff` — including dry-run previews, drift
detection in CI, and an `--allow-destructive` gate that has to be
crossed explicitly before anything is dropped.

## Status: v0.2.0 (Catalog Schema + Content Block)

v0.2.0 ships **Catalog Schema** and **Content Block** end-to-end:

| Command | What it does |
|:---|:---|
| `braze-sync export` | Pulls current Braze state into local files |
| `braze-sync diff` | Shows drift between local files and Braze |
| `braze-sync apply` | Applies local intent to Braze (dry-run by default) |
| `braze-sync validate` | Local-only structural and naming checks (no API call) |

Three other resource kinds (Email Template, Catalog Items, Custom
Attribute) are visible in `--resource` and emit a "not yet implemented
(Phase B)" warning. They fill in across v0.3.0 → v0.5.0.

### Content Block specifics

Content Blocks live as `content_blocks/<name>.liquid` files: YAML
frontmatter (name, description, tags, state) followed by the Liquid
body. `braze-sync apply` can create new blocks and update existing
ones, but **the Braze API has no DELETE for content blocks**, so blocks
that exist in Braze but not in Git become *orphans* — `diff` flags
them and `apply` does nothing about them by default. Pass
`--archive-orphans` to rename them remotely with an
`[ARCHIVED-YYYY-MM-DD]` prefix; the data is never silently dropped.

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

1. Set your Braze API key in an environment variable:

   ```bash
   export BRAZE_DEV_API_KEY="your-key-here"
   ```

2. Create `braze-sync.config.yaml`:

   ```yaml
   version: 1
   default_environment: dev
   environments:
     dev:
       api_endpoint: https://rest.fra-02.braze.eu
       api_key_env: BRAZE_DEV_API_KEY
   ```

3. Pull the current state from Braze:

   ```bash
   braze-sync export
   ```

   This writes `catalogs/<name>/schema.yaml` for every Catalog Schema in
   your workspace.

4. Edit a schema (e.g. add a field) and check the drift:

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
to write to Braze. Destructive operations (field deletes) require an
additional `--allow-destructive` flag — `apply` exits with code **6**
if you try to drop a field without it.

```bash
braze-sync apply --confirm                     # add fields ok, drop fields → exit 6
braze-sync apply --confirm --allow-destructive # field drops permitted
```

API keys never live in the config file. The config only references the
*name* of the environment variable (`api_key_env`), and the key is
held in `secrecy::SecretString` from the moment it leaves the OS so
that `tracing` / `Debug` / panic messages cannot leak it.

## v0.2.0 limitations

These will be lifted across the v0.x → v1.0 milestones:

- **Catalog Schema and Content Block only.** Email Template, Catalog
  Items, and Custom Attribute land in v0.3 → v0.5. They appear in
  `--resource` so the CLI surface stays stable, but selecting one in
  v0.2.0 just emits a "not yet implemented (Phase B)" warning.
- **No catalog create / delete.** v0.2.0 manages fields on existing
  catalogs. To create a brand-new catalog, create it in the Braze
  dashboard first, then run `braze-sync export`.
- **No field type changes.** Changing a field's type from `string` to
  `number` (or similar) is not auto-applied because the operation is
  data-losing on the field. Drop the field manually in Braze, then
  run `braze-sync apply` to re-add it with the new type.
- **No DELETE for content blocks.** Braze's content blocks API does
  not expose a DELETE endpoint, so blocks that exist in Braze but not
  in Git become *orphans*. `diff` flags them; `apply` does nothing
  about them unless you pass `--archive-orphans`, which renames them
  remotely with an `[ARCHIVED-YYYY-MM-DD]` prefix instead of pretending
  they were dropped.
- **Content block `state` is local-only.** The `state: active|draft`
  field in `content_blocks/<name>.liquid` frontmatter is parsed and
  round-tripped, but Braze's content_blocks API does not expose state,
  so braze-sync's diff intentionally ignores it. `apply` writes the
  field exactly once — when *creating* a new block — and never sends
  it on updates, so editing `state` in a file that already exists on
  Braze has no effect. Treat it as a documentation aid for the file's
  reader rather than a syncable property.
- **No pagination yet.** v0.2.0 sends a single page request to
  `/catalogs` and `/content_blocks/list` (limit 100). For
  `/content_blocks/list` this is a **hard error** if Braze reports more
  results than fit on one page, or if a full page comes back with no
  total to verify against — workspaces with >100 content blocks cannot
  use v0.2.0 yet. Without the guard, `apply` could create duplicates of
  blocks living on page 2+ (their names would diff as `Added`). This
  limit is symmetric for `--name <foo>`: content blocks have no
  get-by-name endpoint, so `diff --name`, `apply --name`, and
  `export --name` still list-then-filter and hit the same page cap.
  For `/catalogs` v0.2.0 still only warns; the same guard will be
  applied symmetrically in a follow-up. Pagination support lands in
  Phase C scale validation.
- **`--archive-orphans` is a two-step read-modify-write.** The rename
  fetches `/content_blocks/info` to preserve the body, then posts
  `/content_blocks/update` with the archived name. If another operator
  edits the same block in the dashboard between those two calls, the
  update clobbers their change with the pre-rename body. Safe for the
  single-operator GitOps workflow v0.2.0 targets; a compare-and-swap
  header would lift it, but Braze's content_blocks API does not
  currently document one.
- **`--no-color` only affects tracing output.** v0.2.0 does not emit
  ANSI colors in table or diff output, so the flag currently only
  suppresses ANSI escapes from the tracing subscriber on stderr.

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

## License

[MIT](LICENSE)
