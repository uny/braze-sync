# Braze-managed placeholders (`__BRAZESYNC.lid.*__`, `__BRAZESYNC.cb_id.*__`)

When braze-sync pushes from Git to multiple environments, two values
inside a resource body are **structurally identical but literally
different per workspace**:

- **`lid`** — the auto-numbered Link Aliasing identifier Braze assigns
  to every click-tracked link.
- **`cb_id`** — the per-workspace internal id Braze assigns to a
  referenced content block (the `cbN` in
  `{{content_blocks.${NAME} | id: 'cbN'}}`).

Both are **Braze-owned**: each workspace assigns its own values on
first save, the dashboard may reassign them after manual edits, and
nothing the operator does in Git can guarantee a specific value.
braze-sync therefore treats them as **runtime-resolved** — the local
Git body carries a stable `__BRAZESYNC.*__` placeholder, and resolution
happens at apply/diff time by reading the live remote body via the
Braze API.

## Placeholder syntax

```text
__BRAZESYNC.<type>.<key>__
```

- `<type>` ∈ `lid` | `cb_id`
- `<key>` matches `[a-z][a-z0-9_]*` (snake_case)

Example template body (this is what `braze-sync templatize` produces):

```liquid
<a href="https://example.com/spring-sale">{{ ${cblid} | lid: '__BRAZESYNC.lid.spring_sale__' }}click</a>
{{ content_blocks.${cb_promo_image} | id: '__BRAZESYNC.cb_id.cb_promo_image__' }}
```

The double-underscore envelope and dot namespace are picked so the
token is verbatim-safe in Liquid, HTML, and JSON contexts.

A typo with the wrong envelope (`__BRAZSYNC.…__`) or a retired/unknown
type (`__BRAZESYNC.custom.foo__`) surfaces as a `WARN:` line so it
doesn't slip through silently.

## How resolution works

For every `apply` and `diff`:

1. braze-sync `GET`s the live remote body for the resource.
2. For each `__BRAZESYNC.lid.<key>__` in the local template, it
   identifies the **URL anchor** (the surrounding `<a href>`,
   VML/SVG `href`, or bare URL in plaintext) and pairs it with the
   matching anchor in the remote body to lift the live `lid` value.
3. For each `__BRAZESYNC.cb_id.<key>__`, it identifies the surrounding
   `{{content_blocks.${NAME}}}` include and looks up the live `cb_id`
   under the same `${NAME}` in the remote body.
4. The resolved body is what diff compares against remote and what
   apply POSTs.

Because Git only carries the placeholder, dashboard edits that
reassign `lid` / `cb_id` are invisible to `diff` — only real template
structure changes show up.

## New resources (no remote yet)

When a resource doesn't exist in Braze yet, there is no remote body to
correlate against. braze-sync applies a controlled fallback:

- **`lid`**: the placeholder key is used verbatim as the lid value
  (e.g. `__BRAZESYNC.lid.spring_sale__` → `spring_sale`). Braze
  accepts arbitrary strings and may reassign on first dashboard open;
  the next apply picks up the reassigned value via the
  remote-resolution path above.
- **`cb_id`**: the `| id: '__BRAZESYNC.cb_id.<key>__'` filter is
  stripped entirely, leaving the documented form
  `{{content_blocks.${NAME}}}`. Braze derives an internal `cb_id` on
  save.

## How it ties into existing commands

- **`diff`** fetches the remote, resolves placeholders against it,
  and compares. A body that differs only by `lid` / `cb_id` reads as
  in-sync.
- **`apply`** fetches the remote (or treats it as a new resource if
  absent), resolves placeholders, and POSTs the resolved body.
- **`export`** writes the templated local body verbatim — `lid` /
  `cb_id` are never round-tripped through Git.
- **`templatize`** is the one-shot migration that detects raw
  `| lid: 'X'` and `{{content_blocks.${NAME} | id: 'cbN'}}` literals
  in your existing files and rewrites them to `__BRAZESYNC.*__`
  placeholders. Idempotent; safe to re-run.

## Migration from v0.14.x

v0.14.x stored `lid` / `cb_id` values in per-env `values/<env>.yaml`.
v0.15 removes that mechanism entirely: bodies templatized in v0.14
keep working (the placeholder envelope is unchanged), and stale
`values/<env>.yaml` files can simply be deleted — no command in
v0.15 reads them.

```bash
# Confirm everything still resolves cleanly after upgrading:
braze-sync diff --env=prod
braze-sync diff --env=dev

# Delete the now-unused values directory once diff is happy:
rm -rf values/
```

## Known limitations

- **Subject / preheader `lid` cannot be auto-resolved.** There is no
  URL anchor in those fields, so a `__BRAZESYNC.lid.*__` placeholder
  there will fail resolution. `templatize` warns about this at
  detection time.
- **Two placeholders sharing one URL** are resolved positionally
  (template appearance order → remote appearance order). Use distinct
  keys per occurrence to keep the mapping deterministic.
- **Structural drift between local and remote** (e.g. the dashboard
  removed a tracked link your template still references) aborts the
  apply with a clear error pointing at the unresolvable placeholder.
