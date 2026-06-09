//! Flow loading with path resolution and caching.
//!
//! This module provides the [`HookLoader`] struct which handles:
//! - Path resolution via [`HookResolver`](super::hook_resolver::HookResolver)
//! - YAML parsing via [`HookParser`](super::parser::HookParser)
//! - Flow caching by canonical path (avoids reloading the same file)
//!
//! # Example
//!
//! ```rust,ignore
//! use aiki::flows::loader::HookLoader;
//!
//! let mut loader = HookLoader::new()?;
//!
//! // Load a flow (automatically cached)
//! let (hook, canonical_path) = loader.load("aiki/quick-lint")?;
//!
//! // Loading the same flow again returns cached version
//! let (flow2, _) = loader.load("aiki/quick-lint")?;
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::hook_resolver::HookResolver;
use super::parser::HookParser;
use super::types::Hook;
use crate::error::{AikiError, Result};

/// Flow loader with path resolution and caching.
///
/// The loader resolves flow paths using [`HookResolver`], parses the YAML using
/// [`HookParser`], and caches flows by their canonical path to avoid redundant
/// file reads and parsing.
///
/// # Caching
///
/// Flows are cached by their canonical (resolved) path. This means:
/// - `./flow.yml` and `../flows/flow.yml` that point to the same file will use the same cache entry
/// - The cache persists for the lifetime of the loader
/// - Use [`clear_cache`](Self::clear_cache) to reset if needed
#[derive(Debug)]
pub struct HookLoader {
    resolver: HookResolver,
    cache: HashMap<PathBuf, Hook>,
    failed_fetches: HashMap<String, String>,
}

impl HookLoader {
    /// Create a new HookLoader starting search from a specific directory.
    ///
    /// This is useful for testing or when you need to load flows from
    /// a directory other than the current working directory.
    ///
    /// # Errors
    ///
    /// Returns `AikiError::NotInAikiProject` if no `.aiki/` directory is found.
    pub fn with_start_dir(start_dir: &Path) -> Result<Self> {
        Ok(Self {
            resolver: HookResolver::with_start_dir(start_dir)?,
            cache: HashMap::new(),
            failed_fetches: HashMap::new(),
        })
    }

    /// Get the project root directory.
    pub fn project_root(&self) -> &Path {
        self.resolver.project_root()
    }

