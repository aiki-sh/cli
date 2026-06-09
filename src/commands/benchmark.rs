use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

// Fixed configuration - convention over configuration
const WARMUP_ITERATIONS: usize = 3;
const MEASUREMENT_RUNS: usize = 3;

/// Vendor types for benchmarking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Vendor {
    ClaudeCode,
    Cursor,
}

impl Vendor {
    fn name(&self) -> &'static str {
        match self {
            Vendor::ClaudeCode => "claude-code",
            Vendor::Cursor => "cursor",
        }
    }

    fn display_name(&self) -> &'static str {
        match self {
            Vendor::ClaudeCode => "Claude Code",
            Vendor::Cursor => "Cursor",
        }
    }

    fn all() -> &'static [Vendor] {
        &[Vendor::ClaudeCode, Vendor::Cursor]
    }
}

/// Event types tracked in benchmarks
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EventType {
    SessionStarted,
    TurnStarted,
    ChangePermissionAsked,
    ChangeCompleted,
    TurnCompleted,          // WITHOUT autoreply
    TurnCompletedAutoreply, // WITH autoreply (turn.completed only)
    CommitMessageStarted,
}

impl EventType {
    fn name(&self) -> &'static str {
        match self {
            EventType::SessionStarted => "session.started",
            EventType::TurnStarted => "turn.started",
            EventType::ChangePermissionAsked => "change.permission_asked",
            EventType::ChangeCompleted => "change.completed",
            EventType::TurnCompleted => "turn.completed",
            EventType::TurnCompletedAutoreply => "turn.completed+autoreply",
            EventType::CommitMessageStarted => "commit.message_started",
        }
    }
}

/// Event-level timing statistics
#[derive(Debug, Clone, Default, Serialize)]
struct EventStats {
    samples: Vec<f64>, // Duration in seconds
}

impl EventStats {
    fn new() -> Self {
        Self {
            samples: Vec::new(),
        }
    }

    fn add_sample(&mut self, duration: Duration) {
        self.samples.push(duration.as_secs_f64());
    }

    fn count(&self) -> usize {
        self.samples.len()
    }

    fn p50(&self) -> f64 {
        self.percentile(50)
    }

    fn p95(&self) -> f64 {
        self.percentile(95)
    }

    fn max(&self) -> f64 {
        self.samples
            .iter()
            .max_by(|a, b| a.partial_cmp(b).unwrap())
            .copied()
            .unwrap_or(0.0)
    }

    fn percentile(&self, p: usize) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let idx = (p as f64 / 100.0 * (sorted.len() - 1) as f64).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn p50_ms(&self) -> f64 {
        self.p50() * 1000.0
    }

    fn p95_ms(&self) -> f64 {
        self.p95() * 1000.0
    }

    fn max_ms(&self) -> f64 {
        self.max() * 1000.0
    }
}

/// Results for a single vendor
#[derive(Debug, Default)]
struct VendorResults {
    events: HashMap<EventType, EventStats>,
}

impl VendorResults {
    fn new() -> Self {
        Self {
            events: HashMap::new(),
        }
    }

    fn add_sample(&mut self, event: EventType, duration: Duration) {
        self.events
            .entry(event)
            .or_insert_with(EventStats::new)
            .add_sample(duration);
    }

    fn get_stats(&self, event: EventType) -> Option<&EventStats> {
        self.events.get(&event)
    }
}

/// Benchmark results across all vendors
#[derive(Debug, Default)]
struct BenchmarkResults {
    vendors: HashMap<Vendor, VendorResults>,
    shared: VendorResults, // For PrepareCommitMessage (shared across vendors)
    query_blame_ms: f64,
    query_authors_ms: f64,
    total_ms: f64,
}

impl BenchmarkResults {
    fn new() -> Self {
        Self {
            vendors: HashMap::new(),
            shared: VendorResults::new(),
            query_blame_ms: 0.0,
            query_authors_ms: 0.0,
            total_ms: 0.0,
        }
    }

    fn vendor_mut(&mut self, vendor: Vendor) -> &mut VendorResults {
        self.vendors
            .entry(vendor)
            .or_insert_with(VendorResults::new)
    }
}

