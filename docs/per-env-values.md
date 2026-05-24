# Per-env values: template + values separation

When braze-sync pushes from Git to multiple environments (Dev / Prod / …),
some values inside a resource body are **structurally identical but
literally different per workspace**. The canonical example is the
auto-numbered Link Aliasing `lid` Braze assigns when a click-tracked
link is created — Dev's `lid` is not Prod's `lid`, but the surrounding
HTML and Liquid is the same.

Pre-v0.14, braze-sync had no way to express "the structure is shared,
the value differs per env": pushing Dev's body to Prod overwrote Prod's
`lid` and broke click tracking on the Prod side. v0.14 introduces a
**template + values** model — analogous to Helm `values.yaml` or
Terraform `*.tfvars` — so the body in Git stays env-agnostic and the
per-env values live in `values/<env>.yaml`.

## Quick start

If you're upgrading an existing repo, run the one-shot migration:

```bash
braze-sync templatize --from-env=prod --dry-run    # preview
braze-sync templatize --from-env=prod              # actually rewrite
```

This walks every `content_blocks/*.liquid` and `email_templates/*`,
rewrites raw `lid` values and content_block include IDs into
`__BRAZESYNC.<type>.<key>__` placeholders, writes
`values/prod.yaml` with the canonical env's values, and writes
**skeleton** `values/<env>.yaml` files (with `value: null`) for every
other configured environment.

For each non-canonical env, run `export` to populate real values:

```bash
braze-sync export --env=dev
braze-sync export --env=staging
```

After that, `diff` / `apply` work as before — placeholders are resolved
against `values/<env>.yaml` before any HTTP write.

## Placeholder syntax

```text
__BRAZESYNC.<type>.<key>__
```

- `<type>` ∈ `lid` | `cb_id` | `custom` | `global`
- `<key>` matches `[a-z][a-z0-9_]*` (snake_case)

Example template body:

```liquid
<a href="https://example.com/spring-sale">{{ ${cblid} | lid: '__BRAZESYNC.lid.spring_sale__' }}click</a>
{{ content_blocks.${cb_promo_image} | id: '__BRAZESYNC.cb_id.cb_promo_image__' }}
<script src="https://__BRAZESYNC.custom.api_host__/widget.js"></script>
```

The double-underscore envelope and dot namespace are picked so the
token is verbatim-safe in Liquid, HTML, and JSON contexts and won't
silently expand in Braze's Liquid engine if a value is ever forgotten.

A typo with the wrong envelope (`__BRAZSYNC.…__`) or an unknown type
(`__BRAZESYNC.url.foo__`) surfaces as a `WARN:` line at pre-flight so
it doesn't slip through silently.

## values/&lt;env&gt;.yaml schema

```yaml
version: 1

# Shared across every resource. Reference as __BRAZESYNC.global.<key>__.
globals:
  custom:
    api_host:
      value: api-prod.example.com

content_block:
  cb_promo_banner:
    lid:
      spring_sale:
        value: ai8kexrxcp03
        url: https://example.com/spring-sale     # export correlation anchor
    cb_id:
      cb_promo_image:
        value: cb42                              # key = referenced block slug
    custom:
      banner_variant:
        value: A

email_template:
  welcome:
    custom:                                       # resource-scoped only
      user_segment_id:
        value: seg_prod_42
    subject:
      lid:                                        # lid / cb_id are field-scoped
        promo_subject:
          value: lidsubj42
          anchor: "{{promo_code}}"                # anchor for URL-less fields
    body_html:
      lid:
        cta:
          value: lidhtml42
          url: https://example.com/welcome/cta
      cb_id:
        cb_promo_image:
          value: cb42
```

### Namespace rules

| Placeholder | Resolves against |
|---|---|
| `__BRAZESYNC.global.<key>__` | `globals.custom.<key>` (cross-resource) |
| `__BRAZESYNC.custom.<key>__` | The resource's own `custom.<key>` |
| `__BRAZESYNC.lid.<key>__` (content_block) | `content_block.<name>.lid.<key>` |
| `__BRAZESYNC.cb_id.<key>__` (content_block) | `content_block.<name>.cb_id.<key>` |
| `__BRAZESYNC.lid.<key>__` (email_template) | The field-scoped `<name>.<field>.lid.<key>` |
| `__BRAZESYNC.cb_id.<key>__` (email_template) | The field-scoped `<name>.<field>.cb_id.<key>` |

