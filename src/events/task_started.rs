use super::prelude::*;

/// Task info for event payloads
///
/// Contains the common task fields used by both task.started and task.closed events.
/// The `type` field is serialized as "type" in JSON but named `task_type` in Rust
/// since `type` is a reserved keyword.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEventPayload {
    /// Task ID
    pub id: String,
    /// Task name/title
    pub name: String,
    /// Task type (e.g., "review", "feature", "bug")
    #[serde(rename = "type")]
    pub task_type: String,
    /// Task status ("in_progress" for started, "closed" for closed)
    pub status: String,
    /// Assigned agent (if any)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// Task outcome: "done", "wont_do" (only for task.closed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Source field if present (for lineage)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Files changed while working on this task (from provenance)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    /// JJ change IDs created while working on this task (from provenance)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<String>>,
}

/// task.started event payload
///
/// Fired when a task is started (via `aiki task start` or `aiki task run`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiTaskStartedPayload {
    /// Task information
    pub task: TaskEventPayload,
    /// Working directory
    pub cwd: PathBuf,
    /// When the task was started
    pub timestamp: DateTime<Utc>,
}

/// Handle task.started event
///
/// Called when a task transitions to in_progress state.
/// Task events don't have session management - they represent task lifecycle,
/// not agent sessions.
pub fn handle_task_started(payload: AikiTaskStartedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| format!("Task started: {} ({})", payload.task.name, payload.task.id));

    // Load core hook for fallback
    let core_hook = crate::flows::load_core_hook();

    // Build execution state from payload
    let mut state = AikiState::new(payload);

    // Execute hook via HookComposer (with fallback to bundled core hook)
    let flow_result = execute_hook(
        EventType::TaskStarted,
        &mut state,
        &core_hook.handlers.task_started,
    )?;

    // Extract failures from state
    let failures = state.take_failures();

    match flow_result {
        HookOutcome::Success | HookOutcome::FailedContinue | HookOutcome::FailedStop => {
            Ok(HookResult {
                context: state.build_context(),
                decision: Decision::Allow,
                failures,
            })
        }
        HookOutcome::FailedBlock => Ok(HookResult {
            context: None,
            decision: Decision::Block,
            failures,
        }),
    }
}
