mod events;
pub mod otel;
mod output;
pub mod session;

use crate::cache::debug_log;
use crate::error::Result;
use crate::events::result::HookResult;
use crate::editors::HookCommandOutput;

use events::build_aiki_event_from_stdin;
use output::build_command_output;

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
    // Normalize camelCase → PascalCase for output module compatibility
    let normalized_event = match codex_event_name {
        "sessionStart" => "SessionStart",
        "userPromptSubmit" => "UserPromptSubmit",
        "preToolUse" => "PreToolUse",
        "stop" => "Stop",
        other => other,
    };

    // Build Aiki event from stdin JSON
    let built_events = build_aiki_event_from_stdin()?;

    for event in built_events.supplemental_events {
        if let Err(err) = crate::event_bus::dispatch(event) {
            debug_log(|| format!("Codex supplemental event dispatch failed: {}", err));
        }
    }

    // Dispatch the primary event
    let aiki_response = crate::event_bus::dispatch(built_events.primary_event)?;

    let hook_output = build_command_output(aiki_response, normalized_event);

    hook_output.print_and_exit();
}

#[allow(dead_code)]
pub fn parse_hook_payload_json(json: &str) -> Result<()> {
    let _ = events::build_aiki_event_from_json_str(json)?;
    Ok(())
}

#[allow(dead_code)]
pub fn render_hook_output(event_name: &str, response: HookResult) -> HookCommandOutput {
    build_command_output(response, event_name)
}
