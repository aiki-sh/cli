/// Integration tests for task lifecycle events
///
/// Tests cover:
/// 1. Event payload serialization (AikiTaskStartedPayload, AikiTaskClosedPayload)
/// 2. AikiEvent conversions for task events
/// 3. AikiState creation from task events
/// 4. Hook statement execution with task events
/// 5. EventType routing to correct hook fields
/// 6. Hook YAML parsing with task events
/// 7. Edge cases for task payloads
///
/// Note: Sugar pattern expansion tests (e.g., review.completed -> task.closed)
/// are covered in the parser module's internal tests (cli/src/flows/parser.rs).

use aiki::events::{
    AikiEvent, AikiTaskClosedPayload, AikiTaskStartedPayload, TaskEventPayload,
};
use aiki::flows::composer::EventType;
use aiki::flows::types::{Action, Hook, HookStatement, IfStatement, LogAction};
use aiki::flows::{AikiState, HookEngine, HookOutcome};
use chrono::Utc;
use std::path::PathBuf;

// ============================================================================
// 1. Event Payload Serialization Tests
// ============================================================================

#[test]
fn test_task_started_payload_serialization() {
    let payload = AikiTaskStartedPayload {
        task: TaskEventPayload {
            id: "abc123".to_string(),
            name: "Test Task".to_string(),
            task_type: "feature".to_string(),
            status: "in_progress".to_string(),
            assignee: Some("claude-code".to_string()),
            outcome: None,
            source: Some("file:ops/plan.md".to_string()),
            files: None,
            changes: None,
        },
        cwd: PathBuf::from("/tmp/test"),
        timestamp: Utc::now(),
    };

    let json = serde_json::to_string(&payload).unwrap();
    assert!(json.contains("\"id\":\"abc123\""));
    assert!(json.contains("\"name\":\"Test Task\""));
    // task_type serializes as "type"
    assert!(json.contains("\"type\":\"feature\""));
    assert!(json.contains("\"status\":\"in_progress\""));
    assert!(json.contains("\"assignee\":\"claude-code\""));
    assert!(json.contains("\"source\":\"file:ops/plan.md\""));
    // outcome should not appear for started events (it's None)
    assert!(!json.contains("\"outcome\""));
    // files should not appear (it's None)
    assert!(!json.contains("\"files\""));
}

#[test]
fn test_task_closed_payload_serialization() {
    let payload = AikiTaskClosedPayload {
        task: TaskEventPayload {
            id: "abc123".to_string(),
            name: "Test Task".to_string(),
            task_type: "review".to_string(),
            status: "closed".to_string(),
            assignee: None,
            outcome: Some("done".to_string()),
            source: Some("task:xyz789".to_string()),
            files: Some(vec!["src/main.rs".to_string()]),
            changes: Some(vec!["abc123".to_string()]),
        },
        cwd: PathBuf::from("/tmp/test"),
        timestamp: Utc::now(),
    };

    let json = serde_json::to_string(&payload).unwrap();
    assert!(json.contains("\"id\":\"abc123\""));
    assert!(json.contains("\"type\":\"review\""));
    assert!(json.contains("\"status\":\"closed\""));
    assert!(json.contains("\"outcome\":\"done\""));
    assert!(json.contains("\"source\":\"task:xyz789\""));
    assert!(json.contains("\"files\":[\"src/main.rs\"]"));
    assert!(json.contains("\"changes\":[\"abc123\"]"));
    // assignee should not appear (it's None)
    assert!(!json.contains("\"assignee\""));
}

#[test]
fn test_task_started_payload_deserialization() {
    let json = r#"{
        "task": {
            "id": "test123",
            "name": "Implement feature",
            "type": "feature",
            "status": "in_progress",
            "assignee": "claude-code"
        },
        "cwd": "/tmp/project",
        "timestamp": "2024-01-15T10:30:00Z"
    }"#;

    let payload: AikiTaskStartedPayload = serde_json::from_str(json).unwrap();
    assert_eq!(payload.task.id, "test123");
    assert_eq!(payload.task.name, "Implement feature");
    assert_eq!(payload.task.task_type, "feature");
    assert_eq!(payload.task.status, "in_progress");
    assert_eq!(payload.task.assignee, Some("claude-code".to_string()));
    assert!(payload.task.outcome.is_none());
    assert!(payload.task.source.is_none());
    assert!(payload.task.files.is_none());
}

