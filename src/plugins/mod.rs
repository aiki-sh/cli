//! Plugin management for remote GitHub-based plugins.
//!
//! Plugins are installed as shallow clones into `~/.aiki/plugins/` and resolved
//! at runtime alongside project-level templates.

pub mod deps;
pub mod git;
pub mod graph;
pub mod lock;
pub mod manifest;
pub mod project;
pub mod scanner;

pub use deps::install;

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use std::fs;

use crate::error::{AikiError, Result};
use crate::plugins::manifest::resolve_display_name;

/// Reserved namespace that maps to the `glasner` GitHub owner.
const AIKI_NAMESPACE: &str = "aiki";
const AIKI_GITHUB_OWNER: &str = "glasner";

/// A validated plugin reference in `namespace/name` format.
///
/// The namespace maps to a GitHub owner (with `aiki` aliased to `glasner`).
/// The name maps to a GitHub repository.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginRef {
    pub namespace: String,
    pub name: String,
}

impl PluginRef {
    /// Returns the GitHub HTTPS clone URL for this plugin.
    ///
    /// The `aiki` namespace is aliased to the `glasner` GitHub owner.
    #[must_use]
    pub fn github_url(&self) -> String {
        let owner = if self.namespace == AIKI_NAMESPACE {
            AIKI_GITHUB_OWNER
        } else {
            &self.namespace
        };
        format!("https://github.com/{}/{}.git", owner, self.name)
    }

    /// Returns the installation directory for this plugin under the given base.
    ///
    /// Always uses the original namespace (not the resolved GitHub owner).
    #[must_use]
    pub fn install_dir(&self, plugins_base: &Path) -> PathBuf {
        plugins_base.join(&self.namespace).join(&self.name)
    }

    /// Returns a human-readable display name for this plugin.
    ///
    /// Returns the name from plugin.yaml or hooks.yaml if one exists,
    /// otherwise falls back to the `namespace/name` path.
    // Note: Reads plugin.yaml from disk. Prefer `PluginGraph::display_name()`
    // when a graph is already built (e.g. in `plugin list`).
    #[must_use]
    pub fn display_name(&self, plugins_base: &Path) -> String {
        let install_dir = self.install_dir(plugins_base);
        let plugin_path = self.to_string();
        resolve_display_name(&install_dir, &plugin_path)
    }
}

impl fmt::Display for PluginRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.namespace, self.name)
    }
}

impl FromStr for PluginRef {
    type Err = AikiError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('/').collect();

        if parts.len() != 2 {
            return Err(AikiError::InvalidPluginRef {
                reference: s.to_string(),
                reason: "Plugin reference must be in 'namespace/name' format".to_string(),
            });
        }

        let namespace = parts[0];
        let name = parts[1];

        if namespace.is_empty() || name.is_empty() {
            return Err(AikiError::InvalidPluginRef {
                reference: s.to_string(),
                reason: "Neither namespace nor name can be empty".to_string(),
            });
        }

        // Reject explicit hosts (first segment contains a dot)
        if namespace.contains('.') {
            return Err(AikiError::InvalidPluginRef {
                reference: s.to_string(),
                reason: "Only GitHub plugins are supported".to_string(),
            });
        }

        // Validate characters: alphanumeric, hyphens, underscores
        let is_valid_segment = |s: &str| {
            s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        };

        if !is_valid_segment(namespace) {
            return Err(AikiError::InvalidPluginRef {
                reference: s.to_string(),
                reason: format!(
                    "Namespace '{}' contains invalid characters (use alphanumeric, hyphens, underscores)",
                    namespace
                ),
            });
        }

        if !is_valid_segment(name) {
            return Err(AikiError::InvalidPluginRef {
                reference: s.to_string(),
                reason: format!(
                    "Name '{}' contains invalid characters (use alphanumeric, hyphens, underscores)",
                    name
                ),
            });
        }

        Ok(PluginRef {
            namespace: namespace.to_string(),
            name: name.to_string(),
        })
    }
}

/// Returns the base directory for installed plugins (`~/.aiki/plugins/`).
///
/// Respects `AIKI_HOME` — when set, plugins are stored under `$AIKI_HOME/plugins/`.
pub fn plugins_base_dir() -> Result<PathBuf> {
    Ok(crate::global::global_aiki_dir().join("plugins"))
}

/// Installation status of a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStatus {
    /// Plugin is fully installed (directory exists with `.git/`).
    Installed,
    /// Directory exists but no `.git/` (interrupted clone).
    PartialInstall,
    /// Plugin is not installed.
    NotInstalled,
}

