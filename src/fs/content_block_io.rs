//! Content Block file I/O.
//!
//! Layout (IMPLEMENTATION.md §9.1, §9.4):
//!
//! ```text
//! <content_blocks_root>/
//! ├── 2504_pr_post_bonus_dialog.liquid
//! └── shared_header.liquid
//! ```
//!
//! Each `.liquid` file is YAML frontmatter (`---` fenced) followed by
//! the Liquid template body. The frontmatter carries every field from
//! [`ContentBlock`] except `content` itself, which lives in the body.
//! Splitting metadata from body is the whole reason this layout uses a
//! single file with frontmatter rather than a directory: humans editing
//! the body should not have to scroll past `tags:` to reach line 1.

use crate::error::{Error, Result};
use crate::fs::{frontmatter, write_atomic};
use crate::resource::{ContentBlock, ContentBlockState};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE_EXT: &str = "liquid";

/// Frontmatter wire shape — every [`ContentBlock`] field except `content`,
/// which lives in the body. Kept private so changing the on-disk
/// representation cannot affect downstream code that consumes
/// [`ContentBlock`].
#[derive(Debug, Serialize, Deserialize)]
struct Frontmatter {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(default = "default_state")]
    state: ContentBlockState,
}

fn default_state() -> ContentBlockState {
    // Braze content_blocks API has no state concept; Active is the
    // forward-compat default. See README v0.2.0 limitations.
    ContentBlockState::Active
}

