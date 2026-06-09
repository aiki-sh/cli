use aiki::editors::codex;
use aiki::events::result::{Decision, Failure, HookResult};
use jsonschema::JSONSchema;
use serde_json::{json, Value};

fn validate(schema: Value, instance: Value) {
    let compiled = JSONSchema::compile(&schema).expect("schema should compile");
    let validation = compiled.validate(&instance);
    if let Err(err) = validation {
        let details = err.map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
        panic!("instance did not match schema:\n{details}\ninstance={instance}");
    }
}

fn hook_event_name_schema() -> Value {
    json!({
        "enum": ["PreToolUse", "SessionStart", "UserPromptSubmit", "Stop"],
        "type": "string"
    })
}

fn permission_mode_schema() -> Value {
    json!({
        "enum": ["default", "acceptEdits", "plan", "dontAsk", "bypassPermissions"],
        "type": "string"
    })
}

fn session_start_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["hook_event_name", "session_id", "cwd", "source", "model", "permission_mode", "transcript_path"],
        "properties": {
            "hook_event_name": { "const": "SessionStart" },
            "session_id": { "type": "string" },
            "cwd": { "type": "string" },
            "source": { "enum": ["startup", "resume", "clear"], "type": "string" },
            "model": { "type": "string" },
            "permission_mode": permission_mode_schema(),
            "transcript_path": { "type": ["string", "null"] }
        }
    })
}

fn user_prompt_submit_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["hook_event_name", "session_id", "cwd", "prompt", "turn_id", "model", "permission_mode", "transcript_path"],
        "properties": {
            "hook_event_name": { "const": "UserPromptSubmit" },
            "session_id": { "type": "string" },
            "cwd": { "type": "string" },
            "prompt": { "type": "string" },
            "turn_id": { "type": "string" },
            "model": { "type": "string" },
            "permission_mode": permission_mode_schema(),
            "transcript_path": { "type": ["string", "null"] }
        }
    })
}

fn pre_tool_use_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["hook_event_name", "session_id", "cwd", "tool_name", "tool_input", "tool_use_id", "turn_id", "model", "permission_mode", "transcript_path"],
        "properties": {
            "hook_event_name": { "const": "PreToolUse" },
            "session_id": { "type": "string" },
            "cwd": { "type": "string" },
            "tool_name": { "type": "string" },
            "tool_input": {
                "type": "object",
                "additionalProperties": false,
                "required": ["command"],
                "properties": {
                    "command": { "type": "string" }
                }
            },
            "tool_use_id": { "type": "string" },
            "turn_id": { "type": "string" },
            "model": { "type": "string" },
            "permission_mode": permission_mode_schema(),
            "transcript_path": { "type": ["string", "null"] }
        }
    })
}

fn stop_input_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["hook_event_name", "session_id", "cwd", "last_assistant_message", "stop_hook_active", "turn_id", "model", "permission_mode", "transcript_path"],
        "properties": {
            "hook_event_name": { "const": "Stop" },
            "session_id": { "type": "string" },
            "cwd": { "type": "string" },
            "last_assistant_message": { "type": ["string", "null"] },
            "stop_hook_active": { "type": "boolean" },
            "turn_id": { "type": "string" },
            "model": { "type": "string" },
            "permission_mode": permission_mode_schema(),
            "transcript_path": { "type": ["string", "null"] }
        }
    })
}

fn session_start_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "hookSpecificOutput": {
                "type": "object",
                "additionalProperties": false,
                "required": ["hookEventName", "additionalContext"],
                "properties": {
                    "hookEventName": { "const": "SessionStart" },
                    "additionalContext": { "type": "string" }
                }
            }
        }
    })
}

fn user_prompt_submit_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "decision": { "enum": ["block"], "type": "string" },
            "reason": { "type": "string" },
            "hookSpecificOutput": {
                "type": "object",
                "additionalProperties": false,
                "required": ["hookEventName", "additionalContext"],
                "properties": {
                    "hookEventName": { "const": "UserPromptSubmit" },
                    "additionalContext": { "type": "string" }
                }
            }
        }
    })
}

