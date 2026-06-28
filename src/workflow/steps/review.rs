//! Review step runners.
//!
//! Contains the workflow step handler for running reviews. Domain types and
//! logic (scope, location, create, detect) live in `crate::reviews`.

use super::StepResult;
use super::WorkflowChange;
use super::WorkflowContext;
use crate::error::{AikiError, Result};
use crate::tasks::runner::{task_run_on_session, TaskRunOptions};
use crate::tasks::{
    find_task, get_subtasks, materialize_graph, read_events, write_event, TaskEvent, TaskStatus,
};
use std::path::Path;

/// Maximum consecutive review-agent restarts that close no new phase before we
/// stop retrying and degrade to a partial review.
///
/// A review agent runs synchronously (explore → record issues → close) with no
/// async lanes running between spawns. In headless mode it sometimes ends its
/// turn (its process exits) mid-review — e.g. right after closing the `explore`
/// phase — which would otherwise surface as a hard "Agent process exited without
/// completing task" error. We re-spawn the agent as long as each attempt closes
/// a new phase, and give up only after this many no-progress attempts in a row.
const REVIEW_MAX_NO_PROGRESS_RETRIES: u32 = 3;

/// Run a pre-created review task from ctx.task_id.
///
/// Used after `SetupReview` has already created the review task. Runs the
/// review agent and reports the issue count. Fix-after-review logic is
/// handled by the `RegressionReview` step via dynamic step injection.
///
/// If the agent exits before closing the review task (common in headless mode),
/// it is auto-replaced while it keeps closing review phases. If it still can't
/// finish, the review degrades to a partial result (the review is marked stopped
/// and the step reports what was recorded) rather than erroring out — see
/// [`drive_review`] and [`REVIEW_MAX_NO_PROGRESS_RETRIES`].
pub(crate) fn run(ctx: &mut WorkflowContext) -> anyhow::Result<StepResult> {
    let review_id = ctx
        .task_id
        .as_ref()
        .ok_or_else(|| {
            AikiError::InvalidArgument("No review task ID in workflow context".to_string())
        })?
        .clone();

    ctx.status("running review agent");

    // Run the review agent, auto-replacing it if it exits before closing the
    // review task — as long as it keeps closing review phases. Each spawn is
    // non-finalizing (`spawn_drain` / `task_run_on_session`), so an early exit
    // does not stop the review task or cascade; finalize happens once below.
    let cwd = ctx.cwd.clone();
    let output = ctx.output;
    if ctx.notify_rx.is_some() {
        let notify_rx = ctx.notify_rx.clone();
        let rid = review_id.clone();
        let spawn_cwd = cwd.clone();
        let spawn_once = || -> Result<()> {
            let options = TaskRunOptions::new();
            let mut handler = super::ReviewDrainHandler::new(rid.clone(), output);
            super::spawn_drain(
                &spawn_cwd,
                &rid,
                &options,
                notify_rx.as_ref(),
                output,
                &mut handler,
            )
        };
        drive_review(&cwd, &review_id, REVIEW_MAX_NO_PROGRESS_RETRIES, spawn_once)?;
    } else {
        let rid = review_id.clone();
        let spawn_cwd = cwd.clone();
        let spawn_once = || -> Result<()> {
            let options = TaskRunOptions::new().quiet();
            task_run_on_session(&spawn_cwd, &rid, options, false).map(|_| ())
        };
        drive_review(&cwd, &review_id, REVIEW_MAX_NO_PROGRESS_RETRIES, spawn_once)?;
    }

    ctx.status("collecting results");
    let events = read_events(&cwd)?;
    let graph = materialize_graph(&events);
    let review = find_task(&graph.tasks, &review_id).ok();
    let ic = review.map(crate::reviews::issue_count).unwrap_or(0);
    let completed = review.map(|t| t.status == TaskStatus::Closed).unwrap_or(false);

    let message = if completed {
        review_outcome_message(ic)
    } else {
        // The agent gave up before closing the review. Don't surface a raw spawn
        // failure: mark the review stopped (so it isn't left dangling in-progress)
        // and report what the partial pass produced.
        let subtasks = get_subtasks(&graph, &review_id);
        let total = subtasks.len();
        let done = subtasks
            .iter()
            .filter(|t| t.status == TaskStatus::Closed)
            .count();
        let already_terminal = review
            .map(|t| matches!(t.status, TaskStatus::Stopped | TaskStatus::Closed))
            .unwrap_or(false);
        if !already_terminal {
            let reason = format!(
                "Review incomplete: agent exited after {}/{} phases",
                done, total
            );
            write_event(
                &cwd,
                &TaskEvent::Stopped {
                    task_ids: vec![review_id.clone()],
                    reason: Some(reason),
                    session_id: None,
                    turn_id: None,
                    timestamp: chrono::Utc::now(),
                },
            )?;
        }
        review_incomplete_message(done, total, ic)
    };

    Ok(StepResult {
        change: WorkflowChange::None,
        message,
        task_id: Some(review_id),
    })
}

