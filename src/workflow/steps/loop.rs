use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::env;
use std::path::Path;
#[cfg(test)]
use std::process::Command;

use super::StepResult;
use super::WorkflowChange;
use super::WorkflowContext;
use crate::agents::AgentType;
use crate::error::{AikiError, Result};
use crate::tasks::md::MdBuilder;
use crate::tasks::runner::{
    handle_session_result, task_run, task_run_async, task_run_on_session, TaskRunOptions,
};
use crate::tasks::{
    find_task, get_subtasks, materialize_graph, read_events, start_task_core, TaskStatus,
};
use crate::workflow::{OutputKind, WorkflowOutput};

/// Options for `run_loop()`
pub struct LoopOptions {
    /// Run asynchronously (return immediately)
    pub run_async: bool,
    /// Agent type override
    pub agent: Option<AgentType>,
    /// Template name override (default: "loop")
    pub template: Option<String>,
}

impl LoopOptions {
    pub fn new() -> Self {
        Self {
            run_async: false,
            agent: None,
            template: None,
        }
    }

    pub fn with_async(mut self, run_async: bool) -> Self {
        self.run_async = run_async;
        self
    }

    pub fn with_agent(mut self, agent: AgentType) -> Self {
        self.agent = Some(agent);
        self
    }

    pub fn with_template(mut self, template: String) -> Self {
        self.template = Some(template);
        self
    }
}

/// Shared loop function used by `aiki loop`, `build.rs`, and `fix.rs`.
///
/// Creates a loop task from the `loop` template with `data.target` set to
/// the parent task ID, writes an `orchestrates` link from the loop task to the
/// parent, and runs the task.
///
/// When `ctx` is provided with an active `notify_rx`, uses spawn_monitored +
/// event drain loop to show subtask progress in real-time. Otherwise falls
/// back to `task_run()` or TUI.
///
/// Returns the loop task ID.
pub fn run_loop(
    cwd: &Path,
    parent_id: &str,
    options: LoopOptions,
    show_tui: bool,
    output: OutputKind,
    ctx: Option<&mut WorkflowContext>,
) -> Result<String> {
    let output_ctx = WorkflowOutput::new(output);

    // Validate parent task exists and has subtasks
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let parent = find_task(&graph.tasks, parent_id)?;
    let parent_id = parent.id.clone(); // resolve prefix to canonical ID

    let subtasks = get_subtasks(&graph, &parent_id);
    if subtasks.is_empty() {
        return Err(AikiError::InvalidArgument(format!(
            "Parent task {} has no subtasks. Nothing to loop over.",
            &parent_id[..parent_id.len().min(8)]
        )));
    }

    // Start the parent task if not already in progress
    if parent.status != TaskStatus::InProgress {
        start_task_core(cwd, &[parent_id.clone()])?;
    }

    // Create loop task from loop template
    let mut data = HashMap::new();
    data.insert("target".to_string(), parent_id.clone());

    let assignee = options
        .agent
        .as_ref()
        .map(|a| a.as_str().to_string())
        .or_else(|| Some("claude-code".to_string()));

    let params = crate::commands::task::TemplateTaskParams {
        template_name: options.template.unwrap_or_else(|| "loop".to_string()),
        data,
        assignee,
        ..Default::default()
    };

    let loop_task_id = crate::commands::task::create_from_template(cwd, params)?;

    // Write orchestrates link: loop task → parent
    let events = crate::tasks::storage::read_events(cwd)?;
    let graph = crate::tasks::graph::materialize_graph(&events);
    crate::tasks::storage::write_link_event(
        cwd,
        &graph,
        "orchestrates",
        &loop_task_id,
        &parent_id,
    )?;

    // Run the loop task
    let task_run_options = if let Some(agent) = options.agent {
        TaskRunOptions::new().with_agent(agent)
    } else {
        TaskRunOptions::new()
    };

    if show_tui && !options.run_async {
        let session_result = task_run_on_session(cwd, &loop_task_id, task_run_options, true)?;
        handle_session_result(cwd, &loop_task_id, session_result, true)?;
    } else if options.run_async {
        let _handle = task_run_async(cwd, &loop_task_id, task_run_options)?;
        emit_loop_async(output_ctx, &loop_task_id, &parent_id);
    } else if let Some(ctx) = ctx {
        if ctx.notify_rx.is_some() {
            let output = ctx.output;
            let mut handler = LoopDrainHandler {
                task_names: &mut ctx.task_names,
                parent_id: parent_id.clone(),
                output,
            };
            super::spawn_drain_finalize(
                cwd,
                &loop_task_id,
                &task_run_options,
                ctx.notify_rx.as_ref(),
                output,
                &mut handler,
            )?;
        } else {
            emit_loop_started(output_ctx, &loop_task_id, &parent_id);
            task_run(cwd, &loop_task_id, task_run_options.quiet())?;
            emit_loop_completed(output_ctx, &loop_task_id, &parent_id);
        }
    } else {
        emit_loop_started(output_ctx, &loop_task_id, &parent_id);
        task_run(cwd, &loop_task_id, task_run_options.quiet())?;
        emit_loop_completed(output_ctx, &loop_task_id, &parent_id);
    }

    Ok(loop_task_id)
}