Lookup is exact — there is **no fallback** between resource-local and
global namespaces. Choose the namespace in the placeholder.

### `value: null` (skeleton)

`templatize` writes `value: null` for envs other than the canonical
one. This signals "needs export"; shape validation skips these entries
and pre-flight aborts apply with a hint to run `braze-sync export`.

## Config

Optional override per environment:

```yaml
environments:
  prod:
    api_endpoint: https://rest.iad-01.braze.com
    api_key_env: BRAZE_PROD_API_KEY
    values_file: values/prod.yaml         # optional; this is the default
```

Omitted → defaults to `values/<env_name>.yaml` relative to the config
directory.

## How it ties into existing commands

- **`diff`** resolves placeholders before comparing against remote, so
  `diff --env=dev` and `diff --env=prod` each show the right
  per-env contents. A body that differs only by `lid` reads as in-sync.
- **`apply`** resolves placeholders before POST. Any unresolved
  placeholder aborts pre-flight before a single Braze write is issued
  (failures are aggregated across every resource and reported together,
  Terraform-style).
- **`export`** preserves the templated body in Git and writes back only
  the values entries, using URL anchors (HTML), bare-URL anchors
  (plaintext), Liquid-identifier anchors (subject / preheader), or
  `${NAME}` syntactic anchors (cb_id include form) to keep
  key↔remote-value correlation stable across commits.
- **`templatize`** is the one-shot migration; idempotent on
  already-templatized bodies and skips them by default.

## Plan/apply lock interaction (v0.13.0 → v0.14.0)

`diff --plan-out=plan.json` now also records a per-resource
**values-input hash** — a blake3 digest over the
`<type>.<key> → value` subset that resource actually consumes. `apply
--plan=plan.json` recomputes it and exits **7** (`PlanDrift`) if the
values file was edited between plan and apply for a resource the plan
froze. Editing values keys that no resource in the plan consumes does
not trip the lock; neither do body-only edits that don't change the
placeholder set, so the v0.13 plan-lock tolerance for benign body
edits is preserved.

A single `globals.custom.<key>` edit invalidates the hash of **every**
resource that uses that global. If apply aborts for many resources at
once, regenerate the plan:

```bash
braze-sync diff --env=prod --plan-out=plan.json
```

## Known limitations (v0.14.0)

These are documented as deferred from the RFC and may be lifted in
later patches:

- **No `--show-template` diff mode.** `diff` always compares the
  resolved body against remote. There is no flag yet to view the
  templated body alongside.
- **Non-ASCII slug naming is silent.** `templatize` / `export` slugify
  URL paths and content_block names; non-ASCII sources (e.g. Japanese
  link text) collapse to `link_` / `cb_` plus an index without an
  explicit warning. The generated keys are valid but lose meaning —
  rename them in `values/<env>.yaml` after the first run.
- **cb_id slug collisions are silent.** If two distinct content_block
  names slug to the same key, the second gets a `_2` / `_3` suffix
  with no warning. Inspect the generated values file after migration
  if you suspect collisions.
- **No automatic key sync on rename.** Renaming a key in
  `values/<env>.yaml` does not rewrite placeholders in templates. Edit
  both sides manually for now.
- **No values-file secret separation.** The values file is plain YAML
  intended for Git. `braze-sync` has no Helm-secrets style encrypted
  overlay; keep secrets out of the file. (API keys still live in env
  vars as before.)

## Migration checklist

1. Upgrade to v0.14.0.
2. `braze-sync templatize --from-env=<canonical>` (preview first).
3. `braze-sync export --env=<other-env>` for each remaining env.
4. Review the generated `values/*.yaml` in a PR alongside the
   templatized bodies.
5. CI: `braze-sync diff --env=<env> --fail-on-drift` should report
   `0 diff` on each env if migration was clean.
