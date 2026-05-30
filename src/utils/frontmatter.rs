//! Shared utility for reading and writing YAML frontmatter in markdown files.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{AikiError, Result};

/// Parsed frontmatter as key-value pairs.
pub type Frontmatter = BTreeMap<String, serde_yaml::Value>;

/// Read frontmatter from a markdown file. Returns (frontmatter, body).
/// Returns empty map if no frontmatter block exists.
pub fn read_frontmatter(path: &Path) -> Result<(Frontmatter, String)> {
    let content = std::fs::read_to_string(path)?;
    parse_frontmatter(&content)
}

/// Parse frontmatter from markdown content. Returns (frontmatter, body).
/// Returns empty map if no frontmatter block exists.
fn parse_frontmatter(content: &str) -> Result<(Frontmatter, String)> {
    let trimmed = content.trim_start();

    if !trimmed.starts_with("---") {
        return Ok((Frontmatter::new(), content.to_string()));
    }

    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches(['\n', '\r']);

    // Search for CRLF delimiter first (5 bytes) before LF-only (4 bytes),
    // so we use the correct offset and don't leave a stray \r in the yaml.
    let (end_idx, delim_len) = if let Some(idx) = after_first.find("\r\n---") {
        (Some(idx), 5) // skip "\r\n---"
    } else if let Some(idx) = after_first.find("\n---") {
        (Some(idx), 4) // skip "\n---"
    } else {
        (None, 0)
    };

    match end_idx {
        Some(idx) => {
            let yaml_content = &after_first[..idx];
            let body_start = idx + delim_len;
            let body = after_first[body_start..].trim_start_matches(['\n', '\r']);

            let fm: Frontmatter =
                serde_yaml::from_str(yaml_content).map_err(|e| AikiError::Other(e.into()))?;
            Ok((fm, body.to_string()))
        }
        None => Err(AikiError::Other(anyhow::anyhow!(
            "Unterminated frontmatter: found opening '---' but no closing '---'"
        ))),
    }
}

/// Write frontmatter + body to a markdown file.
/// If frontmatter is empty, writes body only (no `---` block).
pub fn write_frontmatter(path: &Path, fm: &Frontmatter, body: &str) -> Result<()> {
    let content = render_frontmatter(fm, body);
    std::fs::write(path, content)?;
    Ok(())
}

/// Render frontmatter + body into a string.
fn render_frontmatter(fm: &Frontmatter, body: &str) -> String {
    if fm.is_empty() {
        return body.to_string();
    }

    let yaml = serde_yaml::to_string(fm).unwrap_or_default();
    // serde_yaml::to_string emits a leading "---\n" document marker; strip it
    // to avoid duplicating the delimiter we add ourselves.
    let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    format!("---\n{}---\n\n{}", yaml, body)
}

/// Update a single field in a file's frontmatter, preserving body.
/// Creates frontmatter block if none exists.
pub fn set_frontmatter_field(path: &Path, key: &str, value: serde_yaml::Value) -> Result<()> {
    let (mut fm, body) = read_frontmatter(path)?;
    fm.insert(key.to_string(), value);
    write_frontmatter(path, &fm, &body)
}

