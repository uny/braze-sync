//! `braze-sync init` — scaffold a new braze-sync workspace.
//!
//! Writes:
//! - `braze-sync.config.yaml` at the `--config` path (with commented
//!   guidance so the user can edit in place).
//! - Resource directories (`catalogs/`, `content_blocks/`,
//!   `email_templates/`, `custom_attributes/`) as siblings of the config
//!   file, matching the default paths in [`ResourcesConfig`].
//! - `.gitignore` entries for `.env` and `.env.*` (appended if absent;
//!   never deduped through rewrite).
//!
//! Runs **before** config loading in [`crate::cli::run`], so a missing
//! config file is the normal case rather than exit code 3.
//!
//! `--force` is the only gate on overwriting an existing config file.
//! Directories and `.gitignore` are always idempotent: creating an
//! existing directory is a no-op, and `.gitignore` entries are added
//! only if not already present.
//!
//! With `--from-existing`, init additionally loads the scaffolded (or
//! pre-existing) config, resolves the environment, and runs the same
//! code path as `braze-sync export` with no filter — so the operator
//! ends up with a fully populated layout in one command.

use crate::config::ConfigFile;
use anyhow::{bail, Context as _};
use clap::Args;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Overwrite an existing `braze-sync.config.yaml`. Directories and
    /// `.gitignore` are updated idempotently regardless.
    #[arg(long)]
    pub force: bool,

    /// After scaffolding, pull the current state from Braze into the new
    /// layout. Requires the API key env var from the scaffolded config
    /// (by default `BRAZE_DEV_API_KEY`) to be set.
    #[arg(long)]
    pub from_existing: bool,
}

pub async fn run(
    args: &InitArgs,
    config_path: &Path,
    env_override: Option<&str>,
) -> anyhow::Result<()> {
    let config_dir = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    fs::create_dir_all(&config_dir)
        .with_context(|| format!("creating config directory {}", config_dir.display()))?;

    // With `--from-existing`, an already-present config is assumed to
    // be intentional (the operator configured endpoint/env vars before
    // running init) — we keep it unless `--force` says otherwise.
    // Without `--from-existing`, the presence of a config means the
    // operator either already initialized or hand-wrote one; refusing
    // without `--force` avoids silent clobber.
    let config_written = write_config_file(config_path, args.force, args.from_existing)?;
    let dirs_created = scaffold_resource_dirs(&config_dir)?;
    let gitignore_updated = update_gitignore(&config_dir)?;

    // Report what happened so the operator sees whether any step was a
    // no-op. `init` being idempotent is a feature — we want it to be safe
    // to re-run — but silent no-ops would hide misconfiguration.
    eprintln!(
        "✓ config:     {} ({})",
        config_path.display(),
        if config_written {
            "written"
        } else {
            "exists, kept"
        }
    );
    eprintln!(
        "✓ directories: {dirs_created} created, {} kept",
        4 - dirs_created
    );
    eprintln!(
        "✓ .gitignore: {}",
        if gitignore_updated {
            "updated"
        } else {
            "already has entries"
        }
    );

    if args.from_existing {
        eprintln!("→ --from-existing: loading config and pulling Braze state…");
        run_from_existing(config_path, &config_dir, env_override).await?;
    } else {
        eprintln!();
        eprintln!("Next steps:");
        eprintln!("  1. export BRAZE_DEV_API_KEY=<your key>");
        eprintln!("  2. braze-sync export            # pull current Braze state");
        eprintln!("  3. braze-sync diff              # preview drift");
    }

    Ok(())
}

/// Write the scaffolded config. Returns `true` if a new file was
/// written, `false` if an existing one was kept.
///
/// Policy:
/// - Config missing → always write.
/// - Config exists + `force` → overwrite (with a warning).
/// - Config exists + `allow_keep` (i.e. `--from-existing`) → keep it.
/// - Config exists, neither flag → hard error instructing the operator
///   to pass `--force`.
fn write_config_file(config_path: &Path, force: bool, allow_keep: bool) -> anyhow::Result<bool> {
    if config_path.exists() {
        if force {
            eprintln!("⚠ {} exists; overwriting (--force)", config_path.display());
            // fall through to write
        } else if allow_keep {
            return Ok(false);
        } else {
            bail!(
                "{} already exists; pass --force to overwrite",
                config_path.display()
            );
        }
    }
    fs::write(config_path, CONFIG_TEMPLATE)
        .with_context(|| format!("writing config to {}", config_path.display()))?;
    Ok(true)
}

