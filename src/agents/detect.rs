//! Agent detection via process tree walking
//!
//! Detects the current agent by walking up the process tree and matching
//! known process names/paths.

use super::AgentType;
use sysinfo::{Pid, ProcessesToUpdate, System};

/// Detect the current agent type by walking up the process tree
///
/// Inspects parent processes looking for known agent signatures:
/// - "claude" -> ClaudeCode
/// - "cursor" -> Cursor
/// - "code" (with Cursor in path) -> Cursor
/// - "codex" -> Codex
/// - "gemini" -> Gemini
///
/// Returns None if no known agent is detected (likely human terminal).
pub fn detect_agent_from_process_tree() -> Option<AgentType> {
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

        // Check for known agents
        if let Some(agent) = match_agent(&name, &exe_path) {
            return Some(agent);
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

/// Match process name/path to known agent types
fn match_agent(name: &str, exe_path: &str) -> Option<AgentType> {
    // Claude Code
    if name.contains("claude") {
        return Some(AgentType::ClaudeCode);
    }

    // Cursor (check before generic "code" since Cursor is Electron-based)
    if name.contains("cursor") || exe_path.contains("cursor") {
        return Some(AgentType::Cursor);
    }

    // Codex
    if name.contains("codex") {
        return Some(AgentType::Codex);
    }

    // Gemini
    if name.contains("gemini") {
        return Some(AgentType::Gemini);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_agent_claude() {
        assert_eq!(
            match_agent("claude", ""),
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(
            match_agent("claude-code", "/usr/local/bin/claude"),
            Some(AgentType::ClaudeCode)
        );
    }

    #[test]
    fn test_match_agent_cursor() {
        assert_eq!(
            match_agent("cursor", ""),
            Some(AgentType::Cursor)
        );
        // Note: paths are lowercased in detect_agent_from_process_tree before calling match_agent
        assert_eq!(
            match_agent("cursor helper", "/applications/cursor.app/contents/macos/cursor helper"),
            Some(AgentType::Cursor)
        );
        // Code binary inside Cursor app - detected via exe_path
        assert_eq!(
            match_agent("code", "/applications/cursor.app/contents/resources/app/bin/code"),
            Some(AgentType::Cursor)
        );
    }

    #[test]
    fn test_match_agent_codex() {
        assert_eq!(
            match_agent("codex", ""),
            Some(AgentType::Codex)
        );
    }

    #[test]
    fn test_match_agent_gemini() {
        assert_eq!(
            match_agent("gemini", ""),
            Some(AgentType::Gemini)
        );
    }

    #[test]
    fn test_match_agent_unknown() {
        assert_eq!(match_agent("bash", "/bin/bash"), None);
        assert_eq!(match_agent("zsh", "/bin/zsh"), None);
        assert_eq!(match_agent("fish", "/usr/local/bin/fish"), None);
    }

    #[test]
    fn test_detect_returns_something() {
        // This test just verifies the function runs without panicking
        // The actual result depends on the test environment
        let _result = detect_agent_from_process_tree();
    }
}