/// Check the installation status of a plugin.
pub fn check_install_status(plugin: &PluginRef, plugins_base: &Path) -> InstallStatus {
    let dir = plugin.install_dir(plugins_base);
    if dir.join(".git").is_dir() {
        InstallStatus::Installed
    } else if dir.is_dir() {
        InstallStatus::PartialInstall
    } else {
        InstallStatus::NotInstalled
    }
}

// ---------------------------------------------------------------------------
// Fetch-failure markers
//
// When auto-fetch fails, a marker file is written to
// `~/.aiki/plugins/.fetch-failed/{ns}/{name}` containing the error reason.
// Subsequent process invocations (each event fires a separate process) check
// the marker before hitting the network, preventing repeated clone attempts
// and warning spam within a session.
// ---------------------------------------------------------------------------

const FETCH_FAILED_DIR: &str = ".fetch-failed";

/// Path to a fetch-failure marker for `plugin` under `plugins_base`.
fn fetch_failed_path(plugin: &PluginRef, plugins_base: &Path) -> PathBuf {
    plugins_base
        .join(FETCH_FAILED_DIR)
        .join(&plugin.namespace)
        .join(&plugin.name)
}

/// If `plugin` has a persisted fetch-failure marker, return the reason string.
pub fn check_fetch_failed(plugin: &PluginRef, plugins_base: &Path) -> Option<String> {
    let path = fetch_failed_path(plugin, plugins_base);
    fs::read_to_string(path).ok()
}

/// Record that auto-fetch for `plugin` failed with `reason`.
pub fn mark_fetch_failed(plugin: &PluginRef, plugins_base: &Path, reason: &str) {
    let path = fetch_failed_path(plugin, plugins_base);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, reason);
}

/// Remove the fetch-failure marker for `plugin` (e.g. before an explicit install).
pub fn clear_fetch_failed(plugin: &PluginRef, plugins_base: &Path) {
    let path = fetch_failed_path(plugin, plugins_base);
    let _ = fs::remove_file(path);
}

/// Remove all fetch-failure markers (called on session start).
pub fn clear_all_fetch_failed(plugins_base: &Path) {
    let dir = plugins_base.join(FETCH_FAILED_DIR);
    let _ = fs::remove_dir_all(dir);
}