/// Drain handler for the loop step: displays task lifecycle events
/// (started, completed, skipped, failed) in real-time.
/// Filters events to only include subtasks of the parent epic.
struct LoopDrainHandler<'a> {
    task_names: &'a mut HashMap<String, String>,
    parent_id: String,
    output: WorkflowOutput,
}

impl super::DrainHandler for LoopDrainHandler<'_> {
    fn on_change(&mut self, delta: &super::GraphDelta) {
        use crate::tasks::types::{TaskOutcome, TaskStatus};

        // Build set of child IDs for the parent epic to filter events.
        let child_ids: HashSet<&str> = delta
            .next
            .children_of(&self.parent_id)
            .iter()
            .map(|t| t.id.as_str())
            .collect();

        // Record names from newly created tasks (only epic's children).
        for task in &delta.new_tasks {
            if child_ids.contains(task.id.as_str()) {
                self.task_names.insert(task.id.clone(), task.name.clone());
            }
        }

        // Display status transitions (only epic's children).
        for sc in delta.status_changes.iter().filter(|sc| child_ids.contains(sc.task.id.as_str())) {
            let name = self
                .task_names
                .get(&sc.task.id)
                .map(|s| s.as_str())
                .unwrap_or(&sc.task.id);

            match sc.next_status {
                TaskStatus::InProgress => {
                    self.output.emit(&format!("  \u{25b8} {}", name));
                }
                TaskStatus::Closed => match sc.task.closed_outcome {
                    Some(TaskOutcome::Done) => {
                        self.output
                            .emit(&format!("  \u{2714} {} \u{2014} done", name));
                    }
                    Some(TaskOutcome::WontDo) => {
                        self.output
                            .emit(&format!("  \u{2298} {} \u{2014} skipped", name));
                    }
                    None => {
                        self.output
                            .emit(&format!("  \u{2714} {} \u{2014} done", name));
                    }
                },
                TaskStatus::Stopped => {
                    self.output
                        .emit(&format!("  \u{2718} {} \u{2014} failed", name));
                }
                _ => {}
            }
        }
    }
}

/// Loop step: run the orchestration loop over epic subtasks.
pub(crate) fn run(ctx: &mut WorkflowContext) -> anyhow::Result<StepResult> {
    let epic_id = ctx.require_task_id()?.to_string();

    ctx.status("preparing loop options");
    let mut loop_options = LoopOptions::new();
    if let Some(agent) = ctx.opts.agent {
        loop_options = loop_options.with_agent(agent);
    }
    if let Some(ref tmpl) = ctx.opts.loop_template {
        loop_options = loop_options.with_template(tmpl.clone());
    }

    ctx.status("running subtask loop");
    let cwd = ctx.cwd.clone();
    let output_kind = ctx.output.kind();
    let loop_task_id = run_loop(&cwd, &epic_id, loop_options, false, output_kind, Some(ctx))?;

    Ok(StepResult {
        change: WorkflowChange::None,
        message: "All lanes complete".to_string(),
        task_id: Some(loop_task_id),
    })
}

/// Format loop started message.
fn format_loop_started(loop_id: &str, parent_id: &str) -> String {
    let content = format!(
        "## Loop Started\n- **Loop ID:** {}\n- **Parent ID:** {}\n",
        loop_id, parent_id
    );
    MdBuilder::new().build(&content)
}

fn emit_loop_started(output: WorkflowOutput, loop_id: &str, parent_id: &str) {
    output.emit(&format_loop_started(loop_id, parent_id));
}

/// Format loop completed message.
fn format_loop_completed(loop_id: &str, parent_id: &str) -> String {
    let content = format!(
        "## Loop Completed\n- **Loop ID:** {}\n- **Parent ID:** {}\n",
        loop_id, parent_id
    );
    MdBuilder::new().build(&content)
}

fn emit_loop_completed(output: WorkflowOutput, loop_id: &str, parent_id: &str) {
    output.emit(&format_loop_completed(loop_id, parent_id));
}

