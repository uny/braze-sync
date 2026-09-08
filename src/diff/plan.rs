//! Plan file schema and matching for `diff --plan-out` / `apply --plan`.
//!
//! # Contract
//!
//! A saved plan authorizes exactly this:
//!
//! > Apply the *current* local intent, provided that (a) the set of
//! > operations still has the shape the plan recorded, and (b) the
//! > remote side of every op that would overwrite a remote body is still
//! > what `diff` observed when the plan was generated.
//!
//! Both halves are enforced: (a) by the op multiset comparison in
//! [`PlanFile::diff_ops`], (b) by the [`RemotePrecondition`] carried on
//! each op and checked against a freshly-fetched remote at apply time.
//!
//! `Deprecate` / `Reactivate` are covered by (a) alone — they write a
//! boolean whose expected prior value the op direction already states, so
//! a remote toggle removes the op from the fresh diff rather than
//! surviving it with a stale precondition. See
//! [`PlanOpType::requires_precondition`].
//!
//! # What this does not promise
//!
//! - **It is not concurrency control.** The precondition is checked
//!   against apply's own fetch, then the write happens. As of 2026-09
//!   Braze's REST API offers no `If-Match` or expected-state token on
//!   any endpoint braze-sync writes to, so a lost update remains
//!   possible inside that window.
//! - **It does not freeze the change set.** Local edits between plan and
//!   apply are still applied as long as the op shapes match; the plan
//!   binds the remote preconditions, not the bytes that get written.
//! - **It does not detect identity replacement.** A remote object
//!   replaced by identical content under a different Braze ID projects to
//!   the same digest.
//! - **It does not cover catalog items.** Deleting a catalog deletes its
//!   items, which braze-sync never fetches; the schema digest says
//!   nothing about rows added since the plan.
//! - **`scope` pins the cluster, not the workspace.** `scope.environment`
//!   is a config label and `scope.api_endpoint` the REST host `diff`
//!   talked to; both are checked at apply time, so repointing an
//!   environment at a *different* Braze cluster is now caught. Swapping
//!   only the API key — same endpoint, different workspace — is not: the
//!   key is deliberately absent from the plan so it stays publishable as
//!   a CI artifact, and the plan therefore records no workspace identity.
//! - **It does not expire on its own.** A plan carries `generated_at`,
//!   but age is not evidence that the remote moved, so staleness is only
//!   a warning by default. `apply --max-plan-age` opts into treating the
//!   plan as an approval with an expiry — a separate policy with its own
//!   exit code, deliberately not part of the evidence above. See
//!   [`check_validity_window`].
//! - **The expiry trusts the plan's own clock.** `generated_at` is the
//!   only field `apply` consults that nothing outside the file
//!   corroborates: `version` is checked against
//!   [`CURRENT_PLAN_VERSION`], `scope` against the resolved config and
//!   endpoint, `ops` against a freshly computed diff, and each
//!   `precondition` against a fresh remote fetch — so editing any of
//!   those is caught. Editing `generated_at` is not. Anyone who can
//!   write the artifact between `diff --plan-out` and `apply --plan`
//!   can reset it to now and replay an approval of any age.
//!   `--max-plan-age` bounds *elapsed time*, not a tampered file; it is
//!   not a substitute for controlling who can write the artifact.
//! - **A digest is not confidentiality.** Predictable content can be
//!   guessed and confirmed. It is small enough to publish as a CI
//!   artifact, which is why the plan carries digests rather than payloads.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::diff::custom_attribute::CustomAttributeOp;
use crate::diff::{digest, DiffOp, DiffSummary, ResourceDiff};
use crate::resource::ResourceKind;

/// Bumped whenever the op vocabulary or a digest projection changes.
/// Older plans are rejected rather than migrated: a plan whose meaning
/// this binary cannot reproduce is not evidence of anything.
pub const CURRENT_PLAN_VERSION: u32 = 2;

/// Warn at apply time when the saved plan is older than this.
pub const STALE_PLAN_WARN_THRESHOLD: chrono::TimeDelta = chrono::TimeDelta::hours(24);

/// How far a plan's `generated_at` may sit in the future before apply
/// treats it as a broken clock rather than tolerable skew between the
/// machine that ran `diff` and the one running `apply`.
pub const PLAN_CLOCK_SKEW_TOLERANCE: chrono::TimeDelta = chrono::TimeDelta::minutes(5);

/// Why a plan fell outside the validity window `--max-plan-age` defines.
///
/// Not a form of plan drift: elapsed time is not evidence that the
/// remote moved. This is the expiry of an *approval*, which is why it
/// carries its own exit code.
#[derive(Debug, thiserror::Error)]
pub enum OutsideValidityWindow {
    #[error("generated {} ago, but --max-plan-age is {}", fmt_measured(*age), fmt_limit(*max_age))]
    Expired {
        age: chrono::TimeDelta,
        max_age: chrono::TimeDelta,
    },
    #[error(
        "generated_at is {} in the future (tolerance {}) — check the clock on \
         the machine that ran `diff`",
        fmt_measured(*ahead),
        fmt_limit(PLAN_CLOCK_SKEW_TOLERANCE)
    )]
    AheadOfClock { ahead: chrono::TimeDelta },
}

/// Render a configured *limit* the way `--max-plan-age` is written
/// (`1h 30m`). Limits come from a human-typed flag or a constant, so
/// truncating sub-second precision only ever restates what was asked for.
fn fmt_limit(d: chrono::TimeDelta) -> String {
    let secs = d.num_seconds().unsigned_abs();
    humantime::format_duration(std::time::Duration::from_secs(secs)).to_string()
}

/// Render a *measured* duration, rounding up to the next whole second.
///
/// Truncating here would print a falsehood at the boundary: a plan
/// 3600.4s old rejected against `--max-plan-age 1h` rendered as
/// "generated 1h ago, but --max-plan-age is 1h", two equal values
/// offered as a rejection — and under the supported `--max-plan-age 0`
/// *every* rejection read "generated 0s ago, but --max-plan-age is 0s".
/// Rounding the measured side up keeps it strictly greater than the
/// limit it exceeded, which is the whole claim the message makes.
fn fmt_measured(d: chrono::TimeDelta) -> String {
    let millis = d.num_milliseconds().unsigned_abs();
    let secs = millis / 1000 + u64::from(millis % 1000 != 0);
    humantime::format_duration(std::time::Duration::from_secs(secs)).to_string()
}

