use std::path::Path;

use serde::{Deserialize, Serialize};

use super::async_run;
use super::fix::{self, FixOpts};
use crate::commands::review::ReviewArgs as ReviewCommandArgs;
use crate::commands::OutputFormat;
use crate::error::AikiError;
use crate::reviews::resolve_scope_and_assignee;
use crate::session::find_active_session;
use crate::tasks::{reassign_task, start_task_core};

use super::steps::Step;
use super::{OutputKind, RunKind, Workflow, WorkflowContext, WorkflowOpts, WorkflowOutput};

/// Options for assembling and running a review workflow.
///
/// Contains only dispatch-level fields. Workflow-level options live in `workflow`.
#[derive(Clone, Serialize, Deserialize)]
pub struct ReviewOpts {
    /// Dispatch-level review target.
    ///
    /// This intentionally duplicates `workflow.target`: the CLI dispatcher uses
    /// it to validate `--_continue-async` payloads before we build a workflow,
    /// while `SetupReview` reads the workflow-level target when assembling the
    /// steps for the synchronous setup phase.
    pub target: Option<String>,
    pub run_kind: RunKind,
    pub output: Option<OutputFormat>,
    pub workflow: WorkflowOpts,
}

impl ReviewOpts {
    /// Build `ReviewOpts` from CLI args, resolving flag combinations in one place.
    pub fn from_args(args: &ReviewCommandArgs) -> crate::error::Result<Self> {
        let fix_template = args.fix_template.clone().or(if args.fix {
            Some("fix".to_string())
        } else {
            None
        });
        let fix = fix_template.is_some();

        // --fix and --start cannot be used together
        if fix && args.start {
            return Err(AikiError::InvalidArgument(
                "--fix and --start cannot be used together. Use --fix with blocking or --async mode."
                    .to_string(),
            ));
        }

        let run_kind = RunKind::from_args(args);

        // Keep a copy on ReviewOpts for dispatch/continue-async validation, and
        // forward the same value into workflow.target for SetupReview.
        let target = args.continue_async.clone().or(args.target.clone());

        Ok(ReviewOpts {
            target: target.clone(),
            run_kind,
            output: args.output.clone(),
            workflow: WorkflowOpts {
                target,
                code: args.code,
                review_template: args.template.clone(),
                reviewer: args.agent.clone(),
                fix,
                fix_template,
                coder: args.coder.clone(),
                autorun: args.autorun,
                ..WorkflowOpts::default()
            },
        })
    }
}

/// Assemble a two-step review workflow: SetupReview → Review.
fn workflow(cwd: &Path, opts: &ReviewOpts) -> Workflow {
    Workflow {
        steps: vec![Step::SetupReview, Step::Review],
        ctx: WorkflowContext {
            task_id: None,
            plan_path: None,
            cwd: cwd.to_path_buf(),
            output: WorkflowOutput::new(OutputKind::Text),
            opts: opts.workflow.clone(),
            review_id: None,
            scope: None,
            assignee: None,
            iteration: 0,
            notify_rx: None,
            task_names: std::collections::HashMap::new(),
        },
    }
}

/// Run a review workflow.
pub fn run(cwd: &Path, opts: &ReviewOpts) -> crate::error::Result<WorkflowContext> {
    match opts.run_kind {
        RunKind::SetupOnly => run_setup_only(cwd, opts),
        RunKind::Async => run_async(cwd, opts),
        RunKind::ContinueAsync => run_continue_async(cwd, opts),
        RunKind::Foreground => run_foreground(cwd, opts),
    }
}

/// Run SetupReview only, reassign to current agent, and return.
fn run_setup_only(cwd: &Path, opts: &ReviewOpts) -> crate::error::Result<WorkflowContext> {
    let wf = workflow(cwd, opts);
    let ctx = wf.run_first_step()?;
    let review_id = ctx.require_task_id()?;

    if let Some(session) = find_active_session(cwd) {
        reassign_task(cwd, review_id, session.agent_type.as_str())?;
    }
    start_task_core(cwd, &[review_id.to_string()])?;

    match opts.output {
        Some(OutputFormat::Id) => println!("{}", review_id),
        None => output_review_started(review_id),
    }
    Ok(ctx)
}

