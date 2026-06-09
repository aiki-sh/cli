pub mod detector;
pub mod id;

// Re-export commonly used items for convenience
pub use detector::RepoDetector;
pub use id::{compute_repo_id, ensure_repo_id};

/// Get the repository folder name from a working directory.
///
/// Convenience wrapper around [`RepoDetector::repo_folder_name`].
#[allow(dead_code)]
pub fn repo_folder_name(cwd: &std::path::Path) -> String {
    RepoDetector::new(cwd).repo_folder_name()
}
