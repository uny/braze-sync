# braze-sync

> GitOps CLI for Braze — export, diff, and apply catalog schemas, content blocks, and custom attributes from Git.

## Project Overview

Braze の構成資産（Catalog Schema, Content Block, Custom Attributes, Email Template）を Git で宣言的に管理する CLI ツール。Terraform のような state file を持たず、毎回 Braze API を source of truth として diff を取る **Stateless GitOps** アーキテクチャ。

**設計原典**: `uny/canvas` の `03_strategy/ISS-006_braze-sync-cli.md` に PRD・市場調査・フェーズ計画がある。

## Architecture

```
Git (YAML/Liquid/HTML)  <-->  braze-sync CLI  <-->  Braze REST API
                          |
                     No state file
                  (Braze API = source of truth)
```

### Core Principles

1. **Stateless**: state file を持たない。diff は毎回「Git 上の定義 vs Braze API の現在値」で計算
2. **Safe by default**: `apply` は dry-run がデフォルト。`--confirm` なしでは絶対に変更しない
3. **Destructive ops require explicit opt-in**: フィールド削除等は `--allow-destructive` が追加で必要
4. **Rate-limit aware**: Braze API のレート制限（50 req/min 等）を自動的にハンドリング

## Tech Stack

| Category | Choice | Notes |
|:---------|:-------|:------|
| Language | TypeScript (ESM) | `"type": "module"` in package.json |
| Runtime | Node.js >= 20 | LTS |
| CLI Framework | Commander.js | |
| HTTP Client | undici (Node.js built-in) | `fetch` API を使用。外部依存を最小化 |
| YAML | yaml (npm `yaml` package) | YAML 1.2 compliant |
| Test | Vitest | |
| Build | tsup | ESM + CJS dual output, CLI binary |
| Lint/Format | Biome | |
| CI | GitHub Actions | |
| Package Manager | pnpm | |

## Directory Structure

```
braze-sync/
  src/
    cli/
      index.ts                  # CLI entrypoint (Commander.js program)
      commands/
        export.ts               # `braze-sync export` command
        diff.ts                 # `braze-sync diff` command
        apply.ts                # `braze-sync apply` command
        validate.ts             # `braze-sync validate` command
    core/
      config.ts                 # braze-sync.config.yaml loader & validator
      diff-engine.ts            # Resource-agnostic diff logic
      rate-limiter.ts           # Token bucket rate limiter (40 req/min safe limit)
      braze-client.ts           # Low-level Braze API HTTP client
    providers/
      base.ts                   # Provider interface / abstract base
      catalog.ts                # Catalog schema provider (Phase 1)
      content-block.ts          # Content Block provider (Phase 1)
      custom-attribute.ts       # Custom Attribute provider (Phase 2)
      email-template.ts         # Email Template provider (Phase 3)
    formatters/
      table.ts                  # Human-readable table output
      json.ts                   # Machine-readable JSON output
    types/
      braze-api.ts              # Braze API request/response types
      config.ts                 # Config file types
      diff.ts                   # Diff result types
      resource.ts               # Resource definition types
  tests/
    unit/
      core/
        config.test.ts
        diff-engine.test.ts
        rate-limiter.test.ts
      providers/
        catalog.test.ts
        content-block.test.ts
    integration/                # Braze API integration tests (require API key)
      catalog.integration.test.ts
      content-block.integration.test.ts
    fixtures/
      catalogs/                 # Sample catalog YAML files
      content_blocks/           # Sample Liquid files
      configs/                  # Sample config files
  package.json
  tsconfig.json
  tsup.config.ts
  biome.json
  vitest.config.ts
  CLAUDE.md
  README.md
  LICENSE
```

## User-Facing Config Format

### braze-sync.config.yaml

```yaml
version: 1

environments:
  dev:
    api_url: https://rest.fra-02.braze.eu    # Braze instance URL
    api_key_env: BRAZE_DEV_API_KEY           # Name of env var (NOT the key itself)
  prod:
    api_url: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_PROD_API_KEY

resources:
  catalogs:
    path: catalogs/
  content_blocks:
    path: content_blocks/
  custom_attributes:
    path: custom_attributes/definitions.yaml
  email_templates:
    path: email_templates/
```

### Resource File Formats

**Catalog Schema** (`catalogs/{name}.yaml`):

