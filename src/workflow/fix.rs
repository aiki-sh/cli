use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::async_run;
use super::steps::Step;
use super::{OutputKind, RunKind, Workflow, WorkflowContext, WorkflowOpts, WorkflowOutput};
use crate::agents::AgentType;
use crate::commands::fix::FixArgs;
use crate::commands::task::{create_from_template, TemplateTaskParams};
use crate::commands::OutputFormat;
use crate::error::{AikiError, Result};
use crate::reviews::{
    create_review, determine_followup_assignee, get_issue_comments, has_actionable_issues,
    issue_count, parse_locations, resolve_scope_and_assignee, CreateReviewParams, ReviewScope,
    ReviewScopeKind,
};
use crate::tasks::lanes::ThreadId;
use crate::tasks::runner::{task_run, TaskRunOptions};
use crate::tasks::{
    find_task, materialize_graph_with_ids, read_events_with_ids, start_task_core,
};

/// Options for assembling and running a fix workflow.
///
/// Contains only dispatch-level fields. Workflow-level options live in `workflow`.
#[derive(Clone, Serialize, Deserialize)]
pub struct FixOpts {
    pub review_id: String,
    pub run_kind: RunKind,
    pub output: Option<OutputFormat>,
    pub once: bool,
    pub pair: bool,
    /// Fix-parent task ID when continuing an async fix (`--_continue-async`).
    pub continue_async_id: Option<String>,
    pub workflow: WorkflowOpts,
}

impl FixOpts {
    /// Build `FixOpts` from CLI args, resolving flag combinations in one place.
    pub fn from_args(args: &FixArgs, review_id: String) -> crate::error::Result<Self> {
        let run_kind = RunKind::from_args(args);

        Ok(FixOpts {
            review_id: review_id.clone(),
            run_kind,
            output: args.output.clone(),
            once: args.once,
            pair: args.pair,
            continue_async_id: args.continue_async.clone(),
            workflow: WorkflowOpts {
                plan_template: args.template.clone(),
                decompose_template: args.decompose_template.clone(),
                loop_template: args.loop_template.clone(),
                review_template: args.review_template.clone(),
                coder: args.agent.clone(),
                autorun: args.autorun,
                ..WorkflowOpts::default()
            },
        })
    }

    /// Build `FixOpts` from workflow-level fix settings used by build/review.
    ///
    /// This keeps forwarded fix-loop configuration aligned with `from_args`,
    /// while mapping `fix_template` onto the fix workflow's `plan_template`.
    pub fn from_workflow(
        review_id: String,
        output: Option<OutputFormat>,
        workflow: &WorkflowOpts,
    ) -> Self {
        FixOpts {
            review_id,
            run_kind: RunKind::Foreground,
            output,
            once: false,
            pair: false,
            continue_async_id: None,
            workflow: WorkflowOpts {
                plan_template: workflow.fix_template.clone(),
                decompose_template: workflow.decompose_template.clone(),
                loop_template: workflow.loop_template.clone(),
                review_template: workflow.review_template.clone(),
                reviewer: workflow.reviewer.clone(),
                coder: workflow.coder.clone(),
                autorun: workflow.autorun,
                ..WorkflowOpts::default()
            },
        }
    }
}

/// Build a full fix workflow assembly for architecture tests and future drivers.
pub(crate) fn workflow(
    cwd: &Path,
    review_id: &str,
    opts: &FixOpts,
    scope: &ReviewScope,
    assignee: Option<AgentType>,
) -> Workflow {
    let steps = if opts.once {
        vec![Step::Fix, Step::Decompose, Step::Loop]
    } else {
        vec![
            Step::Fix,
            Step::Decompose,
            Step::Loop,
            Step::SetupReview,
            Step::Review,
            Step::RegressionReview,
        ]
    };

    let mut workflow_opts = opts.workflow.clone();
    workflow_opts.agent = assignee.clone();
    workflow_opts.coder = assignee.as_ref().map(|a| a.as_str().to_string());

    Workflow {
        steps,
        ctx: WorkflowContext {
            task_id: None,
            plan_path: None,
            cwd: cwd.to_path_buf(),
            output: WorkflowOutput::new(OutputKind::Text),
            opts: workflow_opts,
            review_id: Some(review_id.to_string()),
            scope: Some(scope.clone()),
            assignee: assignee.as_ref().map(|a| a.as_str().to_string()),
            iteration: 0,
            notify_rx: None,
            task_names: std::collections::HashMap::new(),
        },
    }
}

