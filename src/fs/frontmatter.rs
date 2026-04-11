//! YAML frontmatter helper.
//!
//! Strict about the fences: the file must start with `---\n` (or
//! `---\r\n`) and the YAML must end with a line that is exactly `---`.
//! Anything else returns a typed error so the validate command can
//! report it cleanly. The body after the closing fence is returned
//! verbatim, trailing newline included.

use crate::error::{Error, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::path::Path;

/// Parse a frontmatter document from `text`. Returns the deserialized
/// frontmatter and the remaining body as a slice into the input.
///
/// Errors are reported as [`Error::InvalidFormat`] / [`Error::YamlParse`]
/// with `path` attached so callers can surface filename context.
pub fn parse<'a, T>(path: &Path, text: &'a str) -> Result<(T, &'a str)>
where
    T: DeserializeOwned,
{
    // Strip an optional UTF-8 BOM and leading whitespace-free start.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let after_open = strip_fence_line(text).ok_or_else(|| Error::InvalidFormat {
        path: path.to_path_buf(),
        message: "missing opening `---` frontmatter fence".into(),
    })?;

    // Find the closing fence: a line that is exactly `---` (CRLF tolerated).
    let (yaml, body) = split_at_closing_fence(after_open).ok_or_else(|| Error::InvalidFormat {
        path: path.to_path_buf(),
        message: "missing closing `---` frontmatter fence".into(),
    })?;

    let parsed: T = serde_yml::from_str(yaml).map_err(|source| Error::YamlParse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok((parsed, body))
}

/// Render `frontmatter` and `body` back to a single text document. The
/// inverse of [`parse`]. Always emits LF line endings and ensures the
/// frontmatter section ends with a newline before the closing fence.
pub fn render<T: Serialize>(path: &Path, frontmatter: &T, body: &str) -> Result<String> {
    let yaml = serde_yml::to_string(frontmatter).map_err(|e| Error::InvalidFormat {
        path: path.to_path_buf(),
        message: format!("frontmatter serialization failed: {e}"),
    })?;

    let mut out = String::with_capacity(yaml.len() + body.len() + 16);
    out.push_str("---\n");
    out.push_str(&yaml);
    if !yaml.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("---\n");
    out.push_str(body);
    Ok(out)
}

/// If `text` starts with a `---` fence line, return the rest. Tolerates
/// both `\n` and `\r\n` line endings.
fn strip_fence_line(text: &str) -> Option<&str> {
    if let Some(rest) = text.strip_prefix("---\n") {
        return Some(rest);
    }
    if let Some(rest) = text.strip_prefix("---\r\n") {
        return Some(rest);
    }
    None
}

