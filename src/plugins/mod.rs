//! Plugin management for remote GitHub-based plugins.
//!
//! Plugins are installed as shallow clones into `~/.aiki/plugins/` and resolved
//! at runtime alongside project-level templates.

pub mod deps;
pub mod git;
pub mod project;
pub mod scanner;

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::error::{AikiError, Result};

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
        assert!(err.to_string().contains("Only GitHub plugins are supported"));

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
        assert_eq!(
            r.github_url(),
            "https://github.com/somecorp/security.git"
        );
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
}
