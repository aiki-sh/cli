pub mod diff;
pub mod workspace;

pub use workspace::JJWorkspace;

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::error::{AikiError, Result};

/// Common locations where `jj` may be installed (not in default PATH for
/// processes spawned by GUI apps / OTel receivers).
const JJ_FALLBACK_PATHS: &[&str] = &[
    "/opt/homebrew/bin/jj",
    "/usr/local/bin/jj",
    "/usr/bin/jj",
];

/// Resolve the absolute path to the `jj` binary, caching the result.
///
/// When aiki is invoked by a process with a limited PATH (e.g. the OTel
/// receiver spawned on-demand by Codex), `jj` may not be discoverable via
/// the default search path. This function:
///
/// 1. Tries the plain `"jj"` name (works when PATH is correct).
/// 2. Falls back to well-known installation directories.
/// 3. Returns `"jj"` as a last resort so error messages stay meaningful.
pub fn jj_binary() -> &'static str {
    static RESOLVED: OnceLock<String> = OnceLock::new();
    RESOLVED.get_or_init(|| {
        // Fast path: `jj` is on PATH
        if Command::new("jj")
            .arg("version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return "jj".to_string();
        }

        // Check well-known locations
        for path in JJ_FALLBACK_PATHS {
            if std::path::Path::new(path).is_file() {
                return (*path).to_string();
            }
        }

        // Last resort — let the caller get a clear ENOENT
        "jj".to_string()
    })
}

/// Create a `std::process::Command` for the `jj` binary.
///
/// Equivalent to `Command::new(jj_binary())` — use this everywhere instead
/// of `Command::new("jj")` so the binary is resolved via [`jj_binary`].
pub fn jj_cmd() -> Command {
    Command::new(OsStr::new(jj_binary()))
}

/// Common flags for read-only JJ queries.
///
/// Returns arguments that should be used for all read-only JJ queries:
/// - `--no-pager`: Disable pager for non-interactive use
/// - `--no-graph`: Disable graph output for cleaner parsing
/// - `--ignore-working-copy`: Skip snapshotting working copy (faster, safe for reads)
///
/// Use with `command.args(jj_readonly_args())` or include these in your args array.
pub const JJ_READONLY_ARGS: &[&str] = &["--no-pager", "--no-graph", "--ignore-working-copy"];

/// Get all JJ change IDs that have a specific task ID in their provenance.
///
/// Queries JJ with revset `description("task=<id>")` to find changes where
/// the task was active (i.e., the task ID appears in the `[aiki]` metadata block).
///
/// Returns a list of change IDs (short form).
pub fn get_changes_for_task(cwd: &std::path::Path, task_id: &str) -> Vec<String> {
    let revset = format!("description(\"task={}\")", task_id);

    let output = jj_cmd()
        .current_dir(cwd)
        .arg("log")
        .arg("-r")
        .arg(&revset)
        .args(JJ_READONLY_ARGS)
        .args(["-T", "change_id ++ \"\\n\""])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|line| !line.is_empty())
                .map(|s| s.to_string())
                .collect()
        }
        _ => vec![],
    }
}

/// Get files modified in a specific JJ change.
///
/// Uses `jj show -r <change> --summary` to extract file paths.
/// Returns files with status M (modified), A (added), D (deleted), or R (renamed).
pub fn get_files_in_change(cwd: &std::path::Path, change_id: &str) -> Vec<String> {
    get_files_in_revset(cwd, change_id)
}

/// Get files modified across all changes matching a revset.
///
/// Uses `jj log -r <revset> --name-only --no-graph` to get file paths
/// in a single query with clean output (one path per line).
///
/// This is more efficient than calling `get_files_in_change` for each change
/// when you have multiple changes to query.
pub fn get_files_in_revset(cwd: &std::path::Path, revset: &str) -> Vec<String> {
    let output = jj_cmd()
        .current_dir(cwd)
        .arg("log")
        .arg("-r")
        .arg(revset)
        .args(JJ_READONLY_ARGS)
        .args(["-T", "", "--name-only"])
        .output();

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .map(|s| s.to_string())
            .collect(),
        _ => vec![],
    }
}

