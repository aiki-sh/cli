use super::prelude::*;
use crate::global;
use crate::history;
use crate::repos;
use crate::session::{prune_dead_pid_sessions, AikiSessionFile};

/// session.started event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiSessionStartPayload {
    pub session: AikiSession,
    pub cwd: PathBuf,
    pub timestamp: DateTime<Utc>,
    #[serde(default)]
    pub transcript_path: Option<String>,
}

/// Handle session.started event
///
/// Currently runs `aiki init --quiet` to ensure repository is initialized.
/// Also records the session start to conversation history (if not opted out).
/// Creates session file for PID-based session detection.
pub fn handle_session_started(payload: AikiSessionStartPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| format!("Session started by {:?}", payload.session.agent_type()));

    // Clear fetch-failure markers from a prior session so plugins are retried.
    if let Ok(plugins_base) = crate::plugins::plugins_base_dir() {
        crate::plugins::clear_all_fetch_failed(&plugins_base);
    }

    // Clean up sessions from crashed agents (PID-based)
    prune_dead_pid_sessions();

    // Create session file for PID-based session detection
    // This preserves the parent_pid from the payload session
    // Session files are stored globally at $AIKI_HOME/sessions/
    let session_file = AikiSessionFile::new(&payload.session);
    if let Err(e) = session_file.create() {
        debug_log(|| format!("Failed to create session file: {}", e));
    }

    // Write repo ID to session file so find_session_by_repo works as a fallback
    // when PID-based detection fails (e.g., Codex sandboxed tool execution)
    let cwd_str = payload.cwd.to_string_lossy();
    let repo_id = repos::compute_repo_id(&payload.cwd).ok();
    if let Some(ref id) = repo_id {
        if let Err(e) = session_file.add_repo(id) {
            debug_log(|| format!("Failed to add repo to session file: {}", e));
        }
    }

    // Record session start to conversation history (non-blocking on failure)
    // Uses global JJ repo at ~/.aiki/.jj/ for cross-repo conversation history
    if let Err(e) = history::record_session_start(
        &global::global_aiki_dir(),
        &payload.session,
        payload.timestamp,
        repo_id.as_deref(),
        Some(&cwd_str),
        payload.transcript_path.as_deref(),
    ) {
        debug_log(|| format!("Failed to record session start: {}", e));
    }

    // Load core hook for fallback
    let core_hook = crate::flows::load_core_hook();

    // Build execution state from payload
    let mut state = AikiState::new(payload);

    // Execute hook via HookComposer (with fallback to bundled core hook)
    let flow_result = execute_hook(
        EventType::SessionStarted,
        &mut state,
        &core_hook.handlers.session_started,
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
