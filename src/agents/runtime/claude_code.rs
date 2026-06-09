//! Claude Code runtime implementation
//!
//! Spawns Claude Code sessions using the `claude` CLI in non-interactive mode.

use std::process::{Command, Stdio};

use super::{
    build_spawn_env, AgentRuntime, AgentSessionResult, AgentSpawnOptions, BackgroundHandle,
    MonitoredChild,
};
use super::AgentType;
use crate::error::{AikiError, Result};

/// Runtime for Claude Code agent
pub struct ClaudeCodeRuntime;

impl ClaudeCodeRuntime {
    /// Create a new Claude Code runtime
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClaudeCodeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRuntime for ClaudeCodeRuntime {
    fn spawn_blocking(&self, options: &AgentSpawnOptions) -> Result<AgentSessionResult> {
        let prompt = options.task_prompt();

        // Spawn claude process with prompt via command args
        // Uses --print for non-interactive mode and --dangerously-skip-permissions
        // to allow the agent to run without user confirmation
        let mut cmd = Command::new("claude");
        cmd.current_dir(&options.cwd)
            .args(["--print", "--dangerously-skip-permissions", &prompt])
            // Unset nesting guard so child Claude Code sessions can start
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .envs(build_spawn_env(options, "background"));
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
                "Failed to spawn claude: {}",
                e
            ))),
        }
    }

    fn spawn_background(&self, options: &AgentSpawnOptions) -> Result<BackgroundHandle> {
        let prompt = options.task_prompt();

        // Spawn claude process detached from parent
        // Uses --print for non-interactive mode and --dangerously-skip-permissions
        // The process runs independently and continues after parent exits
        let mut cmd = Command::new("claude");
        cmd.current_dir(&options.cwd)
            .args(["--print", "--dangerously-skip-permissions", &prompt])
            // Unset nesting guard so child Claude Code sessions can start
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .envs(build_spawn_env(options, "background"))
            // Detach stdin/stdout/stderr so process runs independently
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd.spawn();

        match child {
            Ok(_child) => Ok(BackgroundHandle {
                thread: options.thread.clone(),
                session_id: None,
                agent_type: AgentType::ClaudeCode,
            }),
            Err(e) => Err(AikiError::AgentSpawnFailed(format!(
                "Failed to spawn claude in background: {}",
                e
            ))),
        }
    }

    fn spawn_monitored(&self, options: &AgentSpawnOptions) -> Result<MonitoredChild> {
        let prompt = options.task_prompt();

        // Spawn claude process - keep Child handle for monitoring
        // Uses --print for non-interactive mode and --dangerously-skip-permissions
        let mut cmd = Command::new("claude");
        cmd.current_dir(&options.cwd)
            .args(["--print", "--dangerously-skip-permissions", &prompt])
            // Unset nesting guard so child Claude Code sessions can start
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .envs(build_spawn_env(options, "monitored"))
            // Detach stdin so process runs independently
            .stdin(Stdio::null())
            // Capture stdout/stderr so failures surface the real CLI message
            .stdout(Stdio::piped())
            // Capture stderr so we can report errors when the agent fails
            .stderr(Stdio::piped());
        let child = cmd.spawn();

        match child {
            Ok(child) => Ok(MonitoredChild::new(child)),
            Err(e) => Err(AikiError::AgentSpawnFailed(format!(
                "Failed to spawn claude for monitoring: {}",
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
        assert_eq!(extract_summary("   \n  \n  "), "Task completed");
    }

    #[test]
    fn test_extract_summary_short() {
        let output = "Fixed the bug.\nTests pass.";
        let summary = extract_summary(output);
        assert!(summary.contains("Fixed the bug"));
        assert!(summary.contains("Tests pass"));
    }

    #[test]
    fn test_extract_summary_long() {
        // Create output longer than 500 chars
        let long_output = (0..100)
            .map(|i| format!("Line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let summary = extract_summary(&long_output);
        assert!(summary.len() <= 600); // Allow some margin
    }
}
