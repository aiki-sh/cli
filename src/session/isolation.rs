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

/// Name of the working-copy slot inside a session container.
///
/// Layout v2: `/tmp/aiki/<repo-id>/<session-id>/` is a plain container and
/// the session's JJ working copy lives at `<container>/main`. The name
/// `main` (not `workspace`) is deliberate — a later change makes this slot
/// branch-aware, and task workspaces will live at sibling `subagents/…`.
pub const SESSION_MAIN_SLOT: &str = "main";

/// Marker file written into a session container identifying the layout
/// version, so newer binaries (and shell tooling) can tell a v2 container
/// from a legacy working-copy-at-container-root directory.
pub const LAYOUT_MARKER_FILE: &str = ".aiki-layout";

/// The session container directory: `/tmp/aiki/<repo-id>/<session-id>/`
pub fn session_container_dir(repo_id: &str, session_uuid: &str) -> PathBuf {
    workspaces_dir().join(repo_id).join(session_uuid)
}

/// The session's working copy: `/tmp/aiki/<repo-id>/<session-id>/main`
pub fn session_workspace_dir(repo_id: &str, session_uuid: &str) -> PathBuf {
    session_container_dir(repo_id, session_uuid).join(SESSION_MAIN_SLOT)
}

/// Resolve a session's working-copy path, tolerating the legacy layout.
///
/// Returns `<container>/main` when it exists (layout v2), otherwise the
/// container itself (legacy layout, working copy at container root).
pub fn resolve_session_workspace_path(container: &Path) -> PathBuf {
    let main_slot = container.join(SESSION_MAIN_SLOT);
    if main_slot.exists() {
        main_slot
    } else {
        container.to_path_buf()
    }
}

/// Absorb target directory for a session: the parent session's working copy
/// when it exists, otherwise the repo root.
///
/// Single source of truth for `absorb_workspace` and
/// `workspace_absorb_all`'s post-absorb conflict check — the two used to
/// derive this independently and could drift.
pub fn absorb_target_dir(repo_root: &Path, parent_session_uuid: Option<&str>) -> PathBuf {
    if let Some(parent_uuid) = parent_session_uuid {
        if let Ok(repo_id) = repos::ensure_repo_id(repo_root) {
            let parent_container = session_container_dir(&repo_id, parent_uuid);
            let parent_ws = resolve_session_workspace_path(&parent_container);
            if parent_ws.join(".jj").exists() {
                return parent_ws;
            }
        }
    }
    repo_root.to_path_buf()
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

    let container_path = session_container_dir(&repo_id, session_uuid);
    let workspace_path = container_path.join(SESSION_MAIN_SLOT);
    let workspace_name = format!("aiki-{}", session_uuid);

    let workspace = IsolatedWorkspace {
        name: workspace_name.clone(),
        path: workspace_path.clone(),
    };

    // Hold the absorption lock across the reuse-rebase / destroy / recreate:
    // all of these mutate @-adjacent state and must not interleave with a
    // concurrent absorption's two-step rebase (or with another cleanup).
    // Reentrant with the nested acquisitions in cleanup_workspace_safely /
    // absorb_workspace.
    let _lock = acquire_named_lock(repo_root, "workspace-absorption")?;

    // Legacy-layout migration: before this layout, the session directory
    // itself WAS the working copy. This check must run before the reuse
    // block — in the old layout `<container>/main` does not exist, so
    // control would fall through to `jj workspace add`, which collides
    // because the name is still registered to the container.
    //
    // Absorb-first continuity: cleanup_workspace_safely folds the old
    // working copy's tracked work into main (or bookmarks/quarantines it),
    // forgets the registration, and removes the old directory — then the
    // fresh create below builds the v2 layout. Idempotent: a re-run after a
    // mid-migration crash finds no `<container>/.jj` and skips.
    if container_path.join(".jj").exists() && !workspace_path.exists() {
        debug_log(|| {
            format!(
                "[workspace] Migrating legacy-layout workspace at {} to {}/",
                container_path.display(),
                SESSION_MAIN_SLOT
            )
        });
        let legacy_ws = IsolatedWorkspace {
            name: workspace_name.clone(),
            path: container_path.clone(),
        };
        let parent_uuid = std::env::var("AIKI_PARENT_SESSION_UUID").ok();
        cleanup_workspace_safely(Some(repo_root), &legacy_ws, parent_uuid.as_deref());
    }

    // Workspace survived from previous turn — rebase to current fork point
    // so it picks up other sessions' absorbed changes.
    //
    // IMPORTANT: Do NOT use --ignore-working-copy here. JJ must update the
    // filesystem to reflect changes absorbed by concurrent sessions. Without
    // this, the next snapshot would see stale files and create a diff that
    // reverts other sessions' absorbed changes.
    //
    // Every failure branch below routes through cleanup_workspace_safely:
    // the workspace may hold unabsorbed work from an interrupted turn, and
    // destroy-without-preserve here was the largest confirmed data-loss
    // path in the isolation-02 review. Child sessions preserve into their
    // parent's workspace (same convention as workspace_absorb_all).
    let parent_uuid = std::env::var("AIKI_PARENT_SESSION_UUID").ok();
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
                                        cleanup_workspace_safely(Some(repo_root), &workspace, parent_uuid.as_deref());
                                    }
                                }
                            }
                            Ok(false) => {
                                debug_log(|| {
                                    "[workspace] Workspace lineage diverged from current @-, recreating workspace".to_string()
                                });
                                cleanup_workspace_safely(Some(repo_root), &workspace, parent_uuid.as_deref());
                            }
                            Err(e) => {
                                debug_log(|| format!("[workspace] Failed ancestry check: {e}"));
                                cleanup_workspace_safely(Some(repo_root), &workspace, parent_uuid.as_deref());
                            }
                        }
                    }
                    Err(_) => {
                        // Can't resolve workspace @- — fall through to destroy + recreate
                        debug_log(|| {
                            "[workspace] Could not resolve workspace @-, recreating workspace"
                                .to_string()
                        });
                        cleanup_workspace_safely(Some(repo_root), &workspace, parent_uuid.as_deref());
                    }
                }
            }
            Err(_) => {
                // Can't resolve @- — fall through to destroy + recreate
                debug_log(|| "[workspace] Could not resolve @-, recreating workspace".to_string());
                cleanup_workspace_safely(Some(repo_root), &workspace, parent_uuid.as_deref());
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

    // Layout marker: lets newer binaries and shell tooling identify a v2
    // container without probing for `main/.jj`.
    let _ = fs::write(container_path.join(LAYOUT_MARKER_FILE), "v2\n");

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

/// Per-path lock state: the leaked OS lock plus this process's reentrancy depth.
struct LockEntry {
    lock: &'static CachedLock,
    /// Reentrancy depth. `flock(2)` is per open-file-description: re-locking
    /// the same fd is a no-op, and the *first* unlock would release the lock
    /// while an outer scope still believes it holds it. The depth counter
    /// ensures only the outermost `NamedLockGuard` performs the OS unlock.
    depth: usize,
    /// Live OS-level guard, present while depth > 0.
    guard: Option<fd_lock::RwLockWriteGuard<'static, std::fs::File>>,
}

/// Cache of lock state keyed by lock-file path.
///
/// Ensures that repeated calls to `acquire_named_lock` with the same name
/// share one fd (flock is per file description) and one reentrancy counter.
static LOCK_CACHE: LazyLock<Mutex<HashMap<PathBuf, LockEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// RAII guard for a named lock. Releases the OS lock when the outermost
/// guard for this path drops.
pub struct NamedLockGuard {
    lock_path: PathBuf,
}

impl Drop for NamedLockGuard {
    fn drop(&mut self) {
        let mut cache = LOCK_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = cache.get_mut(&self.lock_path) {
            entry.depth = entry.depth.saturating_sub(1);
            if entry.depth == 0 {
                // Drop the OS guard → flock(LOCK_UN)
                entry.guard.take();
                debug_log(|| format!("Released lock at {}", self.lock_path.display()));
            }
        }
    }
}

/// Acquire a named file lock for the given repo.
///
/// Uses OS-level `flock(2)` via `fd-lock`. The lock is automatically
/// released when the returned guard drops — even on panic or SIGKILL.
/// Blocks until the lock is available (no timeout, no polling).
///
/// **Reentrant within a process:** nested acquisitions of the same lock name
/// succeed immediately and the OS lock is released only when the outermost
/// guard drops. This makes it safe for e.g. `cleanup_workspace_safely` (which
/// holds the lock) to call `absorb_workspace` (which also acquires it).
///
/// This is a **cross-process** mutex plus an in-process reentrancy counter —
/// it is NOT an intra-process thread mutex (aiki's lock-holding paths are
/// single-threaded per invocation).
pub fn acquire_named_lock(repo_root: &Path, name: &str) -> Result<NamedLockGuard> {
    let repo_id = repos::ensure_repo_id(repo_root)?;
    let lock_dir = workspaces_dir().join(&repo_id);
    fs::create_dir_all(&lock_dir)
        .map_err(|e| AikiError::LockFailed(format!("Failed to create lock directory: {e}")))?;
    let lock_path = lock_dir.join(format!(".{}.lock", name));

    let mut cache = LOCK_CACHE.lock().unwrap_or_else(|e| e.into_inner());

    if !cache.contains_key(&lock_path) {
        let file = std::fs::File::create(&lock_path)
            .map_err(|e| AikiError::LockFailed(format!("Failed to create lock file: {}", e)))?;
        let leaked: &'static CachedLock = Box::leak(Box::new(CachedLock(UnsafeCell::new(
            fd_lock::RwLock::new(file),
        ))));
        cache.insert(
            lock_path.clone(),
            LockEntry {
                lock: leaked,
                depth: 0,
                guard: None,
            },
        );
    }

    let entry = cache.get_mut(&lock_path).expect("entry inserted above");

    if entry.depth > 0 {
        // Reentrant acquisition: this process already holds the OS lock.
        entry.depth += 1;
        debug_log(|| format!("Re-entered '{}' lock (depth {})", name, entry.depth));
        return Ok(NamedLockGuard { lock_path });
    }

    // SAFETY: The pointer from UnsafeCell::get() points to Box::leaked memory
    // valid for 'static. The underlying flock(2) call is thread-safe, and in
    // practice callers serialize access through higher-level locks.
    let lock: &'static mut fd_lock::RwLock<std::fs::File> = unsafe { &mut *entry.lock.0.get() };

    // Blocks in the kernel while another PROCESS holds the lock. Holding the
    // cache mutex across this is intentional: it serializes in-process callers
    // behind the same wait, which is the ordering we want anyway.
    let guard = lock
        .write()
        .map_err(|e| AikiError::LockFailed(format!("Failed to acquire {} lock: {}", name, e)))?;

    entry.guard = Some(guard);
    entry.depth = 1;

    debug_log(|| format!("Acquired '{}' lock at {}", name, lock_path.display()));
    Ok(NamedLockGuard { lock_path })
}

