use super::prelude::*;

/// workflow.started event payload.
///
/// Fired when an aiki workflow command (`build`, `fix`, `loop`, `review`)
/// begins, bracketing the whole run. A neutral lifecycle signal — aiki core
/// carries no integration knowledge; plugins subscribe and map it (the herdr
/// plugin makes the workflow appear as a `working` agent row in herdr's
/// sidebar). Granular within-run detail comes from `step.started`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiWorkflowStartedPayload {
    /// Workflow name: `build` | `fix` | `loop` | `review`.
    pub workflow: String,
    /// Working directory the workflow ran in.
    pub cwd: PathBuf,
    /// When the workflow started.
    pub timestamp: DateTime<Utc>,
}

/// Handle workflow.started event.
pub fn handle_workflow_started(payload: AikiWorkflowStartedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| format!("Workflow started: {}", payload.workflow));

    let core_hook = crate::flows::load_core_hook();
    let mut state = AikiState::new(payload);

    let flow_result = execute_hook(
        EventType::WorkflowStarted,
        &mut state,
        &core_hook.handlers.workflow_started,
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
