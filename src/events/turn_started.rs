use super::prelude::*;
use super::Turn;
use crate::global;
use crate::history;
use crate::repos;
use crate::session::turn_state::TurnState;

// Re-export TurnSource from history for backward compatibility
// (TurnSource was previously defined here but moved to history to avoid cycles)
pub use crate::history::TurnSource;

/// turn.started event payload
///
/// Fires when a turn begins (user submits prompt OR autoreply is generated).
/// Each turn has a sequential number and deterministic turn_id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiTurnStartedPayload {
    pub session: AikiSession,
    pub cwd: PathBuf,
    pub timestamp: DateTime<Utc>,
    /// Turn metadata (number, id, source)
    #[serde(default)]
    pub turn: Turn,
    /// The prompt text (user input or autoreply context)
    pub prompt: String,
    /// References to files injected as context (paths only, not content)
    #[serde(default)]
    pub injected_refs: Vec<String>,
}

/// Handle turn.started event
///
/// This event fires when a turn begins (user submits prompt OR autoreply),
/// allowing flows to inject additional context (e.g., project conventions,
/// active files, etc.).
/// Also records the prompt to conversation history and returns the prompt_id
/// so agents can link tasks to the triggering prompt.
/// Returns context via `response.context` and failures via `response.failures`,
/// with graceful degradation on errors.
pub fn handle_turn_started(mut payload: AikiTurnStartedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    // Load turn state from JJ history
    // Uses global JJ repo at ~/.aiki/.jj/ for cross-repo conversation history
    let mut turn_state = TurnState::load(payload.session.uuid(), &global::global_aiki_dir());

    // Check if there's a pending autoreply from history
    // If the latest event for this session is an Autoreply event, set source to Autoreply
    // Defensive: if history lookup fails, we default to User source (no autoreply)
    // Uses global JJ repo at ~/.aiki/.jj/ for cross-repo conversation history
    let source = match history::has_pending_autoreply(&global::global_aiki_dir(), payload.session.uuid()) {
        Ok(true) => TurnSource::Autoreply,
        Ok(false) => TurnSource::User,
        Err(e) => {
            debug_log(|| {
                format!(
                    "Autoreply check failed for session {}, defaulting to User source: {}",
                    payload.session.uuid(),
                    e
                )
            });
            TurnSource::User
        }
    };

    // Increment turn counter and generate turn_id
    let turn_number = turn_state.start_turn(source.clone());
    payload.turn = Turn::new(
        turn_number,
        turn_state.current_turn_id.clone(),
        source.to_string(),
    );

    debug_log(|| {
        format!(
            "turn.started event from {:?}, source: {}, turn: {}, turn_id: {}, prompt length: {}",
            payload.session.agent_type(),
            payload.turn.source,
            payload.turn.number,
            payload.turn.id,
            payload.prompt.len()
        )
    });

    // Record prompt to conversation history (non-blocking on failure)
    // The prompt's change_id is stored in JJ and can be looked up later via
    // `--source prompt` which resolves to the latest prompt for this session
    // Uses global JJ repo at ~/.aiki/.jj/ for cross-repo conversation history
    let cwd_str = payload.cwd.to_string_lossy();
    let repo_id = repos::compute_repo_id(&payload.cwd).ok();
    if let Err(e) = history::record_prompt(
        &global::global_aiki_dir(),
        &payload.session,
        &payload.prompt,
        payload.injected_refs.clone(),
        payload.turn.number,
        source,
        payload.timestamp,
        repo_id.as_deref(),
        Some(&cwd_str),
    ) {
        debug_log(|| format!("Failed to record prompt: {}", e));
    }

    // Load core hook for fallback
    let core_hook = crate::flows::load_core_hook();

    // Build execution state from payload
    let mut state = AikiState::new(payload);

    // Execute hook via HookComposer (with fallback to bundled core hook)
    let flow_result = match execute_hook(
        EventType::TurnStarted,
        &mut state,
        &core_hook.handlers.turn_started,
    ) {
        Ok(result) => result,
        Err(e) => {
            // Hook execution failed - log warning and use original prompt
            eprintln!("turn.started flow failed: {}", e);
            eprintln!("Continuing with original prompt...\n");
            // Return built context (already initialized with original prompt)
            return Ok(HookResult {
                context: state.build_context(),
                decision: Decision::Allow,
                failures: state.take_failures(),
            });
        }
    };

    // Extract failures from state
    let failures = state.take_failures();

    // Return response based on flow result (build context string)
    match flow_result {
        HookOutcome::Success | HookOutcome::FailedContinue | HookOutcome::FailedStop => {
            Ok(HookResult {
                context: state.build_context(),
                decision: Decision::Allow,
                failures,
            })
        }
        HookOutcome::FailedBlock => {
            // Block the prompt - return exit code 2
            Ok(HookResult {
                context: None,
                decision: Decision::Block,
                failures,
            })
        }
    }
}