/// Format loop async message.
fn format_loop_async(loop_id: &str, parent_id: &str) -> String {
    let content = format!(
        "## Loop Started\n- **Loop ID:** {}\n- **Parent ID:** {}\n- Loop started in background.\n",
        loop_id, parent_id
    );
    MdBuilder::new().build(&content)
}

fn emit_loop_async(output: WorkflowOutput, loop_id: &str, parent_id: &str) {
    output.emit(&format_loop_async(loop_id, parent_id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::TaskGraph;
    use crate::tasks::types::Task;
    use crate::workflow::steps::DrainHandler;
    use crate::workflow::OutputKind;

    fn run_loop_output_probe(case: &str) -> String {
        let exe = env::current_exe().expect("resolve current test binary");
        let output = Command::new(exe)
            .arg("--exact")
            .arg("workflow::steps::r#loop::tests::loop_output_probe")
            .arg("--nocapture")
            .env("AIKI_LOOP_OUTPUT_TEST_CASE", case)
            .output()
            .expect("run loop output probe");

        assert!(
            output.status.success(),
            "probe failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8_lossy(&output.stderr).to_string()
    }

    #[test]
    fn loop_output_probe() {
        let Some(case) = env::var("AIKI_LOOP_OUTPUT_TEST_CASE").ok() else {
            return;
        };

        let output = if case.starts_with("quiet") {
            WorkflowOutput::new(OutputKind::Quiet)
        } else {
            WorkflowOutput::new(OutputKind::Text)
        };

        match case.as_str() {
            "text-started" | "quiet-started" => emit_loop_started(output, "loop123", "parent456"),
            "text-completed" | "quiet-completed" => {
                emit_loop_completed(output, "loop123", "parent456")
            }
            "text-async" | "quiet-async" => emit_loop_async(output, "loop123", "parent456"),
            other => panic!("unknown loop output probe case: {other}"),
        }
    }

    #[test]
    fn format_loop_started_includes_loop_and_parent_ids() {
        let rendered = format_loop_started("loop123", "parent456");
        assert!(rendered.contains("Loop Started"));
        assert!(rendered.contains("loop123"));
        assert!(rendered.contains("parent456"));
    }

    #[test]
    fn format_loop_completed_includes_loop_and_parent_ids() {
        let rendered = format_loop_completed("loop123", "parent456");
        assert!(rendered.contains("Loop Completed"));
        assert!(rendered.contains("loop123"));
        assert!(rendered.contains("parent456"));
    }

    #[test]
    fn format_loop_async_mentions_background_execution() {
        let rendered = format_loop_async("loop123", "parent456");
        assert!(rendered.contains("Loop Started"));
        assert!(rendered.contains("loop123"));
        assert!(rendered.contains("parent456"));
        assert!(rendered.contains("background"));
    }

    #[test]
    fn loop_started_output_respects_output_kind() {
        let text_output = run_loop_output_probe("text-started");
        let quiet_output = run_loop_output_probe("quiet-started");

        assert!(text_output.contains("Loop Started"));
        assert!(text_output.contains("loop123"));
        assert!(text_output.contains("parent456"));
        assert!(!quiet_output.contains("Loop Started"));
    }

    #[test]
    fn loop_completed_output_respects_output_kind() {
        let text_output = run_loop_output_probe("text-completed");
        let quiet_output = run_loop_output_probe("quiet-completed");

        assert!(text_output.contains("Loop Completed"));
        assert!(text_output.contains("loop123"));
        assert!(text_output.contains("parent456"));
        assert!(!quiet_output.contains("Loop Completed"));
    }

    #[test]
    fn loop_async_output_respects_output_kind() {
        let text_output = run_loop_output_probe("text-async");
        let quiet_output = run_loop_output_probe("quiet-async");

        assert!(text_output.contains("Loop Started"));
        assert!(text_output.contains("background"));
        assert!(text_output.contains("loop123"));
        assert!(text_output.contains("parent456"));
        assert!(!quiet_output.contains("Loop Started"));
        assert!(!quiet_output.contains("background"));
    }

    /// Helper: build a minimal Task for test graph construction.
    fn make_test_task(id: &str, name: &str, status: TaskStatus) -> Task {
        use crate::tasks::types::TaskPriority;
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

    /// Helper: build an empty TaskGraph.
    fn empty_graph() -> TaskGraph {
        TaskGraph {
            tasks: Default::default(),
            edges: crate::tasks::graph::EdgeStore::new(),
            slug_index: Default::default(),
        }
    }

    /// Verify the handler records new task names from GraphDelta.new_tasks.
    #[test]
    fn loop_drain_records_new_task_names_from_delta() {
        use crate::tasks::graph::{GraphDelta, StatusChange};
        use crate::tasks::types::TaskStatus;

        let mut task_names: HashMap<String, String> = HashMap::new();
        let output = WorkflowOutput::new(OutputKind::Quiet);
        let mut handler = LoopDrainHandler {
            task_names: &mut task_names,
            parent_id: "parent_epic".to_string(),
            output,
        };

        let prev = empty_graph();
        let mut next = empty_graph();
        let task = make_test_task("sub_004", "New runtime task", TaskStatus::Open);
        next.tasks.insert("sub_004".to_string(), task);
        next.edges.add("sub_004", "parent_epic", "subtask-of");

        let delta = GraphDelta {
            prev: &prev,
            next: &next,
            new_tasks: vec![next.tasks.get("sub_004").unwrap()],
            status_changes: vec![],
            new_comments: vec![],
            new_edges: vec![],
        };
        handler.on_change(&delta);

        assert_eq!(task_names.get("sub_004").unwrap(), "New runtime task");
    }

    /// Verify the handler emits correct output for task lifecycle via GraphDelta.
    /// Uses the subprocess probe pattern to capture stderr output.
    #[test]
    fn loop_drain_emits_correct_output_for_task_lifecycle() {
        let output = run_loop_drain_probe("lifecycle");

        // Verify all lifecycle status lines
        assert!(output.contains("  \u{25b8} Fix auth bug"), "missing started line");
        assert!(
            output.contains("  \u{2714} Fix auth bug \u{2014} done"),
            "missing done line"
        );
        assert!(
            output.contains("  \u{2298} Add error handling \u{2014} skipped"),
            "missing skipped line"
        );
        assert!(
            output.contains("  \u{2718} Remove deprecated API \u{2014} failed"),
            "missing failed line"
        );
    }

    /// Verify that unknown task IDs fall back to the raw ID in output.
    #[test]
    fn loop_drain_uses_raw_id_for_unknown_tasks() {
        let output = run_loop_drain_probe("unknown-id");
        assert!(
            output.contains("  \u{25b8} unknown_id"),
            "missing fallback to raw ID"
        );
    }

    /// Subprocess probe for drain handler tests, similar to the loop output probes above.
    fn run_loop_drain_probe(case: &str) -> String {
        let exe = env::current_exe().expect("resolve current test binary");
        let output = Command::new(exe)
            .arg("--exact")
            .arg("workflow::steps::r#loop::tests::loop_drain_probe")
            .arg("--nocapture")
            .env("AIKI_LOOP_DRAIN_TEST_CASE", case)
            .output()
            .expect("run loop drain probe");

        assert!(
            output.status.success(),
            "probe failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8_lossy(&output.stderr).to_string()
    }

    /// Probe entry point — only runs when AIKI_LOOP_DRAIN_TEST_CASE is set.
    #[test]
    fn loop_drain_probe() {
        use crate::tasks::graph::{GraphDelta, StatusChange};
        use crate::tasks::types::{TaskOutcome, TaskStatus};

        let Some(case) = env::var("AIKI_LOOP_DRAIN_TEST_CASE").ok() else {
            return;
        };

        let output = WorkflowOutput::new(OutputKind::Text);

        match case.as_str() {
            "lifecycle" => {
                let mut task_names: HashMap<String, String> = HashMap::new();
                // Pre-populate (simulating decompose step seeding)
                task_names.insert("sub_001".to_string(), "Fix auth bug".to_string());
                task_names.insert("sub_002".to_string(), "Add error handling".to_string());
                task_names
                    .insert("sub_003".to_string(), "Remove deprecated API".to_string());

                let parent_id = "parent_epic".to_string();
                let mut handler = LoopDrainHandler {
                    task_names: &mut task_names,
                    parent_id: parent_id.clone(),
                    output,
                };

                // Helper: add subtask-of edges for given task IDs in a graph.
                fn add_child_edges(graph: &mut TaskGraph, parent: &str, children: &[&str]) {
                    for child in children {
                        graph.edges.add(child, parent, "subtask-of");
                    }
                }

                // Delta 1: new task created
                let g1_prev = empty_graph();
                let mut g1_next = empty_graph();
                g1_next.tasks.insert(
                    "sub_004".to_string(),
                    make_test_task("sub_004", "New runtime task", TaskStatus::Open),
                );
                add_child_edges(&mut g1_next, &parent_id, &["sub_004"]);
                let delta = GraphDelta {
                    prev: &g1_prev,
                    next: &g1_next,
                    new_tasks: vec![g1_next.tasks.get("sub_004").unwrap()],
                    status_changes: vec![],
                    new_comments: vec![],
                    new_edges: vec![],
                };
                handler.on_change(&delta);

                // Delta 2: sub_001 started
                let mut g2 = empty_graph();
                let started = make_test_task("sub_001", "Fix auth bug", TaskStatus::InProgress);
                g2.tasks.insert("sub_001".to_string(), started);
                add_child_edges(&mut g2, &parent_id, &["sub_001"]);
                let delta = GraphDelta {
                    prev: &g1_prev,
                    next: &g2,
                    new_tasks: vec![],
                    status_changes: vec![StatusChange {
                        task: g2.tasks.get("sub_001").unwrap(),
                        prev_status: TaskStatus::Open,
                        next_status: TaskStatus::InProgress,
                    }],
                    new_comments: vec![],
                    new_edges: vec![],
                };
                handler.on_change(&delta);

                // Delta 3: sub_001 closed (Done)
                let mut g3 = empty_graph();
                let mut done = make_test_task("sub_001", "Fix auth bug", TaskStatus::Closed);
                done.closed_outcome = Some(TaskOutcome::Done);
                g3.tasks.insert("sub_001".to_string(), done);
                add_child_edges(&mut g3, &parent_id, &["sub_001"]);
                let delta = GraphDelta {
                    prev: &g1_prev,
                    next: &g3,
                    new_tasks: vec![],
                    status_changes: vec![StatusChange {
                        task: g3.tasks.get("sub_001").unwrap(),
                        prev_status: TaskStatus::InProgress,
                        next_status: TaskStatus::Closed,
                    }],
                    new_comments: vec![],
                    new_edges: vec![],
                };
                handler.on_change(&delta);

                // Delta 4: sub_002 closed (WontDo)
                let mut g4 = empty_graph();
                let mut wont_do =
                    make_test_task("sub_002", "Add error handling", TaskStatus::Closed);
                wont_do.closed_outcome = Some(TaskOutcome::WontDo);
                g4.tasks.insert("sub_002".to_string(), wont_do);
                add_child_edges(&mut g4, &parent_id, &["sub_002"]);
                let delta = GraphDelta {
                    prev: &g1_prev,
                    next: &g4,
                    new_tasks: vec![],
                    status_changes: vec![StatusChange {
                        task: g4.tasks.get("sub_002").unwrap(),
                        prev_status: TaskStatus::Open,
                        next_status: TaskStatus::Closed,
                    }],
                    new_comments: vec![],
                    new_edges: vec![],
                };
                handler.on_change(&delta);

                // Delta 5: sub_003 stopped
                let mut g5 = empty_graph();
                g5.tasks.insert(
                    "sub_003".to_string(),
                    make_test_task("sub_003", "Remove deprecated API", TaskStatus::Stopped),
                );
                add_child_edges(&mut g5, &parent_id, &["sub_003"]);
                let delta = GraphDelta {
                    prev: &g1_prev,
                    next: &g5,
                    new_tasks: vec![],
                    status_changes: vec![StatusChange {
                        task: g5.tasks.get("sub_003").unwrap(),
                        prev_status: TaskStatus::Open,
                        next_status: TaskStatus::Stopped,
                    }],
                    new_comments: vec![],
                    new_edges: vec![],
                };
                handler.on_change(&delta);
            }
            "unknown-id" => {
                let mut task_names: HashMap<String, String> = HashMap::new();
                let parent_id = "parent_epic".to_string();
                let mut handler = LoopDrainHandler {
                    task_names: &mut task_names,
                    parent_id: parent_id.clone(),
                    output,
                };

                // Task has a status change but name was never recorded
                let mut g = empty_graph();
                g.tasks.insert(
                    "unknown_id".to_string(),
                    make_test_task("unknown_id", "unknown_id", TaskStatus::InProgress),
                );
                g.edges.add("unknown_id", &parent_id, "subtask-of");
                let delta = GraphDelta {
                    prev: &empty_graph(),
                    next: &g,
                    new_tasks: vec![],
                    status_changes: vec![StatusChange {
                        task: g.tasks.get("unknown_id").unwrap(),
                        prev_status: TaskStatus::Open,
                        next_status: TaskStatus::InProgress,
                    }],
                    new_comments: vec![],
                    new_edges: vec![],
                };
                handler.on_change(&delta);
            }
            other => panic!("unknown loop drain probe case: {other}"),
        }
    }
}
