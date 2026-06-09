use serde_json::json;

use crate::editors::HookCommandOutput;
use crate::events::result::HookResult;

/// Build HookCommandOutput from HookResult for Codex
///
/// Codex has a simpler hook protocol than Claude Code:
/// - SessionStart: JSON `hookSpecificOutput.additionalContext`
/// - UserPromptSubmit: optional block decision + optional additionalContext
/// - PreToolUse: only deny is emitted; allow is a no-op
/// - Stop: uses `decision: "block"` to continue the session
/// - No PostToolUse or SessionEnd events
pub fn build_command_output(response: HookResult, event_type: &str) -> HookCommandOutput {
    match event_type {
        "SessionStart" => build_session_start_output(&response),
        "UserPromptSubmit" => build_user_prompt_submit_output(&response),
        "PreToolUse" => build_pre_tool_use_output(&response),
        "Stop" => build_stop_output(&response),
        _ => {
            eprintln!("Warning: Unknown Codex event type: {}", event_type);
            HookCommandOutput::new(None, 0)
        }
    }
}

/// Build SessionStart command output for Codex
///
/// Emit the published schema shape with `hookSpecificOutput.additionalContext`.
fn build_session_start_output(response: &HookResult) -> HookCommandOutput {
    let combined = response.combined_output();

    let json_value = if let Some(ctx) = combined {
        json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": ctx
            }
        })
    } else {
        json!({})
    };

    HookCommandOutput::new(Some(json_value), 0)
}

/// Build UserPromptSubmit command output for Codex
///
/// - Block: JSON `{ "decision": "block", "reason": "..." }`
/// - Allow with context: JSON `hookSpecificOutput.additionalContext`
/// - Allow without context: empty JSON object
fn build_user_prompt_submit_output(response: &HookResult) -> HookCommandOutput {
    if response.decision.is_block() {
        let reason = response.format_messages();
        let mut json_value = json!({
            "decision": "block",
            "reason": reason
        });

        if let Some(ref ctx) = response.context {
            json_value["hookSpecificOutput"] = json!({
                "hookEventName": "UserPromptSubmit",
                "additionalContext": ctx
            });
        }

        HookCommandOutput::new(Some(json_value), 0)
    } else {
        let combined = response.combined_output();
        let json_value = if let Some(ctx) = combined {
            json!({
                "hookSpecificOutput": {
                    "hookEventName": "UserPromptSubmit",
                    "additionalContext": ctx
                }
            })
        } else {
            json!({})
        };

        HookCommandOutput::new(Some(json_value), 0)
    }
}

/// Build PreToolUse command output for Codex
///
/// KEY DIFFERENCE from Claude Code: allow is an empty JSON object.
/// Only deny emits permissionDecision state. Never emit "approve".
fn build_pre_tool_use_output(response: &HookResult) -> HookCommandOutput {
    if response.decision.is_block() {
        let reason = response.format_messages();
        let mut json_value = json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny"
            }
        });

        if !reason.is_empty() {
            json_value["hookSpecificOutput"]["permissionDecisionReason"] = json!(reason);
        }

        HookCommandOutput::new(Some(json_value), 0)
    } else {
        HookCommandOutput::new(Some(json!({})), 0)
    }
}

