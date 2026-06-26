//! Bridge turn-level token usage onto the per-task denormalized rollup.
//!
//! Tokens are recorded per-turn on history `Response` events tagged with the
//! focused task (see [`crate::history::types::ConversationEvent::Response`]'s
//! `task_id`). The display surfaces (`aiki tldr`, the build TUI agent-stats
//! line, the run summary) read a single denormalized total at
//! `task.data["tokens"]`. This module keeps that denormalized copy in sync: when
//! a turn is attributed to a focused (leaf) task, its tokens are added to that
//! task's total and to the total of every `subtask-of` ancestor, so an epic
//! shows the sum over its subtree while each turn is counted exactly once.
//!
//! Attribution model (see `ops/now/token-tracking-fixes.md` finding C1): tokens
//! attribute per-turn to the focused task = the most-recently-started in-progress
//! task claimed by the session; parents/epics get their total by rollup over the
//! `subtask-of` tree, never by direct attribution. Forward-only: turns are tagged
//! from this work onward.

use std::collections::HashMap;
use std::path::Path;

use super::graph::TaskGraph;
use super::manager::get_all_descendants;
use super::types::Task;
use super::TaskEvent;
use crate::error::Result;
use crate::history::types::ConversationEvent;

/// Task-data key holding the denormalized rollup total of billed tokens.
pub const TOKENS_DATA_KEY: &str = "tokens";

/// Sentinel bucket key for turns with no focused task ("session overhead").
///
/// Turns that complete with no in-progress task claimed by the session are
/// recorded with `task_id: None`. [`direct_token_totals`] sums those under this
/// key so the unattributed total stays explicit and queryable — it is never
/// rolled onto a real task and never silently dropped.
///
/// Forward-looking: surfaced by the read-through / consumer-path work; the
/// production write path attributes via [`record_turn_tokens`].
#[allow(dead_code)]
pub const UNATTRIBUTED_BUCKET: &str = "__session_overhead__";

/// Sum per-task DIRECT (non-rolled-up) billed tokens from turn-tagged `Response`
/// events. Turns with no focused task land under [`UNATTRIBUTED_BUCKET`].
///
/// This is the canonical definition of a task's direct total
/// (`sum(turns where task_id == T)`), correct across any number of sessions. The
/// denormalized `task.data["tokens"]` rollup written by [`record_turn_tokens`] is
/// a forward-only incremental cache of [`subtree_total`] over this map.
///
/// Forward-looking: backs the optional read-through display and the
/// consumer-path test; the production write path uses [`rollup_updates`].
#[allow(dead_code)]
#[must_use]
pub fn direct_token_totals(events: &[ConversationEvent]) -> HashMap<String, u64> {
    let mut totals: HashMap<String, u64> = HashMap::new();
    for event in events {
        if let ConversationEvent::Response {
            task_id,
            tokens: Some(t),
            ..
        } = event
        {
            let key = task_id
                .clone()
                .unwrap_or_else(|| UNATTRIBUTED_BUCKET.to_string());
            *totals.entry(key).or_insert(0) += t.total();
        }
    }
    totals
}

/// Rollup total for a task: its own direct total plus the direct totals of every
/// `subtask-of` descendant. A leaf task rolls up to its own direct total.
///
/// Forward-looking companion to [`direct_token_totals`] for the read-through
/// display; the production write path uses the incremental [`rollup_updates`].
#[allow(dead_code)]
#[must_use]
pub fn subtree_total(graph: &TaskGraph, direct: &HashMap<String, u64>, task_id: &str) -> u64 {
    let mut total = direct.get(task_id).copied().unwrap_or(0);
    for descendant in get_all_descendants(graph, task_id) {
        total += direct.get(&descendant.id).copied().unwrap_or(0);
    }
    total
}

