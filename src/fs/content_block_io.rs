//! Content Block file I/O.
//!
//! Each `.liquid` file is YAML frontmatter followed by the Liquid body.
//! Single-file-with-frontmatter (rather than a directory) is deliberate
//! so that the body starts on line 1 — operators editing templates
//! shouldn't have to scroll past `tags:` to see what they're changing.

use crate::error::{Error, Result};
use crate::fs::{frontmatter, validate_resource_name, write_atomic};
use crate::resource::{ContentBlock, ContentBlockState};
use serde::{Deserialize, Serialize};
use std::path::Path;

const FILE_EXT: &str = "liquid";

/// On-disk wire shape. Kept private so the file layout can change
/// without affecting consumers of the domain [`ContentBlock`].
#[derive(Debug, Serialize, Deserialize)]
struct Frontmatter {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    // Local-only documentation field. Excluded from
    // `diff::content_block::syncable_eq` and from the update wire body
    // (see `src/diff/content_block.rs` module docs for the
    // "infinite drift" rationale). Optional on disk so a fresh export
    // — which cannot observe real remote state because `/info` does
    // not return it — writes no `state:` line instead of lying with
    // the default `active`. Operator-authored `state: draft` is still
    // round-tripped because `save_content_block` only omits the field
    // when its value equals the type default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<ContentBlockState>,
}

/// Load every `.liquid` file directly under `root`, sorted by name.
/// Missing root is not an error. Each file's stem must match its
/// frontmatter `name:` — divergence is treated as a hard parse error.
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

/// Read a single `.liquid` file. Does not validate that the file stem
/// matches `name`; callers do that.
pub fn read_content_block_file(path: &Path) -> Result<ContentBlock> {
    let text = std::fs::read_to_string(path)?;
    let (fm, body): (Frontmatter, &str) = frontmatter::parse(path, &text)?;
    Ok(ContentBlock {
        name: fm.name,
        description: fm.description,
        content: body.to_string(),
        tags: fm.tags,
        // Missing `state:` on disk → default (Active). Keeps the
        // domain type non-Optional so diff/apply callers don't need
        // an unwrap.
        state: fm.state.unwrap_or_default(),
    })
}

/// Write `cb` to `<root>/<cb.name>.liquid`. Names containing path
/// separators or `..` are rejected as defence in depth.
pub fn save_content_block(root: &Path, cb: &ContentBlock) -> Result<()> {
    validate_resource_name("content block", &cb.name)?;
    let path = root.join(format!("{}.{FILE_EXT}", cb.name));

    // `state` is only emitted when it differs from the type default.
    // Rationale: `braze::content_block::get_content_block` hard-codes
    // `state: Active` because Braze's `/info` endpoint does not return
    // state, so a fresh `export` cannot know whether a block is really
    // Active or Draft. Writing the default would turn an unknown into
    // a confident lie in the file. Operator-authored `state: draft`
    // still round-trips because Draft is a non-default value and gets
    // serialized explicitly.
    let fm = Frontmatter {
        name: cb.name.clone(),
        description: cb.description.clone(),
        tags: cb.tags.clone(),
        state: if cb.state == ContentBlockState::default() {
            None
        } else {
            Some(cb.state)
        },
    };
    // Body is written byte-exact. A previous version unconditionally
    // appended `\n` here, which caused any Braze block whose stored
    // content lacked a trailing newline to round-trip as `body\n`,
    // diff as Modified, and (if Braze normalizes trailing whitespace
    // on store) loop forever. `frontmatter::render` already terminates
    // the closing fence with `\n`, so an empty body still produces a
    // valid file; only the no-trailing-newline body case is affected.
    let text = frontmatter::render(&path, &fm, &cb.content)?;
    write_atomic(&path, text.as_bytes())?;
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
    fn body_without_trailing_newline_round_trips_byte_exact() {
        // Regression: a previous save_content_block appended `\n`
        // unconditionally, so a Braze block whose stored content was
        // `Hello` (no terminator) would reload as `Hello\n` and diff
        // as Modified forever. The fix preserves body bytes verbatim.
        let dir = tempfile::tempdir().unwrap();
        let original = ContentBlock {
            name: "no_eol".into(),
            description: None,
            content: "Hello".into(),
            tags: vec![],
            state: ContentBlockState::Active,
        };
        save_content_block(dir.path(), &original).unwrap();
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "Hello");
        assert_eq!(loaded[0], original);
    }

    #[test]
    fn multiline_body_without_trailing_newline_round_trips() {
        // Same fidelity guarantee for the harder case where the body
        // contains internal newlines but no terminator.
        let dir = tempfile::tempdir().unwrap();
        let original = ContentBlock {
            name: "multi".into(),
            description: None,
            content: "line one\nline two".into(),
            tags: vec![],
            state: ContentBlockState::Active,
        };
        save_content_block(dir.path(), &original).unwrap();
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(loaded[0].content, "line one\nline two");
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

    #[test]
    fn save_with_default_state_does_not_emit_state_line() {
        // Honesty guard: `get_content_block` hard-codes state=Active
        // because Braze's /info doesn't return state, so a fresh
        // export cannot know the real value. Writing `state: active`
        // would be a confident lie about a genuinely unknown field.
        // The fix is to skip the line for the type default and let
        // `read_content_block_file` re-default on load.
        let dir = tempfile::tempdir().unwrap();
        let active = ContentBlock {
            name: "exported".into(),
            description: None,
            content: "Hello\n".into(),
            tags: vec![],
            state: ContentBlockState::Active,
        };
        save_content_block(dir.path(), &active).unwrap();
        let text = std::fs::read_to_string(dir.path().join("exported.liquid")).unwrap();
        assert!(
            !text.contains("state:"),
            "default-state file should not carry a state line; got:\n{text}"
        );
        // And it still round-trips (load defaults missing state to Active).
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(loaded[0], active);
    }

    #[test]
    fn save_with_draft_state_emits_state_line_and_round_trips() {
        // Counterpart to the honesty guard: operator-authored
        // `state: draft` must still persist, otherwise the local
        // annotation feature documented in the README is a fiction.
        // Draft is the non-default value, so serialization keeps it.
        let dir = tempfile::tempdir().unwrap();
        let draft = ContentBlock {
            name: "wip".into(),
            description: None,
            content: "work in progress\n".into(),
            tags: vec![],
            state: ContentBlockState::Draft,
        };
        save_content_block(dir.path(), &draft).unwrap();
        let text = std::fs::read_to_string(dir.path().join("wip.liquid")).unwrap();
        assert!(
            text.contains("state: draft"),
            "draft state should be serialized; got:\n{text}"
        );
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(loaded[0], draft);
    }

    #[test]
    fn load_file_without_state_line_defaults_to_active() {
        // Symmetric guard for `save_with_default_state_does_not_emit_state_line`:
        // after that change, the canonical on-disk shape for an Active
        // block has no `state:` line at all. Loading must default, not
        // fail. Overlaps with `missing_state_field_defaults_to_active`
        // but pins the exact "no line present" shape that the new save
        // path produces, so a future regression that re-introduces
        // state serialization can't sneak past the round-trip tests.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("stateless.liquid"),
            "---\nname: stateless\n---\nbody\n",
        )
        .unwrap();
        let loaded = load_all_content_blocks(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].state, ContentBlockState::Active);
    }
}
