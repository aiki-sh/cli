//! Core types for the task system

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Fast HashMap using ahash for non-cryptographic hashing.
/// 2-5x faster than std HashMap for short string keys (task IDs).
pub type FastHashMap<K, V> = hashbrown::HashMap<K, V, ahash::RandomState>;

/// Task status (derived from event stream)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// Ready to work on
    Open,
    /// Reserved by orchestrator (pre-spawn lock, no session yet)
    Reserved,
    /// Currently being worked on
    InProgress,
    /// Was in progress, now stopped (has reason)
    Stopped,
    /// Done or won't do
    Closed,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskStatus::Open => write!(f, "open"),
            TaskStatus::Reserved => write!(f, "reserved"),
            TaskStatus::InProgress => write!(f, "in_progress"),
            TaskStatus::Stopped => write!(f, "stopped"),
            TaskStatus::Closed => write!(f, "closed"),
        }
    }
}

/// Task priority
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskPriority {
    /// Critical/urgent
    P0,
    /// High priority
    P1,
    /// Normal priority (default)
    P2,
    /// Low priority
    P3,
}

impl Default for TaskPriority {
    fn default() -> Self {
        Self::P2
    }
}

impl fmt::Display for TaskPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskPriority::P0 => write!(f, "p0"),
            TaskPriority::P1 => write!(f, "p1"),
            TaskPriority::P2 => write!(f, "p2"),
            TaskPriority::P3 => write!(f, "p3"),
        }
    }
}

impl TaskPriority {
    /// Parse priority from string (e.g., "p0", "p1", "p2", "p3")
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "p0" => Some(TaskPriority::P0),
            "p1" => Some(TaskPriority::P1),
            "p2" => Some(TaskPriority::P2),
            "p3" => Some(TaskPriority::P3),
            _ => None,
        }
    }
}

/// Task closure outcome
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOutcome {
    /// Completed successfully
    Done,
    /// Won't implement
    WontDo,
}

impl fmt::Display for TaskOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskOutcome::Done => write!(f, "done"),
            TaskOutcome::WontDo => write!(f, "wont_do"),
        }
    }
}

impl TaskOutcome {
    /// Parse outcome from string
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "done" => Some(TaskOutcome::Done),
            "wont_do" | "wontdo" => Some(TaskOutcome::WontDo),
            _ => None,
        }
    }
}

/// Confidence reported when an agent closes done work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConfidenceLevel {
    Low = 1,
    Medium = 2,
    High = 3,
    Verified = 4,
}

impl ConfidenceLevel {
    #[must_use]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Low),
            2 => Some(Self::Medium),
            3 => Some(Self::High),
            4 => Some(Self::Verified),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Verified => "verified",
        }
    }
}

impl fmt::Display for ConfidenceLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_u8())
    }
}

impl std::str::FromStr for ConfidenceLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let value: u8 = s
            .parse()
            .map_err(|_| "Confidence must be a number 1-4".to_string())?;
        Self::from_u8(value).ok_or_else(|| "Confidence must be 1-4".to_string())
    }
}

