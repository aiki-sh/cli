//! Parse plan metadata from markdown files

use std::path::Path;

/// Parsed metadata from a plan markdown file.
#[derive(Debug, Clone)]
pub struct PlanMetadata {
    /// The path that was parsed
    pub path: String,
    /// Title extracted from first H1 heading
    pub title: Option<String>,
    /// Whether the plan is marked as a draft in frontmatter
    pub draft: bool,
}

/// Parse plan metadata from a markdown file.
///
/// Extracts:
/// - Title: first `# ` heading
pub fn parse_plan_metadata(path: &Path) -> PlanMetadata {
    let path_str = path.to_string_lossy().to_string();
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => {
            return PlanMetadata {
                path: path_str,
                title: None,
                draft: false,
            }
        }
    };

    let mut meta = parse_plan_content(&content);
    meta.path = path_str;
    meta
}

/// Strip YAML frontmatter from content, returning (frontmatter_yaml, body).
///
/// If the content starts with `---`, finds the closing `---` and splits.
/// Returns the YAML string (without delimiters) and the remaining body.
fn strip_frontmatter(content: &str) -> (Option<&str>, &str) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (None, content);
    }

    // Find end of first line (the opening ---)
    let after_open = match trimmed[3..].find('\n') {
        Some(idx) => &trimmed[3 + idx + 1..],
        None => return (None, content), // Only "---" with no newline
    };

    // Find the closing ---
    // Look for "\n---" at start of a line
    if let Some(close_idx) = after_open.find("\n---") {
        let yaml = &after_open[..close_idx];
        let rest_start = close_idx + 4; // skip "\n---"
        let body = if rest_start < after_open.len() {
            // Skip past the closing --- line end
            let remaining = &after_open[rest_start..];
            remaining.trim_start_matches(['\n', '\r'])
        } else {
            ""
        };
        (Some(yaml), body)
    } else if after_open.starts_with("---") {
        // Edge case: closing --- is the very first line after opening
        let yaml = "";
        let body = after_open[3..].trim_start_matches(['\n', '\r']);
        (Some(yaml), body)
    } else {
        // No closing delimiter
        (None, content)
    }
}

/// Extract the `draft` field from YAML frontmatter.
fn parse_draft_from_yaml(yaml: &str) -> bool {
    // Simple line-based parsing to avoid pulling in full YAML parser
    for line in yaml.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("draft:") {
            let value = value.trim();
            return value == "true";
        }
    }
    false
}

/// Parse plan metadata from markdown content string.
fn parse_plan_content(content: &str) -> PlanMetadata {
    let (frontmatter_yaml, body) = strip_frontmatter(content);

    let draft = frontmatter_yaml.map_or(false, parse_draft_from_yaml);

    let mut title = None;

    for line in body.lines() {
        if let Some(h1_text) = line.strip_prefix("# ") {
            title = Some(h1_text.trim().to_string());
            break;
        }
    }

    PlanMetadata {
        path: String::new(),
        title,
        draft,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic() {
        let content = "# My Feature\n\nThis is the description of my feature.\n\n## Details\n";
        let meta = parse_plan_content(content);
        assert_eq!(meta.title.as_deref(), Some("My Feature"));
        assert!(!meta.draft);
    }

    #[test]
    fn test_parse_no_h1() {
        let content = "## Not an H1\n\nSome text.\n";
        let meta = parse_plan_content(content);
        assert_eq!(meta.title, None);
    }

    #[test]
    fn test_parse_empty() {
        let meta = parse_plan_content("");
        assert_eq!(meta.title, None);
        assert!(!meta.draft);
    }

    #[test]
    fn test_parse_title_with_extra_spaces() {
        let content = "#   Spaced Title  \n\nDesc.\n";
        let meta = parse_plan_content(content);
        assert_eq!(meta.title.as_deref(), Some("Spaced Title"));
    }

    #[test]
    fn test_parse_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.md");
        std::fs::write(&path, "# Test Plan\n\nA description.\n").unwrap();

        let meta = parse_plan_metadata(&path);
        assert_eq!(meta.title.as_deref(), Some("Test Plan"));
        assert!(!meta.draft);
    }

    #[test]
    fn test_parse_nonexistent_file() {
        let meta = parse_plan_metadata(Path::new("/nonexistent/path.md"));
        assert_eq!(meta.title, None);
    }

    // --- Frontmatter tests ---

    #[test]
    fn test_parse_with_frontmatter() {
        let content = "---\ndraft: true\n---\n\n# My Feature\n\nDescription here.\n";
        let meta = parse_plan_content(content);
        assert_eq!(meta.title.as_deref(), Some("My Feature"));
        assert!(meta.draft);
    }

    #[test]
    fn test_parse_with_frontmatter_draft_false() {
        let content = "---\ndraft: false\n---\n\n# My Feature\n\nDescription here.\n";
        let meta = parse_plan_content(content);
        assert_eq!(meta.title.as_deref(), Some("My Feature"));
        assert!(!meta.draft);
    }

    #[test]
    fn test_parse_with_frontmatter_no_draft() {
        let content = "---\nstatus: Draft\n---\n\n# My Feature\n\nDescription here.\n";
        let meta = parse_plan_content(content);
        assert_eq!(meta.title.as_deref(), Some("My Feature"));
        assert!(!meta.draft);
    }

    #[test]
    fn test_parse_with_empty_frontmatter() {
        let content = "---\n---\n\n# My Feature\n\nDescription here.\n";
        let meta = parse_plan_content(content);
        assert_eq!(meta.title.as_deref(), Some("My Feature"));
        assert!(!meta.draft);
    }

    #[test]
    fn test_parse_frontmatter_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("draft.md");
        std::fs::write(
            &path,
            "---\ndraft: true\n---\n\n# Draft Plan\n\nStill writing.\n",
        )
        .unwrap();

        let meta = parse_plan_metadata(&path);
        assert_eq!(meta.title.as_deref(), Some("Draft Plan"));
        assert!(meta.draft);
    }

    // --- strip_frontmatter tests ---

    #[test]
    fn test_strip_frontmatter_none() {
        let (yaml, body) = strip_frontmatter("# Hello\n\nWorld.\n");
        assert!(yaml.is_none());
        assert_eq!(body, "# Hello\n\nWorld.\n");
    }

    #[test]
    fn test_strip_frontmatter_basic() {
        let (yaml, body) = strip_frontmatter("---\ndraft: true\n---\n\n# Hello\n");
        assert_eq!(yaml.unwrap(), "draft: true");
        assert_eq!(body, "# Hello\n");
    }

    #[test]
    fn test_strip_frontmatter_unclosed() {
        let content = "---\ndraft: true\n# Hello\n";
        let (yaml, body) = strip_frontmatter(content);
        assert!(yaml.is_none());
        assert_eq!(body, content);
    }
}