#[test]
fn test_task_closed_payload_deserialization() {
    let json = r#"{
        "task": {
            "id": "test456",
            "name": "Code review",
            "type": "review",
            "status": "closed",
            "outcome": "done",
            "files": ["src/lib.rs", "src/main.rs"]
        },
        "cwd": "/tmp/project",
        "timestamp": "2024-01-15T11:00:00Z"
    }"#;

    let payload: AikiTaskClosedPayload = serde_json::from_str(json).unwrap();
    assert_eq!(payload.task.id, "test456");
    assert_eq!(payload.task.name, "Code review");
    assert_eq!(payload.task.task_type, "review");
    assert_eq!(payload.task.status, "closed");
    assert_eq!(payload.task.outcome, Some("done".to_string()));
    assert_eq!(
        payload.task.files,
        Some(vec!["src/lib.rs".to_string(), "src/main.rs".to_string()])
    );
    assert!(payload.task.assignee.is_none());
}

#[test]
fn test_task_closed_payload_with_wont_do_outcome() {
    let payload = AikiTaskClosedPayload {
        task: TaskEventPayload {
            id: "skipped123".to_string(),
            name: "Skipped Task".to_string(),
            task_type: "bugfix".to_string(),
            status: "closed".to_string(),
            assignee: Some("claude-code".to_string()),
            outcome: Some("wont_do".to_string()),
            source: None,
            files: None,
            changes: None,
        },
        cwd: PathBuf::from("/tmp/test"),
        timestamp: Utc::now(),
    };

    let json = serde_json::to_string(&payload).unwrap();
    assert!(json.contains("\"outcome\":\"wont_do\""));
    assert!(json.contains("\"type\":\"bugfix\""));
}

// ============================================================================
// 2. AikiEvent Conversions
// ============================================================================

#[test]
fn test_task_started_to_aiki_event() {
    let payload = AikiTaskStartedPayload {
        task: TaskEventPayload {
            id: "event123".to_string(),
            name: "Event Test".to_string(),
            task_type: "feature".to_string(),
            status: "in_progress".to_string(),
            assignee: None,
            outcome: None,
            source: None,
            files: None,
            changes: None,
        },
        cwd: PathBuf::from("/tmp/test"),
        timestamp: Utc::now(),
    };

    let event: AikiEvent = payload.clone().into();
    match event {
        AikiEvent::TaskStarted(p) => {
            assert_eq!(p.task.id, "event123");
            assert_eq!(p.task.name, "Event Test");
        }
        _ => panic!("Expected TaskStarted event"),
    }
}

#[test]
fn test_task_closed_to_aiki_event() {
    let payload = AikiTaskClosedPayload {
        task: TaskEventPayload {
            id: "event456".to_string(),
            name: "Event Test".to_string(),
            task_type: "review".to_string(),
            status: "closed".to_string(),
            assignee: None,
            outcome: Some("done".to_string()),
            source: None,
            files: None,
            changes: None,
        },
        cwd: PathBuf::from("/tmp/test"),
        timestamp: Utc::now(),
    };

    let event: AikiEvent = payload.clone().into();
    match event {
        AikiEvent::TaskClosed(p) => {
            assert_eq!(p.task.id, "event456");
            assert_eq!(p.task.outcome, Some("done".to_string()));
        }
        _ => panic!("Expected TaskClosed event"),
    }
}

#[test]
fn test_aiki_event_cwd_for_task_events() {
    let started_payload = AikiTaskStartedPayload {
        task: TaskEventPayload {
            id: "cwd123".to_string(),
            name: "CWD Test".to_string(),
            task_type: "feature".to_string(),
            status: "in_progress".to_string(),
            assignee: None,
            outcome: None,
            source: None,
            files: None,
            changes: None,
        },
        cwd: PathBuf::from("/project/root"),
        timestamp: Utc::now(),
    };

    let event: AikiEvent = started_payload.into();
    assert_eq!(event.cwd(), std::path::Path::new("/project/root"));
}

// ============================================================================
// 3. AikiState Tests with Task Events
// ============================================================================

#[test]
fn test_aiki_state_from_task_started() {
    let payload = AikiTaskStartedPayload {
        task: TaskEventPayload {
            id: "state123".to_string(),
            name: "State Test".to_string(),
            task_type: "feature".to_string(),
            status: "in_progress".to_string(),
            assignee: None,
            outcome: None,
            source: None,
            files: None,
            changes: None,
        },
        cwd: PathBuf::from("/test/project"),
        timestamp: Utc::now(),
    };

    let state = AikiState::new(payload);
    assert_eq!(state.cwd(), std::path::Path::new("/test/project"));

    // Task events don't have context assembler (unlike turn events)
    assert!(state.build_context().is_none());
}

