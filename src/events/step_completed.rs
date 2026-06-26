use super::prelude::*;

/// step.completed event payload.
///
/// Fired when a single workflow step ends — on success or error. Pairs with
/// `step.started` for granular within-run progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiStepCompletedPayload {
    /// Step name (from the workflow step, e.g. `decompose`, `loop`).
    pub step: String,
    /// Whether the step completed cleanly (vs. erroring).
    pub success: bool,
    /// Working directory the step ran in.
    pub cwd: PathBuf,
    /// When the step ended.
    pub timestamp: DateTime<Utc>,
}

/// Handle step.completed event.
pub fn handle_step_completed(payload: AikiStepCompletedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| format!("Step completed: {} (success={})", payload.step, payload.success));

    let core_hook = crate::flows::load_core_hook();
    let mut state = AikiState::new(payload);

    let flow_result = execute_hook(
        EventType::StepCompleted,
        &mut state,
        &core_hook.handlers.step_completed,
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