```yaml
name: cardiology
description: "Cardiology catalog"
fields:
  - name: condition_id
    type: string
  - name: condition_name
    type: string
  - name: display_order
    type: number
  - name: is_active
    type: boolean
```

Braze catalog field types: `string`, `number`, `boolean`, `time` (ISO-8601)

**Content Block** (`content_blocks/{name}.liquid`):

Liquid テンプレートをそのままファイルとして保存。メタデータは frontmatter-style コメントで埋め込む:

```liquid
---
description: "Post bonus dialog content"
state: active
tags:
  - campaign_2504
  - pr
---
<div class="bonus-dialog">
  {{ ${content} }}
</div>
```

frontmatter は `---` で囲まれた YAML ブロック。パーサは frontmatter を分離して API に渡す。

**Custom Attributes** (`custom_attributes/definitions.yaml`):

```yaml
attributes:
  - name: preferred_language
    data_type: string
    description: "User's preferred language code"
  - name: subscription_tier
    data_type: string
    description: "Current subscription tier"
  - name: onboarding_completed
    data_type: boolean
    description: "Whether user completed onboarding"
```

**Email Template** (`email_templates/{name}.html`):

```html
---
subject: "Welcome to {{brand_name}}"
preheader: "Get started with your account"
description: "Welcome email for new users"
should_inline_css: true
tags:
  - onboarding
---
<html>
<body>
  <h1>Welcome!</h1>
</body>
</html>
```

## CLI Commands Spec

### Global Options

```
--config, -c    Path to braze-sync.config.yaml (default: ./braze-sync.config.yaml)
--env, -e       Target environment name (required for all commands except validate)
--verbose       Show debug output
--format        Output format: "table" (default) | "json"
```

### `braze-sync export`

Export current Braze state to local files.

```
braze-sync export --env <name> [--resource <type>] [--name <resource-name>]
```

| Option | Description |
|:-------|:-----------|
| `--resource` | Filter by resource type: `catalogs`, `content_blocks`, `custom_attributes`, `email_templates` |
| `--name` | Filter by specific resource name (requires `--resource`) |

Behavior:
1. Fetch resources from Braze API
2. Normalize: sort keys alphabetically, strip API-only metadata (IDs, timestamps)
3. Write to local files per config paths
4. Overwrite existing files (Git tracks the diff)

### `braze-sync diff`

Show differences between local definitions and Braze live state.

```
braze-sync diff --env <name> [--resource <type>] [--fail-on-drift]
```

| Option | Description |
|:-------|:-----------|
| `--resource` | Filter by resource type |
| `--fail-on-drift` | Exit with code 1 if any drift detected (for CI) |

Diff categories:
- `+` (add): Exists in Git, missing in Braze
- `-` (remove): Exists in Braze, missing in Git
- `~` (change): Exists in both but differs

### `braze-sync apply`

Apply local definitions to Braze. **Dry-run by default.**

```
braze-sync apply --env <name> [--resource <type>] [--confirm] [--allow-destructive]
```

| Option | Description |
|:-------|:-----------|
| `--confirm` | Actually apply changes (without this, only shows plan) |
| `--allow-destructive` | Allow destructive operations (field deletion, etc.) |
| `--resource` | Filter by resource type |

Behavior:
1. Compute diff (same as `diff` command)
2. Display change plan
3. If `--confirm`: execute API calls to apply changes
4. Report results (success/failure per resource)

Safety rules:
- Without `--confirm`: NEVER make any API write calls
- Destructive changes (delete field, etc.) require BOTH `--confirm` AND `--allow-destructive`
- Log every API call made during apply

### `braze-sync validate`

Validate local definition files without contacting Braze API.

```
braze-sync validate [--config <path>]
```

Checks:
- Config file syntax and required fields
- YAML syntax of all resource files
- Liquid syntax of content block files
- Field types are valid Braze types
- Required fields present (e.g., catalog `name`, `fields`)
- No duplicate resource names

## Braze API Reference (Implementation Guide)

Base URL: `https://rest.{instance}.braze.com`
Auth: `Authorization: Bearer {API_KEY}` header on all requests.

### Catalogs API

