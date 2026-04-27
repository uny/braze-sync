# Scope boundaries

braze-sync manages Braze **configuration** as code. It explicitly does
**not** manage runtime data. This page records the line and the
reasoning so that future feature requests have a shared frame of
reference.

## In scope

Configuration — things that look like schema, templates, or
declarative settings, and that a team would review in a pull request.

| Resource | What braze-sync owns |
|:---|:---|
| **Catalog Schema** | Catalog creation, field definitions, types, constraints |
| **Content Block** | Reusable Liquid fragments |
| **Email Template** | HTML/Liquid templates, subject, metadata |
| **Custom Attribute** registry | Attribute name/type/deprecation — the *definition* only |

All four are low-cardinality (tens to low hundreds of entries per
workspace), human-authored, and worth code review on change.

## Out of scope

Runtime data — things that change as users interact with the product,
are written by application code, or live at volumes that make Git
review nonsensical.

| Runtime data | Why not braze-sync | Where it lives instead |
|:---|:---|:---|
| **Catalog items** (rows) | High volume (10k–100k+); sourced from analytics / product DBs; changes daily | Braze REST `/catalogs/{name}/items` from data pipelines, Cloud Functions, ETL jobs |
| **Custom Attribute values** | Written per-user by the app | `/users/track` from application code |
| **Events** | Emitted per-action | `/users/track` events array |
| **Campaigns / Canvases** | Produced in the Braze dashboard; include runtime analytics | Braze dashboard (no API-first workflow is realistic today) |

If a data source changes faster than a human can review PRs, it does
not belong in braze-sync.

## Why the split matters

1. **Review signal-to-noise.** A PR that adds a new `catalog_schema`
   field is meaningful; a PR that updates 50,000 rows of a product
   catalog is not.
2. **Tool correctness.** braze-sync's diff/apply model assumes the
   full set of managed resources fits in memory and can be compared
   hash-for-hash. Runtime data violates that assumption.
3. **Operational split.** Configuration is a release-gated change
   (merge → deploy). Runtime data is a background stream. Mixing the
   two in one tool couples their cadences.

## Was catalog_items ever in scope?

It was wired up in v0.4–v0.7 as an experiment and removed in **v0.8.0**
for the reasons above. If you were relying on it:

- **Reads** (`export`): hit `GET /catalogs/{name}/items` directly; the
  response is paginated with `next_cursor` in the body.
- **Writes** (`apply`): Braze enforces max 50 items per
  POST/DELETE; batch accordingly. `braze-sync.config.yaml` with a
  leftover `catalog_items:` section will hard-error at load time
  (intentional — see the v0.8.0 CHANGELOG entry for migration steps).

## See also

- [`docs/configuration.md`](configuration.md) — YAML reference for the
  four managed resources
- [`docs/registry-mode.md`](registry-mode.md) — why Custom Attributes
  are registry-only (not row-level CRUD)
- [`CHANGELOG.md`](../CHANGELOG.md) — v0.8.0 breaking changes and
  migration steps