/// Walk `text` line by line until we hit a line that is exactly `---`.
/// Returns `(yaml_section, body)` where `body` starts immediately after
/// the line terminator that follows the closing fence.
fn split_at_closing_fence(text: &str) -> Option<(&str, &str)> {
    let mut cursor = 0usize;
    let bytes = text.as_bytes();
    while cursor < bytes.len() {
        let line_start = cursor;
        let line_end = match bytes[cursor..].iter().position(|&b| b == b'\n') {
            Some(off) => cursor + off,
            None => bytes.len(),
        };
        // The fence line itself: trim a trailing \r so CRLF is tolerated.
        let line_bytes_end = if line_end > line_start && bytes[line_end - 1] == b'\r' {
            line_end - 1
        } else {
            line_end
        };
        if &bytes[line_start..line_bytes_end] == b"---" {
            let yaml = &text[..line_start];
            // body starts after the \n that ends the fence line, or end-of-input
            let body_start = if line_end < bytes.len() {
                line_end + 1
            } else {
                bytes.len()
            };
            return Some((yaml, &text[body_start..]));
        }
        cursor = if line_end < bytes.len() {
            line_end + 1
        } else {
            bytes.len()
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::path::PathBuf;

    #[derive(Debug, Deserialize, Serialize, PartialEq)]
    struct Meta {
        name: String,
        #[serde(default)]
        tags: Vec<String>,
    }

    fn p() -> PathBuf {
        PathBuf::from("test.liquid")
    }

    #[test]
    fn parse_minimal() {
        let text = "---\nname: hi\n---\nbody line\n";
        let (meta, body): (Meta, &str) = parse(&p(), text).unwrap();
        assert_eq!(
            meta,
            Meta {
                name: "hi".into(),
                tags: vec![]
            }
        );
        assert_eq!(body, "body line\n");
    }

    #[test]
    fn parse_empty_body_after_fence() {
        let text = "---\nname: empty\n---\n";
        let (meta, body): (Meta, &str) = parse(&p(), text).unwrap();
        assert_eq!(meta.name, "empty");
        assert_eq!(body, "");
    }

    #[test]
    fn parse_no_trailing_newline_on_body() {
        let text = "---\nname: x\n---\nfinal";
        let (_meta, body): (Meta, &str) = parse(&p(), text).unwrap();
        assert_eq!(body, "final");
    }

    #[test]
    fn parse_crlf_line_endings_tolerated() {
        let text = "---\r\nname: crlf\r\n---\r\nbody\r\n";
        let (meta, body): (Meta, &str) = parse(&p(), text).unwrap();
        assert_eq!(meta.name, "crlf");
        assert_eq!(body, "body\r\n");
    }

    #[test]
    fn parse_bom_stripped() {
        let text = "\u{feff}---\nname: bom\n---\nbody\n";
        let (meta, _): (Meta, &str) = parse(&p(), text).unwrap();
        assert_eq!(meta.name, "bom");
    }

    #[test]
    fn parse_body_containing_triple_dash_is_preserved() {
        // The body legitimately contains a `---` separator. Only the
        // first closing fence terminates the frontmatter.
        let text = "---\nname: x\n---\nintro\n---\nmore body\n";
        let (_, body): (Meta, &str) = parse(&p(), text).unwrap();
        assert_eq!(body, "intro\n---\nmore body\n");
    }

    #[test]
    fn parse_missing_opening_fence_errors() {
        let text = "name: x\n---\nbody\n";
        let err = parse::<Meta>(&p(), text).unwrap_err();
        match err {
            Error::InvalidFormat { message, .. } => assert!(message.contains("opening")),
            other => panic!("expected InvalidFormat, got {other:?}"),
        }
    }

    #[test]
    fn parse_missing_closing_fence_errors() {
        let text = "---\nname: x\nbody never closes\n";
        let err = parse::<Meta>(&p(), text).unwrap_err();
        match err {
            Error::InvalidFormat { message, .. } => assert!(message.contains("closing")),
            other => panic!("expected InvalidFormat, got {other:?}"),
        }
    }

    #[test]
    fn parse_invalid_yaml_in_frontmatter_errors() {
        let text = "---\nname: [unterminated\n---\nbody\n";
        let err = parse::<Meta>(&p(), text).unwrap_err();
        assert!(matches!(err, Error::YamlParse { .. }), "got {err:?}");
    }

    #[test]
    fn render_round_trip() {
        let meta = Meta {
            name: "round".into(),
            tags: vec!["a".into(), "b".into()],
        };
        let body = "hello\nworld\n";
        let text = render(&p(), &meta, body).unwrap();
        assert!(text.starts_with("---\n"));
        assert!(text.contains("name: round"));
        assert!(text.ends_with("hello\nworld\n"));

        let (parsed, parsed_body): (Meta, &str) = parse(&p(), &text).unwrap();
        assert_eq!(parsed, meta);
        assert_eq!(parsed_body, body);
    }

    #[test]
    fn render_empty_body_round_trips() {
        let meta = Meta {
            name: "empty".into(),
            tags: vec![],
        };
        let text = render(&p(), &meta, "").unwrap();
        let (parsed, body): (Meta, &str) = parse(&p(), &text).unwrap();
        assert_eq!(parsed, meta);
        assert_eq!(body, "");
    }
}
