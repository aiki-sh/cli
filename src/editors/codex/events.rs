use serde::Deserialize;
use std::path::PathBuf;

use super::session::create_session;
use crate::error::Result;
use crate::session::AikiSession;
use crate::events::{
    AikiEvent, AikiSessionClearedPayload, AikiSessionResumedPayload, AikiSessionStartPayload,
    AikiShellPermissionAskedPayload, AikiTurnCompletedPayload, AikiTurnStartedPayload, TokenUsage,
};
use crate::editors::transcript::{TranscriptEntry, TurnTranscript};

// ============================================================================
// Hook Payload Structures (matches Codex native hooks API)
// ============================================================================

/// Codex hook event - discriminated by hook_event_name
#[derive(Deserialize, Debug)]
#[serde(tag = "hook_event_name", deny_unknown_fields)]
enum CodexEvent {
    #[serde(rename = "SessionStart")]
    SessionStart {
        #[serde(flatten)]
        payload: SessionStartPayload,
    },
    #[serde(rename = "UserPromptSubmit")]
    UserPromptSubmit {
        #[serde(flatten)]
        payload: UserPromptSubmitPayload,
    },
    #[serde(rename = "PreToolUse")]
    PreToolUse {
        #[serde(flatten)]
        payload: PreToolUsePayload,
    },
    #[serde(rename = "Stop")]
    Stop {
        #[serde(flatten)]
        payload: StopPayload,
    },
}

/// SessionStart hook payload
///
/// Codex provides a `source` field indicating how the session started:
/// - "startup" - New session started
/// - "resume" - Session resumed
/// - "clear" - Session after clear
/// No "compact" variant — Codex doesn't have PreCompact.
#[derive(Deserialize, Debug, Clone, Copy)]
enum PermissionMode {
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "acceptEdits")]
    AcceptEdits,
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "dontAsk")]
    DontAsk,
    #[serde(rename = "bypassPermissions")]
    BypassPermissions,
}

#[derive(Deserialize, Debug, Clone, Copy)]
enum SessionStartSource {
    #[serde(rename = "startup")]
    Startup,
    #[serde(rename = "resume")]
    Resume,
    #[serde(rename = "clear")]
    Clear,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct SessionStartPayload {
    session_id: String,
    cwd: String,
    source: SessionStartSource,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    permission_mode: PermissionMode,
    transcript_path: Option<String>,
}

/// UserPromptSubmit hook payload
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct UserPromptSubmitPayload {
    session_id: String,
    cwd: String,
    prompt: String,
    #[allow(dead_code)]
    turn_id: String,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    permission_mode: PermissionMode,
    #[allow(dead_code)]
    transcript_path: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct PreToolUseToolInput {
    command: String,
}

/// PreToolUse hook payload
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct PreToolUsePayload {
    session_id: String,
    cwd: String,
    #[allow(dead_code)]
    tool_name: String,
    #[allow(dead_code)]
    tool_input: PreToolUseToolInput,
    #[allow(dead_code)]
    tool_use_id: String,
    #[allow(dead_code)]
    turn_id: String,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    permission_mode: PermissionMode,
    #[allow(dead_code)]
    transcript_path: Option<String>,
}

/// Stop hook payload
///
/// Unlike Claude Code, Codex carries `last_assistant_message` directly
/// in the payload — no transcript parsing needed.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct StopPayload {
    session_id: String,
    cwd: String,
    last_assistant_message: Option<String>,
    #[allow(dead_code)]
    stop_hook_active: bool,
    #[allow(dead_code)]
    turn_id: String,
    model: String,
    #[allow(dead_code)]
    permission_mode: PermissionMode,
    transcript_path: Option<String>,
}

// ============================================================================
// Event Building
// ============================================================================

/// Codex's primary event plus any supplemental events to dispatch first.
pub struct BuiltCodexEvents {
    pub supplemental_events: Vec<AikiEvent>,
    pub primary_event: AikiEvent,
}

/// Build Codex events from a pre-read JSON payload buffer (the stdin-once path).
pub(crate) fn build_aiki_event_from_json(payload: &[u8]) -> Result<BuiltCodexEvents> {
    let event: CodexEvent = serde_json::from_slice(payload).map_err(anyhow::Error::from)?;
    build_aiki_event_from_parsed(event)
}

fn build_aiki_event_from_parsed(event: CodexEvent) -> Result<BuiltCodexEvents> {
    let built = match event {
        CodexEvent::SessionStart { payload } => BuiltCodexEvents {
            supplemental_events: vec![],
            primary_event: build_session_started_event(payload),
        },
        CodexEvent::UserPromptSubmit { payload } => BuiltCodexEvents {
            supplemental_events: vec![],
            primary_event: build_turn_started_event(payload),
        },
        CodexEvent::PreToolUse { payload } => BuiltCodexEvents {
            supplemental_events: vec![],
            primary_event: build_shell_permission_asked_event(payload),
        },
        CodexEvent::Stop { payload } => build_stop_events(payload),
    };

    Ok(built)
}

/// Build session event based on SessionStart source field
///
/// Codex emits SessionStart for session lifecycle events.
/// The `source` field distinguishes them:
/// - "startup" or unknown → SessionStarted
/// - "resume" → SessionResumed
/// - "clear" → SessionCleared
/// No "compact" variant (Codex doesn't have PreCompact).
fn build_session_started_event(payload: SessionStartPayload) -> AikiEvent {
    let session = create_session(&payload.session_id, &payload.cwd);
    let cwd = PathBuf::from(&payload.cwd);
    let timestamp = chrono::Utc::now();

    match payload.source {
        SessionStartSource::Resume => AikiEvent::SessionResumed(AikiSessionResumedPayload {
            session,
            cwd,
            timestamp,
        }),
        SessionStartSource::Clear => AikiEvent::SessionCleared(AikiSessionClearedPayload {
            session,
            cwd,
            timestamp,
        }),
        SessionStartSource::Startup => AikiEvent::SessionStarted(AikiSessionStartPayload {
            session,
            cwd,
            timestamp,
            transcript_path: payload.transcript_path,
        }),
    }
}

/// Build turn.started event (maps from UserPromptSubmit hook)
fn build_turn_started_event(payload: UserPromptSubmitPayload) -> AikiEvent {
    AikiEvent::TurnStarted(AikiTurnStartedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        turn: crate::events::Turn::unknown(),
        prompt: payload.prompt,
        injected_refs: vec![],
    })
}

