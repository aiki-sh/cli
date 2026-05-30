use std::path::Path;

use crate::provenance::record::AgentType;
use crate::session::{AikiSession, SessionMode};

/// Create a session for Claude Code events
///
/// This helper ensures consistent session creation across all Claude Code event builders.
/// For SessionStart, detects version (~135ms) and caches in session file.
/// For other events, reads cached version from file (~0ms).
pub fn create_session(session_id: &str, cwd: &str) -> AikiSession {
    let repo_path = Path::new(cwd);
    let agent_version = get_agent_version(session_id, repo_path);

    AikiSession::for_hook(AgentType::ClaudeCode, session_id, agent_version)
}

/// Get agent version from cache or detect it
///
/// For SessionStart events, detects version and caches it in session file.
/// For other events, reads cached version from session file (fast).
/// Falls back to detection if cache read fails.
fn get_agent_version(session_id: &str, repo_path: &Path) -> Option<String> {
    // Compute session file path directly without creating full session object
    let session_uuid = AikiSession::generate_uuid(AgentType::ClaudeCode, session_id);
    let session_file_path = repo_path.join(".aiki/sessions").join(&session_uuid);

    // Try to read cached version from session file
    if let Some(cached_version) = read_agent_version_from_file(&session_file_path) {
        return Some(cached_version);
    }

    // No cache - detect version (this happens on SessionStart or if file missing)
    crate::editors::npm::get_version("@anthropic-ai/claude-code", "claude")
}

/// Read agent_version from session file
fn read_agent_version_from_file(path: &Path) -> Option<String> {
    use std::fs;
    fs::read_to_string(path).ok().and_then(|content| {
        content
            .lines()
            .find(|line| line.starts_with("agent_version="))
            .and_then(|line| line.strip_prefix("agent_version="))
            .map(|v| v.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::record::DetectionMethod;

    #[test]
    fn test_create_session_includes_version() {
        let session_id = "test-session-123";
        let repo_path = Path::new("/tmp");
        let agent_version = get_agent_version(session_id, repo_path);

        // Create session using the detected version
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            session_id,
            agent_version.clone(),
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );

        // Verify session was created
        assert_eq!(session.agent_type(), AgentType::ClaudeCode);
        assert_eq!(session.external_id(), session_id);
        assert_eq!(session.detection_method(), &DetectionMethod::Hook);

        // Check if version was detected (may be None if claude not in PATH)
        if let Some(version) = agent_version {
            println!("Session created with Claude Code version: {}", version);
            assert!(!version.is_empty());
        } else {
            println!("Session created without version (claude not in PATH)");
        }
    }
}
