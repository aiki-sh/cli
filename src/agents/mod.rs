//! Agent types and assignee definitions
//!
//! This module provides the canonical agent type definitions used throughout Aiki,
//! as well as the runtime abstraction for spawning agent sessions.

mod detect;
pub mod runtime;
mod types;

pub use detect::detect_agent_from_process_tree;
pub use runtime::{
    get_runtime, AgentRuntime, AgentSessionResult, AgentSpawnOptions, BackgroundHandle,
};
pub use types::{AgentType, Assignee};

/// Agent types that have a runtime and can be spawned.
const SPAWNABLE_AGENTS: &[AgentType] = &[AgentType::ClaudeCode, AgentType::Codex];

/// Returns agents that have a runtime AND whose CLI binary is installed.
pub fn get_available_agents() -> Vec<AgentType> {
    SPAWNABLE_AGENTS
        .iter()
        .filter(|a| a.is_installed())
        .cloned()
        .collect()
}

/// Returns the default coder agent — the first available installed agent.
pub fn determine_default_coder() -> crate::error::Result<AgentType> {
    get_available_agents()
        .into_iter()
        .next()
        .ok_or(crate::error::AikiError::NoAgentsAvailable)
}

/// Check if a named agent is available (has runtime + installed).
pub fn is_agent_available(agent: &str) -> bool {
    AgentType::from_str(agent)
        .map(|a| SPAWNABLE_AGENTS.contains(&a) && a.is_installed())
        .unwrap_or(false)
}
/// Determine who should review work done by `worker`.
///
/// Dynamically resolves a reviewer from installed agents. Prefers cross-review
/// (a different agent than the worker), falls back to self-review when only one
/// agent is installed, and errors when no agents are available.
///
/// # Arguments
/// * `worker` - The agent that did the work (e.g., "claude-code", "codex")
///
/// # Returns
/// The agent name that should perform the review
///
/// # Errors
/// Returns `AikiError::NoAgentsAvailable` if no agent CLIs are installed
pub fn determine_reviewer(worker: Option<&str>) -> crate::error::Result<String> {
    determine_reviewer_with(worker, &get_available_agents())
}

/// Like [`determine_reviewer`] but accepts an explicit agent list, making it
/// easy to test without depending on which CLIs are installed on the host.
pub fn determine_reviewer_with(
    worker: Option<&str>,
    available: &[AgentType],
) -> crate::error::Result<String> {
    if available.is_empty() {
        return Err(crate::error::AikiError::NoAgentsAvailable);
    }

    match worker {
        Some(worker_name) => {
            // Normalize alias to canonical name (e.g. "claude" → "claude-code")
            let canonical_name = AgentType::from_str(worker_name)
                .map(|a| a.as_str())
                .unwrap_or(worker_name);

            // Try cross-review: find an available agent different from the worker
            if let Some(reviewer) = available.iter().find(|a| a.as_str() != canonical_name) {
                return Ok(reviewer.as_str().to_string());
            }
            // Only the worker agent is installed — self-review
            if available.iter().any(|a| a.as_str() == canonical_name) {
                eprintln!("Warning: Only one agent installed, using self-review");
                return Ok(canonical_name.to_string());
            }
            // Worker not installed, return first available
            Ok(available[0].as_str().to_string())
        }
        None => Ok(available[0].as_str().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alias_cross_review() {
        // "claude" alias should resolve to claude-code and pick codex for cross-review
        let result =
            determine_reviewer_with(Some("claude"), &[AgentType::ClaudeCode, AgentType::Codex])
                .unwrap();
        assert_eq!(result, "codex");
    }

    #[test]
    fn test_alias_self_review() {
        // "claude" alias with only ClaudeCode available → self-review with canonical name
        let result = determine_reviewer_with(Some("claude"), &[AgentType::ClaudeCode]).unwrap();
        assert_eq!(result, "claude-code");
    }

    #[test]
    fn test_canonical_cross_review() {
        // Canonical name still works for cross-review
        let result = determine_reviewer_with(
            Some("claude-code"),
            &[AgentType::ClaudeCode, AgentType::Codex],
        )
        .unwrap();
        assert_eq!(result, "codex");
    }

    #[test]
    fn test_unknown_agent_falls_through() {
        // Unknown agent name falls through to first available
        let result =
            determine_reviewer_with(Some("unknown-agent"), &[AgentType::ClaudeCode]).unwrap();
        assert_eq!(result, "claude-code");
    }

    #[test]
    fn test_no_agents_errors() {
        let result = determine_reviewer_with(Some("claude"), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_none_worker_returns_first() {
        let result =
            determine_reviewer_with(None, &[AgentType::Codex, AgentType::ClaudeCode]).unwrap();
        assert_eq!(result, "codex");
    }
}