/// Build shell.permission_asked event (Codex currently only has Bash tool)
fn build_shell_permission_asked_event(payload: PreToolUsePayload) -> AikiEvent {
    AikiEvent::ShellPermissionAsked(AikiShellPermissionAskedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        command: payload.tool_input.command,
    })
}

/// Build turn.completed event (maps from Stop hook)
///
/// Codex carries `last_assistant_message` and `model` directly in the payload,
/// so those take precedence over transcript data. Token usage comes from the
/// transcript via the shared `TurnTranscript` aggregation.
fn build_turn_completed_event(payload: StopPayload) -> AikiEvent {
    // Build the session once: it both rides the event and locates the session
    // file used to persist the cumulative-token baseline across resumed rollouts.
    let session = create_session(&payload.session_id, &payload.cwd);
    let transcript = payload
        .transcript_path
        .as_deref()
        .map(|p| parse_codex_transcript(p, &session))
        .unwrap_or_default();

    AikiEvent::TurnCompleted(AikiTurnCompletedPayload {
        session,
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        turn: crate::events::Turn::unknown(),
        response: payload.last_assistant_message.unwrap_or(transcript.response),
        modified_files: vec![],
        tasks: Default::default(),
        tokens: transcript.tokens,
        model: Some(payload.model).or(transcript.model),
    })
}

fn build_stop_events(payload: StopPayload) -> BuiltCodexEvents {
    BuiltCodexEvents {
        supplemental_events: vec![],
        primary_event: build_turn_completed_event(payload),
    }
}

// ============================================================================
// Token Usage Parsing
// ============================================================================

/// Token usage counts from `last_token_usage` / `total_token_usage` in Codex
/// session JSONL `event_msg` events with `payload.type == "token_count"`.
#[derive(Deserialize, Debug, Clone, Default)]
struct CodexTokenUsageDetail {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    /// Reasoning tokens billed as output (large for reasoning models). Codex
    /// carries this in `token_count`; the parent structs lack
    /// `deny_unknown_fields`, so older fixtures without it parse to 0.
    #[serde(default)]
    reasoning_output_tokens: u64,
}

impl CodexTokenUsageDetail {
    /// Input tokens excluding the cached portion. OpenAI/Codex `input_tokens` is
    /// the full prompt total and already INCLUDES `cached_input_tokens`, so the
    /// disjoint `input` bucket (matching the Anthropic mapping used elsewhere) is
    /// the difference of the two.
    fn uncached_input(&self) -> u64 {
        self.input_tokens.saturating_sub(self.cached_input_tokens)
    }

    /// Tokens billed as output: the visible output plus reasoning tokens.
    fn total_output(&self) -> u64 {
        self.output_tokens.saturating_add(self.reasoning_output_tokens)
    }

    /// Total billed tokens for this cumulative snapshot. `input_tokens` already
    /// includes `cached_input_tokens`, so cached is not added again here. Used
    /// only to detect a non-monotonic (decreasing) cumulative total.
    fn total(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.reasoning_output_tokens)
    }

    /// Per-field saturating difference of two cumulative snapshots. Used to
    /// derive the carried-over baseline implied by the first `token_count` of a
    /// resumed rollout (`first.total_token_usage - first.last_token_usage`).
    fn saturating_sub(&self, other: &Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_sub(other.input_tokens),
            output_tokens: self.output_tokens.saturating_sub(other.output_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .saturating_sub(other.cached_input_tokens),
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .saturating_sub(other.reasoning_output_tokens),
        }
    }

    /// Serialize the four cumulative buckets for the session-file baseline line
    /// as `input,cached,output,reasoning`.
    fn to_state_string(&self) -> String {
        format!(
            "{},{},{},{}",
            self.input_tokens,
            self.cached_input_tokens,
            self.output_tokens,
            self.reasoning_output_tokens
        )
    }

    /// Parse the `input,cached,output,reasoning` baseline written by
    /// [`to_state_string`](Self::to_state_string). Returns `None` on any
    /// malformed field or wrong arity.
    fn from_state_string(s: &str) -> Option<Self> {
        let mut parts = s.split(',').map(|p| p.trim().parse::<u64>().ok());
        let input_tokens = parts.next()??;
        let cached_input_tokens = parts.next()??;
        let output_tokens = parts.next()??;
        let reasoning_output_tokens = parts.next()??;
        if parts.next().is_some() {
            return None; // too many fields
        }
        Some(Self {
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_output_tokens,
        })
    }
}

/// The `info` object inside a `token_count` payload.
#[derive(Deserialize, Debug, Clone)]
struct CodexTokenCountInfo {
    /// Usage of the LAST API call only (not cumulative). Used to derive the
    /// carried-over baseline on a resumed rollout's first `token_count`.
    last_token_usage: CodexTokenUsageDetail,
    total_token_usage: CodexTokenUsageDetail,
}

/// Payload of a `token_count` event_msg.
#[derive(Deserialize, Debug, Clone)]
struct CodexTokenCountPayload {
    /// `null` on the initial event before any API call completes.
    info: Option<CodexTokenCountInfo>,
}

