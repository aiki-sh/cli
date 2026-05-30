pub mod detector;
pub mod id;

// Re-export commonly used items for convenience
pub use detector::RepoDetector;
pub use id::{
    compute_repo_id, ensure_repo_id, read_repo_id, repo_id_path, try_upgrade_repo_id,
    REPO_ID_FILENAME,
};
