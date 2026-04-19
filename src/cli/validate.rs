//! `braze-sync validate` — local-only structural and naming checks.
//!
//! Runs without a Braze API key so CI on fork PRs (where the secret
//! isn't available) can still gate merges. Issues are collected across
//! the whole run and reported at the end so a single pass surfaces
//! every problem.

use crate::config::ConfigFile;
use crate::error::Error;
use crate::fs::{
    catalog_io, content_block_io, custom_attribute_io, email_template_io, try_read_resource_dir,
};
use crate::resource::ResourceKind;
use anyhow::anyhow;
use clap::Args;
use regex_lite::Regex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::selected_kinds;

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
            ResourceKind::EmailTemplate => {
                let email_templates_root = config_dir.join(&cfg.resources.email_template.path);
                validate_email_templates(&email_templates_root, &mut issues)?;
            }
            ResourceKind::CustomAttribute => {
                let registry_path = config_dir.join(&cfg.resources.custom_attribute.path);
                validate_custom_attributes(
                    &registry_path,
                    cfg.naming.custom_attribute_name_pattern.as_deref(),
                    &mut issues,
                )?;
            }
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

/// Try to open a resource root directory. Returns `None` (and pushes an
/// issue) when the path is missing or is a file — callers should return
/// `Ok(())` in that case.
fn open_resource_dir(
    root: &Path,
    kind_label: &str,
    issues: &mut Vec<ValidationIssue>,
) -> anyhow::Result<Option<std::fs::ReadDir>> {
    match try_read_resource_dir(root, kind_label) {
        Ok(rd) => Ok(rd),
        Err(Error::InvalidFormat { path, message }) => {
            issues.push(ValidationIssue { path, message });
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// Compile an optional naming-pattern regex, returning the raw string
/// alongside the compiled `Regex` so error messages can reference the
/// original pattern.  `config_key` names the config field for the error
/// message (e.g. `"catalog_name_pattern"`).
fn compile_name_pattern(
    raw: Option<&str>,
    config_key: &str,
) -> anyhow::Result<Option<(String, Regex)>> {
    match raw {
        Some(p) => Ok(Some((
            p.to_string(),
            Regex::new(p).map_err(|e| anyhow!("invalid {config_key} regex {p:?}: {e}"))?,
        ))),
        None => Ok(None),
    }
}

/// Check `name` against the compiled pattern and push a uniform
/// "does not match <config_key>" issue when it fails. `kind_label` is
/// the human-readable resource noun for the message (e.g. `"catalog"`).
fn check_name_pattern(
    pattern: Option<&(String, Regex)>,
    name: &str,
    path: &Path,
    kind_label: &str,
    config_key: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    let Some((pattern_str, re)) = pattern else {
        return;
    };
    if !re.is_match(name) {
        issues.push(ValidationIssue {
            path: path.to_path_buf(),
            message: format!(
                "{kind_label} name '{name}' does not match {config_key} '{pattern_str}'"
            ),
        });
    }
}

fn validate_catalog_schemas(
    catalogs_root: &Path,
    name_pattern: Option<&str>,
    issues: &mut Vec<ValidationIssue>,
) -> anyhow::Result<()> {
    let Some(read_dir) = open_resource_dir(catalogs_root, "catalogs", issues)? else {
        return Ok(());
    };

    let pattern = compile_name_pattern(name_pattern, "catalog_name_pattern")?;

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

        check_name_pattern(
            pattern.as_ref(),
            &cat.name,
            &schema_path,
            "catalog",
            "catalog_name_pattern",
            issues,
        );
    }

    Ok(())
}

fn validate_content_blocks(
    content_blocks_root: &Path,
    name_pattern: Option<&str>,
    issues: &mut Vec<ValidationIssue>,
) -> anyhow::Result<()> {
    let Some(read_dir) = open_resource_dir(content_blocks_root, "content_blocks", issues)? else {
        return Ok(());
    };

    let pattern = compile_name_pattern(name_pattern, "content_block_name_pattern")?;

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

        check_name_pattern(
            pattern.as_ref(),
            &cb.name,
            &path,
            "content block",
            "content_block_name_pattern",
            issues,
        );
    }

    Ok(())
}

fn validate_email_templates(
    email_templates_root: &Path,
    issues: &mut Vec<ValidationIssue>,
) -> anyhow::Result<()> {
    let Some(read_dir) = open_resource_dir(email_templates_root, "email_templates", issues)? else {
        return Ok(());
    };

    for entry in read_dir {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            tracing::debug!(path = %path.display(), "skipping non-directory entry");
            continue;
        }
        let template_yaml_path = path.join("template.yaml");
        if !template_yaml_path.is_file() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().into_owned();

        let et = match email_template_io::read_email_template_dir(&path) {
            Ok(et) => et,
            Err(e) => {
                issues.push(ValidationIssue {
                    path: template_yaml_path.clone(),
                    message: format!("parse error: {e}"),
                });
                continue;
            }
        };

        if et.name != dir_name {
            issues.push(ValidationIssue {
                path: template_yaml_path.clone(),
                message: format!(
                    "email template name '{}' does not match its directory '{}'",
                    et.name, dir_name
                ),
            });
        }

        if et.subject.is_empty() {
            issues.push(ValidationIssue {
                path: template_yaml_path.clone(),
                message: format!("email template '{}' has an empty subject", et.name),
            });
        }
    }

    Ok(())
}

fn validate_custom_attributes(
    registry_path: &Path,
    name_pattern: Option<&str>,
    issues: &mut Vec<ValidationIssue>,
) -> anyhow::Result<()> {
    let registry = match custom_attribute_io::load_registry(registry_path) {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(()),
        Err(Error::YamlParse { path, source }) => {
            issues.push(ValidationIssue {
                path,
                message: format!("parse error: {source}"),
            });
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let pattern = compile_name_pattern(name_pattern, "custom_attribute_name_pattern")?;

    let mut seen = HashSet::with_capacity(registry.attributes.len());
    for attr in &registry.attributes {
        if !seen.insert(attr.name.as_str()) {
            issues.push(ValidationIssue {
                path: registry_path.to_path_buf(),
                message: format!("duplicate custom attribute name '{}'", attr.name),
            });
        }

        check_name_pattern(
            pattern.as_ref(),
            &attr.name,
            registry_path,
            "custom attribute",
            "custom_attribute_name_pattern",
            issues,
        );
    }

    Ok(())
}