fn pre_tool_use_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "hookSpecificOutput": {
                "type": "object",
                "additionalProperties": false,
                "required": ["hookEventName", "permissionDecision"],
                "properties": {
                    "hookEventName": { "const": "PreToolUse" },
                    "permissionDecision": { "enum": ["deny"], "type": "string" },
                    "permissionDecisionReason": { "type": "string" }
                }
            }
        }
    })
}

fn stop_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "decision": { "enum": ["block"], "type": "string" },
            "reason": { "type": "string" }
        }
    })
}

fn output_json(event_name: &str, response: HookResult) -> Value {
    codex::render_hook_output(event_name, response)
        .json_value
        .expect("codex output should be json")
}

#[test]
fn test_codex_hook_input_samples_match_schema_and_parse() {
    let session_start = json!({
        "hook_event_name": "SessionStart",
        "session_id": "abc",
        "cwd": "/tmp/test",
        "source": "resume",
        "model": "o3",
        "permission_mode": "default",
        "transcript_path": null
    });
    validate(session_start_input_schema(), session_start.clone());
    codex::parse_hook_payload_json(&session_start.to_string()).unwrap();

    let user_prompt_submit = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "abc",
        "cwd": "/tmp/test",
        "prompt": "Fix the bug",
        "turn_id": "turn-1",
        "model": "o3",
        "permission_mode": "default",
        "transcript_path": null
    });
    validate(user_prompt_submit_input_schema(), user_prompt_submit.clone());
    codex::parse_hook_payload_json(&user_prompt_submit.to_string()).unwrap();

    let pre_tool_use = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "abc",
        "cwd": "/tmp/test",
        "tool_name": "Bash",
        "tool_input": { "command": "cargo test" },
        "tool_use_id": "tool-xyz",
        "turn_id": "turn-1",
        "model": "o3",
        "permission_mode": "default",
        "transcript_path": null
    });
    validate(pre_tool_use_input_schema(), pre_tool_use.clone());
    codex::parse_hook_payload_json(&pre_tool_use.to_string()).unwrap();

    let stop = json!({
        "hook_event_name": "Stop",
        "session_id": "abc",
        "cwd": "/tmp/test",
        "last_assistant_message": "Done fixing",
        "stop_hook_active": true,
        "turn_id": "turn-1",
        "model": "o3",
        "permission_mode": "default",
        "transcript_path": null
    });
    validate(stop_input_schema(), stop.clone());
    codex::parse_hook_payload_json(&stop.to_string()).unwrap();
}

#[test]
fn test_codex_hook_output_samples_match_schema() {
    let session_start = output_json(
        "SessionStart",
        HookResult {
            context: Some("workspace: /tmp/test".to_string()),
            decision: Decision::Allow,
            failures: vec![],
        },
    );
    validate(session_start_output_schema(), session_start);

    let user_prompt_submit = output_json(
        "UserPromptSubmit",
        HookResult {
            context: Some("task: xyz".to_string()),
            decision: Decision::Allow,
            failures: vec![],
        },
    );
    validate(user_prompt_submit_output_schema(), user_prompt_submit);

    let pre_tool_use = output_json(
        "PreToolUse",
        HookResult {
            context: None,
            decision: Decision::Block,
            failures: vec![Failure("Not allowed".to_string())],
        },
    );
    validate(pre_tool_use_output_schema(), pre_tool_use);

    let pre_tool_use_allow = output_json(
        "PreToolUse",
        HookResult {
            context: None,
            decision: Decision::Allow,
            failures: vec![],
        },
    );
    validate(json!({ "type": "object", "additionalProperties": false, "maxProperties": 0 }), pre_tool_use_allow);

    let stop = output_json(
        "Stop",
        HookResult {
            context: Some("Continue working".to_string()),
            decision: Decision::Allow,
            failures: vec![],
        },
    );
    validate(stop_output_schema(), stop);

    let stop_allow = output_json(
        "Stop",
        HookResult {
            context: None,
            decision: Decision::Allow,
            failures: vec![],
        },
    );
    validate(json!({ "type": "object", "additionalProperties": false, "maxProperties": 0 }), stop_allow);
}
