use anyhow::{Context, Result};
use jj_lib::config::StackedConfig;
use jj_lib::settings::UserSettings;
use jj_lib::workspace::Workspace;
use std::path::{Path, PathBuf};

/// Wrapper for JJ workspace operations using jj-lib
#[derive(Debug)]
pub struct JJWorkspace {
    workspace_root: PathBuf,
}

impl JJWorkspace {
    /// Create a new JJ workspace manager for the given path
    #[must_use]
    pub fn new<P: AsRef<Path>>(workspace_root: P) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    /// Find JJ workspace root by walking up from given path
    ///
    /// Searches parent directories for `.jj/` directory.
    /// Returns error if not in a JJ workspace.
    pub fn find(path: &Path) -> Result<Self> {
        let mut current = path.to_path_buf();

        loop {
            let jj_dir = current.join(".jj");
            if jj_dir.is_dir() {
                return Ok(Self::new(current));
            }

            match current.parent() {
                Some(parent) => current = parent.to_path_buf(),
                None => anyhow::bail!("Not in a JJ workspace (no .jj directory found)"),
            }
        }
    }

    /// Get the workspace root path
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Create user settings with default configuration for JJ operations
    pub(crate) fn create_user_settings() -> Result<UserSettings> {
        let config = StackedConfig::with_defaults();
        UserSettings::from_config(config)
            .context("Failed to create user settings for JJ operations")
    }

    /// Initialize a JJ repository with internal Git storage (non-colocated)
    /// This creates a .jj directory with a hidden Git backend in .jj/repo/store/git
    /// The Git repo is completely independent from any .git in the working directory
    pub fn init(&self) -> Result<()> {
        let settings = Self::create_user_settings()?;

        // Initialize JJ workspace with internal Git backend (non-colocated)
        // This is equivalent to `jj git init --no-colocate`
        let (_workspace, _repo) = Workspace::init_internal_git(&settings, &self.workspace_root)
            .context("Failed to initialize JJ workspace")?;

        Ok(())
    }

    /// Check whether the JJ workspace has the expected non-colocated structure.
    ///
    /// Returns `true` if `.jj/repo/store/type` exists and the internal git
    /// backend directory (`.jj/repo/store/git`) is present. Returns `false`
    /// if the workspace is missing critical files (broken) or was initialized
    /// in colocated mode (e.g. by a newer jj that defaults to `--colocate`).
    #[must_use]
    pub fn is_healthy_non_colocated(&self) -> bool {
        let store_type = self.workspace_root.join(".jj/repo/store/type");
        let internal_git = self.workspace_root.join(".jj/repo/store/git");
        store_type.is_file() && internal_git.is_dir()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Verifies that JJ initialization creates .jj directory with internal Git storage
    #[test]
    fn workspace_init_creates_jj_directory() {
        // Create a temporary directory for testing
        let temp_dir = tempfile::tempdir().unwrap();

        let workspace = JJWorkspace::new(temp_dir.path());
        let result = workspace.init();

        // Should succeed
        assert!(
            result.is_ok(),
            "Workspace initialization should succeed: {:?}",
            result.err()
        );

        // Verify .jj directory was created
        assert!(
            temp_dir.path().join(".jj").exists(),
            ".jj directory should exist"
        );

        // Verify .git directory was NOT created in working copy (non-colocated)
        assert!(
            !temp_dir.path().join(".git").exists(),
            ".git directory should not exist in working copy for non-colocated workspace"
        );

        // Verify internal Git storage exists
        assert!(
            temp_dir.path().join(".jj/repo/store/git").exists(),
            "Internal Git storage should exist at .jj/repo/store/git"
        );
    }

    #[test]
    fn workspace_new_stores_root_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = JJWorkspace::new(temp_dir.path());

        assert_eq!(workspace.workspace_root, temp_dir.path());
    }

    #[test]
    fn test_find_workspace_from_root() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::create_dir(temp_dir.path().join(".jj")).unwrap();

        let workspace = JJWorkspace::find(temp_dir.path()).unwrap();
        assert_eq!(
            workspace.workspace_root().canonicalize().unwrap(),
            temp_dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn test_find_workspace_from_subdir() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::create_dir(temp_dir.path().join(".jj")).unwrap();
        let subdir = temp_dir.path().join("src/nested");
        fs::create_dir_all(&subdir).unwrap();

        let workspace = JJWorkspace::find(&subdir).unwrap();
        assert_eq!(
            workspace.workspace_root().canonicalize().unwrap(),
            temp_dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn test_find_workspace_not_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        let result = JJWorkspace::find(temp_dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Not in a JJ workspace"));
    }

    #[test]
    fn test_workspace_root_getter() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = JJWorkspace::new(temp_dir.path());
        assert_eq!(workspace.workspace_root(), temp_dir.path());
    }

    #[test]
    fn healthy_non_colocated_after_init() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = JJWorkspace::new(temp_dir.path());
        workspace.init().unwrap();

        assert!(
            workspace.is_healthy_non_colocated(),
            "Workspace should be healthy after init"
        );
    }

    #[test]
    fn unhealthy_when_jj_dir_empty() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::create_dir(temp_dir.path().join(".jj")).unwrap();

        let workspace = JJWorkspace::new(temp_dir.path());
        assert!(
            !workspace.is_healthy_non_colocated(),
            "Empty .jj should not be healthy"
        );
    }
}
