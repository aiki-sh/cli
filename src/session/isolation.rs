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
use crate::jj::{jj_cmd, JJWorkspace};
use crate::repos;
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

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
/// If the workspace already exists (surviving from a previous turn), rebases it
/// to the current `@-` to pick up changes absorbed by other sessions. On rebase
/// failure, destroys and recreates the workspace.
///
/// - workspace_name: "aiki-<session-id>"
/// - workspace_path: /tmp/aiki/<repo-id>/<session-id>/
/// - Forks from repo's main workspace @- (parent of working copy, starts clean)
pub fn create_isolated_workspace(
    repo_root: &Path,
    session_uuid: &str,
) -> Result<IsolatedWorkspace> {
    let repo_id = repos::ensure_repo_id(repo_root)?;

    let workspace_path = workspaces_dir().join(&repo_id).join(session_uuid);
    let workspace_name = format!("aiki-{}", session_uuid);

    let workspace = IsolatedWorkspace {
        name: workspace_name.clone(),
        path: workspace_path.clone(),
    };

    // Workspace survived from previous turn — rebase to current fork point
    // so it picks up other sessions' absorbed changes.
    //
    // IMPORTANT: Do NOT use --ignore-working-copy here. JJ must update the
    // filesystem to reflect changes absorbed by concurrent sessions. Without
    // this, the next snapshot would see stale files and create a diff that
    // reverts other sessions' absorbed changes.
    if workspace_path.exists() {
        match resolve_at_minus(repo_root) {
            Ok(target) => {
                match resolve_at_minus_in_path(&workspace_path) {
                    Ok(workspace_parent) => {
                        match lineage_contains_change(repo_root, &workspace_parent, &target) {
                            Ok(true) => {
                                let output = jj_cmd()
                                    .current_dir(&workspace_path)
                                    .args(["rebase", "-r", "@", "-d", &target])
                                    .output();

                                match output {
                                    Ok(o) if o.status.success() => {
                                        debug_log(|| {
                                            format!(
                                                "[workspace] Rebased existing workspace to {}",
                                                &target[..target.len().min(12)]
                                            )
                                        });
                                        return Ok(workspace);
                                    }
                                    _ => {
                                        // Rebase failed — fall through to destroy + recreate
                                        debug_log(|| {
                                            "[workspace] Rebase failed, recreating workspace"
                                                .to_string()
                                        });
                                        cleanup_workspace(repo_root, &workspace)?;
                                    }
                                }
                            }
                            Ok(false) => {
                                debug_log(|| {
                                    "[workspace] Workspace lineage diverged from current @-, recreating workspace".to_string()
                                });
                                cleanup_workspace(repo_root, &workspace)?;
                            }
                            Err(e) => {
                                debug_log(|| format!("[workspace] Failed ancestry check: {e}"));
                                cleanup_workspace(repo_root, &workspace)?;
                            }
                        }
                    }
                    Err(_) => {
                        // Can't resolve workspace @- — fall through to destroy + recreate
                        debug_log(|| {
                            "[workspace] Could not resolve workspace @-, recreating workspace"
                                .to_string()
                        });
                        cleanup_workspace(repo_root, &workspace)?;
                    }
                }
            }
            Err(_) => {
                // Can't resolve @- — fall through to destroy + recreate
                debug_log(|| "[workspace] Could not resolve @-, recreating workspace".to_string());
                cleanup_workspace(repo_root, &workspace)?;
            }
        }
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
        .map_err(|e| AikiError::WorkspaceCreationFailed(format!("Failed to resolve @-: {}", e)))?;

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

/// Wrapper for `fd_lock::RwLock<File>` enabling interior mutability in a static cache.
///
/// `fd_lock::RwLock::write()` requires `&mut self`, but the underlying `flock(2)`
/// system call is inherently thread-safe. This wrapper uses `UnsafeCell` to provide
/// the required interior mutability for cached lock instances.
struct CachedLock(UnsafeCell<fd_lock::RwLock<std::fs::File>>);

// SAFETY: The underlying flock(2) is thread-safe. In practice, concurrent access
// to the same CachedLock is prevented by callers serializing through higher-level
// locks (e.g., the workspace-absorption lock).
unsafe impl Sync for CachedLock {}

/// Cache of leaked `RwLock<File>` instances keyed by lock-file path.
///
/// Ensures that repeated calls to `acquire_named_lock` with the same name
/// return the same `&'static RwLock<File>` instead of creating duplicate FDs.
static LOCK_CACHE: LazyLock<Mutex<HashMap<PathBuf, &'static CachedLock>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Acquire a named file lock for the given repo.
///
/// Uses OS-level `flock(2)` via `fd-lock`. The lock is automatically
/// released when the returned guard drops — even on panic or SIGKILL.
/// Blocks until the lock is available (no timeout, no polling).
///
/// Lock instances are cached per path — subsequent calls with the same name
/// return the same underlying lock, preventing duplicate FDs and ensuring
/// mutual exclusion within the process.
pub fn acquire_named_lock(
    repo_root: &Path,
    name: &str,
) -> Result<fd_lock::RwLockWriteGuard<'static, std::fs::File>> {
    let repo_id = repos::ensure_repo_id(repo_root)?;
    let lock_dir = workspaces_dir().join(&repo_id);
    fs::create_dir_all(&lock_dir)
        .map_err(|e| AikiError::LockFailed(format!("Failed to create lock directory: {e}")))?;
    let lock_path = lock_dir.join(format!(".{}.lock", name));

    let cached: &'static CachedLock = {
        let mut cache = LOCK_CACHE.lock().unwrap();
        if let Some(&existing) = cache.get(&lock_path) {
            existing
        } else {
            let file = std::fs::File::create(&lock_path)
                .map_err(|e| AikiError::LockFailed(format!("Failed to create lock file: {}", e)))?;
            let leaked: &'static CachedLock = Box::leak(Box::new(CachedLock(UnsafeCell::new(
                fd_lock::RwLock::new(file),
            ))));
            cache.insert(lock_path.clone(), leaked);
            leaked
        }
    };

    // SAFETY: The pointer from UnsafeCell::get() points to Box::leaked memory
    // valid for 'static. The underlying flock(2) call is thread-safe, and in
    // practice callers serialize access through higher-level locks.
    let lock: &'static mut fd_lock::RwLock<std::fs::File> = unsafe { &mut *cached.0.get() };

    let guard = lock
        .write()
        .map_err(|e| AikiError::LockFailed(format!("Failed to acquire {} lock: {}", name, e)))?;

    debug_log(|| format!("Acquired '{}' lock at {}", name, lock_path.display()));
    Ok(guard)
}

