use anyhow::{anyhow, Context, Result};
use std::fs;
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

    /// Check if a JJ repository exists at the given path
    pub fn has_jj<P: AsRef<Path>>(path: P) -> bool {
        path.as_ref().join(".jj").exists()
    }

    /// Resolve the Git directory path, handling both regular directories and worktree/submodule files
    ///
    /// In normal Git repositories, `.git` is a directory.
    /// In Git worktrees and submodules, `.git` is a file containing `gitdir: /path/to/real/git/dir`
    ///
    /// Returns the path to the actual Git directory, or an error if it cannot be resolved.
    #[allow(dead_code)] // Part of RepoDetector API
    pub fn resolve_git_dir<P: AsRef<Path>>(repo_root: P) -> Result<PathBuf> {
        let git_path = repo_root.as_ref().join(".git");

        if !git_path.exists() {
            return Err(anyhow!(
                "No .git file or directory found at repository root"
            ));
        }

        // Check if .git is a directory (normal case)
        if git_path.is_dir() {
            return Ok(git_path);
        }

        // .git is a file - parse the gitdir pointer (worktree/submodule case)
        let content = fs::read_to_string(&git_path)
            .context("Failed to read .git file - worktree/submodule pointer may be corrupted")?;

        // Parse "gitdir: /path/to/git/dir"
        let gitdir_line = content
            .lines()
            .find(|line| line.starts_with("gitdir: "))
            .ok_or_else(|| anyhow!("Invalid .git file format - expected 'gitdir:' line"))?;

        let git_dir_str = gitdir_line
            .strip_prefix("gitdir: ")
            .ok_or_else(|| anyhow!("Failed to parse gitdir path"))?
            .trim();

        // Convert to PathBuf and resolve relative paths
        let git_dir = PathBuf::from(git_dir_str);

        // If the path is relative, resolve it relative to the repo root
        let resolved = if git_dir.is_absolute() {
            git_dir
        } else {
            repo_root.as_ref().join(git_dir)
        };

        // Verify the resolved path exists
        if !resolved.exists() {
            return Err(anyhow!(
                "Git directory pointer references non-existent path: {}",
                resolved.display()
            ));
        }

        Ok(resolved)
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

    #[test]
    fn resolve_git_dir_handles_normal_repository() {
        let temp_dir = tempfile::tempdir().unwrap();
        fs::create_dir(temp_dir.path().join(".git")).unwrap();

        let result = RepoDetector::resolve_git_dir(temp_dir.path()).unwrap();
        assert_eq!(result, temp_dir.path().join(".git"));
    }

    #[test]
    fn resolve_git_dir_handles_worktree_with_absolute_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let real_git_dir = tempfile::tempdir().unwrap();

        // Create a .git file with absolute gitdir pointer (worktree case)
        let gitdir_content = format!("gitdir: {}", real_git_dir.path().display());
        fs::write(temp_dir.path().join(".git"), gitdir_content).unwrap();

        let result = RepoDetector::resolve_git_dir(temp_dir.path()).unwrap();
        assert_eq!(result, real_git_dir.path());
    }

    #[test]
    fn resolve_git_dir_handles_worktree_with_relative_path() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Create a real git directory as a subdirectory
        let git_subdir = temp_dir.path().join("some").join("nested").join("git");
        fs::create_dir_all(&git_subdir).unwrap();

        // Create a .git file with relative gitdir pointer
        let gitdir_content = "gitdir: some/nested/git";
        fs::write(temp_dir.path().join(".git"), gitdir_content).unwrap();

        let result = RepoDetector::resolve_git_dir(temp_dir.path()).unwrap();
        assert_eq!(result, git_subdir);
    }

    #[test]
    fn resolve_git_dir_errors_when_git_missing() {
        let temp_dir = tempfile::tempdir().unwrap();

        let result = RepoDetector::resolve_git_dir(temp_dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No .git file or directory found"));
    }

    #[test]
    fn resolve_git_dir_errors_on_invalid_worktree_pointer() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Create a .git file with invalid content
        fs::write(temp_dir.path().join(".git"), "invalid content").unwrap();

        let result = RepoDetector::resolve_git_dir(temp_dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("expected 'gitdir:' line"));
    }

    #[test]
    fn resolve_git_dir_errors_when_target_nonexistent() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Create a .git file pointing to non-existent directory
        fs::write(
            temp_dir.path().join(".git"),
            "gitdir: /nonexistent/path/to/git",
        )
        .unwrap();

        let result = RepoDetector::resolve_git_dir(temp_dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("references non-existent path"));
    }
}
