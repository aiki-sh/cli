use std::path::Path;

use crate::cache::debug_log;
use crate::provenance::record::AgentType;
use crate::session::AikiSession;

/// Create a session for Codex events
///
/// This helper ensures consistent session creation across all Codex event builders.
/// For SessionStart, detects version (~10ms) and caches in session file.
/// For other events, reads cached version from file (~0ms).
pub fn create_session(session_id: &str, cwd: &str) -> AikiSession {
    let repo_path = Path::new(cwd);
    let agent_version = get_agent_version(session_id, repo_path);

    AikiSession::for_hook(AgentType::Codex, session_id, agent_version)
}

/// Get agent version from cache or detect it
///
/// For SessionStart events, detects version and caches it in session file.
/// For other events, reads cached version from session file (fast).
/// Falls back to detection if cache read fails.
fn get_agent_version(session_id: &str, repo_path: &Path) -> Option<String> {
    // Compute session file path directly without creating full session object
    let session_uuid = AikiSession::generate_uuid(AgentType::Codex, session_id);
    let session_file_path = repo_path.join(".aiki/sessions").join(&session_uuid);

    // Try to read cached version from session file
    if let Some(cached_version) = read_agent_version_from_file(&session_file_path) {
        return Some(cached_version);
    }

    // No cache - detect version (this happens on SessionStart or if file missing)
    detect_codex_version()
}

/// Detect Codex version by running `codex --version`
fn detect_codex_version() -> Option<String> {
    let output = std::process::Command::new("codex")
        .arg("--version")
        .output()
        .ok()?;

    if !output.status.success() {
        debug_log(|| "codex --version returned non-zero exit code".to_string());
        return None;
    }

    let version_str = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if version_str.is_empty() {
        debug_log(|| "codex --version returned empty output".to_string());
        return None;
    }

    // Handle output like "codex 0.1.0" or just "0.1.0"
    let version = version_str
        .strip_prefix("codex ")
        .unwrap_or(&version_str)
        .to_string();

    Some(version)
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
    use crate::session::SessionMode;

    #[test]
    fn test_create_session_includes_version() {
        let session_id = "test-codex-session-123";
        let repo_path = Path::new("/tmp");
        let agent_version = get_agent_version(session_id, repo_path);

        // Create session using the detected version
        let session = AikiSession::new(
            AgentType::Codex,
            session_id,
            agent_version.clone(),
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );

        // Verify session was created
        assert_eq!(session.agent_type(), AgentType::Codex);
        assert_eq!(session.external_id(), session_id);
        assert_eq!(session.detection_method(), &DetectionMethod::Hook);

        // Check if version was detected (may be None if codex not in PATH)
        if let Some(version) = agent_version {
            println!("Session created with Codex version: {}", version);
            assert!(!version.is_empty());
        } else {
            println!("Session created without version (codex not in PATH)");
        }
    }

    #[test]
    fn test_create_session_captures_parent_pid() {
        let session = create_session("test-codex-parent-pid", "/tmp");
        assert_eq!(session.agent_type(), AgentType::Codex);
        assert_eq!(session.detection_method(), &DetectionMethod::Hook);
        assert!(session.parent_pid().is_some());
    }

    #[test]
    fn test_detect_codex_version_format() {
        // If codex is installed, verify version format
        if let Some(version) = detect_codex_version() {
            assert!(!version.is_empty(), "Version should not be empty");
            // Should not contain the "codex " prefix
            assert!(
                !version.starts_with("codex "),
                "Version should not contain 'codex ' prefix"
            );
            println!("Detected Codex version: {}", version);
        } else {
            println!("Codex not detected (not installed or not in PATH)");
        }
    }

    #[test]
    fn test_read_agent_version_from_file() {
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_codex_session_version");
        std::fs::write(
            &test_file,
            "session_id=test\nagent_version=0.1.0\nagent_type=codex\n",
        )
        .unwrap();

        let version = read_agent_version_from_file(&test_file);
        assert_eq!(version, Some("0.1.0".to_string()));

        std::fs::remove_file(test_file).ok();
    }

    #[test]
    fn test_read_agent_version_from_missing_file() {
        let version = read_agent_version_from_file(Path::new("/nonexistent/path"));
        assert_eq!(version, None);
    }
}
