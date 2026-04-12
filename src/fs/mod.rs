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
pub mod content_block_io;
pub mod email_template_io;
pub mod frontmatter;

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

/// Reject names that would escape the resource root or otherwise
/// confuse the on-disk layout. Used by every resource writer as a last
/// line of defence on top of the validate command's pattern check.
pub(crate) fn validate_resource_name(kind_label: &str, name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0');
    if bad {
        return Err(Error::InvalidFormat {
            path: PathBuf::from(name),
            message: format!("{kind_label} name '{name}' contains invalid characters"),
        });
    }
    Ok(())
}

/// Write `contents` to `path` via write-to-temp-then-rename so readers
/// never see a partially-written file. Creates parent directories as needed.
///
/// The temp file is fsynced before the rename to ensure data reaches stable
/// storage even on a crash between write and rename. The temp name includes
/// the process ID to avoid collisions if two braze-sync processes write to
/// the same workspace concurrently. Same-directory rename guarantees the
/// operation does not cross filesystem boundaries.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let file_name = path.file_name().ok_or_else(|| Error::InvalidFormat {
        path: path.to_path_buf(),
        message: "atomic write target has no file name".into(),
    })?;

    let mut tmp_name = file_name.to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp_path = parent.join(tmp_name);

    let mut file = std::fs::File::create(&tmp_path)?;
    if let Err(e) = file.write_all(contents).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    drop(file);

    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    Ok(())
}
