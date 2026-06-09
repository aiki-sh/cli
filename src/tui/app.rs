use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::select;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::tasks::types::TaskStatus;
use crate::tasks::{materialize_graph, TaskGraph};
use crate::tui::render::{apply_dimming, lines_height, render_lines};
use crate::tui::theme;

/// Which screen to render. Defines lifecycle/exit behavior.
/// View rendering is graph-driven — view functions adapt to whatever
/// tasks exist in the graph. The Screen enum determines WHEN to exit,
/// not WHAT to render (e.g. build::view() renders fix/iteration
/// sections when fix subtasks are present in the graph — no separate
/// BuildFix variant needed).
pub enum Screen {
    TaskRun {
        task_id: String,
    },
    Build {
        epic_id: String,
        plan_path: String,
    },
    Review {
        review_id: String,
        target: String,
    },
    Fix {
        fix_parent_id: String,
        review_id: String,
    },
    EpicShow {
        epic_id: String,
    },
    ReviewShow {
        review_id: String,
    },
}

/// Window state — local view concerns not derived from TaskGraph.
pub struct WindowState {
    pub width: u16,
    pub scroll_offset: u16,
    /// Monotonically increasing tick counter, used for spinner animation.
    /// Incremented on every update cycle.
    pub tick: u64,
}

impl WindowState {
    pub fn new(width: u16) -> Self {
        Self {
            width,
            scroll_offset: 0,
            tick: 0,
        }
    }
}

// ── Worker thread types ──────────────────────────────────────────────

/// Messages sent from the worker thread to the TUI event loop.
pub enum WorkerStatusMsg {
    /// New phase row. Appended to model's entry list.
    PhaseStarted { name: &'static str },
    /// Update current (last) phase's status line.
    Update(String),
    /// Set current phase's agent display (e.g., "claude").
    AgentResolved(String),
    /// Bind current phase to a task ID.
    TaskBound(String),
    /// Bind current phase to a parent task it orchestrates.
    Orchestrates(String),
    /// Mark current phase completed.
    PhaseDone { result: String },
    /// Mark current phase failed.
    PhaseFailed { error: String },
    /// Insert a section header.
    Section(String),
    /// Signal pipeline completion.
    Done,
}

/// Thin wrapper around the channel sender. Eliminates
/// `tx.send(WorkerStatusMsg::...)` boilerplate from worker closures.
pub struct WorkerStatus {
    tx: crossbeam_channel::Sender<WorkerStatusMsg>,
}

impl WorkerStatus {
    pub fn new(tx: crossbeam_channel::Sender<WorkerStatusMsg>) -> Self {
        Self { tx }
    }

    /// Start a new phase. Appends a phase row to the display.
    pub fn start(&self, name: &'static str) {
        self.send(WorkerStatusMsg::PhaseStarted { name });
    }

    /// Update current phase's status line.
    pub fn update(&self, text: &str) {
        self.send(WorkerStatusMsg::Update(text.into()));
    }

    /// Set current phase's agent display.
    pub fn agent(&self, name: &str) {
        self.send(WorkerStatusMsg::AgentResolved(name.into()));
    }

    /// Bind current phase to a task ID.
    pub fn task(&self, id: &str) {
        self.send(WorkerStatusMsg::TaskBound(id.into()));
    }

    /// Bind current phase to a parent task it orchestrates.
    pub fn orchestrates(&self, id: &str) {
        self.send(WorkerStatusMsg::Orchestrates(id.into()));
    }

    /// Mark current phase completed.
    pub fn done(&self, result: &str) {
        self.send(WorkerStatusMsg::PhaseDone {
            result: result.into(),
        });
    }

    /// Mark current phase failed.
    pub fn failed(&self, error: &str) {
        self.send(WorkerStatusMsg::PhaseFailed {
            error: error.into(),
        });
    }

    /// Insert a section header.
    pub fn section(&self, title: &str) {
        self.send(WorkerStatusMsg::Section(title.into()));
    }

    /// Signal pipeline completion.
    pub fn finish(&self) {
        self.send(WorkerStatusMsg::Done);
    }