    /// Load a flow and return both the flow and its canonical path.
    ///
    /// The canonical path is used by [`HookComposer`](super::composer::HookComposer)
    /// for cycle detection. Caching is done by canonical path to avoid loading
    /// the same file multiple times.
    ///
    /// # Arguments
    ///
    /// * `path` - The namespaced flow path (e.g., "aiki/quick-lint", "eslint/check")
    ///
    /// # Returns
    ///
    /// A tuple of (Hook, canonical PathBuf). The canonical path is always absolute
    /// and resolved (no symlinks).
    ///
    /// # Errors
    ///
    /// Returns:
    /// - `AikiError::InvalidHookPath` if path is not in {namespace}/{name} format
    /// - `AikiError::HookNotFound` if the file doesn't exist
    /// - `AikiError::Other` if YAML parsing fails
    pub fn load(&mut self, path: &str) -> Result<(Hook, PathBuf)> {
        // Return cached failure. Use AutoFetchCached so the composer skips
        // silently — the warning was already printed on the first occurrence.
        if let Some(original_reason) = self.failed_fetches.get(path) {
            return Err(AikiError::AutoFetchCached {
                plugin: path.to_string(),
                reason: original_reason.clone(),
            });
        }

        // Try resolving from filesystem first
        match self.resolver.resolve(path) {
            Ok(canonical_path) => {
                // Check cache (by canonical path)
                if let Some(hook) = self.cache.get(&canonical_path) {
                    return Ok((hook.clone(), canonical_path));
                }

                // Load and parse hook file
                let hook = self.load_from_file(&canonical_path, path)?;

                // Cache by canonical path and return both hook and path
                self.cache.insert(canonical_path.clone(), hook.clone());
                Ok((hook, canonical_path))
            }
            Err(ref resolve_err @ AikiError::HookNotFound { .. })
            | Err(ref resolve_err @ AikiError::AutoFetchFailed { .. })
            | Err(ref resolve_err @ AikiError::AutoFetchCached { .. }) => {
                // Fallback: check built-in plugin registry
                let synthetic_path = PathBuf::from(format!("builtin://{}", path));

                // Check cache for built-in plugins too
                if let Some(hook) = self.cache.get(&synthetic_path) {
                    return Ok((hook.clone(), synthetic_path));
                }

                if let Some(result) = super::bundled::load_builtin_plugin(path) {
                    let hook = result.map_err(|e| {
                        AikiError::Other(anyhow::anyhow!(
                            "Failed to parse built-in plugin '{}': {}",
                            path,
                            e
                        ))
                    })?;
                    self.cache.insert(synthetic_path.clone(), hook.clone());
                    return Ok((hook, synthetic_path));
                }

                // Not a built-in either — memoize the failure and return the original error
                match resolve_err {
                    AikiError::AutoFetchFailed { reason, .. } => {
                        self.failed_fetches.insert(path.to_string(), reason.clone());
                        Err(AikiError::AutoFetchFailed {
                            plugin: path.to_string(),
                            reason: reason.clone(),
                        })
                    }
                    AikiError::AutoFetchCached { reason, .. } => {
                        self.failed_fetches.insert(path.to_string(), reason.clone());
                        Err(AikiError::AutoFetchCached {
                            plugin: path.to_string(),
                            reason: reason.clone(),
                        })
                    }
                    _ => Err(AikiError::HookNotFound {
                        path: path.to_string(),
                        resolved_path: path.to_string(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            format!("Hook '{}' not found on disk or as built-in plugin", path),
                        ),
                    }),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Load a flow from an absolute file path.
    ///
    /// This is used for loading flows that aren't in the standard namespace structure,
    /// such as .aiki/hooks/default.yml.
    ///
    /// # Arguments
    ///
    /// * `file_path` - Absolute path to the flow file
    ///
    /// # Returns
    ///
    /// A tuple of (Hook, canonical PathBuf). The canonical path is always absolute
    /// and resolved (no symlinks).
    ///
    /// # Errors
    ///
    /// Returns:
    /// - `AikiError::HookNotFound` if the file doesn't exist
    /// - `AikiError::Other` if YAML parsing fails
    pub fn load_from_file_path(&mut self, file_path: &Path) -> Result<(Hook, PathBuf)> {
        // Canonicalize the path
        let canonical_path = file_path
            .canonicalize()
            .map_err(|e| AikiError::HookNotFound {
                path: file_path.display().to_string(),
                resolved_path: file_path.display().to_string(),
                source: e,
            })?;

        // Check cache (by canonical path)
        if let Some(hook) = self.cache.get(&canonical_path) {
            return Ok((hook.clone(), canonical_path));
        }

        // Load and parse hook file
        let hook = self.load_from_file(&canonical_path, &file_path.display().to_string())?;

        // Cache by canonical path and return both hook and path
        self.cache.insert(canonical_path.clone(), hook.clone());
        Ok((hook, canonical_path))
    }

    /// Load and parse a flow from a file path.
    ///
    /// If the hook has no `name` field in the YAML, autogenerate one from
    /// `original_path` (e.g. "aiki/default" for namespaced loads, or the
    /// file path for direct loads).
    fn load_from_file(&self, path: &Path, original_path: &str) -> Result<Hook> {
        let contents = fs::read_to_string(path).map_err(|e| AikiError::HookNotFound {
            path: original_path.to_string(),
            resolved_path: path.display().to_string(),
            source: e,
        })?;

        let mut hook = HookParser::parse_str(&contents).map_err(|e| {
            AikiError::Other(anyhow::anyhow!(
                "Failed to parse flow '{}' ({}): {}",
                original_path,
                path.display(),
                e
            ))
        })?;

        // Autogenerate name from the load path when not specified in YAML
        if hook.name.is_empty() {
            hook.name = original_path.to_string();
        }

        Ok(hook)
    }
}

#[cfg(test)]
impl HookLoader {
    pub fn default_hooks_dir(&self) -> PathBuf {
        self.resolver.project_root().join(".aiki/hooks")
    }

    pub fn cache_size(&self) -> usize {
        self.cache.len()
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.failed_fetches.clear();
    }

    pub fn failed_fetches_count(&self) -> usize {
        self.failed_fetches.len()
    }

    pub fn mark_fetch_failed(&mut self, path: &str, reason: &str) {
        self.failed_fetches.insert(path.to_string(), reason.to_string());
    }

    pub fn load_core_hook() -> &'static Hook {
        super::bundled::load_core_hook()
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
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/aiki")).unwrap();
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/eslint")).unwrap();
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/helpers")).unwrap();
        temp_dir
    }

    /// Create a flow file with specified before/after dependencies.
    /// Uses the new CompositionBlock format: `before: { include: [...] }`.
    fn create_flow_file(path: &Path, name: &str, before: &[&str], after: &[&str]) {
        let quote_if_needed = |s: &str| -> String {
            if s.starts_with('@') || s.contains(':') || s.contains('#') {
                format!("\"{}\"", s)
            } else {
                s.to_string()
            }
        };

        let before_yaml = if before.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = before
                .iter()
                .map(|b| format!("    - {}", quote_if_needed(b)))
                .collect();
            format!("before:\n  include:\n{}\n", items.join("\n"))
        };

        let after_yaml = if after.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = after
                .iter()
                .map(|a| format!("    - {}", quote_if_needed(a)))
                .collect();
            format!("after:\n  include:\n{}\n", items.join("\n"))
        };

        let content = format!(
            r#"name: {}
version: "1"
{}{}
"#,
            name, before_yaml, after_yaml
        );
        fs::write(path, content).unwrap();
    }

    #[test]
    fn test_load_simple_flow() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/simple.yml");
        create_flow_file(&flow_path, "Simple Flow", &[], &[]);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let (hook, canonical) = loader.load("aiki/simple").unwrap();

        assert_eq!(hook.name, "Simple Flow");
        assert_eq!(
            canonical.canonicalize().unwrap(),
            flow_path.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_load_flow_with_before_after() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/composed.yml");
        create_flow_file(
            &flow_path,
            "Composed Flow",
            &["aiki/base", "./helpers/lint.yml"],
            &["aiki/cleanup"],
        );

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let (hook, _) = loader.load("aiki/composed").unwrap();

        assert_eq!(hook.name, "Composed Flow");
        assert_eq!(hook.before.len(), 1); // One CompositionBlock
        assert_eq!(hook.before[0].include.len(), 2);
        assert_eq!(hook.before[0].include[0], "aiki/base");
        assert_eq!(hook.before[0].include[1], "./helpers/lint.yml");
        assert_eq!(hook.after.len(), 1); // One CompositionBlock
        assert_eq!(hook.after[0].include.len(), 1);
        assert_eq!(hook.after[0].include[0], "aiki/cleanup");
    }

    #[test]
    fn test_load_caching() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/cached.yml");
        create_flow_file(&flow_path, "Cached Flow", &[], &[]);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        // First load
        assert_eq!(loader.cache_size(), 0);
        let (flow1, _) = loader.load("aiki/cached").unwrap();
        assert_eq!(loader.cache_size(), 1);

        // Second load (should hit cache)
        let (flow2, _) = loader.load("aiki/cached").unwrap();
        assert_eq!(loader.cache_size(), 1);

        // Same flow should be returned
        assert_eq!(flow1.name, flow2.name);
    }

