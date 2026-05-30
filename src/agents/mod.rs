//! Agent types and assignee definitions
//!
//! This module provides the canonical agent type definitions used throughout Aiki,
//! as well as the runtime abstraction for spawning agent sessions.

mod detect;
pub mod runtime;
mod types;

pub use detect::detect_agent_from_process_tree;
#[allow(unused_imports)]
pub use runtime::{
    get_runtime, AgentRuntime, AgentSessionResult, AgentSpawnOptions, BackgroundHandle,
    MonitoredChild,
};
pub use types::{AgentType, Assignee};

/// Determine who should review work done by `worker`.
///
/// Returns the agent that should review the work. Currently hardcoded to
/// claude-code/codex pairing. See ops/next/support-all-runtimes.md for
/// future plans to make this dynamic based on available runtimes.
///
/// # Arguments
/// * `worker` - The agent that did the work (e.g., "claude-code", "codex")
///
/// # Returns
/// The agent that should review (opposite of worker, or fallback to "codex")
pub fn determine_reviewer(worker: Option<&str>) -> String {
    match worker {
        Some("claude-code") => "codex".to_string(),
        Some("codex") => "claude-code".to_string(),
        _ => "codex".to_string(), // fallback
    }
}
