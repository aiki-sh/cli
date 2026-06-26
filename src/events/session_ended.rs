use super::prelude::*;
use crate::global;
use crate::history;
use crate::repos;

/// session.ended event payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiSessionEndedPayload {
    pub session: AikiSession,
    pub cwd: PathBuf,
    pub timestamp: DateTime<Utc>,
    /// Reason for session termination (e.g., "clear", "logout", "user_close", "ttl_expired")
    #[serde(default)]
    pub reason: String,
    /// Cumulative token usage for the entire session
    #[serde(default)]
    pub tokens: Option<super::TokenUsage>,
}

/// Handle session.ended event
///
/// Executes the session.ended flow section for user-defined cleanup actions,
/// then cleans up the session file and records session end to history.
pub fn handle_session_ended(payload: AikiSessionEndedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    debug_log(|| format!("Session ended by {:?}", payload.session.agent_type()));

    // Aggregate token usage from all turns in this session
    let mut payload = payload;
    if payload.tokens.is_none() {
        payload.tokens = aggregate_session_tokens(&payload);
    }

    // Record session end to conversation history (non-blocking on failure)
    // Uses global JJ repo at ~/.aiki/.jj/ for cross-repo conversation history
    let cwd_str = payload.cwd.to_string_lossy();
    let repo_id = repos::compute_repo_id(&payload.cwd).ok();
    if let Err(e) = history::record_session_end(
        &global::global_aiki_dir(),
        &payload.session,
        payload.timestamp,
        &payload.reason,
        repo_id.as_deref(),
        Some(&cwd_str),
    ) {
        debug_log(|| format!("Failed to record session end: {}", e));
    }

    // Load core hook for fallback
    let core_hook = crate::flows::load_core_hook();

    // Build execution state from payload (clone needed for session.end() call below)
    let mut state = AikiState::new(payload.clone());

    // Execute hook via HookComposer (with fallback to bundled core hook)
    let flow_result = execute_hook(
        EventType::SessionEnded,
        &mut state,
        &core_hook.handlers.session_ended,
    )?;

    // Clean up session file (always happens, regardless of flow result)
    payload.session.end()?;

    // TurnState is now ephemeral (queried from JJ) - no file cleanup needed

    // Extract failures from state
    let failures = state.take_failures();

    // Translate HookOutcome to HookResult
    match flow_result {
        HookOutcome::Success | HookOutcome::FailedContinue | HookOutcome::FailedStop => {
            Ok(HookResult {
                context: None,
                decision: Decision::Allow,
                failures,
            })
        }
        HookOutcome::FailedBlock => Ok(HookResult {
            context: None,
            decision: Decision::Block,
            failures,
        }),
    }
}

/// Aggregate token usage from all Response events for a session.
///
/// Returns `None` if no turns had token data (rather than returning zeros),
/// per the acceptance criteria.
fn aggregate_session_tokens(payload: &AikiSessionEndedPayload) -> Option<super::TokenUsage> {
    let session_id = payload.session.uuid();
    let events = match history::storage::read_events(&global::global_aiki_dir()) {
        Ok(events) => events,
        Err(e) => {
            debug_log(|| format!("Failed to read events for token aggregation: {}", e));
            return None;
        }
    };

    sum_session_turn_tokens(session_id, &events)
}

/// Sum the per-turn `Response` token usage for one session, **deduplicated by
/// turn number** so a turn that was recorded more than once contributes exactly
/// once.
///
/// Per-turn token slices are disjoint (the parsers reset their accumulators on
/// each new turn; see `test_extract_resets_accumulators_on_user_entry`), so
/// summing distinct turns is correct — but only while each turn is counted
/// once. Without this guard a `Response` event written twice for the same turn
/// (e.g. a re-dispatch) would compound into the session total. When a turn
/// appears more than once the last record wins, matching the most-recent-wins
/// turn attribution used elsewhere.
///
/// Returns `None` when no matching turn carried token data (rather than a zero
/// total), per the session-aggregate contract.
fn sum_session_turn_tokens(
    session_id: &str,
    events: &[history::types::ConversationEvent],
) -> Option<super::TokenUsage> {
    use std::collections::BTreeMap;

    let mut by_turn: BTreeMap<u32, super::TokenUsage> = BTreeMap::new();
    for event in events {
        if let history::types::ConversationEvent::Response {
            session_id: sid,
            turn,
            tokens: Some(t),
            ..
        } = event
        {
            if sid == session_id {
                by_turn.insert(*turn, t.clone());
            }
        }
    }

    if by_turn.is_empty() {
        None
    } else {
        Some(by_turn.into_values().sum())
    }
}

