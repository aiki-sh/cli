use super::prelude::*;

/// A reference to a repo, used in repo.changed payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRef {
    /// Last path component of the repo root (e.g., "aiki")
    pub root: String,
    /// Full absolute path to the repo root
    pub path: PathBuf,
    /// Internal identifier from <repo>/.aiki/repo-id
    pub id: String,
}

impl RepoRef {
    /// Create a RepoRef with pre-resolved values (avoids file I/O).
    pub fn new(root: String, path: PathBuf, id: String) -> Self {
        Self { root, path, id }
    }

    /// Create a RepoRef by reading repo-id from disk.
    /// Use `new()` when you already have the repo_id to avoid the file read.
    pub fn from_path(repo_root: &std::path::Path) -> Self {
        let root = repo_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        let id = std::fs::read_to_string(repo_root.join(".aiki/repo-id"))
            .unwrap_or_else(|_| format!("local-{}", root))
            .trim()
            .to_string();
        Self::new(root, repo_root.to_path_buf(), id)
    }
}

/// repo.changed event payload
///
/// Fires when the engine detects the session has moved to a different JJ repo,
/// based on the repo root of the file being changed. Always fires before
/// change.completed for the triggering file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiRepoChangedPayload {
    pub session: AikiSession,
    pub cwd: PathBuf,
    pub timestamp: DateTime<Utc>,
    /// The repo the session moved into
    pub repo: RepoRef,
    /// The repo the session was previously in (None if no prior repo)
    pub previous_repo: Option<RepoRef>,
}

/// Handle repo.changed event
pub fn handle_repo_changed(payload: AikiRepoChangedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| {
        format!(
            "repo.changed event: session {} moved to repo {} ({})",
            payload.session.uuid(),
            payload.repo.root,
            payload.repo.id,
        )
    });

    let core_hook = crate::flows::load_core_hook();
    let mut state = AikiState::new(payload);

    let _flow_result = execute_hook(
        EventType::RepoChanged,
        &mut state,
        &core_hook.handlers.repo_changed,
    )?;

    let failures = state.take_failures();

    Ok(HookResult {
        context: None,
        decision: Decision::Allow,
        failures,
    })
}