#[test]
fn test_aiki_state_from_task_closed() {
    let payload = AikiTaskClosedPayload {
        task: TaskEventPayload {
            id: "state456".to_string(),
            name: "State Test".to_string(),
            task_type: "review".to_string(),
            status: "closed".to_string(),
            assignee: None,
            outcome: Some("done".to_string()),
            source: None,
            files: None,
            changes: None,
        },
        cwd: PathBuf::from("/test/project"),
        timestamp: Utc::now(),
    };

    let state = AikiState::new(payload);
    assert_eq!(state.cwd(), std::path::Path::new("/test/project"));
}

// ============================================================================
// 4. Hook Statement Execution Tests
// ============================================================================

fn create_task_started_event() -> AikiEvent {
    AikiTaskStartedPayload {
        task: TaskEventPayload {
            id: "exec123".to_string(),
            name: "Execution Test".to_string(),
            task_type: "feature".to_string(),
            status: "in_progress".to_string(),
            assignee: Some("claude-code".to_string()),
            outcome: None,
            source: None,
            files: None,
            changes: None,
        },
        cwd: PathBuf::from("/tmp/test"),
        timestamp: Utc::now(),
    }
    .into()
}

fn create_task_closed_event() -> AikiEvent {
    AikiTaskClosedPayload {
        task: TaskEventPayload {
            id: "exec456".to_string(),
            name: "Execution Test".to_string(),
            task_type: "review".to_string(),
            status: "closed".to_string(),
            assignee: None,
            outcome: Some("done".to_string()),
            source: None,
            files: Some(vec!["src/test.rs".to_string()]),
            changes: Some(vec!["abc123".to_string()]),
        },
        cwd: PathBuf::from("/tmp/test"),
        timestamp: Utc::now(),
    }
    .into()
}

#[test]
fn test_simple_log_statement_with_task_started() {
    let statements = vec![HookStatement::Action(Action::Log(LogAction {
        log: "Task started!".to_string(),
        alias: None,
    }))];

    let mut state = AikiState::new(create_task_started_event());
    let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

    assert!(matches!(result, HookOutcome::Success));
}

#[test]
fn test_simple_log_statement_with_task_closed() {
    let statements = vec![HookStatement::Action(Action::Log(LogAction {
        log: "Task closed!".to_string(),
        alias: None,
    }))];

    let mut state = AikiState::new(create_task_closed_event());
    let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

    assert!(matches!(result, HookOutcome::Success));
}

#[test]
fn test_conditional_statement_with_task_event() {
    // This tests the if statement structure, though variable resolution
    // happens during actual execution with real variable data
    let statements = vec![HookStatement::If(IfStatement {
        condition: "1 == 1".to_string(),
        then: vec![HookStatement::Action(Action::Log(LogAction {
            log: "Condition was true".to_string(),
            alias: None,
        }))],
        else_: None,
    })];

    let mut state = AikiState::new(create_task_started_event());
    let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

    assert!(matches!(result, HookOutcome::Success));
}

// ============================================================================
// 5. EventType Routing Tests
// ============================================================================

#[test]
fn test_event_type_routing_task_started() {
    let yaml = r#"
name: Task Event Flow
version: "1"
task.started:
  - log: "Task started handler"
task.closed:
  - log: "Task closed handler"
session.started:
  - log: "Session started"
"#;

    let hook: Hook = serde_yaml::from_str(yaml).unwrap();

    // Verify EventType routes to correct handlers
    assert_eq!(EventType::TaskStarted.get_statements(&hook).len(), 1);
    assert_eq!(EventType::TaskClosed.get_statements(&hook).len(), 1);
    assert_eq!(EventType::SessionStarted.get_statements(&hook).len(), 1);

    // Verify unrelated events are empty
    assert!(EventType::TurnStarted.get_statements(&hook).is_empty());
    assert!(EventType::ChangeCompleted.get_statements(&hook).is_empty());
}

#[test]
fn test_event_type_routing_empty_handlers() {
    let yaml = r#"
name: Minimal Flow
version: "1"
"#;

    let hook: Hook = serde_yaml::from_str(yaml).unwrap();

    // All handlers should be empty
    assert!(EventType::TaskStarted.get_statements(&hook).is_empty());
    assert!(EventType::TaskClosed.get_statements(&hook).is_empty());
}

#[test]
fn test_event_type_routing_multiple_statements() {
    let yaml = r#"
name: Multi Statement Flow
version: "1"
task.started:
  - log: "First log"
  - log: "Second log"
  - log: "Third log"
"#;

    let hook: Hook = serde_yaml::from_str(yaml).unwrap();

    assert_eq!(EventType::TaskStarted.get_statements(&hook).len(), 3);
}

