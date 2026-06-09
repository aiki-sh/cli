use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::Receiver;

pub(crate) use super::WorkflowContext;
use super::WorkflowOutput;
use crate::error::AikiError;
use crate::tasks::runner::{
    finalize_agent_run, handle_session_result, prepare_task_run, rollback_if_still_reserved,
    task_run, task_run_on_session, TaskRunOptions,
};
use crate::tasks::graph::GraphDelta;
use crate::tasks::storage::read_events;

pub(crate) mod decompose;
pub(crate) mod fix;
pub(crate) mod r#loop;
pub(crate) mod plan;
pub(crate) mod regression_review;
pub(crate) mod review;
pub(crate) mod setup_epic;
pub(crate) mod setup_fix;
pub(crate) mod setup_review;

/// A workflow-level change requested by a step.
pub enum WorkflowChange {
    /// No change to the workflow.
    None,
    /// Append additional steps after the current position.
    NextSteps(Vec<Step>),
    /// Remove matching steps from the remaining queue.
    SkipSteps(Vec<Step>),
}

/// Result returned by a single workflow step.
pub struct StepResult {
    pub message: String,
    pub task_id: Option<String>,
    pub change: WorkflowChange,
}

/// Unified step enum covering all workflow step variants.
///
/// Commands compose workflows by selecting which variants to include in their
/// step sequence. Options are read from `WorkflowContext.opts`; only runtime
/// state that varies per-step remains as variant fields.
pub enum Step {
    /// Validate plan, find/create epic, check blockers, set ctx.task_id.
    ///
    /// When `ctx.task_id` is None (plan path): validates plan, checks draft,
    /// cleans stale builds, finds or creates epic with restart handling.
    /// When `ctx.task_id` is Some (epic ID): looks up epic, extracts plan_path,
    /// checks blockers.
    SetupEpic,

    /// Find/create epic, set ctx.task_id, run decompose agent.
    Decompose,

    /// Run loop orchestrator over subtasks.
    Loop,

    /// Detect target, validate constraints, create review task, set ctx.task_id.
    ///
    /// Cheap setup step — does scope detection and task creation but does NOT
    /// run the review agent. Paired with a subsequent `Review` step.
    SetupReview,

    /// Run a pre-created review task from ctx.task_id.
    ///
    /// Always paired with a prior `SetupReview` step that creates the review
    /// task and sets ctx.task_id. Fix-after-review is handled at the workflow
    /// level by the `RegressionReview` step via dynamic step injection.
    Review,

    /// Validate review task, resolve scope/assignee/template, create fix-parent.
    ///
    /// Cheap setup step — does validation and task creation but does NOT run
    /// any fix agents. Paired with subsequent fix steps.
    /// Reads `ctx.review_id` for the review task to validate.
    SetupFix,

    /// Create fix-parent, write fix plan, and run the plan-fix task.
    /// Short-circuits if the review has no actionable issues.
    /// Reads `ctx.review_id`, `ctx.scope`, and `ctx.assignee` from context.
    Fix,

    /// Regression review — re-review original scope after a fix cycle.
    RegressionReview,

    /// Test-only step variant for unit testing workflow machinery.
    #[cfg(test)]
    _Test {
        name: &'static str,
        section: Option<&'static str>,
        handler: std::sync::Arc<dyn Fn(&mut WorkflowContext) -> Result<StepResult> + Send + Sync>,
    },
}

pub(crate) fn downstream_review_steps() -> Vec<Step> {
    vec![Step::SetupReview, Step::Review, Step::RegressionReview]
}

/// Skip set that jumps straight to `RegressionReview`, bypassing the
/// intermediate fix/decompose/loop/review steps.
///
/// Used by both the Fix step (no actionable issues → short-circuit) and the
/// Decompose step (no subtasks created during fix decomposition). Because
/// `SkipSteps` only removes steps still in the queue, emitting the full set
/// from either call-site is safe: steps that already ran are absent from the
/// queue and silently ignored.
///
/// `RegressionReview` is deliberately kept — it handles the `task_id = None`
/// case as an immediate "approved" result.
pub(crate) fn fix_skip_to_regression_review() -> Vec<Step> {
    vec![Step::Decompose, Step::Loop, Step::SetupReview, Step::Review]
}

impl PartialEq for Step {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Step::SetupEpic, Step::SetupEpic) => true,
            (Step::Decompose, Step::Decompose) => true,
            (Step::Loop, Step::Loop) => true,
            (Step::SetupReview, Step::SetupReview) => true,
            (Step::Review, Step::Review) => true,
            (Step::SetupFix, Step::SetupFix) => true,
            (Step::Fix, Step::Fix) => true,
            (Step::RegressionReview, Step::RegressionReview) => true,
            #[cfg(test)]
            (Step::_Test { name: a, .. }, Step::_Test { name: b, .. }) => a == b,
            _ => false,
        }
    }
}

