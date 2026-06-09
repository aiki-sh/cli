//! Low-level path resolver for all file types (flows, docs, scripts, etc.)
//!
//! This module handles resolution of paths with special prefixes:
//! - `@/` - Project root (relative to project with .aiki/ directory)
//! - `./` - Relative to current directory
//! - `../` - Relative to parent directory
//! - `/` - Absolute path
//!
//! For flow-specific resolution ({namespace}/{name}), use [`HookResolver`](super::hook_resolver::HookResolver).

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
}

impl PathResolver {
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

        Ok(Self {
            project_root,
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
        let temp_dir = tempfile::Builder::new()
            .prefix("aiki-no-project-")
            .tempdir_in("/tmp")
            .unwrap();
        // No .aiki/ directory created in this temp dir or its immediate parent chain.
        // Using /tmp avoids false positives when $TMPDIR itself lives inside an Aiki project.
        let mut current = temp_dir.path();
        while current != Path::new("/") {
            assert!(
                !current.join(".aiki").is_dir(),
                "test setup invalid: found unexpected .aiki at {}",
                current.display()
            );
            current = current.parent().unwrap_or(Path::new("/"));
        }

        let result = PathResolver::with_start_dir(temp_dir.path());
        assert!(matches!(result, Err(AikiError::NotInAikiProject { .. })));
    }
}
