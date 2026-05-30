//! Git operations for plugin management (clone, pull, remove).

use std::fs;
use std::path::Path;
use std::process::Command;

use super::{check_install_status, InstallStatus, PluginRef};
use crate::error::{AikiError, Result};

/// Clone a plugin from GitHub as a shallow clone.
///
/// - If already installed (dir with `.git/`), silently skips.
/// - If partial install (dir without `.git/`), removes and re-clones.
/// - Creates parent directories as needed.
pub fn clone_plugin(plugin: &PluginRef, plugins_base: &Path) -> Result<()> {
    let install_dir = plugin.install_dir(plugins_base);

    match check_install_status(plugin, plugins_base) {
        InstallStatus::Installed => return Ok(()), // Already installed, skip
        InstallStatus::PartialInstall => {
            // Remove partial directory and re-clone
            fs::remove_dir_all(&install_dir).map_err(|e| AikiError::PluginOperationFailed {
                plugin: plugin.to_string(),
                details: format!("Failed to remove partial install: {}", e),
            })?;
        }
        InstallStatus::NotInstalled => {}
    }

    // Create parent directories
    if let Some(parent) = install_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("Failed to create directory: {}", e),
        })?;
    }

    let url = plugin.github_url();
    let output = Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            &url,
            &install_dir.display().to_string(),
        ])
        .output()
        .map_err(|e| AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("Failed to run git clone: {}", e),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("git clone failed: {}", stderr.trim()),
        });
    }

    Ok(())
}

/// Pull latest changes for an installed plugin.
///
/// Returns an error if the plugin is not installed.
pub fn pull_plugin(plugin: &PluginRef, plugins_base: &Path) -> Result<()> {
    let install_dir = plugin.install_dir(plugins_base);

    if check_install_status(plugin, plugins_base) != InstallStatus::Installed {
        return Err(AikiError::PluginNotInstalled(plugin.to_string()));
    }

    let output = Command::new("git")
        .args(["-C", &install_dir.display().to_string(), "pull"])
        .output()
        .map_err(|e| AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("Failed to run git pull: {}", e),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("git pull failed: {}", stderr.trim()),
        });
    }

    Ok(())
}

/// Remove an installed plugin.
///
/// Returns an error if the plugin is not installed.
pub fn remove_plugin(plugin: &PluginRef, plugins_base: &Path) -> Result<()> {
    let install_dir = plugin.install_dir(plugins_base);

    if check_install_status(plugin, plugins_base) != InstallStatus::Installed {
        return Err(AikiError::PluginNotInstalled(plugin.to_string()));
    }

    fs::remove_dir_all(&install_dir).map_err(|e| AikiError::PluginOperationFailed {
        plugin: plugin.to_string(),
        details: format!("Failed to remove plugin directory: {}", e),
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_clone_skips_already_installed() {
        let tmp = TempDir::new().unwrap();
        let plugin: PluginRef = "test/repo".parse().unwrap();
        let dir = plugin.install_dir(tmp.path());
        fs::create_dir_all(dir.join(".git")).unwrap();

        // Should succeed without making any network call
        let result = clone_plugin(&plugin, tmp.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_clone_removes_partial_install() {
        let tmp = TempDir::new().unwrap();
        let plugin: PluginRef = "test/repo".parse().unwrap();
        let dir = plugin.install_dir(tmp.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("partial.txt"), "leftover").unwrap();

        // Clone will fail (no network) but partial dir should be removed first
        let result = clone_plugin(&plugin, tmp.path());
        assert!(result.is_err()); // Expected: git clone fails (no real repo)

        // The partial file should be gone (dir was cleaned up before clone attempt)
        assert!(!dir.join("partial.txt").exists());
    }

    #[test]
    fn test_pull_fails_if_not_installed() {
        let tmp = TempDir::new().unwrap();
        let plugin: PluginRef = "test/repo".parse().unwrap();

        let err = pull_plugin(&plugin, tmp.path()).unwrap_err();
        assert_eq!(err.to_string(), "Plugin test/repo is not installed");
    }

    #[test]
    fn test_remove_fails_if_not_installed() {
        let tmp = TempDir::new().unwrap();
        let plugin: PluginRef = "test/repo".parse().unwrap();

        let err = remove_plugin(&plugin, tmp.path()).unwrap_err();
        assert_eq!(err.to_string(), "Plugin test/repo is not installed");
    }

    #[test]
    fn test_remove_deletes_directory() {
        let tmp = TempDir::new().unwrap();
        let plugin: PluginRef = "test/repo".parse().unwrap();
        let dir = plugin.install_dir(tmp.path());
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("hooks.yaml"), "name: test").unwrap();

        let result = remove_plugin(&plugin, tmp.path());
        assert!(result.is_ok());
        assert!(!dir.exists());
    }

    #[test]
    fn test_remove_partial_install_errors() {
        let tmp = TempDir::new().unwrap();
        let plugin: PluginRef = "test/repo".parse().unwrap();
        let dir = plugin.install_dir(tmp.path());
        fs::create_dir_all(&dir).unwrap(); // No .git/

        let err = remove_plugin(&plugin, tmp.path()).unwrap_err();
        assert_eq!(err.to_string(), "Plugin test/repo is not installed");
    }
}