impl Step {
    pub fn name(&self) -> &'static str {
        match self {
            Step::SetupEpic => "setup epic",
            Step::Decompose => "decompose",
            Step::Loop => "loop",
            Step::SetupReview => "setup review",
            Step::Review => "review",
            Step::SetupFix => "setup fix",
            Step::Fix => "fix",
            Step::RegressionReview => "review for regressions",
            #[cfg(test)]
            Step::_Test { name, .. } => name,
        }
    }

    /// Section header for this step, if any.
    ///
    /// `iteration` is the current quality-loop iteration (0 = initial build).
    /// For the Decompose step this returns "Initial Build" on iteration 0
    /// and "Iteration N" for subsequent fix cycles.
    pub fn section(&self, iteration: usize) -> Option<String> {
        match self {
            Step::Decompose => {
                if iteration == 0 {
                    Some("Initial Build".to_string())
                } else {
                    Some(format!("Iteration {}", iteration))
                }
            }
            #[cfg(test)]
            Step::_Test { section, .. } => section.map(|s| s.to_string()),
            _ => None,
        }
    }

    pub fn run(&self, ctx: &mut WorkflowContext) -> Result<StepResult> {
        match self {
            Step::SetupEpic => setup_epic::run(ctx),

            Step::Decompose => decompose::run(ctx),

            Step::Loop => r#loop::run(ctx),

            Step::SetupReview => setup_review::run(ctx),

            Step::Review => review::run(ctx),

            Step::SetupFix => setup_fix::run(ctx),

            Step::Fix => fix::run(ctx),

            Step::RegressionReview => regression_review::run(ctx),

            #[cfg(test)]
            Step::_Test { handler, .. } => handler(ctx),
        }
    }
}

/// Run a task with optional TUI display.
pub(crate) fn run_task_with_show_tui(
    cwd: &Path,
    task_id: &str,
    options: TaskRunOptions,
    show_tui: bool,
) -> Result<()> {
    if show_tui {
        let result = task_run_on_session(cwd, task_id, options, true)?;
        handle_session_result(cwd, task_id, result, true)?;
    } else {
        task_run(cwd, task_id, options.quiet())?;
    }
    Ok(())
}

// ── Shared spawn-drain-finalize ─────────────────────────────────────

/// Step-specific processing of task-graph changes during the
/// spawn-drain-finalize loop.
///
/// Each step receives pre-computed `GraphDelta`s describing what changed in
/// the task graph since the last poll cycle. Implementations inspect the
/// delta (new tasks, status changes, new comments, new edges) instead of
/// consuming raw events from a channel.
pub(crate) trait DrainHandler {
    /// Called with a pre-computed delta when the task graph has changed.
    fn on_change(&mut self, delta: &GraphDelta);

    /// Called once after the agent exits and final drain completes.
    fn finish(&mut self) {}
}

