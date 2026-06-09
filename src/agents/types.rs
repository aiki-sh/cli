//! Canonical agent type definitions
//!
//! This module provides the single source of truth for agent types
//! used throughout Aiki.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Type of AI agent
///
/// This is the canonical enum used throughout the codebase.
/// Naming uses `ClaudeCode` (not `Claude`) to match CLI conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentType {
    #[serde(alias = "ClaudeCode")]
    ClaudeCode,
    #[serde(alias = "Codex")]
    Codex,
    #[serde(alias = "Cursor")]
    Cursor,
    #[serde(alias = "Gemini")]
    Gemini,
    #[serde(alias = "Unknown")]
    Unknown,
}

impl AgentType {
    /// Parse from string with alias support
    ///
    /// Accepts: "claude-code", "claude", "codex", "cursor", "gemini", "unknown"
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "claude-code" | "claude" => Some(AgentType::ClaudeCode),
            "codex" => Some(AgentType::Codex),
            "cursor" => Some(AgentType::Cursor),
            "gemini" => Some(AgentType::Gemini),
            "unknown" => Some(AgentType::Unknown),
            _ => None,
        }
    }

    /// Get the canonical string identifier (for CLI and metadata)
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentType::ClaudeCode => "claude-code",
            AgentType::Codex => "codex",
            AgentType::Cursor => "cursor",
            AgentType::Gemini => "gemini",
            AgentType::Unknown => "unknown",
        }
    }

    /// Get the lowercase identifier for provenance metadata serialization
    ///
    /// Note: Uses "claude" not "claude-code" for backward compatibility
    /// with existing [aiki] blocks in change descriptions.
    #[must_use]
    pub fn to_metadata_string(&self) -> &'static str {
        match self {
            AgentType::ClaudeCode => "claude",
            AgentType::Codex => "codex",
            AgentType::Cursor => "cursor",
            AgentType::Gemini => "gemini",
            AgentType::Unknown => "unknown",
        }
    }

    /// Get the email address for this agent type
    #[must_use]
    pub fn email(&self) -> &'static str {
        match self {
            AgentType::ClaudeCode => "noreply@anthropic.com",
            AgentType::Codex => "noreply@openai.com",
            AgentType::Cursor => "noreply@cursor.com",
            AgentType::Gemini => "noreply@google.com",
            AgentType::Unknown => "noreply@aiki.sh",
        }
    }

    /// Format as a git author string (name + email)
    #[must_use]
    pub fn git_author(&self) -> String {
        format!("{} <{}>", self.display_name(), self.email())
    }

    /// Get the display name (capitalized, for user-facing output)
    #[must_use]
    pub fn display_name(&self) -> &'static str {
        match self {
            AgentType::ClaudeCode => "Claude",
            AgentType::Codex => "Codex",
            AgentType::Cursor => "Cursor",
            AgentType::Gemini => "Gemini",
            AgentType::Unknown => "Unknown",
        }
    }

    /// Get the CLI binary name for spawnable agents.
    ///
    /// Returns `Some("binary")` for agents that have a runtime and can be spawned,
    /// `None` for agents that can't be spawned (e.g., Cursor, Gemini).
    #[must_use]
    pub fn cli_binary(&self) -> Option<&'static str> {
        match self {
            AgentType::ClaudeCode => Some("claude"),
            AgentType::Codex => Some("codex"),
            AgentType::Cursor | AgentType::Gemini | AgentType::Unknown => None,
        }
    }

    /// Check if this agent's CLI binary is available on PATH.
    ///
    /// Returns `true` only for spawnable agents whose binary is found.
    /// Does NOT run `--version` or any other subcommand — just checks existence.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        let Some(binary) = self.cli_binary() else {
            return false;
        };
        which::which(binary).is_ok()
    }

    /// Get platform-specific install instructions for this agent
    #[must_use]
    pub fn install_hint(&self) -> String {
        match self {
            AgentType::ClaudeCode => {
                if cfg!(target_os = "macos") {
                    "Install: brew install claude-code (or: npm install -g @anthropic-ai/claude-code)".to_string()
                } else {
                    "Install: npm install -g @anthropic-ai/claude-code".to_string()
                }
            }
            AgentType::Codex => "Install: npm install -g @openai/codex".to_string(),
            AgentType::Cursor => {
                "Install Cursor from https://cursor.com (task execution not yet supported)"
                    .to_string()
            }
            AgentType::Gemini => "Gemini task execution not yet supported".to_string(),
            AgentType::Unknown => {
                "No install instructions available for unknown agent type".to_string()
            }
        }
    }
}

impl fmt::Display for AgentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Task assignee - who should work on this task
///
/// Assignees control task visibility and routing:
/// - `Agent(type)`: Only visible to that agent type
/// - `Human`: Only visible to humans (excluded from agent context)
/// - `Unassigned`: Visible to all agents and humans
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assignee {
    /// Assigned to a specific AI agent
    Agent(AgentType),
    /// Assigned to a human developer
    Human,
    /// Unassigned - visible to all
    Unassigned,
}

impl Assignee {
    /// Parse assignee from string
    ///
    /// Accepts:
    /// - Agent types: "claude-code", "claude", "codex", "cursor", "gemini"
    /// - Human: "human", "me"
    /// - Unassigned: "" (empty string)
    ///
    /// Returns None for unrecognized values.
    pub fn from_str(s: &str) -> Option<Self> {
        if s.is_empty() {
            return Some(Assignee::Unassigned);
        }

        match s.to_lowercase().as_str() {
            "human" | "me" => Some(Assignee::Human),
            _ => AgentType::from_str(s).map(Assignee::Agent),
        }
    }

