use super::prelude::*;

/// workflow.completed event payload.
///
/// Fired when an aiki workflow command ends — on success, error, or unwind. The
/// herdr plugin uses it to release the workflow's `working` agent row so herdr's
/// screen detection resumes for the pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiWorkflowCompletedPayload {
    /// Workflow name: `build` | `fix` | `loop` | `review`.
    pub workflow: String,
    /// Whether the workflow reached a clean completion (vs. erroring/unwinding).
    pub success: bool,
    /// Working directory the workflow ran in.
    pub cwd: PathBuf,
    /// When the workflow ended.
    pub timestamp: DateTime<Utc>,
}

/// Handle workflow.completed event.
pub fn handle_workflow_completed(payload: AikiWorkflowCompletedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| {
        format!(
            "Workflow completed: {} (success={})",
            payload.workflow, payload.success
        )
    });

    let core_hook = crate::flows::load_core_hook();
    let mut state = AikiState::new(payload);

    let flow_result = execute_hook(
        EventType::WorkflowCompleted,
        &mut state,
        &core_hook.handlers.workflow_completed,
    )?;

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