/// Run an end-to-end performance benchmark
///
/// This benchmarks the complete Aiki workflow:
/// 1. Repository initialization
/// 2. Session lifecycle per vendor (SessionStart, PrePrompt, edits, PostResponse)
/// 3. Query operations (blame, authors)
/// 4. Git integration (PrepareCommitMessage)
///
/// Results are saved to .aiki/benchmarks/aiki-core/YYYY-MM-DD_HH-MM-SS/
pub fn run(_flow_name: String, num_edits: usize) -> Result<()> {
    println!();
    println!("Aiki Benchmark");
    println!("==============");
    println!("Date: {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    println!(
        "Edits: {} x {} runs = {} samples per hot path event",
        num_edits,
        MEASUREMENT_RUNS,
        num_edits * MEASUREMENT_RUNS
    );
    println!();

    let total_start = Instant::now();

    // Create temporary repository
    let tmp_base = std::env::temp_dir().join(format!("aiki-benchmark-{}", std::process::id()));
    fs::create_dir_all(&tmp_base)?;
    let repo_path = tmp_base.join("benchmark-repo");
    fs::create_dir_all(&repo_path)?;

    let aiki_exe = std::env::current_exe()?;

    // Phase 1: Setup
    println!("Phase 1: Setup");
    println!("--------------");
    setup_repository(&repo_path, &aiki_exe, num_edits)?;
    println!();

    // Warmup phase
    println!("Warmup: {} iterations... ", WARMUP_ITERATIONS);
    std::io::stdout().flush()?;
    run_warmup(&repo_path, &aiki_exe, num_edits)?;
    println!("done");
    println!();

    // Phase 2: Measurement runs
    println!("Running benchmarks...");
    let mut results = BenchmarkResults::new();

    for vendor in Vendor::all() {
        print!("  {}: ", vendor.display_name());
        std::io::stdout().flush()?;

        for run_idx in 0..MEASUREMENT_RUNS {
            // Reset repository state for each run
            reset_repository_state(&repo_path, num_edits)?;

            // Run full lifecycle for this vendor
            run_vendor_lifecycle(&repo_path, &aiki_exe, *vendor, num_edits, &mut results)?;

            // Progress indicator
            let progress = (run_idx + 1) * 100 / MEASUREMENT_RUNS;
            print!("\r  {}: ", vendor.display_name());
            print_progress_bar(progress);
            std::io::stdout().flush()?;
        }
        println!();
    }
    println!();

    // Phase 3: Git Integration (PrepareCommitMessage)
    println!("Phase 3: Git Integration");
    println!("------------------------");
    for _ in 0..MEASUREMENT_RUNS {
        let duration = simulate_prepare_commit_msg(&repo_path, &aiki_exe)?;
        results
            .shared
            .add_sample(EventType::CommitMessageStarted, duration);
    }
    if let Some(stats) = results.shared.get_stats(EventType::CommitMessageStarted) {
        println!(
            "  PrepareCommitMessage: {:.0} / {:.0} / {:.0} ms (p50/p95/max)",
            stats.p50_ms(),
            stats.p95_ms(),
            stats.max_ms()
        );
    }
    println!();

    // Phase 4: Query Operations
    println!("Phase 4: Query Operations");
    println!("-------------------------");
    let blame_start = Instant::now();
    let _ = run_command(
        &repo_path,
        aiki_exe.to_str().unwrap(),
        &["blame", "src/file_1.rs"],
    );
    results.query_blame_ms = blame_start.elapsed().as_secs_f64() * 1000.0;

    let authors_start = Instant::now();
    let _ = run_command(&repo_path, aiki_exe.to_str().unwrap(), &["authors"]);
    results.query_authors_ms = authors_start.elapsed().as_secs_f64() * 1000.0;

    println!("  blame:   {:.0}ms", results.query_blame_ms);
    println!("  authors: {:.0}ms", results.query_authors_ms);
    println!();

    results.total_ms = total_start.elapsed().as_secs_f64() * 1000.0;

    // Print results
    print_results(&results, num_edits);

    // Save results
    let benchmark_dir = PathBuf::from(".aiki/benchmarks/aiki-core");
    fs::create_dir_all(&benchmark_dir)?;

    // Load previous metrics for comparison
    let previous_metrics = load_previous_benchmark(&benchmark_dir)?;
    if let Some(ref prev) = previous_metrics {
        println!();
        print_comparison(prev, &results);
    }

    // Save new results
    save_results(&benchmark_dir, &results, num_edits)?;

    // Cleanup
    let _ = fs::remove_dir_all(&tmp_base);

    println!();
    println!("Total: {:.2}s", results.total_ms / 1000.0);
    println!();

    Ok(())
}

