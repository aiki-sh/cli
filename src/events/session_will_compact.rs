use super::prelude::*;

/// session.will_compact event payload
///
/// Fires before context compaction (from Claude Code's PreCompact hook).
/// Reserved for future state persistence — currently a no-op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiSessionWillCompactPayload {
    pub session: AikiSession,
    pub cwd: PathBuf,
    pub timestamp: DateTime<Utc>,
}

/// Handle session.will_compact event
///
/// Currently a no-op. In the future, this could persist workspace path
/// or active task IDs to a recovery file so session.compacted can
/// recover even if session state is lost.
pub fn handle_session_will_compact(payload: AikiSessionWillCompactPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| {
        format!(
            "Session will compact for {:?}, session: {}",
            payload.session.agent_type(),
            payload.session.external_id()
        )
    });

    // Load core hook for fallback
    let core_hook = crate::flows::load_core_hook();

    // Build execution state from payload
    let mut state = AikiState::new(payload);

    // Execute hook via HookComposer (with fallback to bundled core hook)
    let _flow_result = execute_hook(
        EventType::SessionWillCompact,
        &mut state,
        &core_hook.handlers.session_will_compact,
    )?;

    // Extract failures from state
    let failures = state.take_failures();

    // session.will_compact never blocks and doesn't inject context
    Ok(HookResult {
        context: None,
        decision: Decision::Allow,
        failures,
    })
}
