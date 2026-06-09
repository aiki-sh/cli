pub mod async_run;
pub mod build;
pub mod fix;
pub mod review;
pub mod steps;

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};

use anyhow::Result;
use crossbeam_channel::Receiver;
use serde::{Deserialize, Serialize};

use crate::tasks::storage::{clear_event_pipe_path, set_event_pipe_path};

use crate::agents::AgentType;
use crate::reviews::ReviewScope;
use steps::Step;
#[cfg(test)]
use steps::StepResult;
use steps::WorkflowChange;

/// Unified options bag for all workflow steps.
///
/// Not every workflow uses every field — unused fields are `None`/`false`.
/// Constructed from command-specific opts (`BuildOpts`, `ReviewOpts`, `FixOpts`)
/// when creating a `WorkflowContext`.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct WorkflowOpts {
    // Build options
    pub restart: bool,
    pub decompose_template: Option<String>,
    pub loop_template: Option<String>,
    pub agent: Option<AgentType>,
    #[serde(alias = "review_agent_str")]
    pub reviewer: Option<String>,
    // Review options
    pub target: Option<String>,
    pub code: bool,
    pub review: bool,
    pub review_template: Option<String>,
    pub fix: bool,
    pub fix_template: Option<String>,
    pub autorun: bool,
    // Fix options
    pub plan_template: Option<String>,
    #[serde(alias = "fix_assignee")]
    pub coder: Option<String>,
}

/// Context shared across all steps in a workflow.
pub struct WorkflowContext {
    /// Root task this workflow operates on. Set by Init/Decompose step, read by later steps.
    pub task_id: Option<String>,
    /// Plan path (if applicable). Set at construction or by Init.
    pub plan_path: Option<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Controls how workflow execution reports progress.
    pub(crate) output: WorkflowOutput,
    /// Unified options from the command that launched this workflow.
    pub opts: WorkflowOpts,
    /// Review task ID for fix workflows. Set by workflow runners before execution.
    pub review_id: Option<String>,
    /// Review scope for fix workflows. Set by workflow runners before execution.
    pub scope: Option<ReviewScope>,
    /// Assignee for fix workflows. Set by workflow runners before execution.
    pub assignee: Option<String>,
    /// Current iteration of the quality loop. Starts at 0.
    pub iteration: usize,
    /// Receiver for FIFO change-ID notifications. Present when a FIFO listener
    /// is active. The workflow runner creates a named pipe, spawns a read thread
    /// that forwards each line (a change ID string) to this channel, and shares
    /// it sequentially across steps.
    ///
    /// **Single-consumer constraint:** Each step's drain loop calls
    /// `rx.try_iter()`, which destructively consumes notifications — once a step
    /// drains a notification, no later step can see it. This is intentional:
    /// each step only cares about notifications produced during its own agent
    /// run, and the drain loop in `spawn_drain_finalize` runs only while that
    /// step's agent is alive.
    pub notify_rx: Option<Receiver<String>>,
    /// Best-effort task-id -> task-name cache built from Created events
    /// so later steps can render names without re-reading JJ state.
    pub task_names: HashMap<String, String>,
}

impl WorkflowContext {
    /// Returns task_id or Err(AikiError::InvalidArgument).
    pub fn require_task_id(&self) -> crate::error::Result<&str> {
        self.task_id.as_deref().ok_or_else(|| {
            crate::error::AikiError::InvalidArgument("No task ID in workflow context".to_string())
        })
    }

    /// Returns task_id as Option<&str>.
    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    /// Print a section header.
    pub fn section(&self, name: &str) {
        self.output.section(name);
    }

    /// Print a status/progress message.
    pub fn status(&self, msg: &str) {
        self.output.status(msg);
    }

    /// Print a success message.
    pub fn success(&self, step: &str, msg: &str) {
        self.output.success(step, msg);
    }

    /// Print an error message.
    pub fn error(&self, step: &str, msg: &str) {
        self.output.error(step, msg);
    }