/// Events stored on aiki/tasks branch
#[derive(Debug, Clone)]
pub enum TaskEvent {
    /// Task was created
    Created {
        task_id: String,
        name: String,
        /// Stable slug for automation references (e.g., "build", "run-tests")
        slug: Option<String>,
        /// Task type (e.g., "review", "fix") - enables sugar triggers like review.started
        task_type: Option<String>,
        priority: TaskPriority,
        assignee: Option<String>,
        /// Sources that spawned this task (e.g., "file:ops/now/design.md", "task:abc123")
        sources: Vec<String>,
        /// Template used to create this task (e.g., "review@1.0.0")
        template: Option<String>,
        /// Instructions from template (with variables substituted)
        instructions: Option<String>,
        /// Custom data/metadata for the task
        data: HashMap<String, String>,
        timestamp: DateTime<Utc>,
    },
    /// Task(s) were started (batch operation)
    Started {
        task_ids: Vec<String>,
        agent_type: String,
        /// Session ID that claimed these tasks (deterministic UUID)
        session_id: Option<String>,
        /// Turn ID (UUID v5) from the session's current turn
        turn_id: Option<String>,
        /// Immutable snapshot revset captured from `@` at start time.
        working_copy: Option<String>,
        /// Instructions set atomically at start time (for quick-start with -i)
        instructions: Option<String>,
        timestamp: DateTime<Utc>,
    },
    /// Task(s) were stopped (batch operation)
    Stopped {
        task_ids: Vec<String>,
        reason: Option<String>,
        session_id: Option<String>,
        /// Turn ID (UUID v5) from the session's current turn
        turn_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    /// Task(s) were closed (batch operation)
    Closed {
        task_ids: Vec<String>,
        outcome: TaskOutcome,
        confidence: Option<ConfidenceLevel>,
        /// Summary of what was accomplished (replaces closing comment pattern)
        summary: Option<String>,
        session_id: Option<String>,
        /// Turn ID (UUID v5) from the session's current turn
        turn_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    /// Task was reopened
    Reopened {
        task_id: String,
        reason: String,
        timestamp: DateTime<Utc>,
    },
    /// Comment was added to task(s) (batch operation)
    CommentAdded {
        task_ids: Vec<String>,
        text: String,
        data: HashMap<String, String>,
        timestamp: DateTime<Utc>,
    },
    /// Task was updated
    Updated {
        task_id: String,
        name: Option<String>,
        priority: Option<TaskPriority>,
        /// New assignee value. Some = set to this, None = no change
        assignee: Option<String>,
        /// Data fields to merge (add/update). Empty values mean remove the key (backwards compat).
        data: Option<HashMap<String, String>>,
        /// New instructions content (replaces existing instructions)
        instructions: Option<String>,
        timestamp: DateTime<Utc>,
    },
    /// Fields were cleared on a task
    FieldsCleared {
        task_id: String,
        /// Field names that were cleared (e.g., ["assignee", "instructions", "data.mykey"])
        fields: Vec<String>,
        timestamp: DateTime<Utc>,
    },
    /// Link added between two nodes
    LinkAdded {
        /// Source node (always a task ID)
        from: String,
        /// Target node (task ID or external ref like "file:path")
        to: String,
        /// Open-ended link type (e.g., "blocked-by", "sourced-from")
        kind: String,
        /// Whether to auto-start the `from` task when the `to` (blocker) completes.
        /// None = not specified (treated as false for backward compat with old events).
        autorun: Option<bool>,
        timestamp: DateTime<Utc>,
    },
    /// Link removed between two nodes
    LinkRemoved {
        /// Source node (always a task ID)
        from: String,
        /// Target node (task ID or external ref)
        to: String,
        /// Link type being removed
        kind: String,
        /// Audit trail for why the link was removed
        reason: Option<String>,
        timestamp: DateTime<Utc>,
    },
    /// Task(s) were reserved by orchestrator (pre-spawn lock, no session yet)
    Reserved {
        task_ids: Vec<String>,
        /// Who is reserving (e.g., "claude-code")
        agent_type: String,
        timestamp: DateTime<Utc>,
    },
    /// Task(s) reservation was released (spawn failed or orchestrator gave up)
    Released {
        task_ids: Vec<String>,
        reason: Option<String>,
        timestamp: DateTime<Utc>,
    },
    /// Changes from these tasks have been absorbed into the parent workspace
    Absorbed {
        task_ids: Vec<String>,
        session_id: String,
        turn_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
}

impl TaskEvent {
    /// Get the timestamp of this event
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            TaskEvent::Created { timestamp, .. }
            | TaskEvent::Started { timestamp, .. }
            | TaskEvent::Stopped { timestamp, .. }
            | TaskEvent::Closed { timestamp, .. }
            | TaskEvent::Reopened { timestamp, .. }
            | TaskEvent::CommentAdded { timestamp, .. }
            | TaskEvent::Updated { timestamp, .. }
            | TaskEvent::FieldsCleared { timestamp, .. }
            | TaskEvent::LinkAdded { timestamp, .. }
            | TaskEvent::LinkRemoved { timestamp, .. }
            | TaskEvent::Reserved { timestamp, .. }
            | TaskEvent::Released { timestamp, .. }
            | TaskEvent::Absorbed { timestamp, .. } => *timestamp,
        }
    }

    /// Variant name for deduplication (e.g., "Created", "Started").
    fn variant_name(&self) -> &'static str {
        match self {
            TaskEvent::Created { .. } => "Created",
            TaskEvent::Started { .. } => "Started",
            TaskEvent::Stopped { .. } => "Stopped",
            TaskEvent::Closed { .. } => "Closed",
            TaskEvent::Reopened { .. } => "Reopened",
            TaskEvent::CommentAdded { .. } => "CommentAdded",
            TaskEvent::Updated { .. } => "Updated",
            TaskEvent::FieldsCleared { .. } => "FieldsCleared",
            TaskEvent::LinkAdded { .. } => "LinkAdded",
            TaskEvent::LinkRemoved { .. } => "LinkRemoved",
            TaskEvent::Reserved { .. } => "Reserved",
            TaskEvent::Released { .. } => "Released",
            TaskEvent::Absorbed { .. } => "Absorbed",
        }
    }