/// Top-level JSONL line: `{"type":"event_msg","payload":{...}}`
#[derive(Deserialize, Debug, Clone)]
struct CodexEventMsg {
    payload: CodexTokenCountPayload,
}

/// Session-file key holding the last cumulative `total_token_usage` seen for a
/// Codex session, used as the per-turn baseline when a resumed rollout file
/// carries the running total across files (defect A5).
const CODEX_TOKEN_TOTAL_KEY: &str = "codex_token_total";

/// Per-turn extraction plus the cumulative total to persist for the next turn.
struct CodexTurnUsage {
    entries: Vec<TranscriptEntry>,
    /// Latest cumulative `total_token_usage` in the file, persisted as the
    /// baseline for the next turn. `None` when the file carried no usable usage.
    last_cumulative: Option<CodexTokenUsageDetail>,
}

/// Parse a Codex rollout into a [`TurnTranscript`], using persisted session
/// state to keep the per-turn baseline correct across resumed rollout files
/// (defect A5). Reads the persisted cumulative baseline, computes the turn
/// delta, and writes back the new cumulative for the next turn.
fn parse_codex_transcript(path: &str, session: &AikiSession) -> TurnTranscript {
    let baseline = read_persisted_baseline(session);
    // Retry once on an empty first read: like Claude, the Stop hook can fire
    // before the final `token_count` is flushed to the rollout file.
    for attempt in 0..2 {
        let Ok(content) = std::fs::read_to_string(path) else {
            return TurnTranscript::default();
        };
        let usage = compute_turn_usage(&content, baseline.as_ref());
        if !usage.entries.is_empty() {
            if let Some(total) = &usage.last_cumulative {
                persist_baseline(session, total);
            }
            return TurnTranscript::from_entries(usage.entries);
        }
        if attempt == 0 {
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }
    TurnTranscript::default()
}

/// Read the persisted cumulative-token baseline for this Codex session, if any.
fn read_persisted_baseline(session: &AikiSession) -> Option<CodexTokenUsageDetail> {
    session
        .file()
        .read_metadata_value(CODEX_TOKEN_TOTAL_KEY)
        .as_deref()
        .and_then(CodexTokenUsageDetail::from_state_string)
}

/// Persist the latest cumulative total as the next turn's baseline. Best-effort:
/// a failed write only means the next turn falls back to the in-file baseline.
fn persist_baseline(session: &AikiSession, total: &CodexTokenUsageDetail) {
    let _ = session
        .file()
        .upsert_metadata_value(CODEX_TOKEN_TOTAL_KEY, &total.to_state_string());
}

/// Compute per-turn token usage from Codex JSONL content, optionally using a
/// persisted cumulative baseline.
///
/// Codex emits `token_count` events with cumulative `total_token_usage`. Per-turn
/// usage is `(last total in file) - baseline`, where the baseline is the last
/// cumulative total before the current `turn_context` boundary. For the first
/// turn in a file there is no in-file pre-boundary total
/// (`last_turn_boundary_idx == 0`); that is the resume-prone shape handled by
/// [`resolve_resume_baseline`].
fn compute_turn_usage(
    content: &str,
    persisted_baseline: Option<&CodexTokenUsageDetail>,
) -> CodexTurnUsage {
    // Track the cumulative total at each token_count event, and where turn
    // boundaries fall, so we can compute the delta for the current turn.
    let mut all_totals: Vec<CodexTokenUsageDetail> = Vec::new();
    let mut last_turn_boundary_idx: usize = 0; // index into all_totals
    // `last_token_usage` of the FIRST token_count: lets us derive the baseline a
    // resumed rollout carried in (`first.total - first.last`).
    let mut first_last_usage: Option<CodexTokenUsageDetail> = None;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Parse the line as JSON and switch on its `type` fields rather than
        // substring-sniffing: the literal `turn_context`/`token_count` strings
        // can appear inside unrelated content (agent messages, reasoning), so a
        // `contains` check risks false boundaries / false positives.
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        // A turn boundary is a top-level `{"type":"turn_context",...}` line.
        if val.get("type").and_then(|t| t.as_str()) == Some("turn_context") {
            // Mark boundary: baseline for next turn is whatever total we've seen so far
            last_turn_boundary_idx = all_totals.len();
            continue;
        }
        // Token usage lives in an `event_msg` whose `payload.type` is `token_count`.
        let is_token_count = val
            .get("payload")
            .and_then(|p| p.get("type"))
            .and_then(|t| t.as_str())
            == Some("token_count");
        if is_token_count {
            if let Ok(msg) = serde_json::from_value::<CodexEventMsg>(val) {
                if let Some(info) = msg.payload.info {
                    if first_last_usage.is_none() {
                        first_last_usage = Some(info.last_token_usage);
                    }
                    all_totals.push(info.total_token_usage);
                }
            }
        }
    }

    let last = match all_totals.last() {
        Some(t) => t.clone(),
        None => {
            return CodexTurnUsage {
                entries: vec![],
                last_cumulative: None,
            }
        }
    };

    // Baseline: last total before the current turn boundary. With no in-file
    // pre-boundary total (turn 1, including a resumed file's first turn), defer
    // to the resume-aware resolver instead of assuming zero.
    let baseline = if last_turn_boundary_idx > 0 {
        all_totals[last_turn_boundary_idx - 1].clone()
    } else {
        resolve_resume_baseline(
            persisted_baseline,
            all_totals.first(),
            first_last_usage.as_ref(),
            &last,
        )
    };

    // The cumulative total must be monotonic: `last` should never fall below the
    // baseline. If it does, the baseline is mismatched (e.g. the A5 resume bug
    // picked the wrong boundary). Surface that and skip rather than letting the
    // per-bucket `saturating_sub` below silently clamp the delta to 0, which
    // would masquerade as a real (but empty) turn.
    if last.total() < baseline.total() {
        eprintln!(
            "[aiki] Warning: Codex cumulative token total decreased \
             (last={} < baseline={}); skipping turn delta to avoid reporting a \
             clamped-to-zero usage. Likely a turn-boundary/baseline mismatch.",
            last.total(),
            baseline.total()
        );
        return CodexTurnUsage {
            entries: vec![],
            last_cumulative: Some(last),
        };
    }

    let entry = TranscriptEntry {
        // Codex's token_count payload has no stable per-call message id.
        id: None,
        response: None,
        model: None,
        tokens: Some(TokenUsage {
            // Disjoint buckets: `input` excludes cached (Codex `input_tokens`
            // already includes it), `cache_read` holds the cached delta, and
            // `output` folds in reasoning tokens.
            input: last.uncached_input().saturating_sub(baseline.uncached_input()),
            output: last.total_output().saturating_sub(baseline.total_output()),
            cache_read: last
                .cached_input_tokens
                .saturating_sub(baseline.cached_input_tokens),
            // Codex's token_count payload exposes no cache-creation count, so this
            // stays 0. Wire it through here if Codex ever reports one.
            cache_created: 0,
        }),
    };

    CodexTurnUsage {
        entries: vec![entry],
        last_cumulative: Some(last),
    }
}