    fn send(&self, msg: WorkerStatusMsg) {
        let _ = self.tx.send(msg);
    }
}

/// An entry in the worker status display — either a phase or section header.
pub enum Entry {
    Phase(PhaseState),
    Section(String),
}

/// State of a single worker phase.
pub struct PhaseState {
    pub name: &'static str,
    pub agent: Option<String>,
    pub task_id: Option<String>,
    pub orchestrates_id: Option<String>,
    pub worker_status: Option<String>,
    pub state: PhaseLifecycle,
    pub started_at: Instant,
}

/// Lifecycle state of a phase.
pub enum PhaseLifecycle {
    Active,
    Done { result: String },
    Failed { error: String },
}

// ── Model ────────────────────────────────────────────────────────────

/// Everything the view needs to render.
pub struct Model {
    /// Domain state — materialized from JJ task events.
    /// Wrapped in Arc so the JJ-contention fallback path ("use cached")
    /// is a cheap refcount bump instead of cloning the full graph.
    pub graph: Arc<TaskGraph>,
    /// Which screen we're rendering.
    pub screen: Screen,
    /// Window state (scroll, terminal size, etc.).
    pub window: WindowState,
    /// Ordered list of phases and sections, built up by worker messages.
    pub entries: Vec<Entry>,
    /// True after worker sends Done (or WorkerDisconnected handled).
    pub finished: bool,
    /// True after Ctrl+C.
    pub detached: bool,
}

/// Events that can change the model.
pub enum Msg {
    /// New TaskGraph read from JJ (or cached if JJ was slow).
    GraphUpdated(TaskGraph),
    /// Worker thread status update.
    Worker(WorkerStatusMsg),
    /// Worker channel disconnected without sending Done.
    WorkerDisconnected,
    /// User pressed Ctrl+C.
    Detach,
    /// Terminal resized.
    Resize { width: u16, height: u16 },
    /// Render tick — no new data, but re-render for elapsed time updates + spinner.
    Tick,
}

/// Result of update — tells the event loop what to do.
pub enum Effect {
    /// Continue the loop.
    Continue,
    /// Stop the loop — build completed or task done.
    Done,
    /// Stop the loop — user detached.
    Detached,
}

/// Pure state transition. Returns updated model + effect.
pub fn update(mut model: Model, msg: Msg) -> (Model, Effect) {
    model.window.tick += 1;
    match msg {
        Msg::GraphUpdated(graph) => {
            let done = is_finished(&graph, &model.screen);
            model.graph = Arc::new(graph);
            let effect = if done { Effect::Done } else { Effect::Continue };
            (model, effect)
        }
        Msg::Worker(worker_msg) => {
            match worker_msg {
                WorkerStatusMsg::PhaseStarted { name } => {
                    model.entries.push(Entry::Phase(PhaseState {
                        name,
                        agent: None,
                        task_id: None,
                        orchestrates_id: None,
                        worker_status: None,
                        state: PhaseLifecycle::Active,
                        started_at: Instant::now(),
                    }));
                }
                WorkerStatusMsg::Update(text) => {
                    if let Some(Entry::Phase(phase)) = model.entries.last_mut() {
                        phase.worker_status = Some(text);
                    }
                }
                WorkerStatusMsg::AgentResolved(name) => {
                    if let Some(Entry::Phase(phase)) = model.entries.last_mut() {
                        phase.agent = Some(name);
                    }
                }
                WorkerStatusMsg::TaskBound(id) => {
                    if let Some(Entry::Phase(phase)) = model.entries.last_mut() {
                        phase.task_id = Some(id);
                    }
                }
                WorkerStatusMsg::Orchestrates(id) => {
                    if let Some(Entry::Phase(phase)) = model.entries.last_mut() {
                        phase.orchestrates_id = Some(id);
                    }
                }
                WorkerStatusMsg::PhaseDone { result } => {
                    if let Some(Entry::Phase(phase)) = model.entries.last_mut() {
                        phase.state = PhaseLifecycle::Done { result };
                    }
                }
                WorkerStatusMsg::PhaseFailed { error } => {
                    if let Some(Entry::Phase(phase)) = model.entries.last_mut() {
                        phase.state = PhaseLifecycle::Failed { error };
                    }
                }
                WorkerStatusMsg::Section(title) => {
                    model.entries.push(Entry::Section(title));
                }
                WorkerStatusMsg::Done => {
                    model.finished = true;
                }
            }
            (model, Effect::Continue)
        }
        Msg::WorkerDisconnected => {
            // Mark last active phase as failed
            if let Some(Entry::Phase(phase)) = model.entries.last_mut() {
                if matches!(phase.state, PhaseLifecycle::Active) {
                    phase.state = PhaseLifecycle::Failed {
                        error: "worker exited unexpectedly".to_string(),
                    };
                }
            }
            model.finished = true;
            (model, Effect::Continue)
        }
        Msg::Detach => (model, Effect::Detached),
        Msg::Resize { width, .. } => {
            model.window.width = width;
            (model, Effect::Continue)
        }
        Msg::Tick => (model, Effect::Continue),
    }
}

/// Check if the monitored task/epic has finished (done or failed).
fn is_finished(graph: &TaskGraph, screen: &Screen) -> bool {
    match screen {
        Screen::TaskRun { task_id } => graph
            .tasks
            .get(task_id)
            .map(|t| matches!(t.status, TaskStatus::Closed | TaskStatus::Stopped))
            .unwrap_or(false),
        Screen::Build { epic_id, .. } => graph
            .tasks
            .get(epic_id)
            .map(|t| matches!(t.status, TaskStatus::Closed | TaskStatus::Stopped))
            .unwrap_or(false),
        Screen::Review { review_id, .. } => graph
            .tasks
            .get(review_id)
            .map(|t| matches!(t.status, TaskStatus::Closed | TaskStatus::Stopped))
            .unwrap_or(false),
        Screen::Fix { fix_parent_id, .. } => graph
            .tasks
            .get(fix_parent_id)
            .map(|t| matches!(t.status, TaskStatus::Closed | TaskStatus::Stopped))
            .unwrap_or(false),
        // Static views — render once, always done
        Screen::EpicShow { .. } | Screen::ReviewShow { .. } => true,
    }
}

/// A single rendered line.
pub struct Line {
    /// Indent level (0 = flush left, 1 = 4 spaces, etc.)
    pub indent: u8,
    /// The text content (indent applied at render time)
    pub text: String,
    /// Right-aligned metadata (elapsed time, etc.)
    pub meta: Option<String>,
    /// Visual style
    pub style: LineStyle,
    /// Phase group — lines with the same group dim together
    pub group: u16,
    /// Whether this line is dimmed (set by apply_dimming, not by builders)
    pub dimmed: bool,
}

#[derive(Clone, Copy)]
pub enum LineStyle {
    PhaseHeader {
        active: bool,
    },
    /// Failed phase header — red+bold `合` icon and text.
    PhaseHeaderFailed,
    /// Subtask table header — `[short-id] title` with dim brackets+id, fg title.
    SubtaskHeader,
    Child,
    ChildActive,
    ChildDone,
    ChildError,
    ChildWarning,
    ChildBold,
    Subtask {
        status: SubtaskStatus,
    },
    Separator,
    SectionHeader,
    Issue,
    Dim,
    Blank,
}

#[derive(Clone, Copy)]
pub enum SubtaskStatus {
    PendingUnassigned, // ◌ — no lane has claimed this yet
    Pending,           // ○ — in active lane, not yet started
    Assigned,          // ⧗ — assigned to session
    Active,            // ▸
    Done,              // ✔
    Failed,            // ✘
}

/// Run the Elm loop until done or detached.
///
/// Creates an inline viewport terminal on stdout, renders via the
/// view → render → poll → update cycle at ~100ms tick rate.
/// Terminal events read by the crossterm reader thread.
enum TermEvent {
    Key(KeyEvent),
    Resize(u16, u16),
}

/// Spawn a thread that reads crossterm events and sends them on a channel.
/// Returns the receiver. The thread exits when the stop flag is set or the
/// receiver is dropped.
fn spawn_crossterm_reader(
    stop: Arc<AtomicBool>,
) -> crossbeam_channel::Receiver<TermEvent> {
    let (tx, rx) = crossbeam_channel::bounded(16);
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            // Poll with a short timeout so we can check the stop flag periodically
            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                let ev = match event::read() {
                    Ok(ev) => ev,
                    Err(_) => break,
                };
                let term_ev = match ev {
                    Event::Key(key) => TermEvent::Key(key),
                    Event::Resize(w, h) => TermEvent::Resize(w, h),
                    _ => continue,
                };
                if tx.send(term_ev).is_err() {
                    break; // receiver dropped
                }
            }
        }
    });
    rx
}

