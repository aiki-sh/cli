use include_dir::{include_dir, Dir};

// Canonical path: BUILTIN_TEMPLATES_SOURCE (see mod.rs). Macro requires a string literal.
static TEMPLATE_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/tasks/templates/core");

/// Subdirectories that belong to the aiki/default plugin.
/// Other top-level directories (e.g., `intel/`) are excluded.
const KNOWN_AIKI_SUBDIRS: &[&str] = &["explore", "review"];

/// Returns all built-in plugin templates as `(relative_path, content)` pairs.
///
/// Paths do NOT include the `aiki/` prefix (e.g., `plan.md`, `review/task.md`).
/// Only includes templates belonging to the aiki/default plugin (excludes other
/// plugin directories like `intel/`).
pub fn default_plugin_templates() -> Vec<(&'static str, &'static [u8])> {
    let mut templates = Vec::new();
    // Collect root-level .md files
    for file in TEMPLATE_DIR.files() {
        if file.path().extension().map_or(false, |e| e == "md") {
            templates.push((file.path().to_str().unwrap(), file.contents()));
        }
    }
    // Collect from known aiki subdirectories only
    for subdir in TEMPLATE_DIR.dirs() {
        let name = subdir
            .path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if KNOWN_AIKI_SUBDIRS.contains(&name) {
            collect_files(subdir, &mut templates);
        }
    }
    templates
}

/// Returns the set of known aiki/default template names (without `.md` extension).
///
/// Used for ref normalization (e.g., detecting that `aiki/plan` maps to `plan`).
pub fn known_default_template_names() -> Vec<String> {
    default_plugin_templates()
        .iter()
        .map(|(path, _)| path.trim_end_matches(".md").to_string())
        .collect()
}

fn collect_files(dir: &Dir<'static>, out: &mut Vec<(&'static str, &'static [u8])>) {
    for file in dir.files() {
        out.push((file.path().to_str().unwrap(), file.contents()));
    }
    for subdir in dir.dirs() {
        collect_files(subdir, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_plugin_templates_count() {
        let templates = default_plugin_templates();
        assert_eq!(
            templates.len(),
            14,
            "Expected 14 templates, got {}",
            templates.len()
        );
    }

    #[test]
    fn test_default_plugin_templates_names() {
        let templates = default_plugin_templates();
        let names: Vec<&str> = templates.iter().map(|(name, _)| *name).collect();

        let expected = [
            "decompose.md",
            "explore/code.md",
            "explore/plan.md",
            "explore/session.md",
            "explore/task.md",
            "fix.md",
            "loop.md",
            "plan.md",
            "resolve.md",
            "review/code.md",
            "review/plan.md",
            "review/task.md",
            "tldr.md",
        ];

        for name in &expected {
            assert!(
                names.contains(name),
                "Missing template: {name}. Found: {names:?}"
            );
        }
    }

    #[test]
    fn test_default_plugin_templates_non_empty() {
        let templates = default_plugin_templates();
        for (name, content) in &templates {
            assert!(!content.is_empty(), "Template {name} has empty content");
        }
    }
}
