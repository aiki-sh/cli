use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// Detects and validates repository state (Git and JJ)
pub struct RepoDetector {
    current_dir: PathBuf,
}

impl RepoDetector {
    /// Create a new repository detector for the given directory
    pub fn new<P: AsRef<Path>>(current_dir: P) -> Self {
        Self {
            current_dir: current_dir.as_ref().to_path_buf(),
        }
    }

    /// Find the repository root by walking up the directory tree
    /// Returns the path to the repository root if found
    ///
    /// Stops at the filesystem root or home directory to avoid escaping
    /// the user's repository tree and accidentally finding a parent's .git
    pub fn find_repo_root(&self) -> Result<PathBuf> {
        // Get home directory as a boundary - don't walk above it
        let home_dir = dirs::home_dir();

        // Reuse a single PathBuf for checking, avoiding allocations in the loop
        let mut search_path = self.current_dir.join(".git");

        loop {
            // Check if .git exists at current level
            if search_path.exists() {
                search_path.pop(); // Remove ".git", leaving repo root
                return Ok(search_path);
            }

            // Move up one level by removing ".git" first
            search_path.pop();

            // Stop if we've reached the home directory without finding a repo
            if let Some(ref home) = home_dir {
                if search_path == *home {
                    return Err(anyhow!(
                        "Not in a Git repository\n\nRun 'git init' first, or navigate to an existing Git repository."
                    ));
                }
            }

            // Try to move up to parent directory
            if !search_path.pop() {
                // pop() returns false when at filesystem root
                return Err(anyhow!(
                    "Not in a Git repository\n\nRun 'git init' first, or navigate to an existing Git repository."
                ));
            }

            // Add ".git" back for next iteration
            search_path.push(".git");
        }
    }

    /// Find the aiki project root by walking up from `current_dir` looking for `.aiki/`.
    ///
    /// Stops at the filesystem root or home directory.
    pub fn find_aiki_root(&self) -> Result<PathBuf> {
        let home_dir = dirs::home_dir();
        let mut current = self.current_dir.clone();

        loop {
            if current.join(".aiki").is_dir() {
                return Ok(current);
            }

            // Stop at home directory
            if let Some(ref home) = home_dir {
                if current == *home {
                    return Err(anyhow!(
                        "Not in an aiki project\n\nRun 'aiki init' first to initialize this repository."
                    ));
                }
            }

            if !current.pop() {
                return Err(anyhow!(
                    "Not in an aiki project\n\nRun 'aiki init' first to initialize this repository."
                ));
            }
        }
    }

    /// Check if a JJ repository exists at the given path
    pub fn has_jj<P: AsRef<Path>>(path: P) -> bool {
        path.as_ref().join(".jj").exists()
    }

    /// Get the repo folder name (last path component of the git root).
    ///
    /// Falls back to the `cwd` folder name if `.git` can't be found.
    #[allow(dead_code)]
    pub fn repo_folder_name(&self) -> String {
        let root = self
            .find_repo_root()
            .ok()
            .unwrap_or_else(|| self.current_dir.clone());
        root.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn find_repo_root_at_current_directory() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::create_dir(temp_dir.path().join(".git")).unwrap();

        let detector = RepoDetector::new(temp_dir.path());
        let root = detector.find_repo_root().unwrap();

        assert_eq!(root, temp_dir.path());
    }

    #[test]
    fn find_repo_root_from_subdirectory() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::create_dir(temp_dir.path().join(".git")).unwrap();

        // Create a subdirectory
        let subdir = temp_dir.path().join("src").join("components");
        fs::create_dir_all(&subdir).unwrap();

        let detector = RepoDetector::new(&subdir);
        let root = detector.find_repo_root().unwrap();

        assert_eq!(root, temp_dir.path());
    }

    #[test]
    fn find_repo_root_errors_when_not_in_repo() {
        let temp_dir = tempfile::tempdir().unwrap();

        let detector = RepoDetector::new(temp_dir.path());
        let result = detector.find_repo_root();

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Not in a Git repository"));
    }

    #[test]
    fn find_repo_root_stops_at_home_directory() {
        // This test verifies that repo discovery stops at the home directory
        // We can't easily test this in isolation without mocking, but we can
        // verify the behavior when starting from home directory itself
        if let Some(home) = dirs::home_dir() {
            let detector = RepoDetector::new(&home);
            let result = detector.find_repo_root();

            // If there's no .git in home directory, it should error immediately
            // without walking above home
            if !home.join(".git").exists() {
                assert!(result.is_err());
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("Not in a Git repository"));
            }
        }
    }

    #[test]
    fn find_aiki_root_at_current_directory() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::create_dir(temp_dir.path().join(".aiki")).unwrap();

        let detector = RepoDetector::new(temp_dir.path());
        let root = detector.find_aiki_root().unwrap();

        assert_eq!(root, temp_dir.path());
    }

    #[test]
    fn find_aiki_root_from_subdirectory() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::create_dir(temp_dir.path().join(".aiki")).unwrap();

        let subdir = temp_dir.path().join("src").join("components");
        fs::create_dir_all(&subdir).unwrap();

        let detector = RepoDetector::new(&subdir);
        let root = detector.find_aiki_root().unwrap();

        assert_eq!(root, temp_dir.path());
    }

    #[test]
    fn find_aiki_root_stops_at_home_directory() {
        // Verify that find_aiki_root stops at home (or filesystem root)
        // and doesn't walk above it
        if let Some(home) = dirs::home_dir() {
            let detector = RepoDetector::new(&home);
            let result = detector.find_aiki_root();

            if !home.join(".aiki").exists() {
                assert!(result.is_err());
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("Not in an aiki project"));
            }
        }
    }

    #[test]
    fn has_jj_returns_true_when_jj_directory_exists() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::create_dir(temp_dir.path().join(".jj")).unwrap();

        assert!(RepoDetector::has_jj(temp_dir.path()));
    }

    #[test]
    fn has_jj_returns_false_when_jj_directory_missing() {
        let temp_dir = tempfile::tempdir().unwrap();

        assert!(!RepoDetector::has_jj(temp_dir.path()));
    }
}
