//! Workspace isolation for concurrent agent sessions
//!
//! When multiple agent sessions run concurrently in the same repo, they share
//! the same JJ workspace. This module provides isolated JJ workspaces per
//! session, with lazy creation (only when concurrent), automatic merge-back
//! at session end, and crash recovery.
//!
//! Workspace paths follow: `/tmp/aiki/<repo-id>/<session-id>/`

use crate::cache::debug_log;
use crate::error::{AikiError, Result};
use crate::global;
use crate::jj::{jj_cmd, JJWorkspace};
use crate::repos;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Base directory for isolated workspaces: `/tmp/aiki/`
///
/// Respects `AIKI_WORKSPACES_DIR` env var for testing.
pub fn workspaces_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("AIKI_WORKSPACES_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from("/tmp/aiki")
}

/// An isolated JJ workspace for a specific session/repo pair
#[derive(Debug, Clone)]
pub struct IsolatedWorkspace {
    /// Workspace name: "aiki-<session-id>"
    pub name: String,
    /// Workspace path: /tmp/aiki/<repo-id>/<session-id>/
    pub path: PathBuf,
    /// Project root this workspace belongs to
    pub repo_root: PathBuf,
    /// Session UUID that owns this workspace
    pub session_uuid: String,
}

/// Walk up from path looking for `.jj/` directory. Returns repo root or None.
///
/// Delegates to `JJWorkspace::find()` — does not reimplement the walk.
pub fn find_jj_root(path: &Path) -> Option<PathBuf> {
    JJWorkspace::find(path)
        .ok()
        .map(|ws| ws.workspace_root().to_path_buf())
}

/// Create an isolated JJ workspace for a repo/session pair.
///
/// Idempotent: returns existing workspace if directory already exists.
///
/// - workspace_name: "aiki-<session-id>"
/// - workspace_path: /tmp/aiki/<repo-id>/<session-id>/
/// - Forks from repo's main workspace @- (parent of working copy, starts clean)
pub fn create_isolated_workspace(
    repo_root: &Path,
    session_uuid: &str,
) -> Result<IsolatedWorkspace> {
    let repo_id = repos::ensure_repo_id(repo_root)?;

    let workspace_path = workspaces_dir()
        .join(&repo_id)
        .join(session_uuid);
    let workspace_name = format!("aiki-{}", session_uuid);

    let workspace = IsolatedWorkspace {
        name: workspace_name.clone(),
        path: workspace_path.clone(),
        repo_root: repo_root.to_path_buf(),
        session_uuid: session_uuid.to_string(),
    };

    // Idempotent: if workspace directory already exists, return it
    if workspace_path.exists() {
        debug_log(|| {
            format!(
                "Workspace already exists at {}, reusing",
                workspace_path.display()
            )
        });
        return Ok(workspace);
    }

    // Create parent directories
    if let Some(parent) = workspace_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            AikiError::WorkspaceCreationFailed(format!(
                "Failed to create workspace parent dirs: {}",
                e
            ))
        })?;
    }

    // Resolve default workspace parent explicitly to avoid ambiguous @
    // in multi-workspace contexts (where --ignore-working-copy can cause @- to
    // resolve to root() instead of the actual parent)
    let parent_output = jj_cmd()
        .current_dir(repo_root)
        .args([
            "log",
            "-r",
            "@-",
            "-T",
            "change_id",
            "--no-graph",
            "--limit",
            "1",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| {
            AikiError::WorkspaceCreationFailed(format!("Failed to resolve @-: {}", e))
        })?;

    let parent_change_id = if parent_output.status.success() {
        let id = String::from_utf8_lossy(&parent_output.stdout)
            .trim()
            .to_string();
        if id.is_empty() {
            "@-".to_string()
        } else {
            id
        }
    } else {
        "@-".to_string()
    };

    // Create workspace forked from the resolved parent
    let output = jj_cmd()
        .current_dir(repo_root)
        .args([
            "workspace",
            "add",
            &workspace_path.to_string_lossy(),
            "--name",
            &workspace_name,
            "-r",
            &parent_change_id,
        ])
        .output()
        .map_err(|e| {
            AikiError::WorkspaceCreationFailed(format!("Failed to run jj workspace add: {}", e))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::WorkspaceCreationFailed(format!(
            "jj workspace add failed: {}",
            stderr.trim()
        )));
    }

    debug_log(|| {
        format!(
            "Created isolated workspace '{}' at {}",
            workspace_name,
            workspace_path.display()
        )
    });

    Ok(workspace)
}

