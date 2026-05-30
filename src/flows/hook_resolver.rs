//! High-level flow resolver (uses PathResolver + flow-specific logic).
//!
//! This module handles resolution of flow paths with namespacing:
//! - `{namespace}/*` - Namespaced flows (e.g., `aiki/*`, `eslint/*`, `prettier/*`)
//!   - Searches project `.aiki/hooks/{namespace}/` first, then `~/.aiki/hooks/{namespace}/`
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

/// High-level flow resolver (uses PathResolver + flow-specific logic).
///
/// Resolves flow paths to canonical absolute paths. Supports:
/// - `{namespace}/{name}` - Namespaced flows (e.g., `aiki/*`, `eslint/*`, `prettier/*`)
///   - All top-level directories in `.aiki/hooks/` are treated as namespaces
///   - `aiki` is just another namespace, not a special case
///   - Searches project `.aiki/hooks/{namespace}/` first, then `~/.aiki/hooks/{namespace}/`
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
    /// Create a new HookResolver by discovering project root.
    ///
    /// # Errors
    ///
    /// Returns `AikiError::NotInAikiProject` if no `.aiki/` directory is found.
    #[allow(dead_code)] // Part of HookResolver API
    pub fn new() -> Result<Self> {
        Ok(Self {
            path_resolver: PathResolver::new()?,
        })
    }

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
    #[allow(dead_code)] // Part of HookResolver API
    pub fn project_root(&self) -> &Path {
        self.path_resolver.project_root()
    }

    /// Get the home directory.
    #[must_use]
    #[allow(dead_code)] // Part of HookResolver API
    pub fn home_dir(&self) -> &Path {
        self.path_resolver.home_dir()
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
    /// | `{namespace}/{name}` | 1. Project `.aiki/hooks/{namespace}/{name}.yml`<br>2. User `~/.aiki/hooks/{namespace}/{name}.yml` |
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
    /// 2. `~/.aiki/hooks/{namespace}/{name}.yml`
    ///
    /// # Arguments
    ///
    /// * `namespace` - The namespace (e.g., "aiki", "eslint", "prettier")
    /// * `name` - The flow name within the namespace (may contain slashes for nesting)
    fn resolve_namespaced_flow(&self, namespace: &str, name: &str) -> Result<PathBuf> {
        // Try project first
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

        // Fall back to user
        let user_path = self
            .path_resolver
            .home_dir()
            .join(".aiki/hooks")
            .join(namespace)
            .join(name);
        let user_path = Self::add_yml_extension(&user_path);

        Ok(user_path)
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
        assert!(matches!(result, Err(AikiError::HookNotFound { .. })));
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
}
