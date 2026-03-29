# Roadmap

## Phase 1 — MVP (complete)

- Config loader: `braze-sync.config.yaml` loading & validation
- Braze HTTP client: auth, rate limiting, retry on 429
- Catalog provider: export / diff / apply (schema fields only, not items)
- Content Block provider: export / diff / apply
- CLI commands: export, diff, apply, validate
- Table formatter: human-readable diff output
- Unit tests: core modules + both providers

## Phase 2

- Custom Attributes provider (read-only diff — no apply API exists)
- JSON formatter (`--format json`)
- Catalog Items management
- GitHub Action for CI drift detection
- Provider registry pattern to reduce command-level duplication

## Phase 3

- Email Template provider: export / diff / apply
- Colored terminal output
- Progress indicators for batch operations
- Integration test suite (requires Braze API key)

## Resource File Formats

### Catalog (`catalogs/{name}.yaml`)

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

Supported field types: `string`, `number`, `boolean`, `time` (ISO-8601)

### Content Block (`content_blocks/{name}.liquid`)

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

### Custom Attributes (`custom_attributes/definitions.yaml`) — Phase 2

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

### Email Template (`email_templates/{name}.html`) — Phase 3

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