    /// Emit a raw output line when text output is enabled.
    #[allow(dead_code)]
    pub fn emit(&self, msg: &str) {
        self.output.emit(msg);
    }

    /// Print a warning message.
    pub fn warn(&self, msg: &str) {
        self.output.warn(msg);
    }
}

/// Controls how workflow execution reports progress.
#[derive(Clone, Copy)]
pub enum OutputKind {
    /// Sequential on main thread, minimal text output (eprintln status lines).
    Text,
    /// Silent — background/async processes.
    Quiet,
}

/// Output-focused helper for workflow status reporting.
#[derive(Clone, Copy)]
pub struct WorkflowOutput {
    kind: OutputKind,
}

impl WorkflowOutput {
    pub fn new(kind: OutputKind) -> Self {
        Self { kind }
    }

    pub fn kind(self) -> OutputKind {
        self.kind
    }

    pub fn section(self, name: &str) {
        if matches!(self.kind, OutputKind::Text) {
            eprintln!("\n── {} ──", name);
        }
    }

    pub fn status(self, msg: &str) {
        if matches!(self.kind, OutputKind::Text) {
            eprintln!("⠙ {}...", msg);
        }
    }

    pub fn success(self, step: &str, msg: &str) {
        if matches!(self.kind, OutputKind::Text) {
            eprintln!("合 {} — {}", step, msg);
        }
    }

    pub fn error(self, step: &str, msg: &str) {
        if matches!(self.kind, OutputKind::Text) {
            eprintln!("✗ {} — {}", step, msg);
        }
    }

    #[allow(dead_code)]
    pub fn emit(self, msg: &str) {
        if matches!(self.kind, OutputKind::Text) {
            eprintln!("{}", msg);
        }
    }

    pub fn warn(self, msg: &str) {
        if matches!(self.kind, OutputKind::Text) {
            eprintln!("Warning: {}", msg);
        }
    }
}

/// How to run a workflow. Replaces mutually exclusive bool flags
/// (`run_async`, `continue_async`, `start`).
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunKind {
    /// Run all steps with text output (default).
    Foreground,
    /// Run first step only, return ID to user (e.g. `--start`).
    SetupOnly,
    /// Run first step, spawn background for remaining steps (e.g. `--async`).
    Async,
    /// Continue a previously started async workflow silently (e.g. `--_continue-async`).
    ContinueAsync,
}

/// Trait for CLI arg structs that carry the common async/continue/start flags.
pub trait HasRunKind {
    fn continue_async(&self) -> Option<&str>;
    fn run_async(&self) -> bool;
    fn start(&self) -> bool {
        false
    }
}

impl RunKind {
    pub fn from_args(args: &impl HasRunKind) -> Self {
        if args.continue_async().is_some() {
            RunKind::ContinueAsync
        } else if args.start() {
            RunKind::SetupOnly
        } else if args.run_async() {
            RunKind::Async
        } else {
            RunKind::Foreground
        }
    }
}

/// RAII guard for the workflow's event FIFO.
///
/// On creation, `mkfifo` creates a named pipe, a keep-alive fd keeps the pipe
/// open (preventing reader EOF), and a read thread forwards each line (change
/// ID) to a crossbeam channel. On drop, the keep-alive fd is closed (triggering
/// reader EOF), the read thread is joined, the FIFO file is removed, and the
/// global `EVENT_PIPE_PATH` is cleared.
struct FifoGuard {
    path: PathBuf,
    keep_alive: Option<OwnedFd>,
    read_handle: Option<JoinHandle<()>>,
}

impl FifoGuard {
    /// Create the event FIFO and spawn the read thread.
    ///
    /// Returns `(guard, receiver)` where receiver yields change-ID strings.
    fn new(workflow_id: &str) -> std::io::Result<(Self, Receiver<String>)> {
        let dir = PathBuf::from("/tmp/aiki/events");
        fs::create_dir_all(&dir)?;

        let path = dir.join(format!("{workflow_id}.workflow.pipe"));

        // Remove stale FIFO from a previous crashed run.
        let _ = fs::remove_file(&path);

        // Create the named pipe.
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
        let ret = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }

        // Keep-alive fd: O_RDWR never blocks and prevents reader EOF.
        let ka_fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if ka_fd < 0 {
            let err = std::io::Error::last_os_error();
            let _ = fs::remove_file(&path);
            return Err(err);
        }
        let keep_alive = unsafe { OwnedFd::from_raw_fd(ka_fd) };

        // Read fd: O_RDONLY succeeds immediately because keep-alive fd exists.
        let rd_fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if rd_fd < 0 {
            let err = std::io::Error::last_os_error();
            let _ = fs::remove_file(&path);
            return Err(err);
        }
        let rd_file = unsafe { std::fs::File::from_raw_fd(rd_fd) };

        // Register the FIFO path so in-process `write_event` calls find it.
        set_event_pipe_path(path.clone());

        // Spawn read thread: blocking BufReader::lines → channel sender.
        let (tx, rx) = crossbeam_channel::unbounded::<String>();
        let read_handle = thread::spawn(move || {
            let reader = BufReader::new(rd_file);
            for line in reader.lines() {
                match line {
                    Ok(s) => {
                        let trimmed = s.trim().to_string();
                        if !trimmed.is_empty() {
                            if tx.send(trimmed).is_err() {
                                break; // receiver dropped
                            }
                        }
                    }
                    Err(_) => break, // EOF or error
                }
            }
        });

        Ok((
            Self {
                path,
                keep_alive: Some(keep_alive),
                read_handle: Some(read_handle),
            },
            rx,
        ))
    }
}

impl Drop for FifoGuard {
    fn drop(&mut self) {
        // Clear the global pipe path first so no new writes target this FIFO.
        clear_event_pipe_path();

        // Drop the keep-alive fd → triggers EOF on the read thread.
        self.keep_alive.take();

        // Join the read thread.
        if let Some(handle) = self.read_handle.take() {
            let _ = handle.join();
        }

        // Remove the FIFO file.
        let _ = fs::remove_file(&self.path);
    }
}

/// A sequence of steps executed against a shared context.
pub struct Workflow {
    pub steps: Vec<Step>,
    pub ctx: WorkflowContext,
}

impl Workflow {
    fn apply_change(&mut self, change: WorkflowChange) {
        match change {
            WorkflowChange::None => {}
            WorkflowChange::SkipSteps(skip) => {
                self.steps.retain(|s| !skip.contains(s));
            }
            WorkflowChange::NextSteps(next) => {
                self.steps.extend(next);
            }
        }
    }

    /// Run only the first step, return the context.
    ///
    /// Used by `SetupOnly` and `Async` run kinds to execute the setup step
    /// before handing off or spawning background work.
    pub fn run_first_step(mut self) -> Result<WorkflowContext> {
        if let Some(step) = self.steps.first() {
            step.run(&mut self.ctx)?;
        }
        Ok(self.ctx)
    }

    pub fn run(mut self) -> Result<WorkflowContext> {
        let verbose = matches!(self.ctx.output.kind(), OutputKind::Text);

        // Create a FIFO for the entire workflow. The resulting Receiver is
        // shared sequentially across steps — each step's DrainHandler consumes
        // only notifications produced during its own agent run.
        // FifoGuard's Drop cleans up the FIFO, joins the read thread, and
        // clears the global EVENT_PIPE_PATH.
        let _fifo_guard = if verbose {
            match FifoGuard::new(&format!("{:x}", std::process::id())) {
                Ok((guard, rx)) => {
                    self.ctx.notify_rx = Some(rx);
                    Some(guard)
                }
                Err(_) => None,
            }
        } else {
            None
        };

        let result = self.run_step_loop(verbose);

        // _fifo_guard drops here, cleaning up the FIFO.

        match result {
            Ok(()) => Ok(self.ctx),
            Err(e) => Err(e),
        }
    }

