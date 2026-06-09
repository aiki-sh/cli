use super::prelude::*;
use super::task_started::TaskEventPayload;

/// task.closed event payload
///
/// Fired when a task reaches closed state (via `aiki task close`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiTaskClosedPayload {
    /// Task information (includes outcome, source, files for closed events)
    pub task: TaskEventPayload,
    /// Working directory
    pub cwd: PathBuf,
    /// When the task was closed
    pub timestamp: DateTime<Utc>,
}

/// Handle task.closed event
///
/// Called when a task transitions to closed state.
/// Task events don't have session management - they represent task lifecycle,
/// not agent sessions.
pub fn handle_task_closed(payload: AikiTaskClosedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| {
        format!(
            "Task closed: {} ({}) - outcome: {:?}",
            payload.task.name, payload.task.id, payload.task.outcome
        )
    });

    // Load core hook for fallback
    let core_hook = crate::flows::load_core_hook();

    // Build execution state from payload
    let mut state = AikiState::new(payload);

    // Execute hook via HookComposer (with fallback to bundled core hook)
    let flow_result = execute_hook(
        EventType::TaskClosed,
        &mut state,
        &core_hook.handlers.task_closed,
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
