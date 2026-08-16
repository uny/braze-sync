# CI integration

`braze-sync` is designed to run the same way in CI as on a laptop.
This page covers the two patterns most workspaces need:

1. **Drift detection** on every push — fail the build if Braze drifts
   from the checked-in state.
2. **Apply on merge** to `main` — promote changes through the usual
   pull-request review workflow.

Both patterns rely on the [frozen exit codes](../README.md#exit-codes)
and the `--format json` output, both stable at v1.0.

## Drift detection (every push)

Run `validate` and `diff --fail-on-drift` on every PR. `validate` is
local-only, so it works cleanly on forks that can't see repository
secrets.

```yaml
# .github/workflows/braze-drift.yml
name: braze-drift

on:
  pull_request:
  push:
    branches: [main]
  schedule:
    - cron: "0 6 * * *"  # daily re-check in case someone edits in the dashboard

jobs:
  validate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: uny/setup-braze-sync@v1
      - run: braze-sync validate

  drift:
    needs: validate
    runs-on: ubuntu-latest
    # Don't leak secrets onto fork PRs:
    if: github.event.pull_request.head.repo.full_name == github.repository || github.event_name != 'pull_request'
    steps:
      - uses: actions/checkout@v4
      - uses: uny/setup-braze-sync@v1
      - run: braze-sync diff --env prod --fail-on-drift
        env:
          BRAZE_PROD_API_KEY: ${{ secrets.BRAZE_PROD_API_KEY }}
```

Exit-code contract:

| Code | Meaning | CI action |
|:---:|:---|:---|
| `0` | In sync | Pass |
| `1` | Generic failure (I/O, parse, unmapped error) | Fail the build |
| `2` | Drift detected | Fail the build |
| `3` | `validate` caught a local issue | Fail the build |
| `4` | API key is invalid | Fail & page the operator |
| `5` | Rate limit retries exhausted | Retry the job |
| `6` | Destructive change blocked (see `apply --allow-destructive`) | Fail the build / block merge |
| `8` | Fallback gate: unmatched placeholder + unconsumed remote lid value (see `apply --allow-fallback`). Unlike code `2`, this fires unconditionally — plain `diff` has no opt-in flag for it | Fail the build / block merge |

## Apply on merge

Once a PR is merged to `main`, apply the changes. Keep `--confirm`
gated on the branch and run `apply` *without* `--allow-destructive`
first — anything destructive will surface as exit `6` and demand a
follow-up change that opts in explicitly.

```yaml
# .github/workflows/braze-apply.yml
name: braze-apply

on:
  push:
    branches: [main]

concurrency:
  group: braze-apply-prod
  cancel-in-progress: false

jobs:
  apply:
    runs-on: ubuntu-latest
    environment: braze-prod  # require manual approval
    steps:
      - uses: actions/checkout@v4
      - uses: uny/setup-braze-sync@v1
      - run: braze-sync apply --env prod --confirm --format json | tee apply.json
        env:
          BRAZE_PROD_API_KEY: ${{ secrets.BRAZE_PROD_API_KEY }}
      - uses: actions/upload-artifact@v4
        with:
          name: braze-apply-log
          path: apply.json
```

The `concurrency` block prevents two `apply` runs from racing on the
same workspace — Braze's API has no compare-and-swap for most
resources, so serializing writes matters.

To *permit* destructive changes, add a one-time workflow input and pass
`--allow-destructive` only when the operator explicitly opts in:

```yaml
on:
  workflow_dispatch:
    inputs:
      allow_destructive:
        type: boolean
        default: false

# ...
      - if: ${{ inputs.allow_destructive }}
        run: braze-sync apply --env prod --confirm --allow-destructive
      - if: ${{ !inputs.allow_destructive }}
        run: braze-sync apply --env prod --confirm
```

The fallback gate (exit `8`) works the same way: it fires when a
template placeholder had no matching remote value *and* the remote
body left a lid value unconsumed — a shape that can indicate a link
merge/reorder rather than a plain new link. Review the flagged
resource(s) in the `diff`/`apply` output before re-running with
`--allow-fallback`; there is no auto-retry path for this one.

## Consuming `--format json`

`diff --format json` and `apply --format json` both emit a
`version: 1` envelope frozen at v1.0. CI consumers should branch on
that field; a future v2 schema will bump it.

```bash
braze-sync diff --format json > diff.json
jq '.summary' diff.json
# {
#   "changed": 5,
#   "in_sync": 1,
#   "destructive": 1,
#   "orphan": 1
# }
```

`destructive` and `orphan` are the two counts worth surfacing in PR
comments — they are the changes the reviewer most needs to eyeball.

## Posting the diff into a PR comment

The table output is meant to be read by a human, but on a workspace
with a few hundred resources almost every line is `no drift`, which
buries the review material and eats into GitHub's 65536-character
comment limit. Pass `--only-drift` to keep just the blocks that need
attention:

```yaml
  # A second job in the drift-check workflow above.
  comment:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      pull-requests: write   # createComment needs this
    steps:
      - uses: actions/checkout@v4
      - uses: uny/setup-braze-sync@v1
      - run: braze-sync diff --env prod --only-drift > diff.txt
        env:
          BRAZE_PROD_API_KEY: ${{ secrets.BRAZE_PROD_API_KEY }}
      - uses: actions/github-script@v7
        with:
          script: |
            const fs = require('fs');
            let out = fs.readFileSync('diff.txt', 'utf8');
            // A comment body over 65536 chars is rejected outright, so a
            // workspace that drifts badly enough would post nothing at
            // all. Clip instead, and say so.
            const LIMIT = 65000;
            if (out.length > LIMIT) {
              out = out.slice(0, LIMIT) +
                '\n… truncated — see the full diff in the step log\n';
            }
            // Resource names come from Braze and may contain backticks,
            // so pick a fence longer than any run in the output rather
            // than hard-coding three.
            const runs = out.match(/`+/g) || [];
            const fence = '`'.repeat(Math.max(3, ...runs.map((r) => r.length + 1)));
            await github.rest.issues.createComment({
              issue_number: context.issue.number,
              owner: context.repo.owner,
              repo: context.repo.repo,
              body: `${fence}text\n${out}${fence}`,
            });
```

`--only-drift` drops only the blocks whose entire body is `no drift`.
An in-sync resource that still carries an informational line — a Custom
Attribute type mismatch, for example — stays in the output, and the
`Summary:` trailer keeps counting every resource, so no drifting
resource is missing from the comment. The flag does not affect
`--format json`.

One caveat on the redirect rather than the flag: `> diff.txt` captures
stdout only, and `diff` writes its fallback-`lid` notices and other
warnings to **stderr**. Append `2>&1` if the comment should carry those
too; otherwise they stay in the step log.

## Secrets hygiene

- API keys live in CI secrets, never in `braze-sync.config.yaml`.
- The config references the *name* of the env var (`api_key_env`);
  `braze-sync` loads the value into `secrecy::SecretString` so it
  cannot leak through tracing, `Debug`, or panic output.
- `validate` never touches the network, so it's safe to run from fork
  PRs that don't have access to repository secrets.
- `diff` and `apply` *do* hit the network — gate those jobs on
  `github.event.pull_request.head.repo.full_name == github.repository`
  (or the equivalent on your CI provider) to avoid exposing keys to
  untrusted forks.

## Scheduling tip

Even with PR-level drift checks, schedule a daily `diff --fail-on-drift`
against production. Dashboard edits are the dominant source of drift in
practice, and they bypass every PR-gated check you install.
