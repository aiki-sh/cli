//! High-level flow resolver (uses PathResolver + flow-specific logic).
//!
//! This module handles resolution of flow paths with namespacing:
//! - `{namespace}/*` - Namespaced flows (e.g., `aiki/*`, `eslint/*`, `prettier/*`)
//!   - Searches project `.aiki/hooks/{namespace}/` first, then `$AIKI_HOME/hooks/{namespace}/`,
//!     then installed plugins at `$AIKI_HOME/plugins/{namespace}/`
//!
//! All top-level directories in `.aiki/hooks/` are treated as namespaces.
//!
//! # Canonicalization
//!
//! All paths are canonicalized before being returned. This ensures that cycle
//! detection works correctly regardless of how a flow is referenced:
//!
//! ```text
//! ./flow-a.yml         → /project/.aiki/hooks/flow-a.yml
//! ../flows/flow-a.yml  → /project/.aiki/hooks/flow-a.yml
//! @/.aiki/hooks/flow-a.yml → /project/.aiki/hooks/flow-a.yml
//! ```

use std::path::{Path, PathBuf};

use super::path_resolver::PathResolver;
use crate::error::{AikiError, Result};
use crate::plugins::{self, PluginRef};

/// High-level flow resolver (uses PathResolver + flow-specific logic).
///
/// Resolves flow paths to canonical absolute paths. Supports:
/// - `{namespace}/{name}` - Namespaced flows (e.g., `aiki/*`, `eslint/*`, `prettier/*`)
///   - All top-level directories in `.aiki/hooks/` are treated as namespaces
///   - `aiki` is just another namespace, not a special case
///   - Searches: project hooks, user hooks, installed plugins
///
/// # Example
///
/// ```rust,ignore
/// use aiki::flows::hook_resolver::HookResolver;
///
/// let resolver = HookResolver::new()?;
///
/// // Resolve aiki namespaced flow (tries project, then user)
/// let path = resolver.resolve("aiki/quick-lint")?;
///
/// // Resolve eslint namespaced flow
/// let path = resolver.resolve("eslint/check-rules")?;
/// ```
#[derive(Debug, Clone)]
pub struct HookResolver {
    path_resolver: PathResolver,
}

impl HookResolver {
    /// Create a new HookResolver starting search from a specific directory.
    ///
    /// This is useful for testing or when you need to resolve paths from
    /// a directory other than the current working directory.
    ///
    /// # Errors
    ///
    /// Returns `AikiError::NotInAikiProject` if no `.aiki/` directory is found.
    pub fn with_start_dir(start_dir: &Path) -> Result<Self> {
        Ok(Self {
            path_resolver: PathResolver::with_start_dir(start_dir)?,
        })
    }

