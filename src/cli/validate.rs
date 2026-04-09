//! `braze-sync validate` — local-only structural and naming checks.
//!
//! v0.1.0 supports Catalog Schema. Other resource kinds emit a "not yet
//! implemented" warning.
//!
//! Validate is special among CLI commands: **it does not need a Braze
//! API key**. The whole point is "I want a pre-merge check that runs in
//! CI on a fork PR where the secret isn't available". `cli::run`
//! dispatches Validate directly from the parsed `ConfigFile`, skipping
//! the env-resolution stage that other commands go through.
//!
//! Issues are collected across the whole run and reported at the end,
//! so the user sees every problem in one pass instead of fix-and-rerun
//! cycles.

use crate::config::ConfigFile;
use crate::error::Error;
use crate::fs::catalog_io;
use crate::resource::ResourceKind;
use anyhow::anyhow;
use clap::Args;
use regex_lite::Regex;
use std::path::{Path, PathBuf};

use super::{selected_kinds, warn_unimplemented};

#[derive(Args, Debug)]
pub struct ValidateArgs {
    /// Limit validation to a specific resource kind.
    #[arg(long, value_enum)]
    pub resource: Option<ResourceKind>,
}

#[derive(Debug)]
struct ValidationIssue {
    path: PathBuf,
    message: String,
}

pub async fn run(args: &ValidateArgs, cfg: &ConfigFile, config_dir: &Path) -> anyhow::Result<()> {
    let kinds = selected_kinds(args.resource, &cfg.resources);

    let mut issues: Vec<ValidationIssue> = Vec::new();

    for kind in kinds {
        match kind {
            ResourceKind::CatalogSchema => {
                let catalogs_root = config_dir.join(&cfg.resources.catalog_schema.path);
                validate_catalog_schemas(
                    &catalogs_root,
                    cfg.naming.catalog_name_pattern.as_deref(),
                    &mut issues,
                )?;
            }
            other => warn_unimplemented(other),
        }
    }

    if issues.is_empty() {
        eprintln!("✓ All checks passed.");
        return Ok(());
    }

    eprintln!("✗ Validation found {} issue(s):", issues.len());
    for issue in &issues {
        eprintln!("  • {}: {}", issue.path.display(), issue.message);
    }

    // Wrap in Error::Config so exit_code_for maps it to exit 3
    // (config / argument error per §7.1) — semantically the user gave
    // bad input.
    Err(Error::Config(format!("{} validation issue(s) found", issues.len())).into())
}

fn validate_catalog_schemas(
    catalogs_root: &Path,
    name_pattern: Option<&str>,
    issues: &mut Vec<ValidationIssue>,
) -> anyhow::Result<()> {
    if !catalogs_root.exists() {
        // Empty project — nothing to validate but nothing wrong either.
        return Ok(());
    }
    if !catalogs_root.is_dir() {
        issues.push(ValidationIssue {
            path: catalogs_root.to_path_buf(),
            message: "expected directory for catalogs root".into(),
        });
        return Ok(());
    }

    // Compile the naming pattern once. A bad regex in the user's
    // config is a hard failure, not a per-catalog issue, so propagate
    // it via anyhow rather than pushing into `issues`.
    let pattern: Option<(String, Regex)> = match name_pattern {
        Some(p) => Some((
            p.to_string(),
            Regex::new(p).map_err(|e| anyhow!("invalid catalog_name_pattern regex {p:?}: {e}"))?,
        )),
        None => None,
    };

    for entry in std::fs::read_dir(catalogs_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            tracing::debug!(path = %entry.path().display(), "skipping non-directory entry");
            continue;
        }
        let dir = entry.path();
        let schema_path = dir.join("schema.yaml");
        if !schema_path.is_file() {
            // Catalog dir without schema.yaml: silently skip, mirroring
            // load_all_schemas. A future Phase B layout might have
            // items.csv-only dirs during partial edits.
            continue;
        }

        // Try to parse. On failure, record an issue and continue —
        // we want to surface every bad file in a single validate run,
        // not bail at the first one.
        let cat = match catalog_io::read_schema_file(&schema_path) {
            Ok(c) => c,
            Err(e) => {
                issues.push(ValidationIssue {
                    path: schema_path.clone(),
                    message: format!("parse error: {e}"),
                });
                continue;
            }
        };

        // Directory name must match the schema's `name:` field.
        // load_all_schemas treats this as a hard error; here we
        // downgrade to a soft issue so multiple files can be checked
        // in one run.
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        if cat.name != dir_name {
            issues.push(ValidationIssue {
                path: schema_path.clone(),
                message: format!(
                    "catalog name '{}' does not match its directory '{}'",
                    cat.name, dir_name
                ),
            });
        }

        // Naming pattern check (only if configured).
        if let Some((pattern_str, re)) = &pattern {
            if !re.is_match(&cat.name) {
                issues.push(ValidationIssue {
                    path: schema_path.clone(),
                    message: format!(
                        "catalog name '{}' does not match catalog_name_pattern '{}'",
                        cat.name, pattern_str
                    ),
                });
            }
        }
    }

    Ok(())
}
