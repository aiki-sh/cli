//! Repository identification
//!
//! Provides stable repository identifiers that persist across clones, moves, and renames.
//! The primary identifier is the git root commit hash (first commit in history).
//!
//! For repositories without commits, a fallback `local-{hash}` identifier is used.

use crate::error::{AikiError, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::process::Command;

/// The filename for storing the repository ID
pub const REPO_ID_FILENAME: &str = "repo-id";

/// Get the repository ID file path for a repo
#[must_use]
pub fn repo_id_path(repo_path: &Path) -> std::path::PathBuf {
    repo_path.join(".aiki").join(REPO_ID_FILENAME)
}

/// Read the repository ID from the repo-id file
///
/// Returns `None` if the file doesn't exist or is empty.
pub fn read_repo_id(repo_path: &Path) -> Result<Option<String>> {
    let path = repo_id_path(repo_path);

    match fs::read_to_string(&path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AikiError::Other(anyhow::anyhow!(
            "Failed to read repo-id file: {}",
            e
        ))),
    }
}

/// Compute the repository ID for a git repository
///
/// Uses the root commit hash (first commit) as the stable identifier.
/// Falls back to `local-{hash(canonical_path)}` for repos without commits.
pub fn compute_repo_id(repo_path: &Path) -> Result<String> {
    // Try to get the root commit hash (truncated to 8 hex chars)
    if let Some(root_hash) = get_git_root_commit(repo_path)? {
        return Ok(root_hash[..8].to_string());
    }

    // Fallback: Use path-based hash for repos without commits
    let canonical = repo_path.canonicalize().map_err(|e| {
        AikiError::Other(anyhow::anyhow!(
            "Failed to canonicalize repo path '{}': {}",
            repo_path.display(),
            e
        ))
    })?;

    let path_str = canonical.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let hash = hasher.finalize();

    // Use first 8 hex characters for the hash
    let short_hash = hex::encode(&hash[..4]);
    Ok(format!("local-{}", short_hash))
}

/// Get the git root commit hash (first commit in history)
///
/// Returns `None` if:
/// - Not a git repository
/// - Repository has no commits
fn get_git_root_commit(repo_path: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to run git: {}", e)))?;

    if !output.status.success() {
        // Could be: not a git repo, no commits, etc.
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let hash = stdout.lines().next().map(|s| s.trim().to_string());

    // Filter out empty strings
    Ok(hash.filter(|h| !h.is_empty()))
}

/// Ensure the repo-id file exists and is up-to-date
///
/// Behavior:
/// - If file exists with content: skip (idempotent)
/// - If file exists but empty and repo now has commits: update with root hash
/// - If file doesn't exist: create with computed ID
///
/// Returns the repository ID.
pub fn ensure_repo_id(repo_path: &Path) -> Result<String> {
    let path = repo_id_path(repo_path);

    // Ensure .aiki directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            AikiError::Other(anyhow::anyhow!("Failed to create .aiki directory: {}", e))
        })?;
    }

    // Check existing file
    let existing = read_repo_id(repo_path)?;

    match existing {
        Some(id) if is_long_hex_id(&id) => {
            // Truncate legacy 40-char hex IDs to 8 chars
            let short = id[..8].to_string();
            fs::write(&path, format!("{}\n", short)).map_err(|e| {
                AikiError::Other(anyhow::anyhow!("Failed to write repo-id file: {}", e))
            })?;
            Ok(short)
        }
        Some(id) => {
            // File exists with content - keep it (idempotent)
            Ok(id)
        }
        None => {
            // File doesn't exist or is empty - compute and write
            let repo_id = compute_repo_id(repo_path)?;
            fs::write(&path, format!("{}\n", repo_id)).map_err(|e| {
                AikiError::Other(anyhow::anyhow!("Failed to write repo-id file: {}", e))
            })?;
            Ok(repo_id)
        }
    }
}

