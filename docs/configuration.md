# Configuration

`braze-sync` is driven by a single YAML file, `braze-sync.config.yaml`, in
the root of your workspace. `braze-sync init` scaffolds a commented
template; this page documents every field.

The config schema is **frozen at v1.0** under the `version: 1` tag. A
future v2 schema would bump that number — v1.x binaries will only accept
`version: 1`.

## Full example

```yaml
version: 1

default_environment: dev

environments:
  dev:
    api_endpoint: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_DEV_API_KEY
  prod:
    api_endpoint: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_PROD_API_KEY

resources:
  catalog_schema:
    enabled: true
    path: catalogs/
  content_block:
    enabled: true
    path: content_blocks/
  email_template:
    enabled: true
    path: email_templates/
  custom_attribute:
    enabled: true
    path: custom_attributes/registry.yaml

naming:
  catalog_name_pattern: "^[a-z][a-z0-9_]*$"
  content_block_name_pattern: "^[a-zA-Z0-9_]+$"
  custom_attribute_name_pattern: "^[a-z][a-z0-9_]*$"
```

## Top-level fields

### `version` (required)

Config schema version. Must be exactly `1` for every v1.x release of
`braze-sync`. Any other value is a config error (exit code `3`).

### `default_environment` (required)

Name of the environment used when `--env` is not passed on the command
line. Must match a key under `environments`.

### `environments` (required, at least one entry)

Map of environment name → settings. Pick the active one with
`--env <name>`; otherwise `default_environment` applies.

| Field | Type | Required | Notes |
|:---|:---|:---|:---|
| `api_endpoint` | URL | yes | Braze REST endpoint for your instance (see [Braze API endpoints](https://www.braze.com/docs/api/basics/#endpoints)). The scaffold defaults to the EU `fra-02` cluster — change it if your instance lives elsewhere. |
| `api_key_env` | string | yes | Name of the **environment variable** holding the API key. The key itself must never appear in this file. |

API keys are loaded into `secrecy::SecretString` at startup and never
appear in `Debug` output, tracing, or panic messages. A `.env` file in
the current working directory is loaded via `dotenvy` (no recursive
parent-directory search).

### Rate limiting

braze-sync does not carry a client-side rate limiter. The HTTP client
reacts to 429 responses by honoring `Retry-After` when present and
falling back to exponential backoff with jitter otherwise. Total retry
sleep is bounded by an internal budget; beyond that a
`RateLimitExhausted` error surfaces to the caller.

### `resources` (optional)

Toggles and paths for each v1.0 resource kind. Every sub-block is
optional — omitted entries fall back to the defaults shown below. To
skip a resource entirely in a workspace, set `enabled: false`.

#### `catalog_schema`

| Field | Type | Default |
|:---|:---|:---|
| `enabled` | bool | `true` |
| `path` | path | `catalogs/` |

Directory holding one `<catalog>/schema.yaml` per catalog.

#### `content_block`

| Field | Type | Default |
|:---|:---|:---|
| `enabled` | bool | `true` |
| `path` | path | `content_blocks/` |

Directory of `<name>.liquid` files.

#### `email_template`

| Field | Type | Default |
|:---|:---|:---|
| `enabled` | bool | `true` |
| `path` | path | `email_templates/` |

Directory holding one `<template>/` subdirectory per email template, each
containing `template.yaml`, `body.html`, and `body.txt`.

#### `custom_attribute`

| Field | Type | Default |
|:---|:---|:---|
| `enabled` | bool | `true` |
| `path` | path | `custom_attributes/registry.yaml` |

A single-file registry — see [registry-mode.md](registry-mode.md).

### `naming` (optional)

Optional name validators enforced by `braze-sync validate`. Each entry
is a regex evaluated by the [`regex-lite`](https://docs.rs/regex-lite)
crate — a subset of the full `regex` crate with no Unicode classes
(`\p{…}`), limited `\d`/`\w` behavior, and no look-around. Consult the
`regex-lite` syntax reference when writing patterns. Omitted patterns
mean "no check".

| Field | Applies to |
|:---|:---|
| `catalog_name_pattern` | Catalog names |
| `content_block_name_pattern` | Content block names |
| `custom_attribute_name_pattern` | Custom attribute names |

Naming checks exit code `3` on mismatch, the same as any other
`validate` failure.

## Strictness

The config file itself uses `#[serde(deny_unknown_fields)]` at every
level, so typos and stray keys fail fast with a pointer to the offending
line. Resource files (`schema.yaml`, `template.yaml`, the registry, etc.)
are intentionally **permissive** — unknown fields there are ignored,
preserving forward-compatibility across v1.x.
