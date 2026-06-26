use super::prelude::*;

/// step.started event payload.
///
/// Fired when a single step within an aiki workflow begins (e.g. `decompose`,
/// `loop`, `review`). Provides the granular within-run detail that
/// `workflow.started`/`workflow.completed` bracket. The herdr plugin maps it to
/// the agent row's status label (so a row reads `aiki build · decompose`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiStepStartedPayload {
    /// Step name (from the workflow step, e.g. `decompose`, `loop`).
    pub step: String,
    /// Working directory the step ran in.
    pub cwd: PathBuf,
    /// When the step started.
    pub timestamp: DateTime<Utc>,
}

/// Handle step.started event.
pub fn handle_step_started(payload: AikiStepStartedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| format!("Step started: {}", payload.step));

    let core_hook = crate::flows::load_core_hook();
    let mut state = AikiState::new(payload);

    let flow_result = execute_hook(
        EventType::StepStarted,
        &mut state,
        &core_hook.handlers.step_started,
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