/// Creates the four resource directories as siblings of the config file.
/// Returns the count of directories that didn't already exist — purely
/// for the summary message.
fn scaffold_resource_dirs(config_dir: &Path) -> anyhow::Result<usize> {
    // These match the default `ResourcesConfig` paths; any operator who
    // edits the config to point elsewhere is expected to create those
    // directories themselves.
    const SUBDIRS: [&str; 4] = [
        "catalogs",
        "content_blocks",
        "email_templates",
        "custom_attributes",
    ];
    let mut created = 0;
    for sub in SUBDIRS {
        let dir = config_dir.join(sub);
        if !dir.exists() {
            fs::create_dir_all(&dir)
                .with_context(|| format!("creating directory {}", dir.display()))?;
            created += 1;
        }
    }
    Ok(created)
}

/// Ensures `.gitignore` contains lines for `.env` and `.env.*`. Appends
/// missing entries under a clearly-labeled braze-sync section so the
/// operator can tell where they came from. Returns `true` if any entry
/// was added.
fn update_gitignore(config_dir: &Path) -> anyhow::Result<bool> {
    let path = config_dir.join(".gitignore");
    const ENTRIES: [&str; 2] = [".env", ".env.*"];

    let existing = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", path.display()));
        }
    };

    let has_line = |needle: &str| existing.lines().any(|l| l.trim() == needle);
    let missing: Vec<&str> = ENTRIES.iter().copied().filter(|e| !has_line(e)).collect();
    if missing.is_empty() {
        return Ok(false);
    }

    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {} for append", path.display()))?;

    // Insert a leading newline only if the existing file doesn't already
    // end with one, so we don't glue our section onto the last existing
    // entry.
    if !existing.is_empty() && !existing.ends_with('\n') {
        f.write_all(b"\n")?;
    }
    if !existing.is_empty() {
        f.write_all(b"\n# braze-sync\n")?;
    } else {
        f.write_all(b"# braze-sync\n")?;
    }
    for entry in missing {
        writeln!(f, "{entry}")?;
    }
    Ok(true)
}

async fn run_from_existing(
    config_path: &Path,
    config_dir: &Path,
    env_override: Option<&str>,
) -> anyhow::Result<()> {
    // Reload dotenv now that the user may have edited the just-written
    // config or dropped a .env. The initial call in cli::run already
    // happened, but re-running it is cheap and harmless.
    if let Err(e) = crate::config::load_dotenv() {
        tracing::warn!("dotenv: {e}");
    }

    let cfg = ConfigFile::load(config_path)
        .with_context(|| format!("reloading config from {}", config_path.display()))?;
    let resolved = cfg
        .resolve(env_override)
        .context("resolving environment for --from-existing")?;

    // Delegate to the same code path as `braze-sync export` (no filter
    // — pull everything that's enabled in the config).
    let export_args = super::export::ExportArgs {
        resource: None,
        name: None,
    };
    super::export::run(&export_args, resolved, config_dir).await
}

/// Commented config template — mirrors IMPLEMENTATION.md §10. Kept in
/// the binary so `init` works offline and so the comments stay in sync
/// with the deserializer.
pub(crate) const CONFIG_TEMPLATE: &str = r#"# braze-sync configuration (v1 schema, frozen at v1.0).
# Reference: https://github.com/uny/braze-sync#configuration

version: 1

# Environment picked when --env is not passed.
default_environment: dev

defaults:
  # Requests/minute cap applied via governor. Lower if you hit 429s.
  rate_limit_per_minute: 40

environments:
  dev:
    # Braze REST endpoint for your instance. See:
    # https://www.braze.com/docs/api/basics/#endpoints
    api_endpoint: https://rest.fra-02.braze.eu
    # Name of the env var holding the API key — NEVER put the key itself
    # in this file.
    api_key_env: BRAZE_DEV_API_KEY
  # prod:
  #   api_endpoint: https://rest.fra-02.braze.eu
  #   api_key_env: BRAZE_PROD_API_KEY
  #   rate_limit_per_minute: 30

resources:
  catalog_schema:
    enabled: true
    path: catalogs/
  catalog_items:
    enabled: true
    parallel_batches: 4
  content_block:
    enabled: true
    path: content_blocks/
  email_template:
    enabled: true
    path: email_templates/
  custom_attribute:
    enabled: true
    path: custom_attributes/registry.yaml