/// Assemble a fix workflow with SetupFix as the first step.
///
/// Used by `run()` for `SetupOnly` and `Async` run kinds where the first step
/// (validation + fix-parent creation) runs synchronously before handing off.
fn setup_workflow(cwd: &Path, opts: &FixOpts) -> Workflow {
    Workflow {
        steps: vec![Step::SetupFix],
        ctx: WorkflowContext {
            task_id: None,
            plan_path: None,
            cwd: cwd.to_path_buf(),
            output: WorkflowOutput::new(OutputKind::Text),
            opts: opts.workflow.clone(),
            review_id: Some(opts.review_id.clone()),
            scope: None,
            assignee: None,
            iteration: 0,
            notify_rx: None,
            task_names: std::collections::HashMap::new(),
        },
    }
}

/// Run a fix workflow.
///
/// Entry point that mirrors `build::run()` and `review::run()`. Dispatches
/// on `RunKind` to handle setup-only, async, continue-async, and foreground modes.
pub fn run(cwd: &Path, opts: &FixOpts) -> Result<WorkflowContext> {
    if opts.pair {
        return run_pair(cwd, opts);
    }
    match opts.run_kind {
        RunKind::SetupOnly => run_setup_only(cwd, opts),
        RunKind::Async => run_async(cwd, opts),
        RunKind::ContinueAsync => run_continue_async(cwd, opts),
        RunKind::Foreground => run_foreground(cwd, opts),
    }
}

/// Run SetupFix only, return the fix-parent ID to the caller.
fn run_setup_only(cwd: &Path, opts: &FixOpts) -> Result<WorkflowContext> {
    let wf = setup_workflow(cwd, opts);
    let ctx = wf.run_first_step().map_err(AikiError::Other)?;

    if let Some(fix_parent_id) = ctx.task_id() {
        match opts.output {
            Some(OutputFormat::Id) => println!("{}", fix_parent_id),
            None => eprintln!("Fix: {}", fix_parent_id),
        }
    }
    Ok(ctx)
}

/// Run SetupFix synchronously, then spawn remaining steps in background.
fn run_async(cwd: &Path, opts: &FixOpts) -> Result<WorkflowContext> {
    let wf = setup_workflow(cwd, opts);
    let ctx = wf.run_first_step().map_err(AikiError::Other)?;
    let Some(fix_parent_id) = ctx.task_id() else {
        return Ok(ctx);
    };

    let mut continue_opts = opts.clone();
    continue_opts.continue_async_id = Some(fix_parent_id.to_string());
    async_run::spawn_continue(
        &ctx.cwd,
        &["fix", &opts.review_id, "--_continue-async", fix_parent_id],
        &continue_opts,
    )?;

    match opts.output {
        Some(OutputFormat::Id) => println!("{}", fix_parent_id),
        None => eprintln!("Fix: {}", fix_parent_id),
    }
    Ok(ctx)
}

/// Continue a previously started async fix silently.
fn run_continue_async(cwd: &Path, cli_opts: &FixOpts) -> Result<WorkflowContext> {
    let opts: FixOpts = async_run::read_continue_opts()?;

    if !cli_opts.review_id.is_empty() && opts.review_id != cli_opts.review_id {
        return Err(AikiError::InvalidArgument(format!(
            "stdin target does not match CLI review ID: expected {}, got {}",
            opts.review_id, cli_opts.review_id,
        )));
    }

    // Load the fix-parent task to recover real scope and assignee.
    let fix_parent_id = opts.continue_async_id.as_deref().ok_or_else(|| {
        AikiError::InvalidArgument("ContinueAsync requires a fix-parent task ID".to_string())
    })?;
    let events_with_ids = read_events_with_ids(cwd)?;
    let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
    let fix_parent = find_task(&tasks, fix_parent_id)?;

    let scope = ReviewScope::from_data(&fix_parent.data)?;
    let agent_type = opts
        .workflow
        .coder
        .as_deref()
        .map(AgentType::parse);
    let assignee = determine_followup_assignee(agent_type, Some(fix_parent), None, None).ok();

    let assignee_type = assignee.as_deref().map(AgentType::parse);
    let wf = continue_async_workflow(cwd, &opts, &scope, assignee_type, fix_parent_id);
    wf.run().map_err(AikiError::Other)
}

