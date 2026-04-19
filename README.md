# braze-sync

GitOps CLI for managing Braze configuration as code.

`braze-sync` lets you keep Braze workspace state in a Git repository and
synchronize it to Braze with the same workflow you'd use for
`terraform plan` / `kubectl diff` — including dry-run previews, drift
detection in CI, and an `--allow-destructive` gate that has to be
crossed explicitly before anything is dropped.

## Status: v0.6.0 (all 5 resources + init)

All five v1.0 resource kinds are implemented end-to-end: **Catalog
Schema**, **Catalog Items**, **Content Block**, **Email Template**, and
**Custom Attribute** (registry mode).

| Command | What it does |
|:---|:---|
| `braze-sync init` | Scaffolds a new workspace (config, directories, `.gitignore`) |
| `braze-sync export` | Pulls current Braze state into local files |
| `braze-sync diff` | Shows drift between local files and Braze |
| `braze-sync apply` | Applies local intent to Braze (dry-run by default) |
| `braze-sync validate` | Local-only structural and naming checks (no API call) |

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
cargo install braze-sync --locked
```

`--locked` is recommended. Without it, `cargo install` ignores the
published `Cargo.lock` and resolves transitive dependencies to the
newest semver-compatible versions, some of which bump their MSRV
beyond this crate's (currently 1.86) and fail to build on older
toolchains.

**Build from source:**

```bash
cargo install --path . --locked
```

## Quick start

1. Scaffold a new workspace (config, directories, `.gitignore`):

   ```bash
   braze-sync init
   ```

   This writes a commented `braze-sync.config.yaml` (pointing at the
   EU default endpoint — edit if your instance is elsewhere) plus
   empty `catalogs/`, `content_blocks/`, `email_templates/`, and
   `custom_attributes/` directories. Safe to re-run: existing configs
   are kept unless `--force` is passed.

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

## Limitations

These will be lifted across the v0.x → v1.0 milestones:

- **No catalog create / delete.** v0.6.0 manages fields on existing
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

## License

[MIT](LICENSE)