/// Format the completed-review step message.
fn review_outcome_message(issue_count: usize) -> String {
    if issue_count > 0 {
        format!("Found {} issue{}", issue_count, plural(issue_count))
    } else {
        "approved".to_string()
    }
}

/// Format the give-up (partial review) step message.
fn review_incomplete_message(done: usize, total: usize, issue_count: usize) -> String {
    format!(
        "incomplete: {}/{} phases done, {} issue{} recorded",
        done,
        total,
        issue_count,
        plural(issue_count)
    )
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// `(review task is terminal i.e. Closed/Stopped, # of its closed subtasks)`.
/// Reads fresh JJ state; returns `(false, 0)` if events can't be read.
fn review_state(cwd: &Path, review_id: &str) -> (bool, usize) {
    let Ok(events) = read_events(cwd) else {
        return (false, 0);
    };
    let graph = materialize_graph(&events);
    let terminal = find_task(&graph.tasks, review_id)
        .ok()
        .map(|t| matches!(t.status, TaskStatus::Closed | TaskStatus::Stopped))
        .unwrap_or(false);
    let closed = get_subtasks(&graph, review_id)
        .iter()
        .filter(|t| t.status == TaskStatus::Closed)
        .count();
    (terminal, closed)
}

/// Pure decision: whether to re-spawn the review agent after an early exit.
///
/// Returns `(retry, next_no_progress)`:
/// - the review task is terminal (the agent closed/stopped it) → stop;
/// - a new phase closed since the last spawn → retry, reset the counter;
/// - otherwise count it; give up once `max_no_progress` is reached.
fn should_retry_review(
    terminal: bool,
    closed_before: usize,
    closed_after: usize,
    no_progress: u32,
    max_no_progress: u32,
) -> (bool, u32) {
    if terminal {
        return (false, no_progress);
    }
    if closed_after > closed_before {
        (true, 0)
    } else {
        let n = no_progress + 1;
        (n < max_no_progress, n)
    }
}

/// Run the review agent, re-spawning it while it keeps closing review phases.
///
/// Unlike the loop orchestrator (`steps::loop`), a review has no async lanes —
/// nothing runs between spawns — so we never poll/wait: we re-spawn immediately
/// on progress and give up after `max_no_progress` consecutive attempts that
/// close no new phase. Each `spawn_once` is non-finalizing; the terminal
/// finalize (graceful degrade vs. completion) is the caller's job.
fn drive_review(
    cwd: &Path,
    review_id: &str,
    max_no_progress: u32,
    mut spawn_once: impl FnMut() -> Result<()>,
) -> Result<()> {
    let mut no_progress = 0u32;
    loop {
        let (_, closed_before) = review_state(cwd, review_id);
        // A spawn error (e.g. the agent binary failed to launch) is itself a
        // no-progress outcome; fall through to the state check so the cap bounds
        // retries instead of erroring immediately.
        let _ = spawn_once();
        let (terminal, closed_after) = review_state(cwd, review_id);
        let (retry, n) = should_retry_review(
            terminal,
            closed_before,
            closed_after,
            no_progress,
            max_no_progress,
        );
        no_progress = n;
        if !retry {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Verify the review drain logic counts only CommentAdded events that are review issues.
    #[test]
    fn review_drain_counts_issues_from_comments() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let review_id = "review_001";
        let now = chrono::Utc::now();

        let issue_data: HashMap<String, String> =
            [("issue".to_string(), "true".to_string())].into();

        // Review issue on the review task (should count)
        tx.send(TaskEvent::CommentAdded {
            task_ids: vec![review_id.to_string()],
            text: "Issue: null check missing".to_string(),
            data: issue_data.clone(),
            timestamp: now,
        })
        .unwrap();

        // Regular comment on the review task (should NOT count)
        tx.send(TaskEvent::CommentAdded {
            task_ids: vec![review_id.to_string()],
            text: "Progress update: halfway done".to_string(),
            data: HashMap::new(),
            timestamp: now,
        })
        .unwrap();

        // Comment on a different task (should not count)
        tx.send(TaskEvent::CommentAdded {
            task_ids: vec!["other_task".to_string()],
            text: "Unrelated comment".to_string(),
            data: issue_data.clone(),
            timestamp: now,
        })
        .unwrap();

        // Another review issue on the review task (should count)
        tx.send(TaskEvent::CommentAdded {
            task_ids: vec![review_id.to_string()],
            text: "Issue: error handling missing".to_string(),
            data: issue_data.clone(),
            timestamp: now,
        })
        .unwrap();

        drop(tx);

        let mut issue_count: usize = 0;
        for event in rx.try_iter() {
            if let TaskEvent::CommentAdded { task_ids, data, .. } = &event {
                if task_ids.iter().any(|id| id == review_id)
                    && data.get("issue").map(|v| v == "true").unwrap_or(false)
                {
                    issue_count += 1;
                }
            }
        }

        assert_eq!(issue_count, 2);
    }

    /// Verify singular/plural formatting of issue count.
    #[test]
    fn review_issue_count_formatting() {
        let fmt = |count: usize| -> String {
            format!(
                "  Found {} issue{}",
                count,
                if count == 1 { "" } else { "s" }
            )
        };

        assert_eq!(fmt(1), "  Found 1 issue");
        assert_eq!(fmt(3), "  Found 3 issues");
    }

    /// A terminal review (agent closed/stopped it) ends the retry loop, whatever
    /// the phase counts say.
    #[test]
    fn should_retry_review_stops_when_review_terminal() {
        assert_eq!(should_retry_review(true, 0, 2, 0, 3), (false, 0));
        assert_eq!(should_retry_review(true, 1, 1, 2, 3), (false, 2));
    }

    /// A newly-closed phase since the last spawn means the agent is making
    /// progress: retry and reset the no-progress counter.
    #[test]
    fn should_retry_review_retries_and_resets_on_progress() {
        assert_eq!(should_retry_review(false, 0, 1, 0, 3), (true, 0));
        // Even with a high prior no-progress count, real progress resets it.
        assert_eq!(should_retry_review(false, 1, 2, 2, 3), (true, 0));
    }

    /// No new phase closed: count it, and give up once the cap is reached.
    #[test]
    fn should_retry_review_counts_no_progress_and_gives_up_at_cap() {
        assert_eq!(should_retry_review(false, 1, 1, 0, 3), (true, 1));
        assert_eq!(should_retry_review(false, 1, 1, 1, 3), (true, 2));
        assert_eq!(should_retry_review(false, 1, 1, 2, 3), (false, 3));
    }

    /// Completed-review message: pluralized issue count, or "approved" at zero.
    #[test]
    fn review_outcome_message_formats_issue_count() {
        assert_eq!(review_outcome_message(0), "approved");
        assert_eq!(review_outcome_message(1), "Found 1 issue");
        assert_eq!(review_outcome_message(4), "Found 4 issues");
    }

    /// Partial-review message reports phases done and issues recorded so far.
    #[test]
    fn review_incomplete_message_reports_phases_and_issues() {
        assert_eq!(
            review_incomplete_message(1, 2, 0),
            "incomplete: 1/2 phases done, 0 issues recorded"
        );
        assert_eq!(
            review_incomplete_message(1, 2, 1),
            "incomplete: 1/2 phases done, 1 issue recorded"
        );
    }
}
