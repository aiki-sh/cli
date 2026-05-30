//! State management for ACP proxy
//!
//! This module handles mutable state used by the proxy:
//! - Autoreply counters (limit automated follow-ups)
//! - Tool call context tracking
//! - Response accumulators

use std::collections::HashMap;
use std::path::PathBuf;

use super::handlers::Autoreply;
use super::protocol::{ClientInfo, SessionId};

/// Maximum number of autoreplies allowed per turn
pub const MAX_AUTOREPLIES: usize = 5;

/// State coordination messages sent between proxy threads
/// (IDE→Agent thread sends, Agent→IDE thread owns state)
#[derive(Debug, Clone)]
pub enum StateMessage {
    /// Update client (IDE) information detected from initialize request
    SetClientInfo(ClientInfo),
    /// Update working directory from session/new or session/load
    SetWorkingDirectory(PathBuf),
    /// Track session/prompt request for session.ended event matching
    TrackPrompt {
        request_id: serde_json::Value, // Raw JSON-RPC "id" field (normalized at consumption)
        session_id: SessionId,
    },
    /// Clear response accumulator for a session (on new prompt)
    ClearAccumulator {
        session_id: SessionId,
    },
    /// Reset autoreply counter for a session (on new user prompt)
    ResetAutoreplyCounter {
        session_id: SessionId,
    },
    /// Track session/new request ID to match with response for session.started event
    TrackNewSession {
        request_id: serde_json::Value, // Raw JSON-RPC "id" field (normalized at consumption)
        agent_pid: Option<u32>,        // Optional agent PID for PID-based session detection
    },
    /// Signal shutdown when agent process exits
    Shutdown,
}

/// Messages sent through the autoreply channel
#[derive(Debug, Clone)]
pub enum AutoreplyMessage {
    /// A JSON-RPC autoreply message to be sent to the agent only (not forwarded to IDE)
    SendAutoreply(Autoreply),
    /// Explicit shutdown signal
    Shutdown,
}

// ============================================================================
// Autoreply counter management
// ============================================================================

/// Check if autoreply limit has been reached
#[must_use]
pub fn check_autoreply_limit(current_count: usize, max_autoreplies: usize) -> bool {
    current_count >= max_autoreplies
}

/// Increment autoreply counter for a session
pub fn increment_autoreply_counter(
    counters: &mut HashMap<SessionId, usize>,
    session_id: &SessionId,
) -> usize {
    let current_count = counters.get(session_id).copied().unwrap_or(0);
    let new_count = current_count + 1;
    counters.insert(session_id.clone(), new_count);
    new_count
}

/// Reset autoreply counter for a session
pub fn reset_autoreply_counter(counters: &mut HashMap<SessionId, usize>, session_id: &SessionId) {
    counters.remove(session_id);
}