    /// Primary task ID for this event (first task_id from single or batch events).
    fn primary_task_id(&self) -> &str {
        match self {
            TaskEvent::Created { task_id, .. }
            | TaskEvent::Reopened { task_id, .. }
            | TaskEvent::Updated { task_id, .. }
            | TaskEvent::FieldsCleared { task_id, .. }
            | TaskEvent::LinkAdded { from: task_id, .. }
            | TaskEvent::LinkRemoved { from: task_id, .. } => task_id,
            TaskEvent::Started { task_ids, .. }
            | TaskEvent::Stopped { task_ids, .. }
            | TaskEvent::Closed { task_ids, .. }
            | TaskEvent::CommentAdded { task_ids, .. }
            | TaskEvent::Reserved { task_ids, .. }
            | TaskEvent::Released { task_ids, .. }
            | TaskEvent::Absorbed { task_ids, .. } => {
                task_ids.first().map(|s| s.as_str()).unwrap_or("")
            }
        }
    }

    /// Deduplication key: (task_id, variant_name, timestamp).
    ///
    /// The same logical event read from different JJ workspaces produces the
    /// same key, enabling cross-workspace dedup in the drain loop.
    pub fn dedup_key(&self) -> (String, &'static str, DateTime<Utc>) {
        (
            self.primary_task_id().to_string(),
            self.variant_name(),
            self.timestamp(),
        )
    }
}

/// A comment on a task
#[derive(Debug, Clone)]
pub struct TaskComment {
    /// Unique identifier for this comment (JJ change_id of the CommentAdded event)
    /// Used for `source: comment:<id>` references in followup tasks
    pub id: Option<String>,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    /// Custom metadata for this comment (e.g., `issue: "true"` for review issues)
    pub data: HashMap<String, String>,
}

/// Materialized task view (computed from events)
#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub name: String,
    /// Stable slug for automation references (e.g., "build", "run-tests")
    pub slug: Option<String>,
    /// Task type (e.g., "review", "fix") - enables sugar triggers like review.started
    pub task_type: Option<String>,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub assignee: Option<String>,
    /// Sources that spawned this task (e.g., "file:ops/now/design.md", "task:abc123")
    pub sources: Vec<String>,
    /// Template used to create this task (e.g., "review@1.0.0")
    pub template: Option<String>,
    /// Instructions from template (with variables substituted)
    pub instructions: Option<String>,
    /// Custom data/metadata for the task
    pub data: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
    /// When the task was most recently started (for ordering in provenance)
    pub started_at: Option<DateTime<Utc>>,
    /// Session that claimed this task (if in progress)
    pub claimed_by_session: Option<String>,
    /// Session ID that last worked on this task (persists even after close)
    pub last_session_id: Option<String>,
    /// Latest stop reason (if stopped)
    pub stopped_reason: Option<String>,
    /// Closure outcome (if closed)
    pub closed_outcome: Option<TaskOutcome>,
    /// Agent-reported confidence for done closes, when provided
    pub confidence: Option<ConfidenceLevel>,
    /// Summary of what was accomplished when task was closed
    pub summary: Option<String>,
    /// Turn ID when this task was most recently started
    pub turn_started: Option<String>,
    /// When the task was closed
    pub closed_at: Option<DateTime<Utc>>,
    /// Turn ID when this task was closed
    pub turn_closed: Option<String>,
    /// Turn ID when this task was stopped (if currently stopped)
    pub turn_stopped: Option<String>,
    /// Comments on this task
    pub comments: Vec<TaskComment>,
}

