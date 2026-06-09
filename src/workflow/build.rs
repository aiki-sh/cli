use std::path::Path;

use serde::{Deserialize, Serialize};

use super::async_run;
use super::fix::{self, FixOpts};
use super::steps::Step;
use super::{OutputKind, Workflow, WorkflowContext, WorkflowOpts, WorkflowOutput};
use crate::agents::AgentType;
use crate::commands::build::BuildArgs as BuildCommandArgs;
use crate::error::AikiError;
use crate::plans::PlanGraph;
use crate::reviews::resolve_scope_and_assignee;
use crate::tasks::looks_like_task_id;
use crate::tasks::{materialize_graph, read_events};
use crate::tui;
use crate::tui::theme::{detect_mode, Theme};

/// Options for assembling and running a build workflow.
///
/// Contains only dispatch-level fields. Workflow-level options live in `workflow`.
#[derive(Clone, Serialize, Deserialize)]
pub struct BuildOpts {
    pub target: String,
    pub run_kind: super::RunKind,
    pub output: Option<crate::commands::OutputFormat>,
    pub workflow: WorkflowOpts,
}

impl BuildOpts {
    /// Build `BuildOpts` from CLI args, resolving flag combinations in one place.
    ///
    /// Validates that a target (plan path or epic ID) is present. For
    /// `--_continue-async`, the epic ID is the target.
    pub fn from_args(args: &BuildCommandArgs, agent: Option<AgentType>) -> crate::error::Result<Self> {
        let target = args
            .continue_async
            .as_deref()
            .or(args.target.as_deref())
            .ok_or_else(|| {
                AikiError::InvalidArgument(
                    "No plan path or epic ID provided. Usage: aiki build <plan-path-or-epic-id>"
                        .to_string(),
                )
            })?
            .to_string();

        let fix = args.fix || args.fix_template.is_some();
        let review_template = args.review_template.clone();
        let review = review_template.is_some() || args.review || fix;

        let run_kind = super::RunKind::from_args(args);

        Ok(BuildOpts {
            target,
            run_kind,
            output: args.output.clone(),
            workflow: WorkflowOpts {
                restart: args.restart,
                decompose_template: args.decompose_template.clone(),
                loop_template: args.loop_template.clone(),
                agent,
                review,
                review_template,
                fix,
                fix_template: args.fix_template.clone(),
                coder: args.coder.clone(),
                reviewer: args.reviewer.clone(),
                ..WorkflowOpts::default()
            },
        })
    }
}

/// Build a `WorkflowContext` from the target in opts.
///
/// If target is a task ID, sets `task_id` (SetupEpic will look up plan_path).
/// If target is a plan path, sets `plan_path` (SetupEpic will create the epic).
fn build_context(cwd: &Path, opts: &BuildOpts) -> WorkflowContext {
    let (task_id, plan_path) = if looks_like_task_id(&opts.target) {
        (Some(opts.target.clone()), None)
    } else {
        (None, Some(opts.target.clone()))
    };
    WorkflowContext {
        task_id,
        plan_path,
        cwd: cwd.to_path_buf(),
        output: WorkflowOutput::new(OutputKind::Text),
        opts: opts.workflow.clone(),
        review_id: None,
        scope: None,
        assignee: None,
        iteration: 0,
        notify_rx: None,
        task_names: std::collections::HashMap::new(),
    }
}

/// Assemble a build workflow for a target (plan path or epic ID).
fn workflow(cwd: &Path, opts: &BuildOpts) -> Workflow {
    let mut steps: Vec<Step> = vec![Step::SetupEpic, Step::Decompose, Step::Loop];

    if opts.workflow.review {
        steps.push(Step::SetupReview);
        steps.push(Step::Review);
    }

    Workflow {
        steps,
        ctx: build_context(cwd, opts),
    }
}