/// Choose the baseline for a file with no in-file pre-boundary total
/// (`last_turn_boundary_idx == 0`) — the resume-prone shape.
///
/// `derived` is the cumulative implied by the first `token_count` itself
/// (`first.total_token_usage - first.last_token_usage`):
/// - For a fresh / reset rollout it is ZERO, because Codex restarts the
///   cumulative counter so `total == last` on the first event. This is the
///   empirically observed behavior across real rollouts (open question 4), and
///   keeping the zero baseline preserves the historical (correct) result.
/// - For a rollout that genuinely continues a prior cumulative it equals the
///   carried-in prior, so subtracting it yields just this turn's usage.
///
/// The persisted session baseline is preferred when it is consistent with the
/// file: not greater than the file's last total (monotonic) and not less than
/// the in-file `derived` evidence. A reset rollout makes
/// `persisted.total() > last.total()`, so we fall back to `derived` and never
/// subtract a stale prior cumulative against a freshly-restarted counter.
fn resolve_resume_baseline(
    persisted: Option<&CodexTokenUsageDetail>,
    first_total: Option<&CodexTokenUsageDetail>,
    first_last: Option<&CodexTokenUsageDetail>,
    last: &CodexTokenUsageDetail,
) -> CodexTokenUsageDetail {
    let derived = match (first_total, first_last) {
        (Some(ft), Some(fl)) => ft.saturating_sub(fl),
        _ => CodexTokenUsageDetail::default(),
    };
    match persisted {
        Some(p) if p.total() <= last.total() && p.total() >= derived.total() => p.clone(),
        _ => derived,
    }
}

