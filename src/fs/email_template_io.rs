//! Email Template file I/O.
//!
//! Each email template lives in its own directory under the templates root:
//!
//! ```text
//! email_templates/
//! └── welcome/
//!     ├── template.yaml   # metadata (name, subject, preheader, tags, …)
//!     ├── body.html       # HTML body (byte-faithful)
//!     └── body.txt        # plaintext fallback (byte-faithful)
//! ```
//!
//! The 3-file layout keeps body diffs reviewable in PRs (no YAML escaping)
//! and follows the directory-per-resource pattern from IMPLEMENTATION.md §9.5.

use crate::error::{Error, Result};
use crate::fs::{try_read_resource_dir, validate_resource_name, write_atomic};
use crate::resource::EmailTemplate;
use serde::{Deserialize, Serialize};
use std::path::Path;

const TEMPLATE_YAML: &str = "template.yaml";
const BODY_HTML: &str = "body.html";
const BODY_TXT: &str = "body.txt";

/// On-disk wire shape for `template.yaml`. Kept private so the file
/// layout can change without affecting consumers of [`EmailTemplate`].
#[derive(Debug, Serialize, Deserialize)]
struct TemplateYaml {
    name: String,
    subject: String,
    /// Read-only field from Braze /info. Not settable via create/update.
    /// Excluded from syncable_eq (same pattern as ContentBlock `state`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preheader: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    should_inline_css: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
}

/// Load every email template directory directly under `root`, sorted by name.
/// Missing root is not an error. Each directory's name must match its
/// `template.yaml` `name:` field — divergence is a hard parse error.
pub fn load_all_email_templates(root: &Path) -> Result<Vec<EmailTemplate>> {
    let Some(read_dir) = try_read_resource_dir(root, "email_templates")? else {
        return Ok(Vec::new());
    };

    let mut templates = Vec::new();
    for entry in read_dir {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            tracing::debug!(path = %path.display(), "skipping non-directory entry");
            continue;
        }
        let template_yaml_path = path.join(TEMPLATE_YAML);
        if !template_yaml_path.is_file() {
            tracing::debug!(path = %path.display(), "skipping directory without template.yaml");
            continue;
        }
        let dir_name = entry.file_name().to_str().unwrap_or_default().to_string();
        let et = read_email_template_dir(&path)?;
        if et.name != dir_name {
            return Err(Error::InvalidFormat {
                path: template_yaml_path,
                message: format!(
                    "email template name '{}' does not match its directory '{}'",
                    et.name, dir_name
                ),
            });
        }
        templates.push(et);
    }

    templates.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(templates)
}

/// Read a single email template directory. Does not validate that the
/// directory name matches `name`; callers do that.
pub fn read_email_template_dir(dir: &Path) -> Result<EmailTemplate> {
    let yaml_path = dir.join(TEMPLATE_YAML);
    let yaml_text = std::fs::read_to_string(&yaml_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::InvalidFormat {
                path: yaml_path.clone(),
                message: "template.yaml not found".into(),
            }
        } else {
            e.into()
        }
    })?;
    let meta: TemplateYaml = serde_norway::from_str(&yaml_text).map_err(|e| Error::YamlParse {
        path: yaml_path.clone(),
        source: e,
    })?;

    let html_path = dir.join(BODY_HTML);
    let body_html = read_body_file(&html_path)?;

    let txt_path = dir.join(BODY_TXT);
    let body_plaintext = read_body_file(&txt_path)?;

    Ok(EmailTemplate {
        name: meta.name,
        subject: meta.subject,
        body_html,
        body_plaintext,
        description: meta.description,
        preheader: meta.preheader,
        should_inline_css: meta.should_inline_css,
        tags: meta.tags,
    })
}