fn setup_repository(repo_path: &PathBuf, aiki_exe: &PathBuf, num_edits: usize) -> Result<()> {
    // Git init
    run_command(repo_path, "git", &["init"])?;
    run_command(repo_path, "git", &["config", "user.name", "Benchmark"])?;
    run_command(
        repo_path,
        "git",
        &["config", "user.email", "bench@test.com"],
    )?;
    println!("  Git init: done");

    // Aiki init
    run_command(repo_path, aiki_exe.to_str().unwrap(), &["init", "--quiet"])?;
    println!("  Aiki init: done");

    // Create test files
    let src_dir = repo_path.join("src");
    fs::create_dir_all(&src_dir)?;
    for i in 1..=num_edits {
        let file_path = src_dir.join(format!("file_{}.rs", i));
        fs::write(&file_path, format!("fn function_{}() {{}}\n", i))?;
    }

    // Initial commit
    run_command(repo_path, "git", &["add", "."])?;
    run_command(repo_path, "git", &["commit", "-m", "Initial commit"])?;
    println!("  Initial commit: done");

    // Seed session file for caching behavior
    seed_session_file(repo_path, "benchmark-session-id", "0.0.0-benchmark")?;

    Ok(())
}

fn reset_repository_state(repo_path: &PathBuf, num_edits: usize) -> Result<()> {
    // Reset files to initial state
    let src_dir = repo_path.join("src");
    for i in 1..=num_edits {
        let file_path = src_dir.join(format!("file_{}.rs", i));
        fs::write(&file_path, format!("fn function_{}() {{}}\n", i))?;
    }
    Ok(())
}

fn run_warmup(repo_path: &PathBuf, aiki_exe: &PathBuf, _num_edits: usize) -> Result<()> {
    for _ in 0..WARMUP_ITERATIONS {
        // Run a quick lifecycle for warmup (just first file, Claude Code only)
        let file_path = repo_path.join("src/file_1.rs");

        // Simulate PostToolUse (most common hook)
        let payload = serde_json::json!({
            "session_id": "warmup-session",
            "hook_event_name": "PostToolUse",
            "tool_name": "Edit",
            "tool_input": {
                "file_path": file_path.to_str().unwrap(),
                "cwd": repo_path.to_str().unwrap()
            },
            "cwd": repo_path.to_str().unwrap(),
            "transcript_path": "/dev/null"
        });

        invoke_hook(repo_path, aiki_exe, "claude-code", "PostToolUse", &payload)?;
    }
    Ok(())
}