    /// Check if this task is visible to the given agent
    ///
    /// Rules:
    /// - Unassigned: visible to all agents
    /// - Human: NOT visible to any agent
    /// - Agent(X): only visible to agent X
    #[must_use]
    pub fn is_visible_to(&self, agent: &AgentType) -> bool {
        match self {
            Assignee::Unassigned => true,
            Assignee::Human => false,
            Assignee::Agent(a) => a == agent,
        }
    }

    /// Check if this task is visible to humans
    ///
    /// Rules:
    /// - Unassigned: visible to humans
    /// - Human: visible to humans
    /// - Agent(X): NOT visible to humans
    #[must_use]
    pub fn is_visible_to_human(&self) -> bool {
        match self {
            Assignee::Unassigned => true,
            Assignee::Human => true,
            Assignee::Agent(_) => false,
        }
    }

    /// Get the string representation for storage/display
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Assignee::Agent(a) => Some(a.as_str()),
            Assignee::Human => Some("human"),
            Assignee::Unassigned => None,
        }
    }
}

impl fmt::Display for Assignee {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Assignee::Agent(a) => write!(f, "{}", a),
            Assignee::Human => write!(f, "human"),
            Assignee::Unassigned => write!(f, "unassigned"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_type_from_str() {
        assert_eq!(
            AgentType::from_str("claude-code"),
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(AgentType::from_str("claude"), Some(AgentType::ClaudeCode));
        assert_eq!(AgentType::from_str("CLAUDE"), Some(AgentType::ClaudeCode));
        assert_eq!(AgentType::from_str("codex"), Some(AgentType::Codex));
        assert_eq!(AgentType::from_str("cursor"), Some(AgentType::Cursor));
        assert_eq!(AgentType::from_str("gemini"), Some(AgentType::Gemini));
        assert_eq!(AgentType::from_str("unknown"), Some(AgentType::Unknown));
        assert_eq!(AgentType::from_str("invalid"), None);
    }

    #[test]
    fn test_agent_type_as_str() {
        assert_eq!(AgentType::ClaudeCode.as_str(), "claude-code");
        assert_eq!(AgentType::Codex.as_str(), "codex");
        assert_eq!(AgentType::Cursor.as_str(), "cursor");
        assert_eq!(AgentType::Gemini.as_str(), "gemini");
        assert_eq!(AgentType::Unknown.as_str(), "unknown");
    }

    #[test]
    fn test_agent_type_to_metadata_string() {
        // Claude uses "claude" for backward compatibility
        assert_eq!(AgentType::ClaudeCode.to_metadata_string(), "claude");
        assert_eq!(AgentType::Codex.to_metadata_string(), "codex");
        assert_eq!(AgentType::Cursor.to_metadata_string(), "cursor");
    }

    #[test]
    fn test_agent_type_display_name() {
        assert_eq!(AgentType::ClaudeCode.display_name(), "Claude");
        assert_eq!(AgentType::Codex.display_name(), "Codex");
    }

    #[test]
    fn test_agent_type_deserializes_legacy_pascal_case() {
        assert_eq!(
            serde_yaml::from_str::<AgentType>("ClaudeCode").unwrap(),
            AgentType::ClaudeCode
        );
        assert_eq!(
            serde_yaml::from_str::<AgentType>("Codex").unwrap(),
            AgentType::Codex
        );
        assert_eq!(
            serde_yaml::from_str::<AgentType>("Cursor").unwrap(),
            AgentType::Cursor
        );
        assert_eq!(
            serde_yaml::from_str::<AgentType>("Gemini").unwrap(),
            AgentType::Gemini
        );
        assert_eq!(
            serde_yaml::from_str::<AgentType>("Unknown").unwrap(),
            AgentType::Unknown
        );
    }

    #[test]
    fn test_assignee_from_str() {
        assert_eq!(
            Assignee::from_str("claude-code"),
            Some(Assignee::Agent(AgentType::ClaudeCode))
        );
        assert_eq!(
            Assignee::from_str("claude"),
            Some(Assignee::Agent(AgentType::ClaudeCode))
        );
        assert_eq!(Assignee::from_str("human"), Some(Assignee::Human));
        assert_eq!(Assignee::from_str("me"), Some(Assignee::Human));
        assert_eq!(Assignee::from_str(""), Some(Assignee::Unassigned));
        assert_eq!(Assignee::from_str("invalid"), None);
    }

    #[test]
    fn test_assignee_visibility_to_agent() {
        let claude = AgentType::ClaudeCode;
        let codex = AgentType::Codex;

        // Unassigned visible to all
        assert!(Assignee::Unassigned.is_visible_to(&claude));
        assert!(Assignee::Unassigned.is_visible_to(&codex));

        // Human not visible to agents
        assert!(!Assignee::Human.is_visible_to(&claude));
        assert!(!Assignee::Human.is_visible_to(&codex));

        // Agent-specific visibility
        assert!(Assignee::Agent(AgentType::ClaudeCode).is_visible_to(&claude));
        assert!(!Assignee::Agent(AgentType::ClaudeCode).is_visible_to(&codex));
    }

    #[test]
    fn test_assignee_visibility_to_human() {
        assert!(Assignee::Unassigned.is_visible_to_human());
        assert!(Assignee::Human.is_visible_to_human());
        assert!(!Assignee::Agent(AgentType::ClaudeCode).is_visible_to_human());
    }

    #[test]
    fn test_assignee_as_str() {
        assert_eq!(
            Assignee::Agent(AgentType::ClaudeCode).as_str(),
            Some("claude-code")
        );
        assert_eq!(Assignee::Human.as_str(), Some("human"));
        assert_eq!(Assignee::Unassigned.as_str(), None);
    }
}