# Optional name validators enforced by `braze-sync validate`.
# naming:
#   catalog_name_pattern: "^[a-z][a-z0-9_]*$"
#   content_block_name_pattern: "^[a-zA-Z0-9_]+$"
#   custom_attribute_name_pattern: "^[a-z][a-z0-9_]*$"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_template_parses_as_valid_config() {
        // Guard against drift: if the template ever disagrees with the
        // deserializer, init would produce a config that the rest of
        // braze-sync refuses to load. That's a bug we want to catch at
        // build time, not at user `export` time.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("braze-sync.config.yaml");
        fs::write(&path, CONFIG_TEMPLATE).unwrap();
        let cfg = ConfigFile::load(&path).expect("template must load cleanly");
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.default_environment, "dev");
        assert!(cfg.environments.contains_key("dev"));
    }

    #[test]
    fn gitignore_entries_added_on_fresh_file() {
        let tmp = tempfile::tempdir().unwrap();
        let updated = update_gitignore(tmp.path()).unwrap();
        assert!(updated);
        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.contains(".env"));
        assert!(content.contains(".env.*"));
    }

    #[test]
    fn gitignore_idempotent_on_second_run() {
        let tmp = tempfile::tempdir().unwrap();
        let first = update_gitignore(tmp.path()).unwrap();
        assert!(first);
        let second = update_gitignore(tmp.path()).unwrap();
        assert!(!second, "second run should be a no-op");
    }

    #[test]
    fn gitignore_preserves_existing_content() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".gitignore");
        fs::write(&path, "target/\ndist/\n").unwrap();
        update_gitignore(tmp.path()).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("target/"), "existing entry must survive");
        assert!(content.contains("dist/"));
        assert!(content.contains(".env"));
    }

    #[test]
    fn gitignore_skips_already_present_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".gitignore");
        fs::write(&path, ".env\n").unwrap();
        let updated = update_gitignore(tmp.path()).unwrap();
        // `.env.*` is still missing, so we expect an update.
        assert!(updated);
        let content = fs::read_to_string(&path).unwrap();
        // `.env` should still appear exactly once.
        let count = content.lines().filter(|l| l.trim() == ".env").count();
        assert_eq!(count, 1, "should not duplicate .env");
        assert!(content.contains(".env.*"));
    }

    #[test]
    fn scaffold_creates_all_four_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let n = scaffold_resource_dirs(tmp.path()).unwrap();
        assert_eq!(n, 4);
        for sub in [
            "catalogs",
            "content_blocks",
            "email_templates",
            "custom_attributes",
        ] {
            assert!(tmp.path().join(sub).is_dir(), "{sub} should exist");
        }
    }

    #[test]
    fn scaffold_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        scaffold_resource_dirs(tmp.path()).unwrap();
        let second = scaffold_resource_dirs(tmp.path()).unwrap();
        assert_eq!(second, 0, "second run creates nothing");
    }

    #[test]
    fn write_config_refuses_to_overwrite_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("braze-sync.config.yaml");
        fs::write(&path, "version: 1\n# user edits\n").unwrap();
        let err = write_config_file(&path, false, false).unwrap_err();
        assert!(err.to_string().contains("--force"));
        // Original content preserved.
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("user edits"));
    }

    #[test]
    fn write_config_overwrites_with_force() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("braze-sync.config.yaml");
        fs::write(&path, "# old\n").unwrap();
        let written = write_config_file(&path, true, false).unwrap();
        assert!(written);
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("braze-sync configuration"));
    }

    #[test]
    fn write_config_keeps_existing_with_allow_keep() {
        // --from-existing path: existing config is kept, not overwritten.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("braze-sync.config.yaml");
        fs::write(&path, "# operator-tuned\nversion: 1\n").unwrap();
        let written = write_config_file(&path, false, true).unwrap();
        assert!(!written, "existing config should be kept");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("operator-tuned"));
    }

    #[test]
    fn write_config_writes_fresh_when_allow_keep_and_no_existing() {
        // --from-existing in an empty dir still needs to scaffold the config.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("braze-sync.config.yaml");
        let written = write_config_file(&path, false, true).unwrap();
        assert!(written);
        assert!(path.exists());
    }

    #[test]
    fn write_config_force_overrides_allow_keep() {
        // If the operator explicitly says --force, overwrite even if
        // --from-existing would otherwise keep the file.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("braze-sync.config.yaml");
        fs::write(&path, "# old\n").unwrap();
        let written = write_config_file(&path, true, true).unwrap();
        assert!(written);
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("braze-sync configuration"));
    }
}
