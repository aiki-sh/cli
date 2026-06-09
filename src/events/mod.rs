use crate::provenance::record::AgentType;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ============================================================================
// Result Types Module (contains HookResult, Decision, Failure)
// ============================================================================

pub mod result;

// ============================================================================
// Turn Info (shared across event payloads)
// ============================================================================

/// Turn metadata for events
///
/// Tracks turn number, deterministic ID, and source within a session.
/// Used by turn.started, turn.completed, and change.completed events.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Turn {
    /// Sequential turn number within session (starts at 1, 0 if unknown)
    #[serde(default)]
    pub number: u32,
    /// Deterministic turn identifier: uuid_v5(session_uuid, turn.to_string())
    #[serde(default)]
    pub id: String,
    /// Source of this turn: "user" or "autoreply"
    #[serde(default)]
    pub source: String,
}

impl Turn {
    /// Create a new Turn with the given values
    #[must_use]
    pub fn new(number: u32, id: String, source: String) -> Self {
        Self { number, id, source }
    }

    /// Create an empty/unknown turn (defaults)
    #[must_use]
    pub fn unknown() -> Self {
        Self::default()
    }

    /// Check if this turn has valid data (number > 0)
    #[must_use]
    pub fn is_known(&self) -> bool {
        self.number > 0
    }
}

// ============================================================================
// Token Usage
// ============================================================================

/// Token usage counters for a turn or session
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_created: u64,
}

#[allow(dead_code)]
impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input + self.output
    }

    /// Returns true if all fields are zero
    pub fn is_zero(&self) -> bool {
        self.input == 0 && self.output == 0 && self.cache_read == 0 && self.cache_created == 0
    }
}

impl std::ops::Add for TokenUsage {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self {
            input: self.input + rhs.input,
            output: self.output + rhs.output,
            cache_read: self.cache_read + rhs.cache_read,
            cache_created: self.cache_created + rhs.cache_created,
        }
    }
}

impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, rhs: Self) {
        self.input += rhs.input;
        self.output += rhs.output;
        self.cache_read += rhs.cache_read;
        self.cache_created += rhs.cache_created;
    }
}

impl std::iter::Sum for TokenUsage {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |a, b| a + b)
    }
}

// ============================================================================
// Main Event Enum
// ============================================================================

