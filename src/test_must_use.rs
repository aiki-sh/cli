// Test to verify event construction works properly
#![cfg(test)]

use crate::events::{
    AikiChangeCompletedPayload, AikiEvent, AikiSessionStartPayload, ChangeOperation, WriteOperation,
};
use crate::provenance::record::{AgentType, DetectionMethod};
use crate::session::{AikiSession, SessionMode};
use std::path::PathBuf;

#[test]
fn test_must_use_warning_compilation() {
    // Test that event construction works
    let session = AikiSession::new(
        AgentType::ClaudeCode,
        "session-123".to_string(),
        None::<&str>,
        DetectionMethod::Hook,
        SessionMode::Interactive,
    );
    let _event = AikiEvent::SessionStarted(AikiSessionStartPayload {
        session,
        cwd: PathBuf::from("/tmp"),
        timestamp: chrono::Utc::now(),
        transcript_path: None,
    });
}

#[test]
fn test_impl_asref_path_ergonomics() {
    // Test that events can be constructed with various path types

    // Using &str
    let session1 = AikiSession::new(
        AgentType::ClaudeCode,
        "session-1".to_string(),
        None::<&str>,
        DetectionMethod::Hook,
        SessionMode::Interactive,
    );
    let _event1 = AikiEvent::SessionStarted(AikiSessionStartPayload {
        session: session1,
        cwd: PathBuf::from("/tmp"),
        timestamp: chrono::Utc::now(),
        transcript_path: None,
    });

    // Using String
    let session2 = AikiSession::new(
        AgentType::ClaudeCode,
        "session-123".to_string(),
        None::<&str>,
        DetectionMethod::Hook,
        SessionMode::Interactive,
    );
    let _event2 = AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
        session: session2,
        cwd: PathBuf::from(String::from("/tmp")),
        timestamp: chrono::Utc::now(),
        tool_name: "Edit".to_string(),
        success: true,
        turn: crate::events::Turn::unknown(),
        operation: ChangeOperation::Write(WriteOperation {
            file_paths: vec!["/tmp/file.rs".to_string()],
            edit_details: vec![],
        }),
    });

    // Using &String
    let s = String::from("/tmp");
    let session3 = AikiSession::new(
        AgentType::Cursor,
        "session-2".to_string(),
        None::<&str>,
        DetectionMethod::Hook,
        SessionMode::Interactive,
    );
    let _event3 = AikiEvent::SessionStarted(AikiSessionStartPayload {
        session: session3,
        cwd: PathBuf::from(&s),
        timestamp: chrono::Utc::now(),
        transcript_path: None,
    });

    // Using PathBuf
    let session4 = AikiSession::new(
        AgentType::ClaudeCode,
        "session-3".to_string(),
        None::<&str>,
        DetectionMethod::Hook,
        SessionMode::Interactive,
    );
    let _event4 = AikiEvent::SessionStarted(AikiSessionStartPayload {
        session: session4,
        cwd: PathBuf::from("/tmp"),
        timestamp: chrono::Utc::now(),
        transcript_path: None,
    });

    // Using &PathBuf
    let pb = PathBuf::from("/tmp");
    let session5 = AikiSession::new(
        AgentType::ClaudeCode,
        "session-123".to_string(),
        None::<&str>,
        DetectionMethod::Hook,
        SessionMode::Interactive,
    );
    let _event5 = AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
        session: session5,
        cwd: pb.clone(),
        timestamp: chrono::Utc::now(),
        tool_name: "Write".to_string(),
        success: true,
        turn: crate::events::Turn::unknown(),
        operation: ChangeOperation::Write(WriteOperation {
            file_paths: vec!["/tmp/file.rs".to_string()],
            edit_details: vec![],
        }),
    });

    // Using &Path
    let session6 = AikiSession::new(
        AgentType::Cursor,
        "session-4".to_string(),
        None::<&str>,
        DetectionMethod::Hook,
        SessionMode::Interactive,
    );
    let _event6 = AikiEvent::SessionStarted(AikiSessionStartPayload {
        session: session6,
        cwd: pb.as_path().to_path_buf(),
        timestamp: chrono::Utc::now(),
        transcript_path: None,
    });
}
