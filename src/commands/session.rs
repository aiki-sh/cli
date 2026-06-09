use crate::error::{AikiError, Result};
use crate::global;
use crate::history;
use crate::history::types::ConversationEvent;
use crate::provenance::record::AgentType;
use crate::session::flags::{AgentFilterFlags, AgentFlags, SessionIdFlags};
use crate::session::{self, SessionMode};
use crate::tasks::{self, md::short_id, types::TaskStatus};
use clap::Subcommand;
use std::collections::HashMap;
use std::path::Path;

/// Resolve a session ID from a thread task ID.
///
/// Scans SessionStart events for one whose `run_thread_id` contains the given
/// task ID as head or tail.
fn resolve_session_from_thread(task_id: &str) -> Result<String> {
    let aiki_dir = global::global_aiki_dir();
    let events = history::storage::read_events(&aiki_dir)?;

    // Search in reverse (most recent first) for a SessionStart whose
    // run_thread_id head or tail starts with the given prefix.
    for event in events.iter().rev() {
        if let ConversationEvent::SessionStart {
            session_id,
            run_thread_id: Some(thread_wire),
            ..
        } = event
        {
            // thread_wire is "head:tail" or "head" (single-task)
            let (head, tail) = thread_wire
                .split_once(':')
                .map(|(h, t)| (h, t))
                .unwrap_or((thread_wire.as_str(), thread_wire.as_str()));

            if head.starts_with(task_id) || tail.starts_with(task_id) {
                return Ok(session_id.clone());
            }
        }
    }

    Err(AikiError::InvalidArgument(format!(
        "No session found with thread matching '{}'",
        task_id
    )))
}

/// Output format for the `transcript` subcommand.
#[derive(Clone, Debug, PartialEq, clap::ValueEnum)]
pub enum TranscriptOutput {
    /// Print only the file path (no content)
    Path,
}

#[derive(Subcommand)]
#[command(disable_help_subcommand = true)]
pub enum SessionCommands {
    /// List sessions
    List {
        /// Show only active sessions (with running agent process)
        #[arg(long)]
        active: bool,
        /// Show only background sessions (created by `aiki run`)
        #[arg(long, conflicts_with = "interactive")]
        background: bool,
        /// Show only interactive sessions (user-driven)
        #[arg(long, conflicts_with = "background")]
        interactive: bool,
        /// Limit the number of results shown
        #[arg(long, short = 'n')]
        number: Option<usize>,
        #[command(flatten)]
        agent: AgentFilterFlags,
    },
    /// Show summary for a specific session
    Show {
        /// Session ID (or unique prefix)
        #[arg(conflicts_with_all = ["agent_session", "thread"])]
        id: Option<String>,

        #[command(flatten)]
        session: SessionIdFlags,

        /// Look up session by thread task ID (or prefix)
        #[arg(long, conflicts_with = "agent_session")]
        thread: Option<String>,
    },
    /// Wait for session(s) to complete
    Wait {
        /// Session IDs to wait for
        ids: Vec<String>,
        /// Return when any session completes (instead of waiting for all)
        #[arg(long)]
        any: bool,
        /// Output format (e.g., `id` for bare session IDs on stdout)
        #[arg(long, short = 'o')]
        output: Option<super::OutputFormat>,
    },
    /// Show the transcript for a session
    Transcript {
        /// Session ID (or unique prefix)
        #[arg(conflicts_with_all = ["agent_session", "thread"])]
        id: Option<String>,

        #[command(flatten)]
        session: SessionIdFlags,

        /// Look up session by thread task ID (or prefix)
        #[arg(long, conflicts_with = "agent_session")]
        thread: Option<String>,

        /// Output format: `path` to print just the file path
        #[arg(long, short = 'o')]
        output: Option<TranscriptOutput>,
    },
}

