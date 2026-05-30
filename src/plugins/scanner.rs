//! Reference scanning for plugin dependencies.
//!
//! Scans YAML and markdown files for three-part template references
//! (`ns/plugin/template`) and extracts unique `PluginRef` pairs.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use super::PluginRef;

/// Scan a directory for plugin references in hooks.yaml and templates/**/*.md.
///
/// Returns unique `PluginRef` pairs found in active template syntax.
/// Self-references (matching `self_ref`) are excluded.
pub fn derive_plugin_refs(dir: &Path, self_ref: Option<&PluginRef>) -> Vec<PluginRef> {
    let mut refs = HashSet::new();

    // Scan hooks.yaml / hooks.yml
    for name in &["hooks.yaml", "hooks.yml"] {
        let path = dir.join(name);
        if path.is_file() {
            if let Ok(content) = fs::read_to_string(&path) {
                for r in scan_yaml_for_refs(&content) {
                    refs.insert(r);
                }
            }
        }
    }

    // Scan templates/**/*.md recursively
    let templates_dir = dir.join("templates");
    if templates_dir.is_dir() {
        scan_templates_dir(&templates_dir, &mut refs);
    }

    // Filter out self-references
    let mut result: Vec<PluginRef> = refs.into_iter().collect();
    if let Some(self_plugin) = self_ref {
        result.retain(|r| r != self_plugin);
    }
    result.sort_by(|a, b| a.to_string().cmp(&b.to_string()));
    result
}

/// Recursively scan a templates directory for markdown files.
fn scan_templates_dir(dir: &Path, refs: &mut HashSet<PluginRef>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_templates_dir(&path, refs);
        } else if path.is_file() && path.extension().map_or(false, |e| e == "md") {
            if let Ok(content) = fs::read_to_string(&path) {
                for r in scan_markdown_for_refs(&content) {
                    refs.insert(r);
                }
            }
        }
    }
}

/// Scan YAML content for `template:` values that are three-part references.
///
/// Parses the YAML structure and recursively walks all mappings to find keys
/// named `template` whose values are strings. This avoids false positives from
/// block scalars and correctly handles `template:` in any nesting position
/// (e.g., list items, nested mappings).
pub fn scan_yaml_for_refs(content: &str) -> Vec<PluginRef> {
    let doc: serde_yaml::Value = match serde_yaml::from_str(content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut refs = Vec::new();
    collect_template_refs(&doc, &mut refs);
    refs
}

/// Recursively walk a YAML value tree, collecting plugin refs from `template:` keys.
fn collect_template_refs(value: &serde_yaml::Value, refs: &mut Vec<PluginRef>) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (k, v) in map {
                if let serde_yaml::Value::String(key) = k {
                    if key == "template" {
                        if let serde_yaml::Value::String(tmpl) = v {
                            if let Some(r) = extract_plugin_ref_from_three_part(tmpl) {
                                refs.push(r);
                            }
                        }
                        continue;
                    }
                }
                collect_template_refs(v, refs);
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for item in seq {
                collect_template_refs(item, refs);
            }
        }
        _ => {}
    }
}

/// Scan markdown content for `{{> ns/plugin/template}}` partial invocations.
///
/// Excludes references inside fenced code blocks, HTML comments, and inline code.
pub fn scan_markdown_for_refs(content: &str) -> Vec<PluginRef> {
    let cleaned = strip_excluded_regions(content);
    let mut refs = Vec::new();

    // Match {{> path }} patterns
    let mut pos = 0;
    while let Some(start) = cleaned[pos..].find("{{>") {
        let abs_start = pos + start + 3; // After "{{>"
        if let Some(end) = cleaned[abs_start..].find("}}") {
            let abs_end = abs_start + end;
            let path = cleaned[abs_start..abs_end].trim();
            if let Some(r) = extract_plugin_ref_from_three_part(path) {
                refs.push(r);
            }
            pos = abs_end + 2;
        } else {
            break;
        }
    }

    refs
}