/// Read the current denormalized rollup total from a task's data (0 if absent).
fn current_total(task: Option<&Task>) -> u64 {
    task.and_then(|t| t.data.get(TOKENS_DATA_KEY))
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Compute the `(task_id, new_total)` denormalized rollup writes for attributing
/// `delta` billed tokens to the focused (leaf) task `focused_id`.
///
/// The focused task and every `subtask-of` ancestor each gain `delta`: the leaf
/// is in every ancestor's subtree, so each ancestor's rollup grows by exactly the
/// same delta. Returns an empty vec when `delta == 0` so token-less turns (e.g.
/// Cursor/ACP today) write nothing. New totals are computed incrementally from
/// the current `task.data["tokens"]` in `graph`; this is exact because this path
/// is the sole writer of that key (forward-only).
#[must_use]
pub fn rollup_updates(graph: &TaskGraph, focused_id: &str, delta: u64) -> Vec<(String, u64)> {
    if delta == 0 {
        return Vec::new();
    }
    let mut targets = vec![focused_id.to_string()];
    targets.extend(graph.ancestor_chain(focused_id));

    targets
        .into_iter()
        .map(|id| {
            let new_total = current_total(graph.tasks.get(&id)) + delta;
            (id, new_total)
        })
        .collect()
}

/// Attribute a completed turn's `delta` billed tokens to `focused_id` and its
/// `subtask-of` ancestors, persisting the denormalized rollup onto `task.data`.
///
/// Best-effort: a no-op when `delta == 0`. On any write the focused task plus
/// every ancestor are updated atomically in one batch. Errors are returned for
/// the caller to log; token bookkeeping must never abort the turn.
pub fn record_turn_tokens(
    cwd: &Path,
    graph: &TaskGraph,
    focused_id: &str,
    delta: u64,
) -> Result<()> {
    let updates = rollup_updates(graph, focused_id, delta);
    if updates.is_empty() {
        return Ok(());
    }
    let timestamp = chrono::Utc::now();
    let events: Vec<TaskEvent> = updates
        .into_iter()
        .map(|(task_id, total)| TaskEvent::Updated {
            task_id,
            name: None,
            priority: None,
            assignee: None,
            data: Some(HashMap::from([(
                TOKENS_DATA_KEY.to_string(),
                total.to_string(),
            )])),
            instructions: None,
            timestamp,
        })
        .collect();
    super::storage::write_events_batch(cwd, &events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::TokenUsage;
    use crate::tasks::graph::materialize_graph;
    use crate::tasks::types::{TaskPriority, TaskStatus};

    fn response_with_task(task_id: Option<&str>, total: u64) -> ConversationEvent {
        ConversationEvent::Response {
            session_id: "sess".to_string(),
            agent_type: crate::agents::AgentType::ClaudeCode,
            turn: 1,
            files_written: vec![],
            content: None,
            // Put the whole figure in `output` so `total()` == `total`.
            tokens: Some(TokenUsage {
                input: 0,
                output: total,
                cache_read: 0,
                cache_created: 0,
            }),
            model: None,
            task_id: task_id.map(String::from),
            timestamp: chrono::Utc::now(),
            repo_id: None,
            cwd: None,
        }
    }

    fn created(task_id: &str) -> TaskEvent {
        TaskEvent::Created {
            task_id: task_id.to_string(),
            name: task_id.to_string(),
            slug: None,
            task_type: None,
            priority: TaskPriority::P2,
            assignee: None,
            sources: vec![],
            template: None,
            instructions: None,
            data: HashMap::new(),
            timestamp: chrono::Utc::now(),
        }
    }

    fn subtask_link(child: &str, parent: &str) -> TaskEvent {
        TaskEvent::LinkAdded {
            from: child.to_string(),
            to: parent.to_string(),
            kind: "subtask-of".to_string(),
            autorun: None,
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn direct_totals_sum_per_task_and_bucket_unattributed() {
        let events = vec![
            response_with_task(Some("leaf"), 100),
            response_with_task(Some("leaf"), 50),
            response_with_task(Some("other"), 7),
            response_with_task(None, 9),
            response_with_task(None, 1),
        ];
        let totals = direct_token_totals(&events);
        assert_eq!(totals.get("leaf"), Some(&150));
        assert_eq!(totals.get("other"), Some(&7));
        assert_eq!(totals.get(UNATTRIBUTED_BUCKET), Some(&10));
    }

    #[test]
    fn subtree_total_rolls_up_descendants() {
        // parent <- child <- grandchild
        let events = vec![
            created("parent"),
            created("child"),
            created("grandchild"),
            subtask_link("child", "parent"),
            subtask_link("grandchild", "child"),
        ];
        let graph = materialize_graph(&events);
        let mut direct = HashMap::new();
        direct.insert("parent".to_string(), 1);
        direct.insert("child".to_string(), 10);
        direct.insert("grandchild".to_string(), 100);

        assert_eq!(subtree_total(&graph, &direct, "grandchild"), 100);
        assert_eq!(subtree_total(&graph, &direct, "child"), 110);
        assert_eq!(subtree_total(&graph, &direct, "parent"), 111);
    }

    #[test]
    fn rollup_updates_hits_leaf_and_every_ancestor() {
        let events = vec![
            created("parent"),
            created("child"),
            created("grandchild"),
            subtask_link("child", "parent"),
            subtask_link("grandchild", "child"),
        ];
        let graph = materialize_graph(&events);

        let updates = rollup_updates(&graph, "grandchild", 100);
        let map: HashMap<_, _> = updates.into_iter().collect();
        // Leaf + both ancestors each gain the full delta (all start at 0).
        assert_eq!(map.get("grandchild"), Some(&100));
        assert_eq!(map.get("child"), Some(&100));
        assert_eq!(map.get("parent"), Some(&100));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn rollup_updates_adds_to_existing_denormalized_total() {
        let mut child_data = HashMap::new();
        child_data.insert(TOKENS_DATA_KEY.to_string(), "40".to_string());
        let events = vec![
            TaskEvent::Created {
                task_id: "child".to_string(),
                name: "child".to_string(),
                slug: None,
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: vec![],
                template: None,
                instructions: None,
                data: child_data,
                timestamp: chrono::Utc::now(),
            },
        ];
        let graph = materialize_graph(&events);
        assert_eq!(graph.tasks.get("child").map(|t| t.status), Some(TaskStatus::Open));

        let updates = rollup_updates(&graph, "child", 5);
        assert_eq!(updates, vec![("child".to_string(), 45)]);
    }

    #[test]
    fn rollup_updates_zero_delta_is_noop() {
        let events = vec![created("solo")];
        let graph = materialize_graph(&events);
        assert!(rollup_updates(&graph, "solo", 0).is_empty());
    }
}