pub fn run(command: SessionCommands) -> Result<()> {
    match command {
        SessionCommands::List {
            active,
            background,
            interactive,
            number,
            agent,
        } => run_list(active, background, interactive, number, agent),
        SessionCommands::Show {
            id,
            session,
            thread,
        } => {
            let effective_id = if let Some(task_id) = thread {
                resolve_session_from_thread(&task_id)?
            } else {
                resolve_effective_id(id, &session)?
            };
            run_show(&effective_id)
        }
        SessionCommands::Wait { ids, any, output } => run_wait(ids, any, output),
        SessionCommands::Transcript {
            id,
            session,
            thread,
            output,
        } => {
            let effective_id = if let Some(task_id) = thread {
                resolve_session_from_thread(&task_id)?
            } else {
                resolve_effective_id(id, &session)?
            };
            run_transcript(&effective_id, output)
        }
    }
}

/// Resolve the effective session ID from either a positional argument or agent flags.
fn resolve_effective_id(id: Option<String>, session: &SessionIdFlags) -> Result<String> {
    if let Some(uuid) = session.session_uuid() {
        Ok(uuid)
    } else if let Some(id) = id {
        Ok(id)
    } else {
        Err(AikiError::InvalidArgument(
            "Provide a session ID or use --claude/--codex/--cursor/--gemini".into(),
        ))
    }
}

fn format_timestamp(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn format_time_only(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%H:%M").to_string()
}

fn format_duration_between(
    start: &chrono::DateTime<chrono::Utc>,
    end: &chrono::DateTime<chrono::Utc>,
) -> String {
    let secs = (*end - *start).num_seconds().max(0);
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("{}h", h)
        } else {
            format!("{}h{}m", h, m)
        }
    }
}

struct SessionRow {
    session_id: String,
    agent_type: AgentType,
    mode: SessionMode,
    pid: Option<u32>,
    turns: String,
    started: String,
    last_activity: String,
    status: &'static str,
}

fn print_session_table(rows: &[SessionRow]) {
    println!(
        "{:<38}  {:<20}  {:<12}  {:<7}  {:<20}  {:<20}  {}",
        "SESSION", "AGENT", "TYPE", "TURNS", "STARTED", "LAST ACTIVITY", "STATUS"
    );

    for row in rows {
        let agent_display = match row.pid {
            Some(pid) => format!("{} ({})", row.agent_type, pid),
            None => row.agent_type.to_string(),
        };

        println!(
            "{:<38}  {:<20}  {:<12}  {:<7}  {:<20}  {:<20}  {}",
            row.session_id,
            agent_display,
            row.mode.to_string(),
            row.turns,
            row.started,
            row.last_activity,
            row.status
        );
    }

    let count = rows.len();
    println!("\n{} session{}", count, if count == 1 { "" } else { "s" });
}