fn run_vendor_lifecycle(
    repo_path: &PathBuf,
    aiki_exe: &PathBuf,
    vendor: Vendor,
    num_edits: usize,
    results: &mut BenchmarkResults,
) -> Result<()> {
    let vendor_results = results.vendor_mut(vendor);

    // 1. SessionStart (only for vendors that support it)
    if let Some(duration) = simulate_session_start(repo_path, aiki_exe, vendor)? {
        vendor_results.add_sample(EventType::SessionStarted, duration);
    }

    // 2. PrePrompt
    let duration = simulate_pre_prompt(repo_path, aiki_exe, vendor)?;
    vendor_results.add_sample(EventType::TurnStarted, duration);

    // 3. Hot path: PreFileChange + Edit + PostFileChange
    let src_dir = repo_path.join("src");
    for i in 1..=num_edits {
        let file_path = src_dir.join(format!("file_{}.rs", i));

        // PreFileChange (write.permission_asked)
        let duration = simulate_pre_file_change(repo_path, aiki_exe, vendor, &file_path)?;
        vendor_results.add_sample(EventType::ChangePermissionAsked, duration);

        // Actual file edit
        let mut file = fs::OpenOptions::new().append(true).open(&file_path)?;
        writeln!(file, "    println!(\"Edit {}\");", i)?;

        // PostFileChange (write.completed)
        let duration = simulate_post_file_change(repo_path, aiki_exe, vendor, &file_path, i)?;
        vendor_results.add_sample(EventType::ChangeCompleted, duration);
    }

    // 4a. TurnCompleted WITHOUT autoreply
    let duration = simulate_post_response(repo_path, aiki_exe, vendor)?;
    vendor_results.add_sample(EventType::TurnCompleted, duration);

    // 4b. TurnCompleted WITH autoreply (session continues)
    let duration = simulate_post_response_with_autoreply(repo_path, aiki_exe, vendor)?;
    vendor_results.add_sample(EventType::TurnCompletedAutoreply, duration);

    Ok(())
}

fn simulate_session_start(
    repo_path: &PathBuf,
    aiki_exe: &PathBuf,
    vendor: Vendor,
) -> Result<Option<Duration>> {
    match vendor {
        Vendor::ClaudeCode => {
            let start = Instant::now();
            let payload = serde_json::json!({
                "session_id": "benchmark-session-id",
                "hook_event_name": "SessionStart",
                "cwd": repo_path.to_str().unwrap(),
                "transcript_path": "/dev/null"
            });
            invoke_hook(repo_path, aiki_exe, "claude-code", "SessionStart", &payload)?;
            Ok(Some(start.elapsed()))
        }
        Vendor::Cursor => {
            // Cursor doesn't have a dedicated SessionStart hook.
            // beforeSubmitPrompt maps to PrePrompt, not SessionStart.
            // Return None to mark as unsupported.
            Ok(None)
        }
    }
}

fn simulate_pre_prompt(
    repo_path: &PathBuf,
    aiki_exe: &PathBuf,
    vendor: Vendor,
) -> Result<Duration> {
    let start = Instant::now();

    match vendor {
        Vendor::ClaudeCode => {
            let payload = serde_json::json!({
                "session_id": "benchmark-session-id",
                "hook_event_name": "UserPromptSubmit",
                "prompt": "Add error handling to the parse function",
                "cwd": repo_path.to_str().unwrap(),
                "transcript_path": "/dev/null"
            });
            invoke_hook(
                repo_path,
                aiki_exe,
                "claude-code",
                "UserPromptSubmit",
                &payload,
            )?;
        }
        Vendor::Cursor => {
            let payload = serde_json::json!({
                "sessionId": "benchmark-session-id",
                "workingDirectory": repo_path.to_str().unwrap(),
                "eventName": "beforeSubmitPrompt",
                "prompt": "Add error handling to the parse function",
                "conversation_id": "benchmark-conv-id",
                "workspace_roots": [repo_path.to_str().unwrap()]
            });
            invoke_hook(
                repo_path,
                aiki_exe,
                "cursor",
                "beforeSubmitPrompt",
                &payload,
            )?;
        }
    }

    Ok(start.elapsed())
}

fn simulate_pre_file_change(
    repo_path: &PathBuf,
    aiki_exe: &PathBuf,
    vendor: Vendor,
    file_path: &PathBuf,
) -> Result<Duration> {
    let start = Instant::now();

    match vendor {
        Vendor::ClaudeCode => {
            let payload = serde_json::json!({
                "session_id": "benchmark-session-id",
                "hook_event_name": "PreToolUse",
                "tool_name": "Edit",
                "tool_input": {
                    "file_path": file_path.to_str().unwrap(),
                    "command": "str_replace"
                },
                "cwd": repo_path.to_str().unwrap(),
                "transcript_path": "/dev/null"
            });
            invoke_hook(repo_path, aiki_exe, "claude-code", "PreToolUse", &payload)?;
        }
        Vendor::Cursor => {
            let payload = serde_json::json!({
                "sessionId": "benchmark-session-id",
                "workingDirectory": repo_path.to_str().unwrap(),
                "eventName": "beforeShellExecution",
                "toolName": "Edit",  // camelCase to match CursorPayload
                "conversation_id": "benchmark-conv-id",
                "workspace_roots": [repo_path.to_str().unwrap()]
            });
            invoke_hook(
                repo_path,
                aiki_exe,
                "cursor",
                "beforeShellExecution",
                &payload,
            )?;
        }
    }

    Ok(start.elapsed())
}

