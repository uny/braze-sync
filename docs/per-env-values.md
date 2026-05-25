# Braze-managed placeholders (`__BRAZESYNC__`)

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
Git body carries a stable `__BRAZESYNC__` placeholder, and resolution
happens at apply/diff time by reading the live remote body via the
Braze API.

## Placeholder syntax

```text
__BRAZESYNC__
```

A single anonymous token represents both `lid` and `cb_id`
placeholders. The type is inferred from the surrounding filter syntax:

- `| lid: '__BRAZESYNC__'` → `lid`
- `| id:  '__BRAZESYNC__'` → `cb_id`

Example template body (this is what `braze-sync templatize` produces):

```liquid
<a href="https://example.com/spring-sale">{{ ${cblid} | lid: '__BRAZESYNC__' }}click</a>
{{ content_blocks.${cb_promo_image} | id: '__BRAZESYNC__' }}
```

The double-underscore envelope is picked so the token is verbatim-safe
in Liquid, HTML, and JSON contexts. A `__BRAZESYNC__` outside a
recognized `| lid:` / `| id:` argument fails resolution with an
`UnknownContext` error so the typo doesn't slip through silently.

The v0.15 form `__BRAZESYNC.<type>.<key>__` is **retired**. It is
detected on parse and surfaces as a `RetiredNamespace` error pointing
operators at `braze-sync templatize`.

## How resolution works

For every `apply` and `diff`:

1. braze-sync `GET`s the live remote body for the resource.
2. For each `__BRAZESYNC__` in a `| lid:` argument, it identifies the
   **URL anchor** (the surrounding `<a href>`, VML/SVG `href`, or
   bare URL in plaintext) and pairs it with the matching anchor in
   the remote body to lift the live `lid` value. Multiple
   placeholders sharing one URL consume distinct remote values in
   template appearance order.
3. For each `__BRAZESYNC__` in a `| id:` argument inside a
   `{{content_blocks.${NAME} ...}}` include, it looks up the live
   `cb_id` under the same `${NAME}` in the remote body.
4. The resolved body is what diff compares against remote and what
   apply POSTs.

Because Git only carries the placeholder, dashboard edits that
reassign `lid` / `cb_id` are invisible to `diff` — only real template
structure changes show up.

## New resources (no remote yet)

When a resource doesn't exist in Braze yet, there is no remote body to
correlate against. braze-sync applies a controlled fallback:

- **`lid`**: derived from the URL path tail of the surrounding anchor,
  slug-normalized (e.g. `https://example.com/spring-sale` →
  `spring_sale`). Repeated slugs are disambiguated with `_2`, `_3`, …
  URL-less placeholders fall back to positional `lid_1`, `lid_2`, …
  Braze may reassign on first dashboard open; the next apply picks up
  the reassigned value via the remote-resolution path above.
- **`cb_id`**: the `| id: '__BRAZESYNC__'` filter is stripped
  entirely, leaving the documented form `{{content_blocks.${NAME}}}`.
  Braze derives an internal `cb_id` on save.

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
  in your existing files and rewrites them to `__BRAZESYNC__`.
  Idempotent; safe to re-run.

## Migration from v0.15.x

v0.15.x used the keyed form `__BRAZESYNC.<type>.<key>__`. v0.16
removes it entirely. Re-run `braze-sync templatize` against each
workspace before upgrading — that regenerates bodies with the new
anonymous token. The v0.15 envelope is surfaced as a fatal error if
it ever reaches resolve, so a partial migration fails loudly rather
than shipping broken templates.

```bash
# On v0.15 (or earlier), re-templatize from the raw remote body to
# emit the v0.16 anonymous form:
braze-sync export --env=prod        # pull raw remote bodies
braze-sync templatize               # rewrite to __BRAZESYNC__
```

## Known limitations

- **Subject / preheader `lid` is resolved positionally.** Those fields
  have no URL anchor, so the Nth `__BRAZESYNC__` is paired with the
  Nth `| lid: '…'` value in the remote field. If the placeholder
  count and remote value count differ, the resolver emits a warning
  and any leftover placeholders fail at resolve time.
- **Two placeholders sharing one URL** are resolved positionally
  (template appearance order → remote appearance order). When a URL
  has multiple remote occurrences *and* multiple template
  placeholders, the resolver emits an ambiguity warning so a
  dashboard-side link reorder cannot silently miscorrelate.
- **Structural drift between local and remote** (e.g. the dashboard
  removed a tracked link your template still references) aborts the
  apply with a clear error pointing at the unresolvable placeholder.

All resolve-time warnings are written to stderr scoped by resource
(and field for email_template), so `URL anchor 'X' not found in remote
body`, `cb_id filter stripped for new resource`, and the positional /
ambiguity warnings above each carry the resource + field they came
from. Pipe stderr through your CI logger to capture them.