/// Print sessions with their tasks listed underneath.
///
/// Output format:
/// ```text
/// 2 Sessions Ended
/// a1b2c3d4 · claude-code · 5 turns · 3m
///   ✔ osonox  Fix auth bug (confidence: 3)
///             Updated token validation to check expiry
///   ✘ prqxyz  Add caching (stopped)
/// ```
fn print_sessions_with_tasks(
    session_ids: &[&str],
    conv_map: &HashMap<&str, &history::types::ConversationSummary>,
    task_graph: &tasks::TaskGraph,
) {
    let count = session_ids.len();
    println!(
        "{} Session{} Ended",
        count,
        if count == 1 { "" } else { "s" }
    );

    for (i, session_id) in session_ids.iter().enumerate() {
        if i > 0 {
            println!();
        }

        let conv = conv_map.get(session_id);

        // Session header: short ID · agent · turns · duration
        let short_session = &session_id[..8.min(session_id.len())];
        let agent = conv
            .map(|c| c.agent_type.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let turns = conv
            .map(|c| c.turn_count.to_string())
            .unwrap_or_else(|| "-".to_string());
        let duration = conv
            .map(|c| format_duration_between(&c.started_at, &c.last_activity))
            .unwrap_or_else(|| "-".to_string());

        println!(
            "{} \u{00b7} {} \u{00b7} {} turns \u{00b7} {}",
            short_session, agent, turns, duration
        );

        // Find tasks associated with this session
        let mut session_tasks: Vec<&tasks::types::Task> = task_graph
            .tasks
            .values()
            .filter(|t| t.last_session_id.as_deref() == Some(session_id))
            .collect();

        // Sort by started_at (earliest first), then by name
        session_tasks.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.name.cmp(&b.name)));

        if session_tasks.is_empty() {
            println!("  (no tasks)");
        } else {
            for task in &session_tasks {
                let icon = match task.status {
                    TaskStatus::Closed => {
                        match task.closed_outcome {
                            Some(tasks::types::TaskOutcome::WontDo) => "\u{2298}", // ⊘
                            _ => "\u{2714}",                                        // ✔
                        }
                    }
                    TaskStatus::Stopped => "\u{2718}",    // ✘
                    TaskStatus::InProgress => "\u{25b8}", // ▸
                    _ => "\u{25cb}",                       // ○
                };

                let confidence_str = task
                    .confidence
                    .map(|c| format!(" (confidence: {})", c))
                    .unwrap_or_default();

                println!(
                    "  {} {}  {}{}",
                    icon,
                    short_id(&task.id),
                    task.name,
                    confidence_str
                );

                // Show summary indented under the task
                if let Some(summary) = task.effective_summary() {
                    // Indent to align with task name (icon + space + short_id + 2 spaces)
                    let indent_len = 2 + 1 + short_id(&task.id).len() + 2 + 1;
                    let indent: String = " ".repeat(indent_len);
                    for line in summary.lines() {
                        println!("{}{}", indent, line);
                    }
                }
            }
        }
    }
}