/// Emit build status display after workflow completes.
///
/// Shows the TUI build view for the epic associated with the plan.
pub(crate) fn output_build_status(
    ctx: &WorkflowContext,
    output: &Option<crate::commands::OutputFormat>,
) {
    let plan_path = match ctx.plan_path.as_deref() {
        Some(p) => p,
        None => return,
    };

    let events = match read_events(&ctx.cwd) {
        Ok(e) => e,
        Err(_) => return,
    };
    let graph = materialize_graph(&events);
    let plan_graph = PlanGraph::build(&graph);

    let epic = match plan_graph.resolve_epic_for_plan(plan_path, &graph) {
        Ok(Some(e)) => e,
        Ok(None) => return,
        Err(err) => {
            eprintln!("{}", err);
            return;
        }
    };

    match output {
        Some(crate::commands::OutputFormat::Id) => println!("{}", epic.id),
        None => {
            let theme = Theme::from_mode(detect_mode());
            let window = tui::app::WindowState::new(80);
            let mut lines = tui::screens::build::view(&graph, &epic.id, plan_path, &window);
            println!("{}", tui::render::render_to_string(&mut lines, &theme));
        }
    }
}

/// Run a build workflow.
pub fn run(cwd: &Path, opts: &BuildOpts) -> crate::error::Result<WorkflowContext> {
    use super::RunKind;

    match opts.run_kind {
        RunKind::SetupOnly => run_setup_only(cwd, opts),
        RunKind::Async => run_async(cwd, opts),
        RunKind::ContinueAsync => run_continue_async(cwd, opts),
        RunKind::Foreground => run_foreground(cwd, opts),
    }
}

/// Run SetupEpic only, return the context to the caller.
fn run_setup_only(cwd: &Path, opts: &BuildOpts) -> crate::error::Result<WorkflowContext> {
    let wf = workflow(cwd, opts);
    Ok(wf.run_first_step()?)
}

/// Run SetupEpic synchronously, then spawn remaining steps in background.
fn run_async(cwd: &Path, opts: &BuildOpts) -> crate::error::Result<WorkflowContext> {
    let wf = workflow(cwd, opts);
    let ctx = wf.run_first_step()?;
    let epic_id = ctx.require_task_id()?;

    if let Some(crate::commands::OutputFormat::Id) = opts.output {
        println!("{}", epic_id);
    }

    let mut continue_opts = opts.clone();
    continue_opts.target = epic_id.to_string();
    continue_opts.run_kind = super::RunKind::ContinueAsync;
    async_run::spawn_continue(
        &ctx.cwd,
        &["build", "--_continue-async", epic_id],
        &continue_opts,
    )?;
    Ok(ctx)
}

/// Continue a previously started async build silently.
///
/// Re-runs the full workflow including SetupEpic. This is safe because
/// `opts.target` is the epic ID (set by `run_async`), so `build_context`
/// pre-populates `ctx.task_id`. SetupEpic's `run_from_epic_id` path is
/// idempotent: it only reads the task graph to populate `ctx.plan_path`
/// (required by Decompose) and checks blockers — no tasks are created or
/// modified.
fn run_continue_async(cwd: &Path, cli_opts: &BuildOpts) -> crate::error::Result<WorkflowContext> {
    // The `cli_opts` parameter (from CLI args) is intentionally ignored. The actual
    // opts — including `target` (epic ID) and `run_kind` — are read from stdin
    // via `read_continue_opts`, where `run_async` serialized them. The original
    // CLI `target` (the plan path) was already overwritten with the epic ID in
    // `run_async` before spawning this process.
    debug_assert!(
        cli_opts.run_kind == super::RunKind::ContinueAsync,
        "run_continue_async should only be called with ContinueAsync run_kind"
    );
    let opts: BuildOpts = async_run::read_continue_opts()?;

    if !cli_opts.target.is_empty() && opts.target != cli_opts.target {
        return Err(AikiError::InvalidArgument(format!(
            "stdin target does not match CLI epic ID: expected {}, got {}",
            opts.target, cli_opts.target,
        )));
    }

    let mut wf = workflow(cwd, &opts);
    wf.ctx.output = WorkflowOutput::new(OutputKind::Quiet);
    let ctx = wf.run()?;
    if let Some(ref review_id) = ctx.task_id {
        maybe_run_fix(cwd, review_id, &opts, true)?;
    }
    Ok(ctx)
}

