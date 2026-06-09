//! TL;DR command — summarize what a closed epic changed.
//!
//! Resolves an epic (by task ID or plan path), validates it's closed with
//! subtasks, extracts ground truth data (plan, subtasks, reviews, diffs),
//! and spawns an interactive agent session to produce the summary.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as _;
use std::process::{Command, ExitStatus};

use chrono::{DateTime, Utc};

use crate::agents::AgentType;
use crate::error::{AikiError, Result};
use crate::output_utils;
use crate::epic::{read_plan_for_epic, resolve_epic_from_plan_path};
use crate::reviews::epic_review_history;
use crate::session::find_active_session;
use crate::tasks::manager::find_task_in_graph;
use crate::tasks::templates::create_review_task_from_template;
use crate::tasks::{
    get_subtasks, materialize_graph, read_events, start_task_core, write_event, Task, TaskEvent,
    TaskOutcome, TaskStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TldrLaunchConfig {
    agent_type: AgentType,
    binary: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TldrPlanPayload {
    mode: &'static str,
    path: Option<String>,
    before_narrative_mode: &'static str,
    content: String,
    warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TldrTaskFinalization {
    Close { summary: String, message: String },
    Stop { reason: String, message: String },
}

/// Arguments for the tldr command
#[derive(clap::Args)]
pub struct TldrArgs {
    /// Epic task ID or plan file path
    pub target: String,
    /// Override agent for interactive session
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

    /// Task template to use (default: tldr)
    #[arg(long)]
    pub template: Option<String>,
}

/// Run the tldr command
pub fn run(args: TldrArgs) -> Result<()> {
    use crate::session::flags::resolve_agent_shorthand;
    let agent_type = resolve_agent_shorthand(args.agent, args.claude, args.codex, args.cursor, args.gemini);

    let cwd = env::current_dir()
        .map_err(|_| AikiError::InvalidArgument("Failed to get current directory".to_string()))?;

    // Read events and build graph
    let events = read_events(&cwd)?;
    let graph = materialize_graph(&events);

    // Step 1: Resolve target → epic task
    let is_plan_path = args.target.contains('/') || args.target.ends_with(".md");
    let epic = if is_plan_path {
        resolve_epic_from_plan_path(&graph, &args.target).map_err(|e| AikiError::Other(e))?
    } else {
        find_task_in_graph(&graph, &args.target)?
    };
    let epic_id = epic.id.clone();

    // Step 2: Validate
    let subtasks = get_subtasks(&graph, &epic_id);
    validate_tldr_epic_target(epic, &graph, &subtasks)?;
    if epic.status != TaskStatus::Closed {
        return Err(AikiError::InvalidArgument(format!(
            "Epic {} is not closed.",
            epic_id
        )));
    }

    // Step 3: Extract ground truth data
    let mut data: HashMap<String, String> = HashMap::new();

    // 3a. Plan content
    let plan_payload = build_tldr_plan_payload(&cwd, &graph, &epic_id);
    insert_tldr_plan_payload_data(&mut data, &plan_payload);
    if let Some(warning) = plan_payload.warning {
        eprintln!("{}", warning);
    }

    // 3b. Epic metadata
    data.insert("epic_id".to_string(), epic_id.clone());
    data.insert("epic_name".to_string(), epic.name.clone());
    data.insert("epic_status".to_string(), format!("{:?}", epic.status));
    if let Some(ref assignee) = epic.assignee {
        data.insert("epic_agent".to_string(), assignee.clone());
    }
    if let (Some(started), Some(closed)) = (epic.started_at, epic.closed_at) {
        let duration = closed - started;
        data.insert("epic_duration".to_string(), format_duration(duration));
    }
    let reviews = epic_review_history(&cwd, &events, &graph, &epic_id);
    data.insert(
        "epic_metadata".to_string(),
        render_epic_metadata(epic, &subtasks, &reviews),
    );

    // 3c. Subtask table
    let mut subtask_rows = Vec::new();
    for st in &subtasks {
        let outcome = st
            .closed_outcome
            .as_ref()
            .map(|o| format!("{:?}", o))
            .unwrap_or_else(|| format!("{:?}", st.status));
        let duration = match (st.started_at, st.closed_at) {
            (Some(s), Some(c)) => format_duration(c - s),
            _ => "—".to_string(),
        };
        subtask_rows.push(format!("| {} | {} | {} |", st.name, outcome, duration));
    }
    let subtask_table = format!(
        "| Name | Outcome | Duration |\n| --- | --- | --- |\n{}",
        subtask_rows.join("\n")
    );
    data.insert("subtask_table".to_string(), subtask_table);
    data.insert(
        "subtask_summary".to_string(),
        render_subtask_summary(&subtasks),
    );

    // 3d. Review history
    data.insert(
        "review_history".to_string(),
        render_review_history_json(&reviews),
    );

    // 3e. Session summary
    data.insert(
        "session_summary".to_string(),
        render_session_summary(epic, &graph, &subtasks, &events),
    );

    // 3f. Diff artifacts are deferred to the agent session.
    // Computing diffs inline is too expensive for large epics (O(n) JJ queries
    // per subtask). The agent uses `aiki task diff` on demand instead.
    data.insert("diff".to_string(), "Run `aiki task diff` to view the diff.".to_string());
    data.insert("files_changed".to_string(), "Run `aiki task diff --summary` to view changed files.".to_string());
    data.insert("file_stats".to_string(), "Run `aiki task diff --stat` to view file stats.".to_string());

    // Step 4: Resolve launch agent and preflight before creating the task
    let launch = resolve_tldr_launch(agent_type)?;
    validate_tldr_launch(launch, |agent| agent.is_installed())?;

    // Step 5: Load template and create task
    let template_name = args.template.as_deref().unwrap_or("tldr");
    let sources = vec![format!("task:{}", epic_id)];
    let assignee = Some(launch.agent_type.as_str().to_string());

    let tldr_task_id =
        create_review_task_from_template(&cwd, &data, &sources, &assignee, template_name)?;

    // Start the task
    start_task_core(&cwd, &[tldr_task_id.clone()])?;

    // Step 6: Spawn interactive agent

    output_utils::emit(|| {
        format!(
            "Spawning {} agent session for tldr task {}...",
            launch.agent_type.display_name(),
            tldr_task_id
        )
    });

    let thread = crate::tasks::lanes::ThreadId::single(tldr_task_id.clone());
    let prompt = format!(
        "You are running an interactive task session with the user. Run `aiki task start {}` to see your instructions, then begin immediately.",
        tldr_task_id
    );

    let status = Command::new(launch.binary)
        .current_dir(&cwd)
        .env("AIKI_THREAD", &thread.serialize())
        .arg(&prompt)
        .status();

    match status {
        Ok(exit_status) => {
            let finalization = classify_tldr_exit_status(exit_status);
            finalize_tldr_task(&cwd, &tldr_task_id, &finalization)?;
            output_utils::emit(|| tldr_finalization_message(&finalization, &epic_id));
        }
        Err(e) => {
            let reason = format!("Failed to spawn interactive TL;DR agent: {}", e);
            finalize_tldr_task(
                &cwd,
                &tldr_task_id,
                &TldrTaskFinalization::Stop {
                    message: reason.clone(),
                    reason,
                },
            )?;
            return Err(AikiError::InvalidArgument(format!(
                "Failed to spawn agent: {}",
                e
            )));
        }
    }

    Ok(())
}

fn classify_tldr_exit_status(exit_status: ExitStatus) -> TldrTaskFinalization {
    if exit_status.success() {
        return TldrTaskFinalization::Close {
            summary: "Interactive TL;DR session completed successfully.".to_string(),
            message: "TL;DR session completed.".to_string(),
        };
    }

    if exit_status.code() == Some(130) {
        return TldrTaskFinalization::Stop {
            reason: "Interactive TL;DR session cancelled by user.".to_string(),
            message: "TL;DR session cancelled by user.".to_string(),
        };
    }

    let detail = exit_status
        .code()
        .map(|code| format!("exited with code {}", code))
        .unwrap_or_else(|| "terminated by signal".to_string());
    TldrTaskFinalization::Stop {
        reason: format!("Interactive TL;DR session {}.", detail),
        message: format!("TL;DR session {}.", detail),
    }
}

fn finalize_tldr_task(cwd: &std::path::Path, task_id: &str, finalization: &TldrTaskFinalization) -> Result<()> {
    let session_match = find_active_session(cwd);
    let session_id = session_match.as_ref().map(|m| m.session_id.clone());
    let turn_id = crate::tasks::current_turn_id(session_match.as_ref().map(|m| m.session_id.as_str()));
    let timestamp = chrono::Utc::now();

    let event = build_tldr_finalization_event(task_id, finalization, session_id, turn_id, timestamp);

    write_event(cwd, &event)
}

fn build_tldr_finalization_event(
    task_id: &str,
    finalization: &TldrTaskFinalization,
    session_id: Option<String>,
    turn_id: Option<String>,
    timestamp: chrono::DateTime<Utc>,
) -> TaskEvent {
    match finalization {
        TldrTaskFinalization::Close { summary, .. } => TaskEvent::Closed {
            task_ids: vec![task_id.to_string()],
            outcome: TaskOutcome::Done,
            confidence: None,
            summary: Some(summary.clone()),
            session_id,
            turn_id,
            timestamp,
        },
        TldrTaskFinalization::Stop { reason, .. } => TaskEvent::Stopped {
            task_ids: vec![task_id.to_string()],
            reason: Some(reason.clone()),
            session_id,
            turn_id,
            timestamp,
        },
    }
}

fn tldr_finalization_message(finalization: &TldrTaskFinalization, epic_id: &str) -> String {
    match finalization {
        TldrTaskFinalization::Close { .. } => {
            format!("TL;DR session completed for epic {}.", epic_id)
        }
        TldrTaskFinalization::Stop { message, .. } => message.clone(),
    }
}

fn validate_tldr_epic_target(
    epic: &Task,
    graph: &crate::tasks::TaskGraph,
    subtasks: &[&Task],
) -> Result<()> {
    if has_tldr_epic_structure(epic, graph, subtasks) {
        return Ok(());
    }

    Err(AikiError::InvalidArgument(format!(
        "Task {} is not an epic. tldr v1 only supports epics.",
        epic.id
    )))
}

fn has_tldr_epic_structure(
    epic: &Task,
    graph: &crate::tasks::TaskGraph,
    subtasks: &[&Task],
) -> bool {
    let has_plan_link = !graph.edges.targets(&epic.id, "implements-plan").is_empty();
    let is_epic_type = epic.task_type.as_deref() == Some("epic");
    (has_plan_link || is_epic_type) && !subtasks.is_empty()
}

fn linked_plan_path_for_epic(graph: &crate::tasks::TaskGraph, epic_id: &str) -> Option<String> {
    graph
        .edges
        .targets(epic_id, "implements-plan")
        .into_iter()
        .find(|target| target.starts_with("file:"))
        .map(|target| target.trim_start_matches("file:").to_string())
}

fn insert_tldr_plan_payload_data(data: &mut HashMap<String, String>, payload: &TldrPlanPayload) {
    data.insert("plan_mode".to_string(), payload.mode.to_string());
    data.insert(
        "before_narrative_mode".to_string(),
        payload.before_narrative_mode.to_string(),
    );
    data.insert(
        "plan_path".to_string(),
        payload.path.clone().unwrap_or_default(),
    );
    data.insert("plan_content".to_string(), payload.content.clone());
}

fn build_tldr_plan_payload(
    cwd: &std::path::Path,
    graph: &crate::tasks::TaskGraph,
    epic_id: &str,
) -> TldrPlanPayload {
    let path = linked_plan_path_for_epic(graph, epic_id);
    let content = read_plan_for_epic(cwd, graph, epic_id);

    match (path, content) {
        (Some(path), Some(content)) => TldrPlanPayload {
            mode: "available",
            path: Some(path),
            before_narrative_mode: "plan-backed",
            content,
            warning: None,
        },
        (Some(path), None) => TldrPlanPayload {
            mode: "missing",
            path: Some(path.clone()),
            before_narrative_mode: "inferred-from-diff",
            content: format!("Plan file unavailable at {}.", path),
            warning: Some(format!(
                "Warning: Plan file for epic {} is missing at {}. Running in degraded mode.",
                epic_id, path
            )),
        },
        (None, _) => TldrPlanPayload {
            mode: "unlinked",
            path: None,
            before_narrative_mode: "inferred-from-diff",
            content: "No linked plan file is available.".to_string(),
            warning: Some(format!(
                "Warning: No linked plan file found for epic {}. Running in degraded mode.",
                epic_id
            )),
        },
    }
}

/// Resolve the effective interactive agent and CLI binary for tldr sessions.
fn resolve_tldr_launch(agent: Option<AgentType>) -> Result<TldrLaunchConfig> {
    let agent_type = agent.unwrap_or(AgentType::ClaudeCode);

    let binary = agent_type.cli_binary().ok_or_else(|| {
        AikiError::InvalidArgument(format!(
            "Agent '{}' does not support interactive `aiki tldr` sessions. {}",
            agent_type.as_str(),
            agent_type.install_hint()
        ))
    })?;

    Ok(TldrLaunchConfig { agent_type, binary })
}

fn validate_tldr_launch(
    launch: TldrLaunchConfig,
    is_installed: impl FnOnce(AgentType) -> bool,
) -> Result<()> {
    if is_installed(launch.agent_type) {
        Ok(())
    } else {
        Err(AikiError::InvalidArgument(format!(
            "Agent '{}' is not installed. {}",
            launch.agent_type.as_str(),
            launch.agent_type.install_hint()
        )))
    }
}

/// Format a chrono::Duration in a human-readable way.
fn format_duration(d: chrono::Duration) -> String {
    let total_secs = d.num_seconds();
    if total_secs < 60 {
        format!("{}s", total_secs)
    } else if total_secs < 3600 {
        format!("{}m", total_secs / 60)
    } else {
        let hours = total_secs / 3600;
        let mins = (total_secs % 3600) / 60;
        if mins == 0 {
            format!("{}h", hours)
        } else {
            format!("{}h{}m", hours, mins)
        }
    }
}

fn render_epic_metadata(
    epic: &Task,
    subtasks: &[&Task],
    reviews: &[crate::reviews::ReviewIteration],
) -> String {
    let completed = subtasks
        .iter()
        .filter(|task| task.status == TaskStatus::Closed)
        .count();
    let duration = match (epic.started_at, epic.closed_at) {
        (Some(started), Some(closed)) => format_duration(closed - started),
        _ => "unknown".to_string(),
    };
    let review_summary = if reviews.is_empty() {
        "none".to_string()
    } else {
        let mut parts = Vec::new();
        for review in reviews {
            let status = match review.outcome.as_str() {
                "approved" => "approved".to_string(),
                "issues_found" => format!("issues_found ({})", review.issues.len()),
                other => other.to_string(),
            };
            parts.push(format!("iteration {}: {}", review.iteration, status));
        }
        parts.join(", ")
    };

    let mut metadata = String::new();
    let _ = writeln!(metadata, "ID: {}", epic.id);
    let _ = writeln!(metadata, "Name: {}", epic.name);
    let _ = writeln!(metadata, "Status: {:?}", epic.status);
    let _ = writeln!(metadata, "Subtasks: {}/{}", completed, subtasks.len());
    let _ = writeln!(metadata, "Duration: {}", duration);
    let _ = writeln!(
        metadata,
        "Agent: {}",
        epic.agent_label().unwrap_or("unknown")
    );
    let _ = write!(metadata, "Reviews: {}", review_summary);
    metadata
}

fn render_subtask_summary(subtasks: &[&Task]) -> String {
    if subtasks.is_empty() {
        return "No subtasks found.".to_string();
    }

    subtasks
        .iter()
        .map(|task| {
            let symbol = match task.closed_outcome {
                Some(TaskOutcome::Done) => "✔",
                Some(TaskOutcome::WontDo) => "–",
                None if task.status == TaskStatus::Stopped => "!",
                None => "•",
            };
            let outcome = task
                .closed_outcome
                .map(|o| o.to_string())
                .unwrap_or_else(|| task.status.to_string());
            let duration = match (task.started_at, task.closed_at) {
                (Some(started), Some(closed)) => format_duration(closed - started),
                (Some(started), None) => format_duration(chrono::Utc::now() - started),
                _ => "unknown".to_string(),
            };
            let mut line = format!("{} {} — {} — {}", symbol, task.name, outcome, duration);
            if let Some(summary) = task.effective_summary() {
                line.push_str(&format!(" — {}", summary));
            } else if let Some(reason) = task.stopped_reason.as_deref() {
                line.push_str(&format!(" — {}", reason));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_review_history_json(reviews: &[crate::reviews::ReviewIteration]) -> String {
    let iterations = reviews
        .iter()
        .map(|review| {
            serde_json::json!({
                "review_task_id": review.review_task_id,
                "iteration": review.iteration,
                "outcome": review.outcome,
                "issues": review.issues.iter().map(|issue| serde_json::json!({
                    "description": issue.description,
                    "severity": issue.severity,
                    "locations": issue.locations,
                })).collect::<Vec<_>>(),
                "fixes": review.fixes.iter().map(|fix| serde_json::json!({
                    "task_id": fix.task_id,
                    "name": fix.name,
                    "outcome": fix.outcome,
                    "summary": fix.summary,
                    "revset": fix.revset,
                    "files_changed": fix.files_changed,
                    "diff_stat": fix.diff_stat,
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();

    serde_json::to_string_pretty(&serde_json::json!({ "iterations": iterations }))
        .unwrap_or_else(|_| "{\n  \"iterations\": []\n}".to_string())
}

fn render_session_summary(
    epic: &Task,
    graph: &crate::tasks::TaskGraph,
    subtasks: &[&Task],
    events: &[crate::tasks::types::TaskEvent],
) -> String {
    let runs = session_summary_runs(epic, graph, subtasks, events);

    let mut total_secs = 0i64;
    let mut total_tokens = 0u64;
    let mut lines = Vec::new();

    for run in &runs {
        let secs = run.elapsed_secs();
        total_secs += secs;
        total_tokens += run.tokens;
        lines.push(session_summary_line(run, secs));
    }

    let mut summary = vec![format!(
        "Total: {} session{} — {} — {} tokens",
        runs.len(),
        if runs.len() == 1 { "" } else { "s" },
        format_duration(chrono::Duration::seconds(total_secs)),
        format_tokens(total_tokens)
    )];
    summary.extend(lines);
    summary.join("\n")
}

fn session_summary_runs<'a>(
    epic: &'a Task,
    graph: &'a crate::tasks::TaskGraph,
    subtasks: &[&'a Task],
    events: &[crate::tasks::types::TaskEvent],
) -> Vec<SessionRun<'a>> {
    let tasks = session_summary_tasks(epic, graph, subtasks);
    let tracked_task_ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    let task_lookup = tasks
        .iter()
        .map(|task| (task.id.as_str(), *task))
        .collect::<HashMap<_, _>>();
    let final_run_index = tasks
        .iter()
        .filter_map(|task| {
            if !has_session_data(task) {
                return None;
            }
            Some((
                task.id.as_str(),
                FinalRunMetadata {
                    tokens: task
                        .data
                        .get("tokens")
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(0),
                    last_session_id: task.last_session_id.as_deref(),
                    turn_closed: task.turn_closed.as_deref(),
                    turn_stopped: task.turn_stopped.as_deref(),
                    closed_at: task.closed_at,
                    status: task.status,
                },
            ))
        })
        .collect::<HashMap<_, _>>();

    let mut active = HashMap::<String, ActiveRun>::new();
    let mut runs = Vec::new();

    for event in events {
        match event {
            crate::tasks::types::TaskEvent::Started {
                task_ids,
                agent_type: _agent_type,
                session_id,
                turn_id,
                timestamp,
                ..
            } => {
                for task_id in task_ids {
                    if !tracked_task_ids.contains(task_id.as_str()) {
                        continue;
                    }
                    active.insert(
                        task_id.clone(),
                        ActiveRun {
                            started_at: *timestamp,
                            session_id: session_id.clone(),
                            turn_id: turn_id.clone(),
                        },
                    );
                }
            }
            crate::tasks::types::TaskEvent::Stopped {
                task_ids,
                turn_id,
                timestamp,
                ..
            } => {
                for task_id in task_ids {
                    if let (Some(task), Some(active_run)) =
                        (task_lookup.get(task_id.as_str()), active.remove(task_id))
                    {
                        runs.push(SessionRun {
                            task,
                            started_at: active_run.started_at,
                            ended_at: *timestamp,
                            status: SessionRunStatus::Stopped,
                            session_id: active_run.session_id,
                            turn_id: turn_id.clone().or(active_run.turn_id),
                            tokens: 0,
                        });
                    }
                }
            }
            crate::tasks::types::TaskEvent::Closed {
                task_ids,
                outcome,
                turn_id,
                timestamp,
                ..
            } => {
                for task_id in task_ids {
                    if let (Some(task), Some(active_run)) =
                        (task_lookup.get(task_id.as_str()), active.remove(task_id))
                    {
                        runs.push(SessionRun {
                            task,
                            started_at: active_run.started_at,
                            ended_at: *timestamp,
                            status: SessionRunStatus::Closed(*outcome),
                            session_id: active_run.session_id,
                            turn_id: turn_id.clone().or(active_run.turn_id),
                            tokens: 0,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    let tasks_with_runs = runs
        .iter()
        .map(|run| run.task.id.as_str())
        .collect::<HashSet<_>>();
    for task in &tasks {
        if tasks_with_runs.contains(task.id.as_str()) || !has_session_data(task) {
            continue;
        }
        if let Some(started_at) = task.started_at {
            let ended_at = task.closed_at.unwrap_or(started_at);
            let status = match task.closed_outcome {
                Some(outcome) => SessionRunStatus::Closed(outcome),
                None if task.status == TaskStatus::Closed => SessionRunStatus::ClosedUnknown,
                None if task.status == TaskStatus::Stopped => SessionRunStatus::Stopped,
                _ => continue,
            };
            runs.push(SessionRun {
                task,
                started_at,
                ended_at,
                status,
                session_id: task.last_session_id.clone(),
                turn_id: task.turn_closed.clone().or(task.turn_stopped.clone()),
                tokens: 0,
            });
        }
    }

    for run in &mut runs {
        if let Some(final_run) = final_run_index.get(run.task.id.as_str()) {
            let same_session = final_run.last_session_id == run.session_id.as_deref();
            let same_turn = final_run.turn_closed == run.turn_id.as_deref()
                || final_run.turn_stopped == run.turn_id.as_deref();
            let same_terminal_timestamp = final_run.closed_at == Some(run.ended_at)
                && matches!(final_run.status, TaskStatus::Closed);

            if same_session || same_turn || same_terminal_timestamp {
                run.tokens = final_run.tokens;
            }
        }
    }

    runs.sort_by_key(|run| (run.started_at, run.task.created_at));
    runs
}

fn session_summary_tasks<'a>(
    epic: &'a Task,
    graph: &'a crate::tasks::TaskGraph,
    subtasks: &[&'a Task],
) -> Vec<&'a Task> {
    let mut tasks = vec![epic];
    tasks.extend(subtasks.iter().copied());
    tasks.extend(crate::tasks::manager::get_all_descendants(graph, &epic.id));
    tasks.sort_by_key(|task| task.created_at);

    let mut seen = HashSet::new();
    tasks.retain(|task| seen.insert(task.id.clone()));
    tasks
}

fn has_session_data(task: &Task) -> bool {
    task.started_at.is_some() || task.data.contains_key("tokens") || task.last_session_id.is_some()
}

fn session_summary_line(run: &SessionRun<'_>, secs: i64) -> String {
    let symbol = match run.status {
        SessionRunStatus::Closed(TaskOutcome::Done) => "✔",
        SessionRunStatus::Closed(TaskOutcome::WontDo) => "–",
        SessionRunStatus::ClosedUnknown => "•",
        SessionRunStatus::Stopped => "!",
    };

    let mut line = format!(
        "{} {} — {}",
        symbol,
        run.task.name,
        format_duration(chrono::Duration::seconds(secs))
    );
    if let Some(session_id) = &run.session_id {
        let short = &session_id[..session_id.len().min(8)];
        line.push_str(&format!(" [session {}]", short));
    }
    line
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{}", tokens)
    }
}

struct ActiveRun {
    started_at: DateTime<Utc>,
    session_id: Option<String>,
    turn_id: Option<String>,
}

struct FinalRunMetadata<'a> {
    tokens: u64,
    last_session_id: Option<&'a str>,
    turn_closed: Option<&'a str>,
    turn_stopped: Option<&'a str>,
    closed_at: Option<DateTime<Utc>>,
    status: TaskStatus,
}

struct SessionRun<'a> {
    task: &'a Task,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    status: SessionRunStatus,
    session_id: Option<String>,
    turn_id: Option<String>,
    tokens: u64,
}

impl SessionRun<'_> {
    fn elapsed_secs(&self) -> i64 {
        (self.ended_at - self.started_at).num_seconds().max(0)
    }
}

enum SessionRunStatus {
    Closed(TaskOutcome),
    ClosedUnknown,
    Stopped,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reviews::{ReviewFix, ReviewIssue, ReviewIteration};
    use crate::tasks::graph::EdgeStore;
    use crate::tasks::types::{FastHashMap, TaskComment, TaskEvent, TaskPriority};
    use chrono::{Duration, Utc};
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    use tempfile::tempdir;

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
            created_at: Utc::now(),
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
            comments: Vec::<TaskComment>::new(),
        }
    }

    fn started_event(
        task_id: &str,
        agent_type: &str,
        session_id: &str,
        timestamp: DateTime<Utc>,
    ) -> TaskEvent {
        TaskEvent::Started {
            task_ids: vec![task_id.to_string()],
            agent_type: agent_type.to_string(),
            session_id: Some(session_id.to_string()),
            turn_id: None,
            working_copy: None,
            instructions: None,
            timestamp,
        }
    }

    fn stopped_event(task_id: &str, session_id: &str, timestamp: DateTime<Utc>) -> TaskEvent {
        TaskEvent::Stopped {
            task_ids: vec![task_id.to_string()],
            reason: Some("paused".to_string()),
            session_id: Some(session_id.to_string()),
            turn_id: None,
            timestamp,
        }
    }

    fn closed_event(task_id: &str, session_id: &str, timestamp: DateTime<Utc>) -> TaskEvent {
        TaskEvent::Closed {
            task_ids: vec![task_id.to_string()],
            outcome: TaskOutcome::Done,
            confidence: None,
            summary: None,
            session_id: Some(session_id.to_string()),
            turn_id: None,
            timestamp,
        }
    }

    #[test]
    fn session_summary_includes_nested_descendants() {
        let mut epic = make_task("epic", "Epic", TaskStatus::Closed);
        epic.closed_outcome = Some(TaskOutcome::Done);
        epic.started_at = Some(Utc::now() - Duration::minutes(10));
        epic.closed_at = Some(Utc::now());
        epic.data.insert("tokens".to_string(), "300".to_string());

        let mut parent = make_task("fix-parent", "Fix Parent", TaskStatus::Closed);
        parent.closed_outcome = Some(TaskOutcome::Done);
        parent.assignee = Some("codex".to_string());
        parent.started_at = Some(Utc::now() - Duration::minutes(4));
        parent.closed_at = Some(Utc::now() - Duration::minutes(2));
        parent.data.insert("tokens".to_string(), "1500".to_string());

        let mut child = make_task("fix-child", "Fix Child", TaskStatus::Closed);
        child.assignee = Some("claude-code".to_string());
        child.started_at = Some(Utc::now() - Duration::minutes(2));
        child.closed_at = Some(Utc::now());
        child.data.insert("tokens".to_string(), "700".to_string());

        let mut tasks = FastHashMap::default();
        tasks.insert(epic.id.clone(), epic.clone());
        tasks.insert(parent.id.clone(), parent.clone());
        tasks.insert(child.id.clone(), child);

        let mut edges = EdgeStore::new();
        edges.add("fix-parent", "epic", "subtask-of");
        edges.add("fix-child", "fix-parent", "subtask-of");

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let subtasks = vec![graph.tasks.get("fix-parent").unwrap()];
        let summary = render_session_summary(&epic, &graph, &subtasks, &[]);
        assert!(summary.contains("Total: 3 sessions"));
        assert!(summary.contains("2.5K tokens"));
        assert!(summary.contains("✔ Epic — 10m"));
        assert!(summary.contains("✔ Fix Parent — 2m"));
        assert!(summary.contains("• Fix Child — 2m"));
    }

    #[test]
    fn session_summary_renders_per_task_lines_instead_of_agent_aggregates() {
        let mut epic = make_task("epic", "Epic", TaskStatus::Closed);
        epic.closed_outcome = Some(TaskOutcome::Done);
        epic.assignee = Some("codex".to_string());
        epic.started_at = Some(Utc::now() - Duration::minutes(6));
        epic.closed_at = Some(Utc::now() - Duration::minutes(3));
        epic.data.insert("tokens".to_string(), "900".to_string());

        let mut subtask = make_task("subtask", "Write Tests", TaskStatus::Closed);
        subtask.closed_outcome = Some(TaskOutcome::Done);
        subtask.assignee = Some("claude-code".to_string());
        subtask.started_at = Some(Utc::now() - Duration::minutes(3));
        subtask.closed_at = Some(Utc::now());
        subtask
            .data
            .insert("tokens".to_string(), "1100".to_string());

        let mut tasks = FastHashMap::default();
        tasks.insert(epic.id.clone(), epic.clone());
        tasks.insert(subtask.id.clone(), subtask);

        let mut edges = EdgeStore::new();
        edges.add("subtask", "epic", "subtask-of");

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let subtasks = vec![graph.tasks.get("subtask").unwrap()];
        let summary = render_session_summary(&epic, &graph, &subtasks, &[]);
        assert!(summary.contains("Total: 2 sessions — 6m — 2.0K tokens"));
        assert!(summary.contains("✔ Epic — 3m"));
        assert!(summary.contains("✔ Write Tests — 3m"));
        assert!(!summary.contains("codex: 1 session"));
        assert!(!summary.contains("claude-code: 1 session"));
    }

    #[test]
    fn session_summary_counts_resumed_tasks_as_multiple_runs() {
        let base = Utc::now();

        let mut epic = make_task("epic", "Epic", TaskStatus::Closed);
        epic.closed_outcome = Some(TaskOutcome::Done);
        epic.started_at = Some(base);
        epic.closed_at = Some(base + Duration::minutes(10));
        epic.last_session_id = Some("epic-session".to_string());

        let mut subtask = make_task("subtask", "Write Tests", TaskStatus::Closed);
        subtask.closed_outcome = Some(TaskOutcome::Done);
        subtask.started_at = Some(base + Duration::minutes(4));
        subtask.closed_at = Some(base + Duration::minutes(6));
        subtask.last_session_id = Some("resume-2".to_string());

        let mut tasks = FastHashMap::default();
        tasks.insert(epic.id.clone(), epic.clone());
        tasks.insert(subtask.id.clone(), subtask);

        let mut edges = EdgeStore::new();
        edges.add("subtask", "epic", "subtask-of");

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let events = vec![
            started_event("epic", "codex", "epic-session", base),
            started_event("subtask", "codex", "resume-1", base + Duration::minutes(1)),
            stopped_event("subtask", "resume-1", base + Duration::minutes(3)),
            started_event("subtask", "codex", "resume-2", base + Duration::minutes(4)),
            closed_event("subtask", "resume-2", base + Duration::minutes(6)),
            closed_event("epic", "epic-session", base + Duration::minutes(10)),
        ];

        let subtasks = vec![graph.tasks.get("subtask").unwrap()];
        let summary = render_session_summary(&epic, &graph, &subtasks, &events);
        assert!(summary.contains("Total: 3 sessions — 14m — 0 tokens"));
        assert_eq!(summary.matches("Write Tests").count(), 2);
    }

    #[test]
    fn session_summary_surfaces_handoffs_as_distinct_runs() {
        let base = Utc::now();

        let mut epic = make_task("epic", "Epic", TaskStatus::Closed);
        epic.closed_outcome = Some(TaskOutcome::Done);
        epic.started_at = Some(base);
        epic.closed_at = Some(base + Duration::minutes(8));
        epic.last_session_id = Some("epic-session".to_string());

        let mut subtask = make_task("handoff", "Implement Parser", TaskStatus::Closed);
        subtask.closed_outcome = Some(TaskOutcome::Done);
        subtask.started_at = Some(base + Duration::minutes(3));
        subtask.closed_at = Some(base + Duration::minutes(5));
        subtask.last_session_id = Some("beta5678".to_string());

        let mut tasks = FastHashMap::default();
        tasks.insert(epic.id.clone(), epic.clone());
        tasks.insert(subtask.id.clone(), subtask);

        let mut edges = EdgeStore::new();
        edges.add("handoff", "epic", "subtask-of");

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let events = vec![
            started_event("epic", "codex", "epic-session", base),
            started_event("handoff", "codex", "alpha1234", base + Duration::minutes(1)),
            stopped_event("handoff", "alpha1234", base + Duration::minutes(3)),
            started_event(
                "handoff",
                "claude-code",
                "beta5678",
                base + Duration::minutes(3),
            ),
            closed_event("handoff", "beta5678", base + Duration::minutes(5)),
            closed_event("epic", "epic-session", base + Duration::minutes(8)),
        ];

        let subtasks = vec![graph.tasks.get("handoff").unwrap()];
        let summary = render_session_summary(&epic, &graph, &subtasks, &events);
        assert!(summary.contains("Total: 3 sessions — 12m — 0 tokens"));
        assert!(summary.contains("Implement Parser — 2m [session alpha123]"));
        assert!(summary.contains("Implement Parser — 2m [session beta5678]"));
        assert_eq!(summary.matches("Implement Parser").count(), 2);
    }

    #[test]
    fn session_summary_reconstructs_closed_descendant_run_without_outcome() {
        let base = Utc::now();

        let mut epic = make_task("epic", "Epic", TaskStatus::Closed);
        epic.closed_outcome = Some(TaskOutcome::Done);
        epic.started_at = Some(base);
        epic.closed_at = Some(base + Duration::minutes(6));

        let mut descendant = make_task("descendant", "Nested Descendant", TaskStatus::Closed);
        descendant.started_at = Some(base + Duration::minutes(2));
        descendant.closed_at = Some(base + Duration::minutes(5));
        descendant
            .data
            .insert("tokens".to_string(), "700".to_string());

        let mut tasks = FastHashMap::default();
        tasks.insert(epic.id.clone(), epic.clone());
        tasks.insert(descendant.id.clone(), descendant);

        let mut edges = EdgeStore::new();
        edges.add("descendant", "epic", "subtask-of");

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let subtasks = vec![graph.tasks.get("descendant").unwrap()];
        let summary = render_session_summary(&epic, &graph, &subtasks, &[]);
        assert!(summary.contains("Total: 2 sessions — 9m — 700 tokens"));
        assert!(summary.contains("• Nested Descendant — 3m"));
    }

    #[test]
    fn subtask_summary_renders_outcomes_and_summaries() {
        let mut task = make_task("sub1", "Write tests", TaskStatus::Closed);
        task.closed_outcome = Some(TaskOutcome::Done);
        task.started_at = Some(Utc::now() - Duration::minutes(1));
        task.closed_at = Some(Utc::now());
        task.summary = Some("Added regression coverage".to_string());

        let rendered = render_subtask_summary(&[&task]);
        assert!(rendered.contains("✔ Write tests"));
        assert!(rendered.contains("done"));
        assert!(rendered.contains("Added regression coverage"));
    }

    #[test]
    fn review_history_json_renders_empty_iterations() {
        let rendered = render_review_history_json(&[]);
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed, serde_json::json!({ "iterations": [] }));
    }

    #[test]
    fn review_history_json_preserves_structured_fix_metadata() {
        let reviews = vec![
            ReviewIteration {
                review_task_id: "review-1".to_string(),
                iteration: 1,
                outcome: "issues_found".to_string(),
                issues: vec![ReviewIssue {
                    description: "Handle duplicate plan matches".to_string(),
                    severity: "high".to_string(),
                    locations: vec!["cli/src/plans/graph.rs:42".to_string()],
                }],
                fixes: vec![
                    ReviewFix {
                        task_id: "fix-1".to_string(),
                        name: "Fix graph ambiguity".to_string(),
                        outcome: "Done".to_string(),
                        summary: Some("Removed newest-match fallback".to_string()),
                        revset: "description(substring:\"task=fix-1\") ~ ::aiki/tasks".to_string(),
                        files_changed: vec!["cli/src/plans/graph.rs".to_string()],
                        diff_stat: Some(
                            "1 file changed, 10 insertions(+), 2 deletions(-)".to_string(),
                        ),
                    },
                    ReviewFix {
                        task_id: "fix-2".to_string(),
                        name: "Add tests".to_string(),
                        outcome: "Done".to_string(),
                        summary: None,
                        revset: "description(substring:\"task=fix-2\") ~ ::aiki/tasks".to_string(),
                        files_changed: vec!["cli/src/plans/graph.rs".to_string()],
                        diff_stat: None,
                    },
                ],
                fix_task_id: Some("fix-1".to_string()),
            },
            ReviewIteration {
                review_task_id: "review-2".to_string(),
                iteration: 2,
                outcome: "approved".to_string(),
                issues: vec![],
                fixes: vec![],
                fix_task_id: None,
            },
        ];

        let rendered = render_review_history_json(&reviews);
        assert!(!rendered.contains("Review #1"));
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["iterations"][0]["review_task_id"], "review-1");
        assert_eq!(parsed["iterations"][0]["issues"][0]["severity"], "high");
        assert_eq!(parsed["iterations"][0]["fixes"][0]["task_id"], "fix-1");
        assert_eq!(
            parsed["iterations"][0]["fixes"][0]["files_changed"][0],
            "cli/src/plans/graph.rs"
        );
        assert_eq!(parsed["iterations"][0]["fixes"][1]["task_id"], "fix-2");
        assert_eq!(parsed["iterations"][1]["outcome"], "approved");
        assert_eq!(parsed["iterations"][1]["fixes"], serde_json::json!([]));
    }

    #[test]
    fn validate_tldr_epic_target_accepts_renamed_epic_with_subtasks_by_task_id() {
        let parent = make_task("epic-parent", "Review Container", TaskStatus::Closed);
        let child = make_task("review-child", "Review Child", TaskStatus::Closed);

        let mut tasks = FastHashMap::default();
        tasks.insert(parent.id.clone(), parent.clone());
        tasks.insert(child.id.clone(), child);

        let mut edges = EdgeStore::new();
        edges.add("review-child", "epic-parent", "subtask-of");
        edges.add("epic-parent", "file:ops/now/review.md", "implements-plan");

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let subtasks = get_subtasks(&graph, &parent.id);
        validate_tldr_epic_target(&parent, &graph, &subtasks).unwrap();
    }

    #[test]
    fn validate_tldr_epic_target_rejects_parent_task_id_without_plan_link() {
        let parent = make_task("review-parent", "Review Container", TaskStatus::Closed);
        let child = make_task("review-child", "Review Child", TaskStatus::Closed);

        let mut tasks = FastHashMap::default();
        tasks.insert(parent.id.clone(), parent.clone());
        tasks.insert(child.id.clone(), child);

        let mut edges = EdgeStore::new();
        edges.add("review-child", "review-parent", "subtask-of");

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let subtasks = get_subtasks(&graph, &parent.id);
        let err = validate_tldr_epic_target(&parent, &graph, &subtasks).unwrap_err();
        assert_eq!(
            format!("{}", err),
            "Task review-parent is not an epic. tldr v1 only supports epics."
        );
    }

    #[test]
    fn build_tldr_plan_payload_marks_missing_linked_plan_with_specific_path() {
        let epic = make_task("epic", "Epic: Missing Plan", TaskStatus::Closed);

        let mut tasks = FastHashMap::default();
        tasks.insert(epic.id.clone(), epic);

        let mut edges = EdgeStore::new();
        edges.add("epic", "file:ops/now/missing-plan.md", "implements-plan");

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let cwd = tempdir().unwrap();
        let payload = build_tldr_plan_payload(cwd.path(), &graph, "epic");

        assert_eq!(payload.mode, "missing");
        assert_eq!(payload.path.as_deref(), Some("ops/now/missing-plan.md"));
        assert_eq!(payload.before_narrative_mode, "inferred-from-diff");
        assert!(payload.content.contains("ops/now/missing-plan.md"));
        assert_ne!(payload.content, "None");
        assert_eq!(
            payload.warning.as_deref(),
            Some(
                "Warning: Plan file for epic epic is missing at ops/now/missing-plan.md. Running in degraded mode."
            )
        );
    }

    #[test]
    fn build_tldr_plan_payload_marks_unlinked_plan_without_none_sentinel() {
        let epic = make_task("epic", "Epic: Unlinked", TaskStatus::Closed);

        let mut tasks = FastHashMap::default();
        tasks.insert(epic.id.clone(), epic);

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges: EdgeStore::new(),
            slug_index: FastHashMap::default(),
        };

        let cwd = tempdir().unwrap();
        let payload = build_tldr_plan_payload(cwd.path(), &graph, "epic");

        assert_eq!(payload.mode, "unlinked");
        assert_eq!(payload.path, None);
        assert_eq!(payload.before_narrative_mode, "inferred-from-diff");
        assert_eq!(payload.content, "No linked plan file is available.");
        assert_ne!(payload.content, "None");
        assert_eq!(
            payload.warning.as_deref(),
            Some("Warning: No linked plan file found for epic epic. Running in degraded mode.")
        );
    }

    #[test]
    fn insert_tldr_plan_payload_data_keeps_unlinked_plan_path_empty() {
        let payload = TldrPlanPayload {
            mode: "unlinked",
            path: None,
            before_narrative_mode: "inferred-from-diff",
            content: "No linked plan file is available.".to_string(),
            warning: None,
        };
        let mut data = HashMap::new();

        insert_tldr_plan_payload_data(&mut data, &payload);

        assert_eq!(data.get("plan_mode").map(String::as_str), Some("unlinked"));
        assert_eq!(data.get("plan_path").map(String::as_str), Some(""));
        assert_ne!(data.get("plan_path").map(String::as_str), Some("unlinked"));
        assert_eq!(
            data.get("plan_content").map(String::as_str),
            Some("No linked plan file is available.")
        );
    }

    #[test]
    fn tldr_template_guides_inferred_before_narrative_when_plan_is_missing() {
        let template = include_str!("../tasks/templates/core/tldr.md");

        assert!(template.contains("<plan-status>"));
        assert!(template.contains("plan-status.mode"));
        assert!(template.contains("diff-based"));
        assert!(template.contains("inferred/unsourced"));
        assert!(template.contains("only when that field is non-empty"));
        assert!(!template.contains("shown as empty or \"None\""));
    }

    #[test]
    fn validate_tldr_epic_target_rejects_non_parent_task_id_target() {
        let leaf = make_task("review-leaf", "Review Leaf", TaskStatus::Closed);

        let mut tasks = FastHashMap::default();
        tasks.insert(leaf.id.clone(), leaf.clone());

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges: EdgeStore::new(),
            slug_index: FastHashMap::default(),
        };

        let subtasks = get_subtasks(&graph, &leaf.id);
        let err = validate_tldr_epic_target(&leaf, &graph, &subtasks).unwrap_err();
        assert_eq!(
            format!("{}", err),
            "Task review-leaf is not an epic. tldr v1 only supports epics."
        );
    }

    #[test]
    fn validate_tldr_epic_target_accepts_renamed_plan_path_target() {
        let parent = make_task("review-parent", "Review Container", TaskStatus::Closed);
        let child = make_task("review-child", "Review Child", TaskStatus::Closed);

        let mut tasks = FastHashMap::default();
        tasks.insert(parent.id.clone(), parent);
        tasks.insert(child.id.clone(), child);

        let mut edges = EdgeStore::new();
        edges.add("review-child", "review-parent", "subtask-of");
        edges.add("review-parent", "file:ops/now/review.md", "implements-plan");

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let resolved = resolve_epic_from_plan_path(&graph, "ops/now/review.md").unwrap();
        let subtasks = get_subtasks(&graph, &resolved.id);
        validate_tldr_epic_target(resolved, &graph, &subtasks).unwrap();
    }

    #[test]
    fn validate_tldr_epic_target_accepts_epic_task_type_without_plan_link() {
        let mut parent = make_task("epic-parent", "Epic: My Feature", TaskStatus::Closed);
        parent.task_type = Some("epic".to_string());
        let child = make_task("child-task", "Subtask 1", TaskStatus::Closed);

        let mut tasks = FastHashMap::default();
        tasks.insert(parent.id.clone(), parent.clone());
        tasks.insert(child.id.clone(), child);

        let mut edges = EdgeStore::new();
        edges.add("child-task", "epic-parent", "subtask-of");

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let subtasks = get_subtasks(&graph, &parent.id);
        validate_tldr_epic_target(&parent, &graph, &subtasks).unwrap();
    }

    #[test]
    fn validate_tldr_epic_target_rejects_plan_path_target_without_subtasks() {
        let parent = make_task("epic-parent", "Epic: Empty Plan Epic", TaskStatus::Closed);

        let mut tasks = FastHashMap::default();
        tasks.insert(parent.id.clone(), parent);

        let mut edges = EdgeStore::new();
        edges.add(
            "epic-parent",
            "file:ops/now/empty-plan.md",
            "implements-plan",
        );

        let graph = crate::tasks::TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let resolved = resolve_epic_from_plan_path(&graph, "ops/now/empty-plan.md").unwrap();
        let subtasks = get_subtasks(&graph, &resolved.id);
        let err = validate_tldr_epic_target(resolved, &graph, &subtasks).unwrap_err();
        assert_eq!(
            format!("{}", err),
            "Task epic-parent is not an epic. tldr v1 only supports epics."
        );
    }

    #[test]
    fn resolve_tldr_launch_defaults_to_claude_without_session() {
        let launch = resolve_tldr_launch(None).unwrap();
        assert_eq!(launch.agent_type, AgentType::ClaudeCode);
        assert_eq!(launch.binary, "claude");
    }

    #[test]
    fn resolve_tldr_launch_explicit_agent_overrides_default() {
        let launch = resolve_tldr_launch(Some(AgentType::Codex)).unwrap();
        assert_eq!(launch.agent_type, AgentType::Codex);
        assert_eq!(launch.binary, "codex");
    }

    #[test]
    fn resolve_tldr_launch_rejects_non_spawnable_session_agent() {
        let err = resolve_tldr_launch(Some(AgentType::Cursor)).unwrap_err();
        let rendered = format!("{}", err);
        assert!(rendered.contains("does not support interactive `aiki tldr` sessions"));
        assert!(rendered.contains(&AgentType::Cursor.install_hint()));
    }

    #[cfg(unix)]
    #[test]
    fn classify_tldr_exit_status_closes_successful_runs() {
        let finalization = classify_tldr_exit_status(ExitStatus::from_raw(0));
        assert_eq!(
            finalization,
            TldrTaskFinalization::Close {
                summary: "Interactive TL;DR session completed successfully.".to_string(),
                message: "TL;DR session completed.".to_string(),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn classify_tldr_exit_status_stops_cancelled_runs() {
        let finalization = classify_tldr_exit_status(ExitStatus::from_raw(130 << 8));
        assert_eq!(
            finalization,
            TldrTaskFinalization::Stop {
                reason: "Interactive TL;DR session cancelled by user.".to_string(),
                message: "TL;DR session cancelled by user.".to_string(),
            }
        );
    }

    #[test]
    fn build_tldr_finalization_event_closes_task_with_summary() {
        let timestamp = Utc::now();
        let event = build_tldr_finalization_event(
            "tldr-task",
            &TldrTaskFinalization::Close {
                summary: "Interactive TL;DR session completed successfully.".to_string(),
                message: "TL;DR session completed.".to_string(),
            },
            Some("session-1".to_string()),
            Some("turn-1".to_string()),
            timestamp,
        );

        match event {
            TaskEvent::Closed {
                task_ids,
                outcome,
                confidence,
                summary,
                session_id,
                turn_id,
                timestamp: event_timestamp,
            } => {
                assert_eq!(task_ids, vec!["tldr-task".to_string()]);
                assert_eq!(outcome, TaskOutcome::Done);
                assert_eq!(confidence, None);
                assert_eq!(
                    summary.as_deref(),
                    Some("Interactive TL;DR session completed successfully.")
                );
                assert_eq!(session_id.as_deref(), Some("session-1"));
                assert_eq!(turn_id.as_deref(), Some("turn-1"));
                assert_eq!(event_timestamp, timestamp);
            }
            other => panic!("expected Closed event, got {:?}", other),
        }
    }

    #[test]
    fn build_tldr_finalization_event_stops_task_with_reason() {
        let timestamp = Utc::now();
        let event = build_tldr_finalization_event(
            "tldr-task",
            &TldrTaskFinalization::Stop {
                reason: "Interactive TL;DR session exited with code 1.".to_string(),
                message: "TL;DR session exited with code 1.".to_string(),
            },
            Some("session-1".to_string()),
            Some("turn-1".to_string()),
            timestamp,
        );

        match event {
            TaskEvent::Stopped {
                task_ids,
                reason,
                session_id,
                turn_id,
                timestamp: event_timestamp,
            } => {
                assert_eq!(task_ids, vec!["tldr-task".to_string()]);
                assert_eq!(
                    reason.as_deref(),
                    Some("Interactive TL;DR session exited with code 1.")
                );
                assert_eq!(session_id.as_deref(), Some("session-1"));
                assert_eq!(turn_id.as_deref(), Some("turn-1"));
                assert_eq!(event_timestamp, timestamp);
            }
            other => panic!("expected Stopped event, got {:?}", other),
        }
    }

    #[test]
    fn validate_tldr_launch_rejects_uninstalled_spawnable_agent() {
        let launch = TldrLaunchConfig {
            agent_type: AgentType::Codex,
            binary: "codex",
        };

        let err = validate_tldr_launch(launch, |_| false).unwrap_err();
        let rendered = format!("{}", err);
        assert!(rendered.contains("Agent 'codex' is not installed."));
        assert!(rendered.contains(&AgentType::Codex.install_hint()));
    }
}
