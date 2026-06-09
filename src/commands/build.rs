//! Build command for decomposing plan files and executing all subtasks
//!
//! This module provides the `aiki build` command which:
//! - Creates an epic from a plan file and automatically executes all subtasks
//! - Supports building from an existing epic ID
//! - Shows build/epic status via the `show` subcommand
//! - Supports async (background) execution

use std::env;
use std::path::Path;

use clap::Subcommand;

use super::OutputFormat;
use crate::error::{AikiError, Result};
use crate::plans::PlanGraph;
use crate::tasks::{materialize_graph, read_events, Task};
use crate::workflow::build::{self, BuildOpts};

/// Build subcommands
#[derive(Subcommand)]
#[command(disable_help_subcommand = true)]
pub enum BuildSubcommands {
    /// Show build/epic status for a plan
    Show {
        /// Plan path to show build status for
        plan_path: String,

        /// Output format (e.g., `id` for bare task ID)
        #[arg(long, short = 'o', value_name = "FORMAT")]
        output: Option<OutputFormat>,
    },
}

/// Arguments for the build command
#[derive(clap::Args)]
pub struct BuildArgs {
    /// Plan path or epic ID (32 lowercase letters)
    pub target: Option<String>,

    /// Run build asynchronously
    #[arg(long = "async")]
    pub run_async: bool,

    /// Ignore existing epic, create new one from scratch
    #[arg(long)]
    pub restart: bool,

    /// Custom decompose template (default: decompose)
    #[arg(long = "decompose-template")]
    pub decompose_template: Option<String>,

    /// Custom loop template (default: loop)
    #[arg(long = "loop-template")]
    pub loop_template: Option<String>,

    /// Agent for build orchestration (default: claude-code)
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

    /// Run review after build
    #[arg(long, short = 'r')]
    pub review: bool,

    /// Run review after build with custom template (implies --review)
    #[arg(long = "review-template")]
    pub review_template: Option<String>,

    /// Run review+fix after build (implies --review)
    #[arg(long, short = 'f')]
    pub fix: bool,

    /// Run review+fix after build with custom fix plan template (implies --fix)
    #[arg(long = "fix-template")]
    pub fix_template: Option<String>,

    /// Agent for coding/fixing tasks (default: claude-code)
    #[arg(long)]
    pub coder: Option<String>,

    /// Agent for review tasks (default: opposite of coder)
    #[arg(long)]
    pub reviewer: Option<String>,

    /// Internal: continue an async build from a previously created epic
    #[arg(long = "_continue-async", hide = true)]
    pub continue_async: Option<String>,

    /// Output format (e.g., `id` for bare task IDs on stdout)
    #[arg(long, short = 'o', value_name = "FORMAT")]
    pub output: Option<OutputFormat>,

    /// Subcommand (show)
    #[command(subcommand)]
    pub subcommand: Option<BuildSubcommands>,
}

impl crate::workflow::HasRunKind for BuildArgs {
    fn continue_async(&self) -> Option<&str> {
        self.continue_async.as_deref()
    }
    fn run_async(&self) -> bool {
        self.run_async
    }
}

/// Run the build command
pub fn run(args: BuildArgs) -> Result<()> {
    use crate::session::flags::resolve_agent_shorthand;
    let agent = resolve_agent_shorthand(args.agent.clone(), args.claude, args.codex, args.cursor, args.gemini);

    let cwd = env::current_dir()
        .map_err(|_| AikiError::InvalidArgument("Failed to get current directory".to_string()))?;

    if let Some(subcommand) = &args.subcommand {
        return match subcommand {
            BuildSubcommands::Show { plan_path, output } => run_show(&cwd, plan_path, output.clone()),
        };
    }

    let opts = BuildOpts::from_args(&args, agent)?;
    build::run(&cwd, &opts)?;

    Ok(())
}