/// Result of attempting to absorb a workspace
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbsorbResult {
    /// Workspace absorbed successfully
    Absorbed,
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
    // Uses `jj status` (which triggers a snapshot as a side effect) instead of
    // `jj debug snapshot` to avoid unstable API.
    let _ = jj_cmd()
        .current_dir(&workspace.path)
        .args(["status"])
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
        let repo_id = repos::ensure_repo_id(repo_root).unwrap_or_default();
        let parent_ws_path = workspaces_dir().join(&repo_id).join(parent_uuid);
        if parent_ws_path.exists() {
            parent_ws_path
        } else {
            repo_root.to_path_buf()
        }
    } else {
        repo_root.to_path_buf()
    };

    // Acquire file lock to serialize absorptions across concurrent agents.
    // Without this, concurrent absorptions interleave their two-step rebases,
    // causing each to disconnect from the previous absorption's changes.
    let _lock = acquire_named_lock(repo_root, "workspace-absorption")?;

    // Snapshot target working copy while holding the absorption lock.
    // Without this, changes made in the target workspace while an agent is
    // working in an isolated one are not captured into @'s committed tree.
    let _ = jj_cmd().current_dir(&target_dir).args(["status"]).output();

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
            "rebase",
            "-b",
            &ws_head,
            "-d",
            "@-",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| {
            AikiError::WorkspaceAbsorbFailed(format!(
                "Failed to rebase workspace chain onto @-: {}",
                e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::WorkspaceAbsorbFailed(format!(
            "jj rebase (step 1: workspace chain onto @-) failed: {}",
            stderr.trim()
        )));
    }

    // Idempotency guard: After step 1, check if ws_head is already an ancestor
    // of @. This happens when the same workspace is absorbed twice (e.g., from
    // both turn.completed and session.ended). In that case, step 1 was a no-op
    // and step 2 would move @ BACKWARD, orphaning changes absorbed between the
    // two calls. Skip step 2 to prevent silent data loss.
    //
    // Uses the revset `ws_head & ::@` — if ws_head appears in @'s ancestors,
    // it was already absorbed. On first absorption, ws_head is a sibling of @
    // (not an ancestor), so this correctly allows step 2 to proceed.
    let ancestor_check = jj_cmd()
        .current_dir(&target_dir)
        .args([
            "log",
            "-r",
            &format!("{} & ::@", ws_head),
            "--no-graph",
            "-T",
            "change_id",
            "--limit",
            "1",
            "--ignore-working-copy",
        ])
        .output();

    if let Ok(check_output) = ancestor_check {
        if check_output.status.success() {
            let already_ancestor = String::from_utf8_lossy(&check_output.stdout);
            if !already_ancestor.trim().is_empty() {
                // ws_head is already an ancestor of @ — this workspace was
                // already absorbed. Skip step 2 to avoid moving @ backward.
                debug_log(|| {
                    format!(
                        "Workspace '{}' ws_head {} is already an ancestor of @ — \
                         skipping step 2 (already absorbed)",
                        workspace.name, ws_head
                    )
                });
                return Ok(AbsorbResult::Skipped);
            }
        }
    }

    // Step 2: Rebase target's @ onto workspace head
    //
    // Uses -s (source) to move only @ (a leaf node) onto ws_head, which is now
    // a descendant of @- (thanks to step 1). This completes the chain:
    //   @- → ws_changes → ws_head → @
    //
    // Uses --ignore-working-copy (matching step 1) because JJ's working-copy
    // tracking is stale after step 1's rebase. We use `workspace update-stale`
    // after this to sync the filesystem.
    let output = jj_cmd()
        .current_dir(&target_dir)
        .args(["rebase", "-s", "@", "-d", &ws_head, "--ignore-working-copy"])
        .output()
        .map_err(|e| {
            AikiError::WorkspaceAbsorbFailed(format!(
                "Failed to rebase @ onto workspace head: {}",
                e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::WorkspaceAbsorbFailed(format!(
            "jj rebase (step 2: @ onto ws_head) failed: {}",
            stderr.trim()
        )));
    }

    // Post-absorption safety check: verify ws_head is in @'s ancestry.
    // If not, a concurrent absorption (or hook-created `jj new` between turns)
    // stranded our commits on a side branch. Fix by rebasing @ onto ws_head.
    let verify_check = jj_cmd()
        .current_dir(&target_dir)
        .args([
            "log",
            "-r",
            &format!("{} & ::@", ws_head),
            "--no-graph",
            "-T",
            "change_id",
            "--limit",
            "1",
            "--ignore-working-copy",
        ])
        .output();

    if let Ok(verify_output) = verify_check {
        if verify_output.status.success() {
            let in_ancestry = String::from_utf8_lossy(&verify_output.stdout);
            if in_ancestry.trim().is_empty() {
                // ws_head is NOT in @'s ancestry — stranded! Fix it.
                debug_log(|| {
                    format!(
                        "[workspace] Post-absorption: ws_head {} stranded \
                         (not in ::@), rebasing @ onto ws_head to fix",
                        &ws_head[..ws_head.len().min(12)]
                    )
                });
                let fix_output = jj_cmd()
                    .current_dir(&target_dir)
                    .args(["rebase", "-s", "@", "-d", &ws_head, "--ignore-working-copy"])
                    .output();
                match fix_output {
                    Ok(fo) if !fo.status.success() => {
                        let stderr = String::from_utf8_lossy(&fo.stderr);
                        eprintln!(
                            "[aiki] WARNING: post-absorption fix rebase failed: {}",
                            stderr.trim()
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "[aiki] WARNING: post-absorption fix rebase failed to run: {}",
                            e
                        );
                    }
                    _ => {
                        debug_log(|| {
                            "[workspace] Post-absorption fix: rebased @ onto ws_head".to_string()
                        });
                    }
                }
            }
        }
    }

    // Sync the working copy after both rebases used --ignore-working-copy.
    // Without this, the filesystem would be stale and the next snapshot would
    // see the workspace's files as "deleted" — silently reverting the absorbed
    // changes.
    let update_output = jj_cmd()
        .current_dir(&target_dir)
        .args(["workspace", "update-stale"])
        .output();

    match update_output {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "[aiki] WARNING: workspace update-stale failed after absorption — \
                 filesystem may be stale. Run `jj workspace update-stale` manually.\n\
                 stderr: {}",
                stderr.trim()
            );
        }
        Err(e) => {
            eprintln!(
                "[aiki] WARNING: workspace update-stale failed to execute after \
                 absorption: {} — filesystem may be stale.",
                e
            );
        }
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
pub fn cleanup_workspace(repo_root: &Path, workspace: &IsolatedWorkspace) -> Result<()> {
    // Forget the workspace in JJ
    let output = jj_cmd()
        .current_dir(repo_root)
        .args([
            "workspace",
            "forget",
            &workspace.name,
            "--ignore-working-copy",
        ])
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
    let entries = fs::read_dir(&ws_dir)
        .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to read workspaces dir: {}", e)))?;

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
                continue;
            }
        };

        let workspace = IsolatedWorkspace {
            name: workspace_name.clone(),
            path: session_ws_dir,
        };

        // Try to absorb into main
        match absorb_workspace(&repo_root, &workspace, None) {
            Ok(AbsorbResult::Absorbed) => {
                recovered += 1;
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
                if let Ok(Some(ws_cid)) = find_workspace_change_id(&repo_root, &workspace_name) {
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
/// is still backed by a live session file in `~/.aiki/sessions/{uuid}`,
/// and forgets workspaces for dead sessions. This prevents the JJ workspace
/// list from growing unbounded.
pub fn cleanup_orphaned_workspaces(repo_root: &Path) -> Result<u32> {
    let output = jj_cmd()
        .current_dir(repo_root)
        .args(["workspace", "list", "--ignore-working-copy"])
        .output()
        .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to list workspaces: {}", e)))?;

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

        // Check if this session is still active (has a session file)
        if crate::global::global_sessions_dir().join(uuid).exists() {
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
            let ws_dir = workspaces_dir().join(&repo_id).join(uuid);
            if ws_dir.exists() {
                let _ = fs::remove_dir_all(&ws_dir);
            }
        }
    }

    if cleaned > 0 {
        debug_log(|| format!("Cleaned up {} orphaned workspace(s)", cleaned));
    }

    Ok(cleaned)
}

/// Find the full change ID for a named workspace.
///
/// Returns the workspace's full change ID, or None if the workspace is not
/// found. We parse `jj workspace list` to identify the workspace, then resolve
/// its short ID to full to avoid short-ID ambiguity.
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

    let short_change_id = match list_str
        .lines()
        .find(|line| line.starts_with(&prefix))
        .and_then(|line| {
            // After "workspace_name: ", first token is the short change ID
            line[prefix.len()..]
                .trim()
                .split_whitespace()
                .next()
                .map(String::from)
        }) {
        Some(id) => id,
        None => return Ok(None),
    };

    let output = jj_cmd()
        .current_dir(repo_root)
        .args([
            "log",
            "-r",
            &short_change_id,
            "--no-graph",
            "-T",
            "change_id",
            "--limit",
            "1",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| {
            AikiError::WorkspaceAbsorbFailed(format!(
                "Failed to resolve workspace `{}` short id `{}`: {}",
                workspace_name, short_change_id, e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::WorkspaceAbsorbFailed(format!(
            "Failed to resolve workspace `{}` short id `{}` to full id: {}",
            workspace_name,
            short_change_id,
            stderr.trim()
        )));
    }

    let change_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if change_id.is_empty() {
        return Ok(None);
    }

    Ok(Some(change_id))
}

/// Try to determine the repo root from a workspace directory.
///
/// JJ workspaces store their repo location in `.jj/repo`. In older JJ versions
/// this was a symlink; in JJ 0.38+ it's a plain text file containing the path.
/// We try both: read as text first (modern), then as symlink (legacy).
pub fn find_repo_root_from_workspace(workspace_path: &Path) -> Option<PathBuf> {
    let jj_dir = workspace_path.join(".jj");
    let repo_link = jj_dir.join("repo");

    // Modern JJ (0.38+): .jj/repo is a plain text file containing the repo path.
    // JJ writes relative paths (e.g., "../../../../../../Users/glasner/code/aiki/.jj/repo")
    // which must be resolved relative to the workspace's .jj/ directory.
    if let Ok(contents) = fs::read_to_string(&repo_link) {
        let target = PathBuf::from(contents.trim());
        let target = if target.is_relative() {
            match jj_dir.join(&target).canonicalize() {
                Ok(resolved) => resolved,
                Err(_) => return None,
            }
        } else {
            target
        };
        // The path points to <original_repo>/.jj/repo — walk up to repo root
        if let Some(jj_parent) = target.parent() {
            if let Some(repo_root) = jj_parent.parent() {
                return Some(repo_root.to_path_buf());
            }
        }
    }

    // Legacy JJ: .jj/repo is a symlink to <original_repo>/.jj/repo
    if let Ok(target) = fs::read_link(&repo_link) {
        let target = if target.is_relative() {
            match jj_dir.join(&target).canonicalize() {
                Ok(resolved) => resolved,
                Err(_) => return None,
            }
        } else {
            target
        };
        if let Some(jj_parent) = target.parent() {
            if let Some(repo_root) = jj_parent.parent() {
                return Some(repo_root.to_path_buf());
            }
        }
    }

    None
}

/// Resolve the current `@-` (parent of main workspace's working copy) change ID.
///
/// Used when reusing an existing workspace — rebase it to the current `@-`
/// to pick up changes absorbed by other sessions since the last turn.
fn resolve_at_minus(repo_root: &Path) -> Result<String> {
    resolve_at_minus_in_path(repo_root)
}

/// Resolve the current `@-` (parent of a workspace's working copy) change ID.
fn resolve_at_minus_in_path(path: &Path) -> Result<String> {
    let output = jj_cmd()
        .current_dir(path)
        .args([
            "log",
            "-r",
            "@-",
            "--no-graph",
            "-T",
            "change_id",
            "--ignore-working-copy",
            "--limit",
            "1",
        ])
        .output()
        .map_err(|e| {
            AikiError::Other(anyhow::anyhow!(
                "Failed to run jj log for @- in {}: {}",
                path.display(),
                e
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::Other(anyhow::anyhow!(
            "jj log -r @- failed in {}: {}",
            path.display(),
            stderr.trim()
        )));
    }
    let change_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if change_id.is_empty() {
        return Err(AikiError::Other(anyhow::anyhow!(
            "jj log -r @- returned empty output in {}",
            path.display()
        )));
    }
    Ok(change_id)
}

/// Check whether a workspace parent is still an ancestor of the current `@-` chain.
///
/// Uses an explicit ancestry query rather than trusting rebase alone. If the
/// revset is empty, the workspace likely forked from a diverged branch.
fn lineage_contains_change(
    repo_root: &Path,
    workspace_parent: &str,
    current_parent: &str,
) -> Result<bool> {
    let revset = format!("{}::{}", workspace_parent, current_parent);
    let output = jj_cmd()
        .current_dir(repo_root)
        .args([
            "log",
            "-r",
            &revset,
            "--no-graph",
            "--limit",
            "1",
            "-T",
            "change_id",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| {
            AikiError::Other(anyhow::anyhow!(
                "Failed to run jj log for lineage check {} -> {}: {}",
                workspace_parent,
                current_parent,
                e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::Other(anyhow::anyhow!(
            "jj log -r {} failed in {}: {}",
            revset,
            repo_root.display(),
            stderr.trim()
        )));
    }

    let ancestry = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(!ancestry.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use the process-wide mutex from global.rs to avoid races with other modules
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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
    fn test_find_repo_root_from_workspace_relative_path() {
        // JJ writes relative paths in .jj/repo when creating workspaces.
        // The function must resolve these relative to the workspace's .jj/
        // directory, not the process CWD.
        let temp_dir = tempfile::tempdir().unwrap();
        let fake_repo_root = temp_dir.path().join("my-project");
        let fake_jj_repo = fake_repo_root.join(".jj").join("repo");
        fs::create_dir_all(&fake_jj_repo).unwrap();

        let workspace_dir = temp_dir.path().join("workspaces").join("session-1");
        let ws_jj_dir = workspace_dir.join(".jj");
        fs::create_dir_all(&ws_jj_dir).unwrap();

        // Write a relative path like JJ does (from .jj/ up to temp_dir, then down)
        fs::write(ws_jj_dir.join("repo"), "../../../my-project/.jj/repo").unwrap();

        let result = find_repo_root_from_workspace(&workspace_dir);
        assert!(result.is_some(), "Should resolve relative .jj/repo path");
        assert_eq!(
            result.unwrap().canonicalize().unwrap(),
            fake_repo_root.canonicalize().unwrap()
        );
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
        let _lock = env_lock();
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
        let _lock = env_lock();
        let original = std::env::var("AIKI_WORKSPACES_DIR").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", "/custom/workspaces");

        let dir = workspaces_dir();
        assert_eq!(dir, PathBuf::from("/custom/workspaces"));

        match original {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
    }

    /// Helper: write a file and describe it in a JJ workspace
    fn jj_write_and_describe(path: &Path, filename: &str, content: &str, desc: &str) {
        use std::process::Command;
        fs::write(path.join(filename), content).unwrap();
        Command::new("jj")
            .args(["describe", "-m", desc])
            .current_dir(path)
            .output()
            .expect("jj describe failed");
    }

    /// Helper: create a JJ repo in a temp dir, make an initial commit,
    /// and set up AIKI_WORKSPACES_DIR. Returns (repo_root, workspaces_dir).
    fn setup_jj_repo() -> (tempfile::TempDir, tempfile::TempDir) {
        use std::process::Command;

        let repo_dir = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();

        let output = Command::new("jj")
            .args(["git", "init", "--colocate"])
            .current_dir(repo_dir.path())
            .output()
            .expect("jj git init failed");
        assert!(
            output.status.success(),
            "jj git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        fs::write(repo_dir.path().join("init.txt"), "initial").unwrap();
        Command::new("jj")
            .args(["describe", "-m", "initial content"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
        Command::new("jj")
            .args(["new"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();

        let aiki_dir = repo_dir.path().join(".aiki");
        fs::create_dir_all(&aiki_dir).unwrap();
        fs::write(aiki_dir.join("repo-id"), "testrepo\n").unwrap();

        (repo_dir, ws_dir)
    }

    /// Regression test for fix-absorbtion-on-multiple-runs.md:
    /// Two sequential absorptions into default@ must both survive on disk.
    #[test]
    fn test_sequential_absorptions_preserve_earlier_changes() {
        use std::process::Command;

        let _lock = env_lock();
        let (repo_dir, ws_dir) = setup_jj_repo();
        let original = std::env::var("AIKI_WORKSPACES_DIR").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", ws_dir.path());

        // Simulate session.started hook: jj new --ignore-working-copy
        Command::new("jj")
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();

        // --- Session A ---
        let ws_a = create_isolated_workspace(repo_dir.path(), "session-a").unwrap();
        jj_write_and_describe(&ws_a.path, "file_a.txt", "changes from session A", "[aiki] session A");

        let result_a = absorb_workspace(repo_dir.path(), &ws_a, None);
        assert!(
            matches!(result_a, Ok(AbsorbResult::Absorbed)),
            "Session A absorption should succeed, got: {:?}",
            result_a
        );
        cleanup_workspace(repo_dir.path(), &ws_a).unwrap();

        let file_a_path = repo_dir.path().join("file_a.txt");
        assert!(file_a_path.exists(), "file_a.txt must exist after absorption A");
        assert_eq!(fs::read_to_string(&file_a_path).unwrap(), "changes from session A");

        // Simulate session B startup: jj new --ignore-working-copy
        Command::new("jj")
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();

        // --- Session B ---
        let ws_b = create_isolated_workspace(repo_dir.path(), "session-b").unwrap();
        jj_write_and_describe(&ws_b.path, "file_b.txt", "changes from session B", "[aiki] session B");

        let result_b = absorb_workspace(repo_dir.path(), &ws_b, None);
        assert!(
            matches!(result_b, Ok(AbsorbResult::Absorbed)),
            "Session B absorption should succeed, got: {:?}",
            result_b
        );
        cleanup_workspace(repo_dir.path(), &ws_b).unwrap();

        // CRITICAL: Both files must survive
        assert!(
            file_a_path.exists(),
            "BUG: file_a.txt from session A was reverted by session B's absorption!"
        );
        assert_eq!(fs::read_to_string(&file_a_path).unwrap(), "changes from session A");

        let file_b_path = repo_dir.path().join("file_b.txt");
        assert!(file_b_path.exists(), "file_b.txt must exist after absorption B");
        assert_eq!(fs::read_to_string(&file_b_path).unwrap(), "changes from session B");

        match original {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
    }

    /// Variant: Both sessions start before either absorbs (concurrent --async).
    #[test]
    fn test_sequential_absorptions_with_concurrent_startup() {
        use std::process::Command;

        let _lock = env_lock();
        let (repo_dir, ws_dir) = setup_jj_repo();
        let original = std::env::var("AIKI_WORKSPACES_DIR").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", ws_dir.path());

        // Session A startup
        Command::new("jj")
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
        let ws_a = create_isolated_workspace(repo_dir.path(), "session-a2").unwrap();

        // Session B startup (before A completes)
        Command::new("jj")
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
        let ws_b = create_isolated_workspace(repo_dir.path(), "session-b2").unwrap();

        // Both agents work
        jj_write_and_describe(&ws_a.path, "file_a.txt", "changes from A", "[aiki] A");
        jj_write_and_describe(&ws_b.path, "file_b.txt", "changes from B", "[aiki] B");

        // Agent A finishes first, absorbs
        let result_a = absorb_workspace(repo_dir.path(), &ws_a, None);
        assert!(matches!(result_a, Ok(AbsorbResult::Absorbed)));
        cleanup_workspace(repo_dir.path(), &ws_a).unwrap();

        let file_a_path = repo_dir.path().join("file_a.txt");
        assert!(file_a_path.exists(), "file_a.txt must exist after A absorbs");

        // Agent B finishes, absorbs
        let result_b = absorb_workspace(repo_dir.path(), &ws_b, None);
        assert!(matches!(result_b, Ok(AbsorbResult::Absorbed)));
        cleanup_workspace(repo_dir.path(), &ws_b).unwrap();

        // CRITICAL: Both files must survive
        assert!(file_a_path.exists(), "BUG: file_a.txt reverted by session B absorption!");
        let file_b_path = repo_dir.path().join("file_b.txt");
        assert!(file_b_path.exists(), "file_b.txt must exist after B absorbs");

        match original {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
    }

    /// Stress test: 7 concurrent sessions absorb sequentially.
    #[test]
    fn test_seven_sequential_absorptions_all_survive() {
        use std::process::Command;

        let _lock = env_lock();
        let (repo_dir, ws_dir) = setup_jj_repo();
        let original = std::env::var("AIKI_WORKSPACES_DIR").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", ws_dir.path());

        let num_sessions = 7;
        let mut workspaces = Vec::new();

        // All sessions start concurrently
        for i in 0..num_sessions {
            Command::new("jj")
                .args(["new", "--ignore-working-copy"])
                .current_dir(repo_dir.path())
                .output()
                .unwrap();

            let session_id = format!("stress-{}", i);
            let ws = create_isolated_workspace(repo_dir.path(), &session_id).unwrap();

            let filename = format!("file_{}.txt", i);
            let content = format!("changes from session {}", i);
            jj_write_and_describe(&ws.path, &filename, &content, &format!("[aiki] session {}", i));

            workspaces.push(ws);
        }

        // All sessions absorb sequentially
        for (i, ws) in workspaces.iter().enumerate() {
            let result = absorb_workspace(repo_dir.path(), ws, None);
            assert!(
                matches!(result, Ok(AbsorbResult::Absorbed)),
                "Session {} absorption failed: {:?}", i, result
            );
            cleanup_workspace(repo_dir.path(), ws).unwrap();

            // Verify all previously absorbed files still exist
            for j in 0..=i {
                let filepath = repo_dir.path().join(format!("file_{}.txt", j));
                assert!(
                    filepath.exists(),
                    "BUG: file_{}.txt reverted after session {} absorbed!", j, i
                );
                assert_eq!(
                    fs::read_to_string(&filepath).unwrap(),
                    format!("changes from session {}", j),
                );
            }
        }

        match original {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
    }
}