/// Read a body file. Missing file → empty string (§6.4: empty is valid).
/// Byte-faithful — no trailing newline normalization.
fn read_body_file(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

/// Write `et` to `<root>/<et.name>/{template.yaml, body.html, body.txt}`.
/// Names containing path separators or `..` are rejected.
pub fn save_email_template(root: &Path, et: &EmailTemplate) -> Result<()> {
    validate_resource_name("email template", &et.name)?;
    let dir = root.join(&et.name);

    let meta = TemplateYaml {
        name: et.name.clone(),
        subject: et.subject.clone(),
        // `description` is only emitted when present. A fresh export from
        // Braze /info will carry it; operator-authored files without one
        // omit the field entirely rather than writing `description: ""`.
        description: et.description.clone(),
        preheader: et.preheader.clone(),
        should_inline_css: et.should_inline_css,
        tags: et.tags.clone(),
    };
    let yaml_text = format!(
        "# Generated by braze-sync.\n{}",
        serde_norway::to_string(&meta).map_err(|e| Error::InvalidFormat {
            path: dir.join(TEMPLATE_YAML),
            message: format!("failed to serialize template.yaml: {e}"),
        })?
    );

    write_atomic(&dir.join(TEMPLATE_YAML), yaml_text.as_bytes())?;
    // Body files are written byte-exact (B1 invariant #2).
    write_atomic(&dir.join(BODY_HTML), et.body_html.as_bytes())?;
    write_atomic(&dir.join(BODY_TXT), et.body_plaintext.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn et(name: &str, html: &str) -> EmailTemplate {
        EmailTemplate {
            name: name.into(),
            subject: format!("Subject for {name}"),
            body_html: html.into(),
            body_plaintext: format!("plain: {name}"),
            description: Some(format!("desc for {name}")),
            preheader: Some("preview".into()),
            should_inline_css: Some(true),
            tags: vec!["t1".into()],
        }
    }

    #[test]
    fn round_trip_single_template() {
        let dir = tempfile::tempdir().unwrap();
        let original = et("welcome", "<p>Hello</p>\n");
        save_email_template(dir.path(), &original).unwrap();
        let loaded = load_all_email_templates(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], original);
    }

    #[test]
    fn save_creates_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("braze").join("email_templates");
        save_email_template(&nested, &et("nested", "x")).unwrap();
        assert!(nested.join("nested").join("template.yaml").is_file());
        assert!(nested.join("nested").join("body.html").is_file());
        assert!(nested.join("nested").join("body.txt").is_file());
    }

    #[test]
    fn load_sorts_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        save_email_template(dir.path(), &et("zebra", "z")).unwrap();
        save_email_template(dir.path(), &et("apple", "a")).unwrap();
        save_email_template(dir.path(), &et("mango", "m")).unwrap();
        let loaded = load_all_email_templates(dir.path()).unwrap();
        assert_eq!(
            loaded.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["apple", "mango", "zebra"]
        );
    }

    #[test]
    fn missing_root_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let nonexistent = dir.path().join("not_here");
        let loaded = load_all_email_templates(&nonexistent).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn empty_root_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_all_email_templates(dir.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn root_pointing_at_a_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("not_a_dir");
        std::fs::write(&file_path, "x").unwrap();
        let err = load_all_email_templates(&file_path).unwrap_err();
        assert!(matches!(err, Error::InvalidFormat { .. }), "got {err:?}");
    }

    #[test]
    fn dir_without_template_yaml_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("empty_dir")).unwrap();
        save_email_template(dir.path(), &et("real", "body")).unwrap();
        let loaded = load_all_email_templates(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "real");
    }

    #[test]
    fn name_mismatch_with_dir_name_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let wrong_dir = dir.path().join("on_disk_name");
        std::fs::create_dir_all(&wrong_dir).unwrap();
        std::fs::write(
            wrong_dir.join("template.yaml"),
            "name: in_yaml_name\nsubject: x\n",
        )
        .unwrap();
        std::fs::write(wrong_dir.join("body.html"), "").unwrap();
        std::fs::write(wrong_dir.join("body.txt"), "").unwrap();
        let err = load_all_email_templates(dir.path()).unwrap_err();
        match err {
            Error::InvalidFormat { message, .. } => {
                assert!(message.contains("on_disk_name"));
                assert!(message.contains("in_yaml_name"));
            }
            other => panic!("expected InvalidFormat, got {other:?}"),
        }
    }

    #[test]
    fn unknown_yaml_field_is_ignored_for_forward_compat() {
        let dir = tempfile::tempdir().unwrap();
        let tpl_dir = dir.path().join("future");
        std::fs::create_dir_all(&tpl_dir).unwrap();
        std::fs::write(
            tpl_dir.join("template.yaml"),
            "name: future\nsubject: s\nfuture_v2_field: surprise\n",
        )
        .unwrap();
        std::fs::write(tpl_dir.join("body.html"), "<p>hi</p>").unwrap();
        std::fs::write(tpl_dir.join("body.txt"), "hi").unwrap();
        let loaded = load_all_email_templates(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "future");
    }

    #[test]
    fn save_rejects_path_traversal_in_name() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["../evil", "..", ".", "", "a/b", "a\\b"] {
            let bad_et = EmailTemplate {
                name: bad.into(),
                subject: "x".into(),
                body_html: String::new(),
                body_plaintext: String::new(),
                description: None,
                preheader: None,
                should_inline_css: None,
                tags: vec![],
            };
            let err = save_email_template(dir.path(), &bad_et).unwrap_err();
            assert!(
                matches!(err, Error::InvalidFormat { .. }),
                "name {bad:?} should be rejected; got {err:?}"
            );
        }
    }

    #[test]
    fn save_overwrites_existing_template() {
        let dir = tempfile::tempdir().unwrap();
        save_email_template(dir.path(), &et("ovr", "v1\n")).unwrap();
        save_email_template(dir.path(), &et("ovr", "v2\n")).unwrap();
        let loaded = load_all_email_templates(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].body_html, "v2\n");
    }

    #[test]
    fn body_without_trailing_newline_round_trips_byte_exact() {
        let dir = tempfile::tempdir().unwrap();
        let original = EmailTemplate {
            name: "no_eol".into(),
            subject: "x".into(),
            body_html: "<p>Hello</p>".into(),
            body_plaintext: "Hello".into(),
            description: None,
            preheader: None,
            should_inline_css: None,
            tags: vec![],
        };
        save_email_template(dir.path(), &original).unwrap();
        let loaded = load_all_email_templates(dir.path()).unwrap();
        assert_eq!(loaded[0].body_html, "<p>Hello</p>");
        assert_eq!(loaded[0].body_plaintext, "Hello");
        assert_eq!(loaded[0], original);
    }

    #[test]
    fn empty_body_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let empty = EmailTemplate {
            name: "blank".into(),
            subject: "x".into(),
            body_html: String::new(),
            body_plaintext: String::new(),
            description: None,
            preheader: None,
            should_inline_css: None,
            tags: vec![],
        };
        save_email_template(dir.path(), &empty).unwrap();
        let loaded = load_all_email_templates(dir.path()).unwrap();
        assert_eq!(loaded[0], empty);
    }

    #[test]
    fn missing_body_files_default_to_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        let tpl_dir = dir.path().join("minimal");
        std::fs::create_dir_all(&tpl_dir).unwrap();
        std::fs::write(tpl_dir.join("template.yaml"), "name: minimal\nsubject: s\n").unwrap();
        // No body.html or body.txt files
        let loaded = load_all_email_templates(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].body_html, "");
        assert_eq!(loaded[0].body_plaintext, "");
    }

    #[test]
    fn template_yaml_has_header_comment() {
        let dir = tempfile::tempdir().unwrap();
        save_email_template(dir.path(), &et("commented", "<p>hi</p>")).unwrap();
        let text =
            std::fs::read_to_string(dir.path().join("commented").join("template.yaml")).unwrap();
        assert!(
            text.starts_with("# Generated by braze-sync."),
            "expected header comment; got:\n{text}"
        );
    }

    #[test]
    fn description_none_omits_field_from_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let no_desc = EmailTemplate {
            name: "nodesc".into(),
            subject: "x".into(),
            body_html: String::new(),
            body_plaintext: String::new(),
            description: None,
            preheader: None,
            should_inline_css: None,
            tags: vec![],
        };
        save_email_template(dir.path(), &no_desc).unwrap();
        let text =
            std::fs::read_to_string(dir.path().join("nodesc").join("template.yaml")).unwrap();
        assert!(
            !text.contains("description"),
            "None description should not be serialized; got:\n{text}"
        );
        let loaded = load_all_email_templates(dir.path()).unwrap();
        assert_eq!(loaded[0], no_desc);
    }
}