fn continue_async_workflow(
    cwd: &Path,
    opts: &FixOpts,
    scope: &ReviewScope,
    assignee: Option<AgentType>,
    fix_parent_id: &str,
) -> Workflow {
    let mut wf = workflow(cwd, &opts.review_id, opts, scope, assignee);
    wf.ctx.task_id = Some(fix_parent_id.to_string());
    wf.ctx.output = WorkflowOutput::new(OutputKind::Quiet);
    wf
}

/// Run the full fix workflow in foreground with TUI output.
fn run_foreground(cwd: &Path, opts: &FixOpts) -> Result<WorkflowContext> {
    let (scope, assignee) =
        resolve_scope_and_assignee(cwd, &opts.review_id, opts.workflow.coder.as_deref())?;
    let agent_type = super::steps::fix::resolve_fix_agent(
        opts.workflow.coder.as_deref(),
        assignee.as_deref(),
        false, // foreground = autonomous
    )?;
    let show_tui = io::stderr().is_terminal();
    let output = if show_tui {
        OutputKind::Text
    } else {
        OutputKind::Quiet
    };
    let mut wf = workflow(cwd, &opts.review_id, opts, &scope, Some(agent_type));
    wf.ctx.output = WorkflowOutput::new(output);
    let ctx = wf.run().map_err(AikiError::Other)?;
    if show_tui {
        let short_review = &opts.review_id[..7.min(opts.review_id.len())];
        let short_scope = &scope.id[..7.min(scope.id.len())];
        if has_remaining_issues(cwd, &opts.review_id) {
            eprintln!("\nRun `aiki fix {} --pair` to interactively remediate remaining issues.\n", short_review);
        } else {
            eprintln!("\nRun `aiki tldr {}` for a walkthrough.\n", short_scope);
        }
    }
    Ok(ctx)
}

/// Check whether a review still has actionable issues after fix.
pub(crate) fn has_remaining_issues(cwd: &Path, review_id: &str) -> bool {
    let Ok(events_with_ids) = read_events_with_ids(cwd) else {
        return false;
    };
    let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
    let Ok(review_task) = find_task(&tasks, review_id) else {
        return false;
    };
    has_actionable_issues(review_task)
}

