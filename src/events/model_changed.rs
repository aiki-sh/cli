use super::prelude::*;

/// model.changed event payload
///
/// Fires when TurnCompleted reports a different model than the session's stored model.
/// This detects mid-session model switches (e.g., `/model` command in Claude Code).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiModelChangedPayload {
    pub session: AikiSession,
    pub cwd: PathBuf,
    pub timestamp: DateTime<Utc>,
    /// The model previously stored in the session (None if first observation)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_model: Option<String>,
    /// The new model observed in the turn
    pub new_model: String,
}

/// Handle model.changed event
pub fn handle_model_changed(payload: AikiModelChangedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| {
        format!(
            "model.changed event: session {} switched from {:?} to {}",
            payload.session.uuid(),
            payload.previous_model,
            payload.new_model,
        )
    });

    // Record model change to conversation history
    let cwd_str = payload.cwd.to_string_lossy();
    let repo_id = crate::repos::compute_repo_id(&payload.cwd).ok();
    if let Err(e) = crate::history::record_model_changed(
        &crate::global::global_aiki_dir(),
        &payload.session,
        payload.previous_model.as_deref(),
        &payload.new_model,
        payload.timestamp,
        repo_id.as_deref(),
        Some(&cwd_str),
    ) {
        debug_log(|| format!("Failed to record model change: {}", e));
    }

    let core_hook = crate::flows::load_core_hook();
    let mut state = AikiState::new(payload);

    let _flow_result = execute_hook(
        EventType::ModelChanged,
        &mut state,
        &core_hook.handlers.model_changed,
    )?;

    let failures = state.take_failures();

    Ok(HookResult {
        context: None,
        decision: Decision::Allow,
        failures,
    })
}