/// RAII guard that removes the lock file on drop.
struct AbsorbLock {
    path: PathBuf,
}

impl Drop for AbsorbLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Get the absorb lock file path for a repo.
fn absorb_lock_path(repo_root: &Path) -> Result<PathBuf> {
    let repo_id = repos::ensure_repo_id(repo_root)?;
    let lock_dir = workspaces_dir().join(&repo_id);
    let _ = fs::create_dir_all(&lock_dir);
    Ok(lock_dir.join(".absorb.lock"))
}

/// Acquire an exclusive file lock for workspace absorption.
///
/// Uses atomic file creation (O_CREAT|O_EXCL) via `hard_link` as a
/// cross-platform advisory lock. Retries with backoff for up to 30 seconds.
fn acquire_absorb_lock(lock_path: &Path) -> Result<AbsorbLock> {
    let max_wait = Duration::from_secs(30);
    let poll_interval = Duration::from_millis(100);
    let start = std::time::Instant::now();

    loop {
        // Try to atomically create the lock file.
        // OpenOptions with create_new is atomic (O_CREAT|O_EXCL).
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(_file) => {
                debug_log(|| format!("Acquired absorb lock at {}", lock_path.display()));
                return Ok(AbsorbLock {
                    path: lock_path.to_path_buf(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if start.elapsed() > max_wait {
                    // Stale lock — another agent may have crashed. Force-remove and retry.
                    debug_log(|| {
                        format!(
                            "Absorb lock timed out after {:?}, removing stale lock",
                            max_wait
                        )
                    });
                    let _ = fs::remove_file(lock_path);
                    continue;
                }
                std::thread::sleep(poll_interval);
            }
            Err(e) => {
                return Err(AikiError::WorkspaceAbsorbFailed(format!(
                    "Failed to acquire absorb lock: {}",
                    e
                )));
            }
        }
    }
}

/// Result of attempting to absorb a workspace
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbsorbResult {
    /// Workspace absorbed successfully
    Absorbed,
    /// Conflicts detected — workspace kept alive for agent resolution
    Conflicts {
        /// JJ change ID of the conflicted workspace change
        conflict_id: String,
        /// Files with unresolved conflicts
        conflicted_files: Vec<String>,
    },
    /// Nothing to absorb (workspace not found, empty, or root change)
    Skipped,
}

/// Absorb workspace changes into the target workspace.
///
/// Target is parent session's workspace if it exists, otherwise main.
///
/// Two-step rebase with file lock to safely chain multiple absorptions:
/// 1. Acquire absorb lock (serializes concurrent absorptions)
/// 2. Rebase workspace chain onto target's @- (inserts changes before @)
/// 3. Rebase target's @ onto workspace head (moves @ after the changes)
/// 4. Release lock
///
/// Why two steps: Workspaces may fork from different ancestors (because
/// workspace creation at different times sees different @-). A single
/// `jj rebase -b @ -d <ws_head>` drags intermediate default-workspace
/// ancestors along, cascading rewrites to sibling workspaces and creating
/// divergent changes. The two-step approach moves only workspace-specific
/// commits and then repositions @, avoiding cross-workspace rewrites.
///
/// Why a lock: Without serialization, concurrent step-2s (`-s @ -d <ws_head>`)
/// each move @ to their own target, disconnecting from previous absorptions.
/// The lock ensures absorptions chain correctly: each one builds on the last.
pub fn absorb_workspace(
    repo_root: &Path,
    workspace: &IsolatedWorkspace,
    parent_session_uuid: Option<&str>,
) -> Result<AbsorbResult> {
    // Get workspace working copy change ID by parsing `jj workspace list`
    // (workspace_id() revset doesn't exist in JJ 0.38)
    let ws_change_id = find_workspace_change_id(repo_root, &workspace.name)?;
    let ws_change_id = match ws_change_id {
        Some(id) => id,
        None => {
            debug_log(|| {
                format!(
                    "Workspace '{}' not found in jj workspace list, skipping absorb",
                    workspace.name
                )
            });
            return Ok(AbsorbResult::Skipped);
        }
    };

    // Snapshot workspace working copy to capture files written since last snapshot.
    // All subsequent JJ commands use --ignore-working-copy, so without this,
    // files written after the last implicit snapshot would be lost.
    let _ = jj_cmd()
        .current_dir(&workspace.path)
        .args(["debug", "snapshot"])
        .output();

    // Use the workspace's working copy (@) directly as the rebase target.
    // Previously this resolved @- (parent), which skipped all file changes
    // in the working copy commit.
    let ws_head = ws_change_id;

    // Guard against root/empty change heads — these indicate no real changes
    // were made in the workspace. JJ's root change ID is all zeros.
    if ws_head.chars().all(|c| c == '0') {
        debug_log(|| "Workspace head is root change, skipping absorb");
        return Ok(AbsorbResult::Skipped);
    }

    // Determine absorb target directory
    let target_dir = if let Some(parent_uuid) = parent_session_uuid {
        let repo_id = repos::ensure_repo_id(repo_root)
            .unwrap_or_default();
        let parent_ws_path = workspaces_dir()
            .join(&repo_id)
            .join(parent_uuid);
        if parent_ws_path.exists() {
            parent_ws_path
        } else {
            repo_root.to_path_buf()
        }
    } else {
        repo_root.to_path_buf()
    };

    // Step 0: Pre-rebase workspace onto target @- to detect conflicts early.
    // This runs OUTSIDE the absorb lock — only the fast-path steps 1+2 hold the lock.
    let target_at_minus = jj_cmd()
        .current_dir(&target_dir)
        .args([
            "log", "-r", "@-", "--no-graph", "-T", "change_id",
            "--limit", "1", "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| {
            AikiError::WorkspaceAbsorbFailed(format!("Failed to get target @-: {}", e))
        })?;
    let target_at_minus_id = String::from_utf8_lossy(&target_at_minus.stdout)
        .trim()
        .to_string();

    if !target_at_minus_id.is_empty() {
        // Rebase workspace chain onto target @- (pulls in other agents' absorbed changes)
        let rebase_output = jj_cmd()
            .current_dir(&workspace.path)
            .args([
                "rebase", "-b", &ws_head, "-d", &target_at_minus_id,
                "--ignore-working-copy",
            ])
            .output()
            .map_err(|e| {
                AikiError::WorkspaceAbsorbFailed(format!("Pre-rebase failed: {}", e))
            })?;

        if !rebase_output.status.success() {
            let stderr = String::from_utf8_lossy(&rebase_output.stderr);
            debug_log(|| format!("Pre-rebase warning: {}", stderr.trim()));
            // Non-fatal — continue with absorption attempt
        }

        // Snapshot to materialize any conflict markers in working copy
        let _ = jj_cmd()
            .current_dir(&workspace.path)
            .args(["debug", "snapshot"])
            .output();

        // Check for conflicts in workspace
        let conflict_check = jj_cmd()
            .current_dir(&workspace.path)
            .args(["resolve", "--list"])
            .output()
            .map_err(|e| {
                AikiError::WorkspaceAbsorbFailed(format!("Conflict check failed: {}", e))
            })?;
        let conflicts = String::from_utf8_lossy(&conflict_check.stdout);

        if !conflicts.trim().is_empty() {
            // Try auto-resolve for simple conflicts (append-only, non-overlapping)
            let _ = jj_cmd()
                .current_dir(&workspace.path)
                .args(["resolve", "--all"])
                .output();

            // Snapshot to materialize any resolution
            let _ = jj_cmd()
                .current_dir(&workspace.path)
                .args(["debug", "snapshot"])
                .output();

            // Re-check if conflicts remain after auto-resolve
            let recheck = jj_cmd()
                .current_dir(&workspace.path)
                .args(["resolve", "--list"])
                .output()
                .map_err(|e| {
                    AikiError::WorkspaceAbsorbFailed(format!(
                        "Conflict re-check failed: {}",
                        e
                    ))
                })?;
            let remaining = String::from_utf8_lossy(&recheck.stdout);

            if remaining.trim().is_empty() {
                // Auto-resolve fixed everything — fall through to normal absorption
                debug_log(|| "Auto-resolved all conflicts, continuing absorption");
            } else {
                // Parse conflicted files from remaining conflicts
                let conflicted_files: Vec<String> = remaining
                    .lines()
                    .filter_map(|l| l.split_whitespace().next())
                    .map(String::from)
                    .collect();

                // Conflicts detected — check retry count
                let retries_path = workspace.path.join(".conflict_retries");
                let conflict_retry_count = fs::read_to_string(&retries_path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .unwrap_or(0);

                if conflict_retry_count >= 3 {
                    // Force absorb — too many retries, let human resolve
                    debug_log(|| {
                        format!("Force-absorbing after {} retries", conflict_retry_count)
                    });
                    // Fall through to normal absorption
                } else {
                    // Increment retry count and return Conflicts
                    fs::write(&retries_path, (conflict_retry_count + 1).to_string())
                        .map_err(|e| {
                            AikiError::WorkspaceAbsorbFailed(format!(
                                "Failed to write retry count: {}",
                                e
                            ))
                        })?;
                    debug_log(|| {
                        format!(
                            "Conflicts detected (retry {}), deferring absorption",
                            conflict_retry_count + 1
                        )
                    });
                    return Ok(AbsorbResult::Conflicts {
                        conflict_id: ws_head.clone(),
                        conflicted_files,
                    });
                }
            }
        }
    }

    // Acquire file lock to serialize absorptions across concurrent agents.
    // Without this, concurrent absorptions interleave their two-step rebases,
    // causing each to disconnect from the previous absorption's changes.
    let lock_path = absorb_lock_path(repo_root)?;
    let _lock = acquire_absorb_lock(&lock_path)?;

    // Step 1: Rebase workspace chain onto target's @-
    //
    // Uses -b (branch) to move the entire workspace chain (from fork point to
    // ws_head) onto @-. This inserts the workspace's changes just before @ in
    // the graph. Uses --ignore-working-copy since we don't need to update the
    // filesystem yet (step 2 handles that).
    //
    // This is safe because the workspace chain contains only workspace-specific
    // commits (no shared ancestors), so -b doesn't cascade rewrites to other
    // workspaces.
    let output = jj_cmd()
        .current_dir(&target_dir)
        .args([
            "rebase", "-b", &ws_head, "-d", "@-",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| {
            AikiError::WorkspaceAbsorbFailed(format!(
                "Failed to rebase workspace chain onto @-: {}", e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::WorkspaceAbsorbFailed(format!(
            "jj rebase (step 1: workspace chain onto @-) failed: {}",
            stderr.trim()
        )));
    }

    // Step 2: Rebase target's @ onto workspace head
    //
    // Uses -s (source) to move only @ (a leaf node) onto ws_head, which is now
    // a descendant of @- (thanks to step 1). This completes the chain:
    //   @- → ws_changes → ws_head → @
    //
    // IMPORTANT: Do NOT use --ignore-working-copy here. JJ must update the
    // target's filesystem to reflect the new state. Without this, the next JJ
    // snapshot would see the workspace's files as "deleted" — silently reverting
    // the absorbed changes.
    let output = jj_cmd()
        .current_dir(&target_dir)
        .args(["rebase", "-s", "@", "-d", &ws_head])
        .output()
        .map_err(|e| {
            AikiError::WorkspaceAbsorbFailed(format!(
                "Failed to rebase @ onto workspace head: {}", e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::WorkspaceAbsorbFailed(format!(
            "jj rebase (step 2: @ onto ws_head) failed: {}",
            stderr.trim()
        )));
    }

    // Log any divergent-operation warning from stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        debug_log(|| format!("jj rebase stderr: {}", stderr.trim()));
    }

    debug_log(|| {
        format!(
            "Absorbed workspace '{}' into {}",
            workspace.name,
            target_dir.display()
        )
    });

    Ok(AbsorbResult::Absorbed)
}

/// Forget workspace in JJ and delete its directory.
pub fn cleanup_workspace(
    repo_root: &Path,
    workspace: &IsolatedWorkspace,
) -> Result<()> {
    // Forget the workspace in JJ
    let output = jj_cmd()
        .current_dir(repo_root)
        .args(["workspace", "forget", &workspace.name, "--ignore-working-copy"])
        .output()
        .map_err(|e| {
            AikiError::Other(anyhow::anyhow!(
                "Failed to forget workspace '{}': {}",
                workspace.name,
                e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        debug_log(|| format!("jj workspace forget warning: {}", stderr.trim()));
        // Don't fail — workspace might already be forgotten
    }

    // Remove the directory
    match fs::remove_dir_all(&workspace.path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            debug_log(|| {
                format!(
                    "Warning: failed to remove workspace dir {}: {}",
                    workspace.path.display(),
                    e
                )
            });
        }
    }

    // Clean up empty parent directory (e.g., /tmp/aiki/<repo-id>/)
    if let Some(parent) = workspace.path.parent() {
        if let Ok(entries) = fs::read_dir(parent) {
            if entries.count() == 0 {
                let _ = fs::remove_dir(parent);
            }
        }
    }

    debug_log(|| format!("Cleaned up workspace '{}'", workspace.name));
    Ok(())
}

/// Find and recover all workspaces for a dead session across all repos.
///
/// Scans `/tmp/aiki/*/<session-id>/` (where * is repo-id).
/// For each: absorb into main, then cleanup.
/// If absorb fails, creates a recovery bookmark and warns.
pub fn recover_orphaned_workspaces(session_uuid: &str) -> Result<u32> {
    let ws_dir = workspaces_dir();
    if !ws_dir.exists() {
        return Ok(0);
    }

    let mut recovered = 0u32;

    // Scan repo-id directories
    let entries = fs::read_dir(&ws_dir).map_err(|e| {
        AikiError::Other(anyhow::anyhow!(
            "Failed to read workspaces dir: {}",
            e
        ))
    })?;

    for entry in entries.flatten() {
        let repo_id_dir = entry.path();
        if !repo_id_dir.is_dir() {
            continue;
        }

        let session_ws_dir = repo_id_dir.join(session_uuid);
        if !session_ws_dir.exists() {
            continue;
        }

        let workspace_name = format!("aiki-{}", session_uuid);

        // Try to find the repo root from the workspace
        // The workspace contains a .jj/ that links back to the repo
        let repo_root = match find_repo_root_from_workspace(&session_ws_dir) {
            Some(root) => root,
            None => {
                eprintln!(
                    "[aiki] Warning: could not determine repo root for orphaned workspace at {}",
                    session_ws_dir.display()
                );
                // Clean up the directory even if we can't absorb
                let _ = fs::remove_dir_all(&session_ws_dir);
                // Clean up empty parent directory
                if let Ok(entries) = fs::read_dir(&repo_id_dir) {
                    if entries.count() == 0 {
                        let _ = fs::remove_dir(&repo_id_dir);
                    }
                }
                continue;
            }
        };

        let workspace = IsolatedWorkspace {
            name: workspace_name.clone(),
            path: session_ws_dir,
            repo_root: repo_root.clone(),
            session_uuid: session_uuid.to_string(),
        };

        // Try to absorb into main
        match absorb_workspace(&repo_root, &workspace, None) {
            Ok(AbsorbResult::Absorbed) => {
                recovered += 1;
            }
            Ok(AbsorbResult::Conflicts { .. }) => {
                // Orphaned workspace — force cleanup even with conflicts
                eprintln!(
                    "[aiki] Warning: orphaned workspace '{}' has conflicts, cleaning up anyway",
                    workspace_name
                );
            }
            Ok(AbsorbResult::Skipped) => {
                // Nothing to absorb
            }
            Err(e) => {
                // Fallback: create recovery bookmark
                eprintln!(
                    "[aiki] Warning: failed to absorb orphaned workspace '{}': {}",
                    workspace_name, e
                );
                // Try to create a recovery bookmark using workspace list parsing
                let bookmark_name = format!("aiki/recovered/{}", workspace_name);
                if let Ok(Some(ws_cid)) =
                    find_workspace_change_id(&repo_root, &workspace_name)
                {
                    let _ = jj_cmd()
                        .current_dir(&repo_root)
                        .args([
                            "bookmark",
                            "create",
                            &bookmark_name,
                            "-r",
                            &ws_cid,
                            "--ignore-working-copy",
                        ])
                        .output();
                    eprintln!(
                        "[aiki] Orphaned workspace had untagged changes at {}",
                        bookmark_name
                    );
                } else {
                    eprintln!(
                        "[aiki] Warning: could not find workspace '{}' in jj workspace list for recovery bookmark",
                        workspace_name
                    );
                }
            }
        }

        // Always clean up
        let _ = cleanup_workspace(&repo_root, &workspace);
    }

    Ok(recovered)
}

/// Clean up orphaned JJ workspaces that no longer have active sessions.
///
/// Scans `jj workspace list` for `aiki-*` entries, checks if each session
/// is still registered in by-repo/, and forgets workspaces for dead sessions.
/// This prevents the JJ workspace list from growing unbounded.
pub fn cleanup_orphaned_workspaces(repo_root: &Path) -> Result<u32> {
    let output = jj_cmd()
        .current_dir(repo_root)
        .args(["workspace", "list", "--ignore-working-copy"])
        .output()
        .map_err(|e| {
            AikiError::Other(anyhow::anyhow!("Failed to list workspaces: {}", e))
        })?;

    if !output.status.success() {
        return Ok(0);
    }

    let list_str = String::from_utf8_lossy(&output.stdout);
    let mut cleaned = 0u32;

    for line in list_str.lines() {
        // Match lines like "aiki-<uuid>: ..."
        let ws_name = match line.split(':').next() {
            Some(name) if name.starts_with("aiki-") => name.trim(),
            _ => continue,
        };

        // Extract UUID from workspace name "aiki-<uuid>"
        let uuid = &ws_name["aiki-".len()..];

        // Check if this session is still active (has a by-repo sidecar)
        if find_session_repo(uuid).is_some() {
            continue; // Session is still active, skip
        }

        // Session is dead — forget the workspace
        debug_log(|| {
            format!(
                "Forgetting orphaned workspace '{}' (no active session)",
                ws_name
            )
        });

        let forget_output = jj_cmd()
            .current_dir(repo_root)
            .args(["workspace", "forget", ws_name, "--ignore-working-copy"])
            .output();

        if let Ok(out) = forget_output {
            if out.status.success() {
                cleaned += 1;
            }
        }

        // Also clean up workspace directory if it exists
        if let Ok(repo_id) = crate::repos::ensure_repo_id(repo_root) {
            let ws_dir = workspaces_dir()
                .join(&repo_id)
                .join(uuid);
            if ws_dir.exists() {
                let _ = fs::remove_dir_all(&ws_dir);
            }
            // Clean up empty parent directory (e.g., /tmp/aiki/<repo-id>/)
            let repo_dir = workspaces_dir().join(&repo_id);
            if let Ok(entries) = fs::read_dir(&repo_dir) {
                if entries.count() == 0 {
                    let _ = fs::remove_dir(&repo_dir);
                }
            }
        }
    }

    if cleaned > 0 {
        debug_log(|| format!("Cleaned up {} orphaned workspace(s)", cleaned));
    }

    Ok(cleaned)
}

/// Count how many active sessions are using a specific repo.
///
/// Counts entries in `~/.aiki/sessions/by-repo/<repo-id>/` — O(1) directory listing
/// instead of scanning and parsing every session file.
pub fn count_sessions_in_repo(repo_id: &str) -> usize {
    let repo_dir = by_repo_dir().join(repo_id);
    match fs::read_dir(&repo_dir) {
        Ok(entries) => entries.count(),
        Err(_) => 0,
    }
}

/// Path to the by-repo sidecar directory.
fn by_repo_dir() -> PathBuf {
    global::global_aiki_dir()
        .join("sessions")
        .join("by-repo")
}

/// Register a session as active in a specific repo.
///
/// Creates an empty marker file at `~/.aiki/sessions/by-repo/<repo-id>/<session-id>`.
/// Idempotent — no-op if already registered.
pub fn register_session_in_repo(repo_id: &str, session_uuid: &str) {
    let sidecar = by_repo_dir().join(repo_id).join(session_uuid);
    if sidecar.exists() {
        return;
    }
    if let Some(parent) = sidecar.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&sidecar, "");
}

/// Unregister a session from a specific repo.
///
/// Removes `~/.aiki/sessions/by-repo/<repo-id>/<session-id>`.
/// Cleans up the repo-id directory if empty.
/// Idempotent — no-op if not registered.
pub fn unregister_session_from_repo(repo_id: &str, session_uuid: &str) {
    let repo_dir = by_repo_dir().join(repo_id);
    let sidecar = repo_dir.join(session_uuid);
    let _ = fs::remove_file(&sidecar);
    // Clean up empty repo directory
    if let Ok(entries) = fs::read_dir(&repo_dir) {
        if entries.count() == 0 {
            let _ = fs::remove_dir(&repo_dir);
        }
    }
}

/// Find which repo a session is currently registered in.
///
/// Scans `~/.aiki/sessions/by-repo/*/` for a file named `<session-id>`.
/// Returns the repo-id if found. Typically 1-2 directories to scan.
pub fn find_session_repo(session_uuid: &str) -> Option<String> {
    let base = by_repo_dir();
    let entries = fs::read_dir(&base).ok()?;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        if entry.path().join(session_uuid).exists() {
            return entry.file_name().to_str().map(String::from);
        }
    }
    None
}

/// Find the change ID for a named workspace by parsing `jj workspace list`.
///
/// Returns the short change ID of the workspace's working copy, or None if
/// the workspace is not found. This avoids using the `workspace_id()` revset
/// function which doesn't exist in JJ 0.38.
///
/// Output format: `workspace_name: <short_change_id> <commit_hash> ...`
pub fn find_workspace_change_id(repo_root: &Path, workspace_name: &str) -> Result<Option<String>> {
    let output = jj_cmd()
        .current_dir(repo_root)
        .args(["workspace", "list", "--ignore-working-copy"])
        .output()
        .map_err(|e| {
            AikiError::WorkspaceAbsorbFailed(format!("Failed to list workspaces: {}", e))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::WorkspaceAbsorbFailed(format!(
            "jj workspace list failed: {}",
            stderr.trim()
        )));
    }

    let list_str = String::from_utf8_lossy(&output.stdout);
    let prefix = format!("{}: ", workspace_name);

    let change_id = list_str
        .lines()
        .find(|line| line.starts_with(&prefix))
        .and_then(|line| {
            // After "workspace_name: ", first token is the short change ID
            line[prefix.len()..].trim().split_whitespace().next().map(String::from)
        });

    Ok(change_id)
}

/// Try to determine the repo root from a workspace directory.
///
/// JJ workspaces store their repo location in `.jj/repo`. In older JJ versions
/// this was a symlink; in JJ 0.38+ it's a plain text file containing the path.
/// We try both: read as text first (modern), then as symlink (legacy).
pub fn find_repo_root_from_workspace(workspace_path: &Path) -> Option<PathBuf> {
    let repo_link = workspace_path.join(".jj").join("repo");

    // Modern JJ (0.38+): .jj/repo is a plain text file containing the repo path
    // e.g., "/Users/glasner/code/aiki/.jj/repo"
    if let Ok(contents) = fs::read_to_string(&repo_link) {
        let target = PathBuf::from(contents.trim());
        // The path points to <original_repo>/.jj/repo — walk up to repo root
        if let Some(jj_dir) = target.parent() {
            if let Some(repo_root) = jj_dir.parent() {
                return Some(repo_root.to_path_buf());
            }
        }
    }

    // Legacy JJ: .jj/repo is a symlink to <original_repo>/.jj/repo
    if let Ok(target) = fs::read_link(&repo_link) {
        if let Some(jj_dir) = target.parent() {
            if let Some(repo_root) = jj_dir.parent() {
                return Some(repo_root.to_path_buf());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to serialize tests that modify AIKI_HOME env var
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper to run a test with a temporary AIKI_HOME value.
    /// Serializes access to prevent parallel test interference.
    fn with_aiki_home<F, R>(temp_dir: &std::path::Path, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::var("AIKI_HOME").ok();
        std::env::set_var("AIKI_HOME", temp_dir);
        let result = f();
        match original {
            Some(v) => std::env::set_var("AIKI_HOME", v),
            None => std::env::remove_var("AIKI_HOME"),
        }
        result
    }

    #[test]
    fn test_find_jj_root_with_jj_dir() {
        let temp_dir = tempfile::tempdir().unwrap();
        let jj_dir = temp_dir.path().join(".jj");
        fs::create_dir(&jj_dir).unwrap();

        let nested = temp_dir.path().join("src").join("nested");
        fs::create_dir_all(&nested).unwrap();

        let result = find_jj_root(&nested);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().canonicalize().unwrap(),
            temp_dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn test_find_jj_root_not_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        let result = find_jj_root(temp_dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn test_count_sessions_in_repo_empty() {
        let temp_dir = tempfile::tempdir().unwrap();
        with_aiki_home(temp_dir.path(), || {
            let count = count_sessions_in_repo("test-repo-id");
            assert_eq!(count, 0);
        });
    }

    #[test]
    fn test_count_sessions_in_repo_with_sessions() {
        let temp_dir = tempfile::tempdir().unwrap();
        with_aiki_home(temp_dir.path(), || {
            register_session_in_repo("test-repo-id", "session-1");
            register_session_in_repo("test-repo-id", "session-2");
            register_session_in_repo("other-repo", "session-3");

            assert_eq!(count_sessions_in_repo("test-repo-id"), 2);
            assert_eq!(count_sessions_in_repo("other-repo"), 1);
        });
    }

    #[test]
    fn test_register_session_in_repo() {
        let temp_dir = tempfile::tempdir().unwrap();
        with_aiki_home(temp_dir.path(), || {
            register_session_in_repo("repo-1", "session-abc");

            let sidecar = temp_dir.path().join("sessions/by-repo/repo-1/session-abc");
            assert!(sidecar.exists());

            // Idempotent — second call is a no-op
            register_session_in_repo("repo-1", "session-abc");
            assert!(sidecar.exists());
        });
    }

    #[test]
    fn test_unregister_session_from_repo() {
        let temp_dir = tempfile::tempdir().unwrap();
        with_aiki_home(temp_dir.path(), || {
            register_session_in_repo("repo-1", "session-abc");
            let sidecar = temp_dir.path().join("sessions/by-repo/repo-1/session-abc");
            assert!(sidecar.exists());

            unregister_session_from_repo("repo-1", "session-abc");
            assert!(!sidecar.exists());
            // Empty repo dir should be cleaned up
            assert!(!temp_dir.path().join("sessions/by-repo/repo-1").exists());

            // Idempotent — second call is a no-op
            unregister_session_from_repo("repo-1", "session-abc");
        });
    }

    #[test]
    fn test_find_session_repo() {
        let temp_dir = tempfile::tempdir().unwrap();
        with_aiki_home(temp_dir.path(), || {
            // No sidecars → None
            assert_eq!(find_session_repo("session-abc"), None);

            // Register and find
            register_session_in_repo("repo-1", "session-abc");
            assert_eq!(find_session_repo("session-abc"), Some("repo-1".to_string()));

            // Different session → None
            assert_eq!(find_session_repo("session-xyz"), None);

            // Unregister → None
            unregister_session_from_repo("repo-1", "session-abc");
            assert_eq!(find_session_repo("session-abc"), None);
        });
    }

    #[test]
    fn test_find_repo_root_from_workspace_text_file() {
        // Simulate modern JJ (0.38+): .jj/repo is a plain text file
        let temp_dir = tempfile::tempdir().unwrap();
        let fake_repo_root = temp_dir.path().join("my-project");
        let fake_jj_repo = fake_repo_root.join(".jj").join("repo");
        fs::create_dir_all(&fake_jj_repo).unwrap();

        // Create a workspace directory with .jj/repo as a text file
        let workspace_dir = temp_dir.path().join("workspace");
        let ws_jj_dir = workspace_dir.join(".jj");
        fs::create_dir_all(&ws_jj_dir).unwrap();
        fs::write(
            ws_jj_dir.join("repo"),
            fake_jj_repo.to_string_lossy().as_ref(),
        )
        .unwrap();

        let result = find_repo_root_from_workspace(&workspace_dir);
        assert_eq!(result, Some(fake_repo_root));
    }

    #[test]
    fn test_find_repo_root_from_workspace_symlink() {
        // Simulate legacy JJ: .jj/repo is a symlink
        let temp_dir = tempfile::tempdir().unwrap();
        let fake_repo_root = temp_dir.path().join("my-project");
        let fake_jj_repo = fake_repo_root.join(".jj").join("repo");
        fs::create_dir_all(&fake_jj_repo).unwrap();

        let workspace_dir = temp_dir.path().join("workspace");
        let ws_jj_dir = workspace_dir.join(".jj");
        fs::create_dir_all(&ws_jj_dir).unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(&fake_jj_repo, ws_jj_dir.join("repo")).unwrap();

        #[cfg(unix)]
        {
            let result = find_repo_root_from_workspace(&workspace_dir);
            assert_eq!(result, Some(fake_repo_root));
        }
    }

    #[test]
    fn test_find_repo_root_from_workspace_missing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let result = find_repo_root_from_workspace(temp_dir.path());
        assert_eq!(result, None);
    }

    #[test]
    fn test_workspaces_dir_default() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::var("AIKI_WORKSPACES_DIR").ok();
        std::env::remove_var("AIKI_WORKSPACES_DIR");

        let dir = workspaces_dir();
        assert_eq!(dir, PathBuf::from("/tmp/aiki"));

        if let Some(v) = original {
            std::env::set_var("AIKI_WORKSPACES_DIR", v);
        }
    }

    #[test]
    fn test_workspaces_dir_override() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::var("AIKI_WORKSPACES_DIR").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", "/custom/workspaces");

        let dir = workspaces_dir();
        assert_eq!(dir, PathBuf::from("/custom/workspaces"));

        match original {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
    }
}