fn simulate_post_file_change(
    repo_path: &PathBuf,
    aiki_exe: &PathBuf,
    vendor: Vendor,
    file_path: &PathBuf,
    edit_num: usize,
) -> Result<Duration> {
    let start = Instant::now();
    let new_content = format!("    println!(\"Edit {}\");", edit_num);

    match vendor {
        Vendor::ClaudeCode => {
            let payload = serde_json::json!({
                "session_id": "benchmark-session-id",
                "hook_event_name": "PostToolUse",
                "tool_name": "Edit",
                "tool_input": {
                    "file_path": file_path.to_str().unwrap(),
                    "old_string": "",
                    "new_string": new_content,
                    "cwd": repo_path.to_str().unwrap()
                },
                "cwd": repo_path.to_str().unwrap(),
                "transcript_path": "/dev/null"
            });
            invoke_hook(repo_path, aiki_exe, "claude-code", "PostToolUse", &payload)?;
        }
        Vendor::Cursor => {
            let payload = serde_json::json!({
                "sessionId": "benchmark-session-id",
                "workingDirectory": repo_path.to_str().unwrap(),
                "eventName": "afterFileEdit",
                "file_path": file_path.to_str().unwrap(),
                "edits": [{
                    "old_string": "",
                    "new_string": new_content
                }],
                "conversation_id": "benchmark-conv-id",
                "workspace_roots": [repo_path.to_str().unwrap()]
            });
            invoke_hook(repo_path, aiki_exe, "cursor", "afterFileEdit", &payload)?;
        }
    }

    Ok(start.elapsed())
}

fn simulate_post_response(
    repo_path: &PathBuf,
    aiki_exe: &PathBuf,
    vendor: Vendor,
) -> Result<Duration> {
    let start = Instant::now();

    match vendor {
        Vendor::ClaudeCode => {
            let payload = serde_json::json!({
                "session_id": "benchmark-session-id",
                "hook_event_name": "Stop",
                "stop_hook_active": true,
                "cwd": repo_path.to_str().unwrap(),
                "transcript_path": "/dev/null"
            });
            invoke_hook(repo_path, aiki_exe, "claude-code", "Stop", &payload)?;
        }
        Vendor::Cursor => {
            let payload = serde_json::json!({
                "sessionId": "benchmark-session-id",
                "workingDirectory": repo_path.to_str().unwrap(),
                "eventName": "stop",
                "status": "completed",
                "conversation_id": "benchmark-conv-id",
                "workspace_roots": [repo_path.to_str().unwrap()]
            });
            invoke_hook(repo_path, aiki_exe, "cursor", "stop", &payload)?;
        }
    }

    Ok(start.elapsed())
}

