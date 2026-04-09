//! `braze-sync` binary entry point.
//!
//! Defers all logic to [`braze_sync::cli`] so the binary stays a thin
//! wrapper around the library — keeps the surface symmetric for embedding
//! and integration testing via `assert_cmd`.

#[tokio::main]
async fn main() {
    let code = braze_sync::cli::run().await;
    std::process::exit(code);
}
