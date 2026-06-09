use serde::Deserialize;
use std::path::PathBuf;

use super::session::create_session;
use crate::error::Result;
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

/// Build AikiEvent from Codex event read from stdin
pub struct BuiltCodexEvents {
    pub supplemental_events: Vec<AikiEvent>,
    pub primary_event: AikiEvent,
}

pub fn build_aiki_event_from_stdin() -> Result<BuiltCodexEvents> {
    let event: CodexEvent = super::super::read_stdin_json()?;
    build_aiki_event_from_parsed(event)
}

#[allow(dead_code)]
pub(crate) fn build_aiki_event_from_json_str(json: &str) -> Result<BuiltCodexEvents> {
    let event: CodexEvent = serde_json::from_str(json).map_err(anyhow::Error::from)?;
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
    let transcript = payload
        .transcript_path
        .as_deref()
        .map(|p| TurnTranscript::parse(p, parse_transcript_lines))
        .unwrap_or_default();

    AikiEvent::TurnCompleted(AikiTurnCompletedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
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
#[derive(Deserialize, Debug, Clone)]
struct CodexTokenUsageDetail {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
}

/// The `info` object inside a `token_count` payload.
#[derive(Deserialize, Debug, Clone)]
struct CodexTokenCountInfo {
    #[allow(dead_code)]
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

/// Parse Codex JSONL content into transcript entries for the current turn.
///
/// Codex emits `token_count` events with cumulative `total_token_usage`.
/// To get per-turn usage, we find the last `turn_context` boundary and
/// compute: (last total in file) - (last total before that boundary).
/// For turn 1, the baseline is zero.
fn parse_transcript_lines(content: &str) -> Vec<TranscriptEntry> {
    // Track the cumulative total at each token_count event, and where turn
    // boundaries fall, so we can compute the delta for the current turn.
    let mut all_totals: Vec<CodexTokenUsageDetail> = Vec::new();
    let mut last_turn_boundary_idx: usize = 0; // index into all_totals

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.contains("\"turn_context\"") {
            // Mark boundary: baseline for next turn is whatever total we've seen so far
            last_turn_boundary_idx = all_totals.len();
            continue;
        }
        if !line.contains("\"token_count\"") {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            let is_token_count = val
                .get("payload")
                .and_then(|p| p.get("type"))
                .and_then(|t| t.as_str())
                == Some("token_count");
            if is_token_count {
                if let Ok(msg) = serde_json::from_value::<CodexEventMsg>(val) {
                    if let Some(info) = msg.payload.info {
                        all_totals.push(info.total_token_usage);
                    }
                }
            }
        }
    }

    let last = match all_totals.last() {
        Some(t) => t,
        None => return vec![],
    };

    // Baseline: last total before the current turn boundary (zero for turn 1)
    let baseline = if last_turn_boundary_idx > 0 {
        &all_totals[last_turn_boundary_idx - 1]
    } else {
        &CodexTokenUsageDetail {
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
        }
    };

    vec![TranscriptEntry {
        response: None,
        model: None,
        tokens: Some(TokenUsage {
            input: last.input_tokens.saturating_sub(baseline.input_tokens),
            output: last.output_tokens.saturating_sub(baseline.output_tokens),
            cache_read: last.cached_input_tokens.saturating_sub(baseline.cached_input_tokens),
            cache_created: 0,
        }),
    }]
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
        assert_eq!(usage.input, 1000);
        assert_eq!(usage.output, 500);
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
        // Last total_token_usage, baseline is zero (no turn_context)
        assert_eq!(usage.input, 47759);
        assert_eq!(usage.output, 249);
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
        // Delta: last total (5100/230/4200) - baseline before turn-2 (1000/50/500)
        assert_eq!(usage.input, 4100);
        assert_eq!(usage.output, 180);
        assert_eq!(usage.cache_read, 3700);
    }

    #[test]
    fn test_parse_token_usage_skips_null_info() {
        // First token_count event has info: null, second has data
        let content = r#"{"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":null}}
{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":5000,"output_tokens":300,"cached_input_tokens":1000,"reasoning_output_tokens":20,"total_tokens":5300},"total_token_usage":{"input_tokens":5000,"output_tokens":300,"cached_input_tokens":1000,"reasoning_output_tokens":20,"total_tokens":5300},"model_context_window":258400},"rate_limits":null}}
"#;
        let extract = parse_and_aggregate(content);
        let usage = extract.tokens.unwrap();
        assert_eq!(usage.input, 5000);
        assert_eq!(usage.output, 300);
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

}
