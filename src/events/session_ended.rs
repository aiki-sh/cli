use super::prelude::*;
use crate::global;
use crate::history;
use crate::repos;

/// session.ended event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiSessionEndedPayload {
    pub session: AikiSession,
    pub cwd: PathBuf,
    pub timestamp: DateTime<Utc>,
    /// Reason for session termination (e.g., "clear", "logout", "user_close", "ttl_expired")
    #[serde(default)]
    pub reason: String,
    /// Cumulative token usage for the entire session
    #[serde(default)]
    pub tokens: Option<super::TokenUsage>,
}

/// Handle session.ended event
///
/// Executes the session.ended flow section for user-defined cleanup actions,
/// then cleans up the session file and records session end to history.
pub fn handle_session_ended(payload: AikiSessionEndedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| format!("Session ended by {:?}", payload.session.agent_type()));

    // Aggregate token usage from all turns in this session
    let mut payload = payload;
    if payload.tokens.is_none() {
        payload.tokens = aggregate_session_tokens(&payload);
    }

    // Record session end to conversation history (non-blocking on failure)
    // Uses global JJ repo at ~/.aiki/.jj/ for cross-repo conversation history
    let cwd_str = payload.cwd.to_string_lossy();
    let repo_id = repos::compute_repo_id(&payload.cwd).ok();
    if let Err(e) = history::record_session_end(
        &global::global_aiki_dir(),
        &payload.session,
        payload.timestamp,
        &payload.reason,
        repo_id.as_deref(),
        Some(&cwd_str),
    ) {
        debug_log(|| format!("Failed to record session end: {}", e));
    }

    // Load core hook for fallback
    let core_hook = crate::flows::load_core_hook();

    // Build execution state from payload (clone needed for session.end() call below)
    let mut state = AikiState::new(payload.clone());

    // Execute hook via HookComposer (with fallback to bundled core hook)
    let flow_result = execute_hook(
        EventType::SessionEnded,
        &mut state,
        &core_hook.handlers.session_ended,
    )?;

    // Clean up session file (always happens, regardless of flow result)
    payload.session.end()?;

    // TurnState is now ephemeral (queried from JJ) - no file cleanup needed

    // Extract failures from state
    let failures = state.take_failures();

    // Translate HookOutcome to HookResult
    match flow_result {
        HookOutcome::Success | HookOutcome::FailedContinue | HookOutcome::FailedStop => {
            Ok(HookResult {
                context: None,
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

/// Aggregate token usage from all Response events for a session.
///
/// Returns `None` if no turns had token data (rather than returning zeros),
/// per the acceptance criteria.
fn aggregate_session_tokens(payload: &AikiSessionEndedPayload) -> Option<super::TokenUsage> {
    let session_id = payload.session.uuid();
    let events = match history::storage::read_events(&global::global_aiki_dir()) {
        Ok(events) => events,
        Err(e) => {
            debug_log(|| format!("Failed to read events for token aggregation: {}", e));
            return None;
        }
    };

    let turn_tokens: Vec<super::TokenUsage> = events
        .into_iter()
        .filter_map(|event| match event {
            history::types::ConversationEvent::Response {
                session_id: sid,
                tokens: Some(t),
                ..
            } if sid == session_id => Some(t),
            _ => None,
        })
        .collect();

    if turn_tokens.is_empty() {
        None
    } else {
        let total: super::TokenUsage = turn_tokens.into_iter().sum();
        Some(total)
    }
}
