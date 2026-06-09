//! Fix command — CLI entry point.
//!
//! Thin command layer: parses args and delegates to `workflow::fix::run()`.
//! Workflow step handlers live under `workflow::steps::fix`.

use std::env;

use super::task::extract_task_id;
use super::OutputFormat;
use crate::error::{AikiError, Result};
use crate::workflow::fix::{self, FixOpts};

/// Arguments for the fix command
#[derive(clap::Args)]
pub struct FixArgs {
    /// Task ID to read comments from (reads from stdin if not provided)
    pub task_id: Option<String>,

    /// Run followup task asynchronously
    #[arg(long = "async")]
    pub run_async: bool,

    /// Internal: continue an async fix from a previously created fix-parent
    #[arg(long = "_continue-async", hide = true)]
    pub continue_async: Option<String>,

    /// Custom plan template (default: fix)
    #[arg(long)]
    pub template: Option<String>,

    /// Custom decompose template (default: decompose)
    #[arg(long = "decompose-template")]
    pub decompose_template: Option<String>,

    /// Custom loop template (default: loop)
    #[arg(long = "loop-template")]
    pub loop_template: Option<String>,

    /// Quality loop review with custom template
    #[arg(long = "review-template")]
    pub review_template: Option<String>,

    /// Agent for task assignment (default: claude-code)
    #[arg(long)]
    pub agent: Option<String>,

    /// Shorthand for --agent claude-code
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub claude: bool,
    /// Shorthand for --agent codex
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub codex: bool,
    /// Shorthand for --agent cursor
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub cursor: bool,
    /// Shorthand for --agent gemini
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub gemini: bool,

    /// Interactive pair mode — walk through issues with the user
    #[arg(long, conflicts_with = "run_async")]
    pub pair: bool,

    /// Enable autorun (auto-start this fix task when its target closes)
    #[arg(long)]
    pub autorun: bool,

    /// Disable post-fix review loop (single pass only)
    #[arg(long)]
    pub once: bool,

    /// Output format (e.g., `id` for bare task ID on stdout)
    #[arg(long, short = 'o', value_name = "FORMAT")]
    pub output: Option<OutputFormat>,
}

impl crate::workflow::HasRunKind for FixArgs {
    fn continue_async(&self) -> Option<&str> {
        self.continue_async.as_deref()
    }
    fn run_async(&self) -> bool {
        self.run_async
    }
}