/// Load every `.liquid` file directly under `root`, sorted by name. A
/// missing root is not an error — a fresh project is allowed to have
/// no content blocks yet.
///
/// File-name validation: the basename (without `.liquid`) must match
/// the frontmatter `name:`. This mirrors the catalog dir/name check
/// (`fs/catalog_io.rs`) and makes diff/apply trivially auditable.
pub fn load_all_content_blocks(root: &Path) -> Result<Vec<ContentBlock>> {
    let read_dir = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            if root.is_file() {
                return Err(Error::InvalidFormat {
                    path: root.to_path_buf(),
                    message: "expected a directory for the content_blocks root".into(),
                });
            }
            return Err(e.into());
        }
    };

    let mut blocks = Vec::new();
    for entry in read_dir {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() {
            tracing::debug!(path = %path.display(), "skipping non-file entry");
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some(FILE_EXT) {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let cb = read_content_block_file(&path)?;
        if cb.name != stem {
            return Err(Error::InvalidFormat {
                path: path.clone(),
                message: format!(
                    "content block name '{}' does not match its file stem '{}'",
                    cb.name, stem
                ),
            });
        }
        blocks.push(cb);
    }

    blocks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(blocks)
}

/// Read a single `.liquid` file by absolute path. Does NOT validate that
/// the file stem matches `name` — the caller (typically
/// [`load_all_content_blocks`]) is responsible for that.
pub fn read_content_block_file(path: &Path) -> Result<ContentBlock> {
    let text = std::fs::read_to_string(path)?;
    let (fm, body): (Frontmatter, &str) = frontmatter::parse(path, &text)?;
    Ok(ContentBlock {
        name: fm.name,
        description: fm.description,
        content: body.to_string(),
        tags: fm.tags,
        state: fm.state,
    })
}

/// Write `cb` to `<root>/<cb.name>.liquid`, creating directories as
/// needed. Rejects names that contain path separators or `..` to defeat
/// path-traversal — defense in depth on top of the validate command's
/// naming-pattern check.
pub fn save_content_block(root: &Path, cb: &ContentBlock) -> Result<()> {
    validate_content_block_name(&cb.name)?;
    let path = root.join(format!("{}.{FILE_EXT}", cb.name));

    let fm = Frontmatter {
        name: cb.name.clone(),
        description: cb.description.clone(),
        tags: cb.tags.clone(),
        state: cb.state,
    };
    let mut text = frontmatter::render(&fm, &cb.content)?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    write_atomic(&path, text.as_bytes())?;
    Ok(())
}

fn validate_content_block_name(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0');
    if bad {
        return Err(Error::InvalidFormat {
            path: PathBuf::from(name),
            message: format!("content block name '{name}' contains invalid characters"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cb(name: &str, content: &str) -> ContentBlock {
        ContentBlock {
            name: name.into(),
            description: Some(format!("desc for {name}")),
            content: content.into(),
            tags: vec!["t1".into()],
            state: ContentBlockState::Active,
        }
    }

    #[test]
    fn round_trip_single_block() {
        let dir = tempfile::tempdir().unwrap();
        let original = cb("promo", "Hello {{ user.${first_name} }}\n");
        save_content_block(dir.path(), &original).unwrap();
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], original);
    }

    #[test]
    fn save_creates_root_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("braze").join("content_blocks");
        save_content_block(&nested, &cb("nested", "x")).unwrap();
        assert!(nested.join("nested.liquid").is_file());
    }

    #[test]
    fn load_sorts_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        save_content_block(dir.path(), &cb("zebra", "z")).unwrap();
        save_content_block(dir.path(), &cb("apple", "a")).unwrap();
        save_content_block(dir.path(), &cb("mango", "m")).unwrap();
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(
            loaded.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["apple", "mango", "zebra"]
        );
    }

    #[test]
    fn missing_root_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let nonexistent = dir.path().join("not_here");
        let loaded = load_all_content_blocks(&nonexistent).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn empty_root_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn root_pointing_at_a_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("not_a_dir");
        std::fs::write(&file_path, "x").unwrap();
        let err = load_all_content_blocks(&file_path).unwrap_err();
        assert!(matches!(err, Error::InvalidFormat { .. }), "got {err:?}");
    }

    #[test]
    fn non_liquid_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), "# notes\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "irrelevant\n").unwrap();
        save_content_block(dir.path(), &cb("real", "body")).unwrap();
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "real");
    }

    #[test]
    fn name_mismatch_with_file_stem_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("on_disk_name.liquid"),
            "---\nname: in_yaml_name\nstate: active\n---\nbody\n",
        )
        .unwrap();
        let err = load_all_content_blocks(dir.path()).unwrap_err();
        match err {
            Error::InvalidFormat { message, .. } => {
                assert!(message.contains("on_disk_name"));
                assert!(message.contains("in_yaml_name"));
            }
            other => panic!("expected InvalidFormat, got {other:?}"),
        }
    }

    #[test]
    fn missing_state_field_defaults_to_active() {
        // Forward-compat: a future binary that drops `state` from the
        // frontmatter should still parse cleanly.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("legacy.liquid"),
            "---\nname: legacy\n---\nold body\n",
        )
        .unwrap();
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].state, ContentBlockState::Active);
        assert_eq!(loaded[0].content, "old body\n");
    }

    #[test]
    fn unknown_frontmatter_field_is_ignored_for_forward_compat() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("future.liquid"),
            "---\nname: future\nfuture_v2_field: surprise\nstate: active\n---\nbody\n",
        )
        .unwrap();
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "future");
    }

    #[test]
    fn save_rejects_path_traversal_in_name() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["../evil", "..", ".", "", "a/b", "a\\b"] {
            let bad_cb = ContentBlock {
                name: bad.into(),
                description: None,
                content: String::new(),
                tags: vec![],
                state: ContentBlockState::Active,
            };
            let err = save_content_block(dir.path(), &bad_cb).unwrap_err();
            assert!(
                matches!(err, Error::InvalidFormat { .. }),
                "name {bad:?} should be rejected; got {err:?}"
            );
        }
    }

    #[test]
    fn save_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        save_content_block(dir.path(), &cb("ovr", "v1\n")).unwrap();
        save_content_block(dir.path(), &cb("ovr", "v2\n")).unwrap();
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "v2\n");
    }

    #[test]
    fn empty_body_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let empty_body = ContentBlock {
            name: "blank".into(),
            description: None,
            content: String::new(),
            tags: vec![],
            state: ContentBlockState::Active,
        };
        save_content_block(dir.path(), &empty_body).unwrap();
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(loaded[0], empty_body);
    }
}
