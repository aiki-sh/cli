mod events;
pub mod otel;
mod output;
pub mod session;

use crate::cache::debug_log;
use crate::error::Result;
use crate::events::result::HookResult;
use crate::editors::HookCommandOutput;

use events::{build_aiki_event_from_json, build_aiki_event_from_stdin, BuiltCodexEvents};
use output::build_command_output;

pub use output::build_not_active_output as not_active_output;

/// Normalize Codex's camelCase event names to the PascalCase the output module
/// expects.
fn normalize_event(codex_event_name: &str) -> &str {
    match codex_event_name {
        "sessionStart" => "SessionStart",
        "userPromptSubmit" => "UserPromptSubmit",
        "preToolUse" => "PreToolUse",
        "stop" => "Stop",
        other => other,
    }
}

/// Handle a Codex native hook event (stdin-based)
///
/// Entry point for `aiki hooks stdin --agent codex --event <event_name>`.
/// Reads structured JSON from stdin, builds an AikiEvent, dispatches it,
/// and formats the response for Codex's hook protocol.
///
/// For `source: "clear"` on SessionStart, Codex only fires SessionStart
/// (no preceding SessionEnd), so re-injection is handled directly by the
/// SessionCleared event handler.
pub fn handle_stdin(codex_event_name: &str) -> Result<()> {
    let built_events = build_aiki_event_from_stdin()?;
    dispatch_and_output(codex_event_name, built_events)
}

/// Handle a Codex event from a pre-read payload buffer (the stdin-once path).
pub fn handle_with_payload(codex_event_name: &str, payload: &[u8]) -> Result<()> {
    let built_events = build_aiki_event_from_json(payload)?;
    dispatch_and_output(codex_event_name, built_events)
}

/// Shared dispatch + output for both stdin and pre-read-payload paths.
fn dispatch_and_output(codex_event_name: &str, built_events: BuiltCodexEvents) -> Result<()> {
    let normalized_event = normalize_event(codex_event_name);

    for event in built_events.supplemental_events {
        if let Err(err) = crate::event_bus::dispatch(event) {
            debug_log(|| format!("Codex supplemental event dispatch failed: {}", err));
        }
    }

    let aiki_response = crate::event_bus::dispatch(built_events.primary_event)?;
    let hook_output = build_command_output(aiki_response, normalized_event);
    hook_output.print_and_exit();
}

/// Parse a Codex hook payload JSON string, returning an error if it does not
/// deserialize into a known `CodexEvent`. Used by the schema golden tests to
/// assert that documented sample payloads are accepted by the real parser.
#[allow(dead_code)]
pub fn parse_hook_payload_json(json: &str) -> Result<()> {
    let _ = build_aiki_event_from_json(json.as_bytes())?;
    Ok(())
}

#[allow(dead_code)]
pub fn render_hook_output(event_name: &str, response: HookResult) -> HookCommandOutput {
    build_command_output(response, event_name)
}