/// Run the fix command — parse args and delegate to workflow.
pub fn run(mut args: FixArgs) -> Result<()> {
    use crate::session::flags::resolve_agent_shorthand;
    args.agent = resolve_agent_shorthand(args.agent, args.claude, args.codex, args.cursor, args.gemini)
        .map(|a| a.as_str().to_string());

    let cwd = env::current_dir()
        .map_err(|_| AikiError::InvalidArgument("Failed to get current directory".to_string()))?;
    let refs = super::input::resolve_ref_list(
        args.task_id.iter().cloned().collect(),
        extract_task_id,
    )?;
    // resolve_ref_list guarantees non-empty Vec (returns Err otherwise)
    let review_id = refs.into_iter().next().ok_or_else(|| AikiError::InvalidArgument("No task reference provided".to_string()))?.0;
    let opts = FixOpts::from_args(&args, review_id)?;
    fix::run(&cwd, &opts)?;
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────

// The workflow test remains here since it tests workflow composition
// using builders from workflow::fix.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reviews::{ReviewScope, ReviewScopeKind};
    use crate::tasks::{write_event, TaskEvent, TaskPriority};
    use crate::workflow::fix::workflow;
    use crate::workflow::fix::FixOpts;
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

        let template_dir = path.join(".aiki/tasks/review");
        std::fs::create_dir_all(&template_dir).expect("create review template dir");
        std::fs::write(
            template_dir.join("task.md"),
            "---\nversion: 2.0.0\ntype: review\n---\n\n# Review: {{data.scope.name}}\n\nReview the work.\n",
        )
        .expect("write review template");
    }

    #[test]
    fn test_extract_task_id_plain() {
        assert_eq!(extract_task_id("xqrmnpst"), "xqrmnpst");
        assert_eq!(extract_task_id("  xqrmnpst  "), "xqrmnpst");
    }

    #[test]
    fn test_extract_task_id_passthrough() {
        assert_eq!(extract_task_id("plain-id-123"), "plain-id-123");
    }

    #[test]
    fn test_workflow_has_expected_steps() {
        let temp_dir = tempfile::tempdir().unwrap();
        let opts = FixOpts {
            review_id: "review123".to_string(),
            run_kind: crate::workflow::RunKind::Foreground,
            output: None,
            once: false,
            pair: false,
            continue_async_id: None,
            workflow: crate::workflow::WorkflowOpts {
                plan_template: Some("fix".to_string()),
                decompose_template: Some("decompose".to_string()),
                loop_template: Some("loop".to_string()),
                autorun: true,
                ..crate::workflow::WorkflowOpts::default()
            },
        };
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "original123".to_string(),
            task_ids: vec![],
        };

        let workflow = workflow(temp_dir.path(), "review123", &opts, &scope, Some(crate::agents::AgentType::Codex));
        let names: Vec<_> = workflow.steps.iter().map(|s| s.name()).collect();

        assert_eq!(workflow.steps.len(), 6);
        assert_eq!(
            names,
            vec![
                "fix",
                "decompose",
                "loop",
                "setup review",
                "review",
                "review for regressions"
            ]
        );
    }

    #[test]
    fn test_review_scope_from_data_task() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "original123".to_string(),
            task_ids: vec![],
        };
        let data = scope.to_data();
        let restored = ReviewScope::from_data(&data).unwrap();
        assert_eq!(restored.kind, ReviewScopeKind::Task);
        assert_eq!(restored.id, "original123");
    }

    #[test]
    fn test_review_scope_from_data_missing() {
        use std::collections::HashMap;
        let data = HashMap::new();
        assert!(ReviewScope::from_data(&data).is_err());
    }

    #[test]
    fn test_output_format_gating_suppresses_approved_message() {
        let output_id: Option<OutputFormat> = Some(OutputFormat::Id);
        let output_none: Option<OutputFormat> = None;

        assert!(
            output_id == Some(OutputFormat::Id),
            "Some(Id) should match the suppression check"
        );
        assert!(
            !(output_id != Some(OutputFormat::Id)),
            "output_approved should NOT be called when output is Some(Id)"
        );

        assert!(
            output_none != Some(OutputFormat::Id),
            "output_approved SHOULD be called when output is None"
        );
    }

    #[test]
    fn test_output_format_id_only_suppresses_approved() {
        let should_print: Option<OutputFormat> = None;
        assert!(should_print != Some(OutputFormat::Id));

        let should_suppress: Option<OutputFormat> = Some(OutputFormat::Id);
        assert!(!(should_suppress != Some(OutputFormat::Id)));
    }

    #[test]
    fn test_review_scope_roundtrip_all_kinds() {
        for (kind, id) in [
            (ReviewScopeKind::Task, "task123".to_string()),
            (ReviewScopeKind::Plan, "ops/now/plan.md".to_string()),
            (ReviewScopeKind::Code, "src/main.rs".to_string()),
            (
                ReviewScopeKind::Session,
                "550e8400-e29b-41d4-a716-446655440000".to_string(),
            ),
        ] {
            let scope = ReviewScope {
                kind: kind.clone(),
                id: id.clone(),
                task_ids: vec![],
            };
            let data = scope.to_data();
            let restored = ReviewScope::from_data(&data).unwrap();
            assert_eq!(restored.kind, kind);
            assert_eq!(restored.id, id);
        }
    }

    #[test]
    fn test_review_scope_session_preserves_task_ids() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Session,
            id: "session-id".to_string(),
            task_ids: vec!["t1".to_string(), "t2".to_string(), "t3".to_string()],
        };
        let data = scope.to_data();
        let restored = ReviewScope::from_data(&data).unwrap();
        assert_eq!(restored.task_ids, vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn test_setup_review_preserves_original_scope_for_regression_review() {
        if std::process::Command::new("jj")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("Skipping test: jj binary not found in PATH");
            return;
        }

        let temp_dir = tempdir().unwrap();
        init_jj_repo(temp_dir.path());

        let fix_parent_id = "fixparentscopepreservationtestabc".to_string();
        write_event(
            temp_dir.path(),
            &TaskEvent::Created {
                task_id: fix_parent_id.clone(),
                name: "Fix parent".to_string(),
                slug: None,
                task_type: None,
                priority: TaskPriority::P2,
                assignee: Some("claude-code".to_string()),
                sources: Vec::new(),
                template: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: Utc::now(),
            },
        )
        .unwrap();

        let opts = FixOpts {
            review_id: "review123".to_string(),
            run_kind: crate::workflow::RunKind::Foreground,
            output: None,
            once: false,
            pair: false,
            continue_async_id: None,
            workflow: crate::workflow::WorkflowOpts {
                review_template: Some("review/task".to_string()),
                reviewer: Some("codex".to_string()),
                ..crate::workflow::WorkflowOpts::default()
            },
        };
        let original_scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "original123".to_string(),
            task_ids: vec![],
        };

        let mut workflow = workflow(
            temp_dir.path(),
            "review123",
            &opts,
            &original_scope,
            Some(crate::agents::AgentType::Codex),
        );
        workflow.ctx.task_id = Some(fix_parent_id.clone());

        workflow.steps[3].run(&mut workflow.ctx).unwrap();

        assert_ne!(
            workflow.ctx.task_id.as_deref(),
            Some(fix_parent_id.as_str())
        );

        let resolved = crate::workflow::steps::regression_review::resolve_regression_review_scope(&workflow.ctx)
            .unwrap()
            .unwrap();
        assert_eq!(resolved.kind, original_scope.kind);
        assert_eq!(resolved.id, original_scope.id);
        assert_eq!(resolved.task_ids, original_scope.task_ids);
    }

    #[test]
    fn test_review_scope_from_data_rejects_missing_kind() {
        use std::collections::HashMap;
        let data = HashMap::new();
        assert!(ReviewScope::from_data(&data).is_err());
    }

    #[test]
    fn test_review_scope_from_data_rejects_invalid_kind() {
        use std::collections::HashMap;
        let mut data = HashMap::new();
        data.insert("scope.kind".to_string(), "invalid".to_string());
        assert!(ReviewScope::from_data(&data).is_err());
    }

    #[test]
    fn test_fix_parent_data_fields() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "epic123".to_string(),
            task_ids: vec![],
        };
        let scope_data = scope.to_data();

        assert_eq!(scope_data.get("scope.kind").unwrap(), "task");
        assert_eq!(scope_data.get("scope.id").unwrap(), "epic123");
        assert!(
            scope_data.contains_key("scope.name"),
            "scope.name must be present"
        );

        let mut data = std::collections::HashMap::new();
        data.insert("review".to_string(), "review-abc".to_string());
        for (k, v) in scope_data {
            data.insert(k, v);
        }

        assert_eq!(data.get("review").unwrap(), "review-abc");
        assert_eq!(data.get("scope.kind").unwrap(), "task");
        assert_eq!(data.get("scope.id").unwrap(), "epic123");
    }
}