/// Run SetupReview synchronously, then spawn remaining steps in background.
fn run_async(cwd: &Path, opts: &ReviewOpts) -> crate::error::Result<WorkflowContext> {
    let wf = workflow(cwd, opts);
    let ctx = wf.run_first_step()?;
    let review_id = ctx.require_task_id()?;

    let mut continue_opts = opts.clone();
    continue_opts.target = Some(review_id.to_string());
    continue_opts.run_kind = RunKind::ContinueAsync;
    async_run::spawn_continue(
        &ctx.cwd,
        &["review", "--_continue-async", review_id],
        &continue_opts,
    )?;

    match opts.output {
        Some(OutputFormat::Id) => println!("{}", review_id),
        None => output_review_async(review_id),
    }
    Ok(ctx)
}

/// Continue a previously started async review silently.
///
/// Only runs `Step::Review`, intentionally skipping `SetupReview`. The setup
/// step creates a new review task, but in the continue path the task already
/// exists — it was created during the synchronous first phase in `run_async`.
/// Re-running setup here would create a duplicate task.
///
/// This differs from `build.rs`'s `run_continue_async`, which re-runs its
/// full workflow (including `SetupEpic`) because epic setup is idempotent.
///
/// Validates that the stdin payload's target matches the CLI-provided review
/// ID to prevent accidentally continuing the wrong review task.
fn run_continue_async(cwd: &Path, cli_opts: &ReviewOpts) -> crate::error::Result<WorkflowContext> {
    let opts: ReviewOpts = async_run::read_continue_opts()?;

    if let Some(ref cli_target) = cli_opts.target {
        if opts.target.as_ref() != Some(cli_target) {
            return Err(AikiError::InvalidArgument(format!(
                "stdin target does not match CLI review ID: expected {}, got {}",
                cli_target,
                opts.target.as_deref().unwrap_or("<none>"),
            )));
        }
    }

    let task_id = opts.target.as_ref().ok_or_else(|| {
        AikiError::InvalidArgument("--_continue-async requires a target task ID".to_string())
    })?;
    let mut wf = workflow(cwd, &opts);
    wf.ctx.task_id = Some(task_id.clone());
    // Remove SetupReview — the review task was already created during the
    // synchronous first phase in `run_async`. Re-running it would create a
    // duplicate. We keep the remaining steps so dynamic-step-injection and
    // any future post-review steps are picked up automatically.
    wf.steps.retain(|s| !matches!(s, Step::SetupReview));
    wf.ctx.output = WorkflowOutput::new(OutputKind::Quiet);
    let ctx = wf.run()?;
    let review_id = ctx.require_task_id()?;
    maybe_run_fix(&ctx.cwd, review_id, &opts, true)?;
    Ok(ctx)
}

/// Run the full review workflow in foreground with output.
fn run_foreground(cwd: &Path, opts: &ReviewOpts) -> crate::error::Result<WorkflowContext> {
    let wf = workflow(cwd, opts);
    let ctx = wf.run()?;
    let review_id = ctx.require_task_id()?;
    let fix_has_remaining_issues = maybe_run_fix(cwd, review_id, opts, false)?;

    match opts.output {
        Some(OutputFormat::Id) => println!("{}", review_id),
        None => output_review_completed(cwd, review_id, !opts.workflow.fix, fix_has_remaining_issues),
    }
    Ok(ctx)
}

/// Run the fix quality loop if `--fix` was requested.
///
/// Returns `true` if fix ran and issues remain after completion.
fn maybe_run_fix(
    cwd: &Path,
    review_id: &str,
    opts: &ReviewOpts,
    quiet: bool,
) -> crate::error::Result<bool> {
    if !opts.workflow.fix {
        return Ok(false);
    }
    let (scope, assignee) =
        resolve_scope_and_assignee(cwd, review_id, opts.workflow.coder.as_deref())?;
    let fix_opts = FixOpts::from_workflow(
        review_id.to_string(),
        if quiet { None } else { opts.output.clone() },
        &opts.workflow,
    );
    let assignee_type = assignee.as_deref().and_then(crate::agents::AgentType::from_str);
    let mut wf = fix::workflow(cwd, review_id, &fix_opts, &scope, assignee_type);
    let output = if quiet {
        OutputKind::Quiet
    } else {
        OutputKind::Text
    };
    wf.ctx.output = WorkflowOutput::new(output);
    wf.run().map_err(AikiError::Other)?;
    let remaining = fix::has_remaining_issues(cwd, review_id);
    Ok(remaining)
}

