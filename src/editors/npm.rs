// Package manager detection for finding npm/yarn/bun global installations
//
// This module provides fast detection of globally installed Node.js packages
// without spawning Node.js processes (which add ~100-120ms overhead).

use crate::cache::debug_log;
use std::path::PathBuf;

/// Get the version of a globally installed npm package by reading package.json directly.
/// Avoids ~120ms Node.js startup overhead from spawning the binary with --version.
///
/// # Arguments
/// * `package_name` - The npm package name (e.g., "@anthropic-ai/claude-code")
/// * `binary_name` - The binary name for fallback resolution via `which` (e.g., "claude")
///
/// # Example
/// ```
/// let version = aiki::editors::npm::get_version("@anthropic-ai/claude-code", "claude");
/// ```
pub fn get_version(
    package_name: impl Into<String>,
    binary_name: impl Into<String>,
) -> Option<String> {
    let package_name = package_name.into();
    let binary_name = binary_name.into();

    let result = get_version_impl(&package_name, &binary_name);

    // Log failures in debug mode
    if result.is_none() {
        debug_log(|| {
            format!(
                "Could not detect version for package '{}' - falling back to None",
                package_name
            )
        });
    }

    result
}

fn get_version_impl(package_name: &str, binary_name: &str) -> Option<String> {
    // Try npm global root detection first
    if let Some(npm_root) = find_npm_global_root() {
        let package_json = npm_root.join(package_name).join("package.json");

        debug_log(|| format!("Checking package.json at: {:?}", package_json));

        if let Some(version) = read_version_from_package_json(&package_json) {
            return Some(version);
        }
    }

    // Fallback: resolve via `which` (adds ~10ms but handles edge cases)
    resolve_via_which(binary_name)
}

/// Find npm global node_modules directory without spawning a process.
/// Checks env vars, .npmrc files, NVM, Yarn, Bun, and platform defaults.
fn find_npm_global_root() -> Option<PathBuf> {
    // 1. Check env vars (free)
    if let Ok(prefix) = std::env::var("NPM_CONFIG_PREFIX") {
        return Some(npm_prefix_to_node_modules(&prefix));
    }

    // 2. Check .npmrc files for prefix setting
    let home = std::env::var("HOME").ok();
    let npmrc_paths = [
        home.as_ref().map(|h| format!("{}/.npmrc", h)),
        Some("/etc/npmrc".to_string()),
    ];

    for path in npmrc_paths.into_iter().flatten() {
        if let Some(prefix) = parse_npmrc_prefix(&path) {
            return Some(npm_prefix_to_node_modules(&prefix));
        }
    }

    // 3. Check NVM installation
    if let Some(path) = find_nvm_node_modules() {
        return Some(path);
    }

    // 4. Check Yarn Classic global
    if let Some(path) = find_yarn_classic_global() {
        return Some(path);
    }

    // 5. Check Bun global
    if let Some(path) = find_bun_global() {
        return Some(path);
    }

    // 6. Check user's npm-global directory
    if let Some(ref home) = home {
        let user_global = PathBuf::from(home).join(".npm-global/lib/node_modules");
        if user_global.exists() {
            return Some(user_global);
        }
    }

    // 7. Platform-specific defaults
    find_platform_default_root()
}

/// Parse .npmrc file for prefix setting.
/// Handles comments, whitespace, and quoted values.
fn parse_npmrc_prefix(path: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let line = line.trim();

        // Skip comments
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // Match "prefix = /path" or "prefix=/path"
        if let Some(stripped) = line.strip_prefix("prefix") {
            // Trim whitespace and equals sign
            let value = stripped.trim().trim_start_matches('=').trim();
            // Remove quotes if present
            let value = value.trim_matches('"').trim_matches('\'');
            return Some(value.to_string());
        }
    }
    None
}