    fn run_step_loop(&mut self, verbose: bool) -> Result<()> {
        while !self.steps.is_empty() {
            let step = self.steps.remove(0);

            if verbose {
                if let Some(section) = step.section(self.ctx.iteration) {
                    self.ctx.section(&section);
                }
                self.ctx.status(step.name());
            }

            match step.run(&mut self.ctx) {
                Ok(result) => {
                    if verbose {
                        self.ctx.success(step.name(), &result.message);
                    }
                    self.apply_change(result.change);
                }
                Err(e) => {
                    if verbose {
                        self.ctx.error(step.name(), &e.to_string());
                    }
                    return Err(e);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::env;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    fn test_step(
        name: &'static str,
        value: &'static str,
        seen: Arc<Mutex<Vec<&'static str>>>,
    ) -> Step {
        Step::_Test {
            name,
            section: None,
            handler: Arc::new(move |ctx| {
                seen.lock().unwrap().push(name);
                ctx.task_id = Some(value.to_string());
                ctx.plan_path = Some(value.to_string());
                Ok(StepResult {
                    change: WorkflowChange::None,
                    message: format!("set {value}"),
                    task_id: ctx.task_id.clone(),
                })
            }),
        }
    }

    fn test_step_with_section(
        name: &'static str,
        value: &'static str,
        section: Option<&'static str>,
        seen: Arc<Mutex<Vec<&'static str>>>,
    ) -> Step {
        Step::_Test {
            name,
            section,
            handler: Arc::new(move |ctx| {
                seen.lock().unwrap().push(name);
                ctx.task_id = Some(value.to_string());
                ctx.plan_path = Some(value.to_string());
                Ok(StepResult {
                    change: WorkflowChange::None,
                    message: format!("set {value}"),
                    task_id: ctx.task_id.clone(),
                })
            }),
        }
    }

    fn test_step_assert_plan_path(
        name: &'static str,
        expected: &'static str,
        seen: Arc<Mutex<Vec<&'static str>>>,
    ) -> Step {
        Step::_Test {
            name,
            section: None,
            handler: Arc::new(move |ctx| {
                seen.lock().unwrap().push(name);
                assert_eq!(ctx.plan_path.as_deref(), Some(expected));
                ctx.task_id = Some("asserted".to_string());
                Ok(StepResult {
                    change: WorkflowChange::None,
                    message: format!("saw {expected}"),
                    task_id: ctx.task_id.clone(),
                })
            }),
        }
    }

    fn test_step_fail(name: &'static str, message: &'static str) -> Step {
        Step::_Test {
            name,
            section: None,
            handler: Arc::new(move |_ctx| Err(anyhow!(message))),
        }
    }

    fn run_output_probe(case: &str) -> String {
        let exe = env::current_exe().expect("resolve current test binary");
        let output = Command::new(exe)
            .arg("--exact")
            .arg("workflow::tests::run_mode_output_probe")
            .arg("--nocapture")
            .env("AIKI_WORKFLOW_TEST_CASE", case)
            .output()
            .expect("run workflow output probe");

        assert!(
            output.status.success(),
            "probe failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8_lossy(&output.stderr).to_string()
    }

    #[test]
    fn run_mode_output_probe() {
        let Some(case) = env::var("AIKI_WORKFLOW_TEST_CASE").ok() else {
            return;
        };

        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut workflow = match case.as_str() {
            "text-emit-warn" | "quiet-emit-warn" => {
                let output = if case.starts_with("quiet") {
                    OutputKind::Quiet
                } else {
                    OutputKind::Text
                };
                let ctx = WorkflowContext {
                    task_id: None,
                    plan_path: None,
                    cwd: PathBuf::from("."),
                    output: WorkflowOutput::new(output),
                    opts: WorkflowOpts::default(),
                    review_id: None,
                    scope: None,
                    assignee: None,
                    iteration: 0,
                    notify_rx: None,
                    task_names: std::collections::HashMap::new(),
                };
                ctx.emit("workflow-emit-line");
                ctx.warn("workflow-warn-line");
                return;
            }
            "text-sectioned" => Workflow {
                steps: vec![
                    test_step_with_section(
                        "workflow-text-with-section",
                        "first",
                        Some("Workflow Test Section"),
                        Arc::clone(&seen),
                    ),
                    test_step("workflow-text-without-section", "second", seen),
                ],
                ctx: WorkflowContext {
                    task_id: None,
                    plan_path: None,
                    cwd: PathBuf::from("."),
                    output: WorkflowOutput::new(OutputKind::Quiet),
                    opts: WorkflowOpts::default(),
                    review_id: None,
                    scope: None,
                    assignee: None,
                    iteration: 0,
                    notify_rx: None,
                    task_names: std::collections::HashMap::new(),
                },
            },
            "text-basic" => Workflow {
                steps: vec![test_step("workflow-text-step", "text", seen)],
                ctx: WorkflowContext {
                    task_id: None,
                    plan_path: None,
                    cwd: PathBuf::from("."),
                    output: WorkflowOutput::new(OutputKind::Quiet),
                    opts: WorkflowOpts::default(),
                    review_id: None,
                    scope: None,
                    assignee: None,
                    iteration: 0,
                    notify_rx: None,
                    task_names: std::collections::HashMap::new(),
                },
            },
            "quiet-basic" => Workflow {
                steps: vec![test_step("workflow-quiet-step", "quiet", seen)],
                ctx: WorkflowContext {
                    task_id: None,
                    plan_path: None,
                    cwd: PathBuf::from("."),
                    output: WorkflowOutput::new(OutputKind::Quiet),
                    opts: WorkflowOpts::default(),
                    review_id: None,
                    scope: None,
                    assignee: None,
                    iteration: 0,
                    notify_rx: None,
                    task_names: std::collections::HashMap::new(),
                },
            },
            "text-shared" | "quiet-shared" => Workflow {
                steps: vec![test_step("workflow-shared-step", "shared", seen)],
                ctx: WorkflowContext {
                    task_id: None,
                    plan_path: None,
                    cwd: PathBuf::from("."),
                    output: WorkflowOutput::new(OutputKind::Quiet),
                    opts: WorkflowOpts::default(),
                    review_id: None,
                    scope: None,
                    assignee: None,
                    iteration: 0,
                    notify_rx: None,
                    task_names: std::collections::HashMap::new(),
                },
            },
            other => panic!("unknown probe case: {other}"),
        };

        // Set output mode on context based on case prefix
        workflow.ctx.output = WorkflowOutput::new(if case.starts_with("quiet") {
            OutputKind::Quiet
        } else {
            OutputKind::Text
        });

        workflow.run().unwrap();
    }

    #[test]
    fn test_workflow_run_executes_steps_in_order() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow {
            steps: vec![
                test_step("first", "first", Arc::clone(&seen)),
                test_step("second", "second", Arc::clone(&seen)),
            ],
            ctx: WorkflowContext {
                task_id: None,
                plan_path: None,
                cwd: PathBuf::from("."),
                output: WorkflowOutput::new(OutputKind::Quiet),
                opts: WorkflowOpts::default(),
                review_id: None,
                scope: None,
                assignee: None,
                iteration: 0,
                notify_rx: None,
                task_names: std::collections::HashMap::new(),
            },
        };

        let ctx = workflow.run().unwrap();
        assert_eq!(ctx.task_id.as_deref(), Some("second"));
        assert_eq!(ctx.plan_path.as_deref(), Some("second"));
        assert_eq!(*seen.lock().unwrap(), vec!["first", "second"]);
    }

    #[test]
    fn test_workflow_run_stops_on_failure_and_returns_error() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow {
            steps: vec![
                test_step("first", "first", Arc::clone(&seen)),
                test_step_fail("broken", "boom"),
                test_step("third", "third", Arc::clone(&seen)),
            ],
            ctx: WorkflowContext {
                task_id: None,
                plan_path: None,
                cwd: PathBuf::from("."),
                output: WorkflowOutput::new(OutputKind::Quiet),
                opts: WorkflowOpts::default(),
                review_id: None,
                scope: None,
                assignee: None,
                iteration: 0,
                notify_rx: None,
                task_names: std::collections::HashMap::new(),
            },
        };

        let err = match workflow.run() {
            Ok(_) => panic!("workflow should fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("boom"));
        assert_eq!(*seen.lock().unwrap(), vec!["first"]);
    }

    #[test]
    fn test_workflow_context_mutations_are_visible_to_next_step() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow {
            steps: vec![
                test_step("set", "shared-state", Arc::clone(&seen)),
                test_step_assert_plan_path("assert", "shared-state", Arc::clone(&seen)),
            ],
            ctx: WorkflowContext {
                task_id: None,
                plan_path: None,
                cwd: PathBuf::from("."),
                output: WorkflowOutput::new(OutputKind::Quiet),
                opts: WorkflowOpts::default(),
                review_id: None,
                scope: None,
                assignee: None,
                iteration: 0,
                notify_rx: None,
                task_names: std::collections::HashMap::new(),
            },
        };

        let ctx = workflow.run().unwrap();
        assert_eq!(ctx.plan_path.as_deref(), Some("shared-state"));
        assert_eq!(*seen.lock().unwrap(), vec!["set", "assert"]);
    }

    #[test]
    fn test_run_mode_text_emits_sections_only_for_steps_with_headers() {
        let output = run_output_probe("text-sectioned");
        assert!(output.contains("── Workflow Test Section ──"));
        assert_eq!(output.matches("── Workflow Test Section ──").count(), 1);
        assert!(output.contains("⠙ workflow-text-with-section..."));
        assert!(output.contains("⠙ workflow-text-without-section..."));
    }

    #[test]
    fn test_run_mode_text_produces_status_output_and_quiet_does_not() {
        let text_output = run_output_probe("text-basic");
        let quiet_output = run_output_probe("quiet-basic");

        assert!(text_output.contains("⠙ workflow-text-step..."));
        assert!(text_output.contains("合 workflow-text-step"));
        assert!(!quiet_output.contains("workflow-quiet-step"));
    }

    #[test]
    fn test_same_workflow_can_run_in_text_or_quiet_mode() {
        let text_ctx = Workflow {
            steps: vec![test_step(
                "step",
                "shared",
                Arc::new(Mutex::new(Vec::new())),
            )],
            ctx: WorkflowContext {
                task_id: None,
                plan_path: None,
                cwd: PathBuf::from("."),
                output: WorkflowOutput::new(OutputKind::Text),
                opts: WorkflowOpts::default(),
                review_id: None,
                scope: None,
                assignee: None,
                iteration: 0,
                notify_rx: None,
                task_names: std::collections::HashMap::new(),
            },
        }
        .run()
        .unwrap();
        let quiet_ctx = Workflow {
            steps: vec![test_step(
                "step",
                "shared",
                Arc::new(Mutex::new(Vec::new())),
            )],
            ctx: WorkflowContext {
                task_id: None,
                plan_path: None,
                cwd: PathBuf::from("."),
                output: WorkflowOutput::new(OutputKind::Quiet),
                opts: WorkflowOpts::default(),
                review_id: None,
                scope: None,
                assignee: None,
                iteration: 0,
                notify_rx: None,
                task_names: std::collections::HashMap::new(),
            },
        }
        .run()
        .unwrap();
        let text_output = run_output_probe("text-shared");
        let quiet_output = run_output_probe("quiet-shared");

        assert_eq!(text_ctx.task_id, quiet_ctx.task_id);
        assert_eq!(text_ctx.plan_path, quiet_ctx.plan_path);
        assert!(text_output.contains("workflow-shared-step"));
        assert!(!quiet_output.contains("workflow-shared-step"));
    }

    #[test]
    fn test_emit_and_warn_follow_output_mode() {
        let text_output = run_output_probe("text-emit-warn");
        let quiet_output = run_output_probe("quiet-emit-warn");

        assert!(text_output.contains("workflow-emit-line"));
        assert!(text_output.contains("Warning: workflow-warn-line"));
        assert!(!quiet_output.contains("workflow-emit-line"));
        assert!(!quiet_output.contains("workflow-warn-line"));
    }

    fn test_step_with_change(
        name: &'static str,
        change: WorkflowChange,
        seen: Arc<Mutex<Vec<&'static str>>>,
    ) -> Step {
        let change = Arc::new(Mutex::new(Some(change)));
        Step::_Test {
            name,
            section: None,
            handler: Arc::new(move |_ctx| {
                seen.lock().unwrap().push(name);
                let change = change
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap_or(WorkflowChange::None);
                Ok(StepResult {
                    change,
                    message: format!("ran {name}"),
                    task_id: None,
                })
            }),
        }
    }

    #[test]
    fn test_next_steps_appends_and_executes() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let extra = test_step("extra", "extra", Arc::clone(&seen));
        let workflow = Workflow {
            steps: vec![
                test_step_with_change(
                    "first",
                    WorkflowChange::NextSteps(vec![extra]),
                    Arc::clone(&seen),
                ),
                test_step("second", "second", Arc::clone(&seen)),
            ],
            ctx: WorkflowContext {
                task_id: None,
                plan_path: None,
                cwd: PathBuf::from("."),
                output: WorkflowOutput::new(OutputKind::Quiet),
                opts: WorkflowOpts::default(),
                review_id: None,
                scope: None,
                assignee: None,
                iteration: 0,
                notify_rx: None,
                task_names: std::collections::HashMap::new(),
            },
        };

        workflow.run().unwrap();
        assert_eq!(*seen.lock().unwrap(), vec!["first", "second", "extra"]);
    }

    #[test]
    fn test_skip_steps_removes_matching() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let workflow = Workflow {
            steps: vec![
                test_step_with_change(
                    "first",
                    WorkflowChange::SkipSteps(vec![Step::_Test {
                        name: "second",
                        section: None,
                        handler: Arc::new(|_| unreachable!()),
                    }]),
                    Arc::clone(&seen),
                ),
                test_step("second", "second", Arc::clone(&seen)),
                test_step("third", "third", Arc::clone(&seen)),
            ],
            ctx: WorkflowContext {
                task_id: None,
                plan_path: None,
                cwd: PathBuf::from("."),
                output: WorkflowOutput::new(OutputKind::Quiet),
                opts: WorkflowOpts::default(),
                review_id: None,
                scope: None,
                assignee: None,
                iteration: 0,
                notify_rx: None,
                task_names: std::collections::HashMap::new(),
            },
        };

        workflow.run().unwrap();
        assert_eq!(*seen.lock().unwrap(), vec!["first", "third"]);
    }

    #[test]
    fn test_fix_short_circuit_preserves_review_tail_in_current_pass() {
        let mut workflow = Workflow {
            steps: vec![
                Step::Decompose,
                Step::Loop,
                Step::SetupReview,
                Step::Review,
                Step::RegressionReview,
                Step::_Test {
                    name: "after",
                    section: None,
                    handler: Arc::new(|_| unreachable!()),
                },
            ],
            ctx: WorkflowContext {
                task_id: None,
                plan_path: None,
                cwd: PathBuf::from("."),
                output: WorkflowOutput::new(OutputKind::Quiet),
                opts: WorkflowOpts::default(),
                review_id: None,
                scope: None,
                assignee: None,
                iteration: 0,
                notify_rx: None,
                task_names: std::collections::HashMap::new(),
            },
        };

        workflow.apply_change(WorkflowChange::SkipSteps(vec![Step::Decompose, Step::Loop]));

        assert_eq!(workflow.steps.len(), 4);
        assert!(matches!(workflow.steps.first(), Some(Step::SetupReview)));
        assert!(matches!(workflow.steps.get(1), Some(Step::Review)));
        assert!(matches!(
            workflow.steps.get(2),
            Some(Step::RegressionReview)
        ));
        assert!(matches!(
            workflow.steps.get(3),
            Some(Step::_Test { name: "after", .. })
        ));
    }

}