/// Output review started message (for --start mode).
fn output_review_started(review_id: &str) {
    println!("Started: {review_id}\n");
}

/// Output review async message (for --async mode).
fn output_review_async(review_id: &str) {
    println!("Dispatched: {review_id}\n");
}

/// Output review completed message (for blocking mode).
///
/// Hints:
/// - `show_fix_hint`: review found issues, no fix attempted → suggest `aiki fix`
/// - `show_pair_hint`: fix ran but issues remain → suggest `aiki fix --pair`
/// - Otherwise (no issues, or fix resolved all) → suggest `aiki tldr`
fn output_review_completed(cwd: &Path, review_id: &str, show_fix_hint: bool, show_pair_hint: bool) {
    let summary =
        crate::reviews::review_summary(cwd, review_id).unwrap_or_else(|_| "unknown".to_string());
    let short_id = &review_id[..7.min(review_id.len())];
    let status = format!("Completed: {review_id} — {summary}\n");
    let hint = if show_pair_hint {
        format!("Run `aiki fix {} --pair` to interactively remediate remaining issues.\n", short_id)
    } else if show_fix_hint {
        format!("Run `aiki fix {}` to remediate.\n", short_id)
    } else {
        format!("Run `aiki tldr {}` for a walkthrough.\n", short_id)
    };
    println!("{status}\n{hint}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::review::ReviewArgs as ReviewCommandArgs;

    // ── 7a: JSON serialization roundtrip tests ──

    #[test]
    fn review_opts_json_roundtrip_preserves_fix_flags() {
        let opts = ReviewOpts {
            target: Some("review123".to_string()),
            run_kind: RunKind::ContinueAsync,
            output: Some(OutputFormat::Id),
            workflow: WorkflowOpts {
                fix: true,
                fix_template: Some("fix".to_string()),
                reviewer: Some("claude-code".to_string()),
                autorun: true,
                review_template: Some("review/task".to_string()),
                code: true,
                ..WorkflowOpts::default()
            },
        };
        let json = serde_json::to_string(&opts).unwrap();
        let restored: ReviewOpts = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.target, opts.target);
        assert!(restored.run_kind == RunKind::ContinueAsync);
        assert_eq!(restored.output, Some(OutputFormat::Id));
        assert!(restored.workflow.fix);
        assert_eq!(restored.workflow.fix_template, Some("fix".to_string()));
        assert_eq!(
            restored.workflow.reviewer,
            Some("claude-code".to_string())
        );
        assert!(restored.workflow.autorun);
        assert_eq!(
            restored.workflow.review_template,
            Some("review/task".to_string())
        );
        assert!(restored.workflow.code);
    }

    #[test]
    fn review_opts_json_roundtrip_default_flags_are_false() {
        let opts = ReviewOpts {
            target: None,
            run_kind: RunKind::Foreground,
            output: None,
            workflow: WorkflowOpts::default(),
        };
        let json = serde_json::to_string(&opts).unwrap();
        let restored: ReviewOpts = serde_json::from_str(&json).unwrap();

        assert!(!restored.workflow.fix);
        assert!(restored.workflow.fix_template.is_none());
        assert!(!restored.workflow.autorun);
        assert!(!restored.workflow.code);
    }

    // ── 7b: Flag propagation contract tests ──

    fn default_review_args() -> ReviewCommandArgs {
        ReviewCommandArgs {
            target: None,
            code: false,
            fix: false,
            fix_template: None,
            run_async: false,
            start: false,
            template: None,
            agent: None,
            claude: false,
            codex: false,
            cursor: false,
            gemini: false,
            coder: None,
            autorun: false,
            output: None,
            continue_async: None,
            subcommand: None,
        }
    }

    #[test]
    fn from_args_fix_flag_sets_fix_and_fix_template() {
        let args = ReviewCommandArgs {
            fix: true,
            ..default_review_args()
        };
        let opts = ReviewOpts::from_args(&args).unwrap();
        assert!(opts.workflow.fix);
        assert_eq!(opts.workflow.fix_template, Some("fix".to_string()));
    }

    #[test]
    fn from_args_fix_template_sets_fix_true() {
        let args = ReviewCommandArgs {
            fix_template: Some("custom-fix".to_string()),
            ..default_review_args()
        };
        let opts = ReviewOpts::from_args(&args).unwrap();
        assert!(opts.workflow.fix);
        assert_eq!(opts.workflow.fix_template, Some("custom-fix".to_string()));
    }

    #[test]
    fn from_args_no_fix_flags_leaves_fix_false() {
        let args = default_review_args();
        let opts = ReviewOpts::from_args(&args).unwrap();
        assert!(!opts.workflow.fix);
        assert!(opts.workflow.fix_template.is_none());
    }

    #[test]
    fn from_args_fix_with_start_is_error() {
        let args = ReviewCommandArgs {
            fix: true,
            start: true,
            ..default_review_args()
        };
        assert!(ReviewOpts::from_args(&args).is_err());
    }

    // ── 7d: Async spawn payload tests ──

    #[test]
    fn review_async_continue_opts_preserve_fix() {
        let opts = ReviewOpts {
            target: Some("task123".to_string()),
            run_kind: RunKind::Async,
            output: None,
            workflow: WorkflowOpts {
                fix: true,
                fix_template: Some("fix".to_string()),
                reviewer: Some("claude-code".to_string()),
                autorun: true,
                ..WorkflowOpts::default()
            },
        };

        let mut continue_opts = opts.clone();
        continue_opts.target = Some("review456".to_string());
        continue_opts.run_kind = RunKind::ContinueAsync;

        let json = serde_json::to_string(&continue_opts).unwrap();
        let restored: ReviewOpts = serde_json::from_str(&json).unwrap();

        assert!(restored.run_kind == RunKind::ContinueAsync);
        assert_eq!(restored.target, Some("review456".to_string()));
        assert!(
            restored.workflow.fix,
            "fix flag must survive async continue"
        );
        assert_eq!(restored.workflow.fix_template, Some("fix".to_string()));
        assert!(restored.workflow.autorun);
        assert_eq!(
            restored.workflow.reviewer,
            Some("claude-code".to_string())
        );
    }

    // ── 7e: Workflow step composition tests ──

    #[test]
    fn review_workflow_has_two_steps() {
        let opts = ReviewOpts {
            target: Some("task123".to_string()),
            run_kind: RunKind::Foreground,
            output: None,
            workflow: WorkflowOpts::default(),
        };
        let temp_dir = tempfile::TempDir::new().unwrap();
        let wf = workflow(temp_dir.path(), &opts);
        assert_eq!(wf.steps.len(), 2);
        assert_eq!(wf.steps[0].name(), "setup review");
        assert_eq!(wf.steps[1].name(), "review");
    }

    #[test]
    fn review_workflow_fix_flag_does_not_add_steps() {
        let opts = ReviewOpts {
            target: Some("task123".to_string()),
            run_kind: RunKind::Foreground,
            output: None,
            workflow: WorkflowOpts {
                fix: true,
                fix_template: Some("fix".to_string()),
                ..WorkflowOpts::default()
            },
        };
        let temp_dir = tempfile::TempDir::new().unwrap();
        let wf = workflow(temp_dir.path(), &opts);
        assert_eq!(wf.steps.len(), 2, "fix is at runner level, not step level");
    }

    // ── fix_opts_for_review propagation tests ──

    #[test]
    fn review_fix_opts_forward_shared_fix_settings() {
        let opts = ReviewOpts {
            target: Some("task123".to_string()),
            run_kind: RunKind::Foreground,
            output: Some(OutputFormat::Id),
            workflow: WorkflowOpts {
                fix: true,
                fix_template: Some("custom-fix".to_string()),
                decompose_template: Some("custom-decompose".to_string()),
                loop_template: Some("custom-loop".to_string()),
                review_template: Some("custom-review".to_string()),
                reviewer: Some("reviewer".to_string()),
                coder: Some("codex".to_string()),
                autorun: true,
                ..WorkflowOpts::default()
            },
        };

        let fix_opts =
            FixOpts::from_workflow("review123".to_string(), opts.output.clone(), &opts.workflow);

        assert_eq!(fix_opts.review_id, "review123");
        assert_eq!(fix_opts.output, Some(OutputFormat::Id));
        assert_eq!(
            fix_opts.workflow.plan_template,
            Some("custom-fix".to_string()),
            "fix_template must propagate as plan_template"
        );
        assert_eq!(
            fix_opts.workflow.decompose_template,
            Some("custom-decompose".to_string())
        );
        assert_eq!(
            fix_opts.workflow.loop_template,
            Some("custom-loop".to_string())
        );
        assert_eq!(
            fix_opts.workflow.review_template,
            Some("custom-review".to_string())
        );
        assert_eq!(
            fix_opts.workflow.reviewer,
            Some("reviewer".to_string())
        );
        assert_eq!(fix_opts.workflow.coder, Some("codex".to_string()));
        assert!(fix_opts.workflow.autorun);
    }

    #[test]
    fn review_fix_opts_preserve_reviewer_override_separately_from_coder() {
        let args = ReviewCommandArgs {
            target: Some("task123".to_string()),
            code: true,
            fix: true,
            fix_template: Some("custom-fix".to_string()),
            run_async: false,
            start: false,
            template: Some("custom-review".to_string()),
            agent: Some("codex".to_string()),
            claude: false,
            codex: false,
            cursor: false,
            gemini: false,
            coder: None,
            autorun: true,
            output: Some(OutputFormat::Id),
            continue_async: None,
            subcommand: None,
        };
        let opts = ReviewOpts::from_args(&args).unwrap();

        let forwarded =
            FixOpts::from_workflow("review123".to_string(), opts.output.clone(), &opts.workflow);
        assert_eq!(forwarded.review_id, "review123");
        assert!(forwarded.run_kind == RunKind::Foreground);
        assert_eq!(forwarded.output, Some(OutputFormat::Id));
        assert!(!forwarded.once);
        assert_eq!(forwarded.continue_async_id, None);
        assert_eq!(
            forwarded.workflow.plan_template,
            Some("custom-fix".to_string())
        );
        assert_eq!(
            forwarded.workflow.review_template,
            Some("custom-review".to_string())
        );
        assert_eq!(
            forwarded.workflow.reviewer,
            Some("codex".to_string())
        );
        assert_eq!(forwarded.workflow.coder, None);
        assert!(forwarded.workflow.autorun);
    }

    #[test]
    fn review_fix_opts_quiet_mode_suppresses_output_but_matches_fix_from_args_elsewhere() {
        let opts = ReviewOpts {
            target: Some("task123".to_string()),
            run_kind: RunKind::Foreground,
            output: Some(OutputFormat::Id),
            workflow: WorkflowOpts {
                fix: true,
                fix_template: Some("custom-fix".to_string()),
                decompose_template: Some("custom-decompose".to_string()),
                loop_template: Some("custom-loop".to_string()),
                review_template: Some("custom-review".to_string()),
                reviewer: Some("reviewer".to_string()),
                coder: Some("codex".to_string()),
                autorun: true,
                ..WorkflowOpts::default()
            },
        };

        let quiet_forwarded = FixOpts::from_workflow("review123".to_string(), None, &opts.workflow);
        assert!(
            quiet_forwarded.output.is_none(),
            "quiet mode must suppress output"
        );
        assert_eq!(
            quiet_forwarded.workflow.plan_template,
            Some("custom-fix".to_string())
        );
        assert_eq!(
            quiet_forwarded.workflow.decompose_template,
            Some("custom-decompose".to_string())
        );
        assert_eq!(
            quiet_forwarded.workflow.loop_template,
            Some("custom-loop".to_string())
        );
        assert_eq!(
            quiet_forwarded.workflow.review_template,
            Some("custom-review".to_string())
        );
        assert_eq!(
            quiet_forwarded.workflow.reviewer,
            Some("reviewer".to_string())
        );
        assert_eq!(
            quiet_forwarded.workflow.coder,
            Some("codex".to_string())
        );
        assert!(quiet_forwarded.workflow.autorun);
    }
}