/// Core event types in the Aiki system
///
/// Event naming follows semantic conventions:
/// - `{domain}.{state}` format (e.g., `session.started`, `file.done`)
/// - Past tense for completed actions
/// - `permission_asked` suffix for gateable pre-events
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum AikiEvent {
    // ========================================================================
    // Session Lifecycle Events
    // ========================================================================
    /// Session initialization - new agent session began
    #[serde(rename = "session.started")]
    SessionStarted(AikiSessionStartPayload),
    /// Session resumed - continuing a previous session
    #[serde(rename = "session.resumed")]
    SessionResumed(AikiSessionResumedPayload),
    /// Session compaction is about to happen (pre-compaction)
    #[serde(rename = "session.will_compact")]
    SessionWillCompact(AikiSessionWillCompactPayload),
    /// Session was compacted — re-inject critical state
    #[serde(rename = "session.compacted")]
    SessionCompacted(AikiSessionCompactedPayload),
    /// Session was cleared via /clear — re-inject critical state
    #[serde(rename = "session.cleared")]
    SessionCleared(AikiSessionClearedPayload),
    /// Agent session terminated
    #[serde(rename = "session.ended")]
    SessionEnded(AikiSessionEndedPayload),

    // ========================================================================
    // Turn Lifecycle Events
    // ========================================================================
    /// Turn started - user submitted a prompt or autoreply was generated
    /// (allows context injection and blocking)
    #[serde(rename = "turn.started")]
    TurnStarted(AikiTurnStartedPayload),
    /// Turn completed - agent finished processing
    /// (allows validation and autoreply; does NOT auto-trigger session.ended)
    #[serde(rename = "turn.completed")]
    TurnCompleted(AikiTurnCompletedPayload),

    // ========================================================================
    // Read Operation Events
    // ========================================================================
    /// Agent is about to read a file (gateable - can block sensitive file reads)
    #[serde(rename = "read.permission_asked")]
    ReadPermissionAsked(AikiReadPermissionAskedPayload),
    /// Agent finished reading a file
    #[serde(rename = "read.completed")]
    ReadCompleted(AikiReadCompletedPayload),

    // ========================================================================
    // Change Operation Events (Unified mutations: write, delete, move)
    // ========================================================================
    /// Agent is about to perform a file mutation (write, delete, or move)
    /// Gateable - stash user changes, gate destructive operations
    #[serde(rename = "change.permission_asked")]
    ChangePermissionAsked(AikiChangePermissionAskedPayload),
    /// Agent finished performing a file mutation (write, delete, or move)
    /// Core provenance tracking event for all file mutations
    #[serde(rename = "change.completed")]
    ChangeCompleted(AikiChangeCompletedPayload),

    // ========================================================================
    // Shell Command Events
    // ========================================================================
    /// Agent is about to execute a shell command (gateable - the autonomous review wedge)
    #[serde(rename = "shell.permission_asked")]
    ShellPermissionAsked(AikiShellPermissionAskedPayload),
    /// Shell command completed
    #[serde(rename = "shell.completed")]
    ShellCompleted(AikiShellCompletedPayload),

    // ========================================================================
    // Web Access Events
    // ========================================================================
    /// Agent is about to make a web request (gateable)
    /// Operations: fetch, search
    #[serde(rename = "web.permission_asked")]
    WebPermissionAsked(AikiWebPermissionAskedPayload),
    /// Web request completed
    #[serde(rename = "web.completed")]
    WebCompleted(AikiWebCompletedPayload),

    // ========================================================================
    // MCP Tool Events
    // ========================================================================
    /// Agent is about to call an MCP tool (gateable)
    #[serde(rename = "mcp.permission_asked")]
    McpPermissionAsked(AikiMcpPermissionAskedPayload),
    /// MCP tool call completed
    #[serde(rename = "mcp.completed")]
    McpCompleted(AikiMcpCompletedPayload),

    // ========================================================================
    // Commit Integration Events
    // ========================================================================
    /// Git's prepare-commit-msg hook fired
    #[serde(rename = "commit.message_started")]
    CommitMessageStarted(AikiCommitMessageStartedPayload),

    // ========================================================================
    // Model Transition Events
    // ========================================================================
    /// Model changed mid-session (e.g., /model command)
    #[serde(rename = "model.changed")]
    ModelChanged(AikiModelChangedPayload),

    // ========================================================================
    // Repo Transition Events
    // ========================================================================
    /// Session moved to a different JJ repo
    #[serde(rename = "repo.changed")]
    RepoChanged(AikiRepoChangedPayload),

    // ========================================================================
    // Task Lifecycle Events
    // ========================================================================
    /// Task started - task transitioned to in_progress state
    #[serde(rename = "task.started")]
    TaskStarted(AikiTaskStartedPayload),
    /// Task closed - task reached closed state
    #[serde(rename = "task.closed")]
    TaskClosed(AikiTaskClosedPayload),

    // ========================================================================
    // Fallback
    // ========================================================================
    /// Unsupported event (unknown events or non-file tools that don't require processing)
    #[serde(other)]
    Unsupported,
}