impl Task {
    /// Returns the effective summary for display purposes.
    ///
    /// Prefers `summary` field (set by `--summary` on close), but falls back
    /// to the last comment for backward compatibility with tasks closed before
    /// the summary field existed.
    pub fn effective_summary(&self) -> Option<&str> {
        self.summary
            .as_deref()
            .or_else(|| self.comments.last().map(|c| c.text.as_str()))
    }

    /// Agent name for phase header (reads data["agent_type"] or falls back to assignee)
    pub fn agent_label(&self) -> Option<&str> {
        self.data
            .get("agent_type")
            .map(|s| s.as_str())
            .or(self.assignee.as_deref())
    }

    /// Most recent heartbeat text from comments. Falls back to empty string.
    pub fn latest_heartbeat(&self) -> &str {
        self.comments
            .iter()
            .rev()
            .find(|c| {
                c.data
                    .get("type")
                    .map(|t| t == "heartbeat")
                    .unwrap_or(false)
            })
            .map(|c| c.text.as_str())
            .unwrap_or("")
    }

    /// Formatted elapsed time since started_at, e.g. "1m 23s"
    /// For closed/stopped tasks, uses closed_at as the end time.
    pub fn elapsed_str(&self) -> Option<String> {
        let started = self.started_at?;
        let end = if self.is_terminal() {
            self.closed_at.unwrap_or_else(chrono::Utc::now)
        } else {
            chrono::Utc::now()
        };
        let elapsed = end - started;
        let secs = elapsed.num_seconds();
        if secs < 60 {
            Some(format!("{}s", secs))
        } else {
            Some(format!("{}m {}s", secs / 60, secs % 60))
        }
    }

    /// Summary or fallback to task name for done state
    pub fn display_summary(&self) -> &str {
        self.effective_summary().unwrap_or(&self.name)
    }

    /// Reason task was stopped, or fallback
    pub fn display_stopped_reason(&self) -> &str {
        self.stopped_reason.as_deref().unwrap_or("stopped")
    }

    /// First 6 chars of task ID for display
    pub fn short_id(&self) -> &str {
        &self.id[..6.min(self.id.len())]
    }

    /// True for Closed or Stopped
    pub fn is_terminal(&self) -> bool {
        matches!(self.status, TaskStatus::Closed | TaskStatus::Stopped)
    }

    /// Returns true if this task is an orchestrator (coordinates subtask execution).
    ///
    /// Orchestrator tasks get special lifecycle behavior: when stopped or failed,
    /// all their unclosed descendants are automatically cascade-closed as WontDo.
    pub fn is_orchestrator(&self) -> bool {
        self.task_type.as_deref() == Some("orchestrator")
    }
}

/// A lightweight reference to a task for event payloads and APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReference {
    /// Task ID (32-char change_id)
    pub id: String,
    /// Task name
    pub name: String,
    /// Task type (None for original work, Some for generated tasks like review/fix)
    pub task_type: Option<String>,
}

