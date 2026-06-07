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
    {
      "kind": "content_block", "op": "unchanged",
      "name": "legacy_promo", "orphan": true
    },
    {
      "kind": "email_template", "op": "unchanged",
      "name": "old_welcome", "orphan": true,
      "subject_changed": false, "metadata_changed": false
    }
  ]
}
```

Orphans **do not** trigger the drift exit code by themselves — an orphan
is a report, not a drift. Combine `--fail-on-drift` with explicit
orphan checks in CI if you want the build to block on them.

## How `apply` handles orphans

### Always report-only

`apply` lists orphans in the diff report and makes **zero** API calls
for them. This is the only behavior: the tool cannot delete content
blocks or email templates (no DELETE endpoint), and it cannot safely
mutate them in place either. Both `diff` and `apply` print an always-on
report when orphans are present:

```
ℹ 2 Braze resource(s) not present in Git. Archive them in the Braze
  dashboard if intended, or add them to exclude_patterns to keep them.
```

### Why braze-sync does not auto-archive

Earlier versions had an `--archive-orphans` flag that tried to retire
orphans by renaming them to `[ARCHIVED-YYYY-MM-DD] <name>`. It was
removed in v0.17.0 because it was non-functional for one resource kind
and unsafe for the other:

- **Content Block** — Braze rejects renaming a content block once it
  has been activated (`HTTP 400: "Content Block name cannot be changed
  after activation."`), and `/content_blocks/info` exposes no state
  field to gate on. The rename path was never wired; content block
  orphans were always report-only.
- **Email Template** — `POST /templates/email/update` accepts a name
  change unconditionally (no "in use" guard), `GET /templates/email/info`
  returns no usage/inclusion data, and **API-triggered campaigns
  resolve a template by name at send time** — so a rename can silently
  break live sends, and the tool has no signal to detect it. Renaming
  orphans we cannot prove are unused is a footgun, not a feature.

### Deletion is never attempted

`braze-sync` will not pretend to delete what it cannot delete. There is
no `--delete-orphans` flag and there will not be one until Braze ships
a DELETE endpoint.

> The genuinely safe prune path is unaffected: catalogs, catalog fields,
> and custom-attribute deprecation use real Braze delete/deprecate APIs
> and are gated behind `--allow-destructive`.

## Retiring an orphan

To retire an orphaned content block or email template, archive or rename
it manually in the Braze dashboard — that is the only path Braze allows.
If the resource is intentionally managed outside the repo, add it to
`exclude_patterns` so `diff` stops reporting it.

To bring an orphan back under Git management instead, run
`braze-sync export` and commit the newly written file.

## Why not a state file?

Other GitOps tools solve this with a separate state file that tracks
"deleted" resources. `braze-sync` deliberately does not:

- A state file means a second source of truth, and the `stateless-first`
  design principle rules that out.
- The Braze workspace itself is authoritative about what exists. If a
  resource is there, it's there; pretending otherwise is how teams get
  into trouble.
- The orphan report is recomputed from live Braze state on every run,
  so it never goes stale the way a checked-in state file would.

## Operator checklist

When the orphan report flags resources:

- [ ] Decide intent per orphan: retire it (archive/rename in the Braze
      dashboard) or keep it (add to `exclude_patterns`).
- [ ] Before retiring an email template, confirm no campaign or canvas
      references it by name. Braze resolves references at send time, so
      renaming or archiving a referenced template breaks live sends.
- [ ] Use `--fail-on-drift` plus an orphan check in CI if you want the
      build to block until orphans are resolved.