/// Thin pure wrapper preserving the `fn(&str) -> Vec<TranscriptEntry>` shape
/// used by the parser unit tests. Uses a zero baseline (no persisted state).
#[cfg(test)]
fn parse_transcript_lines(content: &str) -> Vec<TranscriptEntry> {
    compute_turn_usage(content, None).entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session_start(source: &str) -> SessionStartPayload {
        SessionStartPayload {
            session_id: "test-session-123".to_string(),
            cwd: "/tmp/test".to_string(),
            source: match source {
                "resume" => SessionStartSource::Resume,
                "clear" => SessionStartSource::Clear,
                _ => SessionStartSource::Startup,
            },
            model: "o3".to_string(),
            permission_mode: PermissionMode::Default,
            transcript_path: None,
        }
    }

    #[test]
    fn test_session_start_startup_maps_to_session_started() {
        let event = build_session_started_event(make_session_start("startup"));
        assert!(
            matches!(event, AikiEvent::SessionStarted(_)),
            "SessionStart(source=startup) should map to SessionStarted"
        );
    }

    #[test]
    fn test_session_start_resume_maps_to_session_resumed() {
        let event = build_session_started_event(make_session_start("resume"));
        match event {
            AikiEvent::SessionResumed(payload) => {
                assert_eq!(payload.session.external_id(), "test-session-123");
                assert_eq!(payload.cwd, PathBuf::from("/tmp/test"));
            }
            _ => panic!("SessionStart(source=resume) should map to SessionResumed"),
        }
    }

    #[test]
    fn test_session_start_clear_maps_to_session_cleared() {
        let event = build_session_started_event(make_session_start("clear"));
        match event {
            AikiEvent::SessionCleared(payload) => {
                assert_eq!(payload.session.external_id(), "test-session-123");
                assert_eq!(payload.cwd, PathBuf::from("/tmp/test"));
            }
            _ => panic!("SessionStart(source=clear) should map to SessionCleared"),
        }
    }

    #[test]
    fn test_session_start_deserialization_with_source() {
        let json = r#"{"hook_event_name":"SessionStart","session_id":"abc","cwd":"/tmp","source":"resume","model":"o3","permission_mode":"default","transcript_path":null}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::SessionStart { payload } => {
                assert!(matches!(payload.source, SessionStartSource::Resume));
            }
            _ => panic!("Expected SessionStart variant"),
        }
    }

    #[test]
    fn test_session_start_deserialization_requires_source() {
        let json = r#"{"hook_event_name":"SessionStart","session_id":"abc","cwd":"/tmp","model":"o3","permission_mode":"default","transcript_path":null}"#;
        assert!(serde_json::from_str::<CodexEvent>(json).is_err());
    }

    #[test]
    fn test_user_prompt_submit_deserialization() {
        let json = r#"{"hook_event_name":"UserPromptSubmit","session_id":"abc","cwd":"/tmp","prompt":"Fix the bug","turn_id":"turn-1","model":"o3","permission_mode":"default","transcript_path":null}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::UserPromptSubmit { payload } => {
                assert_eq!(payload.prompt, "Fix the bug");
                assert_eq!(payload.session_id, "abc");
            }
            _ => panic!("Expected UserPromptSubmit variant"),
        }
    }

    #[test]
    fn test_pre_tool_use_deserialization() {
        let json = r#"{"hook_event_name":"PreToolUse","session_id":"abc","cwd":"/tmp","tool_name":"Bash","tool_input":{"command":"cargo test"},"tool_use_id":"tool-xyz","turn_id":"turn-1","model":"o3","permission_mode":"default","transcript_path":null}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::PreToolUse { payload } => {
                assert_eq!(payload.tool_name, "Bash");
                assert_eq!(payload.tool_input.command, "cargo test");
            }
            _ => panic!("Expected PreToolUse variant"),
        }
    }

    #[test]
    fn test_stop_deserialization() {
        let json = r#"{"hook_event_name":"Stop","session_id":"abc","cwd":"/tmp","last_assistant_message":"Done fixing","stop_hook_active":true,"turn_id":"turn-1","model":"o3","permission_mode":"default","transcript_path":null}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::Stop { payload } => {
                assert_eq!(
                    payload.last_assistant_message,
                    Some("Done fixing".to_string())
                );
            }
            _ => panic!("Expected Stop variant"),
        }
    }

    #[test]
    fn test_turn_started_event_uses_prompt() {
        let payload = UserPromptSubmitPayload {
            session_id: "test-session".to_string(),
            cwd: "/tmp/test".to_string(),
            prompt: "Fix the login bug".to_string(),
            turn_id: "turn-1".to_string(),
            model: "o3".to_string(),
            permission_mode: PermissionMode::Default,
            transcript_path: None,
        };
        let event = build_turn_started_event(payload);
        match event {
            AikiEvent::TurnStarted(p) => {
                assert_eq!(p.prompt, "Fix the login bug");
            }
            _ => panic!("Expected TurnStarted"),
        }
    }

    #[test]
    fn test_shell_permission_extracts_command() {
        let payload = PreToolUsePayload {
            session_id: "test-session".to_string(),
            cwd: "/tmp/test".to_string(),
            tool_name: "Bash".to_string(),
            tool_input: PreToolUseToolInput {
                command: "cargo test".to_string(),
            },
            tool_use_id: "tool-1".to_string(),
            turn_id: "turn-1".to_string(),
            model: "o3".to_string(),
            permission_mode: PermissionMode::Default,
            transcript_path: None,
        };
        let event = build_shell_permission_asked_event(payload);
        match event {
            AikiEvent::ShellPermissionAsked(p) => {
                assert_eq!(p.command, "cargo test");
            }
            _ => panic!("Expected ShellPermissionAsked"),
        }
    }

    #[test]
    fn test_turn_completed_uses_last_assistant_message() {
        let payload = StopPayload {
            session_id: "test-session".to_string(),
            cwd: "/tmp/test".to_string(),
            last_assistant_message: Some("I fixed the bug".to_string()),
            stop_hook_active: true,
            turn_id: "turn-1".to_string(),
            model: "o3".to_string(),
            permission_mode: PermissionMode::Default,
            transcript_path: None,
        };
        let event = build_turn_completed_event(payload);
        match event {
            AikiEvent::TurnCompleted(p) => {
                assert_eq!(p.response, "I fixed the bug");
            }
            _ => panic!("Expected TurnCompleted"),
        }
    }

    #[test]
    fn test_turn_completed_empty_message() {
        let payload = StopPayload {
            session_id: "test-session".to_string(),
            cwd: "/tmp/test".to_string(),
            last_assistant_message: None,
            stop_hook_active: true,
            turn_id: "turn-1".to_string(),
            model: "o3".to_string(),
            permission_mode: PermissionMode::Default,
            transcript_path: None,
        };
        let event = build_turn_completed_event(payload);
        match event {
            AikiEvent::TurnCompleted(p) => {
                assert_eq!(p.response, "");
            }
            _ => panic!("Expected TurnCompleted"),
        }
    }

    #[test]
    fn test_turn_completed_extracts_model() {
        let payload = StopPayload {
            session_id: "test-session".to_string(),
            cwd: "/tmp/test".to_string(),
            last_assistant_message: None,
            stop_hook_active: true,
            turn_id: "turn-1".to_string(),
            model: "o3".to_string(),
            permission_mode: PermissionMode::Default,
            transcript_path: None,
        };
        let event = build_turn_completed_event(payload);
        match event {
            AikiEvent::TurnCompleted(p) => {
                assert_eq!(p.model, Some("o3".to_string()));
            }
            _ => panic!("Expected TurnCompleted"),
        }
    }

    #[test]
    fn test_user_prompt_submit_rejects_unknown_fields() {
        let json = r#"{"hook_event_name":"UserPromptSubmit","session_id":"abc","cwd":"/tmp","prompt":"Fix the bug","turn_id":"turn-1","model":"o3","permission_mode":"default","transcript_path":null,"extra":"nope"}"#;
        assert!(serde_json::from_str::<CodexEvent>(json).is_err());
    }

    #[test]
    fn test_pre_tool_use_requires_command_string() {
        let json = r#"{"hook_event_name":"PreToolUse","session_id":"abc","cwd":"/tmp","tool_name":"Bash","tool_input":{},"tool_use_id":"tool-xyz","turn_id":"turn-1","model":"o3","permission_mode":"default","transcript_path":null}"#;
        assert!(serde_json::from_str::<CodexEvent>(json).is_err());
    }

    use crate::editors::transcript::TurnTranscript;

    /// Helper: parse content and aggregate via TurnTranscript
    fn parse_and_aggregate(content: &str) -> TurnTranscript {
        TurnTranscript::from_entries(parse_transcript_lines(content))
    }

    #[test]
    fn test_parse_token_usage_single_event() {
        let content = r#"{"type":"event_msg","payload":{"type":"agent_message","message":"hello"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"output_tokens":500,"cached_input_tokens":200,"reasoning_output_tokens":50,"total_tokens":1750},"total_token_usage":{"input_tokens":1000,"output_tokens":500,"cached_input_tokens":200,"reasoning_output_tokens":50,"total_tokens":1750},"model_context_window":258400},"rate_limits":null}}
"#;
        let extract = parse_and_aggregate(content);
        let usage = extract.tokens.unwrap();
        // Disjoint: input excludes cached (1000 - 200), output folds reasoning
        // (500 + 50), cache_read holds the cached count.
        assert_eq!(usage.input, 800);
        assert_eq!(usage.output, 550);
        assert_eq!(usage.cache_read, 200);
        assert_eq!(usage.cache_created, 0);
    }

    #[test]
    fn test_parse_token_usage_uses_last_total() {
        // Multiple API calls in one turn — use last total_token_usage (no turn boundary = baseline zero)
        let content = r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"output_tokens":500,"cached_input_tokens":200,"reasoning_output_tokens":50,"total_tokens":1750},"total_token_usage":{"input_tokens":1000,"output_tokens":500,"cached_input_tokens":200,"reasoning_output_tokens":50,"total_tokens":1750},"model_context_window":258400},"rate_limits":null}}
{"type":"event_msg","payload":{"type":"agent_message","message":"working..."}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":24014,"output_tokens":98,"cached_input_tokens":23808,"reasoning_output_tokens":13,"total_tokens":24112},"total_token_usage":{"input_tokens":47759,"output_tokens":249,"cached_input_tokens":27264,"reasoning_output_tokens":79,"total_tokens":48008},"model_context_window":258400},"rate_limits":null}}
"#;
        let extract = parse_and_aggregate(content);
        let usage = extract.tokens.unwrap();
        // Last total_token_usage, baseline is zero (no turn_context). Disjoint:
        // input excludes cached (47759 - 27264), output folds reasoning
        // (249 + 79), cache_read holds the cached count.
        assert_eq!(usage.input, 20495);
        assert_eq!(usage.output, 328);
        assert_eq!(usage.cache_read, 27264);
        assert_eq!(usage.cache_created, 0);
    }

    #[test]
    fn test_parse_token_usage_multi_turn_uses_delta() {
        // Turn 1: one API call. Turn 2: stale duplicate + two API calls.
        // At Stop for turn 2, file has all events. Baseline = last total before turn_context.
        let content = r#"{"type":"turn_context","payload":{"turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"output_tokens":50,"cached_input_tokens":500},"total_token_usage":{"input_tokens":1000,"output_tokens":50,"cached_input_tokens":500}}}}
{"type":"turn_context","payload":{"turn_id":"turn-2"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"output_tokens":50,"cached_input_tokens":500},"total_token_usage":{"input_tokens":1000,"output_tokens":50,"cached_input_tokens":500}}}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":2000,"output_tokens":100,"cached_input_tokens":1800},"total_token_usage":{"input_tokens":3000,"output_tokens":150,"cached_input_tokens":2300}}}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":2100,"output_tokens":80,"cached_input_tokens":1900},"total_token_usage":{"input_tokens":5100,"output_tokens":230,"cached_input_tokens":4200}}}}
"#;
        let extract = parse_and_aggregate(content);
        let usage = extract.tokens.unwrap();
        // Delta: last total (5100/230/4200) - baseline before turn-2 (1000/50/500).
        // Disjoint input is the delta of uncached input: (5100-4200) - (1000-500).
        assert_eq!(usage.input, 400);
        assert_eq!(usage.output, 180);
        assert_eq!(usage.cache_read, 3700);
    }

    #[test]
    fn test_parse_token_usage_ignores_substring_turn_context() {
        // A6: an agent_message whose value is literally "turn_context" must NOT
        // be mistaken for a turn boundary. Substring matching would falsely
        // shift the baseline; JSON `type` matching keeps the baseline at zero.
        let content = r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"output_tokens":50,"cached_input_tokens":500},"total_token_usage":{"input_tokens":1000,"output_tokens":50,"cached_input_tokens":500}}}}
{"type":"event_msg","payload":{"type":"agent_message","message":"turn_context"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":2000,"output_tokens":100,"cached_input_tokens":1800},"total_token_usage":{"input_tokens":3000,"output_tokens":150,"cached_input_tokens":2300}}}}
"#;
        let extract = parse_and_aggregate(content);
        let usage = extract.tokens.unwrap();
        // No real turn_context line → baseline is zero → full delta of the last
        // total (3000/150/2300). Substring matching would have used 1000/50/500
        // as the baseline and reported input=200, output=100, cache_read=1800.
        assert_eq!(usage.input, 700);
        assert_eq!(usage.output, 150);
        assert_eq!(usage.cache_read, 2300);
    }

    #[test]
    fn test_parse_token_usage_skips_decreasing_total() {
        // A7: if the cumulative total at Stop is BELOW the baseline (a sign of a
        // baseline mismatch), the delta is skipped rather than clamped to 0.
        let content = r#"{"type":"turn_context","payload":{"turn_id":"turn-1"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":5000,"output_tokens":300,"cached_input_tokens":1000},"total_token_usage":{"input_tokens":5000,"output_tokens":300,"cached_input_tokens":1000}}}}
{"type":"turn_context","payload":{"turn_id":"turn-2"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":2000,"output_tokens":100,"cached_input_tokens":500},"total_token_usage":{"input_tokens":2000,"output_tokens":100,"cached_input_tokens":500}}}}
"#;
        // last total (2000/100/500) is below the turn-2 baseline (5000/300/1000),
        // so the parser surfaces the inconsistency and yields no entries — the
        // turn reports no tokens instead of a clamped-to-zero usage.
        assert!(parse_and_aggregate(content).tokens.is_none());
    }

    #[test]
    fn test_parse_token_usage_skips_null_info() {
        // First token_count event has info: null, second has data
        let content = r#"{"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":null}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":5000,"output_tokens":300,"cached_input_tokens":1000,"reasoning_output_tokens":20,"total_tokens":5300},"total_token_usage":{"input_tokens":5000,"output_tokens":300,"cached_input_tokens":1000,"reasoning_output_tokens":20,"total_tokens":5300},"model_context_window":258400},"rate_limits":null}}
"#;
        let extract = parse_and_aggregate(content);
        let usage = extract.tokens.unwrap();
        // Disjoint: input excludes cached (5000 - 1000), output folds reasoning
        // (300 + 20), cache_read holds the cached count.
        assert_eq!(usage.input, 4000);
        assert_eq!(usage.output, 320);
        assert_eq!(usage.cache_read, 1000);
    }

    #[test]
    fn test_parse_token_usage_no_events() {
        let content = r#"{"type":"event_msg","payload":{"type":"agent_message","message":"hello"}}
{"type":"event_msg","payload":{"type":"agent_message","message":"world"}}
"#;
        assert!(parse_and_aggregate(content).tokens.is_none());
    }

    #[test]
    fn test_parse_token_usage_empty_content() {
        assert!(parse_and_aggregate("").tokens.is_none());
    }

    #[test]
    fn test_parse_token_usage_only_null_info() {
        // Only token_count events with info: null — should return no tokens
        let content = r#"{"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":null}}
"#;
        assert!(parse_and_aggregate(content).tokens.is_none());
    }

    #[test]
    fn test_golden_multi_turn_resume_fixture_parses() {
        // Drive the committed golden rollout through the real harness parser.
        // Phase 0 (D2) asserts only structural facts that survive the A3/A4/A5
        // extractor fixes — the exact per-bucket TokenUsage assertions land with
        // the Phase 2/4 changes. See cli/tests/fixtures/tokens/.
        let content =
            include_str!("../../../tests/fixtures/tokens/codex_multi_turn_resume.jsonl");
        let extract = parse_and_aggregate(content);

        // A cumulative multi-turn sequence with cached input and reasoning
        // output exercises every bucket the current parser populates.
        let usage = extract.tokens.expect("fixture carries cumulative usage");
        assert!(usage.input > 0, "input present");
        assert!(usage.output > 0, "output present");
        assert!(usage.cache_read > 0, "cache_read present");
    }

    // ---- A5: resume baseline via persisted session state -------------------

    /// Slice the golden rollout to just the first resumed turn: everything up to
    /// (but not including) the SECOND `turn_context` line. This reproduces the
    /// A5-triggering shape — a `turn_context` with a large cumulative total and
    /// no preceding `token_count` (`last_turn_boundary_idx == 0`).
    fn first_resumed_turn(content: &str) -> String {
        let mut out = String::new();
        let mut turn_contexts = 0;
        for line in content.lines() {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                if val.get("type").and_then(|t| t.as_str()) == Some("turn_context") {
                    turn_contexts += 1;
                    if turn_contexts == 2 {
                        break;
                    }
                }
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    #[test]
    fn test_resume_first_turn_uses_persisted_baseline() {
        // The first resumed turn carries a cumulative total of 52000 input that
        // includes the prior session. Without a baseline the delta would be the
        // full cumulative (input 11000 / output 3920 / cache 41000). With the
        // prior cumulative supplied from persisted session state, the turn
        // reports only its own usage.
        let full = include_str!("../../../tests/fixtures/tokens/codex_multi_turn_resume.jsonl");
        let slice = first_resumed_turn(full);

        // Prior cumulative carried into the resumed rollout (= first.total -
        // first.last for the fixture's opening token_count).
        let persisted = CodexTokenUsageDetail {
            input_tokens: 40000,
            output_tokens: 2500,
            cached_input_tokens: 32000,
            reasoning_output_tokens: 640,
        };

        let usage = compute_turn_usage(&slice, Some(&persisted));
        let extract = TurnTranscript::from_entries(usage.entries);
        let tokens = extract.tokens.expect("first resumed turn has usage");

        // Corrected attribution: just this turn's call (disjoint input
        // 12000-9000, output 600+180, cache 9000) — NOT the full cumulative.
        assert_eq!(tokens.input, 3000, "input is the turn delta, not 11000");
        assert_eq!(tokens.output, 780, "output is the turn delta, not 3920");
        assert_eq!(tokens.cache_read, 9000, "cache_read is the turn delta, not 41000");

        // The latest cumulative is returned for persistence as the next baseline.
        let next = usage.last_cumulative.expect("cumulative to persist");
        assert_eq!(next.input_tokens, 52000);
        assert_eq!(next.cached_input_tokens, 41000);
    }

    #[test]
    fn test_resume_first_turn_derives_baseline_without_persisted() {
        // Even without persisted state, the carried-over baseline is derivable
        // in-file (first.total - first.last), so a resumed first turn is not
        // inflated to the full cumulative.
        let full = include_str!("../../../tests/fixtures/tokens/codex_multi_turn_resume.jsonl");
        let slice = first_resumed_turn(full);

        let usage = compute_turn_usage(&slice, None);
        let tokens = TurnTranscript::from_entries(usage.entries)
            .tokens
            .expect("first resumed turn has usage");
        assert_eq!(tokens.input, 3000);
        assert_eq!(tokens.output, 780);
        assert_eq!(tokens.cache_read, 9000);
    }

    #[test]
    fn test_resume_first_turn_without_baseline_would_inflate() {
        // Documents the pre-fix behavior the baseline guards against: a fresh
        // (reset) first token_count where total == last has derived baseline 0,
        // so the full first-call cumulative is the turn — correct for a fresh
        // session, and the exact shape that misattributes on a *carryover*
        // resume if no baseline is derived. Here total != last (carryover), and
        // the in-file derivation keeps it correct (covered above); this test
        // pins the inflated value to make the contrast explicit.
        let full = include_str!("../../../tests/fixtures/tokens/codex_multi_turn_resume.jsonl");
        let slice = first_resumed_turn(full);

        // Force the legacy zero baseline by handing a first token_count whose
        // total == last (no carryover signal) and no persisted state: that is
        // the only shape that legitimately attributes the full cumulative.
        let reset = r#"{"type":"turn_context","payload":{}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":52000,"cached_input_tokens":41000,"output_tokens":3100,"reasoning_output_tokens":820},"total_token_usage":{"input_tokens":52000,"cached_input_tokens":41000,"output_tokens":3100,"reasoning_output_tokens":820}}}}
"#;
        let fresh = TurnTranscript::from_entries(compute_turn_usage(reset, None).entries)
            .tokens
            .expect("fresh turn has usage");
        // Fresh session turn 1 legitimately bills the whole cumulative.
        assert_eq!(fresh.input, 11000);
        assert_eq!(fresh.output, 3920);
        assert_eq!(fresh.cache_read, 41000);

        // The carryover slice (total != last) does NOT inflate — sanity re-check.
        let carry = TurnTranscript::from_entries(compute_turn_usage(&slice, None).entries)
            .tokens
            .expect("resumed turn has usage");
        assert!(carry.input < fresh.input, "carryover turn must not inflate");
    }

    #[test]
    fn test_reset_rollout_ignores_inconsistent_persisted_baseline() {
        // Real Codex restarts the cumulative counter per rollout file, so a
        // resumed file's first token_count has total == last (no carryover).
        // A stale persisted baseline larger than the file's total must be
        // ignored (monotonic guard), keeping the historical zero-baseline result
        // rather than clamping/under-counting.
        let reset = r#"{"type":"turn_context","payload":{}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":5000,"output_tokens":300,"cached_input_tokens":1000,"reasoning_output_tokens":20},"total_token_usage":{"input_tokens":5000,"output_tokens":300,"cached_input_tokens":1000,"reasoning_output_tokens":20}}}}
"#;
        let stale = CodexTokenUsageDetail {
            input_tokens: 100_000,
            output_tokens: 5000,
            cached_input_tokens: 80_000,
            reasoning_output_tokens: 1000,
        };
        let tokens = TurnTranscript::from_entries(compute_turn_usage(reset, Some(&stale)).entries)
            .tokens
            .expect("reset turn has usage");
        // Full first-call delta against a zero baseline (stale persisted ignored).
        assert_eq!(tokens.input, 4000);
        assert_eq!(tokens.output, 320);
        assert_eq!(tokens.cache_read, 1000);
    }

    #[test]
    fn test_token_usage_state_string_round_trip() {
        let detail = CodexTokenUsageDetail {
            input_tokens: 81000,
            output_tokens: 5400,
            cached_input_tokens: 63000,
            reasoning_output_tokens: 1450,
        };
        let s = detail.to_state_string();
        assert_eq!(s, "81000,63000,5400,1450");
        let parsed = CodexTokenUsageDetail::from_state_string(&s).unwrap();
        assert_eq!(parsed.input_tokens, 81000);
        assert_eq!(parsed.cached_input_tokens, 63000);
        assert_eq!(parsed.output_tokens, 5400);
        assert_eq!(parsed.reasoning_output_tokens, 1450);

        // Malformed inputs reject rather than silently zero out.
        assert!(CodexTokenUsageDetail::from_state_string("1,2,3").is_none());
        assert!(CodexTokenUsageDetail::from_state_string("1,2,3,4,5").is_none());
        assert!(CodexTokenUsageDetail::from_state_string("a,b,c,d").is_none());
    }

    #[test]
    fn test_persist_and_read_baseline_round_trip() {
        use crate::provenance::record::{AgentType, DetectionMethod};
        use crate::session::SessionMode;

        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("sessions")).unwrap();
        let prev = std::env::var(crate::global::AIKI_HOME_ENV).ok();
        std::env::set_var(crate::global::AIKI_HOME_ENV, home.path());

        let session = AikiSession::new(
            AgentType::Codex,
            "codex-baseline-roundtrip",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        session.file().create().unwrap();

        // Nothing persisted yet.
        assert!(read_persisted_baseline(&session).is_none());

        let total = CodexTokenUsageDetail {
            input_tokens: 81000,
            output_tokens: 5400,
            cached_input_tokens: 63000,
            reasoning_output_tokens: 1450,
        };
        persist_baseline(&session, &total);

        let got = read_persisted_baseline(&session).expect("baseline persisted to session file");
        assert_eq!(got.input_tokens, 81000);
        assert_eq!(got.cached_input_tokens, 63000);
        assert_eq!(got.output_tokens, 5400);
        assert_eq!(got.reasoning_output_tokens, 1450);

        match prev {
            Some(v) => std::env::set_var(crate::global::AIKI_HOME_ENV, v),
            None => std::env::remove_var(crate::global::AIKI_HOME_ENV),
        }
    }
}