/// Simulate TurnCompleted WITH autoreply (session continues)
///
/// This measures the TurnCompleted handler with forced autoreply context.
/// Uses AIKI_BENCHMARK_FORCE_AUTOREPLY env var to force autoreply behavior.
fn simulate_post_response_with_autoreply(
    repo_path: &PathBuf,
    aiki_exe: &PathBuf,
    vendor: Vendor,
) -> Result<Duration> {
    let start = Instant::now();

    match vendor {
        Vendor::ClaudeCode => {
            let payload = serde_json::json!({
                "session_id": "benchmark-session-id",
                "hook_event_name": "Stop",
                "stop_hook_active": true,
                "cwd": repo_path.to_str().unwrap(),
                "transcript_path": "/dev/null"
            });
            invoke_hook_with_env(
                repo_path,
                aiki_exe,
                "claude-code",
                "Stop",
                &payload,
                &[("AIKI_BENCHMARK_FORCE_AUTOREPLY", "1")],
            )?;
        }
        Vendor::Cursor => {
            let payload = serde_json::json!({
                "sessionId": "benchmark-session-id",
                "workingDirectory": repo_path.to_str().unwrap(),
                "eventName": "stop",
                "status": "completed",
                "conversation_id": "benchmark-conv-id",
                "workspace_roots": [repo_path.to_str().unwrap()]
            });
            invoke_hook_with_env(
                repo_path,
                aiki_exe,
                "cursor",
                "stop",
                &payload,
                &[("AIKI_BENCHMARK_FORCE_AUTOREPLY", "1")],
            )?;
        }
    }

    Ok(start.elapsed())
}

