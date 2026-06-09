//! Plugin manifest (`plugin.yaml`) parsing and validation.
//!
//! Every plugin is expected to have a `plugin.yaml` at its root containing
//! optional metadata such as a human-readable display name and description.

use std::path::Path;

use serde::Deserialize;

use crate::error::{AikiError, Result};

/// Filename for the plugin manifest.
pub const PLUGIN_MANIFEST_FILENAME: &str = "plugin.yaml";

/// Parsed contents of a `plugin.yaml` manifest.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    /// Human-readable display name (recommended, optional).
    pub name: Option<String>,
    /// One-line summary (recommended, optional).
    #[allow(dead_code)]
    pub description: Option<String>,
}

/// Parse `plugin.yaml` from a plugin directory.
///
/// Returns `Ok(manifest)` if the file exists and is valid YAML.
/// Returns an error if the file is missing or cannot be parsed.
pub fn load_manifest(plugin_dir: &Path) -> Result<PluginManifest> {
    let manifest_path = plugin_dir.join(PLUGIN_MANIFEST_FILENAME);

    if !manifest_path.is_file() {
        return Err(AikiError::PluginOperationFailed {
            plugin: plugin_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            details: format!(
                "Missing {PLUGIN_MANIFEST_FILENAME}. Every plugin must have a plugin.yaml at its root."
            ),
        });
    }

    let contents = std::fs::read_to_string(&manifest_path).map_err(|e| {
        AikiError::PluginOperationFailed {
            plugin: plugin_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            details: format!("Failed to read {PLUGIN_MANIFEST_FILENAME}: {e}"),
        }
    })?;

    let manifest: PluginManifest =
        serde_yaml::from_str(&contents).map_err(|e| AikiError::PluginOperationFailed {
            plugin: plugin_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            details: format!("Failed to parse {PLUGIN_MANIFEST_FILENAME}: {e}"),
        })?;

    Ok(manifest)
}

/// Resolve the display name for a plugin.
///
/// Fallback chain:
/// 1. `plugin.yaml` `name:` field
/// 2. `hooks.yaml` `name:` field (for standalone hook files)
/// 3. `{namespace}/{plugin}` path
pub fn resolve_display_name(plugin_dir: &Path, fallback_path: &str) -> String {
    // 1. Try plugin.yaml name
    if let Ok(manifest) = load_manifest(plugin_dir) {
        if let Some(name) = manifest.name {
            if !name.is_empty() {
                return name;
            }
        }
    }

    // 2. Try hooks.yaml name field
    if let Some(name) = read_hooks_yaml_name(plugin_dir) {
        if !name.is_empty() {
            return name;
        }
    }

    // 3. Fall back to namespace/plugin path
    fallback_path.to_string()
}

/// Read the `name` field from hooks.yaml/hooks.yml if present.
fn read_hooks_yaml_name(plugin_dir: &Path) -> Option<String> {
    let candidates = ["hooks.yaml", "hooks.yml"];
    for filename in &candidates {
        let path = plugin_dir.join(filename);
        if path.is_file() {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&contents) {
                    if let Some(name) = value.get("name").and_then(|v| v.as_str()) {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_full_manifest() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("plugin.yaml"),
            "name: ESLint Standard\ndescription: Auto-lint JS files\n",
        )
        .unwrap();

        let manifest = load_manifest(tmp.path()).unwrap();
        assert_eq!(manifest.name.as_deref(), Some("ESLint Standard"));
        assert_eq!(manifest.description.as_deref(), Some("Auto-lint JS files"));
    }

    #[test]
    fn test_parse_manifest_name_only() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("plugin.yaml"), "name: My Plugin\n").unwrap();

        let manifest = load_manifest(tmp.path()).unwrap();
        assert_eq!(manifest.name.as_deref(), Some("My Plugin"));
        assert_eq!(manifest.description, None);
    }

    #[test]
    fn test_parse_empty_manifest() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("plugin.yaml"), "{}\n").unwrap();

        let manifest = load_manifest(tmp.path()).unwrap();
        assert_eq!(manifest.name, None);
        assert_eq!(manifest.description, None);
    }

    #[test]
    fn test_missing_manifest_errors() {
        let tmp = TempDir::new().unwrap();
        let result = load_manifest(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing plugin.yaml"));
    }

    #[test]
    fn test_invalid_yaml_errors() {
        let tmp = TempDir::new().unwrap();
        // Use YAML that parses as a sequence, which can't deserialize into a struct
        std::fs::write(tmp.path().join("plugin.yaml"), "- item1\n- item2\n").unwrap();

        let result = load_manifest(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Failed to parse"));
    }

    #[test]
    fn test_display_name_from_manifest() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("plugin.yaml"), "name: ESLint Standard\n").unwrap();

        let name = resolve_display_name(tmp.path(), "eslint/standard");
        assert_eq!(name, "ESLint Standard");
    }

    #[test]
    fn test_display_name_from_hooks_yaml() {
        let tmp = TempDir::new().unwrap();
        // No plugin.yaml, but hooks.yaml has a name
        std::fs::write(
            tmp.path().join("hooks.yaml"),
            "name: My Hook\nversion: '1'\n",
        )
        .unwrap();

        let name = resolve_display_name(tmp.path(), "myns/myhook");
        assert_eq!(name, "My Hook");
    }

    #[test]
    fn test_display_name_fallback_to_path() {
        let tmp = TempDir::new().unwrap();
        // No plugin.yaml, no hooks.yaml
        let name = resolve_display_name(tmp.path(), "mycompany/lint");
        assert_eq!(name, "mycompany/lint");
    }

    #[test]
    fn test_display_name_manifest_takes_priority_over_hooks() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("plugin.yaml"), "name: From Manifest\n").unwrap();
        std::fs::write(
            tmp.path().join("hooks.yaml"),
            "name: From Hooks\nversion: '1'\n",
        )
        .unwrap();

        let name = resolve_display_name(tmp.path(), "ns/plugin");
        assert_eq!(name, "From Manifest");
    }

    #[test]
    fn test_display_name_empty_manifest_name_falls_through() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("plugin.yaml"), "name: ''\n").unwrap();
        std::fs::write(
            tmp.path().join("hooks.yaml"),
            "name: Hook Name\nversion: '1'\n",
        )
        .unwrap();

        let name = resolve_display_name(tmp.path(), "ns/plugin");
        assert_eq!(name, "Hook Name");
    }
}