fn run_pair(cwd: &Path, opts: &FixOpts) -> Result<WorkflowContext> {
    // 1. Resolve scope and assignee
    let (scope, assignee) =
        resolve_scope_and_assignee(cwd, &opts.review_id, opts.workflow.coder.as_deref())?;

    // 2. Load review task and validate it has actionable issues
    let events_with_ids = read_events_with_ids(cwd)?;
    let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
    let review_task = find_task(&tasks, &opts.review_id)?;
    if !has_actionable_issues(review_task) {
        eprintln!("No actionable issues to fix.");
        return Ok(WorkflowContext {
            task_id: None,
            plan_path: None,
            cwd: cwd.to_path_buf(),
            output: WorkflowOutput::new(OutputKind::Text),
            opts: opts.workflow.clone(),
            review_id: Some(opts.review_id.clone()),
            scope: Some(scope),
            assignee,
            iteration: 0,
            notify_rx: None,
            task_names: std::collections::HashMap::new(),
        });
    }

    // 3. Create fix-parent task
    let autorun = opts.workflow.autorun;
    let fix_parent_id = super::steps::fix::create_fix_parent(
        cwd,
        &opts.review_id,
        &scope,
        &assignee,
        autorun,
    )?;

    // 4. Resolve launch agent binary.
    let agent_type = super::steps::fix::resolve_fix_agent(
        opts.workflow.coder.as_deref(),
        assignee.as_deref(),
        true, // pair = interactive
    )?;
    let binary = agent_type.cli_binary().ok_or_else(|| {
        AikiError::InvalidArgument(format!(
            "Agent '{}' does not support interactive sessions. {}",
            agent_type.as_str(),
            agent_type.install_hint()
        ))
    })?;
    if !agent_type.is_installed() {
        return Err(AikiError::InvalidArgument(format!(
            "Agent '{}' is not installed. {}",
            agent_type.as_str(),
            agent_type.install_hint()
        )));
    }

    // 5. Two-phase quality loop
    let output = WorkflowOutput::new(OutputKind::Text);
    let mut current_review_id = opts.review_id.clone();
    for iteration in 0..MAX_QUALITY_ITERATIONS {
        // ── Pair session ──
        output.section(&format!("Pair Fix — iteration {}", iteration + 1));
        show_review_issues(cwd, &current_review_id, output)?;

        let issues_md = build_issues_md(cwd, &current_review_id)?;
        let pair_fix_id = create_pair_fix_task(
            cwd,
            &issues_md,
            &current_review_id,
            &fix_parent_id,
            &scope,
            iteration + 1,
        )?;
        start_task_core(cwd, &[pair_fix_id.clone()])?;

        let prompt = format!(
            "You are assigned thread `{}`. Start by running `aiki task start {}`.",
            pair_fix_id, pair_fix_id
        );

        let thread = ThreadId::single(pair_fix_id.clone());

        // Use .status() to inherit all stdio — the interactive TUI needs
        // stdout connected to the real terminal to detect TTY mode.
        let status = Command::new(binary)
            .current_dir(cwd)
            .env("AIKI_THREAD", &thread.serialize())
            .arg(&prompt)
            .status()
            .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to spawn agent: {}", e)))?;

        // User cancelled (SIGINT)
        if !status.success() && status.code() == Some(130) {
            break;
        }

        // Show pair session summary
        show_pair_session_summary(cwd, &pair_fix_id, output)?;

        if opts.once {
            break;
        }

        // ── Phase 1: Review fix-parent ──
        output.status("running fix-parent review");
        let fp_scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: fix_parent_id.clone(),
            task_ids: vec![],
        };
        let fp_review_result = create_review(
            cwd,
            CreateReviewParams {
                scope: fp_scope,
                agent_override: opts.workflow.reviewer.clone(),
                template: opts.workflow.review_template.clone(),
                fix_template: None,
                autorun: false,
            },
        )?;
        let fp_review_id = fp_review_result.review_task_id;
        let run_options = TaskRunOptions::new().quiet();
        task_run(cwd, &fp_review_id, run_options)?;

        // Reload and check results
        let events_with_ids = read_events_with_ids(cwd)?;
        let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
        let fp_review = find_task(&tasks, &fp_review_id)?;
        let fp_has_issues = has_actionable_issues(fp_review);

        let ic = issue_count(fp_review);
        if ic > 0 {
            output.success("review", &format!("{} issues found", ic));
        } else {
            output.success("review", "approved");
        }

        let outcome = determine_review_outcome(fp_has_issues, &fp_review_id, None, None);

        match outcome {
            ReviewOutcome::LoopBack(id) => {
                output.status("issues found — starting next fix iteration");
                show_review_issues(cwd, &id, output)?;
                current_review_id = id;
                continue;
            }
            ReviewOutcome::ReReviewOriginalScope => {
                // ── Phase 2: Regression review ──
                output.section("Regression Review");
                output.status("reviewing original scope for regressions");
                let reg_review_result = create_review(
                    cwd,
                    CreateReviewParams {
                        scope: scope.clone(),
                        agent_override: opts.workflow.reviewer.clone(),
                        template: opts.workflow.review_template.clone(),
                        fix_template: None,
                        autorun: false,
                    },
                )?;
                let reg_review_id = reg_review_result.review_task_id;
                let run_options = TaskRunOptions::new().quiet();
                task_run(cwd, &reg_review_id, run_options)?;

                let events_with_ids = read_events_with_ids(cwd)?;
                let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
                let reg_review = find_task(&tasks, &reg_review_id)?;
                let reg_has_issues = has_actionable_issues(reg_review);

                let reg_ic = issue_count(reg_review);
                if reg_ic > 0 {
                    output.success("regression review", &format!("{} issues found", reg_ic));
                } else {
                    output.success("regression review", "approved");
                }

                let reg_outcome = determine_review_outcome(
                    false,
                    &fp_review_id,
                    Some(reg_has_issues),
                    Some(&reg_review_id),
                );

                match reg_outcome {
                    ReviewOutcome::Approved(_) => {
                        output.success("quality loop", "all reviews passed");
                        break;
                    }
                    ReviewOutcome::LoopBack(id) => {
                        output.status("regressions found — starting next fix iteration");
                        show_review_issues(cwd, &id, output)?;
                        current_review_id = id;
                        continue;
                    }
                    _ => break,
                }
            }
            _ => unreachable!(),
        }
    }

    Ok(WorkflowContext {
        task_id: Some(fix_parent_id),
        plan_path: None,
        cwd: cwd.to_path_buf(),
        output: WorkflowOutput::new(OutputKind::Text),
        opts: opts.workflow.clone(),
        review_id: Some(opts.review_id.clone()),
        scope: Some(scope),
        assignee,
        iteration: 0,
        notify_rx: None,
        task_names: std::collections::HashMap::new(),
    })
}