| Operation | Method | Path | Notes |
|:----------|:-------|:-----|:------|
| List catalogs | `GET` | `/catalogs` | Returns ALL catalogs with `fields[]`. No pagination. |
| Create catalog | `POST` | `/catalogs` | Body: `{ catalogs: [{ name, description, fields: [{ name, type }] }] }`. One per request. |
| Create fields | `POST` | `/catalogs/{catalog_name}/fields` | Body: `{ fields: [{ name, type }] }`. Max 50 fields/req, 500 fields/catalog. Returns 202. |
| Delete field | `DELETE` | `/catalogs/{catalog_name}/fields/{field_name}` | Returns 202. |

Rate limit: **50 req/min** (shared across all catalog sync endpoints)

Important:
- `GET /catalogs` returns fields for ALL catalogs in one response — no need for per-catalog fetch
- Field create/delete are **async** (return 202)
- No "update field" endpoint — to change a field type, must delete and recreate
- Catalog `id` field is auto-created and cannot be modified

### Content Blocks API

| Operation | Method | Path | Notes |
|:----------|:-------|:-----|:------|
| List | `GET` | `/content_blocks/list` | Query: `limit` (max 1000), `offset`, `modified_after`, `modified_before`. Returns `content_blocks[]` with `content_block_id`, `name`. |
| Get info | `GET` | `/content_blocks/info` | Query: `content_block_id` (required). Returns full `content`, `description`, `tags[]`, `state`. |
| Create | `POST` | `/content_blocks/create` | Body: `{ name, content, description, state, tags }`. Returns `content_block_id`. |
| Update | `POST` | `/content_blocks/update` | Body: `{ content_block_id, name, content, description, state, tags }`. |

Rate limit: **250,000 req/hr**

Important:
- **No DELETE endpoint** — content blocks cannot be deleted via API
- List returns minimal info — must call `/info` for each block to get `content`
- `state`: "active" or "draft"
- Tags must already exist in the workspace
- Both create and update use `POST` (not PUT/PATCH)
- Identification: list returns `content_block_id`, which is needed for info/update. Match by `name` for Git<->Braze mapping.

### Custom Attributes API

| Operation | Method | Path | Notes |
|:----------|:-------|:-----|:------|
| Export | `GET` | `/custom_attributes` | Returns 50 per page. Cursor-based pagination via `Link` header. |

Rate limit: **1,000 req/hr** (shared with `/events`, `/events/list`, `/purchases/product_list`)

Important:
- **Read-only** — no create/update/delete API. Attributes are implicitly created when first set via `/users/track`
- For braze-sync: diff-only (detect drift between definition file and live state). Cannot apply.

### Email Templates API

| Operation | Method | Path | Notes |
|:----------|:-------|:-----|:------|
| List | `GET` | `/templates/email/list` | Query: `limit` (max 1000), `offset`. Returns `templates[]` with `email_template_id`, `template_name`. |
| Get info | `GET` | `/templates/email/info` | Query: `email_template_id`. Returns `body`, `subject`, `preheader`, `tags[]`, etc. |
| Create | `POST` | `/templates/email/create` | Body: `{ template_name, subject, body, plaintext_body, preheader, tags, should_inline_css }`. |
| Update | `POST` | `/templates/email/update` | Body: `{ email_template_id, template_name, subject, body, ... }`. |

Rate limit: **250,000 req/hr**

Important:
- **No DELETE endpoint**
- Drag-and-drop editor templates are NOT accessible via API
- Match by `template_name` for Git<->Braze mapping (list returns `email_template_id`)

## Rate Limiter Design

Token Bucket algorithm:

```
Target: 40 req/min for catalog endpoints (safety margin from 50 limit)
        No limiting needed for content blocks/email templates (250K/hr)

On 429 response:
  1. Read Retry-After header
  2. Wait specified duration
  3. Retry the request

Catalog operations:
  - Sequential execution (no parallel API calls)
  - Batch where possible (e.g., multiple fields in one POST /fields)
```

## Provider Interface

Each resource type implements a `Provider` interface:

```typescript
interface Provider<TLocal, TRemote> {
  /** Resource type identifier */
  readonly resourceType: string;

  /** Read local definition files from disk */
  readLocal(configPath: string): Promise<TLocal[]>;

  /** Fetch current state from Braze API */
  fetchRemote(client: BrazeClient): Promise<TRemote[]>;

  /** Compute diff between local and remote */
  diff(local: TLocal[], remote: TRemote[]): DiffResult[];

  /** Apply changes to Braze (only additions/updates matching the diff) */
  apply(client: BrazeClient, diffs: DiffResult[], options: ApplyOptions): Promise<ApplyResult[]>;

  /** Serialize remote state to local file format (for export) */
  serialize(remote: TRemote): LocalFileOutput;

  /** Validate local definition files */
  validate(local: TLocal[]): ValidationError[];
}
```

