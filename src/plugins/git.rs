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

/// Clone a plugin at a specific locked SHA (shallow fetch of exactly that commit).
///
/// - If already installed (dir with `.git/`), silently skips.
/// - If partial install (dir without `.git/`), removes and re-clones.
/// - Creates parent directories as needed.
pub fn clone_locked_plugin(plugin: &PluginRef, plugins_base: &Path, sha: &str) -> Result<()> {
    let install_dir = plugin.install_dir(plugins_base);

    match check_install_status(plugin, plugins_base) {
        InstallStatus::Installed => return Ok(()),
        InstallStatus::PartialInstall => {
            fs::remove_dir_all(&install_dir).map_err(|e| AikiError::PluginOperationFailed {
                plugin: plugin.to_string(),
                details: format!("Failed to remove partial install: {}", e),
            })?;
        }
        InstallStatus::NotInstalled => {}
    }

    if let Some(parent) = install_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("Failed to create directory: {}", e),
        })?;
    }

    let dir_str = install_dir.display().to_string();
    let url = plugin.github_url();

    // Initialize a bare repo, add remote, fetch exactly the locked SHA
    let init_output = Command::new("git")
        .args(["init", &dir_str])
        .output()
        .map_err(|e| AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("Failed to run git init: {}", e),
        })?;

    if !init_output.status.success() {
        let stderr = String::from_utf8_lossy(&init_output.stderr);
        return Err(AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("git init failed: {}", stderr.trim()),
        });
    }

    let remote_output = Command::new("git")
        .args(["-C", &dir_str, "remote", "add", "origin", &url])
        .output()
        .map_err(|e| AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("Failed to run git remote add: {}", e),
        })?;

    if !remote_output.status.success() {
        let stderr = String::from_utf8_lossy(&remote_output.stderr);
        return Err(AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("git remote add failed: {}", stderr.trim()),
        });
    }

    let fetch_output = Command::new("git")
        .args(["-C", &dir_str, "fetch", "--depth", "1", "origin", sha])
        .output()
        .map_err(|e| AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("Failed to run git fetch: {}", e),
        })?;

    if !fetch_output.status.success() {
        // Clean up the partially initialized directory
        let _ = fs::remove_dir_all(&install_dir);
        return Err(AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!(
                "{}: locked commit {} not found. Run `aiki plugin update {}` to re-resolve.",
                plugin, sha, plugin
            ),
        });
    }

    let checkout_output = Command::new("git")
        .args(["-C", &dir_str, "checkout", "FETCH_HEAD"])
        .output()
        .map_err(|e| AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("Failed to run git checkout: {}", e),
        })?;

    if !checkout_output.status.success() {
        let stderr = String::from_utf8_lossy(&checkout_output.stderr);
        return Err(AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("git checkout FETCH_HEAD failed: {}", stderr.trim()),
        });
    }

    Ok(())
}

/// Get the HEAD SHA of an installed plugin.
pub fn get_head_sha(plugin: &PluginRef, plugins_base: &Path) -> Result<String> {
    let install_dir = plugin.install_dir(plugins_base);
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&install_dir)
        .output()
        .map_err(|e| AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("Failed to run git rev-parse HEAD: {}", e),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("git rev-parse HEAD failed: {}", stderr.trim()),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Resolve the remote HEAD SHA for a plugin without cloning.
///
/// Runs `git ls-remote <clone_url> HEAD` and parses the SHA from the output.
pub fn resolve_remote_head(plugin: &PluginRef) -> Result<String> {
    let url = plugin.github_url();
    let output = Command::new("git")
        .args(["ls-remote", &url, "HEAD"])
        .output()
        .map_err(|e| AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("Failed to run git ls-remote: {}", e),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: format!("git ls-remote failed: {}", stderr.trim()),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let sha = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| AikiError::PluginOperationFailed {
            plugin: plugin.to_string(),
            details: "git ls-remote returned empty output".to_string(),
        })?
        .to_string();

    Ok(sha)
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

    // Clean up empty parent (namespace) directory
    if let Some(parent) = install_dir.parent() {
        if parent != plugins_base {
            let _ = fs::remove_dir(parent); // Only removes if empty, ignore errors
        }
    }

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

    #[test]
    fn test_remove_cleans_up_empty_parent_dir() {
        let tmp = TempDir::new().unwrap();
        let plugin: PluginRef = "test/repo".parse().unwrap();
        let dir = plugin.install_dir(tmp.path());
        fs::create_dir_all(dir.join(".git")).unwrap();

        let ns_dir = dir.parent().unwrap();
        let result = remove_plugin(&plugin, tmp.path());
        assert!(result.is_ok());
        assert!(!dir.exists());
        // Namespace dir should be removed since it's now empty
        assert!(!ns_dir.exists());
    }

    #[test]
    fn test_clone_locked_skips_already_installed() {
        let tmp = TempDir::new().unwrap();
        let plugin: PluginRef = "test/repo".parse().unwrap();
        let dir = plugin.install_dir(tmp.path());
        fs::create_dir_all(dir.join(".git")).unwrap();

        // Should succeed without making any network call
        let result = clone_locked_plugin(&plugin, tmp.path(), "abc123");
        assert!(result.is_ok());
    }

    #[test]
    fn test_clone_locked_removes_partial_install() {
        let tmp = TempDir::new().unwrap();
        let plugin: PluginRef = "test/repo".parse().unwrap();
        let dir = plugin.install_dir(tmp.path());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("partial.txt"), "leftover").unwrap();

        // Will fail (no real remote) but partial dir should be cleaned up
        let result = clone_locked_plugin(&plugin, tmp.path(), "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
        assert!(result.is_err());
        assert!(!dir.join("partial.txt").exists());
    }

    #[test]
    fn test_clone_locked_bad_sha_reports_error() {
        let tmp = TempDir::new().unwrap();
        let plugin: PluginRef = "test/repo".parse().unwrap();

        let result = clone_locked_plugin(&plugin, tmp.path(), "not_a_real_sha");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("test/repo"),
            "error should mention the plugin name"
        );
    }

    #[test]
    fn test_remove_keeps_parent_dir_with_siblings() {
        let tmp = TempDir::new().unwrap();
        let plugin: PluginRef = "test/repo".parse().unwrap();
        let sibling: PluginRef = "test/other".parse().unwrap();

        let dir = plugin.install_dir(tmp.path());
        let sibling_dir = sibling.install_dir(tmp.path());
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::create_dir_all(sibling_dir.join(".git")).unwrap();

        let ns_dir = dir.parent().unwrap().to_path_buf();
        let result = remove_plugin(&plugin, tmp.path());
        assert!(result.is_ok());
        assert!(!dir.exists());
        // Namespace dir should still exist because sibling plugin is there
        assert!(ns_dir.exists());
    }
}
