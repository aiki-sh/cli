//! Harness detection via process tree walking
//!
//! Detects the current harness by walking up the process tree and matching
//! known process names/paths against the harness registry.

use super::definition::HarnessDefinition;
use super::runtime::RuntimeKind;
use sysinfo::{Pid, ProcessesToUpdate, System};

/// Detect the parent harness from environment variables (fast — no process-tree
/// walk). Used by the CLI gate to pick agent-facing vs user-facing wording.
///
/// Env signatures:
/// - Claude Code: `CLAUDECODE=1` / `CLAUDE_CODE_ENTRYPOINT`
/// - Cursor: `CURSOR_TRACE_ID`
/// - Codex: `CODEX_HOME` set AND stdout non-TTY (an interactive human in a
///   Codex-configured shell still has a TTY)
///
/// Returns `None` for a plain human terminal (the TTY backstop covers harnesses
/// we haven't enumerated).
pub fn detect_parent_harness() -> Option<crate::agents::AgentType> {
    use crate::agents::AgentType;
    use std::io::IsTerminal;

    if std::env::var_os("CLAUDECODE").is_some()
        || std::env::var_os("CLAUDE_CODE_ENTRYPOINT").is_some()
    {
        return Some(AgentType::ClaudeCode);
    }
    if std::env::var_os("CURSOR_TRACE_ID").is_some() {
        return Some(AgentType::Cursor);
    }
    if std::env::var_os("CODEX_HOME").is_some() && !std::io::stdout().is_terminal() {
        return Some(AgentType::Codex);
    }
    None
}

/// Detect the current harness by walking up the process tree.
///
/// Inspects parent processes looking for known harness signatures. Only
/// harnesses with a CLI runtime are considered — harnesses registered for
/// identity-only (e.g. Cursor, Gemini) are skipped, since aiki can't drive
/// them as agents anyway.
///
/// Returns `None` if no known harness is detected (likely human terminal).
pub fn detect_harness_from_process_tree() -> Option<&'static HarnessDefinition> {
    let mut system = System::new();
    // Refresh all processes to populate the process tree
    system.refresh_processes(ProcessesToUpdate::All, true);

    let mut pid = Pid::from_u32(std::process::id());

    // Walk up the process tree
    loop {
        let Some(process) = system.process(pid) else {
            break;
        };

        let name = process.name().to_string_lossy().to_lowercase();
        let exe_path = process
            .exe()
            .map(|p| p.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        if let Some(harness) = match_harness(&name, &exe_path) {
            return Some(harness);
        }

        // Move to parent process
        let Some(parent_pid) = process.parent() else {
            break;
        };

        // Prevent infinite loop (shouldn't happen, but safety check)
        if parent_pid == pid {
            break;
        }

        pid = parent_pid;
    }

    None
}

/// Match process name/path to a registered harness.
///
/// Iterates all CLI harnesses and returns the first whose binary appears as a
/// case-insensitive substring in `name` or `exe_path`.
fn match_harness(name: &str, exe_path: &str) -> Option<&'static HarnessDefinition> {
    let name_lower = name.to_lowercase();
    let exe_lower = exe_path.to_lowercase();

    super::iter().find(|def| match def.runtime.as_ref().map(|r| &r.kind) {
        Some(RuntimeKind::Cli(cli)) => {
            let binary_lower = cli.binary.to_lowercase();
            name_lower.contains(&binary_lower) || exe_lower.contains(&binary_lower)
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentType;

    fn matched_agent_type(name: &str, exe_path: &str) -> Option<AgentType> {
        match_harness(name, exe_path).map(|h| h.identity.agent_type)
    }

    #[test]
    fn test_match_harness_claude() {
        assert_eq!(matched_agent_type("claude", ""), Some(AgentType::ClaudeCode));
        assert_eq!(
            matched_agent_type("claude-code", "/usr/local/bin/claude"),
            Some(AgentType::ClaudeCode)
        );
    }

    #[test]
    fn test_match_harness_codex() {
        assert_eq!(matched_agent_type("codex", ""), Some(AgentType::Codex));
    }

    // Cursor and Gemini have no CLI runtime, so detection deliberately
    // ignores them; tests for those agents were removed when the
    // CliRuntime migration landed.

    #[test]
    fn test_match_harness_unknown() {
        assert!(match_harness("bash", "/bin/bash").is_none());
        assert!(match_harness("zsh", "/bin/zsh").is_none());
        assert!(match_harness("fish", "/usr/local/bin/fish").is_none());
    }

    #[test]
    fn match_harness_substring_edge_cases() {
        // Case-insensitive: uppercase "CLAUDE" matches (registry-driven behavior)
        assert_eq!(matched_agent_type("CLAUDE", ""), Some(AgentType::ClaudeCode));

        // Substring overlap: "my-claude-tool" contains "claude" → matches
        assert_eq!(
            matched_agent_type("my-claude-tool", ""),
            Some(AgentType::ClaudeCode)
        );
    }

    #[test]
    fn match_harness_returns_full_definition() {
        // The harness returned must carry full identity, not just an AgentType.
        let h = match_harness("claude", "").expect("claude harness matches");
        assert_eq!(h.identity.id, "claude-code");
        assert_eq!(h.identity.agent_type, AgentType::ClaudeCode);
    }

    #[test]
    fn test_detect_returns_something() {
        // This test just verifies the function runs without panicking
        // The actual result depends on the test environment
        let _result = detect_harness_from_process_tree();
    }
}