/// Optionally run the fix quality loop after a build that includes review.
fn maybe_run_fix(
    cwd: &Path,
    review_id: &str,
    opts: &BuildOpts,
    quiet: bool,
) -> crate::error::Result<()> {
    if !opts.workflow.fix {
        return Ok(());
    }
    let (scope, assignee) =
        resolve_scope_and_assignee(cwd, review_id, opts.workflow.coder.as_deref())?;
    let fix_opts = FixOpts::from_workflow(
        review_id.to_string(),
        if quiet { None } else { opts.output.clone() },
        &opts.workflow,
    );
    let assignee_type = assignee.as_deref().and_then(AgentType::from_str);
    let mut wf = fix::workflow(cwd, review_id, &fix_opts, &scope, assignee_type);
    let output = if quiet {
        OutputKind::Quiet
    } else {
        OutputKind::Text
    };
    wf.ctx.output = WorkflowOutput::new(output);
    wf.run().map_err(AikiError::Other)?;
    Ok(())
}

/// Run the full build workflow in foreground with TUI output.
fn run_foreground(cwd: &Path, opts: &BuildOpts) -> crate::error::Result<WorkflowContext> {
    let wf = workflow(cwd, opts);
    let ctx = wf.run()?;
    if let Some(ref review_id) = ctx.task_id {
        maybe_run_fix(cwd, review_id, opts, false)?;
    }
    output_build_status(&ctx, &opts.output);
    Ok(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::RunKind;

    fn test_opts() -> BuildOpts {
        BuildOpts {
            target: "plan.md".to_string(),
            run_kind: super::super::RunKind::Foreground,
            output: None,
            workflow: super::super::WorkflowOpts::default(),
        }
    }

    #[test]
    fn review_step_added_when_review_is_true() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("plan.md"), "# Plan").unwrap();

        let without = workflow(temp_dir.path(), &test_opts());
        let with = workflow(
            temp_dir.path(),
            &BuildOpts {
                workflow: super::super::WorkflowOpts {
                    review: true,
                    ..super::super::WorkflowOpts::default()
                },
                ..test_opts()
            },
        );

        assert_eq!(without.steps.len(), 3);
        assert_eq!(with.steps.len(), 5);
        assert_eq!(with.steps[3].name(), "setup review");
        assert_eq!(with.steps[4].name(), "review");
    }

    #[test]
    fn fix_flag_does_not_add_fix_step() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("plan.md"), "# Plan").unwrap();

        let wf = workflow(
            temp_dir.path(),
            &BuildOpts {
                workflow: super::super::WorkflowOpts {
                    review: true,
                    fix: true,
                    ..super::super::WorkflowOpts::default()
                },
                ..test_opts()
            },
        );

        assert!(!wf.steps.iter().any(|s| s.name() == "fix"));
    }

    #[test]
    fn plan_target_sets_plan_path_in_context() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("plan.md"), "# Plan").unwrap();

        let wf = workflow(temp_dir.path(), &test_opts());
        assert_eq!(wf.ctx.plan_path.as_deref(), Some("plan.md"));
        assert!(wf.ctx.task_id.is_none());
    }

    #[test]
    fn epic_id_target_sets_task_id_in_context() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        let opts = BuildOpts {
            target: "onnlrwntommtvtnzovwromnkyulorwtz".to_string(),
            ..test_opts()
        };
        let wf = workflow(temp_dir.path(), &opts);
        assert_eq!(
            wf.ctx.task_id.as_deref(),
            Some("onnlrwntommtvtnzovwromnkyulorwtz")
        );
        assert!(wf.ctx.plan_path.is_none());
    }

    #[test]
    fn review_scope_uses_task_kind() {
        use crate::reviews::ReviewScopeKind;

        let epic_id = "onnlrwntommtvtnzovwromnkyulorwtz";
        let scope = crate::workflow::steps::setup_review::build_review_scope(epic_id);

        assert_eq!(scope.kind, ReviewScopeKind::Task);
        assert_eq!(scope.id, epic_id);
        assert!(scope.task_ids.is_empty());
    }

    // ── 7a: JSON serialization roundtrip tests ──

    #[test]
    fn build_opts_json_roundtrip_preserves_all_fields() {
        use crate::agents::AgentType;

        let opts = BuildOpts {
            target: "plan.md".to_string(),
            run_kind: super::super::RunKind::ContinueAsync,
            output: Some(crate::commands::OutputFormat::Id),
            workflow: super::super::WorkflowOpts {
                restart: true,
                decompose_template: Some("custom-decompose".to_string()),
                loop_template: Some("custom-loop".to_string()),
                agent: Some(AgentType::Codex),
                review: true,
                review_template: Some("review/task".to_string()),
                fix: true,
                fix_template: Some("fix".to_string()),
                ..super::super::WorkflowOpts::default()
            },
        };
        let json = serde_json::to_string(&opts).unwrap();
        let restored: BuildOpts = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.target, "plan.md");
        assert!(restored.run_kind == super::super::RunKind::ContinueAsync);
        assert!(restored.workflow.restart);
        assert!(restored.workflow.review);
        assert!(restored.workflow.fix);
        assert_eq!(restored.workflow.fix_template, Some("fix".to_string()));
        assert_eq!(restored.workflow.agent, Some(AgentType::Codex));
        assert_eq!(
            restored.workflow.decompose_template,
            Some("custom-decompose".to_string())
        );
        assert_eq!(
            restored.workflow.loop_template,
            Some("custom-loop".to_string())
        );
    }

    // ── 7c: Flag propagation contract tests ──

    #[test]
    fn build_from_args_fix_implies_review() {
        use crate::commands::build::BuildArgs as BuildCommandArgs;

        let args = BuildCommandArgs {
            target: Some("plan.md".to_string()),
            run_async: false,
            restart: false,
            decompose_template: None,
            loop_template: None,
            agent: None,
            claude: false,
            codex: false,
            cursor: false,
            gemini: false,
            review: false,
            review_template: None,
            fix: true,
            fix_template: None,
            coder: None,
            reviewer: None,
            continue_async: None,
            output: None,
            subcommand: None,
        };
        let opts = BuildOpts::from_args(&args, None).unwrap();
        assert!(opts.workflow.fix);
        assert!(opts.workflow.review, "--fix must imply --review");
    }

    #[test]
    fn build_from_args_fix_template_implies_fix_and_review() {
        use crate::commands::build::BuildArgs as BuildCommandArgs;

        let args = BuildCommandArgs {
            target: Some("plan.md".to_string()),
            run_async: false,
            restart: false,
            decompose_template: None,
            loop_template: None,
            agent: None,
            claude: false,
            codex: false,
            cursor: false,
            gemini: false,
            review: false,
            review_template: None,
            fix: false,
            fix_template: Some("custom".to_string()),
            coder: None,
            reviewer: None,
            continue_async: None,
            output: None,
            subcommand: None,
        };
        let opts = BuildOpts::from_args(&args, None).unwrap();
        assert!(opts.workflow.fix);
        assert!(opts.workflow.review);
        assert_eq!(opts.workflow.fix_template, Some("custom".to_string()));
    }

    // ── 7d: Async spawn payload tests ──

    #[test]
    fn build_async_continue_opts_preserve_fix() {
        use crate::agents::AgentType;

        let opts = BuildOpts {
            target: "plan.md".to_string(),
            run_kind: super::super::RunKind::Async,
            output: None,
            workflow: super::super::WorkflowOpts {
                review: true,
                fix: true,
                fix_template: Some("fix".to_string()),
                agent: Some(AgentType::ClaudeCode),
                loop_template: Some("loop".to_string()),
                ..super::super::WorkflowOpts::default()
            },
        };

        let mut continue_opts = opts.clone();
        continue_opts.target = "epic789".to_string();
        continue_opts.run_kind = super::super::RunKind::ContinueAsync;

        let json = serde_json::to_string(&continue_opts).unwrap();
        let restored: BuildOpts = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.target, "epic789");
        assert!(
            restored.workflow.fix,
            "fix flag must survive async continue"
        );
        assert!(
            restored.workflow.review,
            "review flag must survive async continue"
        );
        assert_eq!(restored.workflow.fix_template, Some("fix".to_string()));
        assert_eq!(restored.workflow.loop_template, Some("loop".to_string()));
    }

    #[test]
    fn build_fix_opts_forward_shared_fix_settings() {
        let opts = BuildOpts {
            output: Some(crate::commands::OutputFormat::Id),
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
            ..test_opts()
        };

        let fix_opts =
            FixOpts::from_workflow("review123".to_string(), opts.output.clone(), &opts.workflow);

        assert_eq!(fix_opts.review_id, "review123");
        assert_eq!(fix_opts.output, Some(crate::commands::OutputFormat::Id));
        assert_eq!(
            fix_opts.workflow.plan_template,
            Some("custom-fix".to_string())
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
    fn build_fix_opts_do_not_forward_build_agent_as_coder() {
        use crate::agents::AgentType;

        let args = BuildCommandArgs {
            target: Some("plan.md".to_string()),
            run_async: false,
            restart: false,
            decompose_template: Some("custom-decompose".to_string()),
            loop_template: Some("custom-loop".to_string()),
            agent: Some("codex".to_string()),
            claude: false,
            codex: false,
            cursor: false,
            gemini: false,
            review: false,
            review_template: Some("custom-review".to_string()),
            fix: true,
            fix_template: Some("custom-fix".to_string()),
            coder: None,
            reviewer: None,
            continue_async: None,
            output: Some(crate::commands::OutputFormat::Id),
            subcommand: None,
        };
        let opts = BuildOpts::from_args(&args, Some(AgentType::Codex)).unwrap();

        let forwarded =
            FixOpts::from_workflow("review123".to_string(), opts.output.clone(), &opts.workflow);
        assert_eq!(forwarded.review_id, "review123");
        assert!(forwarded.run_kind == RunKind::Foreground);
        assert_eq!(forwarded.output, Some(crate::commands::OutputFormat::Id));
        assert!(!forwarded.once);
        assert_eq!(forwarded.continue_async_id, None);
        assert_eq!(
            forwarded.workflow.plan_template,
            Some("custom-fix".to_string())
        );
        assert_eq!(
            forwarded.workflow.decompose_template,
            Some("custom-decompose".to_string())
        );
        assert_eq!(
            forwarded.workflow.loop_template,
            Some("custom-loop".to_string())
        );
        assert_eq!(
            forwarded.workflow.review_template,
            Some("custom-review".to_string())
        );
        assert_eq!(forwarded.workflow.reviewer, None);
        assert_eq!(forwarded.workflow.coder, None);
        assert!(!forwarded.workflow.autorun);
    }

    #[test]
    fn build_fix_opts_quiet_mode_suppresses_output_but_matches_fix_from_args_elsewhere() {
        let opts = BuildOpts {
            output: Some(crate::commands::OutputFormat::Id),
            workflow: WorkflowOpts {
                fix: true,
                fix_template: Some("custom-fix".to_string()),
                decompose_template: Some("custom-decompose".to_string()),
                loop_template: Some("custom-loop".to_string()),
                review_template: Some("custom-review".to_string()),
                reviewer: Some("reviewer".to_string()),
                coder: Some("codex".to_string()),
                ..WorkflowOpts::default()
            },
            ..test_opts()
        };

        let quiet_forwarded = FixOpts::from_workflow("review123".to_string(), None, &opts.workflow);
        assert!(quiet_forwarded.output.is_none());
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
        assert!(!quiet_forwarded.workflow.autorun);
    }
}
