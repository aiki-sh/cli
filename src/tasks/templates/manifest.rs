use crate::error::{AikiError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

const CURRENT_SCHEMA: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct RepoManifest {
    pub schema: u32,
    pub templates: HashMap<String, PluginTemplateEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PluginTemplateEntry {
    pub source_version: String,
    pub install_root: String,
    pub files: HashMap<String, FileEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub checksum: String,
    pub version: Option<String>,
    pub installed_at: String,
}

pub fn checksum(content: &[u8]) -> String {
    let hash = Sha256::digest(content);
    format!("sha256:{:x}", hash)
}

impl RepoManifest {
    pub fn new() -> Self {
        Self {
            schema: CURRENT_SCHEMA,
            templates: HashMap::new(),
        }
    }

    pub fn load(repo_root: &Path) -> Result<Option<Self>> {
        let path = repo_root.join(".aiki").join(".manifest.json");
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&path)?;

        // First try to parse just the schema field to check compatibility
        #[derive(Deserialize)]
        struct SchemaCheck {
            schema: Option<u32>,
        }

        match serde_json::from_str::<SchemaCheck>(&content) {
            Ok(check) => {
                if let Some(schema) = check.schema {
                    if schema > CURRENT_SCHEMA {
                        return Err(AikiError::Other(anyhow::anyhow!(
                            "Manifest schema version {} is not supported by this CLI (max: {}). \
                             Please upgrade your CLI to work with this repository.",
                            schema,
                            CURRENT_SCHEMA
                        )));
                    }
                }
            }
            Err(_) => {
                eprintln!(
                    // stderr-ok: pre-TUI
                    "Warning: corrupt manifest at {}, creating fresh manifest",
                    path.display()
                );
                return Ok(Some(Self::new()));
            }
        }

        match serde_json::from_str::<RepoManifest>(&content) {
            Ok(manifest) => Ok(Some(manifest)),
            Err(_) => {
                eprintln!(
                    // stderr-ok: pre-TUI
                    "Warning: corrupt manifest at {}, creating fresh manifest",
                    path.display()
                );
                Ok(Some(Self::new()))
            }
        }
    }

    pub fn save(&self, repo_root: &Path) -> Result<()> {
        let dir = repo_root.join(".aiki");
        std::fs::create_dir_all(&dir)?;

        let path = dir.join(".manifest.json");
        let content = serde_json::to_string_pretty(self).map_err(|e| {
            AikiError::Other(anyhow::anyhow!("Failed to serialize manifest: {}", e))
        })?;

        // Atomic write: write to temp file then rename
        let temp_path = dir.join(".manifest.json.tmp");
        std::fs::write(&temp_path, &content)?;
        std::fs::rename(&temp_path, &path)?;

        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_plugin(&self, plugin_ref: &str) -> Option<&PluginTemplateEntry> {
        self.templates.get(plugin_ref)
    }

    pub fn get_or_create_plugin(
        &mut self,
        plugin_ref: &str,
        source_version: &str,
        install_root: &str,
    ) -> &mut PluginTemplateEntry {
        self.templates
            .entry(plugin_ref.to_string())
            .or_insert_with(|| PluginTemplateEntry {
                source_version: source_version.to_string(),
                install_root: install_root.to_string(),
                files: HashMap::new(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_round_trip() {
        let dir = TempDir::new().unwrap();
        let repo_root = dir.path();
        fs::create_dir_all(repo_root.join(".aiki")).unwrap();

        let mut manifest = RepoManifest::new();
        manifest.get_or_create_plugin("org/my-plugin", "1.0.0", "templates/org/my-plugin");
        {
            let plugin = manifest.templates.get_mut("org/my-plugin").unwrap();
            plugin.files.insert(
                "review.md".to_string(),
                FileEntry {
                    checksum: checksum(b"hello"),
                    version: Some("1.0.0".to_string()),
                    installed_at: "2026-03-06T00:00:00Z".to_string(),
                },
            );
        }

        manifest.save(repo_root).unwrap();

        let loaded = RepoManifest::load(repo_root).unwrap().unwrap();
        assert_eq!(loaded.schema, 1);
        let plugin = loaded.get_plugin("org/my-plugin").unwrap();
        assert_eq!(plugin.source_version, "1.0.0");
        assert_eq!(plugin.install_root, "templates/org/my-plugin");
        assert_eq!(plugin.files.len(), 1);
        let file = plugin.files.get("review.md").unwrap();
        assert_eq!(file.checksum, checksum(b"hello"));
        assert_eq!(file.version, Some("1.0.0".to_string()));
    }

    #[test]
    fn test_checksum_consistency() {
        let a = checksum(b"test data");
        let b = checksum(b"test data");
        assert_eq!(a, b);
        assert!(a.starts_with("sha256:"));

        let c = checksum(b"different data");
        assert_ne!(a, c);
    }

    #[test]
    fn test_corrupt_manifest_returns_fresh() {
        let dir = TempDir::new().unwrap();
        let repo_root = dir.path();
        let aiki_dir = repo_root.join(".aiki");
        fs::create_dir_all(&aiki_dir).unwrap();
        fs::write(aiki_dir.join(".manifest.json"), "not valid json!!!").unwrap();

        let loaded = RepoManifest::load(repo_root).unwrap().unwrap();
        assert_eq!(loaded.schema, CURRENT_SCHEMA);
        assert!(loaded.templates.is_empty());
    }

    #[test]
    fn test_unsupported_schema_returns_error() {
        let dir = TempDir::new().unwrap();
        let repo_root = dir.path();
        let aiki_dir = repo_root.join(".aiki");
        fs::create_dir_all(&aiki_dir).unwrap();
        fs::write(
            aiki_dir.join(".manifest.json"),
            r#"{"schema": 999, "templates": {}}"#,
        )
        .unwrap();

        let result = RepoManifest::load(repo_root);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("999"));
        assert!(err_msg.contains("upgrade"));
    }

    #[test]
    fn test_load_nonexistent_returns_none() {
        let dir = TempDir::new().unwrap();
        let result = RepoManifest::load(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_atomic_write_no_temp_file_lingers() {
        let dir = TempDir::new().unwrap();
        let repo_root = dir.path();
        fs::create_dir_all(repo_root.join(".aiki")).unwrap();

        let manifest = RepoManifest::new();
        manifest.save(repo_root).unwrap();

        let temp_path = repo_root.join(".aiki").join(".manifest.json.tmp");
        assert!(
            !temp_path.exists(),
            "temp file should not linger after save"
        );

        let manifest_path = repo_root.join(".aiki").join(".manifest.json");
        assert!(manifest_path.exists(), "manifest file should exist");
    }

    #[test]
    fn test_get_or_create_plugin_existing() {
        let mut manifest = RepoManifest::new();
        manifest.get_or_create_plugin("org/plug", "1.0.0", "templates/org/plug");
        // Call again with different version — should return existing entry unchanged
        let plugin = manifest.get_or_create_plugin("org/plug", "2.0.0", "other/root");
        assert_eq!(plugin.source_version, "1.0.0"); // original preserved
        assert_eq!(plugin.install_root, "templates/org/plug");
    }
}