/// List all installed plugins (directories with `.git/`) under `plugins_base`.
pub fn list_installed_plugins(plugins_base: &Path) -> Vec<PluginRef> {
    let mut plugins = Vec::new();

    if !plugins_base.is_dir() {
        return plugins;
    }

    let ns_entries = match fs::read_dir(plugins_base) {
        Ok(e) => e,
        Err(_) => return plugins,
    };

    for ns_entry in ns_entries.flatten() {
        let ns_path = ns_entry.path();
        if !ns_path.is_dir() {
            continue;
        }

        let namespace = match ns_path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        let name_entries = match fs::read_dir(&ns_path) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for name_entry in name_entries.flatten() {
            let name_path = name_entry.path();
            if !name_path.is_dir() {
                continue;
            }

            let name = match name_path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            if name_path.join(".git").is_dir() {
                if let Ok(plugin) = format!("{}/{}", namespace, name).parse::<PluginRef>() {
                    plugins.push(plugin);
                }
            }
        }
    }

    plugins.sort_by(|a, b| a.to_string().cmp(&b.to_string()));
    plugins
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_valid_refs() {
        let r: PluginRef = "aiki/way".parse().unwrap();
        assert_eq!(r.namespace, "aiki");
        assert_eq!(r.name, "way");

        let r: PluginRef = "somecorp/security".parse().unwrap();
        assert_eq!(r.namespace, "somecorp");
        assert_eq!(r.name, "security");

        let r: PluginRef = "my-org/my_plugin".parse().unwrap();
        assert_eq!(r.namespace, "my-org");
        assert_eq!(r.name, "my_plugin");
    }

    #[test]
    fn test_reject_empty() {
        assert!("".parse::<PluginRef>().is_err());
    }

    #[test]
    fn test_reject_single_segment() {
        let err = "onlyone".parse::<PluginRef>().unwrap_err();
        assert!(err.to_string().contains("namespace/name"));
    }

    #[test]
    fn test_reject_explicit_host() {
        // Two-segment with dot in namespace
        let err = "github.com/repo".parse::<PluginRef>().unwrap_err();
        assert!(err
            .to_string()
            .contains("Only GitHub plugins are supported"));

        // Three-segment also rejected (wrong format)
        assert!("github.com/user/repo".parse::<PluginRef>().is_err());
    }

    #[test]
    fn test_reject_three_segments() {
        let err = "a/b/c".parse::<PluginRef>().unwrap_err();
        assert!(err.to_string().contains("namespace/name"));
    }

    #[test]
    fn test_reject_empty_parts() {
        assert!("/name".parse::<PluginRef>().is_err());
        assert!("ns/".parse::<PluginRef>().is_err());
    }

    #[test]
    fn test_github_url_aiki_namespace() {
        let r: PluginRef = "aiki/way".parse().unwrap();
        assert_eq!(r.github_url(), "https://github.com/glasner/way.git");
    }

    #[test]
    fn test_github_url_other_namespace() {
        let r: PluginRef = "somecorp/security".parse().unwrap();
        assert_eq!(r.github_url(), "https://github.com/somecorp/security.git");
    }

    #[test]
    fn test_install_dir() {
        let r: PluginRef = "aiki/way".parse().unwrap();
        let base = Path::new("/home/user/.aiki/plugins");
        assert_eq!(
            r.install_dir(base),
            PathBuf::from("/home/user/.aiki/plugins/aiki/way")
        );
    }

    #[test]
    fn test_display() {
        let r: PluginRef = "aiki/way".parse().unwrap();
        assert_eq!(r.to_string(), "aiki/way");
    }

    #[test]
    fn test_check_install_status_not_installed() {
        let tmp = TempDir::new().unwrap();
        let r: PluginRef = "aiki/way".parse().unwrap();
        assert_eq!(
            check_install_status(&r, tmp.path()),
            InstallStatus::NotInstalled
        );
    }

    #[test]
    fn test_check_install_status_installed() {
        let tmp = TempDir::new().unwrap();
        let r: PluginRef = "aiki/way".parse().unwrap();
        let dir = r.install_dir(tmp.path());
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(
            check_install_status(&r, tmp.path()),
            InstallStatus::Installed
        );
    }

    #[test]
    fn test_check_install_status_partial() {
        let tmp = TempDir::new().unwrap();
        let r: PluginRef = "aiki/way".parse().unwrap();
        let dir = r.install_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        // Dir exists but no .git/
        assert_eq!(
            check_install_status(&r, tmp.path()),
            InstallStatus::PartialInstall
        );
    }

    #[test]
    fn test_display_name_returns_some_when_manifest_has_name() {
        let tmp = TempDir::new().unwrap();
        let r: PluginRef = "aiki/way".parse().unwrap();
        let dir = r.install_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.yaml"), "name: The Way\n").unwrap();

        assert_eq!(r.display_name(tmp.path()), "The Way");
    }

    #[test]
    fn test_display_name_falls_back_to_path() {
        let tmp = TempDir::new().unwrap();
        let r: PluginRef = "aiki/way".parse().unwrap();
        // No plugin dir, no manifest — falls back to namespace/name
        assert_eq!(r.display_name(tmp.path()), "aiki/way");
    }

    #[test]
    fn test_fetch_failed_marker_round_trip() {
        let tmp = TempDir::new().unwrap();
        let r: PluginRef = "aiki/broken".parse().unwrap();

        // Initially no marker
        assert!(check_fetch_failed(&r, tmp.path()).is_none());

        // Write marker
        mark_fetch_failed(&r, tmp.path(), "repository not found");
        assert_eq!(
            check_fetch_failed(&r, tmp.path()).as_deref(),
            Some("repository not found")
        );

        // Clear individual marker
        clear_fetch_failed(&r, tmp.path());
        assert!(check_fetch_failed(&r, tmp.path()).is_none());
    }

    #[test]
    fn test_clear_all_fetch_failed() {
        let tmp = TempDir::new().unwrap();
        let a: PluginRef = "aiki/one".parse().unwrap();
        let b: PluginRef = "other/two".parse().unwrap();

        mark_fetch_failed(&a, tmp.path(), "reason a");
        mark_fetch_failed(&b, tmp.path(), "reason b");
        assert!(check_fetch_failed(&a, tmp.path()).is_some());
        assert!(check_fetch_failed(&b, tmp.path()).is_some());

        clear_all_fetch_failed(tmp.path());
        assert!(check_fetch_failed(&a, tmp.path()).is_none());
        assert!(check_fetch_failed(&b, tmp.path()).is_none());
    }
}
