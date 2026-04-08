//! CLI dispatch entry point.
//!
//! Two-stage error model so the frozen exit codes from
//! IMPLEMENTATION.md §7.1 are deterministic regardless of which step
//! fails:
//!
//! 1. **Config stage** ([`load_and_resolve_config`]) — parse the YAML,
//!    validate it, resolve the api-key environment variable. Any failure
//!    here, including a missing or unreadable config file, exits with
//!    code **3** (config / argument error).
//! 2. **Dispatch stage** ([`dispatch`]) — run the requested subcommand.
//!    Errors are mapped through [`exit_code_for`], which walks the
//!    `anyhow::Error` chain and downcasts to the typed errors that carry
//!    semantic meaning ([`BrazeApiError`], [`Error`]).
//!
//! `exit_code_for` deliberately walks the entire chain so an error wrapped
//! as `Error::Api(BrazeApiError::Unauthorized)` and a bare
//! `BrazeApiError::Unauthorized` map to the same exit code (4). This
//! matters because `?` from braze API methods produces the latter while
//! some library helpers might produce the former in the future.

pub mod apply;
pub mod diff;
pub mod export;
pub mod validate;

use crate::braze::error::BrazeApiError;
use crate::config::{ConfigFile, ResolvedConfig};
use crate::error::Error;
use crate::format::OutputFormat;
use anyhow::Context as _;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "braze-sync",
    version,
    about = "GitOps CLI for managing Braze configuration as code"
)]
pub struct Cli {
    /// Path to the braze-sync config file
    #[arg(long, default_value = "./braze-sync.config.yaml", global = true)]
    pub config: PathBuf,

    /// Target environment (defaults to `default_environment` in the config)
    #[arg(long, global = true)]
    pub env: Option<String>,

    /// Verbose tracing output (sets log level to debug)
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Output format. `table` for humans, `json` for CI consumption.
    /// Used by diff/apply/validate; export ignores this in v0.1.0.
    #[arg(long, global = true, value_enum)]
    pub format: Option<OutputFormat>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Pull state from Braze into local files
    Export(export::ExportArgs),
    /// Show drift between local files and Braze
    Diff(diff::DiffArgs),
    /// Apply local intent to Braze (dry-run by default)
    Apply(apply::ApplyArgs),
    /// Validate local files (no Braze API access required)
    Validate(validate::ValidateArgs),
}

/// Top-level CLI entry point. Returns the process exit code per
/// IMPLEMENTATION.md §7.1.
pub async fn run() -> i32 {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // clap prints help/version to stdout and parse errors to stderr.
            e.print().ok();
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayVersion
                | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => 0,
                _ => 3,
            };
        }
    };

    init_tracing(cli.verbose);
    if let Err(e) = crate::config::load_dotenv() {
        // dotenv failures are non-fatal — config resolution will surface
        // any actually missing vars with a clearer error.
        tracing::warn!("dotenv: {e}");
    }

    // Stage 1: parse + structurally validate the config file. No env
    // access yet, so a missing BRAZE_*_API_KEY does NOT fail here.
    // Failure → exit 3 (config / argument error per §7.1).
    let cfg = match ConfigFile::load(&cli.config)
        .with_context(|| format!("failed to load config from {}", cli.config.display()))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            return 3;
        }
    };
    let config_dir = cli
        .config
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    // Validate is the only command that does NOT need an api key — its
    // entire job is local file checking. Dispatch it directly from the
    // parsed ConfigFile so a CI on a fork PR (no secrets) can still run
    // it as a pre-merge check. All other commands fall through to the
    // env-resolution stage below.
    if let Command::Validate(args) = &cli.command {
        return finish(validate::run(args, &cfg, &config_dir).await);
    }

    // Stage 2: resolve the environment (api_key from env var, etc.).
    // Failure here is also exit 3 — typically a missing
    // BRAZE_*_API_KEY env var.
    let resolved = match cfg
        .resolve(cli.env.as_deref())
        .context("failed to resolve environment from config")
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e:#}");
            return 3;
        }
    };

    // Stage 3: dispatch the env-resolved command.
    finish(dispatch(&cli, resolved, &config_dir).await)
}

/// Map a command result to an exit code, printing the error chain on
/// failure. Used by both the validate (no-resolve) and dispatch
/// (env-resolved) branches of `run`.
fn finish(result: anyhow::Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            exit_code_for(&e)
        }
    }
}

async fn dispatch(cli: &Cli, resolved: ResolvedConfig, config_dir: &Path) -> anyhow::Result<()> {
    match &cli.command {
        Command::Export(args) => export::run(args, resolved, config_dir).await,
        Command::Diff(args) => {
            let format = cli.format.unwrap_or_default();
            diff::run(args, resolved, config_dir, format).await
        }
        Command::Apply(args) => {
            let format = cli.format.unwrap_or_default();
            apply::run(args, resolved, config_dir, format).await
        }
        Command::Validate(_) => {
            unreachable!("validate is dispatched in cli::run before env resolution")
        }
    }
}