/// A categorized list of tasks by state transitions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskActivity {
    /// Tasks that were closed
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed: Vec<TaskReference>,
    /// Tasks that were started
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub started: Vec<TaskReference>,
    /// Tasks that were stopped
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stopped: Vec<TaskReference>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_ordering() {
        assert!(TaskPriority::P0 < TaskPriority::P1);
        assert!(TaskPriority::P1 < TaskPriority::P2);
        assert!(TaskPriority::P2 < TaskPriority::P3);
    }

    #[test]
    fn test_priority_default() {
        assert_eq!(TaskPriority::default(), TaskPriority::P2);
    }

    #[test]
    fn test_priority_display() {
        assert_eq!(TaskPriority::P0.to_string(), "p0");
        assert_eq!(TaskPriority::P2.to_string(), "p2");
    }

    #[test]
    fn test_priority_from_str() {
        assert_eq!(TaskPriority::from_str("p0"), Some(TaskPriority::P0));
        assert_eq!(TaskPriority::from_str("P2"), Some(TaskPriority::P2));
        assert_eq!(TaskPriority::from_str("invalid"), None);
    }

    #[test]
    fn test_status_display() {
        assert_eq!(TaskStatus::Open.to_string(), "open");
        assert_eq!(TaskStatus::InProgress.to_string(), "in_progress");
    }

    #[test]
    fn test_outcome_display() {
        assert_eq!(TaskOutcome::Done.to_string(), "done");
        assert_eq!(TaskOutcome::WontDo.to_string(), "wont_do");
    }

    #[test]
    fn test_confidence_level_parse_and_display() {
        assert_eq!("1".parse::<ConfidenceLevel>(), Ok(ConfidenceLevel::Low));
        assert_eq!(
            "4".parse::<ConfidenceLevel>(),
            Ok(ConfidenceLevel::Verified)
        );
        assert_eq!(
            "0".parse::<ConfidenceLevel>(),
            Err("Confidence must be 1-4".to_string())
        );
        assert_eq!(
            "hi".parse::<ConfidenceLevel>(),
            Err("Confidence must be a number 1-4".to_string())
        );
        assert_eq!(ConfidenceLevel::High.to_string(), "3");
        assert_eq!(ConfidenceLevel::Verified.label(), "verified");
    }

    fn make_task_for_summary() -> Task {
        Task {
            id: "abcdefghijklmnopqrstuvwxyzabcdef".to_string(),
            name: "Test".to_string(),
            slug: None,
            task_type: None,
            status: TaskStatus::Closed,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            instructions: None,
            data: std::collections::HashMap::new(),
            created_at: chrono::Utc::now(),
            started_at: None,
            claimed_by_session: None,
            last_session_id: None,
            stopped_reason: None,
            closed_outcome: None,
            confidence: None,
            summary: None,
            turn_started: None,
            closed_at: None,
            turn_closed: None,
            turn_stopped: None,
            comments: Vec::new(),
        }
    }

    #[test]
    fn test_effective_summary_prefers_summary_field() {
        let mut task = make_task_for_summary();
        task.summary = Some("The summary".to_string());
        task.comments.push(TaskComment {
            id: None,
            text: "A comment".to_string(),
            timestamp: chrono::Utc::now(),
            data: HashMap::new(),
        });
        assert_eq!(task.effective_summary(), Some("The summary"));
    }

    #[test]
    fn test_effective_summary_falls_back_to_last_comment() {
        let mut task = make_task_for_summary();
        task.comments.push(TaskComment {
            id: None,
            text: "First comment".to_string(),
            timestamp: chrono::Utc::now(),
            data: HashMap::new(),
        });
        task.comments.push(TaskComment {
            id: None,
            text: "Last comment".to_string(),
            timestamp: chrono::Utc::now(),
            data: HashMap::new(),
        });
        assert_eq!(task.effective_summary(), Some("Last comment"));
    }

    #[test]
    fn test_effective_summary_none_when_empty() {
        let task = make_task_for_summary();
        assert_eq!(task.effective_summary(), None);
    }

    #[test]
    fn test_is_orchestrator_true() {
        let mut task = make_task_for_summary();
        task.task_type = Some("orchestrator".to_string());
        assert!(task.is_orchestrator());
    }

    #[test]
    fn test_is_orchestrator_false_for_other_type() {
        let mut task = make_task_for_summary();
        task.task_type = Some("build".to_string());
        assert!(!task.is_orchestrator());
    }

    #[test]
    fn test_is_orchestrator_false_for_none() {
        let task = make_task_for_summary();
        assert!(!task.is_orchestrator());
    }
}
