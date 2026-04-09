//! Local filesystem I/O for braze-sync resource files.
//!
//! Each resource type has its own submodule with concrete reader / writer
//! functions. The functions take a root directory and a domain value and
//! have **no awareness of the config system** — the CLI layer is
//! responsible for joining the config directory with
//! `resources.<kind>.path` to compute the root. This keeps `fs/` standalone
//! testable and avoids a `fs/` ↔ `config/` cycle.
//!
//! See IMPLEMENTATION.md §5, §9.

pub mod catalog_io;

use crate::error::{Error, Result};
use std::path::Path;

/// Atomically write `contents` to `path` by writing to a sibling temp file
/// and renaming on top of the target. Creates parent directories as needed.
///
/// Same-directory rename guarantees the operation does not cross filesystem
/// boundaries. `std::fs::rename` overwrites the destination on both Unix
/// and Windows, so a previous file at `path` is replaced atomically from a
/// reader's perspective.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let file_name = path.file_name().ok_or_else(|| Error::InvalidFormat {
        path: path.to_path_buf(),
        message: "atomic write target has no file name".into(),
    })?;

    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(".tmp");
    let tmp_path = parent.join(tmp_name);

    std::fs::write(&tmp_path, contents)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}