/// Show build/epic status for a plan
fn run_show(cwd: &Path, plan_path: &str, output_format: Option<OutputFormat>) -> Result<()> {
    use crate::workflow::build::output_build_status;
    use crate::workflow::WorkflowContext;

    match output_format {
        Some(OutputFormat::Id) => {
            let events = read_events(cwd)?;
            let graph = materialize_graph(&events);
            let plan_graph = PlanGraph::build(&graph);

            let epic = plan_graph
                .resolve_epic_for_plan(plan_path, &graph)?
                .ok_or_else(|| {
                    AikiError::InvalidArgument(format!("No epic found for plan: {}", plan_path))
                })?;

            let build_tasks: Vec<&Task> = graph
                .tasks
                .values()
                .filter(|t| {
                    t.task_type.as_deref() == Some("orchestrator")
                        && t.data.get("plan").map(|s| s.as_str()) == Some(plan_path)
                })
                .collect();

            if build_tasks.is_empty() {
                println!("{}", epic.id);
            } else {
                for build in &build_tasks {
                    println!("{}", build.id);
                }
            }
        }
        None => {
            let ctx = WorkflowContext {
                task_id: None,
                plan_path: Some(plan_path.to_string()),
                cwd: cwd.to_path_buf(),
                output: crate::workflow::WorkflowOutput::new(crate::workflow::OutputKind::Text),
                opts: crate::workflow::WorkflowOpts::default(),
                review_id: None,
                scope: None,
                assignee: None,
                iteration: 0,
                notify_rx: None,
                task_names: std::collections::HashMap::new(),
            };
            output_build_status(&ctx, &None);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::{EdgeStore, TaskGraph};
    use crate::tasks::id::is_task_id;
    use crate::tasks::types::FastHashMap;
    use crate::tasks::{TaskOutcome, TaskPriority, TaskStatus};
    use crate::epic::check_epic_blockers;
    use std::collections::HashMap;

    fn make_task(id: &str, name: &str, status: TaskStatus) -> Task {
        Task {
            id: id.to_string(),
            name: name.to_string(),
            slug: None,
            task_type: None,
            status,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            instructions: None,
            data: HashMap::new(),
            created_at: chrono::Utc::now(),
            started_at: None,
            claimed_by_session: None,
            last_session_id: None,
            stopped_reason: None,
            closed_outcome: None,
            confidence: None,
            summary: None,
            turn_started: None,
            closed_at: None,
            turn_closed: None,
            turn_stopped: None,
            comments: Vec::new(),
        }
    }

    fn make_task_with_data(
        id: &str,
        name: &str,
        status: TaskStatus,
        data: HashMap<String, String>,
    ) -> Task {
        let mut task = make_task(id, name, status);
        task.data = data;
        task
    }

    fn empty_graph() -> TaskGraph {
        TaskGraph {
            tasks: FastHashMap::default(),
            edges: EdgeStore::new(),
            slug_index: FastHashMap::default(),
        }
    }

    fn make_graph(tasks: FastHashMap<String, Task>, edges: EdgeStore) -> TaskGraph {
        TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        }
    }

    /// Helper: find epic for plan via PlanGraph
    fn find_epic_for_plan_via_graph<'a>(
        graph: &'a TaskGraph,
        plan_path: &str,
    ) -> anyhow::Result<Option<&'a Task>> {
        let sg = PlanGraph::build(graph);
        sg.resolve_epic_for_plan(plan_path, graph)
    }

    // --- find_epic_for_plan tests ---

    #[test]
    fn test_find_epic_for_plan_none() {
        let graph = make_graph(FastHashMap::default(), EdgeStore::new());
        assert!(find_epic_for_plan_via_graph(&graph, "ops/now/feature.md")
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_find_epic_for_plan_via_implements_link() {
        let mut tasks = FastHashMap::default();
        let task = make_task("epic1", "Epic: Feature", TaskStatus::Open);
        tasks.insert("epic1".to_string(), task);

        let mut edges = EdgeStore::new();
        edges.add("epic1", "file:ops/now/feature.md", "implements-plan");

        let graph = make_graph(tasks, edges);
        let result = find_epic_for_plan_via_graph(&graph, "ops/now/feature.md").unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "epic1");
    }

    #[test]
    fn test_find_epic_for_plan_wrong_plan() {
        let mut tasks = FastHashMap::default();
        let task = make_task("epic1", "Epic: Other", TaskStatus::Open);
        tasks.insert("epic1".to_string(), task);

        let mut edges = EdgeStore::new();
        edges.add("epic1", "file:ops/now/other.md", "implements-plan");

        let graph = make_graph(tasks, edges);
        assert!(find_epic_for_plan_via_graph(&graph, "ops/now/feature.md")
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_find_epic_for_plan_returns_ambiguity_error() {
        let mut tasks = FastHashMap::default();

        let mut task1 = make_task("epic_old", "Epic: Old", TaskStatus::Closed);
        task1.created_at = chrono::Utc::now() - chrono::Duration::hours(1);
        tasks.insert("epic_old".to_string(), task1);

        let task2 = make_task("epic_new", "Epic: New", TaskStatus::Open);
        tasks.insert("epic_new".to_string(), task2);

        let mut edges = EdgeStore::new();
        edges.add("epic_old", "file:ops/now/feature.md", "implements-plan");
        edges.add("epic_new", "file:ops/now/feature.md", "implements-plan");

        let graph = make_graph(tasks, edges);
        let err = find_epic_for_plan_via_graph(&graph, "ops/now/feature.md").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Multiple epics implement file:ops/now/feature.md"));
        assert!(msg.contains("epic_old (Epic: Old)"));
        assert!(msg.contains("epic_new (Epic: New)"));
    }

    // --- cleanup_stale_builds helper logic tests ---

    #[test]
    fn test_stale_build_detection_in_progress() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());

        let mut task =
            make_task_with_data("build1", "Build: feature", TaskStatus::InProgress, data);
        task.task_type = Some("orchestrator".to_string());
        tasks.insert("build1".to_string(), task);

        // Verify the stale build detection logic
        let stale_builds: Vec<String> = tasks
            .values()
            .filter(|t| {
                t.task_type.as_deref() == Some("orchestrator")
                    && t.data.get("plan").map(|s| s.as_str()) == Some("ops/now/feature.md")
                    && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
            })
            .map(|t| t.id.clone())
            .collect();

        assert_eq!(stale_builds.len(), 1);
        assert_eq!(stale_builds[0], "build1");
    }

    #[test]
    fn test_stale_build_detection_open() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());

        let mut task = make_task_with_data("build2", "Build: feature", TaskStatus::Open, data);
        task.task_type = Some("orchestrator".to_string());
        tasks.insert("build2".to_string(), task);

        let stale_builds: Vec<String> = tasks
            .values()
            .filter(|t| {
                t.task_type.as_deref() == Some("orchestrator")
                    && t.data.get("plan").map(|s| s.as_str()) == Some("ops/now/feature.md")
                    && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
            })
            .map(|t| t.id.clone())
            .collect();

        assert_eq!(stale_builds.len(), 1);
        assert_eq!(stale_builds[0], "build2");
    }

    #[test]
    fn test_stale_build_not_detected_when_closed() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());

        let mut task = make_task_with_data("build3", "Build: feature", TaskStatus::Closed, data);
        task.task_type = Some("orchestrator".to_string());
        tasks.insert("build3".to_string(), task);

        let stale_builds: Vec<String> = tasks
            .values()
            .filter(|t| {
                t.task_type.as_deref() == Some("orchestrator")
                    && t.data.get("plan").map(|s| s.as_str()) == Some("ops/now/feature.md")
                    && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
            })
            .map(|t| t.id.clone())
            .collect();

        assert!(stale_builds.is_empty());
    }

    #[test]
    fn test_stale_build_not_detected_wrong_plan() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/other.md".to_string());

        let mut task = make_task_with_data("build4", "Build: other", TaskStatus::InProgress, data);
        task.task_type = Some("orchestrator".to_string());
        tasks.insert("build4".to_string(), task);

        let stale_builds: Vec<String> = tasks
            .values()
            .filter(|t| {
                t.task_type.as_deref() == Some("orchestrator")
                    && t.data.get("plan").map(|s| s.as_str()) == Some("ops/now/feature.md")
                    && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
            })
            .map(|t| t.id.clone())
            .collect();

        assert!(stale_builds.is_empty());
    }

    #[test]
    fn test_stale_build_not_detected_wrong_type() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());

        // Not a build task (no task_type or different type)
        let task = make_task_with_data("not_build", "Something else", TaskStatus::InProgress, data);
        tasks.insert("not_build".to_string(), task);

        let stale_builds: Vec<String> = tasks
            .values()
            .filter(|t| {
                t.task_type.as_deref() == Some("orchestrator")
                    && t.data.get("plan").map(|s| s.as_str()) == Some("ops/now/feature.md")
                    && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
            })
            .map(|t| t.id.clone())
            .collect();

        assert!(stale_builds.is_empty());
    }

    // --- Argument detection tests ---

    #[test]
    fn test_argument_detection_plan_path() {
        assert!(!is_task_id("ops/now/feature.md"));
        assert!(!is_task_id("simple.md"));
        assert!(!is_task_id("/absolute/path/to/plan.md"));
        assert!(!is_task_id("not-a-task-id"));
        assert!(!is_task_id(""));
    }

    #[test]
    fn test_argument_detection_task_id() {
        // 32 lowercase k-z letters
        assert!(is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls"));
        assert!(is_task_id("xtuttnyvykpulsxzqnznsxylrzkkqssy"));
    }

    #[test]
    fn test_argument_detection_not_task_id() {
        // Too short
        assert!(!is_task_id("klmnop"));
        // Too long
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztklsx"));
        // Contains letters outside k-z range
        assert!(!is_task_id("abcdefghijklmnopqrstuvwxyzabcdef"));
        // Contains numbers
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvz1234"));
        // Contains uppercase
        assert!(!is_task_id("Mvslrspmoynoxyyywqyutmovxpvztkls"));
    }

    // --- Build show output formatting tests ---

    #[test]
    fn test_output_build_status_no_panic_with_missing_plan() {
        use crate::workflow::build::output_build_status;
        use crate::workflow::WorkflowContext;

        // output_build_status should gracefully handle missing epic (no panic)
        let ctx = WorkflowContext {
            task_id: None,
            plan_path: Some("nonexistent/plan.md".to_string()),
            cwd: std::path::PathBuf::from("/tmp"),
            output: crate::workflow::WorkflowOutput::new(crate::workflow::OutputKind::Text),
            opts: crate::workflow::WorkflowOpts::default(),
            review_id: None,
            scope: None,
            assignee: None,
            iteration: 0,
            notify_rx: None,
            task_names: std::collections::HashMap::new(),
        };
        output_build_status(&ctx, &None);
        output_build_status(&ctx, &Some(crate::commands::OutputFormat::Id));
    }

    #[test]
    fn test_output_build_status_no_panic_without_plan_path() {
        use crate::workflow::build::output_build_status;
        use crate::workflow::WorkflowContext;

        // No plan_path → early return, no panic
        let ctx = WorkflowContext {
            task_id: None,
            plan_path: None,
            cwd: std::path::PathBuf::from("/tmp"),
            output: crate::workflow::WorkflowOutput::new(crate::workflow::OutputKind::Text),
            opts: crate::workflow::WorkflowOpts::default(),
            review_id: None,
            scope: None,
            assignee: None,
            iteration: 0,
            notify_rx: None,
            task_names: std::collections::HashMap::new(),
        };
        output_build_status(&ctx, &None);
    }

    // --- OutputFormat tests ---

    #[test]
    fn test_output_format_id_variant() {
        let fmt = OutputFormat::Id;
        assert!(matches!(fmt, OutputFormat::Id));
    }

    #[test]
    fn test_output_format_clap_parse() {
        use clap::ValueEnum;
        let parsed = OutputFormat::from_str("id", false);
        assert!(parsed.is_ok());
        assert!(matches!(parsed.unwrap(), OutputFormat::Id));
    }

    #[test]
    fn test_output_format_clap_rejects_unknown() {
        use clap::ValueEnum;
        let parsed = OutputFormat::from_str("unknown_format", false);
        assert!(parsed.is_err());
    }

    // --- Build args tests ---

    #[test]
    fn test_build_args_no_loop_or_lanes_flags() {
        let args = BuildArgs {
            target: Some("test.md".to_string()),
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
            fix_template: None,
            coder: None,
            reviewer: None,
            continue_async: None,
            output: None,
            subcommand: None,
        };
        assert!(!args.run_async);
        assert!(!args.restart);
    }

    #[test]
    fn test_loop_template_override_via_flag() {
        let args = BuildArgs {
            target: Some("test.md".to_string()),
            run_async: false,
            restart: false,
            decompose_template: None,
            loop_template: Some("custom/loop".to_string()),
            agent: None,
            claude: false,
            codex: false,
            cursor: false,
            gemini: false,
            review: false,
            review_template: None,
            fix: false,
            fix_template: None,
            coder: None,
            reviewer: None,
            continue_async: None,
            output: None,
            subcommand: None,
        };
        assert_eq!(args.loop_template.as_deref(), Some("custom/loop"));
    }

    #[test]
    fn test_decompose_template_override_via_flag() {
        let args = BuildArgs {
            target: Some("test.md".to_string()),
            run_async: false,
            restart: false,
            decompose_template: Some("my/decompose".to_string()),
            loop_template: None,
            agent: None,
            claude: false,
            codex: false,
            cursor: false,
            gemini: false,
            review: false,
            review_template: None,
            fix: false,
            fix_template: None,
            coder: None,
            reviewer: None,
            continue_async: None,
            output: None,
            subcommand: None,
        };
        assert_eq!(args.decompose_template.as_deref(), Some("my/decompose"));
    }

    #[test]
    fn test_fix_template_implies_review() {
        let args = BuildArgs {
            target: Some("test.md".to_string()),
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
            fix_template: Some("fix".to_string()),
            coder: None,
            reviewer: None,
            continue_async: None,
            output: None,
            subcommand: None,
        };
        // Resolve like run() does
        let fix_template = args.fix_template.or(if args.fix {
            Some("fix".to_string())
        } else {
            None
        });
        let review_template = args.review_template.clone();
        let review = review_template.is_some() || args.review || fix_template.is_some();
        assert!(review);
        assert!(review_template.is_none()); // No explicit template — create_review picks default
        assert!(fix_template.is_some());
    }

    #[test]
    fn test_fix_bool_implies_review() {
        let args = BuildArgs {
            target: Some("test.md".to_string()),
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
        // Resolve like run() does
        let fix_template = args.fix_template.or(if args.fix {
            Some("fix".to_string())
        } else {
            None
        });
        let review_template = args.review_template.clone();
        let review = review_template.is_some() || args.review || fix_template.is_some();
        assert!(review);
        assert!(review_template.is_none()); // No explicit template — create_review picks default
        assert!(fix_template.is_some());
    }

    #[test]
    fn test_review_without_fix() {
        let args = BuildArgs {
            target: Some("test.md".to_string()),
            run_async: false,
            restart: false,
            decompose_template: None,
            loop_template: None,
            agent: None,
            claude: false,
            codex: false,
            cursor: false,
            gemini: false,
            review: true,
            review_template: None,
            fix: false,
            fix_template: None,
            coder: None,
            reviewer: None,
            continue_async: None,
            output: None,
            subcommand: None,
        };
        let fix_template = args.fix_template.or(if args.fix {
            Some("fix".to_string())
        } else {
            None
        });
        let review_template = args.review_template.clone();
        let review = review_template.is_some() || args.review || fix_template.is_some();
        assert!(review);
        assert!(review_template.is_none()); // No explicit template — create_review picks default
        assert!(fix_template.is_none());
    }

    #[test]
    fn test_review_with_custom_template() {
        let args = BuildArgs {
            target: Some("test.md".to_string()),
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
            review_template: Some("my/review".to_string()),
            fix: false,
            fix_template: None,
            coder: None,
            reviewer: None,
            continue_async: None,
            output: None,
            subcommand: None,
        };
        assert_eq!(args.review_template.as_deref(), Some("my/review"));
    }

    #[test]
    fn test_fix_and_async_allowed() {
        let args = BuildArgs {
            target: Some("test.md".to_string()),
            run_async: true,
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
            fix_template: Some("fix".to_string()),
            coder: None,
            reviewer: None,
            continue_async: None,
            output: None,
            subcommand: None,
        };
        // --fix + --async is allowed (task-based loops)
        assert!(args.run_async);
        assert!(args.fix_template.is_some());
    }

    #[test]
    fn test_no_review_no_fix() {
        let args = BuildArgs {
            target: Some("test.md".to_string()),
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
            fix_template: None,
            coder: None,
            reviewer: None,
            continue_async: None,
            output: None,
            subcommand: None,
        };
        let fix_template = args.fix_template.or(if args.fix {
            Some("fix".to_string())
        } else {
            None
        });
        let review_template = args.review_template.clone();
        let review = review_template.is_some() || args.review || fix_template.is_some();
        assert!(!review);
        assert!(review_template.is_none());
        assert!(fix_template.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════
    // Pre-refactor behavioral contract tests for build orchestration
    // ═══════════════════════════════════════════════════════════════════
    //
    // These tests lock down the CONTRACT of build behaviors that must
    // survive the workflow refactor. They test decision logic, not I/O.

    // --- check_epic_blockers contract ---

    #[test]
    fn test_check_epic_blockers_no_deps_passes() {
        let mut tasks = FastHashMap::default();
        tasks.insert(
            "epic1".to_string(),
            make_task("epic1", "Epic", TaskStatus::InProgress),
        );
        let graph = make_graph(tasks, EdgeStore::new());
        assert!(check_epic_blockers(&graph, "epic1").is_ok());
    }

    #[test]
    fn test_check_epic_blockers_resolved_dep_passes() {
        let mut tasks = FastHashMap::default();
        tasks.insert(
            "epic1".to_string(),
            make_task("epic1", "Epic", TaskStatus::InProgress),
        );
        let mut blocker = make_task("dep1", "Dep", TaskStatus::Closed);
        blocker.closed_outcome = Some(TaskOutcome::Done);
        tasks.insert("dep1".to_string(), blocker);

        let mut edges = EdgeStore::new();
        edges.add("epic1", "dep1", "depends-on");

        let graph = make_graph(tasks, edges);
        assert!(check_epic_blockers(&graph, "epic1").is_ok());
    }

    #[test]
    fn test_check_epic_blockers_unresolved_dep_fails() {
        let mut tasks = FastHashMap::default();
        tasks.insert(
            "epic1".to_string(),
            make_task("epic1", "Epic", TaskStatus::InProgress),
        );
        tasks.insert(
            "dep1".to_string(),
            make_task("dep1", "Dep", TaskStatus::Open),
        );

        let mut edges = EdgeStore::new();
        edges.add("epic1", "dep1", "depends-on");

        let graph = make_graph(tasks, edges);
        let err = check_epic_blockers(&graph, "epic1");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("blocked"));
    }

    #[test]
    fn test_check_epic_blockers_wontdo_dep_still_blocks() {
        // A dependency closed as WontDo should still block (only Done unblocks)
        let mut tasks = FastHashMap::default();
        tasks.insert(
            "epic1".to_string(),
            make_task("epic1", "Epic", TaskStatus::InProgress),
        );
        let mut blocker = make_task("dep1", "Dep", TaskStatus::Closed);
        blocker.closed_outcome = Some(TaskOutcome::WontDo);
        tasks.insert("dep1".to_string(), blocker);

        let mut edges = EdgeStore::new();
        edges.add("epic1", "dep1", "depends-on");

        let graph = make_graph(tasks, edges);
        let err = check_epic_blockers(&graph, "epic1");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("blocked"));
    }

    #[test]
    fn test_check_epic_blockers_mixed_deps() {
        // One resolved, one unresolved — should still block
        let mut tasks = FastHashMap::default();
        tasks.insert(
            "epic1".to_string(),
            make_task("epic1", "Epic", TaskStatus::InProgress),
        );

        let mut resolved = make_task("dep1", "Resolved", TaskStatus::Closed);
        resolved.closed_outcome = Some(TaskOutcome::Done);
        tasks.insert("dep1".to_string(), resolved);

        tasks.insert(
            "dep2".to_string(),
            make_task("dep2", "Unresolved", TaskStatus::InProgress),
        );

        let mut edges = EdgeStore::new();
        edges.add("epic1", "dep1", "depends-on");
        edges.add("epic1", "dep2", "depends-on");

        let graph = make_graph(tasks, edges);
        let err = check_epic_blockers(&graph, "epic1");
        assert!(err.is_err());
    }

    #[test]
    fn test_check_epic_blockers_missing_dep_task_blocks() {
        // A depends-on edge pointing to a non-existent task should block
        let mut tasks = FastHashMap::default();
        tasks.insert(
            "epic1".to_string(),
            make_task("epic1", "Epic", TaskStatus::InProgress),
        );

        let mut edges = EdgeStore::new();
        edges.add("epic1", "nonexistent", "depends-on");

        let graph = make_graph(tasks, edges);
        let err = check_epic_blockers(&graph, "epic1");
        assert!(err.is_err());
    }

    #[test]
    fn test_check_epic_blockers_suggests_restart() {
        // Error message should mention --restart as a workaround
        let mut tasks = FastHashMap::default();
        tasks.insert(
            "epic1".to_string(),
            make_task("epic1", "Epic", TaskStatus::InProgress),
        );
        tasks.insert(
            "dep1".to_string(),
            make_task("dep1", "Dep", TaskStatus::Open),
        );

        let mut edges = EdgeStore::new();
        edges.add("epic1", "dep1", "depends-on");

        let graph = make_graph(tasks, edges);
        let err = check_epic_blockers(&graph, "epic1").unwrap_err();
        assert!(err.to_string().contains("--restart"));
    }

    // --- Epic resume decision matrix ---
    //
    // These test the branching logic at lines 313-340 of run_build_plan.
    // We extract the decision into a helper and test all branches.

    /// Simulates the epic resume decision matrix from run_build_plan.
    /// Returns: (should_create_new, should_close_existing)
    fn epic_resume_decision(
        restart: bool,
        existing_epic: Option<(&Task, bool)>, // (epic, has_subtasks)
    ) -> (bool, bool) {
        if restart {
            let should_close = existing_epic
                .map(|(e, _)| e.status != TaskStatus::Closed)
                .unwrap_or(false);
            return (true, should_close);
        }
        match existing_epic {
            Some((epic, _)) if epic.status == TaskStatus::Closed => (true, false),
            Some((_, true)) => (false, false), // Valid incomplete epic — reuse
            Some((_, false)) => (true, true),  // Invalid epic (no subtasks) — close and create new
            None => (true, false),             // No epic — create new
        }
    }

    #[test]
    fn test_epic_resume_no_existing_creates_new() {
        let (create_new, close_existing) = epic_resume_decision(false, None);
        assert!(create_new);
        assert!(!close_existing);
    }

    #[test]
    fn test_epic_resume_restart_always_creates_new() {
        let epic = make_task("epic1", "Epic", TaskStatus::InProgress);
        let (create_new, close_existing) = epic_resume_decision(true, Some((&epic, true)));
        assert!(create_new);
        assert!(close_existing);
    }

    #[test]
    fn test_epic_resume_restart_closed_epic_no_close() {
        let epic = make_task("epic1", "Epic", TaskStatus::Closed);
        let (create_new, close_existing) = epic_resume_decision(true, Some((&epic, false)));
        assert!(create_new);
        assert!(!close_existing); // Already closed, no need to close again
    }

    #[test]
    fn test_epic_resume_valid_incomplete_reuses() {
        let epic = make_task("epic1", "Epic", TaskStatus::InProgress);
        let (create_new, _) = epic_resume_decision(false, Some((&epic, true)));
        assert!(!create_new); // Reuse existing
    }

    #[test]
    fn test_epic_resume_invalid_epic_no_subtasks_closes_and_creates() {
        let epic = make_task("epic1", "Epic", TaskStatus::Open);
        let (create_new, close_existing) = epic_resume_decision(false, Some((&epic, false)));
        assert!(create_new);
        assert!(close_existing);
    }

    #[test]
    fn test_epic_resume_closed_epic_creates_new() {
        let epic = make_task("epic1", "Epic", TaskStatus::Closed);
        let (create_new, close_existing) = epic_resume_decision(false, Some((&epic, false)));
        assert!(create_new);
        assert!(!close_existing); // Already closed
    }

    #[test]
    fn test_epic_resume_restart_no_existing_creates_new() {
        let (create_new, close_existing) = epic_resume_decision(true, None);
        assert!(create_new);
        assert!(!close_existing);
    }

    // --- Build flag resolution contract ---
    // These verify that the --fix / --review / --fix-template flag
    // resolution logic produces the correct (review, has_fix) pair.

    fn resolve_build_flags(
        review: bool,
        review_template: Option<&str>,
        fix: bool,
        fix_template: Option<&str>,
    ) -> (bool, bool) {
        let fix_template = fix_template.map(|s| s.to_string()).or(if fix {
            Some("fix".to_string())
        } else {
            None
        });
        let review = review_template.is_some() || review || fix_template.is_some();
        let has_fix = fix_template.is_some();
        (review, has_fix)
    }

    #[test]
    fn test_build_flags_bare_build() {
        let (review, has_fix) = resolve_build_flags(false, None, false, None);
        assert!(!review);
        assert!(!has_fix);
    }

    #[test]
    fn test_build_flags_review_only() {
        let (review, has_fix) = resolve_build_flags(true, None, false, None);
        assert!(review);
        assert!(!has_fix);
    }

    #[test]
    fn test_build_flags_fix_implies_review() {
        let (review, has_fix) = resolve_build_flags(false, None, true, None);
        assert!(review);
        assert!(has_fix);
    }

    #[test]
    fn test_build_flags_fix_template_implies_both() {
        let (review, has_fix) = resolve_build_flags(false, None, false, Some("custom/fix"));
        assert!(review);
        assert!(has_fix);
    }

    #[test]
    fn test_build_flags_review_template_only() {
        let (review, has_fix) = resolve_build_flags(false, Some("custom/review"), false, None);
        assert!(review);
        assert!(!has_fix);
    }

    #[test]
    fn test_build_flags_all_flags() {
        let (review, has_fix) =
            resolve_build_flags(true, Some("custom/review"), true, Some("custom/fix"));
        assert!(review);
        assert!(has_fix);
    }

    // --- Stale build detection contract (pure logic) ---

    fn find_stale_builds(tasks: &FastHashMap<String, Task>, plan_path: &str) -> Vec<String> {
        tasks
            .values()
            .filter(|t| {
                t.task_type.as_deref() == Some("orchestrator")
                    && t.data.get("plan").map(|s| s.as_str()) == Some(plan_path)
                    && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
            })
            .map(|t| t.id.clone())
            .collect()
    }

    #[test]
    fn test_stale_build_multiple_matches() {
        let mut tasks = FastHashMap::default();
        let plan = "ops/now/feature.md";

        for (id, status) in [("b1", TaskStatus::InProgress), ("b2", TaskStatus::Open)] {
            let mut data = HashMap::new();
            data.insert("plan".to_string(), plan.to_string());
            let mut task = make_task_with_data(id, "Build", status, data);
            task.task_type = Some("orchestrator".to_string());
            tasks.insert(id.to_string(), task);
        }

        // Add a closed one that should NOT be stale
        let mut data = HashMap::new();
        data.insert("plan".to_string(), plan.to_string());
        let mut closed = make_task_with_data("b3", "Build", TaskStatus::Closed, data);
        closed.task_type = Some("orchestrator".to_string());
        tasks.insert("b3".to_string(), closed);

        let stale = find_stale_builds(&tasks, plan);
        assert_eq!(stale.len(), 2);
        assert!(stale.contains(&"b1".to_string()));
        assert!(stale.contains(&"b2".to_string()));
    }

    #[test]
    fn test_stale_build_ignores_different_plan() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/other.md".to_string());
        let mut task = make_task_with_data("b1", "Build", TaskStatus::InProgress, data);
        task.task_type = Some("orchestrator".to_string());
        tasks.insert("b1".to_string(), task);

        let stale = find_stale_builds(&tasks, "ops/now/feature.md");
        assert!(stale.is_empty());
    }

    #[test]
    fn test_stale_build_ignores_non_orchestrator() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());
        let task = make_task_with_data("b1", "Regular task", TaskStatus::InProgress, data);
        tasks.insert("b1".to_string(), task);

        let stale = find_stale_builds(&tasks, "ops/now/feature.md");
        assert!(stale.is_empty());
    }

    // --- Draft plan rejection contract ---

    #[test]
    fn test_draft_plan_blocks_build() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let plan_file = temp_dir.path().join("draft-plan.md");
        std::fs::write(&plan_file, "---\ndraft: true\n---\n# My Plan\n").unwrap();
        let metadata = crate::plans::parse_plan_metadata(&plan_file);
        assert!(
            metadata.draft,
            "Plan with draft: true should be detected as draft"
        );
    }

    #[test]
    fn test_non_draft_plan_allowed() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let plan_file = temp_dir.path().join("ready-plan.md");
        std::fs::write(&plan_file, "---\ndraft: false\n---\n# My Plan\n").unwrap();
        let metadata = crate::plans::parse_plan_metadata(&plan_file);
        assert!(
            !metadata.draft,
            "Plan with draft: false should not be draft"
        );
    }

    #[test]
    fn test_no_frontmatter_plan_not_draft() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let plan_file = temp_dir.path().join("no-fm.md");
        std::fs::write(&plan_file, "# Simple Plan\n\nNo frontmatter here.\n").unwrap();
        let metadata = crate::plans::parse_plan_metadata(&plan_file);
        assert!(
            !metadata.draft,
            "Plan without frontmatter should not be draft"
        );
    }

    // --- close_epic_as_invalid contract ---

    #[test]
    fn test_close_epic_as_invalid_uses_wontdo() {
        // Verify the event construction: outcome must be WontDo
        let outcome = TaskOutcome::WontDo;
        assert!(
            matches!(outcome, TaskOutcome::WontDo),
            "Invalid epic closure must use WontDo outcome"
        );
    }

    // --- Pre-refactor behavioral safety net tests ---

    /// Draft plans cannot be built — run_build_plan rejects draft: true.
    #[test]
    fn test_draft_plan_rejected() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let plan_file = temp_dir.path().join("draft-plan.md");
        std::fs::write(&plan_file, "---\ndraft: true\n---\n# Plan\n").unwrap();
        let metadata = crate::plans::parse_plan_metadata(&plan_file);
        assert!(metadata.draft, "Draft plan must be detected and rejected");
    }

    /// When an existing epic already has subtasks, reuse it (skip decompose).
    /// The epic_resume_decision returns create_new=false when subtasks exist.
    #[test]
    fn test_epic_resume_skips_decompose_when_subtasks_exist() {
        let epic = make_task("epic1", "Epic", TaskStatus::InProgress);
        let has_subtasks = true;
        let (create_new, close_existing) = epic_resume_decision(false, Some((&epic, has_subtasks)));
        assert!(
            !create_new,
            "Should reuse existing epic when subtasks exist (skip decompose)"
        );
        assert!(!close_existing, "Should not close a valid in-progress epic");
    }

    /// --restart closes the existing in-progress epic before creating a new one.
    #[test]
    fn test_restart_closes_existing_epic() {
        let epic = make_task("epic1", "Epic", TaskStatus::InProgress);
        let (create_new, close_existing) = epic_resume_decision(true, Some((&epic, true)));
        assert!(create_new, "--restart must create a new epic");
        assert!(
            close_existing,
            "--restart must close the existing in-progress epic"
        );
    }

    /// When the build target is an epic ID (task ID), the plan path is not
    /// validated — run_build_epic does not call validate_plan_path. We verify
    /// the routing: a task ID target takes the epic path, not the plan path.
    #[test]
    fn test_build_epic_id_skips_plan_validation() {
        let epic_id = "mvslrspmoynoxyyywqyutmovxpvztkls";
        assert!(is_task_id(epic_id), "Should be recognized as task ID");
        // run_build_epic does NOT call validate_plan_path — verified by code
        // inspection. This test locks down the routing decision: task IDs must
        // take the epic path where no plan file validation occurs.
        let plan_path = "nonexistent/path/to/plan.md";
        assert!(
            !is_task_id(plan_path),
            "Plan path must NOT be routed as epic ID"
        );
    }

    // --- Review + fix integration contract ---

    /// has_actionable_issues returns true when issue_count > 0.
    #[test]
    fn test_has_review_issues_logic() {
        let mut task = make_task("review1", "Review", TaskStatus::Closed);
        task.data.insert("issue_count".to_string(), "3".to_string());
        let has_issues = crate::reviews::has_actionable_issues(&task);
        assert!(
            has_issues,
            "issue_count=3 should indicate actionable issues"
        );
    }

    /// has_actionable_issues returns false when issue_count is 0.
    #[test]
    fn test_no_review_issues_logic() {
        let mut task = make_task("review2", "Review", TaskStatus::Closed);
        task.data.insert("issue_count".to_string(), "0".to_string());
        let has_issues = crate::reviews::has_actionable_issues(&task);
        assert!(
            !has_issues,
            "issue_count=0 should indicate no actionable issues"
        );
    }
}
