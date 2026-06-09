use super::prelude::*;
use super::Turn;
use crate::global;
use crate::history;
use crate::history::TurnSource;
use crate::repos;
use crate::session::turn_state::generate_turn_id;


/// turn.completed event payload
///
/// Fires when a turn ends (agent finishes processing).
/// Every turn.started has exactly one turn.completed (1:1 correspondence).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiTurnCompletedPayload {
    pub session: AikiSession,
    pub cwd: PathBuf,
    pub timestamp: DateTime<Utc>,
    /// Turn metadata (number, id, source)
    #[serde(default)]
    pub turn: Turn,
    /// The agent's response text for this turn
    pub response: String,
    /// Files modified during this turn
    #[serde(default)]
    pub modified_files: Vec<PathBuf>,
    /// Task activity during this turn (started, stopped, closed)
    #[serde(default)]
    pub tasks: crate::tasks::TaskActivity,
    /// Token usage for this turn (extracted from transcript)
    #[serde(default)]
    pub tokens: Option<super::TokenUsage>,
    /// Model used for this turn (extracted from transcript)
    #[serde(default)]
    pub model: Option<String>,
}

/// Handle turn.completed event
///
/// This event fires when the agent finishes generating its response,
/// allowing flows to validate output, detect errors, and optionally send
/// an autoreply to the agent.
/// Also records the response to conversation history (if not opted out).
/// Returns autoreply via `response.context` and failures via `response.failures`,
/// with graceful degradation on errors.
///
/// Note: turn.completed may trigger session.ended if the session's driving task
/// (thread tail) has been closed during this turn. This replaces the old
/// task.closed-based session end, which killed conversations mid-reply.
pub fn handle_turn_completed(mut payload: AikiTurnCompletedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    // Query the Prompt event for this session's current turn info
    // This replaces reading from TurnState, getting turn/source from history instead
    // Defensive fallback: if history lookup fails (JJ unavailable, branch doesn't exist, etc.),
    // we use defaults (turn=0, source=User) and continue recording - turn=0 is acceptable.
    // Uses global JJ repo at ~/.aiki/.jj/ for cross-repo conversation history
    let (turn_number, source) =
        match history::get_current_turn_info(&global::global_aiki_dir(), payload.session.uuid()) {
            Ok(result) => result,
            Err(e) => {
                debug_log(|| {
                    format!(
                        "History lookup failed for session {}, using defaults (turn=0): {}",
                        payload.session.uuid(),
                        e
                    )
                });
                (0, TurnSource::User)
            }
        };
    payload.turn = Turn::new(
        turn_number,
        generate_turn_id(payload.session.uuid(), turn_number),
        source.to_string(),
    );

    // Populate task activity for this turn
    payload.tasks = match crate::tasks::storage::read_events(&payload.cwd) {
        Ok(events) => {
            let graph = crate::tasks::graph::materialize_graph(&events);
            crate::tasks::manager::get_task_activity_by_turn(&graph, &payload.turn.id)
        }
        Err(e) => {
            debug_log(|| format!("Task activity lookup failed, using empty: {}", e));
            crate::tasks::TaskActivity::default()
        }
    };

    debug_log(|| {
        format!(
            "turn.completed event from {:?}, source: {}, turn: {}, response length: {}",
            payload.session.agent_type(),
            payload.turn.source,
            payload.turn.number,
            payload.response.len()
        )
    });

    // Record response to conversation history (non-blocking on failure)
    // Uses global JJ repo at ~/.aiki/.jj/ for cross-repo conversation history
    let files_written: Vec<String> = payload
        .modified_files
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    let cwd_str = payload.cwd.to_string_lossy().to_string();
    let repo_id = repos::compute_repo_id(&payload.cwd).ok();

    // Detect model drift: materialize previous model from event history
    if let Some(ref new_model) = payload.model {
        let global_dir = global::global_aiki_dir();
        let session_id = payload.session.uuid().to_string();
        let previous_model =
            history::last_session_model(&global_dir, &session_id).unwrap_or(None);

        let drifted = match &previous_model {
            Some(prev) => prev != new_model,
            None => false, // No previous model — first observation, no drift
        };

        if drifted {
            let model_changed_payload = super::AikiModelChangedPayload {
                session: payload.session.clone(),
                cwd: payload.cwd.clone(),
                timestamp: Utc::now(),
                previous_model,
                new_model: new_model.clone(),
            };
            let event = crate::events::AikiEvent::ModelChanged(model_changed_payload);
            if let Err(e) = crate::event_bus::dispatch(event) {
                debug_log(|| format!("model.changed dispatch error (non-fatal): {}", e));
            }
        }
    }

    if let Err(e) = history::record_response(
        &global::global_aiki_dir(),
        &payload.session,
        &payload.response,
        files_written,
        payload.turn.number,
        payload.timestamp,
        repo_id.as_deref(),
        Some(&cwd_str),
        payload.tokens.clone(),
        payload.model.clone(),
    ) {
        debug_log(|| format!("Failed to record response: {}", e));
    }

    // Save values needed for autoreply recording (payload is moved to state below)
    let payload_cwd = payload.cwd.clone();
    let payload_session = payload.session.clone();
    let payload_turn_number = payload.turn.number;

    // Load core hook for fallback
    let core_hook = crate::flows::load_core_hook();

    // Build execution state from payload
    let mut state = AikiState::new(payload);

    // Execute hook via HookComposer (with fallback to bundled core hook)
    let _flow_result = match execute_hook(
        EventType::TurnCompleted,
        &mut state,
        &core_hook.handlers.turn_completed,
    ) {
        Ok(result) => result,
        Err(e) => {
            // Hook execution failed - log warning and skip autoreply
            eprintln!("\nturn.completed flow failed: {}", e);
            eprintln!("No autoreply generated.\n");
            return Ok(HookResult {
                context: state.build_context(),
                decision: Decision::Allow,
                failures: state.take_failures(),
            });
        }
    };

    // Extract failures from state
    let failures = state.take_failures();

    // Build the autoreply context
    let context = state.build_context();

    // Record autoreply to history if one was generated
    // Uses global JJ repo at ~/.aiki/.jj/ for cross-repo conversation history
    if let Some(autoreply_content) = context.as_ref() {
        // Best-effort - log and continue on failure (matches existing error handling)
        let autoreply_cwd = payload_cwd.to_string_lossy();
        let autoreply_repo_id = repos::compute_repo_id(&payload_cwd).ok();
        if let Err(e) = history::record_autoreply(
            &global::global_aiki_dir(),
            &payload_session,
            autoreply_content,
            payload_turn_number,
            Utc::now(),
            autoreply_repo_id.as_deref(),
            Some(&autoreply_cwd),
        ) {
            debug_log(|| format!("Failed to record autoreply: {}", e));
        }
    }

    // autoreply_to_end_session action sets Decision::Block so the agent's
    // stop hook receives a "stop" signal (e.g., { "continue": false } for Codex).
    let decision = if state.end_session {
        Decision::Block
    } else {
        Decision::Allow
    };

    Ok(HookResult {
        context,
        decision,
        failures,
    })
}