pub fn run(model: Model, cwd: &Path) -> Result<Effect> {
    run_inner(model, cwd)
}

fn run_inner(model: Model, cwd: &Path) -> Result<Effect> {
    crate::tui::panic_hook::install_panic_hook();

    // Shared stop flag — set by signal handlers (SIGTERM, SIGHUP)
    // and checked by the JJ reader thread and poll loop.
    let stop = Arc::new(AtomicBool::new(false));
    let _signal_guards = crate::tui::panic_hook::install_signal_handlers(Arc::clone(&stop));

    let theme = theme::Theme::from_mode(theme::detect_mode());

    // Create inline terminal on stdout — output persists in scrollback
    let backend = CrosstermBackend::new(std::io::stdout());
    let initial_height = 1u16;
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(initial_height),
        },
    )?;
    let mut current_height = initial_height;

    crossterm::terminal::enable_raw_mode()?;

    // Spawn TaskEventListener + bridge thread — sends updated TaskGraphs
    // via channel. The listener polls JJ for raw TaskEvents; the bridge
    // accumulates them and rematerializes the graph for the TUI.
    let (tx, rx) = crossbeam_channel::unbounded::<TaskGraph>();
    let listener =
        crate::tasks::listener::TaskEventListener::new(cwd, Arc::clone(&stop));
    let event_rx = listener.start();
    let jj_stop = Arc::clone(&stop);
    let _jj_bridge = thread::spawn(move || {
        // TODO: all_events grows monotonically — consider periodic compaction
        // or a cap for very long-running TUI sessions (multi-hour build loops).
        let mut all_events: Vec<crate::tasks::types::TaskEvent> = Vec::new();
        loop {
            if jj_stop.load(Ordering::Relaxed) {
                break;
            }
            // Block until at least one new event arrives (or timeout to check stop)
            match event_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(event) => {
                    all_events.push(event);
                    // Drain any additional events that arrived in the same batch
                    while let Ok(ev) = event_rx.try_recv() {
                        all_events.push(ev);
                    }
                    let graph = materialize_graph(&all_events);
                    if tx.send(graph).is_err() {
                        break; // main loop exited, receiver dropped
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    // Spawn crossterm reader thread and tick channel
    let crossterm_rx = spawn_crossterm_reader(Arc::clone(&stop));
    let tick_rx = crossbeam_channel::tick(Duration::from_millis(100));

    let result = run_loop(
        model,
        &rx,
        &crossterm_rx,
        &tick_rx,
        &stop,
        &theme,
        &mut terminal,
        &mut current_height,
    );

    // Cleanup: signal the JJ thread to stop and restore terminal
    stop.store(true, Ordering::Relaxed);
    crossterm::terminal::disable_raw_mode()?;

    match result {
        Ok(Effect::Detached) => {
            println!("[detached]");
            Ok(Effect::Detached)
        }
        other => other,
    }
}

/// Run the Elm loop with a worker closure that drives phases.
///
/// The worker runs on a background thread and sends `WorkerStatusMsg`s
/// to the TUI event loop. The loop exits when the worker sends `Done`
/// (or disconnects unexpectedly).
pub fn run_with_worker<F>(model: Model, cwd: &Path, worker: F) -> Result<Effect>
where
    F: FnOnce(WorkerStatus, PathBuf) -> Result<()> + Send + 'static,
{
    crate::tui::panic_hook::install_panic_hook();

    let stop = Arc::new(AtomicBool::new(false));
    let _signal_guards = crate::tui::panic_hook::install_signal_handlers(Arc::clone(&stop));

    let theme = theme::Theme::from_mode(theme::detect_mode());

    let backend = CrosstermBackend::new(std::io::stdout());
    let initial_height = 1u16;
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(initial_height),
        },
    )?;
    let mut current_height = initial_height;

    crossterm::terminal::enable_raw_mode()?;

    // Spawn TaskEventListener + bridge thread — same pattern as run_inner()
    let (jj_tx, jj_rx) = crossbeam_channel::unbounded::<TaskGraph>();
    let listener =
        crate::tasks::listener::TaskEventListener::new(cwd, Arc::clone(&stop));
    let event_rx = listener.start();
    let jj_stop = Arc::clone(&stop);
    let _jj_bridge = thread::spawn(move || {
        // TODO: all_events grows monotonically — consider periodic compaction
        // or a cap for very long-running TUI sessions (multi-hour build loops).
        let mut all_events: Vec<crate::tasks::types::TaskEvent> = Vec::new();
        loop {
            if jj_stop.load(Ordering::Relaxed) {
                break;
            }
            match event_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(event) => {
                    all_events.push(event);
                    while let Ok(ev) = event_rx.try_recv() {
                        all_events.push(ev);
                    }
                    let graph = materialize_graph(&all_events);
                    if jj_tx.send(graph).is_err() {
                        break;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    // Spawn worker thread
    let (worker_tx, worker_rx) = crossbeam_channel::unbounded::<WorkerStatusMsg>();
    let status = WorkerStatus::new(worker_tx);
    let worker_cwd = cwd.to_owned();
    let worker_handle = thread::spawn(move || worker(status, worker_cwd));

    // Spawn crossterm reader thread and tick channel
    let crossterm_rx = spawn_crossterm_reader(Arc::clone(&stop));
    let tick_rx = crossbeam_channel::tick(Duration::from_millis(100));

    let result = run_loop_with_worker(
        model,
        &jj_rx,
        &worker_rx,
        &crossterm_rx,
        &tick_rx,
        &stop,
        &theme,
        &mut terminal,
        &mut current_height,
    );

    // Cleanup: signal threads to stop, join worker, restore terminal
    stop.store(true, Ordering::Relaxed);
    let _ = worker_handle.join();
    crossterm::terminal::disable_raw_mode()?;

    match result {
        Ok(Effect::Detached) => {
            println!("[detached]");
            Ok(Effect::Detached)
        }
        other => other,
    }
}

/// Inner loop — separated so cleanup always runs via the outer `run()`.
fn run_loop(
    mut model: Model,
    rx: &crossbeam_channel::Receiver<TaskGraph>,
    crossterm_rx: &crossbeam_channel::Receiver<TermEvent>,
    tick_rx: &crossbeam_channel::Receiver<Instant>,
    stop: &AtomicBool,
    theme: &theme::Theme,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    current_height: &mut u16,
) -> Result<Effect> {
    loop {
        // 1. View (pure)
        let mut lines = view(&model);
        apply_dimming(&mut lines);

        // 2. Grow viewport if content exceeds current height
        let new_height = lines_height(&lines).max(1);
        if new_height > *current_height {
            let growth = new_height - *current_height;
            terminal.insert_before(growth, |_| {})?;
            terminal.resize(ratatui::layout::Rect::new(
                0,
                0,
                model.window.width,
                new_height,
            ))?;
            *current_height = new_height;
        }

        // 3. Render via Viewport::Inline
        let tick = model.window.tick;
        terminal.draw(|frame| {
            let area = frame.area();
            render_lines(&lines, frame.buffer_mut(), area, theme, tick);
        })?;

        // 4. Wait for next event via crossbeam::select!
        let msg = if stop.load(Ordering::Relaxed) {
            Msg::Detach
        } else {
            select! {
                recv(rx) -> graph => {
                    match graph {
                        Ok(g) => {
                            // Batch drain: collect all pending graph updates, use latest
                            let mut latest = g;
                            while let Ok(more) = rx.try_recv() {
                                latest = more;
                            }
                            Msg::GraphUpdated(latest)
                        }
                        Err(_) => Msg::Tick, // channel disconnected, treat as tick
                    }
                },
                recv(crossterm_rx) -> ev => {
                    match ev {
                        Ok(TermEvent::Key(KeyEvent {
                            code: KeyCode::Char('c'),
                            modifiers,
                            ..
                        })) if modifiers.contains(KeyModifiers::CONTROL) => Msg::Detach,
                        Ok(TermEvent::Resize(w, h)) => Msg::Resize { width: w, height: h },
                        Ok(TermEvent::Key(_)) => Msg::Tick, // ignore other keys
                        Err(_) => Msg::Detach,
                    }
                },
                recv(tick_rx) -> _ => Msg::Tick,
            }
        };

        // 5. Update
        let (new_model, effect) = update(model, msg);
        model = new_model;

        match effect {
            Effect::Continue => continue,
            Effect::Done | Effect::Detached => {
                // Final render — grow viewport to fit (same logic as main loop)
                let mut lines = view(&model);
                apply_dimming(&mut lines);
                let new_height = lines_height(&lines).max(1);
                if new_height > *current_height {
                    let growth = new_height - *current_height;
                    terminal.insert_before(growth, |_| {})?;
                    terminal.resize(ratatui::layout::Rect::new(
                        0,
                        0,
                        model.window.width,
                        new_height,
                    ))?;
                    *current_height = new_height;
                }
                let tick = model.window.tick;
                terminal.draw(|frame| {
                    let area = frame.area();
                    render_lines(&lines, frame.buffer_mut(), area, theme, tick);
                })?;
                return Ok(effect);
            }
        }
    }
}

/// Inner loop for worker mode — checks `model.finished` for exit.
fn run_loop_with_worker(
    mut model: Model,
    jj_rx: &crossbeam_channel::Receiver<TaskGraph>,
    worker_rx: &crossbeam_channel::Receiver<WorkerStatusMsg>,
    crossterm_rx: &crossbeam_channel::Receiver<TermEvent>,
    tick_rx: &crossbeam_channel::Receiver<Instant>,
    stop: &AtomicBool,
    theme: &theme::Theme,
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    current_height: &mut u16,
) -> Result<Effect> {
    loop {
        // 1. View (pure)
        let mut lines = view(&model);
        apply_dimming(&mut lines);

        // 2. Grow viewport if content exceeds current height
        let new_height = lines_height(&lines).max(1);
        if new_height > *current_height {
            let growth = new_height - *current_height;
            terminal.insert_before(growth, |_| {})?;
            terminal.resize(ratatui::layout::Rect::new(
                0,
                0,
                model.window.width,
                new_height,
            ))?;
            *current_height = new_height;
        }

        // 3. Render
        let tick = model.window.tick;
        terminal.draw(|frame| {
            let area = frame.area();
            render_lines(&lines, frame.buffer_mut(), area, theme, tick);
        })?;

        // 4. Wait for next event via crossbeam::select!
        let msg = if stop.load(Ordering::Relaxed) {
            Msg::Detach
        } else {
            // Check worker channel first (priority — worker messages are structural).
            // try_recv before select! so worker msgs are never starved by other channels.
            match worker_rx.try_recv() {
                Ok(msg) => Msg::Worker(msg),
                Err(crossbeam_channel::TryRecvError::Disconnected) if !model.finished => {
                    Msg::WorkerDisconnected
                }
                _ => {
                    select! {
                        recv(worker_rx) -> msg => {
                            match msg {
                                Ok(m) => Msg::Worker(m),
                                Err(_) if !model.finished => Msg::WorkerDisconnected,
                                Err(_) => Msg::Tick,
                            }
                        },
                        recv(jj_rx) -> graph => {
                            match graph {
                                Ok(g) => {
                                    // Batch drain: use latest graph update
                                    let mut latest = g;
                                    while let Ok(more) = jj_rx.try_recv() {
                                        latest = more;
                                    }
                                    Msg::GraphUpdated(latest)
                                }
                                Err(_) => Msg::Tick,
                            }
                        },
                        recv(crossterm_rx) -> ev => {
                            match ev {
                                Ok(TermEvent::Key(KeyEvent {
                                    code: KeyCode::Char('c'),
                                    modifiers,
                                    ..
                                })) if modifiers.contains(KeyModifiers::CONTROL) => Msg::Detach,
                                Ok(TermEvent::Resize(w, h)) => Msg::Resize { width: w, height: h },
                                Ok(TermEvent::Key(_)) => Msg::Tick,
                                Err(_) => Msg::Detach,
                            }
                        },
                        recv(tick_rx) -> _ => Msg::Tick,
                    }
                }
            }
        };

        // 5. Update
        let (new_model, effect) = update(model, msg);
        model = new_model;

        // In worker mode, finished flag drives exit
        let effect = if model.finished && matches!(effect, Effect::Continue) {
            Effect::Done
        } else {
            effect
        };

        match effect {
            Effect::Continue => continue,
            Effect::Done | Effect::Detached => {
                // Final render
                let mut lines = view(&model);
                apply_dimming(&mut lines);
                let new_height = lines_height(&lines).max(1);
                if new_height > *current_height {
                    let growth = new_height - *current_height;
                    terminal.insert_before(growth, |_| {})?;
                    terminal.resize(ratatui::layout::Rect::new(
                        0,
                        0,
                        model.window.width,
                        new_height,
                    ))?;
                    *current_height = new_height;
                }
                let tick = model.window.tick;
                terminal.draw(|frame| {
                    let area = frame.area();
                    render_lines(&lines, frame.buffer_mut(), area, theme, tick);
                })?;
                return Ok(effect);
            }
        }
    }
}

/// Pure view function — produces lines from model.
///
/// When `model.entries` is non-empty (worker-driven commands), renders from
/// the entry list using shared helpers. Otherwise falls back to per-screen
/// graph-only view functions (for `run()` without worker).
pub fn view(model: &Model) -> Vec<Line> {
    if model.entries.is_empty() {
        return view_from_screen(model);
    }
    render_entries(model)
}

/// Fallback: dispatch to per-screen view functions (graph-only rendering).
fn view_from_screen(model: &Model) -> Vec<Line> {
    match &model.screen {
        Screen::TaskRun { task_id } => {
            super::screens::task_run::view(&model.graph, task_id, &model.window)
        }
        Screen::Build { epic_id, plan_path } => {
            super::screens::build::view(&model.graph, epic_id, plan_path, &model.window)
        }
        Screen::Review { review_id, target } => {
            super::screens::review::view(&model.graph, review_id, target, &model.window)
        }
        Screen::Fix {
            fix_parent_id,
            review_id,
        } => super::screens::fix::view(&model.graph, fix_parent_id, review_id, &model.window),
        Screen::EpicShow { epic_id } => {
            super::screens::epic_show::view(&model.graph, epic_id, &model.window)
        }
        Screen::ReviewShow { review_id } => {
            super::screens::review_show::view(&model.graph, review_id, &model.window)
        }
    }
}

/// Entry-based rendering: iterates `model.entries` and uses shared helpers.
fn render_entries(model: &Model) -> Vec<Line> {
    use super::screens::helpers::{
        render_issue_list, render_lane_blocks, render_phase_line, render_subtask_table,
        render_summary_line,
    };

    let mut lines = Vec::new();

    for entry in &model.entries {
        match entry {
            Entry::Phase(phase) => {
                lines.extend(render_phase_line(phase, &model.graph, &model.window));

                // Graph-derived components keyed by orchestrates_id
                if let Some(orch_id) = &phase.orchestrates_id {
                    // Lane blocks inside loop phases
                    if phase.name == "loop" {
                        lines.extend(render_lane_blocks(&model.graph, orch_id, &model.window));
                    }
                    // Subtask table for any phase with orchestrates_id
                    lines.extend(render_subtask_table(&model.graph, orch_id, &model.window));
                }

                // Issue list for review phases
                if phase.name == "review" {
                    if let Some(task_id) = &phase.task_id {
                        lines.extend(render_issue_list(&model.graph, task_id, &model.window));
                    }
                }
            }
            Entry::Section(title) => {
                lines.extend(crate::tui::components::section_header(0, title));
            }
        }
    }

    if model.finished {
        lines.extend(render_summary_line(model));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::EdgeStore;
    use crate::tasks::types::{FastHashMap, TaskOutcome, TaskPriority, TaskStatus};
    use crate::tasks::Task;
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_task(id: &str, status: TaskStatus) -> Task {
        Task {
            id: id.to_string(),
            name: "test task".to_string(),
            slug: None,
            task_type: None,
            status,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            instructions: None,
            data: HashMap::new(),
            created_at: Utc::now(),
            started_at: None,
            claimed_by_session: None,
            last_session_id: None,
            stopped_reason: None,
            closed_outcome: if status == TaskStatus::Closed {
                Some(TaskOutcome::Done)
            } else {
                None
            },
            confidence: None,
            summary: None,
            turn_started: None,
            closed_at: None,
            turn_closed: None,
            turn_stopped: None,
            comments: Vec::new(),
        }
    }

    fn make_model(screen: Screen) -> Model {
        Model {
            graph: Arc::new(TaskGraph {
                tasks: FastHashMap::default(),
                edges: EdgeStore::default(),
                slug_index: FastHashMap::default(),
            }),
            screen,
            window: WindowState::new(80),
            entries: Vec::new(),
            finished: false,
            detached: false,
        }
    }

    fn make_model_with_entries(entries: Vec<Entry>) -> Model {
        Model {
            graph: Arc::new(TaskGraph {
                tasks: FastHashMap::default(),
                edges: EdgeStore::default(),
                slug_index: FastHashMap::default(),
            }),
            screen: Screen::TaskRun {
                task_id: "test".into(),
            },
            window: WindowState::new(80),
            entries,
            finished: false,
            detached: false,
        }
    }

    fn make_graph_with_task(id: &str, status: TaskStatus) -> TaskGraph {
        let mut tasks = FastHashMap::default();
        tasks.insert(id.to_string(), make_task(id, status));
        TaskGraph {
            tasks,
            edges: EdgeStore::default(),
            slug_index: FastHashMap::default(),
        }
    }

    #[test]
    fn graph_updated_with_terminal_task_returns_done() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let graph = make_graph_with_task("abc", TaskStatus::Closed);
        let (_, effect) = update(model, Msg::GraphUpdated(graph));
        assert!(matches!(effect, Effect::Done));
    }

    #[test]
    fn graph_updated_with_in_progress_task_returns_continue() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let graph = make_graph_with_task("abc", TaskStatus::InProgress);
        let (_, effect) = update(model, Msg::GraphUpdated(graph));
        assert!(matches!(effect, Effect::Continue));
    }

    #[test]
    fn detach_returns_detached() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (_, effect) = update(model, Msg::Detach);
        assert!(matches!(effect, Effect::Detached));
    }

    #[test]
    fn resize_updates_window_width() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, effect) = update(
            model,
            Msg::Resize {
                width: 120,
                height: 40,
            },
        );
        assert_eq!(model.window.width, 120);
        assert!(matches!(effect, Effect::Continue));
    }

    #[test]
    fn tick_returns_continue() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (_, effect) = update(model, Msg::Tick);
        assert!(matches!(effect, Effect::Continue));
    }

    #[test]
    fn build_done_when_epic_closed() {
        let model = make_model(Screen::Build {
            epic_id: "epic1".into(),
            plan_path: "plan.md".into(),
        });
        let graph = make_graph_with_task("epic1", TaskStatus::Closed);
        let (_, effect) = update(model, Msg::GraphUpdated(graph));
        assert!(matches!(effect, Effect::Done));
    }

    #[test]
    fn build_continues_when_epic_in_progress() {
        let model = make_model(Screen::Build {
            epic_id: "epic1".into(),
            plan_path: "plan.md".into(),
        });
        let graph = make_graph_with_task("epic1", TaskStatus::InProgress);
        let (_, effect) = update(model, Msg::GraphUpdated(graph));
        assert!(matches!(effect, Effect::Continue));
    }

    #[test]
    fn static_screen_always_done() {
        let model = make_model(Screen::EpicShow {
            epic_id: "epic1".into(),
        });
        let graph = make_graph_with_task("epic1", TaskStatus::InProgress);
        let (_, effect) = update(model, Msg::GraphUpdated(graph));
        assert!(matches!(effect, Effect::Done));
    }

    #[test]
    fn review_show_always_done() {
        let model = make_model(Screen::ReviewShow {
            review_id: "rev1".into(),
        });
        let graph = make_graph_with_task("rev1", TaskStatus::InProgress);
        let (_, effect) = update(model, Msg::GraphUpdated(graph));
        assert!(matches!(effect, Effect::Done));
    }

    #[test]
    fn stopped_task_is_terminal() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let graph = make_graph_with_task("abc", TaskStatus::Stopped);
        let (_, effect) = update(model, Msg::GraphUpdated(graph));
        assert!(matches!(effect, Effect::Done));
    }

    #[test]
    fn fix_done_when_parent_closed() {
        let model = make_model(Screen::Fix {
            fix_parent_id: "fix1".into(),
            review_id: "rev1".into(),
        });
        let graph = make_graph_with_task("fix1", TaskStatus::Closed);
        let (_, effect) = update(model, Msg::GraphUpdated(graph));
        assert!(matches!(effect, Effect::Done));
    }

    #[test]
    fn review_done_when_review_closed() {
        let model = make_model(Screen::Review {
            review_id: "rev1".into(),
            target: "task1".into(),
        });
        let graph = make_graph_with_task("rev1", TaskStatus::Closed);
        let (_, effect) = update(model, Msg::GraphUpdated(graph));
        assert!(matches!(effect, Effect::Done));
    }

    // ── Worker message tests ────────────────────────────────────────

    #[test]
    fn phase_started_appends_entry() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, effect) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "build" }),
        );
        assert!(matches!(effect, Effect::Continue));
        assert_eq!(model.entries.len(), 1);
        match &model.entries[0] {
            Entry::Phase(p) => {
                assert_eq!(p.name, "build");
                assert!(matches!(p.state, PhaseLifecycle::Active));
                assert!(p.agent.is_none());
                assert!(p.task_id.is_none());
                assert!(p.orchestrates_id.is_none());
            }
            _ => panic!("expected Phase entry"),
        }
    }

    #[test]
    fn worker_update_sets_status_on_last_phase() {
        let mut model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        // First create a phase
        let (m, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "deploy" }),
        );
        model = m;
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::Update("running...".into())),
        );
        match &model.entries[0] {
            Entry::Phase(p) => assert_eq!(p.worker_status.as_deref(), Some("running...")),
            _ => panic!("expected Phase entry"),
        }
    }

    #[test]
    fn agent_resolved_sets_agent() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "test" }),
        );
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::AgentResolved("claude".into())),
        );
        match &model.entries[0] {
            Entry::Phase(p) => assert_eq!(p.agent.as_deref(), Some("claude")),
            _ => panic!("expected Phase entry"),
        }
    }

    #[test]
    fn task_bound_sets_task_id() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "test" }),
        );
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::TaskBound("task123".into())),
        );
        match &model.entries[0] {
            Entry::Phase(p) => assert_eq!(p.task_id.as_deref(), Some("task123")),
            _ => panic!("expected Phase entry"),
        }
    }

    #[test]
    fn orchestrates_sets_orchestrates_id() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "test" }),
        );
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::Orchestrates("parent1".into())),
        );
        match &model.entries[0] {
            Entry::Phase(p) => assert_eq!(p.orchestrates_id.as_deref(), Some("parent1")),
            _ => panic!("expected Phase entry"),
        }
    }

    #[test]
    fn phase_done_marks_done() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "test" }),
        );
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseDone {
                result: "ok".into(),
            }),
        );
        match &model.entries[0] {
            Entry::Phase(p) => {
                assert!(matches!(&p.state, PhaseLifecycle::Done { result } if result == "ok"))
            }
            _ => panic!("expected Phase entry"),
        }
    }

    #[test]
    fn phase_failed_marks_failed() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "test" }),
        );
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseFailed {
                error: "boom".into(),
            }),
        );
        match &model.entries[0] {
            Entry::Phase(p) => {
                assert!(matches!(&p.state, PhaseLifecycle::Failed { error } if error == "boom"))
            }
            _ => panic!("expected Phase entry"),
        }
    }

    #[test]
    fn section_appends_section_entry() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::Section("Header".into())),
        );
        assert_eq!(model.entries.len(), 1);
        assert!(matches!(&model.entries[0], Entry::Section(s) if s == "Header"));
    }

    #[test]
    fn worker_done_sets_finished() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, effect) = update(model, Msg::Worker(WorkerStatusMsg::Done));
        assert!(model.finished);
        assert!(matches!(effect, Effect::Continue));
    }

    #[test]
    fn worker_disconnected_marks_phase_failed_and_finished() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "test" }),
        );
        let (model, effect) = update(model, Msg::WorkerDisconnected);
        assert!(model.finished);
        assert!(matches!(effect, Effect::Continue));
        match &model.entries[0] {
            Entry::Phase(p) => assert!(
                matches!(&p.state, PhaseLifecycle::Failed { error } if error.contains("unexpectedly"))
            ),
            _ => panic!("expected Phase entry"),
        }
    }

    #[test]
    fn worker_disconnected_does_not_overwrite_done_phase() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "test" }),
        );
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseDone {
                result: "ok".into(),
            }),
        );
        let (model, _) = update(model, Msg::WorkerDisconnected);
        // Phase should still be Done, not Failed
        match &model.entries[0] {
            Entry::Phase(p) => assert!(matches!(&p.state, PhaseLifecycle::Done { .. })),
            _ => panic!("expected Phase entry"),
        }
    }

    #[test]
    fn update_on_empty_entries_is_noop() {
        // Worker messages that target "last phase" should be harmless when entries is empty
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, _) = update(model, Msg::Worker(WorkerStatusMsg::Update("text".into())));
        assert!(model.entries.is_empty());
    }

    #[test]
    fn update_on_section_last_entry_is_noop() {
        // Worker messages targeting last phase should skip Section entries
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::Section("Header".into())),
        );
        let (model, _) = update(model, Msg::Worker(WorkerStatusMsg::Update("text".into())));
        // Section should be unchanged, no panic
        assert_eq!(model.entries.len(), 1);
        assert!(matches!(&model.entries[0], Entry::Section(_)));
    }

    #[test]
    fn agent_resolved_doesnt_affect_non_last_phases() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        // Create two phases
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "first" }),
        );
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "second" }),
        );
        // AgentResolved should only affect the last (second) phase
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::AgentResolved("claude".into())),
        );
        match &model.entries[0] {
            Entry::Phase(p) => assert!(p.agent.is_none(), "first phase agent should remain None"),
            _ => panic!("expected Phase entry"),
        }
        match &model.entries[1] {
            Entry::Phase(p) => assert_eq!(p.agent.as_deref(), Some("claude")),
            _ => panic!("expected Phase entry"),
        }
    }

    #[test]
    fn worker_disconnected_noop_when_finished() {
        let mut model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        let (m, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "test" }),
        );
        model = m;
        let (m, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseDone {
                result: "ok".into(),
            }),
        );
        model = m;
        let (m, _) = update(model, Msg::Worker(WorkerStatusMsg::Done));
        model = m;
        assert!(model.finished);
        // WorkerDisconnected after Done should be a no-op for the phase
        let (model, effect) = update(model, Msg::WorkerDisconnected);
        assert!(model.finished);
        assert!(matches!(effect, Effect::Continue));
        match &model.entries[0] {
            Entry::Phase(p) => assert!(
                matches!(&p.state, PhaseLifecycle::Done { .. }),
                "phase should still be Done"
            ),
            _ => panic!("expected Phase entry"),
        }
    }

    #[test]
    fn multiple_phases_update_and_done_affect_only_last() {
        let model = make_model(Screen::TaskRun {
            task_id: "abc".into(),
        });
        // Phase 1: start, update, done
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "phase1" }),
        );
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::Update("running phase1".into())),
        );
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseDone {
                result: "phase1 ok".into(),
            }),
        );
        // Phase 2: start
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::PhaseStarted { name: "phase2" }),
        );
        let (model, _) = update(
            model,
            Msg::Worker(WorkerStatusMsg::Update("running phase2".into())),
        );

        assert_eq!(model.entries.len(), 2);
        // Phase 1 should retain its Done state and original status
        match &model.entries[0] {
            Entry::Phase(p) => {
                assert_eq!(p.name, "phase1");
                assert!(
                    matches!(&p.state, PhaseLifecycle::Done { result } if result == "phase1 ok")
                );
                assert_eq!(p.worker_status.as_deref(), Some("running phase1"));
            }
            _ => panic!("expected Phase entry"),
        }
        // Phase 2 should be active with its own status
        match &model.entries[1] {
            Entry::Phase(p) => {
                assert_eq!(p.name, "phase2");
                assert!(matches!(p.state, PhaseLifecycle::Active));
                assert_eq!(p.worker_status.as_deref(), Some("running phase2"));
            }
            _ => panic!("expected Phase entry"),
        }
    }
}