fn run_list(
    active: bool,
    background: bool,
    interactive: bool,
    number: Option<usize>,
    agent: AgentFilterFlags,
) -> Result<()> {
    agent.validate()?;
    let agent_types = agent.agent_types();
    session::prune_dead_pid_sessions();
    let mut active_sessions = session::list_all_sessions()?;

    // Apply mode filter if specified
    if background {
        active_sessions.retain(|s| s.mode == SessionMode::Background);
    } else if interactive {
        active_sessions.retain(|s| s.mode == SessionMode::Interactive);
    }

    // Apply agent filter before truncation so -n returns the right count
    if !agent_types.is_empty() {
        active_sessions.retain(|s| {
            agent_types.contains(&AgentType::from_str(&s.agent).unwrap_or(AgentType::Unknown))
        });
    }

    if active {
        // --active: show only active sessions, enriched with history data
        if active_sessions.is_empty() {
            println!("No sessions found");
            return Ok(());
        }

        // Load conversation history to get turn counts and timestamps
        let aiki_dir = global::global_aiki_dir();
        let conversations = history::storage::list_conversations(&aiki_dir, None)?;
        let conv_map: HashMap<&str, &history::types::ConversationSummary> = conversations
            .iter()
            .map(|c| (c.session_id.as_str(), c))
            .collect();

        let iter = active_sessions.iter();
        let rows: Vec<SessionRow> = match number {
            Some(n) => iter.take(n).collect::<Vec<_>>(),
            None => iter.collect(),
        }
        .into_iter()
        .map(|s| {
            let agent_type = AgentType::from_str(&s.agent).unwrap_or(AgentType::Unknown);
            let conv = conv_map.get(s.session_id.as_str());

            let turns = conv
                .map(|c| c.turn_count.to_string())
                .unwrap_or_else(|| "-".to_string());

            let started = if let Some(c) = conv {
                format_timestamp(&c.started_at)
            } else if let Some(dot_pos) = s.started_at.find('.') {
                s.started_at[..dot_pos].to_string()
            } else if let Some(plus_pos) = s.started_at.find('+') {
                s.started_at[..plus_pos].to_string()
            } else {
                s.started_at.clone()
            };

            let last_activity = conv
                .map(|c| format_timestamp(&c.last_activity))
                .unwrap_or_else(|| "-".to_string());

            SessionRow {
                session_id: s.session_id.clone(),
                agent_type,
                mode: s.mode,
                pid: s.parent_pid,
                turns,
                started,
                last_activity,
                status: "active",
            }
        })
        .collect();

        print_session_table(&rows);
    } else {
        // Default: show all sessions from JJ history
        let aiki_dir = global::global_aiki_dir();
        let conversations = history::storage::list_conversations(&aiki_dir, None)?;

        if conversations.is_empty() {
            println!("No sessions found");
            return Ok(());
        }

        // Build a lookup of active sessions by session_id
        let active_map: HashMap<String, &session::SessionInfo> = active_sessions
            .iter()
            .map(|s| (s.session_id.clone(), s))
            .collect();

        let mut rows: Vec<SessionRow> = conversations
            .iter()
            .map(|conv| {
                let active_session = active_map.get(&conv.session_id);

                // Use active session mode if available, otherwise use mode from history,
                // falling back to Interactive for legacy events without session_mode
                let (pid, mode, status) = if let Some(session_info) = active_session {
                    (session_info.parent_pid, session_info.mode, "active")
                } else {
                    (
                        None,
                        conv.session_mode.unwrap_or(SessionMode::Interactive),
                        "ended",
                    )
                };

                SessionRow {
                    session_id: conv.session_id.clone(),
                    agent_type: conv.agent_type,
                    mode,
                    pid,
                    turns: conv.turn_count.to_string(),
                    started: format_timestamp(&conv.started_at),
                    last_activity: format_timestamp(&conv.last_activity),
                    status,
                }
            })
            .collect();

        // Apply mode filter if specified (to both active and ended sessions)
        if background {
            rows.retain(|r| r.mode == SessionMode::Background);
        } else if interactive {
            rows.retain(|r| r.mode == SessionMode::Interactive);
        }

        // Apply agent type filter
        if !agent_types.is_empty() {
            rows.retain(|r| agent_types.contains(&r.agent_type));
        }

        // Apply limit after filtering
        if let Some(n) = number {
            rows.truncate(n);
        }

        print_session_table(&rows);
    }

    Ok(())
}

fn event_session_id(event: &ConversationEvent) -> &str {
    match event {
        ConversationEvent::Prompt { session_id, .. }
        | ConversationEvent::Response { session_id, .. }
        | ConversationEvent::SessionStart { session_id, .. }
        | ConversationEvent::SessionEnd { session_id, .. }
        | ConversationEvent::Autoreply { session_id, .. }
        | ConversationEvent::ModelChanged { session_id, .. } => session_id,
    }
}