/// Show review issues before spawning the pair fix agent.
/// Follows the same format as `aiki review show`.
fn show_review_issues(cwd: &Path, review_id: &str, output: WorkflowOutput) -> Result<()> {
    let events_with_ids = read_events_with_ids(cwd)?;
    let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
    let review_task = find_task(&tasks, review_id)?;
    let issues = get_issue_comments(review_task);

    for issue in &issues {
        let severity = issue
            .data
            .get("severity")
            .map(|s| s.as_str())
            .unwrap_or("medium");
        output.emit(&format!("  [{}] {}", severity, issue.text));
    }

    Ok(())
}

/// Show a summary of the completed pair fix session.
fn show_pair_session_summary(
    cwd: &Path,
    pair_fix_id: &str,
    output: WorkflowOutput,
) -> Result<()> {
    let events_with_ids = read_events_with_ids(cwd)?;
    let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
    if let Ok(pair_task) = find_task(&tasks, pair_fix_id) {
        let msg = if pair_task.closed_outcome.is_some() {
            pair_task
                .summary
                .as_deref()
                .unwrap_or("session complete")
        } else {
            "session ended"
        };
        output.success("pair fix", msg);
    } else {
        output.success("pair fix", "session ended");
    }
    Ok(())
}

/// Build a markdown string of issues sorted by severity (high -> medium -> low).
fn build_issues_md(cwd: &Path, review_id: &str) -> Result<String> {
    let events_with_ids = read_events_with_ids(cwd)?;
    let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
    let review_task = find_task(&tasks, review_id)?;
    let issues = get_issue_comments(review_task);

    // Sort by severity: high -> medium -> low
    let severity_order = |comment: &&crate::tasks::types::TaskComment| -> u8 {
        match comment.data.get("severity").map(|s| s.as_str()) {
            Some("high") => 0,
            Some("medium") => 1,
            Some("low") => 2,
            _ => 1, // default to medium
        }
    };

    let mut sorted_issues = issues;
    sorted_issues.sort_by_key(severity_order);

    let mut md = String::new();
    for (i, issue) in sorted_issues.iter().enumerate() {
        let severity = issue
            .data
            .get("severity")
            .map(|s| s.as_str())
            .unwrap_or("medium");
        let description = &issue.text;
        md.push_str(&format!("### {}. {} [{}]\n", i + 1, description, severity));

        let locations = parse_locations(&issue.data);
        for loc in &locations {
            md.push_str(&format!("- **File**: {}\n", loc));
        }
        md.push('\n');
    }

    Ok(md)
}

/// Create a pair-fix task from the `pair-fix` template.
fn create_pair_fix_task(
    cwd: &Path,
    issues_md: &str,
    review_id: &str,
    fix_parent_id: &str,
    scope: &ReviewScope,
    iteration: usize,
) -> Result<String> {
    let mut data = scope.to_data();
    data.insert("review".to_string(), review_id.to_string());
    data.insert("issues_md".to_string(), issues_md.to_string());
    data.insert("iteration".to_string(), iteration.to_string());

    let params = TemplateTaskParams {
        template_name: "pair-fix".to_string(),
        data,
        sources: vec![format!("task:{}", review_id)],
        assignee: None,
        parent_id: Some(fix_parent_id.to_string()),
        ..Default::default()
    };

    create_from_template(cwd, params)
}

/// Maximum iterations of the quality loop to prevent infinite cycles.
pub(crate) const MAX_QUALITY_ITERATIONS: usize = 10;

/// Outcome of the two-phase review decision.
#[derive(Debug, PartialEq)]
pub(crate) enum ReviewOutcome {
    /// Fix-parent review has issues — loop back with this review ID
    LoopBack(String),
    /// Fix-parent review passed, original re-review also passed — approved
    Approved(String),
    /// Fix-parent review passed — need to re-review original scope
    ReReviewOriginalScope,
}

