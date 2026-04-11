//! `braze-sync validate` — local-only structural and naming checks.
//!
//! Runs without a Braze API key so CI on fork PRs (where the secret
//! isn't available) can still gate merges. Issues are collected across
//! the whole run and reported at the end so a single pass surfaces
//! every problem.

use crate::config::ConfigFile;
use crate::error::Error;
use crate::fs::{catalog_io, content_block_io};
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
            ResourceKind::ContentBlock => {
                let content_blocks_root = config_dir.join(&cfg.resources.content_block.path);
                validate_content_blocks(
                    &content_blocks_root,
                    cfg.naming.content_block_name_pattern.as_deref(),
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

    Err(Error::Config(format!("{} validation issue(s) found", issues.len())).into())
}

fn validate_catalog_schemas(
    catalogs_root: &Path,
    name_pattern: Option<&str>,
    issues: &mut Vec<ValidationIssue>,
) -> anyhow::Result<()> {
    let read_dir = match std::fs::read_dir(catalogs_root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) if catalogs_root.is_file() => {
            issues.push(ValidationIssue {
                path: catalogs_root.to_path_buf(),
                message: "expected directory for catalogs root".into(),
            });
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let pattern: Option<(String, Regex)> = match name_pattern {
        Some(p) => Some((
            p.to_string(),
            Regex::new(p).map_err(|e| anyhow!("invalid catalog_name_pattern regex {p:?}: {e}"))?,
        )),
        None => None,
    };

    for entry in read_dir {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            tracing::debug!(path = %entry.path().display(), "skipping non-directory entry");
            continue;
        }
        let dir = entry.path();
        let schema_path = dir.join("schema.yaml");
        if !schema_path.is_file() {
            continue;
        }

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

        // load_all_schemas treats dir/name mismatch as a hard error;
        // here we downgrade to a soft issue so a single run reports
        // every bad file.
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

fn validate_content_blocks(
    content_blocks_root: &Path,
    name_pattern: Option<&str>,
    issues: &mut Vec<ValidationIssue>,
) -> anyhow::Result<()> {
    let read_dir = match std::fs::read_dir(content_blocks_root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) if content_blocks_root.is_file() => {
            issues.push(ValidationIssue {
                path: content_blocks_root.to_path_buf(),
                message: "expected directory for the content_blocks root".into(),
            });
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let pattern: Option<(String, Regex)> = match name_pattern {
        Some(p) => Some((
            p.to_string(),
            Regex::new(p)
                .map_err(|e| anyhow!("invalid content_block_name_pattern regex {p:?}: {e}"))?,
        )),
        None => None,
    };

    for entry in read_dir {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            tracing::debug!(path = %path.display(), "skipping non-file entry");
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("liquid") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();

        let cb = match content_block_io::read_content_block_file(&path) {
            Ok(cb) => cb,
            Err(e) => {
                issues.push(ValidationIssue {
                    path: path.clone(),
                    message: format!("parse error: {e}"),
                });
                continue;
            }
        };

        if cb.name != stem {
            issues.push(ValidationIssue {
                path: path.clone(),
                message: format!(
                    "content block name '{}' does not match its file stem '{}'",
                    cb.name, stem
                ),
            });
        }

        if let Some((pattern_str, re)) = &pattern {
            if !re.is_match(&cb.name) {
                issues.push(ValidationIssue {
                    path: path.clone(),
                    message: format!(
                        "content block name '{}' does not match content_block_name_pattern '{}'",
                        cb.name, pattern_str
                    ),
                });
            }
        }
    }

    Ok(())
}