#[cfg(test)]
mod tests {
    use super::super::TokenUsage;
    use crate::history::types::{AgentType, ConversationEvent};
    use chrono::{DateTime, Utc};

    fn tok(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input,
            output,
            cache_read: 0,
            cache_created: 0,
        }
    }

    fn response(session_id: &str, turn: u32, tokens: Option<TokenUsage>) -> ConversationEvent {
        ConversationEvent::Response {
            session_id: session_id.to_string(),
            agent_type: AgentType::ClaudeCode,
            turn,
            files_written: vec![],
            content: None,
            tokens,
            model: None,
            task_id: None,
            timestamp: DateTime::parse_from_rfc3339("2026-01-09T10:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            repo_id: None,
            cwd: None,
        }
    }

    /// The headline D3 guard: a turn whose `Response` was recorded twice must
    /// not compound into the session aggregate.
    #[test]
    fn two_identical_responses_for_one_turn_do_not_double() {
        let single = super::sum_session_turn_tokens("s", &[response("s", 1, Some(tok(100, 50)))])
            .expect("one turn with tokens aggregates");
        assert_eq!(single.total(), 150);

        let doubled = super::sum_session_turn_tokens(
            "s",
            &[
                response("s", 1, Some(tok(100, 50))),
                response("s", 1, Some(tok(100, 50))),
            ],
        )
        .expect("duplicate turn still aggregates");

        // Deduped by turn id: the second identical record is dropped.
        assert_eq!(doubled.total(), single.total());
        assert_eq!(doubled.input, 100);
        assert_eq!(doubled.output, 50);
    }

    /// Dedup must not collapse genuinely distinct turns: each turn number is a
    /// separate, disjoint slice and all of them are summed.
    #[test]
    fn distinct_turns_are_summed() {
        let agg = super::sum_session_turn_tokens(
            "s",
            &[
                response("s", 1, Some(tok(100, 50))),
                response("s", 2, Some(tok(200, 25))),
            ],
        )
        .expect("two turns aggregate");
        assert_eq!(agg.input, 300);
        assert_eq!(agg.output, 75);
        assert_eq!(agg.total(), 375);
    }

    /// When a turn is re-recorded with updated tokens the last write wins.
    #[test]
    fn duplicate_turn_keeps_last_record() {
        let agg = super::sum_session_turn_tokens(
            "s",
            &[
                response("s", 1, Some(tok(100, 50))),
                response("s", 1, Some(tok(999, 999))),
            ],
        )
        .expect("aggregates");
        assert_eq!(agg.input, 999);
        assert_eq!(agg.output, 999);
    }

    /// Only the target session's turns count toward its aggregate.
    #[test]
    fn other_sessions_are_excluded() {
        let agg = super::sum_session_turn_tokens(
            "s",
            &[
                response("s", 1, Some(tok(100, 50))),
                response("other", 1, Some(tok(9999, 9999))),
            ],
        )
        .expect("target session aggregates");
        assert_eq!(agg.total(), 150);
    }

    /// No turn carried token data => `None`, not a zero total.
    #[test]
    fn no_token_data_yields_none() {
        assert!(super::sum_session_turn_tokens("s", &[]).is_none());
        assert!(
            super::sum_session_turn_tokens("s", &[response("s", 1, None)]).is_none(),
            "a Response with tokens: None contributes nothing"
        );
    }
}