/// Remove a field from a file's frontmatter, preserving body.
/// If frontmatter becomes empty after removal, omit the `---` block entirely.
pub fn remove_frontmatter_field(path: &Path, key: &str) -> Result<()> {
    let (mut fm, body) = read_frontmatter(path)?;
    fm.remove(key);
    write_frontmatter(path, &fm, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn test_no_frontmatter() {
        let (fm, body) = parse_frontmatter("# Hello\n\nSome content.\n").unwrap();
        assert!(fm.is_empty());
        assert_eq!(body, "# Hello\n\nSome content.\n");
    }

    #[test]
    fn test_existing_frontmatter() {
        let content = "---\ndraft: true\ntitle: My Plan\n---\n\n# Hello\n\nBody here.\n";
        let (fm, body) = parse_frontmatter(content).unwrap();
        assert_eq!(fm.get("draft"), Some(&serde_yaml::Value::Bool(true)));
        assert_eq!(
            fm.get("title"),
            Some(&serde_yaml::Value::String("My Plan".to_string()))
        );
        assert_eq!(body, "# Hello\n\nBody here.\n");
    }

    #[test]
    fn test_empty_file() {
        let (fm, body) = parse_frontmatter("").unwrap();
        assert!(fm.is_empty());
        assert_eq!(body, "");
    }

    #[test]
    fn test_frontmatter_only_no_body() {
        let content = "---\ndraft: true\n---\n\n";
        let (fm, body) = parse_frontmatter(content).unwrap();
        assert_eq!(fm.get("draft"), Some(&serde_yaml::Value::Bool(true)));
        assert_eq!(body, "");
    }

    #[test]
    fn test_write_frontmatter_empty_map() {
        let fm = Frontmatter::new();
        let result = render_frontmatter(&fm, "# Hello\n");
        assert_eq!(result, "# Hello\n");
    }

    #[test]
    fn test_write_frontmatter_with_entries() {
        let mut fm = Frontmatter::new();
        fm.insert(
            "draft".to_string(),
            serde_yaml::Value::Bool(true),
        );
        let result = render_frontmatter(&fm, "# Hello\n");
        // Must not produce duplicate document markers (---\n---\n)
        assert_eq!(result, "---\ndraft: true\n---\n\n# Hello\n");
    }

    #[test]
    fn test_set_frontmatter_field_creates_new() {
        let f = write_temp("# No frontmatter\n\nBody.\n");
        set_frontmatter_field(f.path(), "draft", serde_yaml::Value::Bool(true)).unwrap();

        let (fm, body) = read_frontmatter(f.path()).unwrap();
        assert_eq!(fm.get("draft"), Some(&serde_yaml::Value::Bool(true)));
        assert_eq!(body, "# No frontmatter\n\nBody.\n");

        // Verify no duplicate document markers in written file
        let raw = std::fs::read_to_string(f.path()).unwrap();
        assert!(!raw.contains("---\n---"), "duplicate document markers found: {}", raw);
    }

    #[test]
    fn test_set_frontmatter_field_updates_existing() {
        let f = write_temp("---\ndraft: true\n---\n\n# Plan\n");
        set_frontmatter_field(f.path(), "draft", serde_yaml::Value::Bool(false)).unwrap();

        let (fm, body) = read_frontmatter(f.path()).unwrap();
        assert_eq!(fm.get("draft"), Some(&serde_yaml::Value::Bool(false)));
        assert_eq!(body, "# Plan\n");
    }

    #[test]
    fn test_remove_frontmatter_field() {
        let f = write_temp("---\ndraft: true\ntitle: My Plan\n---\n\n# Plan\n");
        remove_frontmatter_field(f.path(), "draft").unwrap();

        let (fm, body) = read_frontmatter(f.path()).unwrap();
        assert!(fm.get("draft").is_none());
        assert_eq!(
            fm.get("title"),
            Some(&serde_yaml::Value::String("My Plan".to_string()))
        );
        assert_eq!(body, "# Plan\n");
    }

    #[test]
    fn test_remove_last_field_removes_frontmatter_block() {
        let f = write_temp("---\ndraft: true\n---\n\n# Plan\n");
        remove_frontmatter_field(f.path(), "draft").unwrap();

        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(!content.contains("---"));
        assert_eq!(content, "# Plan\n");
    }

    #[test]
    fn test_crlf_frontmatter() {
        let content = "---\r\ndraft: true\r\n---\r\n\r\n# Hello\r\n\r\nBody.\r\n";
        let (fm, body) = parse_frontmatter(content).unwrap();
        assert_eq!(fm.get("draft"), Some(&serde_yaml::Value::Bool(true)));
        assert_eq!(body, "# Hello\r\n\r\nBody.\r\n");
    }

    #[test]
    fn test_unterminated_frontmatter() {
        let result = parse_frontmatter("---\ndraft: true\n\n# Body\n");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unterminated"));
    }
}
