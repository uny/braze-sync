# braze-sync

GitOps CLI for Braze — export, diff, and apply catalog schemas and content blocks from Git.

Terraform-like workflow without a state file. Every diff is computed live against the Braze API (source of truth).

## Installation

```bash
npm install -g braze-sync
# or
pnpm add -g braze-sync
```

### From source

```bash
git clone https://github.com/uny/braze-sync.git
cd braze-sync
pnpm install
pnpm build
```

## Quick Start

1. Create a config file:

```yaml
# braze-sync.config.yaml
version: 1

environments:
  dev:
    api_url: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_DEV_API_KEY
  prod:
    api_url: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_PROD_API_KEY

resources:
  catalogs: catalogs/
  content_blocks: content_blocks/
```

2. Set your API key as an environment variable:

```bash
export BRAZE_DEV_API_KEY="your-api-key-here"
```

3. Export current Braze state:

```bash
braze-sync export --env dev
```

4. Check for drift:

```bash
braze-sync diff --env dev
```

5. Apply changes (dry-run by default):

```bash
braze-sync apply --env dev           # dry-run: shows plan only
braze-sync apply --env dev --confirm # actually applies changes
```

## Configuration

### braze-sync.config.yaml

| Field | Description |
|:------|:------------|
| `version` | Config version. Must be `1`. |
| `environments.<name>.api_url` | Braze REST API instance URL. |
| `environments.<name>.api_key_env` | Name of the environment variable holding the API key. The key itself is never stored in the config. |
| `resources.catalogs` | Path to the directory containing catalog YAML files. |
| `resources.content_blocks` | Path to the directory containing content block Liquid files. |

### Resource File Formats

**Catalog** (`catalogs/<name>.yaml`):

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

Supported field types: `string`, `number`, `boolean`, `time`

**Content Block** (`content_blocks/<name>.liquid`):

```liquid
---
description: "Post bonus dialog content"
state: active
tags:
  - campaign_2504
  - pr
---
<div class="bonus-dialog">
  {{ content }}
</div>
```

Metadata is embedded as YAML frontmatter between `---` delimiters.

## Commands

### Global Options

```
-c, --config <path>   Path to config file (default: braze-sync.config.yaml)
-e, --env <name>      Target environment name
    --verbose          Show debug output
```

### `braze-sync export`

Export current Braze state to local files.

```bash
braze-sync export --env <name> [--resource <type>] [--name <resource-name>]
```

| Option | Description |
|:-------|:------------|
| `--resource` | Filter by resource type: `catalogs`, `content_blocks` |
| `--name` | Filter by specific resource name (requires `--resource`) |

### `braze-sync diff`

Show differences between local definitions and Braze live state.

```bash
braze-sync diff --env <name> [--resource <type>] [--fail-on-drift]
```

| Option | Description |
|:-------|:------------|
| `--resource` | Filter by resource type |
| `--fail-on-drift` | Exit with code 1 if any drift detected (useful for CI) |

Diff symbols:
- `[+]` resource or field exists locally but not in Braze (will be added)
- `[-]` resource or field exists in Braze but not locally (will be removed)
- `[~]` resource or field exists in both but differs (will be changed)

### `braze-sync apply`

Apply local definitions to Braze. **Dry-run by default.**

```bash
braze-sync apply --env <name> [--resource <type>] [--confirm] [--allow-destructive]
```

| Option | Description |
|:-------|:------------|
| `--confirm` | Actually apply changes. Without this flag, only the plan is shown. |
| `--allow-destructive` | Allow destructive operations (field deletion, type changes). |
| `--resource` | Filter by resource type |

Safety rules:
- Without `--confirm`: no API write calls are made.
- Destructive changes require both `--confirm` and `--allow-destructive`.

### `braze-sync validate`

Validate local definition files without contacting the Braze API.

```bash
braze-sync validate [--config <path>]
```

Checks:
- Config file syntax and required fields
- YAML syntax of resource files
- Field types are valid Braze types
- Required fields are present
- No duplicate resource names

## Safety

- API keys are **never** stored in config files. The config only references environment variable names.
- `apply` is **dry-run by default**. You must pass `--confirm` to execute changes.
- Destructive operations (field deletion, type changes) require an additional `--allow-destructive` flag.
- Catalog API calls are rate-limited to 40 req/min (safety margin from the 50 req/min Braze limit).
- On HTTP 429 responses, the CLI automatically waits and retries using the `Retry-After` header.

## Development

```bash
pnpm install        # install dependencies
pnpm build          # build CLI
pnpm test           # run tests
pnpm typecheck      # type check
pnpm lint           # lint with Biome
pnpm lint:fix       # auto-fix lint issues
```

## License

MIT