/// Build Stop command output for Codex
///
/// - Continue/block: `{ "decision": "block", "reason": "..." }` with the reason
///   serving as a continuation prompt
/// - End session: `{ "continue": false }` when autoreply_to_end_session action fired
/// - Allow stop: empty JSON object (no opinion — codex decides)
fn build_stop_output(response: &HookResult) -> HookCommandOutput {
    if response.context.is_some() {
        // Block the stop to continue the session (autoreply)
        let reason = response.context.as_deref().unwrap_or("Continue session");
        let json_value = json!({
            "decision": "block",
            "reason": reason
        });
        HookCommandOutput::new(Some(json_value), 0)
    } else if response.decision.is_block() {
        // autoreply_to_end_session action fired — explicitly tell codex to stop
        HookCommandOutput::new(
            Some(json!({ "continue": false, "reason": "All tasks done. Exit immediately." })),
            0,
        )
    } else {
        HookCommandOutput::new(Some(json!({})), 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::result::{Decision, Failure};

    fn make_allow_response() -> HookResult {
        HookResult {
            context: None,
            decision: Decision::Allow,
            failures: Vec::new(),
        }
    }

    fn make_allow_with_context(ctx: &str) -> HookResult {
        HookResult {
            context: Some(ctx.to_string()),
            decision: Decision::Allow,
            failures: Vec::new(),
        }
    }

    fn make_block_response(reason: &str) -> HookResult {
        HookResult {
            context: None,
            decision: Decision::Block,
            failures: vec![Failure(reason.to_string())],
        }
    }

    // SessionStart output tests

    #[test]
    fn test_session_start_with_context_emits_json_additional_context() {
        let response = make_allow_with_context("WORKSPACE ISOLATION: /tmp/test");
        let output = build_command_output(response, "SessionStart");
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout_text.is_none());
        let value = output.json_value.unwrap();
        assert_eq!(
            value["hookSpecificOutput"]["additionalContext"].as_str(),
            Some("WORKSPACE ISOLATION: /tmp/test")
        );
        assert_eq!(
            value["hookSpecificOutput"]["hookEventName"].as_str(),
            Some("SessionStart")
        );
    }

    #[test]
    fn test_session_start_without_context_emits_nothing() {
        let response = make_allow_response();
        let output = build_command_output(response, "SessionStart");
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.json_value.unwrap(), json!({}));
    }

    // PreToolUse output tests

    #[test]
    fn test_pre_tool_use_allow_is_noop() {
        let response = make_allow_response();
        let output = build_command_output(response, "PreToolUse");
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.json_value.unwrap(), json!({}));
        assert!(output.stdout_text.is_none());
    }

    #[test]
    fn test_pre_tool_use_deny_emits_json() {
        let response = make_block_response("Not allowed");
        let output = build_command_output(response, "PreToolUse");
        assert_eq!(output.exit_code, 0);
        let value = output.json_value.unwrap();
        let decision = value["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap();
        assert_eq!(decision, "deny");
        assert!(value["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("Not allowed"));
    }

    #[test]
    fn test_pre_tool_use_deny_never_emits_approve() {
        let response = make_block_response("Blocked");
        let output = build_command_output(response, "PreToolUse");
        let value = output.json_value.unwrap();
        let json_str = serde_json::to_string(&value).unwrap();
        assert!(
            !json_str.contains("approve"),
            "Must never emit 'approve' for Codex PreToolUse"
        );
    }

    // Stop output tests

    #[test]
    fn test_stop_allow_emits_nothing() {
        let response = make_allow_response();
        let output = build_command_output(response, "Stop");
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.json_value.unwrap(), json!({}));
        assert!(output.stdout_text.is_none());
    }

    #[test]
    fn test_stop_block_emits_decision_block() {
        let response = make_allow_with_context("Continue working on the task");
        let output = build_command_output(response, "Stop");
        assert_eq!(output.exit_code, 0);
        let value = output.json_value.unwrap();
        assert_eq!(value["decision"].as_str().unwrap(), "block");
        assert_eq!(
            value["reason"].as_str().unwrap(),
            "Continue working on the task"
        );
    }

    #[test]
    fn test_stop_autoreply_to_end_session_emits_continue_false() {
        // autoreply_to_end_session action sets Decision::Block without context
        let response = HookResult {
            context: None,
            decision: Decision::Block,
            failures: vec![],
        };
        let output = build_command_output(response, "Stop");
        assert_eq!(output.exit_code, 0);
        assert_eq!(
            output.json_value.unwrap(),
            json!({ "continue": false, "reason": "All tasks done. Exit immediately." })
        );
    }

    // UserPromptSubmit output tests

    #[test]
    fn test_user_prompt_submit_allow_no_context() {
        let response = make_allow_response();
        let output = build_command_output(response, "UserPromptSubmit");
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.json_value.unwrap(), json!({}));
    }

    #[test]
    fn test_user_prompt_submit_allow_with_context() {
        let response = make_allow_with_context("Task context: working on xyz");
        let output = build_command_output(response, "UserPromptSubmit");
        assert_eq!(output.exit_code, 0);
        let value = output.json_value.unwrap();
        assert_eq!(
            value["hookSpecificOutput"]["additionalContext"].as_str(),
            Some("Task context: working on xyz")
        );
        assert_eq!(
            value["hookSpecificOutput"]["hookEventName"].as_str(),
            Some("UserPromptSubmit")
        );
    }

    #[test]
    fn test_user_prompt_submit_block() {
        let response = make_block_response("Task is blocked");
        let output = build_command_output(response, "UserPromptSubmit");
        assert_eq!(output.exit_code, 0);
        let value = output.json_value.unwrap();
        assert_eq!(value["decision"].as_str().unwrap(), "block");
        assert!(value["reason"]
            .as_str()
            .unwrap()
            .contains("Task is blocked"));
    }

    // Unknown event test

    #[test]
    fn test_unknown_event_emits_nothing() {
        let response = make_allow_response();
        let output = build_command_output(response, "UnknownEvent");
        assert_eq!(output.exit_code, 0);
        assert!(output.json_value.is_none());
    }
}
