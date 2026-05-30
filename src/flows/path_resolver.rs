//! Low-level path resolver for all file types (flows, docs, scripts, etc.)
//!
//! This module handles resolution of paths with special prefixes:
//! - `@/` - Project root (relative to project with .aiki/ directory)
//! - `./` - Relative to current directory
//! - `../` - Relative to parent directory
//! - `/` - Absolute path
//!
//! For flow-specific resolution ({namespace}/{name}), use [`HookResolver`](super::hook_resolver::HookResolver).

use std::env;
use std::path::{Path, PathBuf};

use crate::error::{AikiError, Result};

/// Low-level path resolver for all file types (flows, docs, scripts, etc.)
///
/// Discovers and caches the project root (directory containing `.aiki/`) and
/// home directory for efficient path resolution.
///
/// # Example
///
/// ```rust,ignore
/// use aiki::flows::path_resolver::PathResolver;
///
/// let resolver = PathResolver::new()?;
///
/// // Resolve project-root relative path
/// let path = resolver.resolve("@/docs/architecture.md", &current_dir)?;
///
/// // Resolve current-directory relative path
/// let path = resolver.resolve("./helpers/lint.yml", &current_dir)?;
/// ```
#[derive(Debug, Clone)]
pub struct PathResolver {
    /// Project root directory (discovered once, cached)
    project_root: PathBuf,
    /// Home directory (cached for performance)
    home_dir: PathBuf,
}

impl PathResolver {
    /// Create a new PathResolver by discovering project root.
    ///
    /// Searches upward from the current directory for a `.aiki/` directory.
    ///
    /// # Errors
    ///
    /// Returns `AikiError::NotInAikiProject` if no `.aiki/` directory is found.
    #[allow(dead_code)] // Part of PathResolver API
    pub fn new() -> Result<Self> {
        let cwd = env::current_dir()?;
        Self::with_start_dir(&cwd)
    }

    /// Create a new PathResolver starting search from a specific directory.
    ///
    /// This is useful for testing or when you need to resolve paths from
    /// a directory other than the current working directory.
    ///
    /// # Errors
    ///
    /// Returns `AikiError::NotInAikiProject` if no `.aiki/` directory is found.
    pub fn with_start_dir(start_dir: &Path) -> Result<Self> {
        let project_root = Self::find_project_root(start_dir)?;
        let home_dir = dirs::home_dir().ok_or_else(|| {
            AikiError::Other(anyhow::anyhow!("Could not determine home directory"))
        })?;

        Ok(Self {
            project_root,
            home_dir,
        })
    }

    /// Find project root by searching upward for .aiki/ directory.
    ///
    /// # Errors
    ///
    /// Returns `AikiError::NotInAikiProject` if no `.aiki/` directory is found.
    fn find_project_root(start_dir: &Path) -> Result<PathBuf> {
        let mut current = start_dir.to_path_buf();

        loop {
            if current.join(".aiki").is_dir() {
                return Ok(current);
            }

            match current.parent() {
                Some(parent) => current = parent.to_path_buf(),
                None => {
                    return Err(AikiError::NotInAikiProject {
                        searched_from: start_dir.to_path_buf(),
                    });
                }
            }
        }
    }

    /// Get the discovered project root directory.
    #[must_use]
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Get the home directory.
    #[must_use]
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// Resolve a generic path (does NOT add .yml extension or search flow directories).
    ///
    /// Used for docs, configs, scripts, or any non-flow file.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to resolve. Must start with `@/`, `./`, `../`, or `/`.
    /// * `current_dir` - The directory to use for relative paths (`./, ../`).
    ///
    /// # Supported Path Prefixes
    ///
    /// | Prefix | Meaning | Example |
    /// |--------|---------|---------|
    /// | `@/` | Project root | `@/docs/arch.md` → `/project/docs/arch.md` |
    /// | `./` | Current directory | `./helpers/lint.yml` → `{current_dir}/helpers/lint.yml` |
    /// | `../` | Parent directory | `../shared/base.yml` → `{current_dir}/../shared/base.yml` |
    /// | `/` | Absolute path | `/abs/path/file.txt` → as-is |
    ///
    /// # Errors
    ///
    /// Returns `AikiError::InvalidPath` if:
    /// - Path is empty
    /// - Path after `@/` is empty
    /// - Path doesn't start with a recognized prefix
    #[allow(dead_code)] // Part of PathResolver API
    pub fn resolve(&self, path: &str, current_dir: &Path) -> Result<PathBuf> {
        if path.is_empty() {
            return Err(AikiError::InvalidPath {
                path: path.to_string(),
                reason: "Path cannot be empty".to_string(),
            });
        }

        let resolved = if let Some(rest) = path.strip_prefix("@/") {
            // Project root
            if rest.is_empty() {
                return Err(AikiError::InvalidPath {
                    path: path.to_string(),
                    reason: "Path after @/ cannot be empty".to_string(),
                });
            }
            self.project_root.join(rest)
        } else if path.starts_with("./") || path.starts_with("../") {
            // Relative to current directory
            current_dir.join(path)
        } else if path.starts_with('/') {
            // Absolute path
            PathBuf::from(path)
        } else {
            return Err(AikiError::InvalidPath {
                path: path.to_string(),
                reason: "Path must start with @/, ./, ../, or /".to_string(),
            });
        };

        Ok(resolved)
    }

