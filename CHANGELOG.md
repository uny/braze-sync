# Changelog

All notable changes to braze-sync are recorded here. The format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
versions follow [semver](https://semver.org/). Per IMPLEMENTATION.md
§0.2 the v0.x line is crates.io-published but tolerates breaking
changes; v1.0 freezes the public surface (CLI flags, config schema,
file formats, JSON output, exit codes) for the full v1.x line.

## [Unreleased]

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