fn simulate_prepare_commit_msg(repo_path: &PathBuf, aiki_exe: &PathBuf) -> Result<Duration> {
    // Create COMMIT_EDITMSG file
    let commit_msg_file = repo_path.join(".git/COMMIT_EDITMSG");
    fs::write(&commit_msg_file, "Test commit\n")?;

    let start = Instant::now();

    // Run prepare-commit-msg via aiki event command with env var
    let output = Command::new(aiki_exe.to_str().unwrap())
        .current_dir(repo_path)
        .env("AIKI_COMMIT_MSG_FILE", &commit_msg_file)
        .args(["event", "prepare-commit-msg"])
        .output()?;

    if !output.status.success() {
        return Err(crate::error::AikiError::Other(anyhow::anyhow!(
            "prepare-commit-msg failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(start.elapsed())
}

fn invoke_hook(
    repo_path: &PathBuf,
    aiki_exe: &PathBuf,
    agent: &str,
    event: &str,
    payload: &serde_json::Value,
) -> Result<()> {
    let mut child = Command::new(aiki_exe.to_str().unwrap())
        .current_dir(repo_path)
        .args(["hooks", "handle", "--agent", agent, "--event", event])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(serde_json::to_string(payload).unwrap().as_bytes());
    }

    let _ = child.wait()?;
    Ok(())
}

fn invoke_hook_with_env(
    repo_path: &PathBuf,
    aiki_exe: &PathBuf,
    agent: &str,
    event: &str,
    payload: &serde_json::Value,
    env_vars: &[(&str, &str)],
) -> Result<()> {
    let mut cmd = Command::new(aiki_exe.to_str().unwrap());
    cmd.current_dir(repo_path)
        .args(["hooks", "handle", "--agent", agent, "--event", event])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn()?;

    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(serde_json::to_string(payload).unwrap().as_bytes());
    }

    let _ = child.wait()?;
    Ok(())
}

fn run_command(cwd: &PathBuf, program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program).current_dir(cwd).args(args).output()?;

    if !output.status.success() {
        return Err(crate::error::AikiError::Other(anyhow::anyhow!(
            "Command failed: {} {}\nStderr: {}",
            program,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(())
}

fn seed_session_file(_repo_path: &PathBuf, session_id: &str, version: &str) -> Result<()> {
    use crate::provenance::record::{AgentType, DetectionMethod};
    use crate::session::{AikiSession, SessionMode};

    let session = AikiSession::new(
        AgentType::ClaudeCode,
        session_id,
        Some(version),
        DetectionMethod::Hook,
        SessionMode::Interactive,
    );

    session.file().create()?;
    Ok(())
}

fn print_progress_bar(percent: usize) {
    let filled = percent / 5; // 20 chars total
    let empty = 20 - filled;
    print!("[");
    for _ in 0..filled {
        print!("=");
    }
    for _ in 0..empty {
        print!(" ");
    }
    print!("] {}%", percent);
}

fn print_results(results: &BenchmarkResults, _num_edits: usize) {
    println!("Results by Event (p50 / p95 / max ms):");
    println!("+-----------------------+---------------------+---------------------+");
    println!("| Event                 | Claude Code         | Cursor              |");
    println!("+-----------------------+---------------------+---------------------+");

    let events = [
        EventType::SessionStarted,
        EventType::TurnStarted,
        EventType::ChangePermissionAsked,
        EventType::ChangeCompleted,
        EventType::TurnCompleted,
        EventType::TurnCompletedAutoreply,
    ];

    for event in events {
        let claude_stats = results
            .vendors
            .get(&Vendor::ClaudeCode)
            .and_then(|v| v.get_stats(event));
        let cursor_stats = results
            .vendors
            .get(&Vendor::Cursor)
            .and_then(|v| v.get_stats(event));

        let claude_str = format_stats(claude_stats);
        let cursor_str = format_stats(cursor_stats);

        println!(
            "| {:<21} | {:>19} | {:>19} |",
            event.name(),
            claude_str,
            cursor_str
        );
    }

    // PrepareCommitMessage (shared)
    let pcm_stats = results.shared.get_stats(EventType::CommitMessageStarted);
    let pcm_str = format_stats(pcm_stats);
    println!(
        "| {:<21} | {:>19} | {:>19} |",
        "commit.message_started", pcm_str, "(same)"
    );

    println!("+-----------------------+---------------------+---------------------+");
    println!();

    println!("Query Operations:");
    println!("  blame:   {:.0}ms", results.query_blame_ms);
    println!("  authors: {:.0}ms", results.query_authors_ms);
}

fn format_stats(stats: Option<&EventStats>) -> String {
    match stats {
        Some(s) if s.count() > 0 => {
            format!(
                "{:>4.0} / {:>4.0} / {:>4.0}",
                s.p50_ms(),
                s.p95_ms(),
                s.max_ms()
            )
        }
        _ => "N/A".to_string(), // No samples = unsupported
    }
}

// --- Persistence & Comparison ---

#[derive(Serialize, Deserialize)]
struct MetricsJson {
    version: u32,
    timestamp: String,
    config: MetricsConfig,
    vendors: HashMap<String, HashMap<String, EventMetrics>>,
    shared: HashMap<String, EventMetrics>,
    queries: QueryMetrics,
    total_ms: f64,
}

#[derive(Serialize, Deserialize)]
struct MetricsConfig {
    edits: usize,
    warmup: usize,
    runs: usize,
}

#[derive(Serialize, Deserialize, Clone)]
struct EventMetrics {
    p50: f64,
    p95: f64,
    max: f64,
    samples: usize,
}

#[derive(Serialize, Deserialize)]
struct QueryMetrics {
    blame_ms: f64,
    authors_ms: f64,
}

fn save_results(
    benchmark_dir: &PathBuf,
    results: &BenchmarkResults,
    num_edits: usize,
) -> Result<()> {
    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let results_dir = benchmark_dir.join(timestamp.to_string());
    fs::create_dir_all(&results_dir)?;

    // Build metrics JSON
    let mut vendors_map: HashMap<String, HashMap<String, EventMetrics>> = HashMap::new();

    for vendor in Vendor::all() {
        let mut events_map: HashMap<String, EventMetrics> = HashMap::new();
        if let Some(vendor_results) = results.vendors.get(vendor) {
            for (event, stats) in &vendor_results.events {
                // Skip events with no samples (unsupported by this vendor)
                if stats.count() == 0 {
                    continue;
                }
                events_map.insert(
                    event.name().to_string(),
                    EventMetrics {
                        p50: stats.p50_ms(),
                        p95: stats.p95_ms(),
                        max: stats.max_ms(),
                        samples: stats.count(),
                    },
                );
            }
        }
        vendors_map.insert(vendor.name().to_string(), events_map);
    }

    let mut shared_map: HashMap<String, EventMetrics> = HashMap::new();
    if let Some(stats) = results.shared.get_stats(EventType::CommitMessageStarted) {
        shared_map.insert(
            "commit.message_started".to_string(),
            EventMetrics {
                p50: stats.p50_ms(),
                p95: stats.p95_ms(),
                max: stats.max_ms(),
                samples: stats.count(),
            },
        );
    }

    let metrics = MetricsJson {
        version: 2,
        timestamp: chrono::Local::now().to_rfc3339(),
        config: MetricsConfig {
            edits: num_edits,
            warmup: WARMUP_ITERATIONS,
            runs: MEASUREMENT_RUNS,
        },
        vendors: vendors_map,
        shared: shared_map,
        queries: QueryMetrics {
            blame_ms: results.query_blame_ms,
            authors_ms: results.query_authors_ms,
        },
        total_ms: results.total_ms,
    };

    let metrics_json = serde_json::to_string_pretty(&metrics).map_err(|e| {
        crate::error::AikiError::Other(anyhow::anyhow!("Failed to serialize metrics: {}", e))
    })?;
    fs::write(results_dir.join("metrics.json"), metrics_json)?;

    // Write human-readable results.txt
    let mut results_file = fs::File::create(results_dir.join("results.txt"))?;
    writeln!(results_file, "Aiki Benchmark Results")?;
    writeln!(results_file, "======================")?;
    writeln!(results_file, "Date: {}", chrono::Local::now())?;
    writeln!(results_file, "Edits: {}", num_edits)?;
    writeln!(results_file, "Warmup: {}", WARMUP_ITERATIONS)?;
    writeln!(results_file, "Runs: {}", MEASUREMENT_RUNS)?;
    writeln!(results_file)?;
    writeln!(results_file, "Total: {:.2}s", results.total_ms / 1000.0)?;

    // Create latest symlink
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let latest_link = benchmark_dir.join("latest");
        let _ = fs::remove_file(&latest_link);
        symlink(timestamp.to_string(), latest_link)?;
    }

    println!("Results saved to:");
    println!("  {}/results.txt", results_dir.display());
    println!("  {}/metrics.json", results_dir.display());

    Ok(())
}

fn load_previous_benchmark(benchmark_dir: &PathBuf) -> Result<Option<MetricsJson>> {
    let latest_link = benchmark_dir.join("latest");

    if !latest_link.exists() {
        return Ok(None);
    }

    let metrics_file = latest_link.join("metrics.json");
    if !metrics_file.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&metrics_file)?;
    let metrics: MetricsJson = serde_json::from_str(&content).map_err(|e| {
        crate::error::AikiError::Other(anyhow::anyhow!("Failed to parse previous metrics: {}", e))
    })?;

    Ok(Some(metrics))
}

fn print_comparison(prev: &MetricsJson, current: &BenchmarkResults) {
    println!("vs Previous Run:");

    // Compare key events (hot path change.completed and turn.started)
    let events_to_compare = [
        ("change.completed", EventType::ChangeCompleted),
        ("turn.started", EventType::TurnStarted),
    ];

    for (name, event_type) in events_to_compare {
        // Compare Claude Code
        if let Some(prev_vendor) = prev.vendors.get("claude-code") {
            if let Some(prev_event) = prev_vendor.get(name) {
                if let Some(curr_stats) = current
                    .vendors
                    .get(&Vendor::ClaudeCode)
                    .and_then(|v| v.get_stats(event_type))
                {
                    let diff = curr_stats.p50_ms() - prev_event.p50;
                    let pct = (diff / prev_event.p50) * 100.0;
                    let indicator = if diff > 0.0 { "+" } else { "" };
                    let emoji = if diff > 0.0 { "🔴" } else { "🟢" };
                    println!(
                        "  {} p50: {:.0}ms → {:.0}ms {} {}{:.1}%",
                        name,
                        prev_event.p50,
                        curr_stats.p50_ms(),
                        emoji,
                        indicator,
                        pct
                    );
                }
            }
        }
    }

    // Overall comparison
    let diff = current.total_ms - prev.total_ms;
    let pct = (diff / prev.total_ms) * 100.0;
    let indicator = if diff > 0.0 { "+" } else { "" };
    let emoji = if diff > 0.0 { "🔴" } else { "🟢" };
    println!(
        "  Overall: {:.2}s → {:.2}s {} {}{:.1}%",
        prev.total_ms / 1000.0,
        current.total_ms / 1000.0,
        emoji,
        indicator,
        pct
    );
}