/// Extract a `PluginRef` from a three-part path like `ns/plugin/template`.
///
/// Returns `None` if the path doesn't have exactly three `/`-separated parts
/// or if the parts don't form a valid plugin reference.
fn extract_plugin_ref_from_three_part(path: &str) -> Option<PluginRef> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != 3 {
        return None;
    }

    let ns = parts[0];
    let plugin = parts[1];

    if ns.is_empty() || plugin.is_empty() || parts[2].is_empty() {
        return None;
    }

    // Try to construct a valid PluginRef
    let ref_str = format!("{}/{}", ns, plugin);
    ref_str.parse().ok()
}

/// Strip regions that should not be scanned for references:
/// - Fenced code blocks (``` ... ```)
/// - HTML comments (<!-- ... -->)
/// - Inline code (` ... `)
fn strip_excluded_regions(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let chars: Vec<char> = content.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Check for fenced code block (```)
        if i + 2 < len && chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
            // Skip until closing ```
            i += 3;
            // Skip optional language tag (rest of line)
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            // Skip content until closing ```
            loop {
                if i >= len {
                    break;
                }
                if i + 2 < len && chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
                    i += 3;
                    // Skip rest of closing fence line
                    while i < len && chars[i] != '\n' {
                        i += 1;
                    }
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Check for HTML comment (<!-- ... -->)
        if i + 3 < len
            && chars[i] == '<'
            && chars[i + 1] == '!'
            && chars[i + 2] == '-'
            && chars[i + 3] == '-'
        {
            i += 4;
            loop {
                if i + 2 >= len {
                    i = len;
                    break;
                }
                if chars[i] == '-' && chars[i + 1] == '-' && chars[i + 2] == '>' {
                    i += 3;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // Check for inline code (` ... `)
        // But not triple backtick (already handled above)
        if chars[i] == '`' && !(i + 1 < len && chars[i + 1] == '`') {
            i += 1;
            while i < len && chars[i] != '`' && chars[i] != '\n' {
                i += 1;
            }
            if i < len && chars[i] == '`' {
                i += 1;
            }
            continue;
        }

        result.push(chars[i]);
        i += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scan_yaml_finds_template_refs() {
        let yaml = r#"
name: My Hook
review:
  template: aiki/core/review-base
build:
  template: somecorp/tools/build
other_key: aiki/core/not-a-template
"#;
        let refs = scan_yaml_for_refs(yaml);
        assert_eq!(refs.len(), 2);
        let names: Vec<String> = refs.iter().map(|r| r.to_string()).collect();
        assert!(names.contains(&"aiki/core".to_string()));
        assert!(names.contains(&"somecorp/tools".to_string()));
    }

    #[test]
    fn test_scan_yaml_ignores_two_part() {
        let yaml = "template: aiki/review\n";
        let refs = scan_yaml_for_refs(yaml);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_scan_markdown_finds_partials() {
        let md = "Some text\n{{> aiki/core/preamble}}\nMore text\n{{> somecorp/tools/footer}}\n";
        let refs = scan_markdown_for_refs(md);
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn test_scan_markdown_ignores_code_block() {
        let md = "Normal text\n```\n{{> aiki/core/example}}\n```\n{{> aiki/real/ref}}\n";
        let refs = scan_markdown_for_refs(md);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].to_string(), "aiki/real");
    }

    #[test]
    fn test_scan_markdown_ignores_html_comment() {
        let md = "<!-- {{> aiki/core/hidden}} -->\n{{> aiki/real/ref}}\n";
        let refs = scan_markdown_for_refs(md);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].to_string(), "aiki/real");
    }

    #[test]
    fn test_scan_markdown_ignores_inline_code() {
        let md = "Use `{{> aiki/core/example}}` for reference.\n{{> aiki/real/ref}}\n";
        let refs = scan_markdown_for_refs(md);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].to_string(), "aiki/real");
    }

    #[test]
    fn test_extract_three_part() {
        assert!(extract_plugin_ref_from_three_part("aiki/core/review").is_some());
        assert!(extract_plugin_ref_from_three_part("aiki/review").is_none()); // Two parts
        assert!(extract_plugin_ref_from_three_part("review").is_none()); // One part
        assert!(extract_plugin_ref_from_three_part("a/b/c/d").is_none()); // Four parts
    }

    #[test]
    fn test_derive_plugin_refs_self_filter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // Create hooks.yaml referencing self and another plugin (separate mappings)
        fs::write(
            dir.join("hooks.yaml"),
            "review:\n  template: aiki/way/review\nbuild:\n  template: aiki/core/base\n",
        )
        .unwrap();

        let self_ref: PluginRef = "aiki/way".parse().unwrap();
        let refs = derive_plugin_refs(dir, Some(&self_ref));

        // Should only contain aiki/core, not aiki/way (self)
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].to_string(), "aiki/core");
    }

    #[test]
    fn test_derive_plugin_refs_dedup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // Multiple references to the same plugin
        fs::write(
            dir.join("hooks.yaml"),
            "a:\n  template: aiki/core/review\nb:\n  template: aiki/core/fix\n",
        )
        .unwrap();

        let refs = derive_plugin_refs(dir, None);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].to_string(), "aiki/core");
    }

    #[test]
    fn test_derive_plugin_refs_from_templates() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // Create templates directory with markdown
        let tmpl_dir = dir.join("templates");
        fs::create_dir_all(&tmpl_dir).unwrap();
        fs::write(
            tmpl_dir.join("review.md"),
            "# Review\n{{> aiki/core/preamble}}\nDo the review.\n",
        )
        .unwrap();

        let refs = derive_plugin_refs(dir, None);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].to_string(), "aiki/core");
    }

    #[test]
    fn test_scan_yaml_finds_list_item_templates() {
        // Regression: line-prefix matching missed `- template:` in list items
        let yaml = r#"
hooks:
  - template: aiki/core/review-base
  - template: somecorp/tools/build
"#;
        let refs = scan_yaml_for_refs(yaml);
        assert_eq!(refs.len(), 2);
        let names: Vec<String> = refs.iter().map(|r| r.to_string()).collect();
        assert!(names.contains(&"aiki/core".to_string()));
        assert!(names.contains(&"somecorp/tools".to_string()));
    }

    #[test]
    fn test_scan_yaml_ignores_block_scalar_template() {
        // Regression: line-prefix matching falsely matched `template:` in block scalars
        let yaml = r#"
docs:
  description: |
    template: aiki/fake/ref
    This is just prose, not a real template key.
real:
  template: aiki/real/plugin
"#;
        let refs = scan_yaml_for_refs(yaml);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].to_string(), "aiki/real");
    }

    #[test]
    fn test_scan_yaml_deeply_nested_template() {
        let yaml = r#"
level1:
  level2:
    level3:
      template: aiki/deep/nested
"#;
        let refs = scan_yaml_for_refs(yaml);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].to_string(), "aiki/deep");
    }

    #[test]
    fn test_scan_yaml_invalid_yaml_returns_empty() {
        let yaml = "{{not valid yaml at all";
        let refs = scan_yaml_for_refs(yaml);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_strip_fenced_code_block() {
        let input = "before\n```rust\nhidden\n```\nafter";
        let result = strip_excluded_regions(input);
        assert!(result.contains("before"));
        assert!(result.contains("after"));
        assert!(!result.contains("hidden"));
    }

    #[test]
    fn test_strip_html_comment() {
        let input = "before<!-- hidden -->after";
        let result = strip_excluded_regions(input);
        assert!(result.contains("before"));
        assert!(result.contains("after"));
        assert!(!result.contains("hidden"));
    }

    #[test]
    fn test_strip_inline_code() {
        let input = "before `hidden` after";
        let result = strip_excluded_regions(input);
        assert!(result.contains("before"));
        assert!(result.contains("after"));
        assert!(!result.contains("hidden"));
    }
}