/// Check if a repo ID is a legacy long hex string (e.g., 40-char SHA-1).
fn is_long_hex_id(id: &str) -> bool {
    id.len() > 8 && !id.starts_with("local-") && id.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_repo() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        fs::create_dir_all(temp_dir.path().join(".aiki")).unwrap();
        temp_dir
    }

    fn init_git_repo(path: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("Failed to init git repo");

        // Configure user for commits
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .expect("Failed to configure git email");
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(path)
            .output()
            .expect("Failed to configure git name");
    }

    fn create_commit(path: &Path, message: &str) {
        // Create a file to commit
        fs::write(path.join("file.txt"), message).unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .expect("Failed to git add");
        Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(path)
            .output()
            .expect("Failed to git commit");
    }

    #[test]
    fn test_repo_id_path() {
        let path = repo_id_path(Path::new("/foo/bar"));
        assert_eq!(path, Path::new("/foo/bar/.aiki/repo-id"));
    }

    #[test]
    fn test_read_repo_id_not_exists() {
        let temp_dir = setup_test_repo();
        let result = read_repo_id(temp_dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_read_repo_id_empty_file() {
        let temp_dir = setup_test_repo();
        fs::write(repo_id_path(temp_dir.path()), "").unwrap();
        let result = read_repo_id(temp_dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_read_repo_id_with_content() {
        let temp_dir = setup_test_repo();
        fs::write(repo_id_path(temp_dir.path()), "abc123\n").unwrap();
        let result = read_repo_id(temp_dir.path()).unwrap();
        assert_eq!(result, Some("abc123".to_string()));
    }

    #[test]
    fn test_compute_repo_id_no_git() {
        let temp_dir = setup_test_repo();
        // No git repo - should get local-* ID
        let id = compute_repo_id(temp_dir.path()).unwrap();
        assert!(
            id.starts_with("local-"),
            "Should be local-* without git: {}",
            id
        );
    }

    #[test]
    fn test_compute_repo_id_git_no_commits() {
        let temp_dir = setup_test_repo();
        init_git_repo(temp_dir.path());
        // Git repo with no commits - should get local-* ID
        let id = compute_repo_id(temp_dir.path()).unwrap();
        assert!(
            id.starts_with("local-"),
            "Should be local-* without commits: {}",
            id
        );
    }

    #[test]
    fn test_compute_repo_id_git_with_commit() {
        let temp_dir = setup_test_repo();
        init_git_repo(temp_dir.path());
        create_commit(temp_dir.path(), "Initial commit");

        let id = compute_repo_id(temp_dir.path()).unwrap();
        assert!(
            !id.starts_with("local-"),
            "Should be root hash with commits: {}",
            id
        );
        assert_eq!(id.len(), 8, "Truncated hash should be 8 chars: {}", id);
    }

    #[test]
    fn test_ensure_repo_id_creates_file() {
        let temp_dir = setup_test_repo();
        init_git_repo(temp_dir.path());
        create_commit(temp_dir.path(), "Initial commit");

        let id = ensure_repo_id(temp_dir.path()).unwrap();
        assert!(!id.starts_with("local-"));

        // Verify file was created
        let content = fs::read_to_string(repo_id_path(temp_dir.path())).unwrap();
        assert_eq!(content.trim(), id);
    }

    #[test]
    fn test_ensure_repo_id_idempotent() {
        let temp_dir = setup_test_repo();
        init_git_repo(temp_dir.path());
        create_commit(temp_dir.path(), "Initial commit");

        let id1 = ensure_repo_id(temp_dir.path()).unwrap();
        let id2 = ensure_repo_id(temp_dir.path()).unwrap();
        assert_eq!(id1, id2, "ensure_repo_id should be idempotent");
    }

    #[test]
    fn test_ensure_repo_id_preserves_existing() {
        let temp_dir = setup_test_repo();
        fs::write(repo_id_path(temp_dir.path()), "custom-id\n").unwrap();

        let id = ensure_repo_id(temp_dir.path()).unwrap();
        assert_eq!(id, "custom-id", "Should preserve existing ID");
    }

    #[test]
    fn test_ensure_repo_id_updates_empty_file() {
        let temp_dir = setup_test_repo();
        init_git_repo(temp_dir.path());
        create_commit(temp_dir.path(), "Initial commit");

        // Create empty file
        fs::write(repo_id_path(temp_dir.path()), "").unwrap();

        let id = ensure_repo_id(temp_dir.path()).unwrap();
        assert!(!id.is_empty());
        assert!(!id.starts_with("local-"));
    }

    #[test]
    fn test_local_id_is_deterministic() {
        let temp_dir = setup_test_repo();

        let id1 = compute_repo_id(temp_dir.path()).unwrap();
        let id2 = compute_repo_id(temp_dir.path()).unwrap();

        assert_eq!(id1, id2, "Local IDs should be deterministic for same path");
    }

    #[test]
    fn test_local_id_differs_by_path() {
        let temp_dir1 = setup_test_repo();
        let temp_dir2 = setup_test_repo();

        let id1 = compute_repo_id(temp_dir1.path()).unwrap();
        let id2 = compute_repo_id(temp_dir2.path()).unwrap();

        assert_ne!(id1, id2, "Local IDs should differ for different paths");
    }
}
