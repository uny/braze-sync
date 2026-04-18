# Changelog

All notable changes to braze-sync are recorded here. The format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
versions follow [semver](https://semver.org/). Per IMPLEMENTATION.md
§0.2 the v0.x line is crates.io-published but tolerates breaking
changes; v1.0 freezes the public surface (CLI flags, config schema,
file formats, JSON output, exit codes) for the full v1.x line.

## [Unreleased]

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
  (plus standalone `.sig` / `.pem`) verifiable against the release
  workflow's OIDC identity. See README → "Verifying release artifacts".

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
