pub mod otel;

use crate::cache::debug_log;
use crate::editors::HookCommandOutput;
use crate::error::Result;
use crate::event_bus;
use crate::events::{AikiEvent, AikiTurnCompletedPayload, AikiTurnStartedPayload};
use crate::global;
use crate::history;
use crate::provenance::record::AgentType;
use crate::session::{AikiSession, AikiSessionFile};
use chrono::Utc;
use serde::Deserialize;
use std::path::PathBuf;

/// Codex notify payload for `agent-turn-complete` event
///
/// Codex appends the JSON payload as the final CLI argument to the notify command.
/// Fields: type, thread-id, turn-id, cwd, input-messages, last-assistant-message
#[derive(Debug, Deserialize)]
pub struct NotifyPayload {
    /// Event type (always "agent-turn-complete" for now)
    #[serde(rename = "type")]
    pub event_type: String,
    /// Thread/conversation ID (same as OTel `conversation.id`)
    #[serde(rename = "thread-id")]
    pub thread_id: String,
    /// Codex's internal turn ID (intentionally unused by aiki).
    ///
    /// Aiki generates its own deterministic turn_id as `{conversation_id}:{turn_number}`
    /// for cross-agent consistency. Codex's turn-id is opaque and not guaranteed stable
    /// across restarts. We capture it here only for debugging/logging purposes.
    #[serde(rename = "turn-id")]
    #[allow(dead_code)]
    pub turn_id: Option<String>,
    /// Working directory
    pub cwd: String,
    /// The agent's complete response for this turn
    #[serde(rename = "last-assistant-message")]
    pub last_assistant_message: Option<String>,
    /// Input messages (user prompts for this turn)
    #[serde(rename = "input-messages")]
    #[allow(dead_code)]
    pub input_messages: Option<serde_json::Value>,
}

fn extract_prompt_from_input_messages(
    input_messages: Option<&serde_json::Value>,
) -> Option<String> {
    let messages = input_messages?.as_array()?;

    for message in messages.iter().rev() {
        let role = message.get("role").and_then(|v| v.as_str());
        if role != Some("user") {
            continue;
        }

        let content = message.get("content")?;
        if let Some(text) = content.as_str() {
            return Some(text.to_string());
        }

        if let Some(text) = content.get("text").and_then(|v| v.as_str()) {
            return Some(text.to_string());
        }

        if let Some(text) = content.get("content").and_then(|v| v.as_str()) {
            return Some(text.to_string());
        }

        if let Some(items) = content.as_array() {
            let mut chunks = Vec::new();
            for item in items {
                if let Some(text) = item.as_str() {
                    chunks.push(text);
                    continue;
                }
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    chunks.push(text);
                    continue;
                }
                if let Some(text) = item.get("content").and_then(|v| v.as_str()) {
                    chunks.push(text);
                }
            }
            if !chunks.is_empty() {
                return Some(chunks.join("\n\n"));
            }
        }
    }

    None
}

/// Handle a Codex hook event
///
/// Entry point for `aiki hooks stdin --agent codex --event <event_name>`
///
/// For Codex, the only event dispatched via notify is `agent-turn-complete`.
/// The JSON payload is passed as a CLI argument (not stdin).
/// Stale session cleanup is handled by prune_dead_pid_sessions in session/mod.rs.
pub fn handle(event_name: &str, payload_json: Option<&str>) -> Result<()> {
    debug_log(|| format!("Codex hook event: {}", event_name));

    match event_name {
        "agent-turn-complete" => handle_turn_complete(payload_json),
        other => {
            debug_log(|| format!("Unknown Codex event: {}", other));
            Ok(())
        }
    }
}

