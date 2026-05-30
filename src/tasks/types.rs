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
        /// Template used to create this task (e.g., "aiki/review@1.0.0")
        template: Option<String>,
        /// Working copy change_id at creation time (for historical template lookup)
        working_copy: Option<String>,
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
        timestamp: DateTime<Utc>,
    },
    /// Task(s) were stopped (batch operation)
    Stopped {
        task_ids: Vec<String>,
        reason: Option<String>,
        /// Turn ID (UUID v5) from the session's current turn
        turn_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
    /// Task(s) were closed (batch operation)
    Closed {
        task_ids: Vec<String>,
        outcome: TaskOutcome,
        /// Summary of what was accomplished (replaces closing comment pattern)
        summary: Option<String>,
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
            | TaskEvent::LinkRemoved { timestamp, .. } => *timestamp,
        }
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
    /// Template used to create this task (e.g., "aiki/review@1.0.0")
    pub template: Option<String>,
    /// Working copy change_id at creation time (for historical template lookup)
    pub working_copy: Option<String>,
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
    /// Summary of what was accomplished when task was closed
    pub summary: Option<String>,
    /// Turn ID when this task was most recently started
    pub turn_started: Option<String>,
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

impl TaskActivity {
    /// Check if there was any activity
    pub fn is_empty(&self) -> bool {
        self.closed.is_empty() && self.started.is_empty() && self.stopped.is_empty()
    }
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
            working_copy: None,
            instructions: None,
            data: std::collections::HashMap::new(),
            created_at: chrono::Utc::now(),
            started_at: None,
            claimed_by_session: None,
            last_session_id: None,
            stopped_reason: None,
            closed_outcome: None,
            summary: None,
            turn_started: None,
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
