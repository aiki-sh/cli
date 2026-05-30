//! Core types for the prompt history system

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// Re-export AgentType from canonical location
pub use crate::agents::AgentType;
// Re-export SessionMode for history tracking
pub use crate::session::SessionMode;

/// Source of a turn (user prompt or autoreply)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TurnSource {
    /// User-initiated turn (from prompt submission)
    User,
    /// Aiki-initiated turn (from autoreply context injection)
    Autoreply,
}

impl std::fmt::Display for TurnSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TurnSource::User => write!(f, "user"),
            TurnSource::Autoreply => write!(f, "autoreply"),
        }
    }
}

/// Branch name for storing conversation history
pub const CONVERSATIONS_BRANCH: &str = "aiki/conversations";

/// Metadata block start marker
pub const METADATA_START: &str = "[aiki-conversation]";

/// Metadata block end marker
pub const METADATA_END: &str = "[/aiki-conversation]";

/// Events stored on aiki/conversations branch
#[derive(Debug, Clone)]
pub enum ConversationEvent {
    /// User prompt submitted
    Prompt {
        session_id: String,
        agent_type: AgentType,
        /// Sequential turn number within session (starts at 1)
        turn: u32,
        /// Source of this turn (user or autoreply)
        source: TurnSource,
        content: String,
        /// References to injected context files (not full content)
        injected_refs: Vec<String>,
        timestamp: DateTime<Utc>,
        /// Stable repository identifier (from repo-id file)
        repo_id: Option<String>,
        /// Current working directory where the event occurred
        cwd: Option<String>,
    },
    /// Agent response received
    Response {
        session_id: String,
        agent_type: AgentType,
        /// Sequential turn number within session (matches the Prompt turn)
        turn: u32,
        /// Files written/modified during this response
        files_written: Vec<String>,
        /// Full response text
        content: Option<String>,
        timestamp: DateTime<Utc>,
        /// Stable repository identifier (from repo-id file)
        repo_id: Option<String>,
        /// Current working directory where the event occurred
        cwd: Option<String>,
    },
    /// Session started
    SessionStart {
        session_id: String,
        agent_type: AgentType,
        timestamp: DateTime<Utc>,
        /// Stable repository identifier (from repo-id file)
        repo_id: Option<String>,
        /// Current working directory where the event occurred
        cwd: Option<String>,
        /// Session mode (background or interactive)
        session_mode: Option<SessionMode>,
    },
    /// Session ended
    SessionEnd {
        session_id: String,
        timestamp: DateTime<Utc>,
        /// Reason for termination (e.g., "clear", "logout", "ttl_expired", "pid_dead")
        reason: String,
        /// Stable repository identifier (from repo-id file)
        repo_id: Option<String>,
        /// Current working directory where the event occurred
        cwd: Option<String>,
    },
    /// Autoreply generated (pending injection into next turn)
    Autoreply {
        session_id: String,
        agent_type: AgentType,
        /// Turn that generated this autoreply
        turn: u32,
        content: String,
        timestamp: DateTime<Utc>,
        /// Stable repository identifier (from repo-id file)
        repo_id: Option<String>,
        /// Current working directory where the event occurred
        cwd: Option<String>,
    },
}

/// Materialized session view (computed from events)
#[derive(Debug, Clone)]
#[allow(dead_code)] // Part of history API
pub struct Session {
    pub id: String,
    pub agent_type: AgentType,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    /// First line of first prompt (for display)
    pub summary: Option<String>,
}

/// Materialized log entry (from response events)
#[derive(Debug, Clone)]
#[allow(dead_code)] // Part of history API
pub struct LogEntry {
    pub session_id: String,
    pub agent_type: AgentType,
    pub files_written: Vec<String>,
    pub summary: Option<String>,
    pub timestamp: DateTime<Utc>,
}

/// Summary of a conversation for listing purposes
#[derive(Debug, Clone)]
#[allow(dead_code)] // Part of history API
pub struct ConversationSummary {
    pub session_id: String,
    pub agent_type: AgentType,
    pub started_at: DateTime<Utc>,
    pub turn_count: u32,
    pub last_activity: DateTime<Utc>,
    pub repo_id: Option<String>,
    /// Session mode (background or interactive), if known from SessionStart event
    pub session_mode: Option<SessionMode>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_type_display() {
        assert_eq!(AgentType::ClaudeCode.to_string(), "claude-code");
        assert_eq!(AgentType::Cursor.to_string(), "cursor");
        assert_eq!(AgentType::Unknown.to_string(), "unknown");
    }

    #[test]
    fn test_agent_type_from_str() {
        assert_eq!(AgentType::from_str("claude-code"), Some(AgentType::ClaudeCode));
        assert_eq!(AgentType::from_str("CURSOR"), Some(AgentType::Cursor));
        assert_eq!(AgentType::from_str("unknown"), Some(AgentType::Unknown));
        assert_eq!(AgentType::from_str("invalid"), None);
    }

    #[test]
    fn test_turn_source_display() {
        assert_eq!(TurnSource::User.to_string(), "user");
        assert_eq!(TurnSource::Autoreply.to_string(), "autoreply");
    }

    #[test]
    fn test_turn_source_equality() {
        assert_eq!(TurnSource::User, TurnSource::User);
        assert_eq!(TurnSource::Autoreply, TurnSource::Autoreply);
        assert_ne!(TurnSource::User, TurnSource::Autoreply);
    }

    #[test]
    fn test_turn_source_serde_roundtrip() {
        // Test User variant serializes to "user"
        let user_json = serde_json::to_string(&TurnSource::User).unwrap();
        assert_eq!(user_json, "\"user\"");
        let user_parsed: TurnSource = serde_json::from_str(&user_json).unwrap();
        assert_eq!(user_parsed, TurnSource::User);

        // Test Autoreply variant serializes to "autoreply"
        let autoreply_json = serde_json::to_string(&TurnSource::Autoreply).unwrap();
        assert_eq!(autoreply_json, "\"autoreply\"");
        let autoreply_parsed: TurnSource = serde_json::from_str(&autoreply_json).unwrap();
        assert_eq!(autoreply_parsed, TurnSource::Autoreply);
    }
}