/// Check `generated_at` against the validity window that `--max-plan-age`
/// defines: `generated_at - PLAN_CLOCK_SKEW_TOLERANCE <= now <=
/// generated_at + max_age`.
///
/// One window, both edges. A future `generated_at` is not a separate
/// policy — it is the lower edge, and it is rejected for the same reason
/// the upper edge is: the plan's age cannot be established, so there is
/// no evidence the approval is still current.
///
/// Only reachable when the caller passed `--max-plan-age`; without it
/// staleness stays a warning (see [`STALE_PLAN_WARN_THRESHOLD`]).
pub fn check_validity_window(
    generated_at: DateTime<Utc>,
    now: DateTime<Utc>,
    max_age: chrono::TimeDelta,
) -> Result<(), OutsideValidityWindow> {
    let age = now.signed_duration_since(generated_at);
    if age < -PLAN_CLOCK_SKEW_TOLERANCE {
        return Err(OutsideValidityWindow::AheadOfClock { ahead: -age });
    }
    if age > max_age {
        return Err(OutsideValidityWindow::Expired { age, max_age });
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanFile {
    pub version: u32,
    pub generated_at: DateTime<Utc>,
    pub braze_sync_version: String,
    pub scope: PlanScope,
    pub ops: Vec<PlanOp>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanScope {
    pub environment: String,
    /// The Braze API endpoint `diff` actually talked to, as
    /// [`url::Url`] normalized it. Required: an environment *name* is a
    /// config label that can be repointed at a different Braze cluster
    /// between plan and apply, so the name alone is not evidence about
    /// where the plan's observations came from.
    pub api_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ResourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The state the remote side of an op must still be in for the plan to
/// authorize it.
///
/// Digests cover the surface `apply` would overwrite — see
/// [`crate::diff::digest`]. Read-only fields are deliberately outside the
/// projection: a remote edit to one cannot be clobbered, so it is not a
/// precondition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state", content = "digest")]
pub enum RemotePrecondition {
    /// The resource must still not exist remotely. Carried by `Add`.
    Absent,
    /// The remote object must still project to this blake3 hex digest.
    /// Carried by `Modify` and `DestructiveDelete`.
    Digest(String),
}

/// Length of the hex form of a blake3 digest, which is what
/// [`crate::diff::digest`] emits.
const DIGEST_HEX_LEN: usize = 64;

/// Whether a plan-supplied string is shaped like one of our digests.
/// The plan file is operator-supplied input, so the *shape* is checked
/// rather than assumed — see [`PlanFile::validate`].
fn is_digest_hex(s: &str) -> bool {
    s.len() == DIGEST_HEX_LEN && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl RemotePrecondition {
    /// Short human-readable form for drift reporting.
    ///
    /// Truncates by *character*, not by byte: `validate` rejects a
    /// malformed digest before this can be reached, but this is a public
    /// method on operator-supplied data and must not be one edit away
    /// from panicking on a split codepoint.
    pub fn describe(&self) -> String {
        match self {
            Self::Absent => "absent".to_string(),
            Self::Digest(d) => format!("digest {}…", d.chars().take(12).collect::<String>()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanOp {
    pub kind: ResourceKind,
    pub name: String,
    pub op: PlanOpType,
    /// `None` only for ops that write nothing to the resource's remote
    /// body — see [`PlanOpType::requires_precondition`]. Never "we could
    /// not compute one": a v2 plan missing a required precondition is
    /// rejected as malformed by [`PlanFile::validate`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition: Option<RemotePrecondition>,
}

impl PlanOp {
    /// The pairing key for [`PlanFile::diff_ops`]. Deliberately excludes
    /// `precondition`: ops are matched by shape first, then their
    /// preconditions are compared, so a drifted remote reads as "this op
    /// changed underneath you" rather than as an unrelated op appearing
    /// and another disappearing.
    fn shape(&self) -> (ResourceKind, &str, PlanOpType) {
        (self.kind, self.name.as_str(), self.op)
    }
}

/// The coarse op classification used for plan locking. Field-level
/// payloads are deliberately excluded so the plan file stays safe to
/// publish as a CI artifact and so the apply-time comparison tolerates
/// benign *local* edits made between plan and apply. Remote drift is
/// caught by [`RemotePrecondition`] instead, which needs only a digest.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PlanOpType {
    Add,
    Modify,
    DestructiveDelete,
    Orphan,
    Deprecate,
    Reactivate,
}

impl PlanOpType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Modify => "modify",
            Self::DestructiveDelete => "destructive_delete",
            Self::Orphan => "orphan",
            Self::Deprecate => "deprecate",
            Self::Reactivate => "reactivate",
        }
    }

    /// Whether a v2 plan op of this type must carry a precondition.
    ///
    /// `Orphan` is report-only and its remote body is never even fetched
    /// (`cli::diff` issues `/info` only for names present on both sides).
    /// `Deprecate` / `Reactivate` write a boolean whose expected prior
    /// value is implied by the op direction itself.
    pub fn requires_precondition(self) -> bool {
        match self {
            Self::Add | Self::Modify | Self::DestructiveDelete => true,
            Self::Orphan | Self::Deprecate | Self::Reactivate => false,
        }
    }
}

impl PlanFile {
    pub fn from_summary(
        summary: &DiffSummary,
        environment: impl Into<String>,
        api_endpoint: impl Into<String>,
        resource: Option<ResourceKind>,
        name: Option<String>,
    ) -> Self {
        Self {
            version: CURRENT_PLAN_VERSION,
            generated_at: Utc::now(),
            braze_sync_version: env!("CARGO_PKG_VERSION").to_string(),
            scope: PlanScope {
                environment: environment.into(),
                api_endpoint: api_endpoint.into(),
                resource,
                name,
            },
            ops: collect_ops(summary),
        }
    }

    /// Read a plan, gating the schema parse on `version`.
    ///
    /// The version is probed on its own first so a plan from an older
    /// format fails with "regenerate", not with whichever field this
    /// binary's schema happens to have gained since. Every other field
    /// is then required: a plan that cannot be fully parsed carries no
    /// evidence this binary can check.
    pub fn read_from(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        let invalid =
            |e: serde_json::Error| std::io::Error::new(std::io::ErrorKind::InvalidData, e);

        #[derive(Deserialize)]
        struct VersionProbe {
            version: u32,
        }
        let probe: VersionProbe = serde_json::from_slice(&bytes).map_err(invalid)?;
        if probe.version != CURRENT_PLAN_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "plan file version {} is not supported by this binary \
                     (expected {}). Regenerate with `diff --plan-out` and review \
                     the new plan — an older plan carries no evidence this binary \
                     can check.",
                    probe.version, CURRENT_PLAN_VERSION,
                ),
            ));
        }

        serde_json::from_slice(&bytes).map_err(invalid)
    }

    /// Serialize and write `path`, atomically where that means
    /// anything — see the last paragraph for where it does not.
    ///
    /// The bytes go to a sibling temporary file which is then `rename`d
    /// over `path`, so an interrupted run leaves either the previous plan
    /// or no plan, never a half-written one. The temporary file has to be
    /// a *sibling*: `rename` across filesystems fails with `EXDEV`.
    ///
    /// *Atomic* is a claim about visibility, not about durability: the
    /// `sync_all` below makes the plan's own bytes durable before the
    /// rename, but the directory entry the rename creates is not synced,
    /// so a machine that loses power just after a successful `write_to`
    /// can come back holding the *previous* plan. That stays inside the
    /// guarantee above — previous plan or no plan — and callers who need
    /// the new plan to survive a power cut need more than this function.
    ///
    /// A run killed between creating the temporary file and the rename
    /// leaves that sibling behind. It is never mistaken for a plan —
    /// nothing reads it and its name is not the one that was asked for —
    /// but nothing reaps it either, so N interrupted runs leave N of
    /// them, and a job collecting `plan*` as an artifact picks them up.
    ///
    /// The plan is a fresh inode on every write, so it takes the
    /// umask-derived mode a newly created file gets rather than the mode
    /// of a plan already at `path` — which can *loosen* a plan that had
    /// been chmod'd tighter. That applies to a symlink's target as much
    /// as to `path` itself.
    ///
    /// A symlink at `path` is followed and the file it points at is
    /// replaced, so the link survives — which means the *target's*
    /// inode and mode are what change, not the link's. A link whose
    /// target is simply absent (`ENOENT`) is the exception and is
    /// replaced by the plan itself. Every *other* way of failing to
    /// stat the destination — `EACCES` on a directory along the chain,
    /// `ELOOP`, `ENOTDIR`, `ENAMETOOLONG` — is surfaced as the error it
    /// is, exactly as the plain write surfaced it: replacing the link
    /// there would destroy a live link and report success.
    ///
    /// Creating a sibling also asks more of `path` than a plain write
    /// did: the parent directory must be writable — for a symlink, the
    /// *target's* parent, so a read-only directory holding a writable
    /// plan no longer works — and the resolved name must leave ~29 bytes
    /// of headroom under `NAME_MAX` for the suffix. A destination that
    /// cannot be replaced at all, because it is a device node, FIFO or
    /// socket, is written *through* instead and keeps every guarantee it
    /// had before, which is none of the above. A mount point is a
    /// regular file and so takes the rename route, where it fails.
    ///
    /// `/dev/stdout` splits on what the shell did with fd 1. Piped or on
    /// a tty it resolves to a stream and is written through. **Redirected
    /// to a regular file it resolves to that file**, and where the kernel
    /// and `realpath` agree on which file that is — Linux, via
    /// `/proc/self/fd/1` — it takes the rename route, so the redirect
    /// target is replaced by a new inode and fd 1 is left on the old one.
    /// Where they do not agree, the plan is written through the name that
    /// was asked for instead.
    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_plan_bytes(path, &json)
    }

    /// Compare saved ops against `fresh` as a multiset over
    /// `(kind, name, op_type)`, order-independent, then compare the
    /// preconditions of the pairs that matched.
    ///
    /// `collect_ops` is invariant-by-design unique per `(kind, name)`
    /// today, but we use a merge walk rather than a set so duplicates
    /// would surface as drift instead of being silently collapsed.
    pub fn diff_ops(&self, fresh: &[PlanOp]) -> PlanOpsDiff {
        let mut saved: Vec<&PlanOp> = self.ops.iter().collect();
        let mut fresh_sorted: Vec<&PlanOp> = fresh.iter().collect();
        saved.sort_by(|a, b| a.shape().cmp(&b.shape()));
        fresh_sorted.sort_by(|a, b| a.shape().cmp(&b.shape()));
        let (mut i, mut j) = (0, 0);
        let mut missing = Vec::new();
        let mut extra = Vec::new();
        let mut precondition_drift = Vec::new();
        while i < saved.len() && j < fresh_sorted.len() {
            match saved[i].shape().cmp(&fresh_sorted[j].shape()) {
                std::cmp::Ordering::Less => {
                    missing.push(saved[i].clone());
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    extra.push(fresh_sorted[j].clone());
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    // Equal shapes imply equal op types, and both
                    // `validate` (saved) and `classify` (fresh) pin the
                    // precondition kind to the op type — so either both
                    // sides carry one or neither does.
                    debug_assert_eq!(
                        saved[i].precondition.is_some(),
                        fresh_sorted[j].precondition.is_some(),
                        "precondition presence must follow the op type",
                    );
                    if let (Some(expected), Some(found)) =
                        (&saved[i].precondition, &fresh_sorted[j].precondition)
                    {
                        if expected != found {
                            precondition_drift.push(PreconditionDrift {
                                op: saved[i].clone(),
                                expected: expected.clone(),
                                found: found.clone(),
                            });
                        }
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        missing.extend(saved[i..].iter().map(|&op| op.clone()));
        extra.extend(fresh_sorted[j..].iter().map(|&op| op.clone()));
        PlanOpsDiff {
            missing,
            extra,
            precondition_drift,
        }
    }

    /// Reject a plan this binary cannot check.
    ///
    /// A missing precondition on an op that writes to a remote body is
    /// treated as a malformed plan, never as "skip the comparison for
    /// this op" — silently degrading to the pre-v2 shape-only check is
    /// exactly the failure mode v2 exists to remove.
    ///
    /// A digest that is not one of ours is rejected here for the same
    /// reason, and because everything downstream — the comparison, the
    /// drift report — is entitled to assume the shape this checks.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();
        for op in &self.ops {
            let expected = match (op.op.requires_precondition(), op.op) {
                (false, _) => None,
                (true, PlanOpType::Add) => Some("absent"),
                (true, _) => Some("a digest"),
            };
            match (expected, &op.precondition) {
                (None, None) => {}
                (Some("absent"), Some(RemotePrecondition::Absent)) => {}
                (Some("a digest"), Some(RemotePrecondition::Digest(d))) if is_digest_hex(d) => {}
                (Some("a digest"), Some(RemotePrecondition::Digest(_))) => problems.push(format!(
                    "{} {}: op `{}` carries a malformed digest \
                     (expected {DIGEST_HEX_LEN} lowercase hex characters)",
                    op.kind.as_str(),
                    op.name,
                    op.op.as_str(),
                )),
                (None, Some(_)) => problems.push(format!(
                    "{} {}: op `{}` must not carry a remote precondition",
                    op.kind.as_str(),
                    op.name,
                    op.op.as_str(),
                )),
                (Some(want), found) => problems.push(format!(
                    "{} {}: op `{}` requires {} precondition, found {}",
                    op.kind.as_str(),
                    op.name,
                    op.op.as_str(),
                    want,
                    match found {
                        None => "none".to_string(),
                        Some(RemotePrecondition::Absent) => "absent".to_string(),
                        Some(RemotePrecondition::Digest(_)) => "a digest".to_string(),
                    },
                )),
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

/// A shape-matched op pair whose remote precondition no longer holds:
/// the live Braze state moved between plan and apply.
#[derive(Debug, Clone)]
pub struct PreconditionDrift {
    pub op: PlanOp,
    pub expected: RemotePrecondition,
    pub found: RemotePrecondition,
}

/// Result of comparing a saved plan's ops against a freshly-computed list.
#[derive(Debug, Default)]
pub struct PlanOpsDiff {
    /// In saved plan but not in fresh (resolved or absorbed remotely).
    pub missing: Vec<PlanOp>,
    /// In fresh but not in saved plan (new drift since plan).
    pub extra: Vec<PlanOp>,
    /// Same op shape, but the remote is no longer what the plan observed.
    pub precondition_drift: Vec<PreconditionDrift>,
}

impl PlanOpsDiff {
    pub fn is_match(&self) -> bool {
        self.missing.is_empty() && self.extra.is_empty() && self.precondition_drift.is_empty()
    }
}

/// Convert a `DiffSummary` into the plan-op list. Skips non-actionable
/// diffs (Tag drift, Custom Attribute metadata-only, Unchanged) so the
/// plan-lock vocabulary matches the set of operations apply can actually
/// perform.
pub fn collect_ops(summary: &DiffSummary) -> Vec<PlanOp> {
    let mut out = Vec::new();
    for diff in &summary.diffs {
        let Some((op, precondition)) = classify(diff) else {
            continue;
        };
        out.push(PlanOp {
            kind: diff.kind(),
            name: diff.name().to_string(),
            op,
            precondition,
        });
    }
    out.sort_by(|a, b| a.shape().cmp(&b.shape()));
    out
}

/// Classify one diff into its plan op and the remote state that op
/// presupposes.
///
/// The precondition always comes from the `from` (remote) side. The `to`
/// side is local intent, which the plan deliberately does not bind — see
/// the module contract.
fn classify(diff: &ResourceDiff) -> Option<(PlanOpType, Option<RemotePrecondition>)> {
    match diff {
        ResourceDiff::CatalogSchema(d) => match &d.op {
            DiffOp::Added(_) => Some((PlanOpType::Add, Some(RemotePrecondition::Absent))),
            DiffOp::Removed(r) => {
                Some((PlanOpType::DestructiveDelete, digest_of(r, digest::catalog)))
            }
            // A field removal rewrites the schema destructively even
            // though the catalog itself is `Modified`. Classify on what
            // apply will actually do, so the plan's vocabulary and the
            // `--allow-destructive` gate agree.
            DiffOp::Modified { from, .. } => {
                let op = if d.field_diffs.iter().any(|f| f.is_destructive()) {
                    PlanOpType::DestructiveDelete
                } else {
                    PlanOpType::Modify
                };
                Some((op, digest_of(from, digest::catalog)))
            }
            // `diff_schema` bases the top-level op solely on field-level
            // changes, so `Unchanged` here means there are none.
            DiffOp::Unchanged => None,
        },
        ResourceDiff::ContentBlock(d) => {
            classify_orphanable(d.orphan, &d.op, digest::content_block)
        }
        ResourceDiff::EmailTemplate(d) => {
            classify_orphanable(d.orphan, &d.op, digest::email_template)
        }
        ResourceDiff::CustomAttribute(d) => match &d.op {
            // The baseline these presuppose is a boolean the op
            // direction already states; a digest would add nothing.
            CustomAttributeOp::DeprecationToggled { to: true, .. } => {
                Some((PlanOpType::Deprecate, None))
            }
            CustomAttributeOp::DeprecationToggled { to: false, .. } => {
                Some((PlanOpType::Reactivate, None))
            }
            _ => None,
        },
        ResourceDiff::Tag(_) => None,
    }
}

fn digest_of<T>(remote: &T, project: fn(&T) -> String) -> Option<RemotePrecondition> {
    Some(RemotePrecondition::Digest(project(remote)))
}

fn classify_orphanable<T>(
    orphan: bool,
    op: &DiffOp<T>,
    project: fn(&T) -> String,
) -> Option<(PlanOpType, Option<RemotePrecondition>)> {
    if orphan {
        // Report-only, and the remote body is never fetched for an
        // orphan — there is nothing to take a precondition on.
        return Some((PlanOpType::Orphan, None));
    }
    match op {
        DiffOp::Added(_) => Some((PlanOpType::Add, Some(RemotePrecondition::Absent))),
        DiffOp::Modified { from, .. } => Some((PlanOpType::Modify, digest_of(from, project))),
        DiffOp::Removed(_) | DiffOp::Unchanged => None,
    }
}

/// Temp-name attempts before giving up. A collision needs a sibling
/// carrying both this process's pid and the same random suffix, which in
/// practice means a leftover from a crashed run rather than a live writer.
const TEMP_NAME_ATTEMPTS: u32 = 8;

fn invalid_path(msg: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, msg)
}

/// The directory the temp file for `path` belongs in.
///
/// It must be `path`'s own directory: `rename` across filesystems fails
/// with `EXDEV`.
fn temp_sibling_dir(path: &Path) -> std::io::Result<&Path> {
    match path.parent() {
        // A bare file name (`plan.json`) parents to `""`, which nothing
        // can be created in.
        Some(parent) if parent.as_os_str().is_empty() => Ok(Path::new(".")),
        Some(parent) => Ok(parent),
        None => Err(invalid_path(format!(
            "{} has no parent directory",
            path.display()
        ))),
    }
}

/// Whether `meta` describes a sink that has to be written *through*
/// rather than replaced.
///
/// A device node, FIFO or socket is a stream with an identity of its own
/// that outlives any one write. `rename` does not update such a thing,
/// it unlinks it and leaves a regular file where it was: as root,
/// `--plan-out /dev/null` would replace `/dev/null` for every process on
/// the box. Atomic replacement has no meaning for these, so they keep
/// the plain write they have always had.
///
/// A directory is deliberately not one of these. Writing a plan to one
/// fails under either route, and diverting it would only change which
/// error it gets.
#[cfg(unix)]
fn is_stream_sink(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;

    let file_type = meta.file_type();
    file_type.is_char_device()
        || file_type.is_block_device()
        || file_type.is_fifo()
        || file_type.is_socket()
}

/// Windows has no destination `std` can recognise as one of these, and
/// `rename` there replaces a directory entry the same way, so every
/// destination takes the atomic route.
#[cfg(not(unix))]
fn is_stream_sink(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Whether two `Metadata` describe the same file.
///
/// Used to check that `realpath` and the kernel agree about where a
/// symlink leads before a `rename` acts on `realpath`'s answer.
#[cfg(unix)]
fn is_same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Windows exposes no stable file identity through `Metadata`
/// (`file_index` is unstable behind `windows_by_handle`), so the check
/// there is just that the canonical path still resolves — which the
/// caller has already established by stat'ing it successfully.
#[cfg(not(unix))]
fn is_same_file(_a: &std::fs::Metadata, _b: &std::fs::Metadata) -> bool {
    true
}

/// Write `bytes` to `path`, atomically where that means anything.
///
/// Symlinks are resolved by the kernel rather than by walking
/// `read_link` here. That is not a shortcut: on Linux `/dev/stdout` is
/// `/proc/self/fd/1`, whose target for a piped stdout reads as
/// `pipe:[12345]` — a string that is not a path and cannot be opened by
/// name. Walking the chain would take that for a destination and create
/// a file called `pipe:[12345]`.
fn write_plan_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,

        // Nothing at `path` — a new plan, or a symlink whose target is
        // absent, which is replaced by the plan rather than having that
        // target created the way the plain write did. It is the one
        // destination whose old behaviour is deliberately not preserved;
        // a plan is an output path, not a mailbox, so a link to a file
        // nobody has created is not a workflow worth reconstructing.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return replace_atomically(path, bytes)
        }

        // Any *other* stat failure — `EACCES` on a directory along the
        // target's chain, `ELOOP`, `ENOTDIR`, `ENAMETOOLONG` — leaves
        // the destination unclassified. Note that this covers links that
        // dangle for a reason other than a missing final component, so
        // "a dangling link is replaced" holds for `ENOENT` alone. The
        // error is surfaced, as the plain write surfaced it: falling
        // through to the rename would replace a live symlink with a
        // regular file and report success, which is the damage this
        // routing exists to prevent.
        Err(e) => return Err(e),
    };

    // Written through under the name that was asked for, so the kernel
    // resolves the chain.
    if is_stream_sink(&meta) {
        return std::fs::write(path, bytes);
    }

    // A symlink to an ordinary file points *at* the artifact rather than
    // being it: replacing the link would leave whatever reads the target
    // reading a plan that silently stops being updated. So the rename
    // targets what it points at, and the link survives.
    //
    // `canonicalize` is `realpath(3)`, an independent resolution rather
    // than a reuse of the stat above, and the two can disagree: on macOS
    // `canonicalize("/dev/stdout")` under `> plan.json` answers
    // `/dev/fd/plan.json`, a name that does not exist, and on Linux it
    // fails outright when fd 1 holds an unlinked file. Both would put
    // the temp sibling somewhere the caller never named. So the answer
    // is only used when a fresh stat says it is the same file the
    // routing decision was made about; otherwise the plain write goes
    // through the name that was asked for.
    if path.is_symlink() {
        if let Ok(real) = std::fs::canonicalize(path) {
            if std::fs::metadata(&real).is_ok_and(|m| is_same_file(&meta, &m)) {
                return replace_atomically(&real, bytes);
            }
        }
        return std::fs::write(path, bytes);
    }

    // An ordinary file, or a directory (which fails in the rename, as it
    // did in the write).
    replace_atomically(path, bytes)
}

/// Replace `path` with `bytes` via a sibling temp file and `rename`.
///
/// Hand-rolled rather than pulled from `tempfile`: that crate would add
/// `rustix` / `errno` / `getrandom` to the binary for the create-flush-
/// rename below, and it creates temp files 0o600, which would tighten the
/// mode of a plan meant to be read as a CI artifact.
fn replace_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = temp_sibling_dir(path)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| invalid_path(format!("{} does not name a file", path.display())))?;

    let (mut file, temp_path) = create_temp_sibling(dir, file_name)?;

    // The temp file exists from here on; every path out of this function
    // either renames it into place or removes it.
    let written = file.write_all(bytes).and_then(|()| {
        // `rename` buys atomicity, not durability: without this a crash
        // just after it can leave the plan present but empty, which is
        // the same symptom by another route.
        file.sync_all()
    });
    // Windows will not rename a file that is still open.
    drop(file);

    let result = written.and_then(|()| std::fs::rename(&temp_path, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// Create a new file in `dir` whose name is derived from `file_name`.
///
/// `create_new` is what keeps two concurrent writers off each other's
/// temp file, so a name that is already taken is retried rather than
/// truncated.
fn create_temp_sibling(dir: &Path, file_name: &OsStr) -> std::io::Result<(File, PathBuf)> {
    let pid = std::process::id();
    for _ in 0..TEMP_NAME_ATTEMPTS {
        let mut name = file_name.to_os_string();
        name.push(format!(".tmp.{pid}.{:016x}", fastrand::u64(..)));
        let candidate = dir.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((file, candidate)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "could not create a temporary file next to {} in {TEMP_NAME_ATTEMPTS} attempts",
            dir.join(file_name).display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn validity_window_accepts_a_plan_inside_max_age() {
        assert!(check_validity_window(
            at("2026-09-07T00:00:00Z"),
            at("2026-09-07T00:59:00Z"),
            TimeDelta::hours(1),
        )
        .is_ok());
    }

    #[test]
    fn validity_window_accepts_a_plan_exactly_at_max_age() {
        // The bound is inclusive: `age > max_age` rejects, so a plan
        // that is exactly `max_age` old is still inside the window.
        assert!(check_validity_window(
            at("2026-09-07T00:00:00Z"),
            at("2026-09-07T01:00:00Z"),
            TimeDelta::hours(1),
        )
        .is_ok());
    }

    #[test]
    fn validity_window_rejects_a_plan_past_max_age() {
        let err = check_validity_window(
            at("2026-09-07T00:00:00Z"),
            at("2026-09-07T01:00:01Z"),
            TimeDelta::hours(1),
        )
        .unwrap_err();
        assert!(matches!(err, OutsideValidityWindow::Expired { .. }));
        assert!(err.to_string().contains("--max-plan-age"));
    }

    #[test]
    fn zero_max_age_rejects_any_elapsed_time() {
        // `--max-plan-age 0` is a coherent policy: regenerate now.
        assert!(check_validity_window(
            at("2026-09-07T00:00:00Z"),
            at("2026-09-07T00:00:01Z"),
            TimeDelta::zero(),
        )
        .is_err());
    }

    #[test]
    fn validity_window_tolerates_future_generated_at_within_skew() {
        // A CI runner a few minutes ahead of the applying machine must
        // not fail the build.
        assert!(check_validity_window(
            at("2026-09-07T00:04:00Z"),
            at("2026-09-07T00:00:00Z"),
            TimeDelta::hours(1),
        )
        .is_ok());
    }

    #[test]
    fn validity_window_rejects_future_generated_at_beyond_skew() {
        // Past the tolerance the age cannot be established at all, so
        // there is no evidence the approval is current — same rejection
        // as expiry, not a warning.
        let err = check_validity_window(
            at("2026-09-07T00:06:00Z"),
            at("2026-09-07T00:00:00Z"),
            TimeDelta::hours(1),
        )
        .unwrap_err();
        assert!(matches!(err, OutsideValidityWindow::AheadOfClock { .. }));
        assert!(err.to_string().contains("clock"));
    }

    #[test]
    fn a_future_plan_is_rejected_even_when_max_age_is_generous() {
        // The lower edge does not move with `max_age`: a huge window
        // must not launder a broken clock.
        assert!(check_validity_window(
            at("2026-09-07T01:00:00Z"),
            at("2026-09-07T00:00:00Z"),
            TimeDelta::days(365),
        )
        .is_err());
    }

    #[test]
    fn validity_window_accepts_generated_at_exactly_at_skew_tolerance() {
        // The mirror of `validity_window_accepts_a_plan_exactly_at_max_age`
        // for the lower edge: `age < -TOLERANCE` rejects, so exactly
        // `PLAN_CLOCK_SKEW_TOLERANCE` ahead is still inside the window.
        // Without this, flipping `<` to `<=` would ship undetected.
        assert!(check_validity_window(
            at("2026-09-07T00:05:00Z"),
            at("2026-09-07T00:00:00Z"),
            TimeDelta::hours(1),
        )
        .is_ok());
    }

    #[test]
    fn a_measured_duration_is_rounded_up_so_it_never_equals_the_limit() {
        // A plan 3600.4s old against `--max-plan-age 1h` must not render
        // as "generated 1h ago, but --max-plan-age is 1h" — two equal
        // values offered as a rejection.
        let err = check_validity_window(
            at("2026-09-07T00:00:00Z"),
            at("2026-09-07T01:00:00.400Z"),
            TimeDelta::hours(1),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "generated 1h 1s ago, but --max-plan-age is 1h"
        );
    }

    #[test]
    fn zero_max_age_still_reports_a_nonzero_age() {
        // `--max-plan-age 0` is supported, so its rejection message is
        // the common case, not an edge: it must not read "generated 0s
        // ago, but --max-plan-age is 0s".
        let err = check_validity_window(
            at("2026-09-07T00:00:00Z"),
            at("2026-09-07T00:00:00.400Z"),
            TimeDelta::zero(),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "generated 1s ago, but --max-plan-age is 0s"
        );
    }

    #[test]
    fn ahead_of_clock_reports_the_measured_lead_and_the_configured_tolerance() {
        // Pins `fmt_measured` / `fmt_limit` on the other edge too: the
        // lead rounds up, the tolerance restates the constant verbatim.
        let err = check_validity_window(
            at("2026-09-07T00:06:00.400Z"),
            at("2026-09-07T00:00:00Z"),
            TimeDelta::hours(1),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "generated_at is 6m 1s in the future (tolerance 5m) — check the \
             clock on the machine that ran `diff`"
        );
    }

    use super::*;
    use crate::diff::catalog::CatalogSchemaDiff;
    use crate::diff::content_block::ContentBlockDiff;
    use crate::resource::{
        Catalog, CatalogField, CatalogFieldType, ContentBlock, ContentBlockState,
    };

    /// A distinct, *well-formed* digest per name. `validate` checks the
    /// hex shape, so a readable placeholder like `digest-of-hero` would
    /// make every fixture below a malformed plan.
    fn digest_like(name: &str) -> String {
        blake3::hash(name.as_bytes()).to_hex().to_string()
    }

    fn op(kind: ResourceKind, name: &str, op: PlanOpType) -> PlanOp {
        let precondition = match op {
            PlanOpType::Add => Some(RemotePrecondition::Absent),
            PlanOpType::Modify | PlanOpType::DestructiveDelete => {
                Some(RemotePrecondition::Digest(digest_like(name)))
            }
            _ => None,
        };
        PlanOp {
            kind,
            name: name.to_string(),
            op,
            precondition,
        }
    }

    fn plan_with(ops: Vec<PlanOp>) -> PlanFile {
        PlanFile {
            version: CURRENT_PLAN_VERSION,
            generated_at: Utc::now(),
            braze_sync_version: "test".into(),
            scope: PlanScope {
                environment: "dev".into(),
                api_endpoint: "https://rest.iad-01.braze.com/".into(),
                resource: None,
                name: None,
            },
            ops,
        }
    }

    /// Entries in `dir` that are not `plan.json` — i.e. temp siblings
    /// `replace_atomically` failed to clean up.
    fn stray_siblings(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "plan.json")
            .collect();
        names.sort();
        names
    }

    #[test]
    fn write_to_round_trips_through_read_from() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.json");
        let plan = plan_with(vec![op(
            ResourceKind::ContentBlock,
            "a",
            PlanOpType::Modify,
        )]);

        plan.write_to(&path).unwrap();
        let read = PlanFile::read_from(&path).unwrap();

        assert_eq!(read.ops, plan.ops);
        assert_eq!(read.scope.environment, plan.scope.environment);
        assert_eq!(stray_siblings(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn write_to_replaces_an_existing_plan_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.json");
        std::fs::write(&path, b"stale, and longer than the plan that replaces it").unwrap();

        let plan = plan_with(vec![op(ResourceKind::CatalogSchema, "x", PlanOpType::Add)]);
        plan.write_to(&path).unwrap();

        assert_eq!(PlanFile::read_from(&path).unwrap().ops, plan.ops);
        assert_eq!(stray_siblings(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn a_bare_file_name_puts_the_temp_file_in_the_current_directory() {
        // `Path::new("plan.json").parent()` is `Some("")`, which nothing
        // can be created in — `diff --plan-out plan.json` reaches here.
        assert_eq!(
            temp_sibling_dir(Path::new("plan.json")).unwrap(),
            Path::new(".")
        );
        assert_eq!(
            temp_sibling_dir(Path::new("out/plan.json")).unwrap(),
            Path::new("out")
        );
        temp_sibling_dir(Path::new("/")).unwrap_err();
    }

    #[test]
    fn write_to_fails_without_creating_anything_when_the_directory_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-dir");
        let plan = plan_with(vec![op(
            ResourceKind::ContentBlock,
            "a",
            PlanOpType::Modify,
        )]);

        plan.write_to(&missing.join("plan.json")).unwrap_err();

        assert!(!missing.exists());
        assert_eq!(stray_siblings(dir.path()), Vec::<String>::new());
    }

    /// `replace_atomically` claims every path out of it either renames the
    /// temp file into place or removes it. The removal is only reached
    /// when the rename fails *after* the temp file exists, which nothing
    /// else here exercises — a directory at `path` produces exactly that.
    #[test]
    fn write_to_removes_the_temp_file_when_the_rename_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.json");
        std::fs::create_dir(&path).unwrap();

        plan_with(vec![]).write_to(&path).unwrap_err();

        assert_eq!(stray_siblings(dir.path()), Vec::<String>::new());
    }

    /// The rename replaces the directory entry rather than the bytes of
    /// the inode already there — that substitution is what buys
    /// atomicity, and it is the one thing the `std::fs::write` this
    /// replaced could not do. A reader holding the old plan open goes on
    /// reading the old plan; under `write` it would have seen the new
    /// bytes, or a truncated prefix of them.
    #[test]
    fn write_to_leaves_a_reader_of_the_previous_plan_on_the_previous_plan() {
        use std::io::Read;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.json");
        std::fs::write(&path, b"the previous plan").unwrap();
        let mut previous = File::open(&path).unwrap();

        plan_with(vec![op(ResourceKind::CatalogSchema, "x", PlanOpType::Add)])
            .write_to(&path)
            .unwrap();

        let mut held = String::new();
        previous.read_to_string(&mut held).unwrap();
        assert_eq!(held, "the previous plan");
        assert_eq!(PlanFile::read_from(&path).unwrap().ops.len(), 1);
    }

    /// A symlink points *at* the artifact rather than being it, so the
    /// link survives and the file it names is what gets replaced.
    /// Replacing the link instead would leave whatever reads the target
    /// on a plan that silently stops being updated.
    ///
    /// The link is deliberately **relative and into a subdirectory**,
    /// and the target's inode is asserted to have been replaced. That
    /// combination is what makes this test able to fail: resolving with
    /// `read_link` instead of `canonicalize` yields the bare
    /// `sub/target.json`, which does not resolve from the process cwd,
    /// so the write would fall back to writing *through* the link — the
    /// target would hold the right bytes on the same inode, and every
    /// other assertion here would still pass.
    #[test]
    #[cfg(unix)]
    fn write_to_replaces_what_a_symlink_points_at_and_keeps_the_link() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let target = sub.join("target.json");
        std::fs::write(&target, b"not a plan").unwrap();
        let before = std::fs::metadata(&target).unwrap().ino();
        let path = dir.path().join("plan.json");
        std::os::unix::fs::symlink("sub/target.json", &path).unwrap();

        let plan = plan_with(vec![op(ResourceKind::CatalogSchema, "x", PlanOpType::Add)]);
        plan.write_to(&path).unwrap();

        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link must survive",
        );
        assert_eq!(PlanFile::read_from(&target).unwrap().ops, plan.ops);
        assert_ne!(
            before,
            std::fs::metadata(&target).unwrap().ino(),
            "the target must be replaced by the rename, not written through",
        );
        // The temp sibling belongs next to the *target*, and nothing is
        // left behind in either directory.
        assert_eq!(stray_siblings(dir.path()), vec!["sub".to_string()]);
        assert_eq!(stray_siblings(&sub), vec!["target.json".to_string()]);
    }

    /// A destination that exists but cannot be classified — here a link
    /// whose target sits under a directory the caller may not search —
    /// is an error, not an invitation to replace the link. Falling
    /// through to the rename would destroy a live symlink and still
    /// report success, which is the damage the routing exists to stop.
    #[test]
    #[cfg(unix)]
    fn write_to_errors_rather_than_replacing_a_link_it_cannot_stat() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        let target = locked.join("target.json");
        std::fs::write(&target, b"not a plan").unwrap();
        let path = dir.path().join("plan.json");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Root searches a 0o000 directory regardless, so there is no
        // unstattable destination to test with; skip rather than assert
        // the opposite of what this pins.
        let privileged = std::fs::metadata(&target).is_ok();

        let result = plan_with(vec![]).write_to(&path);

        // Restore before the tempdir is dropped, or the cleanup leaves
        // the directory behind.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        if privileged {
            return;
        }
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link must survive a destination that could not be classified",
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"not a plan");
    }

    /// The one destination whose pre-atomic behaviour is not preserved:
    /// the plain write created the target, the rename replaces the link.
    /// Pinned so the exception stays a decision rather than a surprise.
    #[test]
    #[cfg(unix)]
    fn write_to_replaces_a_dangling_symlink_rather_than_creating_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.json");
        std::os::unix::fs::symlink(dir.path().join("absent.json"), &path).unwrap();

        plan_with(vec![]).write_to(&path).unwrap();

        assert!(!std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!dir.path().join("absent.json").exists());
    }

    /// `/dev/stdout` in miniature: the destination is a symlink whose
    /// target is a stream. The link must not be followed to a rename —
    /// the stream is written through under the name that was asked for.
    #[test]
    #[cfg(unix)]
    fn write_to_writes_through_a_symlink_to_a_stream() {
        use std::os::unix::fs::FileTypeExt;

        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("sink");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let path = dir.path().join("plan.json");
        std::os::unix::fs::symlink(&fifo, &path).unwrap();

        let reader = std::thread::spawn({
            let fifo = fifo.clone();
            move || std::fs::read(fifo).unwrap()
        });

        let plan = plan_with(vec![op(ResourceKind::CatalogSchema, "x", PlanOpType::Add)]);
        plan.write_to(&path).unwrap();

        // Before the join, for the reason given on the FIFO test above.
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link must survive",
        );
        assert!(
            std::fs::symlink_metadata(&fifo)
                .unwrap()
                .file_type()
                .is_fifo(),
            "the FIFO must survive",
        );
        assert_eq!(
            serde_json::from_slice::<PlanFile>(&reader.join().unwrap())
                .unwrap()
                .ops,
            plan.ops,
        );
    }

    /// The routing table, asserted by stat alone so that a regression
    /// cannot damage the machine running the suite: a broken
    /// `is_stream_sink` under root would have `write_to` replace
    /// `/dev/null` itself, which is exactly what this exists to prevent.
    #[test]
    #[cfg(unix)]
    fn stream_sinks_are_the_destinations_a_rename_would_destroy() {
        let dir = tempfile::tempdir().unwrap();
        let regular = dir.path().join("plan.json");
        std::fs::write(&regular, b"{}").unwrap();

        // A bound socket has a name other processes are connected to,
        // exactly like the FIFO; the listener is held for the assertion
        // so the socket is still bound when it is stat'd.
        let socket_path = dir.path().join("plan.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();

        let sink = |path: &Path| is_stream_sink(&std::fs::metadata(path).unwrap());
        assert!(sink(Path::new("/dev/null")), "character device");
        assert!(sink(&socket_path), "socket");
        assert!(!sink(&regular), "regular file");
        // A directory has to keep taking the rename route: it is what
        // `write_to_removes_the_temp_file_when_the_rename_fails` uses to
        // reach the cleanup branch.
        assert!(!sink(dir.path()), "directory");
    }

    /// End-to-end on a sink that can be created in a tempdir. Under the
    /// unconditional rename this shipped with, the FIFO was unlinked and
    /// a regular file left in its place; the reader below would have
    /// blocked forever instead of seeing the plan.
    #[test]
    #[cfg(unix)]
    fn write_to_writes_through_a_fifo_rather_than_replacing_it() {
        use std::os::unix::fs::FileTypeExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.json");
        // `mkfifo(1)` rather than a libc dependency for one test.
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());

        // Opening a FIFO for writing blocks until a reader arrives, so
        // the reader has to be waiting before `write_to` is called.
        let reader = std::thread::spawn({
            let path = path.clone();
            move || std::fs::read(path).unwrap()
        });

        let plan = plan_with(vec![op(ResourceKind::CatalogSchema, "x", PlanOpType::Add)]);
        plan.write_to(&path).unwrap();

        // Checked before the join, not after: if the FIFO was replaced,
        // no writer ever opens it and the reader blocks forever. Failing
        // here keeps a regression a failing test rather than a hung CI
        // job — the abandoned thread does not hold up process exit.
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_fifo(),
            "the FIFO must survive the write",
        );

        let received = reader.join().unwrap();
        assert_eq!(
            serde_json::from_slice::<PlanFile>(&received).unwrap().ops,
            plan.ops,
        );
        assert_eq!(stray_siblings(dir.path()), Vec::<String>::new());
    }

    /// The plan is a CI artifact other steps read, so the rename must not
    /// quietly tighten its mode the way a 0o600 temp file would.
    #[test]
    #[cfg(unix)]
    fn write_to_gives_the_plan_the_mode_a_plain_write_would() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let reference = dir.path().join("reference");
        std::fs::write(&reference, b"{}").unwrap();

        let path = dir.path().join("plan.json");
        plan_with(vec![]).write_to(&path).unwrap();

        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), mode(&reference));
    }

    /// The other half of the mode story, and the half that can *loosen*
    /// permissions: the plan is a fresh inode, so it does not inherit the
    /// mode of a plan already at `path`. A 0o600 plan replaced under the
    /// usual 0o022 umask comes back 0o644. Under a 0o077 umask the two
    /// modes coincide and this only re-pins the previous test, which is
    /// why the loosening direction is spelled out in the CHANGELOG too.
    #[test]
    #[cfg(unix)]
    fn write_to_does_not_inherit_the_mode_of_the_plan_it_replaces() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let reference = dir.path().join("reference");
        std::fs::write(&reference, b"{}").unwrap();

        let path = dir.path().join("plan.json");
        std::fs::write(&path, b"{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        plan_with(vec![]).write_to(&path).unwrap();

        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), mode(&reference));
    }

    #[test]
    fn ops_match_is_order_independent() {
        let plan = plan_with(vec![
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
            op(ResourceKind::CatalogSchema, "x", PlanOpType::Add),
        ]);
        let fresh = vec![
            op(ResourceKind::CatalogSchema, "x", PlanOpType::Add),
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
        ];
        assert!(plan.diff_ops(&fresh).is_match());
    }

    #[test]
    fn ops_mismatch_when_op_kind_changes() {
        let plan = plan_with(vec![op(
            ResourceKind::ContentBlock,
            "a",
            PlanOpType::Modify,
        )]);
        let fresh = vec![op(ResourceKind::ContentBlock, "a", PlanOpType::Orphan)];
        let diff = plan.diff_ops(&fresh);
        assert!(!diff.is_match());
        assert_eq!(diff.missing.len(), 1);
        assert_eq!(diff.extra.len(), 1);
        assert!(diff.precondition_drift.is_empty());
    }

    #[test]
    fn duplicate_ops_are_treated_as_multiset() {
        // Hand-crafted: two identical ops on either side should match,
        // but `n` on one side and `n+1` on the other should surface as
        // a single extra (set semantics would collapse to a match).
        let plan = plan_with(vec![
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
        ]);
        let fresh_dup = vec![
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
        ];
        assert!(plan.diff_ops(&fresh_dup).is_match());

        let fresh_one = vec![op(ResourceKind::ContentBlock, "a", PlanOpType::Modify)];
        let diff = plan.diff_ops(&fresh_one);
        assert!(!diff.is_match(), "should detect missing duplicate");
        assert_eq!(diff.missing.len(), 1);
        assert!(diff.extra.is_empty());
    }

    #[test]
    fn round_trip_json() {
        let plan = PlanFile {
            version: CURRENT_PLAN_VERSION,
            generated_at: "2026-05-18T12:34:56Z".parse().unwrap(),
            braze_sync_version: "0.12.0".into(),
            scope: PlanScope {
                environment: "dev".into(),
                api_endpoint: "https://rest.iad-01.braze.com/".into(),
                resource: Some(ResourceKind::ContentBlock),
                name: None,
            },
            ops: vec![
                op(ResourceKind::ContentBlock, "hero", PlanOpType::Add),
                op(ResourceKind::ContentBlock, "promo", PlanOpType::Modify),
                op(ResourceKind::ContentBlock, "stale", PlanOpType::Orphan),
            ],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let round: PlanFile = serde_json::from_str(&json).unwrap();
        assert!(plan.diff_ops(&round.ops).is_match());
        assert_eq!(round.scope, plan.scope);
        assert_eq!(round.ops, plan.ops);
    }

    // -------------------------------------------------------------
    // Remote preconditions
    // -------------------------------------------------------------

    #[test]
    fn same_op_shape_with_changed_remote_is_drift() {
        // The #100 scenario in miniature: shape unchanged, remote moved.
        let plan = plan_with(vec![PlanOp {
            kind: ResourceKind::ContentBlock,
            name: "hero".into(),
            op: PlanOpType::Modify,
            precondition: Some(RemotePrecondition::Digest("aaa".into())),
        }]);
        let fresh = vec![PlanOp {
            kind: ResourceKind::ContentBlock,
            name: "hero".into(),
            op: PlanOpType::Modify,
            precondition: Some(RemotePrecondition::Digest("bbb".into())),
        }];
        let diff = plan.diff_ops(&fresh);
        assert!(!diff.is_match());
        assert!(diff.missing.is_empty(), "shape is unchanged");
        assert!(diff.extra.is_empty(), "shape is unchanged");
        assert_eq!(diff.precondition_drift.len(), 1);
    }

    #[test]
    fn validate_rejects_missing_digest() {
        let plan = plan_with(vec![PlanOp {
            kind: ResourceKind::ContentBlock,
            name: "hero".into(),
            op: PlanOpType::Modify,
            precondition: None,
        }]);
        let problems = plan.validate().unwrap_err();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("requires a digest"), "{problems:?}");
    }

    #[test]
    fn validate_rejects_wrong_precondition_kind() {
        let plan = plan_with(vec![
            PlanOp {
                kind: ResourceKind::CatalogSchema,
                name: "c".into(),
                op: PlanOpType::Add,
                precondition: Some(RemotePrecondition::Digest("x".into())),
            },
            PlanOp {
                kind: ResourceKind::ContentBlock,
                name: "orphaned".into(),
                op: PlanOpType::Orphan,
                precondition: Some(RemotePrecondition::Absent),
            },
        ]);
        let problems = plan.validate().unwrap_err();
        assert_eq!(problems.len(), 2);
        assert!(problems[0].contains("requires absent"), "{problems:?}");
        assert!(problems[1].contains("must not carry"), "{problems:?}");
    }

    #[test]
    fn validate_rejects_a_malformed_digest() {
        // Not hex — and byte 12 lands inside the '€', which is what
        // `describe` used to slice straight through.
        let plan = plan_with(vec![PlanOp {
            kind: ResourceKind::ContentBlock,
            name: "hero".into(),
            op: PlanOpType::Modify,
            precondition: Some(RemotePrecondition::Digest("aaaaaaaaaaa€zz".into())),
        }]);
        let problems = plan.validate().unwrap_err();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("malformed digest"), "{problems:?}");
    }

    #[test]
    fn describe_truncates_by_character_not_byte() {
        // `validate` rejects this shape before `describe` can see it;
        // `describe` is public, so it must not panic on it regardless.
        let p = RemotePrecondition::Digest("aaaaaaaaaaa€zz".into());
        assert_eq!(p.describe(), "digest aaaaaaaaaaa€…");
        assert_eq!(
            RemotePrecondition::Digest(digest_like("hero")).describe(),
            format!("digest {}…", &digest_like("hero")[..12]),
        );
    }

    #[test]
    fn validate_accepts_a_well_formed_plan() {
        let plan = plan_with(vec![
            op(ResourceKind::CatalogSchema, "c", PlanOpType::Add),
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
            op(ResourceKind::ContentBlock, "b", PlanOpType::Orphan),
            op(ResourceKind::CustomAttribute, "d", PlanOpType::Deprecate),
            op(
                ResourceKind::CatalogSchema,
                "e",
                PlanOpType::DestructiveDelete,
            ),
        ]);
        assert!(plan.validate().is_ok());
    }

    // -------------------------------------------------------------
    // classify
    // -------------------------------------------------------------

    fn field(name: &str, field_type: CatalogFieldType) -> CatalogField {
        CatalogField {
            name: name.into(),
            field_type,
        }
    }

    fn catalog(name: &str, fields: Vec<CatalogField>) -> Catalog {
        Catalog {
            name: name.into(),
            description: None,
            fields,
        }
    }

    #[test]
    fn catalog_field_removal_classifies_as_destructive() {
        let remote = catalog(
            "c",
            vec![
                field("id", CatalogFieldType::String),
                field("old", CatalogFieldType::Number),
            ],
        );
        let local = catalog("c", vec![field("id", CatalogFieldType::String)]);
        let d = crate::diff::catalog::diff_schema(Some(&local), Some(&remote)).unwrap();
        let (op, precondition) = classify(&ResourceDiff::CatalogSchema(d)).unwrap();
        assert_eq!(op, PlanOpType::DestructiveDelete);
        assert_eq!(
            precondition,
            Some(RemotePrecondition::Digest(digest::catalog(&remote))),
        );
    }

    #[test]
    fn catalog_field_addition_classifies_as_modify_with_remote_digest() {
        let remote = catalog("c", vec![field("id", CatalogFieldType::String)]);
        let local = catalog(
            "c",
            vec![
                field("id", CatalogFieldType::String),
                field("new", CatalogFieldType::Number),
            ],
        );
        let d = crate::diff::catalog::diff_schema(Some(&local), Some(&remote)).unwrap();
        let (op, precondition) = classify(&ResourceDiff::CatalogSchema(d)).unwrap();
        assert_eq!(op, PlanOpType::Modify);
        assert_eq!(
            precondition,
            Some(RemotePrecondition::Digest(digest::catalog(&remote))),
            "precondition must digest the remote, not the local, side",
        );
    }

    #[test]
    fn catalog_with_no_field_diffs_is_not_a_plan_op() {
        let c = catalog("c", vec![field("id", CatalogFieldType::String)]);
        let mut local = c.clone();
        local.description = Some("description-only edit".into());
        let d = crate::diff::catalog::diff_schema(Some(&local), Some(&c)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged));
        assert!(classify(&ResourceDiff::CatalogSchema(d)).is_none());
    }

    #[test]
    fn content_block_modify_digests_the_remote_side() {
        let remote = ContentBlock {
            name: "hero".into(),
            description: None,
            content: "remote".into(),
            tags: vec![],
            state: ContentBlockState::Active,
        };
        let mut local = remote.clone();
        local.content = "local".into();
        let d = crate::diff::content_block::diff(Some(&local), Some(&remote)).unwrap();
        let (op, precondition) = classify(&ResourceDiff::ContentBlock(d)).unwrap();
        assert_eq!(op, PlanOpType::Modify);
        assert_eq!(
            precondition,
            Some(RemotePrecondition::Digest(digest::content_block(&remote))),
        );
        assert_ne!(
            precondition,
            Some(RemotePrecondition::Digest(digest::content_block(&local))),
        );
    }

    #[test]
    fn orphan_carries_no_precondition() {
        let d = ContentBlockDiff::orphan("gone");
        let (op, precondition) = classify(&ResourceDiff::ContentBlock(d)).unwrap();
        assert_eq!(op, PlanOpType::Orphan);
        assert_eq!(precondition, None);
    }

    #[test]
    fn removed_catalog_digests_the_remote_resource() {
        let remote = catalog("gone", vec![field("id", CatalogFieldType::String)]);
        let d: CatalogSchemaDiff = crate::diff::catalog::diff_schema(None, Some(&remote)).unwrap();
        let (op, precondition) = classify(&ResourceDiff::CatalogSchema(d)).unwrap();
        assert_eq!(op, PlanOpType::DestructiveDelete);
        assert_eq!(
            precondition,
            Some(RemotePrecondition::Digest(digest::catalog(&remote))),
        );
    }
}