/// Result of attempting to absorb a workspace
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbsorbResult {
    /// Workspace changes absorbed into the target (or already absorbed).
    /// Safe to clean up the workspace.
    Absorbed,
    /// Workspace had no real changes (root/zero change head).
    /// Safe to clean up the workspace.
    Empty,
    /// Absorption was skipped but the workspace directory may still hold
    /// changes (e.g. not registered in `jj workspace list`).
    /// NOT safe to clean up without preserving first.
    Skipped { reason: String },
    /// Absorption could not be completed safely (snapshot failed, or
    /// update-stale failed after the rebases — in which case a recovery
    /// bookmark was already created). The workspace must be retained.
    Deferred { reason: String },
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
            // The directory may still hold unsnapshotted files — the caller
            // must preserve before any cleanup.
            return Ok(AbsorbResult::Skipped {
                reason: format!(
                    "workspace '{}' not found in jj workspace list",
                    workspace.name
                ),
            });
        }
    };

    // Snapshot workspace working copy to capture files written since last snapshot.
    // All subsequent JJ commands use --ignore-working-copy, so without this,
    // files written after the last implicit snapshot would be lost.
    // Uses `jj status` (which triggers a snapshot as a side effect) instead of
    // `jj debug snapshot` to avoid unstable API.
    //
    // The result is CHECKED: proceeding into --ignore-working-copy rebases on
    // a failed snapshot would absorb a stale tree and report success while
    // the on-disk delta is silently dropped on cleanup.
    if let Err(reason) = checked_snapshot(&workspace.path) {
        eprintln!(
            "[aiki] Warning: workspace snapshot failed, deferring absorb of '{}': {}",
            workspace.name, reason
        );
        return Ok(AbsorbResult::Deferred {
            reason: format!("workspace snapshot failed: {}", reason),
        });
    }

    // Use the workspace's working copy (@) directly as the rebase target.
    // Previously this resolved @- (parent), which skipped all file changes
    // in the working copy commit.
    let ws_head = ws_change_id;

    // Guard against root/empty change heads — these indicate no real changes
    // were made in the workspace. JJ's root change ID is all zeros.
    if ws_head.chars().all(|c| c == '0') {
        debug_log(|| "Workspace head is root change, nothing to absorb");
        return Ok(AbsorbResult::Empty);
    }

    // Acquire file lock to serialize absorptions across concurrent agents.
    // Without this, concurrent absorptions interleave their two-step rebases,
    // causing each to disconnect from the previous absorption's changes.
    let _lock = acquire_named_lock(repo_root, "workspace-absorption")?;

    // Determine absorb target directory UNDER the lock, so a concurrent
    // cleanup of the parent workspace (also serialized on this lock) cannot
    // remove the target between selection and use.
    let target_dir = absorb_target_dir(repo_root, parent_session_uuid);

    // Snapshot target working copy while holding the absorption lock.
    // Without this, changes made in the target workspace while an agent is
    // working in an isolated one are not captured into @'s committed tree.
    // Checked for the same reason as the workspace snapshot above.
    if let Err(reason) = checked_snapshot(&target_dir) {
        eprintln!(
            "[aiki] Warning: target snapshot failed, deferring absorb of '{}': {}",
            workspace.name, reason
        );
        return Ok(AbsorbResult::Deferred {
            reason: format!("target snapshot failed: {}", reason),
        });
    }

    // Find the workspace chain's exclusive roots: commits reachable from
    // ws_head that are NOT already in the target's @- ancestry.
    //
    // If this set is empty, every workspace commit is already an ancestor of
    // @- — the workspace was already absorbed (e.g. turn.completed followed
    // by session.ended). Return Absorbed: it genuinely is.
    let roots_output = jj_cmd()
        .current_dir(&target_dir)
        .args([
            "log",
            "-r",
            &format!("roots({} ~ ::@-)", ws_head),
            "--no-graph",
            "-T",
            "change_id ++ \"\\n\"",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| {
            AikiError::WorkspaceAbsorbFailed(format!(
                "Failed to detect workspace chain roots: {}",
                e
            ))
        })?;

    if !roots_output.status.success() {
        let stderr = String::from_utf8_lossy(&roots_output.stderr);
        return Err(AikiError::WorkspaceAbsorbFailed(format!(
            "jj log (workspace chain root detection) failed: {}",
            stderr.trim()
        )));
    }

    let chain_roots: Vec<String> = String::from_utf8_lossy(&roots_output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if chain_roots.is_empty() {
        debug_log(|| {
            format!(
                "Workspace '{}' chain already in target ancestry — already absorbed",
                workspace.name
            )
        });
        return Ok(AbsorbResult::Absorbed);
    }

    // Step 1: Rebase the workspace chain's exclusive commits onto target's @-
    //
    // Uses -s (source) with the explicitly detected roots instead of
    // -b ws_head. `rebase -b` resolves the branch relative to the fork point
    // and, when two workspace chains share an ancestor, can re-attach the
    // second chain as a SIBLING of the first instead of extending the chain —
    // stranding previously absorbed commits outside @'s ancestry (the
    // topology bug behind the 2026-03-19 incident). `-s <roots>` moves
    // exactly the workspace-specific commits and their descendants.
    //
    // Uses --ignore-working-copy since we don't need to update the
    // filesystem yet (update-stale handles that after step 2).
    let mut rebase_args: Vec<String> = vec!["rebase".to_string()];
    for root in &chain_roots {
        rebase_args.push("-s".to_string());
        rebase_args.push(root.clone());
    }
    rebase_args.push("-d".to_string());
    rebase_args.push("@-".to_string());
    rebase_args.push("--ignore-working-copy".to_string());

    let output = jj_cmd()
        .current_dir(&target_dir)
        .args(&rebase_args)
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
                return Ok(AbsorbResult::Absorbed);
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
    // changes (the exact mechanism of the sequential-absorption post-mortem).
    //
    // A failed sync is treated as a NON-success: retried, and if it still
    // fails, the workspace head gets a recovery bookmark and the result is
    // Deferred so the caller retains the source workspace instead of
    // deleting it on the strength of a false "Absorbed".
    let mut update_stale_error = String::new();
    let mut update_stale_ok = false;
    for attempt in 1..=3u32 {
        let update_output = jj_cmd()
            .current_dir(&target_dir)
            .args(["workspace", "update-stale"])
            .output();

        match update_output {
            Ok(output) if output.status.success() => {
                update_stale_ok = true;
                break;
            }
            Ok(output) => {
                update_stale_error = String::from_utf8_lossy(&output.stderr).trim().to_string();
            }
            Err(e) => {
                update_stale_error = e.to_string();
            }
        }
        debug_log(|| {
            format!(
                "workspace update-stale attempt {} failed: {}",
                attempt, update_stale_error
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    if !update_stale_ok {
        eprintln!(
            "[aiki] WARNING: workspace update-stale failed after absorption — \
             filesystem may be stale. Run `jj workspace update-stale` in {} manually.\n\
             stderr: {}",
            target_dir.display(),
            update_stale_error
        );
        // Preserve the source: bookmark ws_head so the work stays
        // discoverable even if the source workspace is later reaped.
        let bookmark = create_recovery_bookmark(repo_root, &workspace.name, Some(&ws_head));
        if let Some(name) = bookmark {
            eprintln!("[aiki] Workspace changes preserved at bookmark {}", name);
        }
        return Ok(AbsorbResult::Deferred {
            reason: format!("update-stale failed after rebases: {}", update_stale_error),
        });
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

    // Remove the directory. For a v2 layout (working copy at
    // `<container>/main`), reclaim the whole session container — it holds
    // only the main slot, the layout marker, and any subagent slots, all of
    // which belong to this dead/finished session.
    let removal_target = if workspace
        .path
        .file_name()
        .map(|n| n == SESSION_MAIN_SLOT)
        .unwrap_or(false)
    {
        match workspace.path.parent() {
            Some(container) if container.starts_with(workspaces_dir()) => {
                container.to_path_buf()
            }
            _ => workspace.path.clone(),
        }
    } else {
        workspace.path.clone()
    };

    match fs::remove_dir_all(&removal_target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            debug_log(|| {
                format!(
                    "Warning: failed to remove workspace dir {}: {}",
                    removal_target.display(),
                    e
                )
            });
        }
    }

    debug_log(|| format!("Cleaned up workspace '{}'", workspace.name));
    Ok(())
}

/// Raised `snapshot.max-new-file-size` for aiki-driven snapshots, in bytes
/// (64 MiB; jj's default of 1 MiB silently skips larger new files, which
/// then get lost on any workspace cleanup — isolation-02 capture blind spot).
/// Passed as a TOML integer to avoid unit-string parsing ambiguity.
const SNAPSHOT_MAX_NEW_FILE_SIZE: u64 = 64 * 1024 * 1024;

/// Run a checked working-copy snapshot (`jj status`) in the given directory.
///
/// Returns Err(reason) when the snapshot did not verifiably complete. A
/// failed snapshot means on-disk edits are NOT in the shared store, so
/// callers must not destroy the directory or proceed into
/// `--ignore-working-copy` operations that assume a fresh tree.
///
/// Self-heals the stale-working-copy case: concurrent sessions rewrite
/// operations in the shared store, after which jj refuses to snapshot this
/// workspace ("The working copy is stale"). Without healing, every absorb
/// of the workspace defers forever and its work never lands (caught by the
/// e2e_*_concurrent_sessions_both_absorb tests). `jj workspace update-stale`
/// reconciles and the snapshot is retried once.
///
/// jj's own recovery commit only covers NOT-YET-SNAPSHOTTED edits. Files
/// already snapshotted into the workspace head are a blind spot: if a
/// concurrent reconcile moved the workspace pointer (e.g. a divergent-op
/// merge during a racing task close), update-stale resets the disk to the
/// new pointer and jj sees nothing to preserve — the old head's files
/// vanish from disk, reachable only through a dangling or hidden commit
/// (2026-07-09 incident, session 9e6269fd). So before update-stale this
/// resolves the head the disk actually corresponds to (via the operation
/// id in the stale error) and afterwards bookmarks it if it was stranded.
pub fn checked_snapshot(dir: &Path) -> std::result::Result<(), String> {
    match snapshot_once(dir) {
        Ok(()) => Ok(()),
        Err(first_err) => {
            if !first_err.contains("stale") {
                return Err(first_err);
            }
            debug_log(|| {
                format!(
                    "[workspace] stale working copy at {}; running update-stale and retrying snapshot",
                    dir.display()
                )
            });
            let stale_head = stale_workspace_head(dir, &first_err);
            let update = jj_cmd()
                .current_dir(dir)
                .args(["workspace", "update-stale"])
                .output();
            match update {
                Ok(o) if o.status.success() => {
                    if let Some((change_id, commit_id)) = stale_head {
                        preserve_stranded_head(dir, &change_id, &commit_id);
                    }
                    snapshot_once(dir)
                }
                Ok(o) => Err(format!(
                    "{first_err}; update-stale also failed: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                )),
                Err(e) => Err(format!("{first_err}; update-stale failed to run: {e}")),
            }
        }
    }
}

/// Resolve the workspace head the on-disk files correspond to: `@` as of
/// the operation named in jj's stale-working-copy error ("not updated
/// since operation <id>"). Returns `(change_id, commit_id)`; the commit id
/// matters because after a reconcile the change may be hidden and only the
/// commit id still resolves. `None` means the head could not be determined
/// (update-stale proceeds as before; jj still preserves unsnapshotted
/// edits).
fn stale_workspace_head(dir: &Path, stale_err: &str) -> Option<(String, String)> {
    let after = stale_err.split("since operation").nth(1)?;
    let op_id: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    if op_id.len() < 8 {
        return None;
    }
    let output = jj_cmd()
        .current_dir(dir)
        .args([
            "log",
            "--at-op",
            &op_id,
            "-r",
            "@",
            "--no-graph",
            "-T",
            "change_id ++ \" \" ++ commit_id",
            "--limit",
            "1",
            "--ignore-working-copy",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        debug_log(|| {
            format!(
                "[workspace] could not resolve stale head at op {}: {}",
                op_id,
                String::from_utf8_lossy(&output.stderr).trim()
            )
        });
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut parts = stdout.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(change), Some(commit)) => Some((change.to_string(), commit.to_string())),
        _ => None,
    }
}

/// After a successful update-stale, preserve the previous head if the
/// pointer moved away from it. "Stranded" means the old change is no
/// longer in the new `@`'s ancestry: a fresh pointer left it as a dangling
/// head, or the reconcile hid it outright. A rewrite that carried the
/// change along (normal absorb-induced staleness resolves the change id
/// inside `::@`) is NOT stranded and creates no bookmark.
fn preserve_stranded_head(dir: &Path, old_change_id: &str, old_commit_id: &str) {
    let check = jj_cmd()
        .current_dir(dir)
        .args([
            "log",
            "-r",
            &format!("{} & ::@", old_change_id),
            "--no-graph",
            "-T",
            "commit_id",
            "--limit",
            "1",
            "--ignore-working-copy",
        ])
        .output();
    let stranded_rev = match check {
        Ok(o) if o.status.success() => {
            if !String::from_utf8_lossy(&o.stdout).trim().is_empty() {
                return; // carried along by a rewrite — nothing stranded
            }
            // Change is visible but outside ::@ — a dangling head.
            // Bookmark the change's current commit (post-reconcile rewrite,
            // if any) rather than the possibly-superseded old commit id.
            resolve_visible_commit(dir, old_change_id)
                .unwrap_or_else(|| old_commit_id.to_string())
        }
        // Resolution failed — the reconcile hid the change. The commit id
        // still resolves hidden commits; bookmark it directly.
        _ => old_commit_id.to_string(),
    };

    let label = workspace_label(dir);
    match create_recovery_bookmark(dir, &label, Some(&stranded_rev)) {
        Some(name) => eprintln!(
            "[aiki] WARNING: the workspace pointer at {} was moved by a \
             concurrent operation; the previous head {} (with its \
             snapshotted files) is preserved at bookmark {}. \
             Run `aiki recover` to inspect.",
            dir.display(),
            &stranded_rev[..stranded_rev.len().min(12)],
            name
        ),
        None => eprintln!(
            "[aiki] WARNING: the workspace pointer at {} was moved by a \
             concurrent operation and the previous head {} could NOT be \
             bookmarked. It may still be recoverable via `jj op log` and \
             `jj log --at-op <op> -r @`.",
            dir.display(),
            &stranded_rev[..stranded_rev.len().min(12)]
        ),
    }
}

/// Resolve a change id to its current visible commit id, if any.
fn resolve_visible_commit(dir: &Path, change_id: &str) -> Option<String> {
    let output = jj_cmd()
        .current_dir(dir)
        .args([
            "log",
            "-r",
            change_id,
            "--no-graph",
            "-T",
            "commit_id",
            "--limit",
            "1",
            "--ignore-working-copy",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!commit.is_empty()).then_some(commit)
}

/// Human-readable label for recovery bookmarks. Session workspaces live at
/// `<session-id>/main` containers (isolation-11), so a bare `main` leaf is
/// labeled by its session container instead.
fn workspace_label(dir: &Path) -> String {
    let leaf = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "workspace".to_string());
    if leaf == "main" {
        if let Some(parent) = dir.parent().and_then(|p| p.file_name()) {
            return parent.to_string_lossy().to_string();
        }
    }
    leaf
}

fn snapshot_once(dir: &Path) -> std::result::Result<(), String> {
    let output = jj_cmd()
        .current_dir(dir)
        .args([
            "status",
            "--config",
            &format!("snapshot.max-new-file-size={}", SNAPSHOT_MAX_NEW_FILE_SIZE),
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Check whether a workspace working copy has any file changes.
///
/// Returns true when changes exist OR when the state cannot be determined
/// (safe default: assume there is something to lose).
pub fn workspace_has_changes(workspace_path: &Path) -> bool {
    let output = jj_cmd()
        .current_dir(workspace_path)
        .args(["diff", "--summary", "-r", "@"])
        .output();
    match output {
        Ok(o) if o.status.success() => !String::from_utf8_lossy(&o.stdout).trim().is_empty(),
        _ => true,
    }
}

/// Check whether the workspace chain has commits not yet in main's `@-`
/// ancestry. `None` means the state could not be determined.
fn workspace_unabsorbed_commits(repo_root: &Path, ws_change_id: &str) -> Option<bool> {
    let output = jj_cmd()
        .current_dir(repo_root)
        .args([
            "log",
            "-r",
            &format!("roots({} ~ ::@-)", ws_change_id),
            "--no-graph",
            "-T",
            "change_id",
            "--limit",
            "1",
            "--ignore-working-copy",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

/// Create an `aiki/recovered/<workspace-name>` bookmark on the workspace
/// head so unabsorbed work stays discoverable after the workspace is
/// forgotten. On name collision (repeated recovery), suffixes `-2`, `-3`, …
///
/// Returns the created bookmark name, or None if creation failed.
pub fn create_recovery_bookmark(
    repo_root: &Path,
    workspace_name: &str,
    change_id: Option<&str>,
) -> Option<String> {
    let ws_cid = match change_id {
        Some(id) => id.to_string(),
        None => find_workspace_change_id(repo_root, workspace_name).ok()??,
    };

    let base = format!("aiki/recovered/{}", workspace_name);
    for attempt in 1..=10u32 {
        let bookmark_name = if attempt == 1 {
            base.clone()
        } else {
            format!("{}-{}", base, attempt)
        };
        let output = jj_cmd()
            .current_dir(repo_root)
            .args([
                "bookmark",
                "create",
                &bookmark_name,
                "-r",
                &ws_cid,
                "--ignore-working-copy",
            ])
            .output();
        match output {
            Ok(o) if o.status.success() => {
                debug_log(|| format!("Created recovery bookmark {}", bookmark_name));
                return Some(bookmark_name);
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                // Name collision → try the next suffix; anything else is fatal.
                if !stderr.contains("already exists") {
                    eprintln!(
                        "[aiki] Warning: failed to create recovery bookmark {}: {}",
                        bookmark_name,
                        stderr.trim()
                    );
                    return None;
                }
            }
            Err(e) => {
                eprintln!(
                    "[aiki] Warning: failed to run jj bookmark create {}: {}",
                    bookmark_name, e
                );
                return None;
            }
        }
    }
    eprintln!(
        "[aiki] Warning: could not find a free recovery bookmark name for {}",
        workspace_name
    );
    None
}

/// Move a workspace directory into the quarantine area
/// (`$AIKI_HOME/recovered-workspaces/`) instead of deleting it.
///
/// Used when a workspace's content cannot be preserved in the shared jj
/// store (no resolvable repo root, or a failed snapshot). Falls back to
/// copy-then-remove across filesystems. Returns the quarantine path, or
/// None if the directory could not be moved (in which case it is left
/// in place — never deleted).
pub fn quarantine_workspace(dir: &Path) -> Option<PathBuf> {
    let quarantine_root = crate::global::global_aiki_dir().join("recovered-workspaces");
    if fs::create_dir_all(&quarantine_root).is_err() {
        return None;
    }

    let base_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "workspace".to_string());

    for attempt in 1..=100u32 {
        let dest = if attempt == 1 {
            quarantine_root.join(&base_name)
        } else {
            quarantine_root.join(format!("{}-{}", base_name, attempt))
        };
        if dest.exists() {
            continue;
        }
        match fs::rename(dir, &dest) {
            Ok(()) => {
                eprintln!(
                    "[aiki] Workspace quarantined at {} (was {})",
                    dest.display(),
                    dir.display()
                );
                return Some(dest);
            }
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                // Cross-filesystem: copy then remove.
                if copy_dir_recursive(dir, &dest).is_ok() {
                    let _ = fs::remove_dir_all(dir);
                    eprintln!(
                        "[aiki] Workspace quarantined (copied) at {} (was {})",
                        dest.display(),
                        dir.display()
                    );
                    return Some(dest);
                }
                let _ = fs::remove_dir_all(&dest);
                return None;
            }
            Err(_) => return None,
        }
    }
    None
}

/// True when the directory is empty or holds only the layout marker —
/// i.e. there is nothing in it worth preserving.
fn dir_is_effectively_empty(dir: &Path) -> bool {
    match fs::read_dir(dir) {
        Ok(entries) => !entries
            .flatten()
            .any(|e| e.file_name() != LAYOUT_MARKER_FILE),
        // Missing dir is trivially empty; unreadable is NOT (assume content)
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if ty.is_symlink() {
            #[cfg(unix)]
            {
                let target = fs::read_link(entry.path())?;
                let _ = std::os::unix::fs::symlink(target, &dest_path);
            }
        } else {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

/// Outcome of a safe workspace cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeCleanupOutcome {
    /// Changes were absorbed into the target, then the workspace was deleted.
    Absorbed,
    /// Workspace had nothing to preserve; deleted.
    Empty,
    /// Changes were preserved on a recovery bookmark, then the workspace
    /// was deleted.
    Bookmarked(String),
    /// The directory was moved to the quarantine area (not deleted).
    Quarantined(PathBuf),
    /// Nothing could be preserved; the directory was left in place.
    Retained,
}

/// Destroy a workspace ONLY after its content is provably preserved.
///
/// This is the single safe-destroy helper (isolation-02 Invariant C): every
/// call site that previously ran `cleanup_workspace` / `remove_dir_all` on a
/// workspace that might hold unabsorbed work routes through here.
///
/// Order of preference:
/// 1. absorb into target → delete
/// 2. nothing to preserve → delete
/// 3. recovery bookmark on the snapshotted head → delete
/// 4. quarantine the directory (snapshot failed or no repo root) → keep
/// 5. retain in place (could not even quarantine)
///
/// Holds the workspace-absorption lock for the whole operation so a
/// concurrent absorb cannot select this directory as its target mid-destroy.
pub fn cleanup_workspace_safely(
    repo_root: Option<&Path>,
    workspace: &IsolatedWorkspace,
    parent_session_uuid: Option<&str>,
) -> SafeCleanupOutcome {
    let repo_root = match repo_root {
        Some(root) => root,
        None => {
            // A directory with nothing in it (or only the layout marker) is
            // debris, not work — remove instead of quarantining.
            if dir_is_effectively_empty(&workspace.path) {
                let _ = fs::remove_dir_all(&workspace.path);
                return SafeCleanupOutcome::Empty;
            }
            // No resolvable repo root: the shared store is unreachable, so
            // nothing can be preserved in jj. Quarantine, never delete.
            return match quarantine_workspace(&workspace.path) {
                Some(dest) => SafeCleanupOutcome::Quarantined(dest),
                None => {
                    eprintln!(
                        "[aiki] Warning: could not quarantine workspace at {} — leaving in place",
                        workspace.path.display()
                    );
                    SafeCleanupOutcome::Retained
                }
            };
        }
    };

    // Serialize with concurrent absorbs (and other cleanups). Reentrant, so
    // the nested absorb_workspace lock acquisition below is fine.
    let _lock = match acquire_named_lock(repo_root, "workspace-absorption") {
        Ok(guard) => Some(guard),
        Err(e) => {
            eprintln!(
                "[aiki] Warning: could not acquire absorption lock for cleanup: {}",
                e
            );
            None
        }
    };

    match absorb_workspace(repo_root, workspace, parent_session_uuid) {
        Ok(AbsorbResult::Absorbed) => {
            let _ = cleanup_workspace(repo_root, workspace);
            SafeCleanupOutcome::Absorbed
        }
        Ok(AbsorbResult::Empty) => {
            let _ = cleanup_workspace(repo_root, workspace);
            SafeCleanupOutcome::Empty
        }
        Ok(AbsorbResult::Deferred { reason }) => {
            // Snapshot or update-stale failed. The on-disk state is not
            // provably in the store — do not delete. update-stale failures
            // already carry a recovery bookmark from absorb_workspace.
            eprintln!(
                "[aiki] Workspace '{}' cleanup deferred ({}) — retaining directory",
                workspace.name, reason
            );
            SafeCleanupOutcome::Retained
        }
        Ok(AbsorbResult::Skipped { reason }) => {
            preserve_then_cleanup(repo_root, workspace, &reason)
        }
        Err(e) => preserve_then_cleanup(repo_root, workspace, &e.to_string()),
    }
}

/// Bookmark-or-quarantine path for a workspace whose absorb failed or was
/// skipped. Deletes the directory only after the content is preserved.
fn preserve_then_cleanup(
    repo_root: &Path,
    workspace: &IsolatedWorkspace,
    reason: &str,
) -> SafeCleanupOutcome {
    let dir_exists = workspace.path.exists();

    // Snapshot the on-disk state (checked) so the working copy delta is in
    // the shared store before we bookmark or delete anything.
    let snapshot_ok = if dir_exists {
        match checked_snapshot(&workspace.path) {
            Ok(()) => true,
            Err(e) => {
                debug_log(|| format!("preserve_then_cleanup snapshot failed: {}", e));
                false
            }
        }
    } else {
        // Nothing on disk; whatever was snapshotted before is all there is.
        true
    };

    let ws_cid = find_workspace_change_id(repo_root, &workspace.name)
        .ok()
        .flatten();

    // Determine whether there is anything to preserve.
    let has_content = match &ws_cid {
        Some(cid) => {
            let unabsorbed = workspace_unabsorbed_commits(repo_root, cid);
            let disk_changes = dir_exists && workspace_has_changes(&workspace.path);
            unabsorbed.unwrap_or(true) || disk_changes
        }
        None => dir_exists && workspace_has_changes(&workspace.path),
    };

    if !has_content {
        let _ = cleanup_workspace(repo_root, workspace);
        return SafeCleanupOutcome::Empty;
    }

    if !snapshot_ok {
        // On-disk edits are NOT captured in the store; a bookmark would not
        // preserve them. Quarantine the directory instead of deleting.
        eprintln!(
            "[aiki] Workspace '{}' has uncaptured changes (snapshot failed, absorb: {}) — quarantining",
            workspace.name, reason
        );
        // Forget the jj registration but keep the files.
        let _ = jj_cmd()
            .current_dir(repo_root)
            .args([
                "workspace",
                "forget",
                &workspace.name,
                "--ignore-working-copy",
            ])
            .output();
        return match quarantine_workspace(&workspace.path) {
            Some(dest) => SafeCleanupOutcome::Quarantined(dest),
            None => SafeCleanupOutcome::Retained,
        };
    }

    match create_recovery_bookmark(repo_root, &workspace.name, ws_cid.as_deref()) {
        Some(bookmark) => {
            eprintln!(
                "[aiki] Workspace '{}' could not be absorbed ({}); changes preserved at {}",
                workspace.name, reason, bookmark
            );
            let _ = cleanup_workspace(repo_root, workspace);
            SafeCleanupOutcome::Bookmarked(bookmark)
        }
        None => {
            // Could not bookmark: fall back to quarantine, else retain.
            let _ = jj_cmd()
                .current_dir(repo_root)
                .args([
                    "workspace",
                    "forget",
                    &workspace.name,
                    "--ignore-working-copy",
                ])
                .output();
            if dir_exists {
                match quarantine_workspace(&workspace.path) {
                    Some(dest) => SafeCleanupOutcome::Quarantined(dest),
                    None => SafeCleanupOutcome::Retained,
                }
            } else {
                SafeCleanupOutcome::Retained
            }
        }
    }
}

/// Find and recover all workspaces for a dead session across all repos.
///
/// Scans `/tmp/aiki/*/<session-id>/` (where * is repo-id).
/// For each: absorb into main when possible, otherwise preserve via a
/// recovery bookmark or quarantine — the directory is destroyed only after
/// its content is provably preserved (`cleanup_workspace_safely`).
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

        let container = repo_id_dir.join(session_uuid);
        if !container.exists() {
            continue;
        }
        // Layout v2: working copy at <container>/main; legacy: at the root.
        let session_ws_dir = resolve_session_workspace_path(&container);

        let workspace_name = format!("aiki-{}", session_uuid);

        // Try to find the repo root from the workspace
        // The workspace contains a .jj/ that links back to the repo
        let repo_root = find_repo_root_from_workspace(&session_ws_dir);
        if repo_root.is_none() {
            eprintln!(
                "[aiki] Warning: could not determine repo root for orphaned workspace at {}",
                session_ws_dir.display()
            );
            // cleanup_workspace_safely quarantines instead of deleting.
        }

        let workspace = IsolatedWorkspace {
            name: workspace_name.clone(),
            path: session_ws_dir,
        };

        match cleanup_workspace_safely(repo_root.as_deref(), &workspace, None) {
            SafeCleanupOutcome::Absorbed => {
                recovered += 1;
            }
            SafeCleanupOutcome::Bookmarked(bookmark) => {
                eprintln!(
                    "[aiki] Orphaned workspace '{}' preserved at bookmark {}",
                    workspace_name, bookmark
                );
                recovered += 1;
            }
            SafeCleanupOutcome::Quarantined(dest) => {
                eprintln!(
                    "[aiki] Orphaned workspace '{}' quarantined at {}",
                    workspace_name,
                    dest.display()
                );
                recovered += 1;
            }
            SafeCleanupOutcome::Empty | SafeCleanupOutcome::Retained => {}
        }
    }

    Ok(recovered)
}

/// Clean up orphaned JJ workspaces that no longer have active sessions.
///
/// Scans `jj workspace list` for `aiki-*` entries, checks if each session
/// is still backed by a live session file in `~/.aiki/sessions/{uuid}`,
/// and reclaims workspaces for dead sessions. This prevents the JJ workspace
/// list from growing unbounded.
///
/// Reclamation goes through `cleanup_workspace_safely`: unabsorbed work is
/// absorbed, bookmarked, or quarantined before anything is destroyed, and
/// the whole sweep holds the absorption lock so it cannot delete a
/// directory a concurrent absorb has selected as its target.
pub fn cleanup_orphaned_workspaces(repo_root: &Path) -> Result<u32> {
    // Serialize with concurrent absorptions (reentrant with the nested
    // acquisitions inside cleanup_workspace_safely).
    let _lock = acquire_named_lock(repo_root, "workspace-absorption")?;

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

        // Session is dead — reclaim the workspace, preserving content first
        debug_log(|| {
            format!(
                "Reclaiming orphaned workspace '{}' (no active session)",
                ws_name
            )
        });

        let ws_dir = match crate::repos::ensure_repo_id(repo_root) {
            Ok(repo_id) => {
                let container = session_container_dir(&repo_id, uuid);
                // Layout v2: working copy at <container>/main; legacy: root.
                resolve_session_workspace_path(&container)
            }
            Err(_) => PathBuf::new(),
        };

        let workspace = IsolatedWorkspace {
            name: ws_name.to_string(),
            path: ws_dir,
        };

        if workspace.path.as_os_str().is_empty() || !workspace.path.exists() {
            // No directory on disk: preserve any unabsorbed committed chain
            // via bookmark before dropping the registration.
            if let Ok(Some(ws_cid)) = find_workspace_change_id(repo_root, ws_name) {
                if workspace_unabsorbed_commits(repo_root, &ws_cid).unwrap_or(true) {
                    if let Some(bookmark) =
                        create_recovery_bookmark(repo_root, ws_name, Some(&ws_cid))
                    {
                        eprintln!(
                            "[aiki] Orphaned workspace '{}' preserved at bookmark {}",
                            ws_name, bookmark
                        );
                    }
                }
            }
            let forget_output = jj_cmd()
                .current_dir(repo_root)
                .args(["workspace", "forget", ws_name, "--ignore-working-copy"])
                .output();
            if let Ok(out) = forget_output {
                if out.status.success() {
                    cleaned += 1;
                }
            }
            continue;
        }

        match cleanup_workspace_safely(Some(repo_root), &workspace, None) {
            SafeCleanupOutcome::Retained => {}
            _ => {
                cleaned += 1;
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

    /// Regression test for isolation-13-fix-absorbtion-on-multiple-runs.md:
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

    /// Regression (isolation-02 Group A/B): a workspace whose jj registration
    /// is gone but whose directory still holds files must NEVER be deleted —
    /// it is quarantined (the on-disk delta is unsnapshottable once the
    /// registration is gone).
    #[test]
    fn test_forgotten_workspace_with_files_is_quarantined_not_deleted() {
        use std::process::Command;

        let _lock = env_lock();
        let (repo_dir, ws_dir) = setup_jj_repo();
        let aiki_home = tempfile::tempdir().unwrap();
        let original_ws = std::env::var("AIKI_WORKSPACES_DIR").ok();
        let original_home = std::env::var("AIKI_HOME").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", ws_dir.path());
        std::env::set_var("AIKI_HOME", aiki_home.path());

        Command::new("jj")
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();

        let ws = create_isolated_workspace(repo_dir.path(), "session-q").unwrap();
        // Unsnapshotted on-disk edit
        fs::write(ws.path.join("precious.txt"), "do not lose me").unwrap();

        // Simulate the forgotten-in-jj-but-dir-present state (HIGH risk in
        // the isolation-02 state table).
        Command::new("jj")
            .args(["workspace", "forget", &ws.name, "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();

        // Absorb reports Skipped (not in workspace list)
        let absorb = absorb_workspace(repo_dir.path(), &ws, None);
        assert!(
            matches!(absorb, Ok(AbsorbResult::Skipped { .. })),
            "expected Skipped for unregistered workspace, got {:?}",
            absorb
        );

        // Safe cleanup must preserve the file, not delete it
        let outcome = cleanup_workspace_safely(Some(repo_dir.path()), &ws, None);
        match &outcome {
            SafeCleanupOutcome::Quarantined(dest) => {
                assert!(
                    dest.join("precious.txt").exists(),
                    "quarantined dir must contain the file"
                );
                assert!(!ws.path.exists(), "original dir moved away");
            }
            SafeCleanupOutcome::Bookmarked(_) | SafeCleanupOutcome::Retained => {
                // Also acceptable: content preserved / left in place
            }
            other => panic!(
                "workspace with files must not be silently destroyed, got {:?}",
                other
            ),
        }

        match original_ws {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
        match original_home {
            Some(v) => std::env::set_var("AIKI_HOME", v),
            None => std::env::remove_var("AIKI_HOME"),
        }
    }

    /// Regression: no resolvable repo root → quarantine, never remove_dir_all.
    #[test]
    fn test_no_repo_root_quarantines_directory() {
        let _lock = env_lock();
        let aiki_home = tempfile::tempdir().unwrap();
        let original_home = std::env::var("AIKI_HOME").ok();
        std::env::set_var("AIKI_HOME", aiki_home.path());

        let orphan = tempfile::tempdir().unwrap();
        let orphan_dir = orphan.path().join("session-z");
        fs::create_dir_all(&orphan_dir).unwrap();
        fs::write(orphan_dir.join("work.txt"), "unabsorbed").unwrap();

        let ws = IsolatedWorkspace {
            name: "aiki-session-z".to_string(),
            path: orphan_dir.clone(),
        };

        let outcome = cleanup_workspace_safely(None, &ws, None);
        match outcome {
            SafeCleanupOutcome::Quarantined(dest) => {
                assert!(dest.join("work.txt").exists());
                assert!(!orphan_dir.exists());
            }
            SafeCleanupOutcome::Retained => {
                assert!(
                    orphan_dir.join("work.txt").exists(),
                    "retained dir must keep its files"
                );
            }
            other => panic!("expected Quarantined/Retained, got {:?}", other),
        }

        match original_home {
            Some(v) => std::env::set_var("AIKI_HOME", v),
            None => std::env::remove_var("AIKI_HOME"),
        }
    }

    /// Recovery bookmark names must not collide on repeated recovery.
    #[test]
    fn test_recovery_bookmark_collision_gets_suffix() {
        use std::process::Command;

        let _lock = env_lock();
        let (repo_dir, ws_dir) = setup_jj_repo();
        let original_ws = std::env::var("AIKI_WORKSPACES_DIR").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", ws_dir.path());

        Command::new("jj")
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
        let ws = create_isolated_workspace(repo_dir.path(), "session-bm").unwrap();
        jj_write_and_describe(&ws.path, "f.txt", "content", "[aiki] bm test");

        let first = create_recovery_bookmark(repo_dir.path(), &ws.name, None);
        assert_eq!(first.as_deref(), Some("aiki/recovered/aiki-session-bm"));

        let second = create_recovery_bookmark(repo_dir.path(), &ws.name, None);
        assert_eq!(
            second.as_deref(),
            Some("aiki/recovered/aiki-session-bm-2"),
            "second recovery must get a suffixed name"
        );

        match original_ws {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
    }

    /// Regression (2026-07-09 incident, session 9e6269fd): a divergent-op
    /// reconcile during a racing task close moved a workspace pointer to a
    /// fresh empty commit, stranding the snapshotted-but-unabsorbed
    /// provenance commit; `jj workspace update-stale` then deleted the
    /// on-disk file with NO recovery commit (the file was already
    /// snapshotted, so jj saw nothing to preserve). The stale-heal in
    /// checked_snapshot must bookmark the stranded head so the work stays
    /// reachable.
    #[test]
    fn test_stale_heal_preserves_stranded_snapshotted_head() {
        use std::process::Command;

        let _lock = env_lock();
        let (repo_dir, ws_dir) = setup_jj_repo();
        let original_ws = std::env::var("AIKI_WORKSPACES_DIR").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", ws_dir.path());

        Command::new("jj")
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
        let ws = create_isolated_workspace(repo_dir.path(), "session-strand").unwrap();

        // The agent writes a file; the per-tool-call hook snapshots it into
        // the workspace's working-copy commit (the provenance-commit analog).
        fs::write(ws.path.join("gtm-launch.md"), "the plan").unwrap();
        checked_snapshot(&ws.path).expect("initial snapshot");

        let head = Command::new("jj")
            .args(["log", "-r", "@", "--no-graph", "-T", "commit_id"])
            .current_dir(&ws.path)
            .output()
            .unwrap();
        let old_commit = String::from_utf8_lossy(&head.stdout).trim().to_string();
        assert!(!old_commit.is_empty());

        // Simulate the reconcile stranding the head: rewrite the workspace's
        // wc commit from the main repo. jj resets the pointer to a fresh
        // empty commit and hides the old head; the workspace checkout is now
        // stale, and its on-disk file corresponds to a commit that no jj
        // recovery mechanism will preserve on update-stale.
        let abandon = Command::new("jj")
            .args(["abandon", &old_commit, "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
        assert!(
            abandon.status.success(),
            "abandon failed: {}",
            String::from_utf8_lossy(&abandon.stderr)
        );

        // Healing must succeed AND preserve the stranded head.
        checked_snapshot(&ws.path).expect("stale heal");

        let bookmarks = Command::new("jj")
            .args([
                "log",
                "-r",
                "bookmarks(glob:\"aiki/recovered/*\")",
                "--no-graph",
                "-T",
                "bookmarks",
                "--ignore-working-copy",
            ])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
        let names = String::from_utf8_lossy(&bookmarks.stdout).trim().to_string();
        assert!(
            !names.is_empty(),
            "stranded head must get a recovery bookmark"
        );
        let bookmark = names.split_whitespace().next().unwrap().trim_end_matches('*');

        let show = Command::new("jj")
            .args(["file", "show", "-r", bookmark, "gtm-launch.md"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
        assert!(
            show.status.success(),
            "jj file show failed: {}",
            String::from_utf8_lossy(&show.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&show.stdout),
            "the plan",
            "recovered bookmark must hold the snapshotted file content"
        );

        match original_ws {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
    }

    /// Absorbing the same workspace twice returns Absorbed both times
    /// (idempotent), never a result that could trigger destructive cleanup.
    #[test]
    fn test_double_absorb_is_idempotent_absorbed() {
        use std::process::Command;

        let _lock = env_lock();
        let (repo_dir, ws_dir) = setup_jj_repo();
        let original_ws = std::env::var("AIKI_WORKSPACES_DIR").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", ws_dir.path());

        Command::new("jj")
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
        let ws = create_isolated_workspace(repo_dir.path(), "session-dbl").unwrap();
        jj_write_and_describe(&ws.path, "dbl.txt", "content", "[aiki] dbl");

        let first = absorb_workspace(repo_dir.path(), &ws, None);
        assert!(matches!(first, Ok(AbsorbResult::Absorbed)), "{:?}", first);

        // Second absorb (turn.completed followed by session.ended)
        let second = absorb_workspace(repo_dir.path(), &ws, None);
        assert!(
            matches!(second, Ok(AbsorbResult::Absorbed)),
            "double absorb must be idempotent Absorbed, got {:?}",
            second
        );
        assert!(repo_dir.path().join("dbl.txt").exists());

        match original_ws {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
    }

    /// Layout v2 (isolation-11): the working copy lives at
    /// `<container>/main`, the container carries a layout marker, and
    /// cleanup reclaims the whole container.
    #[test]
    fn test_session_workspace_rehomed_under_main() {
        use std::process::Command;

        let _lock = env_lock();
        let (repo_dir, ws_dir) = setup_jj_repo();
        let original = std::env::var("AIKI_WORKSPACES_DIR").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", ws_dir.path());

        Command::new("jj")
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();

        let ws = create_isolated_workspace(repo_dir.path(), "session-v2").unwrap();
        let container = ws_dir.path().join("testrepo").join("session-v2");

        assert_eq!(ws.path, container.join(SESSION_MAIN_SLOT));
        assert!(ws.path.join(".jj").exists(), "working copy at main/");
        assert!(
            container.join(LAYOUT_MARKER_FILE).exists(),
            "container has layout marker"
        );
        assert!(
            find_repo_root_from_workspace(&ws.path).is_some(),
            "resolver works against the main slot"
        );

        // Absorb + cleanup reclaims the whole container
        jj_write_and_describe(&ws.path, "v2.txt", "content", "[aiki] v2");
        let result = absorb_workspace(repo_dir.path(), &ws, None);
        assert!(matches!(result, Ok(AbsorbResult::Absorbed)), "{:?}", result);
        cleanup_workspace(repo_dir.path(), &ws).unwrap();
        assert!(
            !container.exists(),
            "cleanup must reclaim the whole container"
        );
        assert!(repo_dir.path().join("v2.txt").exists());

        match original {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
    }

    /// Legacy migration (isolation-11 Phase 1.2, absorb-first): a session
    /// whose working copy sits at the container root is migrated to
    /// `<container>/main` without a workspace-name collision, and its
    /// tracked work survives (absorbed into main or bookmarked).
    #[test]
    fn test_legacy_layout_migrates_to_main_slot_preserving_work() {
        use std::process::Command;

        let _lock = env_lock();
        let (repo_dir, ws_dir) = setup_jj_repo();
        let original = std::env::var("AIKI_WORKSPACES_DIR").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", ws_dir.path());

        Command::new("jj")
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();

        // Build the LEGACY layout by hand: workspace at the container root.
        let container = ws_dir.path().join("testrepo").join("session-legacy");
        fs::create_dir_all(container.parent().unwrap()).unwrap();
        let add_out = Command::new("jj")
            .args([
                "workspace",
                "add",
                &container.to_string_lossy(),
                "--name",
                "aiki-session-legacy",
                "-r",
                "@-",
            ])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
        assert!(
            add_out.status.success(),
            "legacy workspace add failed: {}",
            String::from_utf8_lossy(&add_out.stderr)
        );
        jj_write_and_describe(&container, "legacy.txt", "old work", "[aiki] legacy");

        // Re-create for the same session: must migrate, not collide.
        let ws = create_isolated_workspace(repo_dir.path(), "session-legacy").unwrap();
        assert_eq!(ws.path, container.join(SESSION_MAIN_SLOT));
        assert!(ws.path.join(".jj").exists(), "fresh working copy at main/");
        assert!(
            !container.join(".jj").exists(),
            "old container-root working copy must be gone"
        );

        // Continuity: the legacy tracked work was absorbed into the repo.
        assert!(
            repo_dir.path().join("legacy.txt").exists(),
            "legacy tracked work must be absorbed into main (absorb-first migration)"
        );

        // Idempotence: running create again just reuses the migrated layout.
        let ws2 = create_isolated_workspace(repo_dir.path(), "session-legacy").unwrap();
        assert_eq!(ws2.path, ws.path);

        match original {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
    }

    /// Reaper: a dead session's container (v2 layout) is fully reclaimed by
    /// the orphan sweep after its work is preserved; a live session's
    /// container is untouched.
    #[test]
    fn test_orphan_sweep_reclaims_dead_container_keeps_live() {
        use std::process::Command;

        let _lock = env_lock();
        let (repo_dir, ws_dir) = setup_jj_repo();
        let aiki_home = tempfile::tempdir().unwrap();
        let original_ws = std::env::var("AIKI_WORKSPACES_DIR").ok();
        let original_home = std::env::var("AIKI_HOME").ok();
        std::env::set_var("AIKI_WORKSPACES_DIR", ws_dir.path());
        std::env::set_var("AIKI_HOME", aiki_home.path());

        Command::new("jj")
            .args(["new", "--ignore-working-copy"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();

        // Live session: has a session file
        let live = create_isolated_workspace(repo_dir.path(), "session-live").unwrap();
        let sessions_dir = crate::global::global_sessions_dir();
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(sessions_dir.join("session-live"), "alive").unwrap();

        // Dead session: no session file, has committed work
        let dead = create_isolated_workspace(repo_dir.path(), "session-dead").unwrap();
        jj_write_and_describe(&dead.path, "dead.txt", "work", "[aiki] dead");
        let dead_container = ws_dir.path().join("testrepo").join("session-dead");

        let cleaned = cleanup_orphaned_workspaces(repo_dir.path()).unwrap();
        assert!(cleaned >= 1, "dead session must be reclaimed");
        assert!(
            !dead_container.exists(),
            "dead container fully reclaimed"
        );
        assert!(
            repo_dir.path().join("dead.txt").exists(),
            "dead session's work absorbed, not lost"
        );
        assert!(live.path.exists(), "live session container untouched");

        match original_ws {
            Some(v) => std::env::set_var("AIKI_WORKSPACES_DIR", v),
            None => std::env::remove_var("AIKI_WORKSPACES_DIR"),
        }
        match original_home {
            Some(v) => std::env::set_var("AIKI_HOME", v),
            None => std::env::remove_var("AIKI_HOME"),
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