fn run_show(id: &str) -> Result<()> {
    let aiki_dir = global::global_aiki_dir();
    let events = history::storage::read_events(&aiki_dir)?;

    // Resolve prefix to full session ID
    let full_id = resolve_session_id(&events, id)?;

    // Filter events for this session
    let matching: Vec<&ConversationEvent> = events
        .iter()
        .filter(|e| event_session_id(e) == full_id)
        .collect();

    // Extract session metadata
    let session_start = matching
        .iter()
        .find(|e| matches!(e, ConversationEvent::SessionStart { .. }));
    let session_end = matching
        .iter()
        .find(|e| matches!(e, ConversationEvent::SessionEnd { .. }));

    // Session ID
    println!("Session: {}", full_id);

    // Agent, mode, status, timing
    if let Some(ConversationEvent::SessionStart {
        timestamp: start_ts,
        agent_type,
        session_mode,
        ..
    }) = session_start
    {
        println!("Agent: {}", agent_type);

        let mode = session_mode.unwrap_or(SessionMode::Interactive);
        println!("Mode: {}", mode.to_string());

        if let Some(ConversationEvent::SessionEnd {
            timestamp: end_ts,
            reason,
            ..
        }) = session_end
        {
            let duration = format_duration_between(start_ts, end_ts);
            println!("Status: ended ({})", reason);
            println!(
                "Started: {} {}",
                start_ts.format("%Y-%m-%d"),
                format_time_only(start_ts)
            );
            println!(
                "Ended: {} {}",
                end_ts.format("%Y-%m-%d"),
                format_time_only(end_ts)
            );
            println!("Duration: {}", duration);
        } else {
            let now = chrono::Utc::now();
            let duration = format_duration_between(start_ts, &now);
            println!("Status: active");
            println!(
                "Started: {} {}",
                start_ts.format("%Y-%m-%d"),
                format_time_only(start_ts)
            );
            println!("Duration: {} (ongoing)", duration);
        }
    }

    // Turn count
    let turn_count = matching
        .iter()
        .filter_map(|e| match e {
            ConversationEvent::Prompt { turn, .. } => Some(*turn),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    println!("Turns: {}", turn_count);

    // Tasks associated with this session
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Ok(task_events) = tasks::storage::read_events(&cwd) {
        let task_graph = tasks::materialize_graph(&task_events);
        let mut session_tasks: Vec<&tasks::types::Task> = task_graph
            .tasks
            .values()
            .filter(|t| t.last_session_id.as_deref() == Some(&full_id))
            .collect();
        session_tasks.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.name.cmp(&b.name)));

        if !session_tasks.is_empty() {
            println!("\nTasks:");
            for task in &session_tasks {
                let icon = match task.status {
                    TaskStatus::Closed => {
                        match task.closed_outcome {
                            Some(tasks::types::TaskOutcome::WontDo) => "\u{2298}", // ⊘
                            _ => "\u{2714}",                                        // ✔
                        }
                    }
                    TaskStatus::Stopped => "\u{2718}",    // ✘
                    TaskStatus::InProgress => "\u{25b8}", // ▸
                    _ => "\u{25cb}",                       // ○
                };

                let confidence_str = task
                    .confidence
                    .map(|c| format!(" (confidence: {})", c))
                    .unwrap_or_default();

                println!(
                    "  {} {}  {}{}",
                    icon,
                    short_id(&task.id),
                    task.name,
                    confidence_str
                );

                if let Some(summary) = task.effective_summary() {
                    let indent_len = 2 + 1 + short_id(&task.id).len() + 2 + 1;
                    let indent: String = " ".repeat(indent_len);
                    for line in summary.lines() {
                        println!("{}{}", indent, line);
                    }
                }
            }
        }
    }

    // Hint
    let display_id = if full_id.len() > 8 {
        &full_id[..8]
    } else {
        &full_id
    };
    println!(
        "\n---\nRun `aiki session transcript {}` to view raw conversation.",
        display_id
    );

    Ok(())
}

