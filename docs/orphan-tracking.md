# Orphan tracking

## The problem

`braze-sync` is a GitOps tool: the Git repository is the source of
truth, and `apply` reconciles Braze to match it. For most resources,
"reconcile" means create, update, *or delete* — the operator removes
a file from Git, and the tool removes the resource from Braze.

**Two Braze resources don't support delete at the API level**:

- **Content Block** — `/content_blocks/*` endpoints cover create, info,
  and update. There is no DELETE.
- **Email Template** — confirmed with Braze support on 2026-04-12:
  there is no DELETE endpoint for email templates either.

A naive GitOps tool would silently ignore this and let the Braze side
drift forever, or — worse — pretend the resource is gone when it
isn't. `braze-sync` does neither.

## Definition

A resource is an **orphan** when:

- It exists in Braze, *and*
- No corresponding file exists in Git.

`braze-sync diff` detects orphans for both content blocks and email
templates, and flags them explicitly.

## How `diff` shows orphans

```
📝 Content Block: legacy_promo
   ⚠ orphaned (exists in Braze, not in Git)

📧 Email Template: old_welcome
   ⚠ orphaned (exists in Braze, not in Git)
```

In `--format json`, orphans surface with `"orphan": true` on the
individual diff entry and in the `summary.orphan` count:

```json
{
  "version": 1,
  "summary": { "changed": 0, "in_sync": 4, "destructive": 0, "orphan": 2 },
  "diffs": [
    { "kind": "content_block",  "name": "legacy_promo",  "orphan": true },
    { "kind": "email_template", "name": "old_welcome",   "orphan": true }
  ]
}
```

Orphans **do not** trigger the drift exit code by themselves — an orphan
is a report, not a drift. Combine `--fail-on-drift` with explicit
orphan checks in CI if you want the build to block on them.

## How `apply` handles orphans

### Default: report-only

With no flags, `apply` lists orphans in the summary and makes **zero**
API calls for them. This is the right default: the tool cannot delete
the resource, and silently leaving stale data behind is not an option
either.

### `--archive-orphans`: rename in place

Passing `--archive-orphans` renames each orphan in Braze with a
date-stamped prefix:

```
legacy_promo  →  [ARCHIVED-2026-04-18] legacy_promo
```

The resource still exists in Braze — it just becomes obvious in the
dashboard that it has been retired. This is a **two-step
read-modify-write**:

1. `GET /content_blocks/info` (or the email equivalent) to fetch the
   current body.
2. `POST /content_blocks/update` with the prefixed name and the body
   from step 1.

If another operator edits the same resource in the Braze dashboard
between those two calls, the update clobbers their edit with the
pre-rename body. This race is acceptable for the single-operator
GitOps workflow `braze-sync` targets; it would lift with a
compare-and-swap header, which Braze's content-block API does not
currently expose.

### Deletion is never attempted

`braze-sync` will not pretend to delete what it cannot delete. There is
no `--delete-orphans` flag and there will not be one until Braze ships
a DELETE endpoint.

## Unarchiving

"Unarchiving" is just `export`:

1. Rename the archived resource in the Braze dashboard (drop the
   `[ARCHIVED-YYYY-MM-DD]` prefix).
2. Run `braze-sync export`.
3. Commit the newly written file.

`braze-sync` never inspects the prefix itself — it's purely a visual
marker for humans.

## Why not a state file?

Other GitOps tools solve this with a separate state file that tracks
"deleted" resources. `braze-sync` deliberately does not:

- A state file means a second source of truth, and the `stateless-first`
  design principle rules that out.
- The Braze workspace itself is authoritative about what exists. If a
  resource is there, it's there; pretending otherwise is how teams get
  into trouble.
- `[ARCHIVED-YYYY-MM-DD]` is legible without any `braze-sync`
  tooling — a dashboard-only operator can see at a glance which
  resources are retired.

## Operator checklist

Before adopting `--archive-orphans` in CI:

- [ ] Confirm there's no other workflow that matches on *exact* content
      block or email template names — archiving changes the name.
- [ ] Confirm no campaign or canvas references the block by name. Braze
      resolves references at send time; a renamed block will break
      anything that still references the old name.
- [ ] Decide on a recovery procedure (see "Unarchiving" above) and
      document it alongside your `braze-sync` runbook.

When in doubt, leave `--archive-orphans` off and let the default
report-only behavior surface orphans in PR review.