/// Convert npm prefix to node_modules path, accounting for platform differences.
/// Unix: prefix → prefix/lib/node_modules
/// Windows: prefix → prefix/node_modules
fn npm_prefix_to_node_modules(prefix: &str) -> PathBuf {
    let prefix_path = PathBuf::from(prefix);

    #[cfg(target_os = "windows")]
    {
        // Windows npm prefix already points at the npm directory
        // e.g., C:\Users\User\AppData\Roaming\npm
        prefix_path.join("node_modules")
    }

    #[cfg(not(target_os = "windows"))]
    {
        // Unix npm prefix needs lib/ subdirectory
        // e.g., /usr/local → /usr/local/lib/node_modules
        prefix_path.join("lib/node_modules")
    }
}

/// Find node_modules directory for NVM installations.
/// Checks NODE_PATH env var first, then resolves NVM default alias.
fn find_nvm_node_modules() -> Option<PathBuf> {
    // Fast path: Check NODE_PATH env var (set by NVM)
    // NODE_PATH can contain multiple entries separated by : (Unix) or ; (Windows)
    if let Ok(node_path) = std::env::var("NODE_PATH") {
        // Use std::env::split_paths for cross-platform path splitting
        for path in std::env::split_paths(&node_path) {
            if path.exists() {
                return Some(path);
            }
        }
    }

    let nvm_dir = std::env::var("NVM_DIR").ok()?;

    // Use NVM's own resolution via shell integration (adds ~20ms but correct)
    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!(
            "source {}/nvm.sh 2>/dev/null && nvm which default 2>/dev/null",
            nvm_dir
        ))
        .output()
        .ok()?;

    if output.status.success() {
        let node_bin = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let node_bin_path = PathBuf::from(node_bin);

        // node_bin is like /path/to/.nvm/versions/node/v20.11.0/bin/node
        // We want /path/to/.nvm/versions/node/v20.11.0/lib/node_modules
        if let Some(version_dir) = node_bin_path.parent()?.parent() {
            let node_modules = version_dir.join("lib/node_modules");
            if node_modules.exists() {
                return Some(node_modules);
            }
        }
    }

    // Fallback: Try to parse alias/default directly
    let nvm_path = PathBuf::from(&nvm_dir);
    let default_alias = nvm_path.join("alias/default");
    let version = std::fs::read_to_string(&default_alias).ok()?;
    let version = version.trim();

    let versions_dir = nvm_path.join("versions/node");

    // If version is a direct version number (e.g., "v20.11.0")
    let direct_path = versions_dir.join(version).join("lib/node_modules");
    if direct_path.exists() {
        return Some(direct_path);
    }

    None
}

/// Find node_modules directory for Yarn Classic (v1) global installations.
/// Yarn Classic stores global packages in ~/.config/yarn/global/node_modules
fn find_yarn_classic_global() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;

    // Yarn Classic default location
    let yarn_global = PathBuf::from(&home).join(".config/yarn/global/node_modules");
    if yarn_global.exists() {
        return Some(yarn_global);
    }

    // Check for custom prefix in .yarnrc
    let yarnrc_path = PathBuf::from(&home).join(".yarnrc");
    if let Ok(content) = std::fs::read_to_string(&yarnrc_path) {
        for line in content.lines() {
            let line = line.trim();

            // Skip comments
            if line.starts_with('#') {
                continue;
            }

            // Match "global-folder" setting
            if let Some(stripped) = line.strip_prefix("global-folder") {
                let value = stripped.trim_start_matches('"').trim().trim_matches('"');
                let global_path = PathBuf::from(value).join("node_modules");
                if global_path.exists() {
                    return Some(global_path);
                }
            }
        }
    }

    None
}