    /// Resolve a tilde-prefixed path (e.g., `~/...`).
    ///
    /// This is a convenience method for resolving paths that start with `~`.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to resolve. If it starts with `~`, expands to home directory.
    ///
    /// # Returns
    ///
    /// The resolved absolute path.
    #[must_use]
    #[allow(dead_code)] // Part of PathResolver API
    pub fn resolve_tilde(&self, path: &str) -> PathBuf {
        if let Some(rest) = path.strip_prefix("~/") {
            self.home_dir.join(rest)
        } else if path == "~" {
            self.home_dir.clone()
        } else {
            PathBuf::from(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a test project with .aiki/ directory
    fn create_test_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks")).unwrap();
        fs::create_dir_all(temp_dir.path().join("docs")).unwrap();
        fs::create_dir_all(temp_dir.path().join("src")).unwrap();
        temp_dir
    }

    #[test]
    fn test_find_project_root_direct() {
        let temp_dir = create_test_project();
        let resolver = PathResolver::with_start_dir(temp_dir.path()).unwrap();
        assert_eq!(resolver.project_root(), temp_dir.path());
    }

    #[test]
    fn test_find_project_root_from_subdirectory() {
        let temp_dir = create_test_project();
        let sub_dir = temp_dir.path().join("src/deep/nested");
        fs::create_dir_all(&sub_dir).unwrap();

        let resolver = PathResolver::with_start_dir(&sub_dir).unwrap();
        assert_eq!(resolver.project_root(), temp_dir.path());
    }

    #[test]
    fn test_find_project_root_not_found() {
        let temp_dir = TempDir::new().unwrap();
        // No .aiki/ directory created

        let result = PathResolver::with_start_dir(temp_dir.path());
        assert!(matches!(result, Err(AikiError::NotInAikiProject { .. })));
    }

    #[test]
    fn test_resolve_project_root_path() {
        let temp_dir = create_test_project();
        let resolver = PathResolver::with_start_dir(temp_dir.path()).unwrap();

        let resolved = resolver
            .resolve("@/docs/architecture.md", temp_dir.path())
            .unwrap();
        assert_eq!(resolved, temp_dir.path().join("docs/architecture.md"));
    }

    #[test]
    fn test_resolve_relative_path_current_dir() {
        let temp_dir = create_test_project();
        let resolver = PathResolver::with_start_dir(temp_dir.path()).unwrap();
        let current_dir = temp_dir.path().join("src");

        let resolved = resolver
            .resolve("./helpers/utils.rs", &current_dir)
            .unwrap();
        assert_eq!(resolved, current_dir.join("helpers/utils.rs"));
    }

    #[test]
    fn test_resolve_relative_path_parent_dir() {
        let temp_dir = create_test_project();
        let resolver = PathResolver::with_start_dir(temp_dir.path()).unwrap();
        let current_dir = temp_dir.path().join("src/helpers");
        fs::create_dir_all(&current_dir).unwrap();

        let resolved = resolver.resolve("../shared/base.rs", &current_dir).unwrap();
        assert_eq!(resolved, current_dir.join("../shared/base.rs"));
    }

    #[test]
    fn test_resolve_absolute_path() {
        let temp_dir = create_test_project();
        let resolver = PathResolver::with_start_dir(temp_dir.path()).unwrap();

        let resolved = resolver
            .resolve("/absolute/path/to/file.txt", temp_dir.path())
            .unwrap();
        assert_eq!(resolved, PathBuf::from("/absolute/path/to/file.txt"));
    }

    #[test]
    fn test_resolve_empty_path_error() {
        let temp_dir = create_test_project();
        let resolver = PathResolver::with_start_dir(temp_dir.path()).unwrap();

        let result = resolver.resolve("", temp_dir.path());
        assert!(matches!(result, Err(AikiError::InvalidPath { .. })));
    }

    #[test]
    fn test_resolve_empty_after_project_root_error() {
        let temp_dir = create_test_project();
        let resolver = PathResolver::with_start_dir(temp_dir.path()).unwrap();

        let result = resolver.resolve("@/", temp_dir.path());
        assert!(matches!(result, Err(AikiError::InvalidPath { .. })));
    }

    #[test]
    fn test_resolve_invalid_prefix_error() {
        let temp_dir = create_test_project();
        let resolver = PathResolver::with_start_dir(temp_dir.path()).unwrap();

        let result = resolver.resolve("invalid/path", temp_dir.path());
        assert!(matches!(result, Err(AikiError::InvalidPath { .. })));
    }

    #[test]
    fn test_resolve_tilde_path() {
        let temp_dir = create_test_project();
        let resolver = PathResolver::with_start_dir(temp_dir.path()).unwrap();

        let resolved = resolver.resolve_tilde("~/.aiki/hooks/my-flow.yml");
        assert!(resolved.ends_with(".aiki/hooks/my-flow.yml"));
        assert!(resolved.starts_with(resolver.home_dir()));
    }

    #[test]
    fn test_resolve_tilde_only() {
        let temp_dir = create_test_project();
        let resolver = PathResolver::with_start_dir(temp_dir.path()).unwrap();

        let resolved = resolver.resolve_tilde("~");
        assert_eq!(resolved, resolver.home_dir().to_path_buf());
    }

    #[test]
    fn test_resolve_non_tilde_path() {
        let temp_dir = create_test_project();
        let resolver = PathResolver::with_start_dir(temp_dir.path()).unwrap();

        let resolved = resolver.resolve_tilde("/absolute/path");
        assert_eq!(resolved, PathBuf::from("/absolute/path"));
    }
}