// ============================================================================
// 6. Hook YAML Parsing Tests
// ============================================================================
//
// Note: Sugar pattern expansion tests (e.g., review.completed -> task.closed)
// are covered in the parser module's internal tests (cli/src/flows/parser.rs).

#[test]
fn test_parse_hook_with_task_events() {
    let yaml = r#"
name: Task Handler Flow
version: "1"
description: Handles task lifecycle events

task.started:
  - log: "Task ${event.task.id} started: ${event.task.name}"

task.closed:
  - log: "Task ${event.task.id} closed with outcome: ${event.task.outcome}"
"#;

    let hook: Hook = serde_yaml::from_str(yaml).unwrap();

    assert_eq!(hook.name, "Task Handler Flow");
    assert_eq!(hook.description, Some("Handles task lifecycle events".to_string()));
    assert_eq!(hook.handlers.task_started.len(), 1);
    assert_eq!(hook.handlers.task_closed.len(), 1);
}

#[test]
fn test_parse_hook_with_task_and_session_events() {
    let yaml = r#"
name: Combined Flow
version: "1"

session.started:
  - log: "Session started"

task.started:
  - log: "Task started"

task.closed:
  - log: "Task closed"

session.ended:
  - log: "Session ended"
"#;

    let hook: Hook = serde_yaml::from_str(yaml).unwrap();

    assert_eq!(hook.handlers.session_started.len(), 1);
    assert_eq!(hook.handlers.task_started.len(), 1);
    assert_eq!(hook.handlers.task_closed.len(), 1);
    assert_eq!(hook.handlers.session_ended.len(), 1);
}

#[test]
fn test_parse_hook_with_before_after_composition() {
    let yaml = r#"
name: Composed Task Flow
version: "1"

before:
  include:
    - aiki/setup

after:
  include:
    - aiki/cleanup

task.started:
  - log: "Main task handler"
"#;

    let hook: Hook = serde_yaml::from_str(yaml).unwrap();

    assert_eq!(hook.before.len(), 1);
    assert_eq!(hook.before[0].include, vec!["aiki/setup"]);
    assert_eq!(hook.after.len(), 1);
    assert_eq!(hook.after[0].include, vec!["aiki/cleanup"]);
    assert_eq!(hook.handlers.task_started.len(), 1);
}

// ============================================================================
// 7. Edge Case Tests
// ============================================================================

#[test]
fn test_task_payload_minimal_fields() {
    // Test with only required fields
    let json = r#"{
        "task": {
            "id": "min123",
            "name": "Minimal",
            "type": "task",
            "status": "in_progress"
        },
        "cwd": "/tmp",
        "timestamp": "2024-01-15T12:00:00Z"
    }"#;

    let payload: AikiTaskStartedPayload = serde_json::from_str(json).unwrap();
    assert_eq!(payload.task.id, "min123");
    assert!(payload.task.assignee.is_none());
    assert!(payload.task.outcome.is_none());
    assert!(payload.task.source.is_none());
    assert!(payload.task.files.is_none());
}

#[test]
fn test_task_payload_all_optional_fields() {
    let payload = AikiTaskClosedPayload {
        task: TaskEventPayload {
            id: "full123".to_string(),
            name: "Full Task".to_string(),
            task_type: "review".to_string(),
            status: "closed".to_string(),
            assignee: Some("claude-code".to_string()),
            outcome: Some("done".to_string()),
            source: Some("file:plan.md".to_string()),
            files: Some(vec![
                "src/a.rs".to_string(),
                "src/b.rs".to_string(),
                "src/c.rs".to_string(),
            ]),
            changes: Some(vec!["abc123".to_string(), "def456".to_string()]),
        },
        cwd: PathBuf::from("/project"),
        timestamp: Utc::now(),
    };

    let json = serde_json::to_string(&payload).unwrap();

    // All fields should be present
    assert!(json.contains("\"assignee\":\"claude-code\""));
    assert!(json.contains("\"outcome\":\"done\""));
    assert!(json.contains("\"source\":\"file:plan.md\""));
    assert!(json.contains("\"files\":[\"src/a.rs\",\"src/b.rs\",\"src/c.rs\"]"));
    assert!(json.contains("\"changes\":[\"abc123\",\"def456\"]"));
}