/// Find node_modules directory for Bun global installations.
/// Bun stores global packages in ~/.bun/install/global/node_modules
fn find_bun_global() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;

    // Bun default location
    let bun_global = PathBuf::from(&home).join(".bun/install/global/node_modules");
    if bun_global.exists() {
        return Some(bun_global);
    }

    // Check BUN_INSTALL env var
    if let Ok(bun_install) = std::env::var("BUN_INSTALL") {
        let bun_global = PathBuf::from(bun_install).join("install/global/node_modules");
        if bun_global.exists() {
            return Some(bun_global);
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn find_platform_default_root() -> Option<PathBuf> {
    let defaults = [
        "/opt/homebrew/lib/node_modules", // Apple Silicon Homebrew
        "/usr/local/lib/node_modules",    // Intel Homebrew / default
    ];

    defaults.iter().map(PathBuf::from).find(|p| p.exists())
}

#[cfg(target_os = "linux")]
fn find_platform_default_root() -> Option<PathBuf> {
    let defaults = ["/usr/local/lib/node_modules", "/usr/lib/node_modules"];

    defaults.iter().map(PathBuf::from).find(|p| p.exists())
}

#[cfg(target_os = "windows")]
fn find_platform_default_root() -> Option<PathBuf> {
    // Check Windows AppData for npm global directory
    let appdata = std::env::var("APPDATA").ok()?;
    let npm_global = PathBuf::from(appdata).join("npm/node_modules");
    if npm_global.exists() {
        return Some(npm_global);
    }

    // Check ProgramFiles
    if let Ok(program_files) = std::env::var("ProgramFiles") {
        let nodejs = PathBuf::from(program_files).join("nodejs/node_modules");
        if nodejs.exists() {
            return Some(nodejs);
        }
    }

    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn find_platform_default_root() -> Option<PathBuf> {
    None
}

fn read_version_from_package_json(path: &PathBuf) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json["version"].as_str().map(String::from)
}

fn resolve_via_which(binary_name: &str) -> Option<String> {
    debug_log(|| format!("Falling back to binary resolution for '{}'", binary_name));

    // Use the `which` crate (already in dependencies) for cross-platform resolution
    // Handles PATH search, PATHEXT on Windows, symlinks, and permissions
    let bin_path = which::which(binary_name).ok()?;
    let resolved = std::fs::canonicalize(&bin_path).ok()?;

    // Walk up directory tree to find package.json
    let mut dir = resolved.parent()?;
    for _ in 0..5 {
        let pkg = dir.join("package.json");
        if let Some(version) = read_version_from_package_json(&pkg.to_path_buf()) {
            return Some(version);
        }
        dir = dir.parent()?;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_detection_claude_code() {
        // Test full end-to-end detection with Claude Code as example
        // This verifies that get_version() works for real packages
        // It may return None if Claude is not installed, which is fine
        let version = get_version("@anthropic-ai/claude-code", "claude");

        if let Some(v) = version {
            // If we got a version, verify it's a reasonable format
            assert!(!v.is_empty(), "Version should not be empty");
            assert!(
                v.chars().next().unwrap().is_ascii_digit(),
                "Version should start with a digit"
            );
            assert!(v.split('.').count() >= 2, "Expected semver format");
            println!("Detected Claude Code version: {}", v);
        } else {
            // If no version detected, that's okay (claude might not be installed)
            println!("Claude Code not detected (not installed or not in PATH)");
        }
    }

    #[test]
    fn test_npm_root_detection() {
        if let Some(root) = find_npm_global_root() {
            assert!(root.exists(), "Detected npm root should exist");
            println!("Detected npm global root: {:?}", root);
        }
    }

    #[test]
    fn test_npmrc_parsing_basic() {
        let temp_dir = std::env::temp_dir();
        let npmrc_path = temp_dir.join(".npmrc_test_basic");
        std::fs::write(&npmrc_path, "prefix=/custom/path\n").unwrap();

        let prefix = parse_npmrc_prefix(&npmrc_path.to_string_lossy());
        assert_eq!(prefix, Some("/custom/path".to_string()));

        std::fs::remove_file(npmrc_path).ok();
    }

    #[test]
    fn test_npmrc_parsing_with_comments() {
        let temp_dir = std::env::temp_dir();
        let npmrc_path = temp_dir.join(".npmrc_test_comments");
        std::fs::write(
            &npmrc_path,
            "# Comment\nprefix=/custom/path\n# Another comment\n",
        )
        .unwrap();

        let prefix = parse_npmrc_prefix(&npmrc_path.to_string_lossy());
        assert_eq!(prefix, Some("/custom/path".to_string()));

        std::fs::remove_file(npmrc_path).ok();
    }

    #[test]
    fn test_npmrc_parsing_with_quotes() {
        let temp_dir = std::env::temp_dir();
        let npmrc_path = temp_dir.join(".npmrc_test_quotes");
        std::fs::write(&npmrc_path, "prefix=\"/path with spaces\"\n").unwrap();

        let prefix = parse_npmrc_prefix(&npmrc_path.to_string_lossy());
        assert_eq!(prefix, Some("/path with spaces".to_string()));

        std::fs::remove_file(npmrc_path).ok();
    }

    #[test]
    fn test_npmrc_parsing_with_whitespace() {
        let temp_dir = std::env::temp_dir();
        let npmrc_path = temp_dir.join(".npmrc_test_whitespace");
        std::fs::write(&npmrc_path, "prefix = /custom/path\n").unwrap();

        let prefix = parse_npmrc_prefix(&npmrc_path.to_string_lossy());
        assert_eq!(prefix, Some("/custom/path".to_string()));

        std::fs::remove_file(npmrc_path).ok();
    }

    #[test]
    fn test_yarn_classic_detection() {
        if let Some(root) = find_yarn_classic_global() {
            assert!(
                root.exists(),
                "Detected Yarn Classic global root should exist"
            );
            println!("Detected Yarn Classic global root: {:?}", root);
        }
    }

    #[test]
    fn test_bun_detection() {
        if let Some(root) = find_bun_global() {
            assert!(root.exists(), "Detected Bun global root should exist");
            println!("Detected Bun global root: {:?}", root);
        }
    }

    #[test]
    fn test_fallback_to_which() {
        // This test verifies the which fallback works
        // Only runs if claude is in PATH but not in standard locations
        if let Some(version) = resolve_via_which("claude") {
            assert!(version.split('.').count() >= 2, "Expected semver format");
            println!("Detected via which fallback: {}", version);
        }
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn test_windows_prefix_path() {
        let prefix = r"C:\Users\Test\AppData\Roaming\npm";
        let result = npm_prefix_to_node_modules(prefix);
        assert_eq!(
            result,
            PathBuf::from(r"C:\Users\Test\AppData\Roaming\npm\node_modules")
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_unix_prefix_path() {
        let prefix = "/usr/local";
        let result = npm_prefix_to_node_modules(prefix);
        assert_eq!(result, PathBuf::from("/usr/local/lib/node_modules"));
    }

    #[test]
    fn test_node_path_multiple_entries() {
        use std::sync::Mutex;
        static TEST_MUTEX: Mutex<()> = Mutex::new(());

        // Serialize env var access to avoid race conditions
        let _guard = TEST_MUTEX.lock().unwrap();

        // Save original NODE_PATH
        let original_node_path = std::env::var("NODE_PATH").ok();

        // Set up multi-entry NODE_PATH with temp directories
        let temp_dir = std::env::temp_dir();
        let node_test1 = temp_dir.join("node_test_multi1");
        let node_test2 = temp_dir.join("node_test_multi2");
        std::fs::create_dir_all(&node_test1).unwrap();
        std::fs::create_dir_all(&node_test2).unwrap();

        // Build multi-entry NODE_PATH: nonexistent path first, then real path
        let paths = vec![
            PathBuf::from("/nonexistent_path_for_test"),
            node_test1.clone(),
        ];
        let node_path = std::env::join_paths(&paths).unwrap();
        std::env::set_var("NODE_PATH", &node_path);

        // Should find the first existing path (node_test1), skipping nonexistent
        let result = find_nvm_node_modules();
        assert_eq!(result, Some(node_test1.clone()));

        // Cleanup
        if let Some(original) = original_node_path {
            std::env::set_var("NODE_PATH", original);
        } else {
            std::env::remove_var("NODE_PATH");
        }
        std::fs::remove_dir_all(node_test1).ok();
        std::fs::remove_dir_all(node_test2).ok();
    }
}
