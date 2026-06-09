//! Codex runtime implementation
//!
//! Spawns Codex sessions using the `codex` CLI in non-interactive mode.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{
    build_spawn_env, AgentRuntime, AgentSessionResult, AgentSpawnOptions, BackgroundHandle,
    MonitoredChild,
};
use super::AgentType;
use crate::error::{AikiError, Result};

/// Check if the working directory is inside a git repository.
/// Walks up from `dir` looking for a `.git` directory or file.
fn has_git_repo(dir: &Path) -> bool {
    let mut current = Some(dir);
    while let Some(d) = current {
        if d.join(".git").exists() {
            return true;
        }
        current = d.parent();
    }
    false
}

/// If `dir` is a JJ workspace whose repo store lives elsewhere (e.g. a shared
/// store in the original repo), return the parent `.jj` directory that needs
/// to be writable. JJ workspaces store a plain-text path in `.jj/repo` that
/// points to the shared repo store. When the store is outside the workspace
/// tree, Codex's sandbox must be told about it via `--add-dir`.
fn jj_shared_store_dir(dir: &Path) -> Option<PathBuf> {
    let repo_file = dir.join(".jj/repo");
    if let Ok(contents) = std::fs::read_to_string(&repo_file) {
        let store_path = PathBuf::from(contents.trim());
        // Only needed when the store is outside the workspace
        if !store_path.starts_with(dir) {
            // Return the `.jj` parent directory (one level up from `repo`)
            return store_path.parent().map(|p| p.to_path_buf());
        }
    }
    None
}

/// Apply common Codex CLI flags: `--skip-git-repo-check` when there's no
/// `.git`, and `--add-dir` when the JJ store lives outside the workspace.
fn apply_jj_flags(cmd: &mut Command, cwd: &Path) {
    if !has_git_repo(cwd) {
        cmd.arg("--skip-git-repo-check");
    }
    if let Some(store_dir) = jj_shared_store_dir(cwd) {
        cmd.arg("--add-dir").arg(store_dir);
    }
}

/// Runtime for Codex agent
pub struct CodexRuntime;

impl CodexRuntime {
    /// Create a new Codex runtime
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for CodexRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRuntime for CodexRuntime {
    fn spawn_blocking(&self, options: &AgentSpawnOptions) -> Result<AgentSessionResult> {
        let prompt = options.task_prompt();

        // Spawn codex process with prompt
        // Uses `codex exec` for non-interactive execution
        // Bypass sandbox to allow nested codex (child inherits parent's seatbelt which blocks API access)
        // TODO: replace with --profile once permission profiles are configured (see ops/now/fix-codex-run.md)
        let mut cmd = Command::new("codex");
        cmd.current_dir(&options.cwd)
            .args(["exec", "--dangerously-bypass-approvals-and-sandbox", &prompt])
            .envs(build_spawn_env(options, "background"));
        apply_jj_flags(&mut cmd, &options.cwd);
        let output = cmd.output();

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if output.status.success() {
                    // Extract summary from output (use last non-empty lines as summary)
                    let summary = extract_summary(&stdout);
                    Ok(AgentSessionResult::completed(summary))
                } else {
                    // Check if the agent explicitly stopped or actually failed
                    if stderr.contains("stopped") || stderr.contains("paused") {
                        Ok(AgentSessionResult::stopped(stderr))
                    } else {
                        Ok(AgentSessionResult::failed(format!(
                            "Exit code: {:?}\nStderr: {}",
                            output.status.code(),
                            stderr
                        )))
                    }
                }
            }
            Err(e) => Ok(AgentSessionResult::failed(format!(
                "Failed to spawn codex: {}",
                e
            ))),
        }
    }

    fn spawn_background(&self, options: &AgentSpawnOptions) -> Result<BackgroundHandle> {
        let prompt = options.task_prompt();

        // Spawn codex process detached from parent
        // The process runs independently and continues after parent exits
        // Bypass sandbox to allow nested codex (child inherits parent's seatbelt which blocks API access)
        // TODO: replace with --profile once permission profiles are configured (see ops/now/fix-codex-run.md)
        let mut cmd = Command::new("codex");
        cmd.current_dir(&options.cwd)
            .args(["exec", "--dangerously-bypass-approvals-and-sandbox", &prompt])
            .envs(build_spawn_env(options, "background"))
            // Detach stdin/stdout/stderr so process runs independently
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        apply_jj_flags(&mut cmd, &options.cwd);
        let child = cmd.spawn();

        match child {
            Ok(_child) => Ok(BackgroundHandle {
                thread: options.thread.clone(),
                session_id: None,
                agent_type: AgentType::Codex,
            }),
            Err(e) => Err(AikiError::AgentSpawnFailed(format!(
                "Failed to spawn codex in background: {}",
                e
            ))),
        }
    }

    fn spawn_monitored(&self, options: &AgentSpawnOptions) -> Result<MonitoredChild> {
        let prompt = options.task_prompt();

        // Spawn codex process - keep Child handle for monitoring
        // Bypass sandbox to allow nested codex (child inherits parent's seatbelt which blocks API access)
        // TODO: replace with --profile once permission profiles are configured (see ops/now/fix-codex-run.md)
        let mut cmd = Command::new("codex");
        cmd.current_dir(&options.cwd)
            .args(["exec", "--dangerously-bypass-approvals-and-sandbox", &prompt])
            .envs(build_spawn_env(options, "monitored"))
            // Detach stdin so process runs independently
            .stdin(Stdio::null())
            // Capture stdout/stderr so failures surface the real CLI message
            .stdout(Stdio::piped())
            // Capture stderr so we can report errors when the agent fails
            .stderr(Stdio::piped());
        apply_jj_flags(&mut cmd, &options.cwd);
        let child = cmd.spawn();

        match child {
            Ok(child) => Ok(MonitoredChild::new(child)),
            Err(e) => Err(AikiError::AgentSpawnFailed(format!(
                "Failed to spawn codex for monitoring: {}",
                e
            ))),
        }
    }
}

/// Extract a summary from the agent's output
///
/// Takes the last few non-empty lines as a summary, up to ~500 chars
fn extract_summary(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();

    if lines.is_empty() {
        return "Task completed".to_string();
    }

    // Take last 10 lines or ~500 chars, whichever is smaller
    let mut summary = String::new();
    for line in lines.iter().rev().take(10) {
        let prepend = format!("{}\n", line);
        if summary.len() + prepend.len() > 500 {
            break;
        }
        summary = prepend + summary.as_str();
    }

    if summary.is_empty() {
        "Task completed".to_string()
    } else {
        summary.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_summary_empty() {
        assert_eq!(extract_summary(""), "Task completed");
    }

    #[test]
    fn test_extract_summary_short() {
        let output = "Fixed the bug.\nTests pass.";
        let summary = extract_summary(output);
        assert!(summary.contains("Fixed the bug"));
    }
}
