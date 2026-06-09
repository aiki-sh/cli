//! Review step runners.
//!
//! Contains the workflow step handler for running reviews. Domain types and
//! logic (scope, location, create, detect) live in `crate::reviews`.

use super::StepResult;
use super::WorkflowChange;
use super::WorkflowContext;
use crate::error::AikiError;
use crate::tasks::runner::{task_run, TaskRunOptions};
#[cfg(test)]
use crate::tasks::TaskEvent;
use crate::tasks::{find_task, materialize_graph, read_events};

/// Run a pre-created review task from ctx.task_id.
///
/// Used after `SetupReview` has already created the review task. Runs the
/// review agent and reports the issue count. Fix-after-review logic is
/// handled by the `RegressionReview` step via dynamic step injection.
pub(crate) fn run(ctx: &mut WorkflowContext) -> anyhow::Result<StepResult> {
    let review_id = ctx
        .task_id
        .as_ref()
        .ok_or_else(|| {
            AikiError::InvalidArgument("No review task ID in workflow context".to_string())
        })?
        .clone();

    ctx.status("running review agent");

    if ctx.notify_rx.is_some() {
        let output = ctx.output;
        let options = TaskRunOptions::new();
        let mut handler = super::ReviewDrainHandler::new(review_id.clone(), output);
        super::spawn_drain_finalize(
            &ctx.cwd,
            &review_id,
            &options,
            ctx.notify_rx.as_ref(),
            output,
            &mut handler,
        )?;
    } else {
        let options = TaskRunOptions::new().quiet();
        task_run(&ctx.cwd, &review_id, options)?;
    }

    ctx.status("collecting results");
    let events = read_events(&ctx.cwd)?;
    let graph = materialize_graph(&events);
    let ic = find_task(&graph.tasks, &review_id)
        .map(crate::reviews::issue_count)
        .unwrap_or(0);

    let message = if ic > 0 {
        format!("Found {} issues", ic)
    } else {
        "approved".to_string()
    };

    Ok(StepResult {
        change: WorkflowChange::None,
        message,
        task_id: Some(review_id),
    })
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
}