impl AikiEvent {
    /// Get the working directory for this event
    #[must_use]
    pub fn cwd(&self) -> &Path {
        match self {
            // Session lifecycle
            Self::SessionStarted(e) => &e.cwd,
            Self::SessionResumed(e) => &e.cwd,
            Self::SessionWillCompact(e) => &e.cwd,
            Self::SessionCompacted(e) => &e.cwd,
            Self::SessionCleared(e) => &e.cwd,
            Self::SessionEnded(e) => &e.cwd,
            // Turn lifecycle
            Self::TurnStarted(e) => &e.cwd,
            Self::TurnCompleted(e) => &e.cwd,
            // Read operations
            Self::ReadPermissionAsked(e) => &e.cwd,
            Self::ReadCompleted(e) => &e.cwd,
            // Change operations (unified mutations: write, delete, move)
            Self::ChangePermissionAsked(e) => &e.cwd,
            Self::ChangeCompleted(e) => &e.cwd,
            // Shell commands
            Self::ShellPermissionAsked(e) => &e.cwd,
            Self::ShellCompleted(e) => &e.cwd,
            // Web access
            Self::WebPermissionAsked(e) => &e.cwd,
            Self::WebCompleted(e) => &e.cwd,
            // MCP tools
            Self::McpPermissionAsked(e) => &e.cwd,
            Self::McpCompleted(e) => &e.cwd,
            // Commit integration
            Self::CommitMessageStarted(e) => &e.cwd,
            // Model transitions
            Self::ModelChanged(e) => &e.cwd,
            // Repo transitions
            Self::RepoChanged(e) => &e.cwd,
            // Task lifecycle
            Self::TaskStarted(e) => &e.cwd,
            Self::TaskClosed(e) => &e.cwd,
            // Fallback
            Self::Unsupported => Path::new("."),
        }
    }

    /// Get the agent type for this event
    #[must_use]
    pub fn agent_type(&self) -> AgentType {
        match self {
            // Session lifecycle
            Self::SessionStarted(e) => e.session.agent_type(),
            Self::SessionResumed(e) => e.session.agent_type(),
            Self::SessionWillCompact(e) => e.session.agent_type(),
            Self::SessionCompacted(e) => e.session.agent_type(),
            Self::SessionCleared(e) => e.session.agent_type(),
            Self::SessionEnded(e) => e.session.agent_type(),
            // Turn lifecycle
            Self::TurnStarted(e) => e.session.agent_type(),
            Self::TurnCompleted(e) => e.session.agent_type(),
            // Read operations
            Self::ReadPermissionAsked(e) => e.session.agent_type(),
            Self::ReadCompleted(e) => e.session.agent_type(),
            // Change operations (unified mutations: write, delete, move)
            Self::ChangePermissionAsked(e) => e.session.agent_type(),
            Self::ChangeCompleted(e) => e.session.agent_type(),
            // Shell commands
            Self::ShellPermissionAsked(e) => e.session.agent_type(),
            Self::ShellCompleted(e) => e.session.agent_type(),
            // Web access
            Self::WebPermissionAsked(e) => e.session.agent_type(),
            Self::WebCompleted(e) => e.session.agent_type(),
            // MCP tools
            Self::McpPermissionAsked(e) => e.session.agent_type(),
            Self::McpCompleted(e) => e.session.agent_type(),
            // Commit integration
            Self::CommitMessageStarted(e) => e.agent_type,
            // Model transitions
            Self::ModelChanged(e) => e.session.agent_type(),
            // Repo transitions
            Self::RepoChanged(e) => e.session.agent_type(),
            // Task lifecycle (tasks don't have a session, so use Unknown)
            Self::TaskStarted(_) => AgentType::Unknown,
            Self::TaskClosed(_) => AgentType::Unknown,
            // Fallback
            Self::Unsupported => AgentType::Unknown,
        }
    }
}

// ============================================================================
// Module Declarations (semantic event names)
// ============================================================================

// Session lifecycle
mod session_cleared;
mod session_compacted;
mod session_ended;
mod session_resumed;
mod session_started;
mod session_will_compact;

// Turn lifecycle
mod turn_completed;
mod turn_started;

// Read operations
mod read_completed;
mod read_permission_asked;

// Change operations (unified mutations: write, delete, move)
mod change_completed;
mod change_permission_asked;
mod prelude;

// Model transitions
mod model_changed;

// Repo transitions
mod repo_changed;

// Shell commands
mod shell_completed;
mod shell_permission_asked;

// Web access
mod web_completed;
mod web_permission_asked;

// MCP tools
mod mcp_completed;
mod mcp_permission_asked;

// Commit integration
mod commit_message_started;

// Task lifecycle
mod task_closed;
mod task_started;

// ============================================================================
// Re-exports
// ============================================================================

// Session lifecycle
pub use session_cleared::*;
pub use session_compacted::*;
pub use session_ended::*;
pub use session_resumed::*;
pub use session_started::*;
pub use session_will_compact::*;

