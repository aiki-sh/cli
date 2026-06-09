//! Plugin lock file (`plugins.lock`) data model and I/O.
//!
//! The lock file pins each installed plugin to a specific git commit SHA,
//! ensuring reproducible installs across machines and sessions.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::plugins::PluginRef;

/// Header comment prepended to the lock file on save.
const LOCK_HEADER: &str = "\
# Auto-generated. Do not edit manually.\n\
# Run 'aiki plugin update' to advance pinned versions.\n\n";

/// Filename for the lock file within the `.aiki/` directory.
const LOCK_FILENAME: &str = "plugins.lock";

/// A single pinned plugin entry in the lock file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginLockEntry {
    /// Full 40-char git commit SHA.
    pub sha: String,
    /// Git clone URL (provenance).
    pub source: String,
    /// ISO 8601 timestamp of when this entry was written.
    pub resolved: String,
}

/// The full lock file: a sorted map of `"namespace/name"` → [`PluginLockEntry`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginLock {
    #[serde(flatten)]
    pub entries: BTreeMap<String, PluginLockEntry>,
}

impl PluginLock {
    /// Load the lock file from `{project_root}/.aiki/plugins.lock`.
    ///
    /// Returns an empty lock if the file does not exist.
    pub fn load(project_root: &Path) -> Result<Self> {
        let path = project_root.join(".aiki").join(LOCK_FILENAME);
        if !path.is_file() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(&path)?;
        let lock: PluginLock = toml::from_str(&contents).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        Ok(lock)
    }

    /// Serialize and write the lock file to `{project_root}/.aiki/plugins.lock`.
    ///
    /// Includes a header comment and writes entries sorted alphabetically by key.
    pub fn save(&self, project_root: &Path) -> Result<()> {
        let dir = project_root.join(".aiki");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(LOCK_FILENAME);
        let body = toml::to_string_pretty(self).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        let output = format!("{LOCK_HEADER}{body}");
        std::fs::write(&path, output)?;
        Ok(())
    }

    /// Look up a lock entry by plugin reference.
    #[must_use]
    pub fn get(&self, plugin_ref: &PluginRef) -> Option<&PluginLockEntry> {
        self.entries.get(&plugin_ref.to_string())
    }

    /// Insert or update a lock entry for the given plugin.
    pub fn insert(&mut self, plugin_ref: &PluginRef, entry: PluginLockEntry) {
        self.entries.insert(plugin_ref.to_string(), entry);
    }

    /// Remove a lock entry, returning it if it existed.
    pub fn remove(&mut self, plugin_ref: &PluginRef) -> Option<PluginLockEntry> {
        self.entries.remove(&plugin_ref.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_entry() -> PluginLockEntry {
        PluginLockEntry {
            sha: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2".to_string(),
            source: "https://github.com/eslint/standard.git".to_string(),
            resolved: "2026-03-28T14:22:01Z".to_string(),
        }
    }

    fn sample_ref() -> PluginRef {
        "eslint/standard".parse().unwrap()
    }

    #[test]
    fn load_returns_empty_when_file_missing() {
        let tmp = TempDir::new().unwrap();
        let lock = PluginLock::load(tmp.path()).unwrap();
        assert!(lock.entries.is_empty());
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = TempDir::new().unwrap();
        let mut lock = PluginLock::default();
        lock.insert(&sample_ref(), sample_entry());
        lock.save(tmp.path()).unwrap();

        let loaded = PluginLock::load(tmp.path()).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        let entry = loaded.get(&sample_ref()).unwrap();
        assert_eq!(entry.sha, "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2");
        assert_eq!(entry.source, "https://github.com/eslint/standard.git");
        assert_eq!(entry.resolved, "2026-03-28T14:22:01Z");
    }

    #[test]
    fn entries_are_sorted_in_output() {
        let tmp = TempDir::new().unwrap();
        let mut lock = PluginLock::default();

        let ref_z: PluginRef = "zoo/zebra".parse().unwrap();
        let ref_a: PluginRef = "alpha/apple".parse().unwrap();

        lock.insert(
            &ref_z,
            PluginLockEntry {
                sha: "bbbb".to_string(),
                source: "https://github.com/zoo/zebra.git".to_string(),
                resolved: "2026-01-01T00:00:00Z".to_string(),
            },
        );
        lock.insert(
            &ref_a,
            PluginLockEntry {
                sha: "aaaa".to_string(),
                source: "https://github.com/alpha/apple.git".to_string(),
                resolved: "2026-01-01T00:00:00Z".to_string(),
            },
        );

        lock.save(tmp.path()).unwrap();

        let contents =
            std::fs::read_to_string(tmp.path().join(".aiki/plugins.lock")).unwrap();
        let pos_alpha = contents.find("[\"alpha/apple\"]").unwrap();
        let pos_zoo = contents.find("[\"zoo/zebra\"]").unwrap();
        assert!(pos_alpha < pos_zoo, "entries should be sorted alphabetically");
    }

    #[test]
    fn header_comment_is_present() {
        let tmp = TempDir::new().unwrap();
        let lock = PluginLock::default();
        lock.save(tmp.path()).unwrap();

        let contents =
            std::fs::read_to_string(tmp.path().join(".aiki/plugins.lock")).unwrap();
        assert!(contents.starts_with("# Auto-generated. Do not edit manually."));
        assert!(contents.contains("Run 'aiki plugin update'"));
    }

    #[test]
    fn insert_overwrites_existing() {
        let mut lock = PluginLock::default();
        let pref = sample_ref();
        lock.insert(&pref, sample_entry());

        let updated = PluginLockEntry {
            sha: "deadbeef".repeat(5),
            source: "https://github.com/eslint/standard.git".to_string(),
            resolved: "2026-04-01T00:00:00Z".to_string(),
        };
        lock.insert(&pref, updated);

        assert_eq!(lock.entries.len(), 1);
        assert!(lock.get(&pref).unwrap().sha.starts_with("deadbeef"));
    }

    #[test]
    fn remove_returns_entry() {
        let mut lock = PluginLock::default();
        let pref = sample_ref();
        lock.insert(&pref, sample_entry());

        let removed = lock.remove(&pref);
        assert!(removed.is_some());
        assert!(lock.entries.is_empty());
    }

    #[test]
    fn remove_nonexistent_returns_none() {
        let mut lock = PluginLock::default();
        let pref = sample_ref();
        assert!(lock.remove(&pref).is_none());
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let lock = PluginLock::default();
        let pref = sample_ref();
        assert!(lock.get(&pref).is_none());
    }
}