/// Get all files changed while working on a task.
///
/// Queries JJ for changes that have `task=<task_id>` in their provenance
/// metadata, then extracts all modified file paths from those changes.
///
/// Uses a single `jj log` command with `--summary` for efficiency.
///
/// Returns a deduplicated, sorted list of file paths.
pub fn get_files_for_task(cwd: &std::path::Path, task_id: &str) -> Vec<String> {
    // Query for files in all changes with this task ID in a single command
    let revset = format!("description(\"task={}\")", task_id);
    let files = get_files_in_revset(cwd, &revset);

    if files.is_empty() {
        return vec![];
    }

    // Deduplicate and sort
    let mut unique: std::collections::HashSet<String> = files.into_iter().collect();
    let mut result: Vec<String> = unique.drain().collect();
    result.sort();
    result
}

/// Process-level cache for branch existence checks.
///
/// Keyed by `(canonicalized_repo_path, branch_name)` so that multi-repo
/// scenarios (including tests) don't suppress checks for different repos.
static ENSURED_BRANCHES: OnceLock<Mutex<HashSet<(PathBuf, String)>>> = OnceLock::new();

/// Ensure a JJ branch (bookmark) exists, creating it from `root()` if needed.
///
/// Uses a process-level cache so that each `(repo, branch)` pair is checked
/// at most once per process. This eliminates redundant `jj bookmark list`
/// calls when multiple events are written in a single command.
pub fn ensure_branch(cwd: &Path, branch: &str) -> Result<()> {
    let key = (
        cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()),
        branch.to_string(),
    );
    let set = ENSURED_BRANCHES.get_or_init(|| Mutex::new(HashSet::new()));
    {
        let guard = set.lock().unwrap();
        if guard.contains(&key) {
            return Ok(());
        }
    }
    ensure_branch_impl(cwd, branch)?;
    set.lock().unwrap().insert(key);
    Ok(())
}

/// Check if a branch exists and return true/false without caching.
///
/// Used by read paths that need to return early (empty results) when the
/// branch doesn't exist yet, without creating it.
pub fn branch_exists(cwd: &Path, branch: &str) -> Result<bool> {
    let key = (
        cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()),
        branch.to_string(),
    );
    let set = ENSURED_BRANCHES.get_or_init(|| Mutex::new(HashSet::new()));
    {
        let guard = set.lock().unwrap();
        if guard.contains(&key) {
            return Ok(true);
        }
    }

    let output = jj_cmd()
        .current_dir(cwd)
        .args(["bookmark", "list", "--all", "--ignore-working-copy"])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to list bookmarks: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::JjCommandFailed(format!(
            "Failed to list bookmarks: {}",
            stderr
        )));
    }

    let bookmarks = String::from_utf8_lossy(&output.stdout);
    let exists = bookmarks.contains(branch);
    if exists {
        // Cache the positive result so future calls skip the check
        set.lock().unwrap().insert(key);
    }
    Ok(exists)
}

/// Implementation: check if branch exists via `jj bookmark list`, create if missing.
fn ensure_branch_impl(cwd: &Path, branch: &str) -> Result<()> {
    let output = jj_cmd()
        .current_dir(cwd)
        .args(["bookmark", "list", "--all", "--ignore-working-copy"])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to list bookmarks: {}", e)))?;

    let bookmarks = String::from_utf8_lossy(&output.stdout);

    if !bookmarks.contains(branch) {
        let result = jj_cmd()
            .current_dir(cwd)
            .args([
                "bookmark", "create", branch, "-r", "root()",
                "--ignore-working-copy",
            ])
            .output()
            .map_err(|e| {
                AikiError::JjCommandFailed(format!(
                    "Failed to create bookmark '{}': {}",
                    branch, e
                ))
            })?;

        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            return Err(AikiError::JjCommandFailed(format!(
                "Failed to create bookmark '{}': {}",
                branch, stderr
            )));
        }
    }
    Ok(())
}