// Turn lifecycle
pub use turn_completed::*;
pub use turn_started::*;

// Read operations
pub use read_completed::*;
pub use read_permission_asked::*;

// Change operations (unified mutations: write, delete, move)
pub use change_completed::*;
pub use change_permission_asked::*;

// Model transitions
pub use model_changed::*;

// Repo transitions
pub use repo_changed::*;

// Shared types
pub use change_completed::EditDetail;

// Re-export FileOperation from tools module for convenience
pub use crate::tools::FileOperation;

// Shell commands
pub use shell_completed::*;
pub use shell_permission_asked::*;

// Web access
pub use web_completed::*;
pub use web_permission_asked::*;

// MCP tools
pub use mcp_completed::*;
pub use mcp_permission_asked::*;

// Commit integration
pub use commit_message_started::*;

// Task lifecycle
pub use task_closed::*;
pub use task_started::*;

// ============================================================================
// From Trait Implementations (enables vendor .into() pattern)
// ============================================================================

impl From<AikiSessionStartPayload> for AikiEvent {
    fn from(payload: AikiSessionStartPayload) -> Self {
        AikiEvent::SessionStarted(payload)
    }
}

impl From<AikiTurnStartedPayload> for AikiEvent {
    fn from(payload: AikiTurnStartedPayload) -> Self {
        AikiEvent::TurnStarted(payload)
    }
}

// Read operations
impl From<AikiReadPermissionAskedPayload> for AikiEvent {
    fn from(payload: AikiReadPermissionAskedPayload) -> Self {
        AikiEvent::ReadPermissionAsked(payload)
    }
}

impl From<AikiReadCompletedPayload> for AikiEvent {
    fn from(payload: AikiReadCompletedPayload) -> Self {
        AikiEvent::ReadCompleted(payload)
    }
}

// Change operations (unified mutations: write, delete, move)
impl From<AikiChangePermissionAskedPayload> for AikiEvent {
    fn from(payload: AikiChangePermissionAskedPayload) -> Self {
        AikiEvent::ChangePermissionAsked(payload)
    }
}

impl From<AikiChangeCompletedPayload> for AikiEvent {
    fn from(payload: AikiChangeCompletedPayload) -> Self {
        AikiEvent::ChangeCompleted(payload)
    }
}

impl From<AikiCommitMessageStartedPayload> for AikiEvent {
    fn from(payload: AikiCommitMessageStartedPayload) -> Self {
        AikiEvent::CommitMessageStarted(payload)
    }
}

impl From<AikiTurnCompletedPayload> for AikiEvent {
    fn from(payload: AikiTurnCompletedPayload) -> Self {
        AikiEvent::TurnCompleted(payload)
    }
}

impl From<AikiSessionEndedPayload> for AikiEvent {
    fn from(payload: AikiSessionEndedPayload) -> Self {
        AikiEvent::SessionEnded(payload)
    }
}

impl From<AikiSessionResumedPayload> for AikiEvent {
    fn from(payload: AikiSessionResumedPayload) -> Self {
        AikiEvent::SessionResumed(payload)
    }
}

impl From<AikiSessionWillCompactPayload> for AikiEvent {
    fn from(payload: AikiSessionWillCompactPayload) -> Self {
        AikiEvent::SessionWillCompact(payload)
    }
}

impl From<AikiSessionCompactedPayload> for AikiEvent {
    fn from(payload: AikiSessionCompactedPayload) -> Self {
        AikiEvent::SessionCompacted(payload)
    }
}

impl From<AikiSessionClearedPayload> for AikiEvent {
    fn from(payload: AikiSessionClearedPayload) -> Self {
        AikiEvent::SessionCleared(payload)
    }
}

impl From<AikiShellPermissionAskedPayload> for AikiEvent {
    fn from(payload: AikiShellPermissionAskedPayload) -> Self {
        AikiEvent::ShellPermissionAsked(payload)
    }
}

impl From<AikiShellCompletedPayload> for AikiEvent {
    fn from(payload: AikiShellCompletedPayload) -> Self {
        AikiEvent::ShellCompleted(payload)
    }
}