/// Handle the `agent-turn-complete` notify event
///
/// Uses JJ history for turn tracking (same as stdin integrations).
/// Modified files come from JJ file tracking.
fn handle_turn_complete(payload_json: Option<&str>) -> Result<()> {
    let json = match payload_json {
        Some(j) => j,
        None => {
            debug_log(|| "No payload JSON for agent-turn-complete".to_string());
            return Ok(());
        }
    };

    let payload: NotifyPayload = match serde_json::from_str(json) {
        Ok(p) => p,
        Err(e) => {
            debug_log(|| format!("Failed to parse Codex notify payload: {}", e));
            return Ok(());
        }
    };

    debug_log(|| {
        format!(
            "Codex turn complete: thread={}, cwd={}",
            payload.thread_id, payload.cwd
        )
    });

    // Build AikiSession for event dispatch
    let session = AikiSession::for_hook(AgentType::Codex, &payload.thread_id, None::<&str>);
    let session_file = AikiSessionFile::new(&session);

    // Check if session file exists (OTel should have created it)
    if !session_file.exists() {
        debug_log(|| {
            format!(
                "No session file for thread {}. Notify without OTel is a no-op.",
                payload.thread_id
            )
        });
        return Ok(());
    }

    let cwd = PathBuf::from(&payload.cwd);
    let now = Utc::now();

    // Get current turn from JJ history (same as stdin integrations)
    let jj_cwd = global::global_aiki_dir();
    let current_turn = match history::get_current_turn_number(&jj_cwd, session.uuid()) {
        Ok(t) => t,
        Err(e) => {
            debug_log(|| format!("Failed to query turn from JJ: {}", e));
            0
        }
    };

    // If no turn recorded yet, emit turn.started first
    if current_turn == 0 {
        let prompt = extract_prompt_from_input_messages(payload.input_messages.as_ref())
            .unwrap_or_default();
        let turn_started = AikiEvent::TurnStarted(AikiTurnStartedPayload {
            session: session.clone(),
            cwd: cwd.clone(),
            timestamp: now,
            turn: crate::events::Turn::unknown(), // Set by handle_turn_started
            prompt,
            injected_refs: vec![],
        });
        if let Err(e) = event_bus::dispatch(turn_started) {
            debug_log(|| format!("Failed to dispatch turn.started for Codex: {}", e));
        }
    }

    // Re-query turn after potential turn.started
    let current_turn = match history::get_current_turn_number(&jj_cwd, session.uuid()) {
        Ok(t) => t.max(1), // At least 1 for turn.completed
        Err(_) => 1,
    };

    // Emit turn.completed
    // Modified files come from JJ file tracking (empty here, will be populated by JJ)
    let turn_completed = AikiEvent::TurnCompleted(AikiTurnCompletedPayload {
        session,
        cwd,
        timestamp: now,
        turn: crate::events::Turn::new(
            current_turn,
            format!("{}:{}", payload.thread_id, current_turn),
            "user".to_string(),
        ),
        response: payload.last_assistant_message.unwrap_or_default(),
        modified_files: vec![], // Files come from JJ
        tasks: Default::default(), // Populated by handle_turn_completed
    });
    if let Err(e) = event_bus::dispatch(turn_completed) {
        debug_log(|| format!("Failed to dispatch turn.completed for Codex: {}", e));
    }

    Ok(())
}

/// Format a Codex hook result for output
///
/// Codex notify is fire-and-forget, so we don't output anything.
/// This is a no-op that returns a success exit code.
#[must_use]
pub fn format_output() -> HookCommandOutput {
    HookCommandOutput::new(None, 0)
}

#[cfg(test)]
mod tests {
    use super::extract_prompt_from_input_messages;
    use serde_json::json;

    #[test]
    fn test_extract_prompt_from_input_messages_string() {
        let input = json!([
            {"role": "system", "content": "sys"},
            {"role": "user", "content": "Fix the login bug"},
            {"role": "assistant", "content": "Sure"},
            {"role": "user", "content": "Add tests too"}
        ]);

        assert_eq!(
            extract_prompt_from_input_messages(Some(&input)),
            Some("Add tests too".to_string())
        );
    }

    #[test]
    fn test_extract_prompt_from_input_messages_array() {
        let input = json!([
            {"role": "user", "content": [
                {"type": "text", "text": "Part 1"},
                {"type": "text", "text": "Part 2"}
            ]}
        ]);

        assert_eq!(
            extract_prompt_from_input_messages(Some(&input)),
            Some("Part 1\n\nPart 2".to_string())
        );
    }
}
