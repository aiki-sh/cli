//! Codex runtime implementation
//!
//! Spawns Codex sessions using the `codex` CLI in non-interactive mode.

use std::process::{Command, Stdio};

use super::{AgentRuntime, AgentSessionResult, AgentSpawnOptions, BackgroundHandle, MonitoredChild};
use crate::agents::AgentType;
use crate::error::{AikiError, Result};

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
    fn agent_type(&self) -> AgentType {
        AgentType::Codex
    }

    fn spawn_blocking(&self, options: &AgentSpawnOptions) -> Result<AgentSessionResult> {
        let prompt = options.task_prompt();

        // Spawn codex process with prompt
        // Uses `codex exec` for non-interactive execution
        // --full-auto enables workspace writes with sandbox protection (-a on-request, --sandbox workspace-write)
        let mut cmd = Command::new("codex");
        cmd.current_dir(&options.cwd)
            .args(["exec", "--full-auto", &prompt]);
        // Propagate parent session UUID for workspace isolation chaining
        if let Some(ref uuid) = options.parent_session_uuid {
            cmd.env("AIKI_PARENT_SESSION_UUID", uuid);
        }
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
        // --full-auto enables workspace writes with sandbox protection (-a on-request, --sandbox workspace-write)
        let mut cmd = Command::new("codex");
        cmd.current_dir(&options.cwd)
            .args(["exec", "--full-auto", &prompt])
            // Pass task ID so session system can track this as a task-driven session
            .env("AIKI_TASK", &options.task_id)
            // Mark as background session for mode detection
            .env("AIKI_SESSION_MODE", "background")
            // Detach stdin/stdout/stderr so process runs independently
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Propagate parent session UUID for workspace isolation chaining
        if let Some(ref uuid) = options.parent_session_uuid {
            cmd.env("AIKI_PARENT_SESSION_UUID", uuid);
        }
        let child = cmd.spawn();

        match child {
            Ok(child) => {
                let pid = child.id();
                Ok(BackgroundHandle {
                    pid,
                    task_id: options.task_id.clone(),
                })
            }
            Err(e) => Err(AikiError::AgentSpawnFailed(format!(
                "Failed to spawn codex in background: {}",
                e
            ))),
        }
    }

    fn spawn_monitored(&self, options: &AgentSpawnOptions) -> Result<MonitoredChild> {
        let prompt = options.task_prompt();

        // Spawn codex process - keep Child handle for monitoring
        // --full-auto enables workspace writes with sandbox protection (-a on-request, --sandbox workspace-write)
        let mut cmd = Command::new("codex");
        cmd.current_dir(&options.cwd)
            .args(["exec", "--full-auto", &prompt])
            // Pass task ID so session system can track this as a task-driven session
            .env("AIKI_TASK", &options.task_id)
            // Mark as monitored session for mode detection
            .env("AIKI_SESSION_MODE", "monitored")
            // Detach stdin/stdout so process runs independently
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // Capture stderr so we can report errors when the agent fails
            .stderr(Stdio::piped());
        // Propagate parent session UUID for workspace isolation chaining
        if let Some(ref uuid) = options.parent_session_uuid {
            cmd.env("AIKI_PARENT_SESSION_UUID", uuid);
        }
        let child = cmd.spawn();

        match child {
            Ok(child) => Ok(MonitoredChild::new(child, &options.task_id)),
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
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();

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
    fn test_codex_runtime_agent_type() {
        let runtime = CodexRuntime::new();
        assert_eq!(runtime.agent_type(), AgentType::Codex);
    }

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