/// Resolve the transcript file path for a session.
///
/// Looks up the session's external ID and cwd from conversation events,
/// then derives the agent-specific transcript path.
fn resolve_transcript_path(session_id: &str) -> Result<std::path::PathBuf> {
    let aiki_dir = global::global_aiki_dir();
    let events = history::storage::read_events(&aiki_dir)?;

    // Resolve prefix to full session ID
    let full_id = resolve_session_id(&events, session_id)?;

    // Find agent type and cwd from session start event
    let start_event = events.iter().find(|e| {
        matches!(
            e,
            ConversationEvent::SessionStart { session_id: sid, .. } if sid == &full_id
        )
    });

    let (agent_type, cwd, transcript_path) = match start_event {
        Some(ConversationEvent::SessionStart {
            agent_type,
            cwd,
            transcript_path,
            ..
        }) => (*agent_type, cwd.clone(), transcript_path.clone()),
        _ => {
            return Err(AikiError::InvalidArgument(format!(
                "No SessionStart event found for session '{}'",
                session_id
            )));
        }
    };

    // Prefer transcript_path from history (works for any agent type)
    if let Some(path) = transcript_path {
        return Ok(std::path::PathBuf::from(path));
    }

    // Fallback: derive from session file (legacy ClaudeCode sessions only)
    let cwd = cwd.ok_or_else(|| {
        AikiError::InvalidArgument(format!(
            "Session '{}' has no cwd recorded",
            session_id
        ))
    })?;

    match agent_type {
        AgentType::ClaudeCode => {
            let sessions_dir = global::global_sessions_dir();
            let session_file = sessions_dir.join(&full_id);
            let external_id = if session_file.exists() {
                read_external_session_id(&session_file)?
            } else {
                find_external_session_id_from_files(&full_id)?
            };

            let external_id = external_id.ok_or_else(|| {
                AikiError::InvalidArgument(format!(
                    "Could not find external session ID for session '{}'",
                    session_id
                ))
            })?;

            let home = dirs::home_dir().ok_or_else(|| {
                AikiError::Other(anyhow::anyhow!("Could not determine home directory"))
            })?;
            let cwd_with_dashes = cwd.replace('/', "-");
            let path = home
                .join(".claude")
                .join("projects")
                .join(&cwd_with_dashes)
                .join(format!("{}.jsonl", external_id));
            Ok(path)
        }
        _ => Err(AikiError::InvalidArgument(format!(
            "Transcript path resolution not supported for agent type '{}'",
            agent_type
        ))),
    }
}

/// Read external_session_id from a session file.
fn read_external_session_id(path: &Path) -> Result<Option<String>> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        AikiError::Other(anyhow::anyhow!("Failed to read session file: {}", e))
    })?;
    Ok(content
        .lines()
        .find(|line| line.trim().starts_with("external_session_id="))
        .and_then(|line| line.trim().strip_prefix("external_session_id="))
        .map(|v| v.to_string()))
}

