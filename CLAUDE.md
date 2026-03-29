# braze-sync

GitOps CLI for Braze. Stateless architecture — no state file, Braze API is always the source of truth.

**Design doc**: `uny/canvas` — `03_strategy/ISS-006_braze-sync-cli.md`

## Commands

```bash
pnpm install        # install dependencies
pnpm build          # build CLI (tsup → dist/)
pnpm test           # run tests (Vitest)
pnpm typecheck      # tsc --noEmit
pnpm lint           # Biome check
pnpm lint:fix       # Biome auto-fix
```

## Architecture

```
Git (YAML/Liquid)  <-->  braze-sync CLI  <-->  Braze REST API
                          |
                     No state file
                (Braze API = source of truth)
```

- **CLI layer** (`src/cli/`): Commander.js — command registration & dispatch
- **Core** (`src/core/`): config loader, HTTP client, rate limiter, diff engine
- **Providers** (`src/providers/`): per-resource-type CRUD logic implementing `Provider<TLocal, TRemote>`
- **Types** (`src/types/`): Braze API types, config types, diff types, resource types
- **Formatters** (`src/formatters/`): table output (json planned for Phase 2)

## Tech Stack

TypeScript (ESM, strict), Node.js >= 22, Commander.js, `yaml` package, Vitest, tsup, Biome, pnpm

## Key Design Decisions

### Safety model
- `apply` is **dry-run by default**. No API writes without `--confirm`
- Destructive ops (field deletion, etc.) require both `--confirm` and `--allow-destructive`
- API keys are never stored in config files — only env var names are referenced (`api_key_env`)

### Rate limiting
- Catalog API: token bucket at 40 req/min (safety margin from Braze's 50 limit)
- Content Blocks / Email Templates: 250K req/hr — no limiting needed
- On 429 response: reads `Retry-After` header and auto-retries (up to 5 times)

### Resource matching
- Git-to-Braze resource mapping is **name-based** (not ID-based)
- Catalog: filename = catalog name
- Content Block: filename (without extension) = block name
- Each provider internally resolves name → ID

### Diff computation
- `computeDiff<L, R>()` generically compares local vs remote
- Catalog: detects field-level add/remove/type-change
- Content Block: compares content, description, state, tags individually
- Tag comparison is order-insensitive (sorted before comparing)

### Export determinism
- `export` output is deterministic (sorted keys, consistent formatting) so Git diffs are meaningful

## Config Format

```yaml
version: 1
environments:
  dev:
    api_url: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_DEV_API_KEY    # env var name, NOT the key itself
resources:
  catalogs: catalogs/                  # flat string path
  content_blocks: content_blocks/
```

**Note**: resource values are flat path strings (`catalogs: catalogs/`), not nested objects.

## Conventions

- **Branch**: `feat/xxx`, `fix/xxx`, `docs/xxx`
- **Commit**: `<type>: <description>` (feat, fix, refactor, docs, test, chore)
- **Indent**: 2 spaces (Biome enforced)
- **Module**: ESM (`"type": "module"` in package.json). Imports use `.js` extension
- **Tests**: `tests/unit/` with Vitest. Fixtures in `tests/fixtures/`
- **Errors**: domain-specific error classes (`ConfigError`, `BrazeApiError`)

## Braze API Gotchas

- Catalog field create/delete are **async** (return 202). Type changes require delete → wait → recreate
- Content Blocks have **no DELETE API**. The `remove` operation in apply only warns
- Custom Attributes API is **read-only**. Diff only, apply not possible
- `GET /catalogs` returns all catalogs + fields in one response. No per-catalog fetch needed
- Content Block list returns minimal info. Fetching content requires per-block `/info` calls

## Current Status

Phase 1 MVP complete. See `docs/roadmap.md` for the full phase plan.
