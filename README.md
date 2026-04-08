# braze-sync

GitOps CLI for managing Braze configuration as code.

`braze-sync` lets you keep Braze workspace state in a Git repository and
synchronize it to Braze with the same workflow you'd use for
`terraform plan` / `kubectl diff` — including dry-run previews, drift
detection in CI, and an `--allow-destructive` gate that has to be
crossed explicitly before anything is dropped.

## Status: v0.1.0 (Catalog Schema)

v0.1.0 ships **Catalog Schema** end-to-end:

| Command | What it does |
|:---|:---|
| `braze-sync export` | Pulls current Braze state into local files |
| `braze-sync diff` | Shows drift between local files and Braze |
| `braze-sync apply` | Applies local intent to Braze (dry-run by default) |
| `braze-sync validate` | Local-only structural and naming checks (no API call) |

Four other resource kinds (Content Block, Email Template, Catalog
Items, Custom Attribute) are visible in `--resource` and emit a
"not yet implemented (Phase B)" warning. They fill in across
v0.2.0 → v0.5.0.

## Install

```bash
cargo install braze-sync
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

## v0.1.0 limitations

These will be lifted across the v0.x → v1.0 milestones:

- **Catalog Schema only.** The other four resource kinds land in
  v0.2 → v0.5. They appear in `--resource` so the CLI surface stays
  stable, but selecting one in v0.1.0 just emits a "not yet
  implemented (Phase B)" warning.
- **No catalog create / delete.** v0.1.0 manages fields on existing
  catalogs. To create a brand-new catalog, create it in the Braze
  dashboard first, then run `braze-sync export`.
- **No field type changes.** Changing a field's type from `string` to
  `number` (or similar) is not auto-applied because the operation is
  data-losing on the field. Drop the field manually in Braze, then
  run `braze-sync apply` to re-add it with the new type.
- **`/catalogs` pagination.** v0.1.0 sends a single GET to `/catalogs`
  and returns the first page. Workspaces with very many catalogs
  (>50) may see truncated results until pagination support lands in
  Phase C scale validation.
- **`--no-color` is a no-op.** ANSI color output isn't implemented
  yet; the flag is reserved in the v0.1.0 CLI surface so it stays
  available post-1.0 without breaking existing scripts.

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