#[test]
fn test_empty_files_array() {
    let payload = AikiTaskClosedPayload {
        task: TaskEventPayload {
            id: "empty123".to_string(),
            name: "Empty Files".to_string(),
            task_type: "task".to_string(),
            status: "closed".to_string(),
            assignee: None,
            outcome: Some("done".to_string()),
            source: None,
            files: Some(vec![]), // Empty but present
            changes: Some(vec![]), // Empty but present
        },
        cwd: PathBuf::from("/tmp"),
        timestamp: Utc::now(),
    };

    let json = serde_json::to_string(&payload).unwrap();
    assert!(json.contains("\"files\":[]"));
    assert!(json.contains("\"changes\":[]"));
}

#[test]
fn test_task_types_variety() {
    // Test various task types serialize correctly
    let task_types = vec!["feature", "bugfix", "review", "refactor", "docs", "test", "custom_type"];

    for task_type in task_types {
        let payload = AikiTaskStartedPayload {
            task: TaskEventPayload {
                id: "type123".to_string(),
                name: format!("{} task", task_type),
                task_type: task_type.to_string(),
                status: "in_progress".to_string(),
                assignee: None,
                outcome: None,
                source: None,
                files: None,
                changes: None,
            },
            cwd: PathBuf::from("/tmp"),
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(
            json.contains(&format!("\"type\":\"{}\"", task_type)),
            "Failed for task_type: {}",
            task_type
        );
    }
}

// ============================================================================
// 8. Integration with HookComposer
// ============================================================================

// Note: Full HookComposer integration tests would require tempdir setup
// which is covered in test_flow_statements.rs and composer module tests.
// These tests verify the data types integrate correctly.

#[test]
fn test_task_event_in_hook_composer_context() {
    // Verify that task events create proper state for composer
    let event = create_task_started_event();
    let mut state = AikiState::new(event);

    // Verify we can set variables (used by hook execution)
    state.set_variable("task_id".to_string(), "abc123".to_string());
    assert_eq!(
        state.get_variable("task_id"),
        Some(&"abc123".to_string())
    );

    // Verify we can clear variables (used by composer for isolation)
    state.clear_variables();
    assert!(state.get_variable("task_id").is_none());
}

// ============================================================================
// 9. Lazy Variable Resolution Tests
// ============================================================================

#[test]
fn test_task_closed_lazy_vars_in_log_statement() {
    // Test that {{event.task.files}} and {{event.task.changes}} can be used in hooks
    // (even though they'll be empty without a real JJ repo, the resolution should work)
    let statements = vec![HookStatement::Action(Action::Log(LogAction {
        log: "Files: {{event.task.files}}, Changes: {{event.task.changes}}".to_string(),
        alias: None,
    }))];

    let mut state = AikiState::new(create_task_closed_event());
    let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

    // Should succeed - lazy vars resolve (to empty strings without JJ)
    assert!(matches!(result, HookOutcome::Success));
}

#[test]
fn test_task_started_no_lazy_provenance_vars() {
    // task.started events should NOT have files/changes lazy vars
    // (provenance only makes sense at task close)
    // With {{var}} syntax, referencing an undefined variable produces an error
    let statements = vec![HookStatement::Action(Action::Log(LogAction {
        log: "Files: {{event.task.files}}".to_string(),
        alias: None,
    }))];

    let mut state = AikiState::new(create_task_started_event());
    let result = HookEngine::execute_statements(&statements, &mut state);

    // Should error because event.task.files is not defined on task.started events
    assert!(result.is_err());
}

#[test]
fn test_task_closed_lazy_vars_only_computed_when_accessed() {
    // Verify lazy vars don't cause issues when NOT accessed
    let statements = vec![HookStatement::Action(Action::Log(LogAction {
        log: "Task {{event.task.id}} closed".to_string(), // Doesn't use files/changes
        alias: None,
    }))];

    let mut state = AikiState::new(create_task_closed_event());
    let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

    // Should succeed quickly (no JJ queries for unused lazy vars)
    assert!(matches!(result, HookOutcome::Success));
}

#[test]
fn test_task_closed_conditional_with_lazy_vars() {
    // Test lazy vars in conditional context
    let statements = vec![HookStatement::If(IfStatement {
        condition: "event.task.outcome == \"done\"".to_string(),
        then: vec![HookStatement::Action(Action::Log(LogAction {
            log: "Done! Changed files: {{event.task.files}}".to_string(),
            alias: None,
        }))],
        else_: None,
    })];

    let mut state = AikiState::new(create_task_closed_event());
    let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

    // Should succeed - condition is true, lazy var is resolved in log
    assert!(matches!(result, HookOutcome::Success));
}