/// Determine the next action after a fix-parent review completes.
///
/// This is the pure decision logic for testability.
pub(crate) fn determine_review_outcome(
    fix_parent_review_has_issues: bool,
    fix_parent_review_id: &str,
    original_review_has_issues: Option<bool>,
    original_review_id: Option<&str>,
) -> ReviewOutcome {
    if fix_parent_review_has_issues {
        return ReviewOutcome::LoopBack(fix_parent_review_id.to_string());
    }
    // Fix-parent passed — check original scope
    match (original_review_has_issues, original_review_id) {
        (Some(false), Some(id)) => ReviewOutcome::Approved(id.to_string()),
        (Some(true), Some(id)) => ReviewOutcome::LoopBack(id.to_string()),
        _ => ReviewOutcome::ReReviewOriginalScope,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{write_event, TaskEvent, TaskPriority};
    use chrono::Utc;
    use tempfile::tempdir;

    fn init_jj_repo(path: &std::path::Path) {
        let git = std::process::Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("initialize git repo");
        assert!(
            git.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&git.stderr)
        );

        let jj = std::process::Command::new("jj")
            .args(["git", "init", "--colocate"])
            .current_dir(path)
            .output()
            .expect("initialize jj repo");
        assert!(
            jj.status.success(),
            "jj git init failed: {}",
            String::from_utf8_lossy(&jj.stderr)
        );
    }

    #[test]
    fn test_review_outcome_loopback_when_fix_parent_has_issues() {
        let outcome = determine_review_outcome(true, "review1", None, None);
        assert_eq!(outcome, ReviewOutcome::LoopBack("review1".to_string()));
    }

    #[test]
    fn test_review_outcome_loopback_when_fix_parent_has_issues_ignores_original() {
        let outcome = determine_review_outcome(true, "review1", Some(false), Some("orig1"));
        assert_eq!(outcome, ReviewOutcome::LoopBack("review1".to_string()));
    }

    #[test]
    fn test_review_outcome_re_review_when_no_original_info() {
        let outcome = determine_review_outcome(false, "review1", None, None);
        assert_eq!(outcome, ReviewOutcome::ReReviewOriginalScope);
    }

    #[test]
    fn test_review_outcome_approved_when_original_passes() {
        let outcome = determine_review_outcome(false, "review1", Some(false), Some("orig1"));
        assert_eq!(outcome, ReviewOutcome::Approved("orig1".to_string()));
    }

    #[test]
    fn test_review_outcome_loopback_when_original_has_issues() {
        let outcome = determine_review_outcome(false, "review1", Some(true), Some("orig1"));
        assert_eq!(outcome, ReviewOutcome::LoopBack("orig1".to_string()));
    }

    #[test]
    fn test_review_outcome_consecutive_loop_backs() {
        for i in 0..MAX_QUALITY_ITERATIONS {
            let review_id = format!("review-iter-{}", i);
            let outcome = determine_review_outcome(true, &review_id, None, None);
            assert_eq!(outcome, ReviewOutcome::LoopBack(review_id));
        }
    }

    #[test]
    fn test_review_outcome_approval_breaks_loop() {
        for i in 0..3 {
            let review_id = format!("review-iter-{}", i);
            let outcome = determine_review_outcome(true, &review_id, None, None);
            assert_eq!(outcome, ReviewOutcome::LoopBack(review_id));
        }
        let outcome =
            determine_review_outcome(false, "fix-review-final", Some(false), Some("orig-final"));
        assert_eq!(outcome, ReviewOutcome::Approved("orig-final".to_string()));
    }

    #[test]
    fn test_max_quality_iterations_value() {
        assert!(MAX_QUALITY_ITERATIONS > 0);
        assert!(MAX_QUALITY_ITERATIONS <= 100);
    }

    #[test]
    fn test_max_quality_iterations_is_ten() {
        assert_eq!(
            MAX_QUALITY_ITERATIONS, 10,
            "MAX_QUALITY_ITERATIONS must be 10"
        );
    }

    #[test]
    fn test_two_phase_fix_parent_fails_always_loops_back() {
        for original_has_issues in [None, Some(true), Some(false)] {
            let outcome =
                determine_review_outcome(true, "fix-review", original_has_issues, Some("orig"));
            assert!(
                matches!(outcome, ReviewOutcome::LoopBack(ref id) if id == "fix-review"),
                "Fix-parent failure must always loop back, original_has_issues={:?}",
                original_has_issues
            );
        }
    }

    #[test]
    fn test_two_phase_fix_passes_triggers_original_review() {
        let outcome = determine_review_outcome(false, "fix-review", None, None);
        assert_eq!(outcome, ReviewOutcome::ReReviewOriginalScope);
    }

    #[test]
    fn test_two_phase_both_pass_approved() {
        let outcome = determine_review_outcome(false, "fix-review", Some(false), Some("orig"));
        assert_eq!(outcome, ReviewOutcome::Approved("orig".to_string()));
    }

    #[test]
    fn test_two_phase_original_fails_loops_back_with_original_id() {
        let outcome = determine_review_outcome(false, "fix-review", Some(true), Some("orig"));
        assert_eq!(outcome, ReviewOutcome::LoopBack("orig".to_string()));
    }

    #[test]
    fn test_quality_loop_bounded_by_max_iterations() {
        let mut review_ids: Vec<String> = Vec::new();
        for i in 0..MAX_QUALITY_ITERATIONS {
            let review_id = format!("review-iter-{}", i);
            let outcome = determine_review_outcome(true, &review_id, None, None);
            match outcome {
                ReviewOutcome::LoopBack(id) => review_ids.push(id),
                _ => panic!("Expected LoopBack on iteration {}", i),
            }
        }
        assert_eq!(review_ids.len(), MAX_QUALITY_ITERATIONS);
    }

    #[test]
    fn test_review_outcome_re_review_when_only_has_issues_none() {
        let outcome = determine_review_outcome(false, "review1", None, Some("orig1"));
        assert_eq!(outcome, ReviewOutcome::ReReviewOriginalScope);
    }

    #[test]
    fn test_review_outcome_re_review_when_only_id_none() {
        let outcome = determine_review_outcome(false, "review1", Some(false), None);
        assert_eq!(outcome, ReviewOutcome::ReReviewOriginalScope);
    }

    #[test]
    fn workflow_includes_live_fix_review_sequence() {
        let opts = FixOpts {
            review_id: "review123".to_string(),
            run_kind: RunKind::Foreground,
            output: None,
            once: false,
            pair: false,
            continue_async_id: None,
            workflow: WorkflowOpts::default(),
        };
        let scope = ReviewScope {
            kind: crate::reviews::ReviewScopeKind::Task,
            id: "target123".to_string(),
            task_ids: vec![],
        };

        let workflow = workflow(
            Path::new("."),
            &opts.review_id,
            &opts,
            &scope,
            Some(AgentType::Codex),
        );

        assert_eq!(workflow.steps.len(), 6);
        assert!(matches!(
            workflow.steps.as_slice(),
            [
                Step::Fix,
                Step::Decompose,
                Step::Loop,
                Step::SetupReview,
                Step::Review,
                Step::RegressionReview,
            ]
        ));
    }

    #[test]
    fn once_workflow_stops_before_review_sequence() {
        let opts = FixOpts {
            review_id: "review123".to_string(),
            run_kind: RunKind::Foreground,
            output: None,
            once: true,
            pair: false,
            continue_async_id: None,
            workflow: WorkflowOpts::default(),
        };
        let scope = ReviewScope {
            kind: crate::reviews::ReviewScopeKind::Task,
            id: "target123".to_string(),
            task_ids: vec![],
        };

        let workflow = workflow(Path::new("."), &opts.review_id, &opts, &scope, None);

        assert_eq!(workflow.steps.len(), 3);
        assert!(matches!(
            workflow.steps.as_slice(),
            [Step::Fix, Step::Decompose, Step::Loop,]
        ));
    }

    // ── 7a: JSON serialization roundtrip tests ──

    #[test]
    fn fix_opts_json_roundtrip_preserves_all_fields() {
        let opts = FixOpts {
            review_id: "review456".to_string(),
            run_kind: RunKind::ContinueAsync,
            output: Some(OutputFormat::Id),
            once: true,
            pair: false,
            continue_async_id: Some("fixparent789".to_string()),
            workflow: WorkflowOpts {
                plan_template: Some("fix".to_string()),
                decompose_template: Some("decompose".to_string()),
                loop_template: Some("loop".to_string()),
                review_template: Some("review/task".to_string()),
                coder: Some("codex".to_string()),
                autorun: true,
                ..WorkflowOpts::default()
            },
        };
        let json = serde_json::to_string(&opts).unwrap();
        let restored: FixOpts = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.review_id, "review456");
        assert!(restored.run_kind == RunKind::ContinueAsync);
        assert!(restored.once);
        assert_eq!(restored.continue_async_id, Some("fixparent789".to_string()));
        assert_eq!(restored.workflow.plan_template, Some("fix".to_string()));
        assert_eq!(
            restored.workflow.decompose_template,
            Some("decompose".to_string())
        );
        assert_eq!(restored.workflow.loop_template, Some("loop".to_string()));
        assert_eq!(
            restored.workflow.review_template,
            Some("review/task".to_string())
        );
        assert_eq!(restored.workflow.coder, Some("codex".to_string()));
        assert!(restored.workflow.autorun);
    }

    #[test]
    fn fix_opts_from_workflow_forwards_shared_fix_settings() {
        let opts = FixOpts::from_workflow(
            "review123".to_string(),
            Some(OutputFormat::Id),
            &WorkflowOpts {
                fix_template: Some("custom-fix".to_string()),
                decompose_template: Some("custom-decompose".to_string()),
                loop_template: Some("custom-loop".to_string()),
                review_template: Some("custom-review".to_string()),
                reviewer: Some("reviewer".to_string()),
                coder: Some("codex".to_string()),
                autorun: true,
                ..WorkflowOpts::default()
            },
        );

        assert_eq!(opts.review_id, "review123");
        assert_eq!(opts.output, Some(OutputFormat::Id));
        assert_eq!(opts.workflow.plan_template, Some("custom-fix".to_string()));
        assert_eq!(
            opts.workflow.decompose_template,
            Some("custom-decompose".to_string())
        );
        assert_eq!(opts.workflow.loop_template, Some("custom-loop".to_string()));
        assert_eq!(
            opts.workflow.review_template,
            Some("custom-review".to_string())
        );
        assert_eq!(opts.workflow.reviewer, Some("reviewer".to_string()));
        assert_eq!(opts.workflow.coder, Some("codex".to_string()));
        assert!(opts.workflow.autorun);
    }

    #[test]
    fn async_fix_returns_success_when_setup_short_circuits() {
        let temp_dir = tempdir().unwrap();
        init_jj_repo(temp_dir.path());

        let review_id = "review-approved-noop";
        let mut data = std::collections::HashMap::new();
        data.insert("issue_count".to_string(), "0".to_string());
        write_event(
            temp_dir.path(),
            &TaskEvent::Created {
                task_id: review_id.to_string(),
                name: "Review: already approved".to_string(),
                slug: None,
                task_type: Some("review".to_string()),
                priority: TaskPriority::P2,
                assignee: Some("codex".to_string()),
                sources: vec![],
                template: Some("review/task".to_string()),
                instructions: None,
                data,
                timestamp: Utc::now(),
            },
        )
        .unwrap();

        let opts = FixOpts {
            review_id: review_id.to_string(),
            run_kind: RunKind::Async,
            output: None,
            once: false,
            pair: false,
            continue_async_id: None,
            workflow: WorkflowOpts::default(),
        };

        let ctx = run_async(temp_dir.path(), &opts).unwrap();

        assert!(ctx.task_id().is_none());
    }

    #[test]
    fn continue_async_workflow_preserves_existing_fix_parent_id() {
        let scope = ReviewScope {
            kind: crate::reviews::ReviewScopeKind::Task,
            id: "original-task".to_string(),
            task_ids: vec![],
        };
        let opts = FixOpts {
            review_id: "review123".to_string(),
            run_kind: RunKind::ContinueAsync,
            output: None,
            once: false,
            pair: false,
            continue_async_id: Some("fixparent789".to_string()),
            workflow: WorkflowOpts::default(),
        };

        let wf =
            continue_async_workflow(Path::new("."), &opts, &scope, Some(AgentType::Codex), "fixparent789");

        assert_eq!(wf.ctx.task_id.as_deref(), Some("fixparent789"));
        assert!(matches!(wf.ctx.output.kind(), OutputKind::Quiet));
    }
}