impl From<AikiWebPermissionAskedPayload> for AikiEvent {
    fn from(payload: AikiWebPermissionAskedPayload) -> Self {
        AikiEvent::WebPermissionAsked(payload)
    }
}

impl From<AikiWebCompletedPayload> for AikiEvent {
    fn from(payload: AikiWebCompletedPayload) -> Self {
        AikiEvent::WebCompleted(payload)
    }
}

impl From<AikiMcpPermissionAskedPayload> for AikiEvent {
    fn from(payload: AikiMcpPermissionAskedPayload) -> Self {
        AikiEvent::McpPermissionAsked(payload)
    }
}

impl From<AikiMcpCompletedPayload> for AikiEvent {
    fn from(payload: AikiMcpCompletedPayload) -> Self {
        AikiEvent::McpCompleted(payload)
    }
}

impl From<AikiModelChangedPayload> for AikiEvent {
    fn from(payload: AikiModelChangedPayload) -> Self {
        AikiEvent::ModelChanged(payload)
    }
}

impl From<AikiRepoChangedPayload> for AikiEvent {
    fn from(payload: AikiRepoChangedPayload) -> Self {
        AikiEvent::RepoChanged(payload)
    }
}

impl From<AikiTaskStartedPayload> for AikiEvent {
    fn from(payload: AikiTaskStartedPayload) -> Self {
        AikiEvent::TaskStarted(payload)
    }
}

impl From<AikiTaskClosedPayload> for AikiEvent {
    fn from(payload: AikiTaskClosedPayload) -> Self {
        AikiEvent::TaskClosed(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::TokenUsage;

    #[test]
    fn test_token_usage_add() {
        let a = TokenUsage {
            input: 100,
            output: 50,
            cache_read: 200,
            cache_created: 10,
        };
        let b = TokenUsage {
            input: 150,
            output: 75,
            cache_read: 300,
            cache_created: 20,
        };
        let result = a + b;
        assert_eq!(result.input, 250);
        assert_eq!(result.output, 125);
        assert_eq!(result.cache_read, 500);
        assert_eq!(result.cache_created, 30);
    }

    #[test]
    fn test_token_usage_add_assign() {
        let mut a = TokenUsage {
            input: 100,
            output: 50,
            cache_read: 200,
            cache_created: 10,
        };
        let b = TokenUsage {
            input: 150,
            output: 75,
            cache_read: 300,
            cache_created: 20,
        };
        a += b;
        assert_eq!(a.input, 250);
        assert_eq!(a.output, 125);
        assert_eq!(a.cache_read, 500);
        assert_eq!(a.cache_created, 30);
    }

    #[test]
    fn test_token_usage_sum() {
        let usages = vec![
            TokenUsage {
                input: 100,
                output: 50,
                cache_read: 0,
                cache_created: 0,
            },
            TokenUsage {
                input: 200,
                output: 100,
                cache_read: 50,
                cache_created: 10,
            },
            TokenUsage {
                input: 300,
                output: 150,
                cache_read: 100,
                cache_created: 20,
            },
        ];
        let total: TokenUsage = usages.into_iter().sum();
        assert_eq!(total.input, 600);
        assert_eq!(total.output, 300);
        assert_eq!(total.cache_read, 150);
        assert_eq!(total.cache_created, 30);
    }

    #[test]
    fn test_token_usage_sum_empty() {
        let usages: Vec<TokenUsage> = vec![];
        let total: TokenUsage = usages.into_iter().sum();
        assert_eq!(total.input, 0);
        assert_eq!(total.output, 0);
        assert_eq!(total.cache_read, 0);
        assert_eq!(total.cache_created, 0);
    }

    #[test]
    fn test_token_usage_is_zero() {
        assert!(TokenUsage::default().is_zero());
        assert!(!TokenUsage {
            input: 1,
            output: 0,
            cache_read: 0,
            cache_created: 0,
        }
        .is_zero());
    }

    #[test]
    fn test_token_usage_total() {
        let usage = TokenUsage {
            input: 100,
            output: 50,
            cache_read: 200,
            cache_created: 10,
        };
        assert_eq!(usage.total(), 150); // Only input + output
    }
}