/// Shared spawn-drain-finalize loop.
///
/// Spawns a monitored agent process, waits for FIFO notifications via
/// `recv_timeout` while the agent runs, then re-reads events fresh from JJ
/// on each cycle and computes a `GraphDelta` for the handler.
///
/// Flow per cycle:
/// 1. `recv_timeout(100ms)` — blocks until notification or timeout (doubles
///    as agent-exit polling interval).
/// 2. On notification: debounce with a 50ms quiet-period loop draining
///    further notifications.
/// 3. `read_events(cwd)` → `materialize_graph` → `compute_delta` →
///    `handler.on_change()`.
/// 4. After agent exits: 200ms silence window for final events.
pub(crate) fn spawn_drain_finalize(
    cwd: &Path,
    task_id: &str,
    run_options: &TaskRunOptions,
    notify_rx: Option<&Receiver<String>>,
    output: WorkflowOutput,
    handler: &mut dyn DrainHandler,
) -> crate::error::Result<()> {
    let prepared = prepare_task_run(cwd, task_id, run_options, |_| {})?;

    let mut agent_handle = match prepared.runtime.spawn_monitored(&prepared.spawn_options) {
        Ok(handle) => handle,
        Err(e) => {
            rollback_if_still_reserved(cwd, &prepared.task_id, &e);
            return Err(e);
        }
    };

    if let Some(rx) = notify_rx {
        use crate::tasks::graph::{compute_delta, materialize_graph};

        let mut prev_graph = if let Ok(events) = read_events(cwd) {
            materialize_graph(&events)
        } else {
            materialize_graph(&[])
        };

        /// Read events fresh from JJ, materialize, compute delta, and
        /// call the handler if anything changed.
        fn process_cycle(
            cwd: &Path,
            prev_graph: &mut crate::tasks::graph::TaskGraph,
            handler: &mut dyn DrainHandler,
        ) {
            if let Ok(events) = read_events(cwd) {
                let next_graph = materialize_graph(&events);
                let delta = compute_delta(prev_graph, &next_graph);
                if !delta.new_tasks.is_empty()
                    || !delta.status_changes.is_empty()
                    || !delta.new_comments.is_empty()
                    || !delta.new_edges.is_empty()
                {
                    handler.on_change(&delta);
                }
                *prev_graph = next_graph;
            }
        }

        // Main drain loop: wait for notifications via recv_timeout.
        while agent_handle
            .try_wait()
            .map_err(|e| AikiError::AgentSpawnFailed(format!("try_wait failed: {}", e)))?
            .is_none()
        {
            // Block until a notification arrives or 100ms elapses (serves
            // as agent status check interval).
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(_) => {
                    // Debounce: drain further notifications with a 50ms
                    // quiet-period window.
                    while rx.recv_timeout(Duration::from_millis(50)).is_ok() {}
                    process_cycle(cwd, &mut prev_graph, handler);
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    // No notification — loop back to check agent status.
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }

        // Tail drain: 200ms silence window after agent exits.
        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(remaining) {
                Ok(_) => {
                    // Keep draining notifications within the window.
                }
                Err(_) => {
                    break;
                }
            }
        }
        // Final read after tail drain.
        process_cycle(cwd, &mut prev_graph, handler);
        handler.finish();
    } else {
        // No event channel — still must wait for the agent to exit before
        // reading output and finalizing.
        agent_handle
            .wait()
            .map_err(|e| AikiError::AgentSpawnFailed(format!("wait failed: {}", e)))?;
    }

    // Agent stderr is discarded — interactive agents (Claude Code, Codex) write
    // their entire TUI output to stderr, which is noise in workflow context.
    // Task success/failure is determined by JJ state in finalize_agent_run.

    finalize_agent_run(cwd, task_id)?;

    Ok(())
}

/// Drain handler for steps that track subtask creation under a parent task.
///
/// Scans the full graph state (`delta.next`) for tasks with a `subtask-of`
/// edge targeting the parent, using a `seen` set to avoid duplicate output.
/// This avoids cross-window races where `Created` and `LinkAdded(subtask-of)`
/// land in different debounce windows. Used by the decompose and fix steps.
pub(crate) struct SubtaskDrainHandler<'a> {
    seen: HashSet<String>,
    task_names: &'a mut HashMap<String, String>,
    parent_id: String,
    output: WorkflowOutput,
}

impl<'a> SubtaskDrainHandler<'a> {
    pub fn new(
        task_names: &'a mut HashMap<String, String>,
        parent_id: String,
        output: WorkflowOutput,
    ) -> Self {
        Self {
            seen: HashSet::new(),
            task_names,
            parent_id,
            output,
        }
    }
}

impl DrainHandler for SubtaskDrainHandler<'_> {
    fn on_change(&mut self, delta: &GraphDelta) {
        // Scan the full graph for subtasks of the parent that we haven't
        // printed yet. This handles the case where Created and
        // LinkAdded(subtask-of) land in different debounce windows.
        for child in delta.next.children_of(&self.parent_id) {
            if self.seen.insert(child.id.clone()) {
                self.task_names
                    .insert(child.id.clone(), child.name.clone());
                self.output.emit(&format!("  + {}", child.name));
            }
        }
    }
}

/// Drain handler for review steps that emits issues as they are found.
///
/// Watches for issue comments (`data.issue == "true"`) targeting the review task
/// and prints each one with its severity. Used by the review and regression_review steps.
pub(crate) struct ReviewDrainHandler {
    review_id: String,
    issue_count: usize,
    output: WorkflowOutput,
}

impl ReviewDrainHandler {
    pub fn new(review_id: String, output: WorkflowOutput) -> Self {
        Self {
            review_id,
            issue_count: 0,
            output,
        }
    }
}

impl DrainHandler for ReviewDrainHandler {
    fn on_change(&mut self, delta: &GraphDelta) {
        for &(task_id, comment) in &delta.new_comments {
            if task_id == self.review_id
                && comment
                    .data
                    .get("issue")
                    .map(|v| v == "true")
                    .unwrap_or(false)
            {
                self.issue_count += 1;
                let severity = comment
                    .data
                    .get("severity")
                    .map(|s| s.as_str())
                    .unwrap_or("medium");
                // Truncate long descriptions to keep output scannable
                let desc = &comment.text;
                let short = if desc.len() > 80 {
                    format!("{}...", &desc[..77])
                } else {
                    desc.to_string()
                };
                self.output
                    .emit(&format!("  [{}] {}", severity, short));
            }
        }
    }
}