    /// Get the discovered project root directory.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        self.path_resolver.project_root()
    }

    /// Resolve a flow path to an absolute, canonical PathBuf.
    ///
    /// Only supports namespaced flows in the format `{namespace}/{name}`.
    /// Adds .yml extension and searches namespace directories (project first, then user).
    ///
    /// **IMPORTANT**: Returns canonicalized path for reliable cycle detection.
    ///
    /// # Arguments
    ///
    /// * `path` - The namespaced flow path (e.g., "aiki/quick-lint", "eslint/check-rules")
    ///
    /// # Supported Format
    ///
    /// | Format | Search Order |
    /// |--------|--------------|
    /// | `{namespace}/{name}` | 1. Project `.aiki/hooks/{namespace}/{name}.yml`<br>2. User `$AIKI_HOME/hooks/{namespace}/{name}.yml`<br>3. Installed `$AIKI_HOME/plugins/{namespace}/{name}/hooks.yaml`<br>4. Auto-fetch from GitHub, then re-check installed path |
    ///
    /// All top-level directories in `.aiki/hooks/` are treated as namespaces.
    /// Examples: `aiki/quick-lint`, `eslint/check-rules`, `prettier/format`, `mycompany/workflows`
    ///
    /// # Errors
    ///
    /// Returns:
    /// - `AikiError::InvalidHookPath` if path is empty or not in `{namespace}/{name}` format
    /// - `AikiError::HookNotFound` if the resolved file doesn't exist
    pub fn resolve(&self, path: &str) -> Result<PathBuf> {
        if path.is_empty() {
            return Err(AikiError::InvalidHookPath {
                path: path.to_string(),
                reason: "Path cannot be empty".to_string(),
            });
        }

        // Only support namespaced flows: {namespace}/{name}
        // Extract namespace and name
        let parts: Vec<&str> = path.splitn(2, '/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(AikiError::InvalidHookPath {
                path: path.to_string(),
                reason:
                    "Flow path must be in format {namespace}/{name} with non-empty namespace and name"
                        .to_string(),
            });
        }

        let namespace = parts[0];
        let name = parts[1];
        let resolved = self.resolve_namespaced_flow(namespace, name)?;

        // CRITICAL: Canonicalize path for reliable cycle detection
        // This ensures consistent paths regardless of symlinks or path variations
        resolved
            .canonicalize()
            .map_err(|e| AikiError::HookNotFound {
                path: path.to_string(),
                resolved_path: resolved.display().to_string(),
                source: e,
            })
    }

    /// Resolve a namespaced flow path.
    ///
    /// All top-level directories in `.aiki/hooks/` are treated as namespaces.
    /// Examples: `aiki`, `eslint`, `prettier`, `typescript`, `mycompany`
    ///
    /// Search order:
    /// 1. `{project}/.aiki/hooks/{namespace}/{name}.yml`
    /// 2. `$AIKI_HOME/hooks/{namespace}/{name}.yml`
    /// 3. `$AIKI_HOME/plugins/{namespace}/{name}/hooks.yaml`
    /// 4. Auto-fetch from GitHub, then re-check installed path
    ///
    /// # Arguments
    ///
    /// * `namespace` - The namespace (e.g., "aiki", "eslint", "prettier")
    /// * `name` - The flow name within the namespace (may contain slashes for nesting)
    fn resolve_namespaced_flow(&self, namespace: &str, name: &str) -> Result<PathBuf> {
        // 1. Try project first: {project}/.aiki/hooks/{ns}/{name}.yml
        let project_path = self
            .path_resolver
            .project_root()
            .join(".aiki/hooks")
            .join(namespace)
            .join(name);
        let project_path = Self::add_yml_extension(&project_path);

        if project_path.exists() {
            return Ok(project_path);
        }

        // 2. Fall back to user: $AIKI_HOME/hooks/{ns}/{name}.yml
        let user_path = crate::global::global_aiki_dir()
            .join("hooks")
            .join(namespace)
            .join(name);
        let user_path = Self::add_yml_extension(&user_path);

        if user_path.exists() {
            return Ok(user_path);
        }

        // 3. Installed plugin: $AIKI_HOME/plugins/{ns}/{name}/hooks.yaml
        let installed_plugin_path = crate::global::global_aiki_dir()
            .join("plugins")
            .join(namespace)
            .join(name)
            .join("hooks.yaml");

        if installed_plugin_path.exists() {
            return Ok(installed_plugin_path);
        }

        // 4. Auto-fetch: try to install the plugin from GitHub, then re-check
        let plugin_ref: PluginRef = format!("{namespace}/{name}").parse()?;
        let plugins_base = plugins::plugins_base_dir()?;
        let plugin_name = format!("{namespace}/{name}");

        // 4a. Check for a persisted fetch-failure marker (written by a prior
        //     event in this session). Avoids repeated network calls and warning
        //     spam across events.
        if let Some(reason) = plugins::check_fetch_failed(&plugin_ref, &plugins_base) {
            return Err(AikiError::AutoFetchCached {
                plugin: plugin_name,
                reason,
            });
        }

        match plugins::install(&plugin_ref, &plugins_base, Some(self.project_root()), None) {
            Err(e) => {
                let reason = e.to_string();
                plugins::mark_fetch_failed(&plugin_ref, &plugins_base, &reason);
                return Err(AikiError::AutoFetchFailed {
                    plugin: plugin_name,
                    reason,
                });
            }
            Ok(ref report) if !report.failed.is_empty() => {
                let (failed_ref, failed_msg) = &report.failed[0];
                let reason = if failed_ref.to_string() == plugin_name {
                    failed_msg.clone()
                } else {
                    format!(
                        "transitive dependency {} could not be fetched ({})",
                        failed_ref, failed_msg
                    )
                };
                plugins::mark_fetch_failed(&plugin_ref, &plugins_base, &reason);
                return Err(AikiError::AutoFetchFailed {
                    plugin: plugin_name,
                    reason,
                });
            }
            Ok(_) => {}
        }

        let installed_path = plugins_base
            .join(namespace)
            .join(name)
            .join("hooks.yaml");

        if installed_path.exists() {
            return Ok(installed_path);
        }

        let reason = "auto-fetch succeeded but hooks.yaml missing".to_string();
        plugins::mark_fetch_failed(&plugin_ref, &plugins_base, &reason);
        Err(AikiError::AutoFetchFailed {
            plugin: plugin_name,
            reason,
        })
    }

    /// Add .yml extension if not already present.
    fn add_yml_extension(path: &Path) -> PathBuf {
        if path
            .extension()
            .map_or(true, |ext| ext != "yml" && ext != "yaml")
        {
            path.with_extension("yml")
        } else {
            path.to_path_buf()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a test project with .aiki/ directory structure
    fn create_test_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        // Create namespaces - aiki is just another namespace
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/aiki")).unwrap();
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/eslint")).unwrap();
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/prettier")).unwrap();
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/helpers")).unwrap();
        fs::create_dir_all(temp_dir.path().join("docs")).unwrap();
        temp_dir
    }

    /// Create a flow file with minimal content
    fn create_flow_file(path: &Path, name: &str) {
        let content = format!(
            r#"name: {name}
version: "1"
"#
        );
        fs::write(path, content).unwrap();
    }

    #[test]
    fn test_resolve_aiki_flow_project_first() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/quick-lint.yml");
        create_flow_file(&flow_path, "Quick Lint");

        let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();
        let resolved = resolver.resolve("aiki/quick-lint").unwrap();

        // Should resolve to project flow
        assert_eq!(
            resolved.canonicalize().unwrap(),
            flow_path.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_resolve_aiki_flow_adds_yml_extension() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/quick-lint.yml");
        create_flow_file(&flow_path, "Quick Lint");

        let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();

        // Should work without .yml extension
        let resolved = resolver.resolve("aiki/quick-lint").unwrap();
        assert!(resolved.to_string_lossy().ends_with(".yml"));
    }

    #[test]
    fn test_resolve_eslint_namespaced_flow() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/eslint/check-rules.yml");
        create_flow_file(&flow_path, "ESLint Check");

        let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();
        let resolved = resolver.resolve("eslint/check-rules").unwrap();

        assert_eq!(
            resolved.canonicalize().unwrap(),
            flow_path.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_resolve_prettier_namespaced_flow() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/prettier/format.yml");
        create_flow_file(&flow_path, "Prettier Format");

        let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();
        let resolved = resolver.resolve("prettier/format").unwrap();

        assert_eq!(
            resolved.canonicalize().unwrap(),
            flow_path.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_resolve_empty_path_error() {
        let temp_dir = create_test_project();
        let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();

        let result = resolver.resolve("");
        assert!(matches!(result, Err(AikiError::InvalidHookPath { .. })));
    }

    #[test]
    fn test_resolve_invalid_format_error() {
        let temp_dir = create_test_project();
        let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();

        // Path without slash is invalid (must be {namespace}/{name})
        let result = resolver.resolve("invalid-path-no-slash");
        assert!(matches!(result, Err(AikiError::InvalidHookPath { .. })));
    }

    #[test]
    fn test_resolve_flow_not_found_error() {
        let temp_dir = create_test_project();
        let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();

        let result = resolver.resolve("aiki/nonexistent");
        assert!(matches!(
            result,
            Err(AikiError::AutoFetchFailed { .. })
                | Err(AikiError::AutoFetchCached { .. })
                | Err(AikiError::HookNotFound { .. })
        ));
    }

    #[test]
    fn test_nested_aiki_flow_path() {
        let temp_dir = create_test_project();
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/aiki/checks")).unwrap();
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/checks/lint.yml");
        create_flow_file(&flow_path, "Nested Lint");

        let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();
        let resolved = resolver.resolve("aiki/checks/lint").unwrap();

        assert_eq!(
            resolved.canonicalize().unwrap(),
            flow_path.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_flow_with_yml_extension_not_doubled() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/test.yml");
        create_flow_file(&flow_path, "Test Flow");

        let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();

        // Should work with explicit .yml extension
        let resolved = resolver.resolve("aiki/test.yml").unwrap();

        // Should not have doubled extension like .yml.yml
        assert!(!resolved.to_string_lossy().ends_with(".yml.yml"));
        assert!(resolved.to_string_lossy().ends_with(".yml"));
    }

    #[test]
    fn test_flow_with_yaml_extension() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/test.yaml");
        create_flow_file(&flow_path, "Test Flow");

        let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();

        // Should work with .yaml extension
        let resolved = resolver.resolve("aiki/test.yaml").unwrap();

        assert!(resolved.to_string_lossy().ends_with(".yaml"));
    }

    // -----------------------------------------------------------------------
    // Auto-fetch tests
    // -----------------------------------------------------------------------

    /// Mutex + env override for AIKI_HOME (serializes tests that modify env).
    static AIKI_HOME_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_temp_aiki_home<F, R>(aiki_home: &Path, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        struct RestoreEnv(Option<String>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(v) => std::env::set_var("AIKI_HOME", v),
                    None => std::env::remove_var("AIKI_HOME"),
                }
            }
        }

        let _lock = AIKI_HOME_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = RestoreEnv(std::env::var("AIKI_HOME").ok());
        std::env::set_var("AIKI_HOME", aiki_home);
        f()
    }

    #[test]
    fn test_auto_fetch_triggered_for_uninstalled_plugin() {
        let temp_dir = create_test_project();
        let aiki_home = TempDir::new().unwrap();

        // No plugin installed, auto-fetch will be attempted but fail (no network)
        let result = with_temp_aiki_home(aiki_home.path(), || {
            let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();
            resolver.resolve("fake/nonexistent")
        });

        assert!(
            matches!(result, Err(AikiError::AutoFetchFailed { .. })),
            "Uninstalled plugin should trigger auto-fetch and return AutoFetchFailed, got: {:?}",
            result
        );
    }

    #[test]
    fn test_auto_fetch_error_includes_plugin_name() {
        let temp_dir = create_test_project();
        let aiki_home = TempDir::new().unwrap();

        let result = with_temp_aiki_home(aiki_home.path(), || {
            let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();
            resolver.resolve("myorg/myplugin")
        });

        match result {
            Err(AikiError::AutoFetchFailed { plugin, .. }) => {
                assert_eq!(plugin, "myorg/myplugin");
            }
            other => panic!(
                "Expected AutoFetchFailed with plugin name, got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_installed_plugin_hooks_yaml_found() {
        let temp_dir = create_test_project();
        let aiki_home = TempDir::new().unwrap();

        // Install a fake plugin with hooks.yaml under AIKI_HOME
        let plugin_dir = aiki_home
            .path()
            .join("plugins")
            .join("testns")
            .join("testplug");
        fs::create_dir_all(plugin_dir.join(".git")).unwrap();
        let hooks_path = plugin_dir.join("hooks.yaml");
        fs::write(&hooks_path, "name: Test\nversion: '1'\n").unwrap();

        let result = with_temp_aiki_home(aiki_home.path(), || {
            let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();
            resolver.resolve("testns/testplug")
        });

        assert!(
            result.is_ok(),
            "Should resolve to installed plugin hooks.yaml, got: {:?}",
            result
        );
        let resolved = result.unwrap();
        assert!(
            resolved.to_string_lossy().contains("hooks.yaml"),
            "Resolved path should point to hooks.yaml: {}",
            resolved.display()
        );
    }

    #[test]
    fn test_project_override_takes_priority_over_installed_plugin() {
        let temp_dir = create_test_project();
        let aiki_home = TempDir::new().unwrap();

        // Create project-level hook
        let project_hook = temp_dir.path().join(".aiki/hooks/testns/testplug.yml");
        fs::create_dir_all(project_hook.parent().unwrap()).unwrap();
        create_flow_file(&project_hook, "Project Override");

        // Also install the plugin
        let plugin_dir = aiki_home
            .path()
            .join("plugins")
            .join("testns")
            .join("testplug");
        fs::create_dir_all(plugin_dir.join(".git")).unwrap();
        fs::write(plugin_dir.join("hooks.yaml"), "name: Plugin\nversion: '1'\n").unwrap();

        let result = with_temp_aiki_home(aiki_home.path(), || {
            let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();
            resolver.resolve("testns/testplug")
        });

        assert!(result.is_ok());
        let resolved = result.unwrap();
        // Should resolve to project hook, not the plugin
        assert!(
            resolved.to_string_lossy().contains(".aiki/hooks"),
            "Project override should take priority. Got: {}",
            resolved.display()
        );
    }

    #[test]
    fn test_user_override_takes_priority_over_installed_plugin() {
        let temp_dir = create_test_project();
        let aiki_home = TempDir::new().unwrap();

        // Create user-level hook at AIKI_HOME/hooks/testns/testplug.yml
        let user_hook = aiki_home.path().join("hooks/testns/testplug.yml");
        fs::create_dir_all(user_hook.parent().unwrap()).unwrap();
        create_flow_file(&user_hook, "User Override");

        // Also install the plugin
        let plugin_dir = aiki_home
            .path()
            .join("plugins")
            .join("testns")
            .join("testplug");
        fs::create_dir_all(plugin_dir.join(".git")).unwrap();
        fs::write(plugin_dir.join("hooks.yaml"), "name: Plugin\nversion: '1'\n").unwrap();

        let result = with_temp_aiki_home(aiki_home.path(), || {
            let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();
            resolver.resolve("testns/testplug")
        });

        assert!(result.is_ok());
        let resolved = result.unwrap();
        // Should resolve to user hook, not the plugin
        assert!(
            resolved.to_string_lossy().contains("hooks/testns"),
            "User override should take priority over installed plugin. Got: {}",
            resolved.display()
        );
    }

    #[test]
    fn test_auto_fetch_hooks_yaml_missing_after_install() {
        let temp_dir = create_test_project();
        let aiki_home = TempDir::new().unwrap();

        // Pre-install a plugin WITHOUT hooks.yaml (e.g., template-only plugin)
        let plugin_dir = aiki_home
            .path()
            .join("plugins")
            .join("tplonly")
            .join("nofile");
        fs::create_dir_all(plugin_dir.join(".git")).unwrap();
        fs::write(plugin_dir.join("plugin.yaml"), "name: No Hooks\n").unwrap();
        // Note: no hooks.yaml

        // The resolver will find the installed plugin dir but hooks.yaml is missing.
        // It will skip step 3 (hooks.yaml doesn't exist) and proceed to step 4 (auto-fetch).
        // Auto-fetch will see the plugin is already installed and skip cloning.
        // Then it checks for hooks.yaml again — still missing → AutoFetchFailed.
        let result = with_temp_aiki_home(aiki_home.path(), || {
            let resolver = HookResolver::with_start_dir(temp_dir.path()).unwrap();
            resolver.resolve("tplonly/nofile")
        });

        match result {
            Err(AikiError::AutoFetchFailed { reason, .. }) => {
                assert!(
                    reason.contains("hooks.yaml missing"),
                    "Should mention hooks.yaml missing, got: {}",
                    reason
                );
            }
            other => panic!(
                "Expected AutoFetchFailed about hooks.yaml, got: {:?}",
                other
            ),
        }
    }
}