## Diff Engine Design

```typescript
type DiffOperation = "add" | "remove" | "change";

interface DiffResult {
  resourceType: string;       // "catalog", "content_block", etc.
  resourceName: string;       // e.g., "cardiology"
  operation: DiffOperation;
  details: DiffDetail[];      // Field-level or content-level changes
}

interface DiffDetail {
  field: string;              // e.g., "fields.condition_id" or "content"
  operation: DiffOperation;
  localValue?: unknown;
  remoteValue?: unknown;
}
```

Catalog diff は field 単位で比較。Content Block diff は content 文字列の行単位 diff。

## Phase 1 Scope (MVP)

Phase 1 で実装するもの:

1. **Config loader**: `braze-sync.config.yaml` の読み込み・バリデーション
2. **Braze HTTP client**: 認証、レート制限、リトライ
3. **Catalog provider**: export / diff / apply (schema fields のみ、items は Phase 2)
4. **Content Block provider**: export / diff / apply
5. **CLI commands**: export, diff, apply, validate
6. **Table formatter**: 人間が読める diff 出力
7. **Unit tests**: core + providers

Phase 1 で実装しないもの:
- Custom Attributes provider (Phase 2)
- Email Template provider (Phase 3)
- JSON formatter (Phase 2)
- GitHub Action (Phase 2)
- Catalog Items 管理 (Phase 2)

## Implementation Order

以下の順序で実装する:

### Step 1: Project Scaffolding
- `package.json` (name: `braze-sync`, type: module, bin: `braze-sync`)
- `tsconfig.json` (strict, ESM, target ES2022)
- `tsup.config.ts` (entry: `src/cli/index.ts`, format: esm + cjs)
- `biome.json`
- `vitest.config.ts`
- `.gitignore`
- pnpm install dependencies

### Step 2: Types
- `src/types/config.ts` — config file schema types
- `src/types/braze-api.ts` — Braze API request/response types
- `src/types/resource.ts` — local resource definition types
- `src/types/diff.ts` — diff result types

### Step 3: Core
- `src/core/config.ts` — YAML config loader + validation
- `src/core/braze-client.ts` — HTTP client with auth + rate limiting + retry
- `src/core/rate-limiter.ts` — Token bucket implementation
- `src/core/diff-engine.ts` — Generic diff computation

### Step 4: Providers
- `src/providers/base.ts` — Provider interface
- `src/providers/catalog.ts` — Catalog schema provider
- `src/providers/content-block.ts` — Content Block provider

### Step 5: Formatters
- `src/formatters/table.ts` — Table output

### Step 6: CLI Commands
- `src/cli/index.ts` — Commander.js program setup
- `src/cli/commands/export.ts`
- `src/cli/commands/diff.ts`
- `src/cli/commands/apply.ts`
- `src/cli/commands/validate.ts`

### Step 7: Tests
- Unit tests for config, diff-engine, rate-limiter
- Unit tests for catalog + content-block providers (mock API responses)
- Fixture files for tests

### Step 8: README
- Installation, quick start, configuration, command reference

## Git Conventions

- Branch: `feat/xxx`, `fix/xxx`, `docs/xxx`
- Commit: `<type>: <description>` (feat, fix, refactor, docs, test, chore)
- PR workflow

## npm Publish

```json
{
  "name": "braze-sync",
  "version": "0.1.0",
  "bin": {
    "braze-sync": "./dist/cli/index.js"
  },
  "files": ["dist"],
  "type": "module",
  "engines": { "node": ">=20" }
}
```

## Important Implementation Notes

- API keys are NEVER stored in config files. Config only references env var names (`api_key_env`).
- All Braze API errors should be caught and displayed with the HTTP status, error message, and which resource/operation caused it.
- `export` should produce deterministic output (sorted keys, consistent formatting) so that Git diffs are meaningful.
- Content Block matching between Git and Braze is done by `name` (not ID). The provider must resolve name -> content_block_id internally.
- Email Template matching is done by `template_name` -> `email_template_id`.
- Catalog matching is done by catalog `name` (1:1 with filename).
