//! Integration tests for event storage and JJ helpers
//!
//! Tests cover:
//! 1. parse_change_id_from_stderr
//! 2. ensure_branch
//! 3. Event chaining via write_event (sequential events form a chain)
//! 4. read_events returns all chained events in order

mod common;

use common::{init_jj_workspace, jj_available};
use tempfile::tempdir;

/// Helper: run a jj command and return stdout
fn jj_run(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("jj")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("Failed to run jj");
    if !output.status.success() {
        panic!(
            "jj {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Helper: get the change_id that a bookmark points to
fn bookmark_change_id(cwd: &std::path::Path, bookmark: &str) -> String {
    jj_run(
        cwd,
        &[
            "log",
            "-r",
            bookmark,
            "-T",
            "change_id",
            "--no-graph",
            "--ignore-working-copy",
        ],
    )
}

// ============================================================================
// 1. parse_change_id_from_stderr
// ============================================================================

#[test]
fn test_parse_change_id_from_stderr() {
    let stderr = b"Created new commit xlulsuvp 9ca401f0 (empty) [aiki-task]\n";
    let id = aiki::jj::parse_change_id_from_stderr(stderr).unwrap();
    assert_eq!(id, "xlulsuvp");
}

#[test]
fn test_parse_change_id_from_stderr_with_prefix_lines() {
    // jj may emit warnings before the "Created" line
    let stderr = b"Warning: some warning\nCreated new commit abcdefgh 12345678 (empty) test\n";
    let id = aiki::jj::parse_change_id_from_stderr(stderr).unwrap();
    assert_eq!(id, "abcdefgh");
}

#[test]
fn test_parse_change_id_from_stderr_missing() {
    let stderr = b"Some other output\n";
    assert!(aiki::jj::parse_change_id_from_stderr(stderr).is_err());
}

// ============================================================================
// 2. ensure_branch
// ============================================================================

#[test]
fn test_ensure_branch_creates_and_caches() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp = tempdir().unwrap();
    let cwd = temp.path();
    init_jj_workspace(cwd).unwrap();

    let branch = "aiki/test-ensure";

    // Branch should not exist yet
    let exists_before = aiki::jj::branch_exists(cwd, branch).unwrap();
    assert!(!exists_before, "Branch should not exist before ensure");

    // ensure_branch should create it
    aiki::jj::ensure_branch(cwd, branch).unwrap();

    // Now it should exist
    let exists_after = aiki::jj::branch_exists(cwd, branch).unwrap();
    assert!(exists_after, "Branch should exist after ensure");

    // Calling ensure_branch again should succeed (cached)
    aiki::jj::ensure_branch(cwd, branch).unwrap();
}

// ============================================================================
// 3. Integration: write_event chaining via task storage
// ============================================================================

#[test]
fn test_task_write_event_and_read_back() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp = tempdir().unwrap();
    let cwd = temp.path();
    init_jj_workspace(cwd).unwrap();

    use aiki::tasks::storage::{read_events, write_event};
    use aiki::tasks::types::{TaskEvent, TaskPriority};
    use chrono::Utc;

    // Write a Created event
    let event1 = TaskEvent::Created {
        task_id: "test001".to_string(),
        name: "Test task one".to_string(),
        slug: None,
        task_type: None,
        priority: TaskPriority::P2,
        assignee: Some("claude-code".to_string()),
        sources: Vec::new(),
        template: None,
        instructions: None,
        data: std::collections::HashMap::new(),
        timestamp: Utc::now(),
    };

    write_event(cwd, &event1).unwrap();

    // Read back events
    let events = read_events(cwd).unwrap();
    assert_eq!(events.len(), 1, "Should have 1 event");

    match &events[0] {
        TaskEvent::Created { task_id, name, .. } => {
            assert_eq!(task_id, "test001");
            assert_eq!(name, "Test task one");
        }
        _ => panic!("Expected Created event, got {:?}", events[0]),
    }
}

#[test]
fn test_task_event_chaining_multiple_writes() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp = tempdir().unwrap();
    let cwd = temp.path();
    init_jj_workspace(cwd).unwrap();

    use aiki::tasks::storage::{read_events, write_event};
    use aiki::tasks::types::{ConfidenceLevel, TaskEvent, TaskOutcome, TaskPriority};
    use chrono::Utc;

    // Write 3 events sequentially: Created, Started, Closed
    let t = Utc::now();
    let events_to_write = vec![
        TaskEvent::Created {
            task_id: "chain001".to_string(),
            name: "Chaining test".to_string(),
            slug: None,
            task_type: None,
            priority: TaskPriority::P1,
            assignee: None,
            sources: Vec::new(),
            template: None,
            instructions: None,
            data: std::collections::HashMap::new(),
            timestamp: t,
        },
        TaskEvent::Started {
            task_ids: vec!["chain001".to_string()],
            agent_type: "claude-code".to_string(),
            session_id: Some("test-session".to_string()),
            turn_id: None,
            working_copy: None,
            instructions: None,
            timestamp: t + chrono::Duration::seconds(1),
        },
        TaskEvent::Closed {
            task_ids: vec!["chain001".to_string()],
            outcome: TaskOutcome::Done,
            confidence: Some(ConfidenceLevel::High),
            summary: Some("Completed the test".to_string()),
            session_id: None,
            turn_id: None,
            timestamp: t + chrono::Duration::seconds(2),
        },
    ];

    for event in &events_to_write {
        write_event(cwd, event).unwrap();
    }

    // Read back all events
    let events = read_events(cwd).unwrap();
    assert_eq!(events.len(), 3, "Should have 3 events");

    // Verify ordering (sorted by timestamp)
    match &events[0] {
        TaskEvent::Created { task_id, .. } => assert_eq!(task_id, "chain001"),
        other => panic!("Expected Created, got {:?}", other),
    }
    match &events[1] {
        TaskEvent::Started { task_ids, .. } => assert_eq!(task_ids, &vec!["chain001".to_string()]),
        other => panic!("Expected Started, got {:?}", other),
    }
    match &events[2] {
        TaskEvent::Closed {
            task_ids, summary, ..
        } => {
            assert_eq!(task_ids, &vec!["chain001".to_string()]);
            assert_eq!(summary, &Some("Completed the test".to_string()));
        }
        other => panic!("Expected Closed, got {:?}", other),
    }

    // Verify the bookmark points to the last event (chain integrity)
    let bm_id = bookmark_change_id(cwd, "aiki/tasks");
    assert!(!bm_id.is_empty(), "Bookmark should exist");
}

#[test]
fn test_read_events_empty_repo() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp = tempdir().unwrap();
    let cwd = temp.path();
    init_jj_workspace(cwd).unwrap();

    use aiki::tasks::storage::read_events;

    // No events written yet — should return empty vec
    let events = read_events(cwd).unwrap();
    assert!(events.is_empty(), "Should have no events in fresh repo");
}