/// Scan session files to find external_session_id matching the given aiki session ID.
fn find_external_session_id_from_files(aiki_session_id: &str) -> Result<Option<String>> {
    let sessions_dir = global::global_sessions_dir();
    if !sessions_dir.exists() {
        return Ok(None);
    }

    let entries = std::fs::read_dir(&sessions_dir).map_err(|e| {
        AikiError::Other(anyhow::anyhow!("Failed to read sessions directory: {}", e))
    })?;

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || path.extension().is_some() {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut ext_id = None;
        let mut sid = None;
        for line in content.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("external_session_id=") {
                ext_id = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("session_id=") {
                sid = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("aiki_session_id=") {
                if sid.is_none() {
                    sid = Some(val.to_string());
                }
            }
        }

        if sid.as_deref() == Some(aiki_session_id) {
            return Ok(ext_id);
        }
    }

    Ok(None)
}

fn run_transcript(id: &str, output: Option<TranscriptOutput>) -> Result<()> {
    let transcript_path = resolve_transcript_path(id)?;

    if !transcript_path.exists() {
        eprintln!(
            "Error: transcript file not found: {}",
            transcript_path.display()
        );
        std::process::exit(1);
    }

    match output {
        Some(TranscriptOutput::Path) => {
            println!("{}", transcript_path.display());
        }
        None => {
            let content = std::fs::read_to_string(&transcript_path).map_err(|e| {
                AikiError::Other(anyhow::anyhow!(
                    "Failed to read transcript file: {}",
                    e
                ))
            })?;
            print!("{}", content);
        }
    }

    Ok(())
}

const WAIT_INITIAL_DELAY_MS: u64 = 500;
const WAIT_MAX_DELAY_MS: u64 = 5000;
const WAIT_BACKOFF_MULTIPLIER: u64 = 2;
const WAIT_ABSORPTION_TIMEOUT_SECS: u64 = 60;

/// Resolve a session ID prefix to the full session ID from conversation events.
/// Returns an error if the prefix is ambiguous or not found.
fn resolve_session_id(events: &[ConversationEvent], prefix: &str) -> Result<String> {
    use std::collections::HashSet;

    let matching: HashSet<&str> = events
        .iter()
        .map(|e| event_session_id(e))
        .filter(|sid| sid.starts_with(prefix))
        .collect();

    match matching.len() {
        0 => Err(AikiError::InvalidArgument(format!(
            "No session found matching '{}'",
            prefix
        ))),
        1 => Ok(matching.into_iter().next().unwrap().to_string()),
        _ => {
            let mut ids: Vec<&str> = matching.into_iter().collect();
            ids.sort();
            Err(AikiError::InvalidArgument(format!(
                "Ambiguous session ID '{}', matches: {}",
                prefix,
                ids.join(", ")
            )))
        }
    }
}

/// Check if a session has ended by looking for a SessionEnd event.
fn has_session_ended(events: &[ConversationEvent], session_id: &str) -> bool {
    events.iter().any(
        |e| matches!(e, ConversationEvent::SessionEnd { session_id: sid, .. } if sid == session_id),
    )
}

fn run_wait(ids: Vec<String>, any: bool, output_format: Option<super::OutputFormat>) -> Result<()> {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    let aiki_dir = global::global_aiki_dir();

    if ids.is_empty() {
        return Err(AikiError::InvalidArgument(
            "At least one session ID is required".to_string(),
        ));
    }

    // Resolve all session IDs up front (prefix -> full)
    let events = history::storage::read_events(&aiki_dir)?;
    let mut resolved_ids = Vec::new();
    for id in &ids {
        resolved_ids.push(resolve_session_id(&events, id)?);
    }
    let ids = resolved_ids;

    let mut delay_ms = WAIT_INITIAL_DELAY_MS;

    // Poll until condition is met
    loop {
        let events = history::storage::read_events(&aiki_dir)?;

        let done = if any {
            ids.iter().any(|id| has_session_ended(&events, id))
        } else {
            ids.iter().all(|id| has_session_ended(&events, id))
        };

        if done {
            // Collect which sessions completed
            let completed: HashSet<&str> = ids
                .iter()
                .filter(|id| has_session_ended(&events, id))
                .map(|s| s.as_str())
                .collect();

            // Wait for workspace absorption
            {
                let repo_root =
                    crate::jj::get_repo_root(&std::env::current_dir().map_err(|e| {
                        AikiError::Other(anyhow::anyhow!("Failed to get cwd: {}", e))
                    })?)?;

                let absorption_start = Instant::now();
                let mut absorption_delay_ms = WAIT_INITIAL_DELAY_MS;

                let repo_id = crate::repos::ensure_repo_id(&repo_root)?;

                for session_id in &completed {
                    let workspace_name = format!("aiki-{}", session_id);

                    loop {
                        // Check if workspace still exists
                        let ws_exists = crate::session::isolation::find_workspace_change_id(
                            &repo_root,
                            &workspace_name,
                        )?
                        .is_some();

                        if !ws_exists {
                            break;
                        }

                        // Try to absorb the workspace
                        let workspace = crate::session::isolation::IsolatedWorkspace {
                            name: workspace_name.clone(),
                            path: crate::session::isolation::workspaces_dir()
                                .join(&repo_id)
                                .join(session_id),
                        };
                        match crate::session::isolation::absorb_workspace(
                            &repo_root, &workspace, None,
                        ) {
                            Ok(_) => break,
                            Err(_) => {
                                if absorption_start.elapsed()
                                    > Duration::from_secs(WAIT_ABSORPTION_TIMEOUT_SECS)
                                {
                                    eprintln!(
                                        "Warning: Workspace '{}' not absorbed after {}s. Run `jj workspace list` to check.",
                                        workspace_name, WAIT_ABSORPTION_TIMEOUT_SECS
                                    );
                                    break;
                                }
                                std::thread::sleep(Duration::from_millis(absorption_delay_ms));
                                absorption_delay_ms = (absorption_delay_ms
                                    * WAIT_BACKOFF_MULTIPLIER)
                                    .min(WAIT_MAX_DELAY_MS);
                            }
                        }
                    }
                }
            }

            // Output
            let output_id = matches!(output_format, Some(super::OutputFormat::Id));

            if output_id {
                for id in &ids {
                    if any && !has_session_ended(&events, id) {
                        continue;
                    }
                    println!("{}", id);
                }
            } else {
                // Re-read conversation events for accurate output after absorption
                let events = history::storage::read_events(&aiki_dir)?;
                let conversations = history::storage::list_conversations(&aiki_dir, None)?;
                let conv_map: HashMap<&str, &history::types::ConversationSummary> = conversations
                    .iter()
                    .map(|c| (c.session_id.as_str(), c))
                    .collect();

                // Read task graph to show tasks per session
                let task_graph = {
                    let cwd = std::env::current_dir().map_err(|e| {
                        AikiError::Other(anyhow::anyhow!("Failed to get cwd: {}", e))
                    })?;
                    let task_events = tasks::storage::read_events(&cwd)?;
                    tasks::materialize_graph(&task_events)
                };

                let completed_ids: Vec<&str> = ids
                    .iter()
                    .filter(|id| !any || has_session_ended(&events, id))
                    .map(|s| s.as_str())
                    .collect();

                print_sessions_with_tasks(&completed_ids, &conv_map, &task_graph);
            }

            return Ok(());
        }

        std::thread::sleep(Duration::from_millis(delay_ms));
        delay_ms = (delay_ms * WAIT_BACKOFF_MULTIPLIER).min(WAIT_MAX_DELAY_MS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_external_session_id_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test-session");
        std::fs::write(
            &file,
            "[aiki]\nagent=claude\nexternal_session_id=abc-123-def\nsession_id=deadbeef\n[/aiki]\n",
        )
        .unwrap();

        let result = read_external_session_id(&file).unwrap();
        assert_eq!(result, Some("abc-123-def".to_string()));
    }

    #[test]
    fn test_read_external_session_id_missing_field() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test-session");
        std::fs::write(&file, "[aiki]\nagent=claude\nsession_id=deadbeef\n[/aiki]\n").unwrap();

        let result = read_external_session_id(&file).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_transcript_output_enum_values() {
        assert_eq!(TranscriptOutput::Path, TranscriptOutput::Path);
    }

    #[test]
    fn test_resolve_effective_id_positional() {
        let flags = SessionIdFlags::default();
        let result = resolve_effective_id(Some("abc-123".into()), &flags).unwrap();
        assert_eq!(result, "abc-123");
    }

    #[test]
    fn test_resolve_effective_id_agent_flag() {
        let flags = SessionIdFlags {
            claude: Some("ext-session-1".into()),
            ..Default::default()
        };
        let result = resolve_effective_id(None, &flags).unwrap();
        // Should return a deterministic UUID from agent + external ID
        assert!(!result.is_empty());
    }

    #[test]
    fn test_resolve_effective_id_agent_flag_takes_precedence() {
        let flags = SessionIdFlags {
            claude: Some("ext-session-1".into()),
            ..Default::default()
        };
        // Even if positional is Some, agent flag (checked first) wins
        let result = resolve_effective_id(Some("positional-id".into()), &flags).unwrap();
        assert_ne!(result, "positional-id");
    }

    #[test]
    fn test_resolve_effective_id_neither() {
        let flags = SessionIdFlags::default();
        let result = resolve_effective_id(None, &flags);
        assert!(result.is_err());
    }
}