    #[test]
    fn test_clear_cache() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/clearable.yml");
        create_flow_file(&flow_path, "Clearable Flow", &[], &[]);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        // Load and verify cache
        loader.load("aiki/clearable").unwrap();
        assert_eq!(loader.cache_size(), 1);

        // Clear cache
        loader.clear_cache();
        assert_eq!(loader.cache_size(), 0);

        // Load again (should reload from disk)
        loader.load("aiki/clearable").unwrap();
        assert_eq!(loader.cache_size(), 1);
    }

    #[test]
    fn test_load_flow_not_found() {
        let temp_dir = create_test_project();
        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        let result = loader.load("aiki/nonexistent");
        assert!(matches!(
            result,
            Err(AikiError::AutoFetchFailed { .. })
                | Err(AikiError::AutoFetchCached { .. })
                | Err(AikiError::HookNotFound { .. })
        ));
    }

    #[test]
    fn test_load_invalid_yaml() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/invalid.yml");
        fs::write(&flow_path, "invalid: yaml: content: [").unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let result = loader.load("aiki/invalid");

        assert!(matches!(result, Err(AikiError::Other(_))));
    }

    #[test]
    fn test_default_hooks_dir() {
        let temp_dir = create_test_project();
        let loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        let hooks_dir = loader.default_hooks_dir();
        assert_eq!(hooks_dir, temp_dir.path().join(".aiki/hooks"));
    }

    #[test]
    fn test_load_core_hook() {
        let core = HookLoader::load_core_hook();
        assert_eq!(core.name, "Aiki Core");
    }

    #[test]
    fn test_load_builtin_fallback() {
        let temp_dir = create_test_project();
        // No aiki/default.yml file on disk
        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        // Should fall back to built-in plugin
        let (hook, path) = loader.load("aiki/default").unwrap();
        assert_eq!(hook.name, "aiki/default");
        assert_eq!(path, PathBuf::from("builtin://aiki/default"));
    }

    #[test]
    fn test_load_project_overrides_builtin() {
        let temp_dir = create_test_project();
        // Create a project-level override for aiki/default
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/default.yml");
        create_flow_file(&flow_path, "Project Override", &[], &[]);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        // Should use the project file, not the built-in
        let (hook, path) = loader.load("aiki/default").unwrap();
        assert_eq!(hook.name, "Project Override");
        // Path should be the real file, not builtin://
        assert!(!path.to_string_lossy().starts_with("builtin://"));
    }

    #[test]
    fn test_load_builtin_caching() {
        let temp_dir = create_test_project();
        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        // First load — should cache
        assert_eq!(loader.cache_size(), 0);
        let (hook1, _) = loader.load("aiki/default").unwrap();
        assert_eq!(loader.cache_size(), 1);

        // Second load — should hit cache
        let (hook2, _) = loader.load("aiki/default").unwrap();
        assert_eq!(loader.cache_size(), 1);
        assert_eq!(hook1.name, hook2.name);
    }

    #[test]
    fn test_auto_fetch_failed_memoization() {
        let temp_dir = create_test_project();
        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        // Manually mark a plugin as having a failed fetch
        loader.mark_fetch_failed("fakens/fakeplugin", "registry returned 404");
        assert_eq!(loader.failed_fetches_count(), 1);

        // Loading should return memoized AutoFetchCached (warning already shown)
        let result = loader.load("fakens/fakeplugin");
        assert!(matches!(
            result,
            Err(AikiError::AutoFetchCached { ref reason, .. })
                if reason == "registry returned 404"
        ), "Expected memoized AutoFetchCached with original reason, got: {:?}", result);

        // Cache size should remain 0 — the resolver was never called
        assert_eq!(loader.cache_size(), 0);

        // Loading again should still return the same memoized error with original reason
        let result2 = loader.load("fakens/fakeplugin");
        assert!(matches!(
            result2,
            Err(AikiError::AutoFetchCached { ref reason, .. })
                if reason == "registry returned 404"
        ));
    }

    #[test]
    fn test_clear_cache_clears_failed_fetches() {
        let temp_dir = create_test_project();
        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        // Mark a plugin as failed
        loader.mark_fetch_failed("fakens/someplugin", "network timeout");
        assert_eq!(loader.failed_fetches_count(), 1);

        // Verify it's memoized (returns AutoFetchCached since warning was already shown)
        let result = loader.load("fakens/someplugin");
        assert!(matches!(result, Err(AikiError::AutoFetchCached { .. })));

        // Also load a successful hook to populate the main cache
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/cleartest.yml");
        create_flow_file(&flow_path, "Clear Test Flow", &[], &[]);
        loader.load("aiki/cleartest").unwrap();
        assert_eq!(loader.cache_size(), 1);

        // Clear everything
        loader.clear_cache();
        assert_eq!(loader.cache_size(), 0);
        assert_eq!(loader.failed_fetches_count(), 0);

        // After clearing, loading the previously-failed plugin should attempt
        // resolution again (not return memoized error). It will still fail,
        // but the error message should NOT be the original memoized reason.
        let result2 = loader.load("fakens/someplugin");
        assert!(result2.is_err());
        if let Err(AikiError::AutoFetchFailed { reason, .. } | AikiError::AutoFetchCached { reason, .. }) = &result2 {
            assert_ne!(
                reason, "network timeout",
                "After clear_cache, load should re-attempt resolution, not return memoized error"
            );
        }
    }

    #[test]
    fn test_memoized_auto_fetch_does_not_affect_other_plugins() {
        let temp_dir = create_test_project();
        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        // Mark one plugin as failed
        loader.mark_fetch_failed("fakens/badplugin", "plugin not found in registry");

        // A different plugin on disk should still load successfully
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/good.yml");
        create_flow_file(&flow_path, "Good Flow", &[], &[]);

        let (hook, _) = loader.load("aiki/good").unwrap();
        assert_eq!(hook.name, "Good Flow");

        // The failed plugin should still be memoized with its original reason
        let result = loader.load("fakens/badplugin");
        assert!(matches!(
            result,
            Err(AikiError::AutoFetchCached { ref reason, .. })
                if reason == "plugin not found in registry"
        ));
    }

    #[test]
    fn test_load_not_found_records_auto_fetch_failure() {
        let temp_dir = create_test_project();
        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();

        assert_eq!(loader.failed_fetches_count(), 0);

        // Load a non-existent plugin — this goes through the full resolve path
        let result = loader.load("aiki/nonexistent");
        assert!(result.is_err());

        // If it was an AutoFetchFailed, it should have been memoized
        if matches!(result, Err(AikiError::AutoFetchFailed { .. })) {
            assert_eq!(
                loader.failed_fetches_count(),
                1,
                "AutoFetchFailed should be memoized in failed_fetches"
            );

            // Second call should return AutoFetchCached (warning already shown)
            let result2 = loader.load("aiki/nonexistent");
            match (&result, &result2) {
                (
                    Err(AikiError::AutoFetchFailed { reason: original, .. }),
                    Err(AikiError::AutoFetchCached { reason: memoized, .. }),
                ) => {
                    assert_eq!(original, memoized, "Memoized reason should match original");
                }
                _ => panic!("Expected memoized AutoFetchCached, got: {:?}", result2),
            }
        }
    }
}