fn init_tracing(verbose: bool) {
    let default_level = if verbose { "debug" } else { "warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Map a stage-2 error to a §7.1 exit code by walking the
/// `anyhow::Error` chain.
fn exit_code_for(err: &anyhow::Error) -> i32 {
    for cause in err.chain() {
        if let Some(b) = cause.downcast_ref::<BrazeApiError>() {
            return match b {
                BrazeApiError::Unauthorized => 4,
                BrazeApiError::RateLimitExhausted => 5,
                _ => 1,
            };
        }
        if let Some(top) = cause.downcast_ref::<Error>() {
            match top {
                // Walk into the chain — the wrapped BrazeApiError is the
                // next entry.
                Error::Api(_) => {}
                Error::DestructiveBlocked => return 6,
                Error::DriftDetected { .. } => return 2,
                Error::Config(_) | Error::MissingEnv(_) => return 3,
                _ => return 1,
            }
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::ResourceKind;

    #[test]
    fn parses_export_with_resource_filter() {
        let cli =
            Cli::try_parse_from(["braze-sync", "export", "--resource", "catalog_schema"]).unwrap();
        let Command::Export(args) = cli.command else {
            panic!("expected Export subcommand");
        };
        assert_eq!(args.resource, Some(ResourceKind::CatalogSchema));
        assert_eq!(args.name, None);
    }

    #[test]
    fn parses_export_with_name_filter() {
        let cli = Cli::try_parse_from([
            "braze-sync",
            "export",
            "--resource",
            "catalog_schema",
            "--name",
            "cardiology",
        ])
        .unwrap();
        let Command::Export(args) = cli.command else {
            panic!("expected Export subcommand");
        };
        assert_eq!(args.resource, Some(ResourceKind::CatalogSchema));
        assert_eq!(args.name.as_deref(), Some("cardiology"));
    }

    #[test]
    fn parses_diff_with_fail_on_drift() {
        let cli = Cli::try_parse_from(["braze-sync", "diff", "--fail-on-drift"]).unwrap();
        let Command::Diff(args) = cli.command else {
            panic!("expected Diff subcommand");
        };
        assert!(args.fail_on_drift);
        assert_eq!(args.resource, None);
    }

    #[test]
    fn parses_validate_subcommand() {
        let cli = Cli::try_parse_from(["braze-sync", "validate"]).unwrap();
        let Command::Validate(args) = cli.command else {
            panic!("expected Validate subcommand");
        };
        assert_eq!(args.resource, None);
    }

    #[test]
    fn parses_validate_with_resource_filter() {
        let cli = Cli::try_parse_from(["braze-sync", "validate", "--resource", "catalog_schema"])
            .unwrap();
        let Command::Validate(args) = cli.command else {
            panic!("expected Validate subcommand");
        };
        assert_eq!(args.resource, Some(ResourceKind::CatalogSchema));
    }

    #[test]
    fn parses_diff_with_resource_and_name() {
        let cli = Cli::try_parse_from([
            "braze-sync",
            "diff",
            "--resource",
            "catalog_schema",
            "--name",
            "cardiology",
        ])
        .unwrap();
        let Command::Diff(args) = cli.command else {
            panic!("expected Diff subcommand");
        };
        assert_eq!(args.resource, Some(ResourceKind::CatalogSchema));
        assert_eq!(args.name.as_deref(), Some("cardiology"));
        assert!(!args.fail_on_drift);
    }

    #[test]
    fn name_requires_resource() {
        let result = Cli::try_parse_from(["braze-sync", "export", "--name", "cardiology"]);
        assert!(
            result.is_err(),
            "expected --name without --resource to error"
        );
    }

    #[test]
    fn config_default_path() {
        let cli = Cli::try_parse_from(["braze-sync", "export"]).unwrap();
        assert_eq!(cli.config, PathBuf::from("./braze-sync.config.yaml"));
    }

    #[test]
    fn global_flags_position_independent() {
        let cli = Cli::try_parse_from(["braze-sync", "export", "--config", "/tmp/x.yaml"]).unwrap();
        assert_eq!(cli.config, PathBuf::from("/tmp/x.yaml"));
    }

    #[test]
    fn env_override_parsed() {
        let cli = Cli::try_parse_from(["braze-sync", "--env", "prod", "export"]).unwrap();
        assert_eq!(cli.env.as_deref(), Some("prod"));
    }

    #[test]
    fn format_value_parsed_as_enum() {
        let cli = Cli::try_parse_from(["braze-sync", "--format", "json", "export"]).unwrap();
        assert_eq!(cli.format, Some(OutputFormat::Json));
    }

    #[test]
    fn exit_code_for_unauthorized() {
        let err = anyhow::Error::new(BrazeApiError::Unauthorized);
        assert_eq!(exit_code_for(&err), 4);
    }

    #[test]
    fn exit_code_for_rate_limit_exhausted() {
        let err = anyhow::Error::new(BrazeApiError::RateLimitExhausted);
        assert_eq!(exit_code_for(&err), 5);
    }

    #[test]
    fn exit_code_for_drift_detected() {
        let err = anyhow::Error::new(Error::DriftDetected { count: 3 });
        assert_eq!(exit_code_for(&err), 2);
    }

    #[test]
    fn exit_code_for_destructive_blocked() {
        let err = anyhow::Error::new(Error::DestructiveBlocked);
        assert_eq!(exit_code_for(&err), 6);
    }

    #[test]
    fn exit_code_for_missing_env() {
        let err = anyhow::Error::new(Error::MissingEnv("X".into()));
        assert_eq!(exit_code_for(&err), 3);
    }

    #[test]
    fn exit_code_for_config_error() {
        let err = anyhow::Error::new(Error::Config("oops".into()));
        assert_eq!(exit_code_for(&err), 3);
    }

    #[test]
    fn exit_code_for_api_wrapped_unauthorized_unwraps_to_4() {
        // Error::Api(BrazeApiError::Unauthorized) — chain walk must reach
        // the inner BrazeApiError on the second iteration.
        let err = anyhow::Error::new(Error::Api(BrazeApiError::Unauthorized));
        assert_eq!(exit_code_for(&err), 4);
    }

    #[test]
    fn exit_code_for_other_anyhow_is_one() {
        let err = anyhow::anyhow!("some random failure");
        assert_eq!(exit_code_for(&err), 1);
    }
}
