//! Integration tests for history recorder session-start thread wiring
//!
//! Tests cover:
//! 4a. record_session_start includes run_thread when session has a thread
//! 4b. record_session_start omits run_thread when session has no thread

mod common;

use common::{init_jj_workspace, jj_available};
use tempfile::tempdir;

use aiki::agents::AgentType;
use aiki::history::record_session_start;
use aiki::history::storage::read_events;
use aiki::history::types::ConversationEvent;
use aiki::provenance::DetectionMethod;
use aiki::session::AikiSession;
use aiki::session::SessionMode;
use aiki::tasks::lanes::ThreadId;
use chrono::Utc;

#[test]
fn test_record_session_start_includes_thread() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp = tempdir().unwrap();
    let cwd = temp.path();
    init_jj_workspace(cwd).unwrap();

    let head = "aaaabbbbccccddddeeeeffffgggghhhh";
    let tail = "iiiijjjjkkkkllllmmmmnnnnoooopppp";
    let thread = ThreadId::parse(&format!("{head}:{tail}")).unwrap();

    let session = AikiSession::new(
        AgentType::ClaudeCode,
        "ext-123",
        Some("0.1.0"),
        DetectionMethod::Hook,
        SessionMode::Interactive,
    )
    .with_thread(Some(thread));

    record_session_start(cwd, &session, Utc::now(), None, None).unwrap();

    let events = read_events(cwd).unwrap();
    assert_eq!(events.len(), 1);

    match &events[0] {
        ConversationEvent::SessionStart { run_thread_id, .. } => {
            let expected = format!("{head}:{tail}");
            assert_eq!(
                run_thread_id,
                &Some(expected),
                "run_thread_id should contain serialized thread H:T"
            );
        }
        other => panic!("Expected SessionStart, got {other:?}"),
    }
}

#[test]
fn test_record_session_start_no_thread() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp = tempdir().unwrap();
    let cwd = temp.path();
    init_jj_workspace(cwd).unwrap();

    let session = AikiSession::new(
        AgentType::ClaudeCode,
        "ext-456",
        Some("0.1.0"),
        DetectionMethod::Hook,
        SessionMode::Interactive,
    );
    // No .with_thread() — thread is None

    record_session_start(cwd, &session, Utc::now(), None, None).unwrap();

    let events = read_events(cwd).unwrap();
    assert_eq!(events.len(), 1);

    match &events[0] {
        ConversationEvent::SessionStart { run_thread_id, .. } => {
            assert_eq!(
                run_thread_id, &None,
                "run_thread_id should be None when session has no thread"
            );
        }
        other => panic!("Expected SessionStart, got {other:?}"),
    }
}
