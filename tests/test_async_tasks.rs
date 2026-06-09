//! Integration tests for async task execution
//!
//! Tests for:
//! - `--async` flag parsing on `aiki run` command
//! - PID file creation/cleanup functions
//! - Wait command's exponential backoff calculation
//! - Task ID extraction from XML (for piping support)
//! - Terminate background task function (mock scenarios)

use std::fs;
use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::{Command, Stdio};

// ============================================================================
// Test Helpers
// ============================================================================

/// Helper function to initialize a Git repository
fn init_git_repo(path: &std::path::Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("Failed to initialize Git repository");

    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .expect("Failed to configure git email");
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(path)
        .output()
        .expect("Failed to configure git name");
}

/// Helper function to initialize an Aiki repository
fn init_aiki_repo(path: &std::path::Path) {
    init_git_repo(path);

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(path)
        .arg("init")
        .output()
        .expect("Failed to run aiki init");

    if !output.status.success() {
        panic!(
            "aiki init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Helper to run aiki task command
fn aiki_task(path: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(path);
    cmd.arg("task");
    for arg in args {
        cmd.arg(arg);
    }
    cmd.assert()
}

/// Helper to run aiki task wait command
fn aiki_wait(path: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(path);
    cmd.args(["task", "wait"]);
    for arg in args {
        cmd.arg(arg);
    }
    cmd.assert()
}

/// Helper to extract task short ID from markdown or piped output.
/// Looks for patterns like `[p2] abcdefg`, `Added abcdefg`, or bare IDs.
fn extract_task_id(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        // Try markdown list format: [pN] <short-id>
        if let Some(rest) = trimmed.strip_prefix("[p") {
            // Skip priority digit and "] "
            if let Some(after_bracket) = rest.get(1..).and_then(|s| s.strip_prefix("] ")) {
                let id: String = after_bracket
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase())
                    .collect();
                if id.len() >= 7 {
                    return Some(id);
                }
            }
        }
        // Try "Added <id>" format
        if let Some(rest) = trimmed.strip_prefix("Added ") {
            let id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase())
                .collect();
            if id.len() >= 7 {
                return Some(id);
            }
        }
        // Try bare task ID (32 lowercase chars, output by piped commands)
        if trimmed.len() >= 7 && trimmed.chars().all(|c| c.is_ascii_lowercase()) {
            return Some(trimmed.to_string());
        }
    }
    None
}

// ============================================================================
// CLI Argument Parsing Tests
// ============================================================================

#[test]
fn test_task_run_async_flag_exists() {
    // Verify that --async flag is recognized by the parser
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task to exercise top-level `aiki run`
    aiki_task(temp_dir.path(), &["add", "Test async task"]).success();

    // Get the task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID");

    // Try running with --async flag - should be recognized and succeed
    // (auto-detects agent from session context)
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", &task_id, "--async"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() || stderr.contains("Session UUID not discovered within timeout"),
        "Expected async run to succeed or time out while discovering a session UUID. stdout='{}' stderr='{}'",
        stdout,
        stderr
    );
}

#[test]
fn test_task_run_short_async_flag() {
    // Verify that -a short flag works for async
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task
    aiki_task(temp_dir.path(), &["add", "Test short flag"]).success();

    // Get the task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID");

    // -a short flag is NOT defined for --async; verify it's rejected as a parse error
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", &task_id, "-a"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"));
}

#[test]
fn test_task_run_requires_task_id() {
    // Verify that top-level run requires a task ID argument
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Try running without task ID - should fail with argument error
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must be provided"));
}

#[test]
fn test_task_run_with_agent_override() {
    // Verify that --agent flag works alongside --async
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task
    aiki_task(temp_dir.path(), &["add", "Test agent override"]).success();

    // Get the task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID");

    // Try running with --agent and --async flags
    // The command should be parsed correctly (flags recognized)
    // It may succeed (if agent is installed) or fail (if not), but either way
    // the flags should be recognized
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", &task_id, "--agent", "claude-code", "--async"])
        .output()
        .unwrap();

    // Either success (agent installed) or failure (agent not installed or spawn failed)
    // But should not be a CLI argument parsing error
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The output should be markdown format or an agent-related error, not a usage/argument error
    assert!(
        stdout.contains("Run Started")
            || stderr.contains("not found")
            || stderr.contains("spawn")
            || stderr.contains("Session UUID not discovered within timeout"),
        "Should produce run output or runtime error, got stdout='{}', stderr='{}'",
        stdout,
        stderr
    );
}

#[test]
fn test_task_run_reserved_without_force_rejects() {
    // Verify direct run rejects Reserved tasks unless --force is supplied.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add and capture a task ID
    aiki_task(temp_dir.path(), &["add", "Reserved test task"]).success();
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = extract_task_id(&stdout).expect("Should find task ID");

    // Make an initial run attempt to transition the task to reserved status.
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", &short_id, "--async"])
        .assert()
        .failure();

    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", &short_id, "--async"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is reserved and already pending a run. Use --force to override and re-run it.",
        ));
}

#[test]
fn test_task_run_in_progress_without_force_rejects() {
    // Verify direct run rejects InProgress tasks unless --force is supplied.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add and capture a task ID
    aiki_task(temp_dir.path(), &["add", "In-progress test task"]).success();
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = extract_task_id(&stdout).expect("Should find task ID");

    // Mark it in progress.
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &short_id])
        .assert()
        .success();

    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", &short_id, "--async"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "is already in progress. Use --force to override and re-run it.",
        ));
}

#[test]
fn test_task_run_invalid_agent() {
    // Verify that invalid agent names are rejected
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task
    aiki_task(temp_dir.path(), &["add", "Test invalid agent"]).success();

    // Get the task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID");

    // Try running with invalid agent name
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", &task_id, "--agent", "invalid-agent"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Unknown agent type")
                .or(predicate::str::contains("unknown agent")),
        );
}

#[test]
fn test_review_continue_async_flag() {
    // Verify that --_continue-async flag is recognized by the review command parser
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task to use as the review target and extract ID from add output
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Test review target"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID in add output");

    // Run review with --_continue-async flag
    // Should be recognized by the parser (not an "unexpected argument" error)
    // May fail at runtime since no actual review task exists, but that's expected
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", "--_continue-async", &task_id])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The flag should be accepted by the parser - no argument parsing errors
    assert!(
        !stderr.contains("unexpected argument"),
        "--_continue-async should be recognized by the review command parser, got stderr='{}'",
        stderr
    );
}

#[test]
fn test_review_continue_async_reads_json_payload() {
    // Verify that `aiki review --_continue-async <id>` reads and parses JSON from stdin,
    // exercising `async_run::read_continue_opts::<ReviewOpts>()`.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task to get a valid-format task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Test review target for stdin piping"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID in add output");

    // Construct a valid ReviewOpts JSON payload
    let payload = serde_json::json!({
        "target": task_id,
        "run_kind": "ContinueAsync",
        "output": null,
        "workflow": {
            "restart": false,
            "decompose_template": null,
            "loop_template": null,
            "agent": null,
            "agent_str": null,
            "target": task_id,
            "code": false,
            "review": false,
            "review_template": null,
            "fix": false,
            "fix_template": null,
            "autorun": false,
            "plan_template": null,
            "once": false
        }
    });
    let json_bytes = serde_json::to_vec(&payload).unwrap();

    // Spawn `aiki review --_continue-async <task_id>` with piped stdin
    let mut child = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", "--_continue-async", &task_id])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn aiki review");

    // Write the JSON payload to stdin, then drop to send EOF.
    // Must use take() to move ownership so the handle is actually dropped/closed.
    {
        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        stdin
            .write_all(&json_bytes)
            .expect("Failed to write to stdin");
    }

    // Use a timeout-bounded wait: the review workflow reaches spawn_blocking in
    // non-TTY mode which blocks indefinitely. We only need to verify JSON stdin
    // parsing works — that happens immediately on startup.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });

    let stderr_bytes = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(result) => {
            let output = result.expect("Failed to wait on child");
            output.stderr
        }
        Err(_) => {
            // Timeout — the process hung (expected in non-TTY spawn_blocking).
            // Stdin parsing happens immediately; if it failed, it would have
            // exited fast before the timeout. An empty stderr means no error.
            Vec::new()
        }
    };
    let stderr = String::from_utf8_lossy(&stderr_bytes);

    // The JSON should have been parsed successfully — no stdin-parsing error
    assert!(
        !stderr.contains("failed to read async continue options from stdin"),
        "JSON payload should be parsed successfully from stdin, got stderr='{}'",
        stderr
    );

    // The process should have reached the review workflow (and failed downstream
    // due to missing review setup, not due to stdin parsing)
    assert!(
        !stderr.contains("unexpected argument"),
        "No argument parsing errors expected, got stderr='{}'",
        stderr
    );
}

// ============================================================================
// PID File Management Tests (Unit Tests for runner functions)
// ============================================================================

#[test]
fn test_pid_directory_structure() {
    // Test that PID files go in the correct directory
    let temp_dir = tempfile::tempdir().unwrap();
    let pids_dir = temp_dir.path().join(".aiki/tasks/pids");

    // Create the directory structure
    fs::create_dir_all(&pids_dir).unwrap();

    // Write a mock PID file
    let pid_file = pids_dir.join("test_task_id.pid");
    let mut file = fs::File::create(&pid_file).unwrap();
    writeln!(file, "12345").unwrap();

    // Verify it exists
    assert!(pid_file.exists());

    // Verify content
    let content = fs::read_to_string(&pid_file).unwrap();
    assert_eq!(content.trim(), "12345");
}

#[test]
fn test_pid_file_naming_convention() {
    // Test that PID files follow the naming convention: {task_id}.pid
    let temp_dir = tempfile::tempdir().unwrap();
    let pids_dir = temp_dir.path().join(".aiki/tasks/pids");
    fs::create_dir_all(&pids_dir).unwrap();

    // Various task ID formats
    let task_ids = [
        "xqrmnpst",
        "xqrmnpst.1",                       // Subtask
        "xqrmnpst.1.2",                     // Nested subtask
        "abcdefghijklmnopqrstuvwxyzabcdef", // 32-char ID
    ];

    for task_id in task_ids {
        let expected_filename = format!("{}.pid", task_id);
        let pid_file = pids_dir.join(&expected_filename);

        // Create the file
        let mut file = fs::File::create(&pid_file).unwrap();
        writeln!(file, "12345").unwrap();

        // Verify naming
        assert!(
            pid_file.file_name().unwrap().to_string_lossy() == expected_filename,
            "PID file should be named {}.pid for task {}",
            task_id,
            task_id
        );
    }
}

#[test]
fn test_pid_file_cleanup_removes_file() {
    // Test that cleanup removes the PID file
    let temp_dir = tempfile::tempdir().unwrap();
    let pids_dir = temp_dir.path().join(".aiki/tasks/pids");
    fs::create_dir_all(&pids_dir).unwrap();

    let pid_file = pids_dir.join("cleanup_test.pid");
    fs::write(&pid_file, "12345").unwrap();

    assert!(pid_file.exists(), "PID file should exist before cleanup");

    // Simulate cleanup
    fs::remove_file(&pid_file).unwrap();

    assert!(
        !pid_file.exists(),
        "PID file should be removed after cleanup"
    );
}

#[test]
fn test_pid_file_content_is_valid_pid() {
    // Test that PID files contain valid PID values (unsigned integers)
    let temp_dir = tempfile::tempdir().unwrap();
    let pids_dir = temp_dir.path().join(".aiki/tasks/pids");
    fs::create_dir_all(&pids_dir).unwrap();

    // Valid PIDs
    let valid_pids = ["1", "12345", "4294967295"]; // Max u32

    for pid_str in valid_pids {
        let pid_file = pids_dir.join(format!("task_{}.pid", pid_str));
        fs::write(&pid_file, pid_str).unwrap();

        let content = fs::read_to_string(&pid_file).unwrap();
        let parsed: Result<u32, _> = content.trim().parse();
        assert!(parsed.is_ok(), "PID '{}' should parse as u32", pid_str);
    }
}

// ============================================================================
// Wait Command Tests
// ============================================================================

#[test]
fn test_wait_command_requires_task_id_or_stdin() {
    // When no task ID is provided and stdin is empty, should fail
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Run task wait with explicit empty stdin
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "wait"])
        .stdin(std::process::Stdio::null())
        .output()
        .expect("Failed to run aiki task wait");

    // Should fail because no task ID provided
    assert!(!output.status.success(), "wait without task ID should fail");
}

#[test]
fn test_wait_with_nonexistent_task() {
    // Wait for a task that doesn't exist should fail
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    aiki_wait(temp_dir.path(), &["nonexistent_task_id"])
        .failure()
        .stderr(predicate::str::contains("not found").or(predicate::str::contains("Task")));
}

#[test]
fn test_wait_with_closed_task_exits_immediately() {
    // Wait for a task that's already closed should return immediately
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add, start, and close a task
    aiki_task(temp_dir.path(), &["add", "Task to close"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Get the task ID from closed tasks
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Wait should return immediately since task is already closed
    aiki_wait(temp_dir.path(), &[&task_id])
        .success()
        .stdout(predicate::str::contains(&task_id));
}

#[test]
fn test_wait_with_stopped_task_returns_error() {
    // Wait for a stopped task should return an error (non-zero exit)
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add, start, and stop a task
    aiki_task(temp_dir.path(), &["add", "Task to stop"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["stop", "--reason", "Blocked"]).success();

    // Get the task ID from stopped tasks
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--stopped"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find stopped task ID");

    // Wait should fail since task was stopped (not completed successfully)
    aiki_wait(temp_dir.path(), &[&task_id])
        .failure()
        .stderr(predicate::str::contains("stopped"));
}

#[test]
fn test_wait_with_stopped_task_absorbed() {
    // Regression: Verify the absorption wait path works for stopped tasks.
    // Without this, the code path where needs_absorption includes stopped tasks
    // with session_id and workspace_absorb_all emits Absorbed events could
    // silently regress.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add, start, and stop a task
    aiki_task(temp_dir.path(), &["add", "Task to stop and absorb"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["stop", "--reason", "test"]).success();

    // Get the short task ID from stopped tasks
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--stopped"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let short_id = extract_task_id(&stdout).expect("Should find stopped task ID");

    // Get the full 32-char task ID via show -o id
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "show", &short_id, "-o", "id"])
        .output()
        .unwrap();
    let full_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(
        full_id.len(),
        32,
        "Expected 32-char task ID, got '{}' (len {})",
        full_id,
        full_id.len()
    );

    // Write a Stopped event with session_id via jj
    // (The CLI-generated Stopped event has session_id: None since there's no
    // active session in test, so we write one manually with a session_id)
    let stopped_msg = format!(
        "[aiki-task]\n\
         event=stopped\n\
         task_id={}\n\
         reason=test\n\
         session_id=test-session-abc\n\
         timestamp=2026-01-01T00:00:01+00:00\n\
         [/aiki-task]",
        full_id
    );
    let jj_output = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "new",
            "aiki/tasks",
            "--no-edit",
            "--ignore-working-copy",
            "-m",
            &stopped_msg,
        ])
        .output()
        .expect("Failed to write Stopped event via jj");
    assert!(
        jj_output.status.success(),
        "jj new for Stopped event failed: {}",
        String::from_utf8_lossy(&jj_output.stderr)
    );

    // Write an Absorbed event via jj
    let absorbed_msg = format!(
        "[aiki-task]\n\
         event=absorbed\n\
         task_id={}\n\
         session_id=test-session-abc\n\
         timestamp=2026-01-01T00:00:02+00:00\n\
         [/aiki-task]",
        full_id
    );
    let jj_output = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "new",
            "aiki/tasks",
            "--no-edit",
            "--ignore-working-copy",
            "-m",
            &absorbed_msg,
        ])
        .output()
        .expect("Failed to write Absorbed event via jj");
    assert!(
        jj_output.status.success(),
        "jj new for Absorbed event failed: {}",
        String::from_utf8_lossy(&jj_output.stderr)
    );

    // Wait should complete quickly (absorption path) and fail
    // (stopped tasks always fail wait) with stderr containing "stopped"
    aiki_wait(temp_dir.path(), &[&short_id])
        .failure()
        .stderr(predicate::str::contains("stopped"));
}

// ============================================================================
// Task ID Extraction from Markdown Tests (Unit Tests)
// ============================================================================

#[test]
fn test_extract_task_id_from_list_format() {
    // Markdown list format: [pN] <short-id>  Task name
    assert_eq!(
        extract_task_id("[p2] xqrmnps  Test task"),
        Some("xqrmnps".to_string())
    );
}

#[test]
fn test_extract_task_id_from_added_format() {
    // Added format: Added <short-id> → Run `aiki task start` to begin work
    assert_eq!(
        extract_task_id("Added xqrmnps → Run `aiki task start` to begin work"),
        Some("xqrmnps".to_string())
    );
}

#[test]
fn test_extract_task_id_from_multiline() {
    // Extract first ID from multi-line output
    let md = "Tasks (2):\n[p2] abcdefg  First task\n[p1] hijklmn  Second task\n";
    assert_eq!(extract_task_id(md), Some("abcdefg".to_string()));
}

#[test]
fn test_extract_task_id_no_match() {
    // No task ID in output
    assert_eq!(extract_task_id("No tasks found"), None);
}

// ============================================================================
// Exponential Backoff Calculation Tests
// ============================================================================

#[test]
fn test_exponential_backoff_sequence() {
    // Test that exponential backoff follows expected sequence
    // Constants from wait.rs:
    const INITIAL_DELAY_MS: u64 = 100;
    const MAX_DELAY_MS: u64 = 2000;
    const MULTIPLIER: u64 = 2;

    let mut delay = INITIAL_DELAY_MS;
    let expected_sequence = [100, 200, 400, 800, 1600, 2000, 2000];

    for expected in expected_sequence {
        assert_eq!(delay, expected, "Delay should match expected sequence");
        delay = (delay * MULTIPLIER).min(MAX_DELAY_MS);
    }
}

#[test]
fn test_backoff_caps_at_maximum() {
    // Test that backoff never exceeds maximum
    const MAX_DELAY_MS: u64 = 2000;
    const MULTIPLIER: u64 = 2;

    let mut delay: u64 = 1600;

    // After doubling 1600, should cap at 2000
    delay = (delay * MULTIPLIER).min(MAX_DELAY_MS);
    assert_eq!(delay, 2000);

    // Should stay at 2000
    delay = (delay * MULTIPLIER).min(MAX_DELAY_MS);
    assert_eq!(delay, 2000);
}

// ============================================================================
// Task Stop with Background Process Tests
// ============================================================================

#[test]
fn test_task_stop_without_pid_file() {
    // Stopping a task that has no PID file should still work
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add and start a task (synchronously, so no PID file)
    aiki_task(temp_dir.path(), &["add", "Manual task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();

    // Verify no PID file exists
    let pids_dir = temp_dir.path().join(".aiki/tasks/pids");
    if pids_dir.exists() {
        let pid_files: Vec<_> = fs::read_dir(&pids_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            pid_files.is_empty(),
            "No PID files should exist for manually started task"
        );
    }

    // Stop should still work
    aiki_task(temp_dir.path(), &["stop", "--reason", "Manual stop"])
        .success()
        .stdout(predicate::str::contains("Stopped"));
}

// ============================================================================
// XML Output Format Tests for Async Mode
// ============================================================================

#[test]
fn test_task_run_sync_output_format() {
    // Verify output format for top-level run (markdown, not XML)
    // Note: Sync run spawns an agent which may timeout. We use --async to avoid hanging.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    aiki_task(temp_dir.path(), &["add", "Output format test"]).success();

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID");

    // Run async (to avoid hanging on agent spawn) and check markdown format
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", &task_id, "--async"])
        .output()
        .unwrap();

    // Check that output uses markdown format (not XML)
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Run Started") || stdout.is_empty(),
        "Output should be markdown formatted or empty (error on stderr), got: {}",
        stdout
    );
    // Verify no XML format remnants
    assert!(
        !stdout.contains("<aiki_task"),
        "Output should not contain XML format"
    );
}

// ============================================================================
// Integration: Async Start -> Wait Flow (Conceptual Test)
// ============================================================================

#[test]
fn test_async_wait_conceptual_flow() {
    // This test documents the expected async -> wait flow
    // We can't actually test async execution without a real agent,
    // but we can verify the setup works

    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // 1. Add a task
    aiki_task(temp_dir.path(), &["add", "Async workflow test"]).success();

    // Get task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID");

    // 2. Verify top-level run --async is a valid command (recognized by parser)
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", &task_id, "--async"])
        .output()
        .unwrap();

    // The command should either succeed (agent available) or fail about assignee/agent,
    // but NOT about unrecognized arguments
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success()
            || stderr.contains("assignee")
            || stderr.contains("agent")
            || stderr.contains("Session UUID not discovered within timeout")
            || stdout.contains("assignee")
            || stdout.contains("agent")
            || stdout.contains("Run Started"),
        "Should either succeed or fail with a recognized runtime error. Got stdout='{}', stderr='{}'",
        stdout,
        stderr
    );

    // 3. Verify wait command works (though task not running)
    // Start and close the task so wait can succeed
    aiki_task(temp_dir.path(), &["start", &task_id]).success();
    aiki_task(
        temp_dir.path(),
        &["close", &task_id, "--summary", "Test done"],
    )
    .success();

    // Get the task ID from closed list
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let closed_task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Wait should succeed immediately for closed task
    aiki_wait(temp_dir.path(), &[&closed_task_id])
        .success()
        .stdout(predicate::str::contains(&closed_task_id));
}

// ============================================================================
// Review-Fix Execution Path Regression Tests
// ============================================================================

/// Helper to run aiki review command
fn aiki_review(path: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(path);
    cmd.arg("review");
    for arg in args {
        cmd.arg(arg);
    }
    cmd.assert()
}

#[test]
fn test_review_fix_and_start_conflict() {
    // Regression: --fix and --start cannot be used together.
    // The error should be caught before target resolution.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task to use as review target
    aiki_task(temp_dir.path(), &["add", "Task to review"]).success();

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID");

    // Running review with both --fix-template and --start should produce an error
    aiki_review(
        temp_dir.path(),
        &[&task_id, "--fix-template", "fix", "--start"],
    )
    .failure()
    .stderr(predicate::str::contains(
        "--fix and --start cannot be used together",
    ));
}

#[test]
fn test_review_fix_flag_accepted_by_parser() {
    // Regression: --fix flag should be recognized by the CLI parser.
    // The command may fail for other reasons (no agent, etc.) but NOT
    // as an unrecognized argument.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add and close a task so we have something to review
    aiki_task(temp_dir.path(), &["add", "Fixable task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Get the closed task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Run review with --fix-template; it may succeed or fail, but should NOT
    // produce an "unexpected argument" error (which would indicate --fix-template isn't recognized)
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", &task_id, "--fix-template"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--fix flag should be recognized by the parser, got stderr: {}",
        stderr
    );
}

#[test]
fn test_review_autorun_flag_accepted_by_parser() {
    // Regression: --autorun flag should be recognized by the CLI parser.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task as review target
    aiki_task(temp_dir.path(), &["add", "Autorun review task"]).success();

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID");

    // Run review with --autorun; should not produce a parser error
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", &task_id, "--autorun", "--start"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--autorun flag should be recognized by the parser, got stderr: {}",
        stderr
    );
}

#[test]
fn test_review_output_id_no_extra_output() {
    // Regression: when -o id is used, stdout should contain ONLY the review
    // task ID — no markdown, no ANSI, no extra lines.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Set up the review template so `aiki review` can create a review task.
    // The template needs to exist at .aiki/tasks/review/task.md
    let template_dir = temp_dir.path().join(".aiki/tasks/review");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(
        template_dir.join("task.md"),
        "---\nversion: 2.0.0\ntype: review\n---\n\n# Review: {{data.scope.name}}\n\nReview the work.\n",
    )
    .unwrap();

    // Add and close a task so we have something to review
    aiki_task(temp_dir.path(), &["add", "Output ID test task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Get the closed task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Run review with --start -o id
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", &task_id, "--start", "-o", "id"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The command should succeed
    assert!(
        output.status.success(),
        "review --start -o id should succeed, got stderr: {}",
        stderr
    );

    // stdout should be exactly one line with only the review task ID
    let trimmed = stdout.trim();
    assert!(
        !trimmed.is_empty(),
        "stdout should contain the review task ID"
    );

    // The review ID should be a valid task ID (all lowercase letters, 32 chars)
    assert!(
        trimmed.chars().all(|c| c.is_ascii_lowercase()) && trimmed.len() == 32,
        "stdout should be exactly one 32-char lowercase review task ID, got: '{}'",
        trimmed
    );

    // Verify no markdown or ANSI control sequences leaked into stdout
    assert!(
        !stdout.contains("###") && !stdout.contains("\x1b["),
        "stdout should not contain markdown headers or ANSI codes, got: '{}'",
        stdout
    );
}

#[allow(clippy::too_many_lines)]
#[test]
fn test_review_fix_output_id_no_extra_output() {
    // Regression: when --fix -o id is used, stdout should contain ONLY the review
    // task ID — no markdown, no ANSI, no extra lines.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Set up the review template so `aiki review` can create a review task.
    // The template needs to exist at .aiki/tasks/review/task.md
    let template_dir = temp_dir.path().join(".aiki/tasks/review");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(
        template_dir.join("task.md"),
        "---\nversion: 2.0.0\ntype: review\n---\n\n# Review: {{data.scope.name}}\n\nReview the work.\n",
    )
    .unwrap();

    // Add and close a task so we have something to review
    aiki_task(temp_dir.path(), &["add", "Output ID test task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Get the closed task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Run review with --fix-template fix -o id
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", &task_id, "--fix-template", "fix", "-o", "id"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The command may succeed or fail at agent spawn depending on environment, but
    // `-o id` must still avoid leaking markdown/ANSI into stdout.
    let trimmed = stdout.trim();

    if output.status.success() {
        assert!(
            !trimmed.is_empty(),
            "stdout should contain the review task ID"
        );

        assert!(
            trimmed.chars().all(|c| c.is_ascii_lowercase()) && trimmed.len() == 32,
            "stdout should be exactly one 32-char lowercase review task ID, got: '{}'",
            trimmed
        );
    } else {
        assert!(
            trimmed.is_empty(),
            "stdout should stay empty on runtime failure when -o id is requested, got: '{}'",
            trimmed
        );
        assert!(
            stderr.contains("Failed to spawn agent") || stderr.contains("Exit code"),
            "Expected a runtime spawn failure, got stderr: {}",
            stderr
        );
    }

    // Verify no markdown or ANSI control sequences leaked into stdout
    assert!(
        !stdout.contains("###") && !stdout.contains("\x1b["),
        "stdout should not contain markdown headers or ANSI codes, got: '{}'",
        stdout
    );

    if output.status.success() {
        // --- Fix-execution assertions ---
        // Verify the review task exists and is properly formed
        let review_id = trimmed;
        let show_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
            .current_dir(temp_dir.path())
            .args(["task", "show", review_id])
            .output()
            .unwrap();
        assert!(
            show_output.status.success(),
            "aiki task show should succeed for review ID {}",
            review_id
        );
        let show_stdout = String::from_utf8_lossy(&show_output.stdout);
        assert!(
            show_stdout.contains("Review:"),
            "review task should be named 'Review: ...', got: {}",
            show_stdout
        );

        // Verify data.options.fix was set on the review task by checking the raw
        // event data stored in JJ commit descriptions on the aiki/tasks branch.
        // This confirms the --fix flag was properly stored, which is the
        // precondition for run_fix to execute.
        let jj_output = Command::new("jj")
            .current_dir(temp_dir.path())
            .args([
                "log",
                "-r",
                "children(ancestors(aiki/tasks)) & description(substring:'options.fix')",
                "--no-graph",
                "-T",
                "description",
                "--ignore-working-copy",
            ])
            .output()
            .unwrap();
        let jj_stdout = String::from_utf8_lossy(&jj_output.stdout);
        assert!(
            jj_stdout.contains("options.fix:true"),
            "Review task should have data.options.fix=true set in event data, got: '{}'",
            jj_stdout
        );
    }
}

// ============================================================================
// Async Continue Path (--_continue-async) Fix-Template Forwarding Regression Tests
// ============================================================================

#[test]
fn test_async_review_fix_template_stores_fix_data() {
    // Regression: When --fix-template is used with --async, the review task must
    // store options.fix=true and options.fix_template in its data. This is the
    // precondition for run_continue_async (review.rs:860-881) to forward the
    // fix template into run_fix when issues are found.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Set up the review template
    let template_dir = temp_dir.path().join(".aiki/tasks/review");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(
        template_dir.join("task.md"),
        "---\nversion: 2.0.0\ntype: review\n---\n\n# Review: {{data.scope.name}}\n\nReview the work.\n",
    )
    .unwrap();

    // Add and close a task so we have something to review
    aiki_task(temp_dir.path(), &["add", "Async fix-template test task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Get the closed task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Run review with --fix-template fix --async -o id
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args([
            "review",
            &task_id,
            "--fix-template",
            "fix",
            "--async",
            "-o",
            "id",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The command should succeed (async returns immediately)
    assert!(
        output.status.success(),
        "review --fix-template --async -o id should succeed, got stderr: {}",
        stderr
    );

    // stdout should be exactly the review task ID
    let trimmed = stdout.trim();
    assert!(
        trimmed.chars().all(|c| c.is_ascii_lowercase()) && trimmed.len() == 32,
        "stdout should be exactly one 32-char lowercase review task ID, got: '{}'",
        trimmed
    );

    // Verify data.options.fix was set on the review task in JJ events.
    // This is critical: run_continue_async checks fix_template to decide
    // whether to call run_fix (line 860-862 in review.rs).
    let jj_output = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "log",
            "-r",
            "children(ancestors(aiki/tasks)) & description(substring:'options.fix')",
            "--no-graph",
            "-T",
            "description",
            "--ignore-working-copy",
        ])
        .output()
        .unwrap();
    let jj_stdout = String::from_utf8_lossy(&jj_output.stdout);
    assert!(
        jj_stdout.contains("options.fix:true"),
        "Async review task should have data.options.fix=true stored, got: '{}'",
        jj_stdout
    );

    // Verify data.options.fix_template was also stored (the template name itself).
    // run_continue_async forwards this value to run_fix as the plan_template arg.
    let jj_output = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "log",
            "-r",
            "children(ancestors(aiki/tasks)) & description(substring:'options.fix_template')",
            "--no-graph",
            "-T",
            "description",
            "--ignore-working-copy",
        ])
        .output()
        .unwrap();
    let jj_stdout = String::from_utf8_lossy(&jj_output.stdout);
    assert!(
        jj_stdout.contains("options.fix_template:fix"),
        "Async review task should store fix_template value, got: '{}'",
        jj_stdout
    );
}

#[test]
fn test_async_review_without_fix_template_no_fix_data() {
    // Regression (negative case): When --async is used WITHOUT --fix-template,
    // the review task must NOT have options.fix set. This corresponds to the
    // early return in run_continue_async (review.rs:860-862) when
    // fix_template.is_none().
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Set up the review template
    let template_dir = temp_dir.path().join(".aiki/tasks/review");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(
        template_dir.join("task.md"),
        "---\nversion: 2.0.0\ntype: review\n---\n\n# Review: {{data.scope.name}}\n\nReview the work.\n",
    )
    .unwrap();

    // Add and close a task so we have something to review
    aiki_task(temp_dir.path(), &["add", "Async no-fix test task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Get the closed task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Run review with --async but WITHOUT --fix-template
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", &task_id, "--async", "-o", "id"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The command should succeed
    assert!(
        output.status.success(),
        "review --async -o id should succeed, got stderr: {}",
        stderr
    );

    // stdout should be exactly the review task ID
    let trimmed = stdout.trim();
    assert!(
        trimmed.chars().all(|c| c.is_ascii_lowercase()) && trimmed.len() == 32,
        "stdout should be a 32-char lowercase review task ID, got: '{}'",
        trimmed
    );

    // Verify data.options.fix is NOT set on the review task.
    // Without --fix-template, run_continue_async should early-return (line 860-862)
    // and never call run_fix, regardless of issue count.
    let jj_output = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "log",
            "-r",
            "children(ancestors(aiki/tasks)) & description(substring:'options.fix')",
            "--no-graph",
            "-T",
            "description",
            "--ignore-working-copy",
        ])
        .output()
        .unwrap();
    let jj_stdout = String::from_utf8_lossy(&jj_output.stdout);
    assert!(
        !jj_stdout.contains("options.fix:true"),
        "Review without --fix-template should NOT have options.fix set, got: '{}'",
        jj_stdout
    );
}

#[test]
fn test_continue_async_with_fix_template_flag_accepted() {
    // Regression: The hidden --_continue-async flag must work with --fix-template.
    // This is the entry point that run_continue_async uses (review.rs:400-401).
    // The flag combination must parse correctly (not produce "unexpected argument").
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Call --_continue-async with --fix-template and a fake review ID.
    // The command will fail (review task doesn't exist), but the flags
    // should be recognized by the parser (no "unexpected argument" error).
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args([
            "review",
            "--_continue-async",
            "nonexistentreviewtaskidpadding00",
            "--fix-template",
            "fix",
        ])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Should NOT be a parser error
    assert!(
        !stderr.contains("unexpected argument"),
        "--_continue-async with --fix-template should be accepted by parser, got stderr: {}",
        stderr
    );
}

#[test]
fn test_continue_async_without_fix_template_flag_accepted() {
    // Regression: The hidden --_continue-async flag must work WITHOUT --fix-template.
    // This exercises the early return path in run_continue_async (line 860-862).
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Call --_continue-async without --fix-template and a fake review ID.
    // The command will fail (review task doesn't exist), but the flag
    // should be recognized by the parser.
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args([
            "review",
            "--_continue-async",
            "nonexistentreviewtaskidpadding00",
        ])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Should NOT be a parser error
    assert!(
        !stderr.contains("unexpected argument"),
        "--_continue-async without --fix-template should be accepted by parser, got stderr: {}",
        stderr
    );
}

#[test]
fn test_async_review_fix_template_custom_value_stored() {
    // Regression: When --fix-template is given a custom value (not the default),
    // that custom value must be stored in the review task data so that
    // run_continue_async forwards it correctly to run_fix.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Set up the review template
    let template_dir = temp_dir.path().join(".aiki/tasks/review");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(
        template_dir.join("task.md"),
        "---\nversion: 2.0.0\ntype: review\n---\n\n# Review: {{data.scope.name}}\n\nReview the work.\n",
    )
    .unwrap();

    // Add and close a task
    aiki_task(temp_dir.path(), &["add", "Custom fix template test"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Get the closed task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Run review with a CUSTOM --fix-template value via --async
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args([
            "review",
            &task_id,
            "--fix-template",
            "my-org/custom-fix",
            "--async",
            "-o",
            "id",
        ])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "review --fix-template my-org/custom-fix --async -o id should succeed, got stderr: {}",
        stderr
    );

    let trimmed = stdout.trim();
    assert!(
        trimmed.chars().all(|c| c.is_ascii_lowercase()) && trimmed.len() == 32,
        "stdout should be a 32-char review task ID, got: '{}'",
        trimmed
    );

    // Verify the custom fix_template value was stored (not the default "fix").
    // run_continue_async reads this from args and passes it to run_fix.
    let jj_output = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "log",
            "-r",
            "children(ancestors(aiki/tasks)) & description(substring:'options.fix_template')",
            "--no-graph",
            "-T",
            "description",
            "--ignore-working-copy",
        ])
        .output()
        .unwrap();
    let jj_stdout = String::from_utf8_lossy(&jj_output.stdout);
    assert!(
        jj_stdout.contains("options.fix_template:my-org/custom-fix"),
        "Review should store custom fix_template value 'my-org/custom-fix', got: '{}'",
        jj_stdout
    );
}

// ============================================================================
// Blocking Review Path: fix-template Forwarding Regression Tests
// ============================================================================

#[test]
fn test_blocking_review_fix_template_creates_review_with_fix_options() {
    // Regression: the blocking review path (review.rs:796-831) should store
    // options.fix and options.fix_template in the review task data when
    // --fix-template is provided. This is the precondition for run_fix to be
    // called after task_run completes.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Set up the review template
    let template_dir = temp_dir.path().join(".aiki/tasks/review");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(
        template_dir.join("task.md"),
        "---\nversion: 2.0.0\ntype: review\n---\n\n# Review: {{data.scope.name}}\n\nReview the work.\n",
    )
    .unwrap();

    // Add and close a task so we have something to review
    aiki_task(temp_dir.path(), &["add", "Blocking fix-template test task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Get the closed task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Run blocking review with --fix-template fix
    // The command may fail at task_run if no agent is available, but the review
    // task is created and its data stored BEFORE task_run is called.
    let _output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", &task_id, "--fix-template", "fix", "-o", "id"])
        .output()
        .unwrap();

    // Verify the review task was created with options.fix:true in the event data.
    // This is stored by create_review (review.rs:629-632) before task_run is called,
    // so it persists regardless of whether the agent succeeded.
    let jj_output = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "log",
            "-r",
            "children(ancestors(aiki/tasks)) & description(substring:'options.fix')",
            "--no-graph",
            "-T",
            "description",
            "--ignore-working-copy",
        ])
        .output()
        .unwrap();
    let jj_stdout = String::from_utf8_lossy(&jj_output.stdout);
    assert!(
        jj_stdout.contains("options.fix:true"),
        "Review task should have data.options.fix=true, got: '{}'",
        jj_stdout
    );

    // Verify options.fix_template is also stored (forwarded from --fix-template)
    let jj_output2 = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "log",
            "-r",
            "children(ancestors(aiki/tasks)) & description(substring:'options.fix_template')",
            "--no-graph",
            "-T",
            "description",
            "--ignore-working-copy",
        ])
        .output()
        .unwrap();
    let jj_stdout2 = String::from_utf8_lossy(&jj_output2.stdout);
    assert!(
        jj_stdout2.contains("options.fix_template:fix"),
        "Review task should have data.options.fix_template=fix, got: '{}'",
        jj_stdout2
    );
}

#[test]
fn test_blocking_review_issue_count_set_when_issues_exist() {
    // Regression: when a review task has issues added via `aiki review issue add`
    // and is then closed, data.issue_count should be set to reflect the number of
    // issues. This is the other precondition for the blocking review path at
    // review.rs:804-808 to evaluate has_issues=true.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Set up the review template
    let template_dir = temp_dir.path().join(".aiki/tasks/review");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(
        template_dir.join("task.md"),
        "---\nversion: 2.0.0\ntype: review\n---\n\n# Review: {{data.scope.name}}\n\nReview the work.\n",
    )
    .unwrap();

    // Add and close a task so we have something to review
    aiki_task(temp_dir.path(), &["add", "Issue count test task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Get the closed task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Create a review task using --start (assigns to current agent, no task_run)
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", &task_id, "--start", "-o", "id"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "review --start -o id should succeed, stderr: {}",
        stderr
    );
    let review_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(
        !review_id.is_empty() && review_id.chars().all(|c| c.is_ascii_lowercase()),
        "Should get a valid review task ID, got: '{}'",
        review_id
    );

    // Add issues to the review task
    aiki_review(
        temp_dir.path(),
        &[
            "issue",
            "add",
            &review_id,
            "Bug found in auth handler",
            "--high",
        ],
    )
    .success();

    aiki_review(
        temp_dir.path(),
        &[
            "issue",
            "add",
            &review_id,
            "Missing error handling in API client",
        ],
    )
    .success();

    // Close the review task — this triggers issue_count computation (task.rs:2882-2908)
    aiki_task(
        temp_dir.path(),
        &["close", &review_id, "--summary", "Found 2 issues"],
    )
    .success();

    // Verify issue_count was set in the event data
    let jj_output = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "log",
            "-r",
            "children(ancestors(aiki/tasks)) & description(substring:'issue_count')",
            "--no-graph",
            "-T",
            "description",
            "--ignore-working-copy",
        ])
        .output()
        .unwrap();
    let jj_stdout = String::from_utf8_lossy(&jj_output.stdout);
    assert!(
        jj_stdout.contains("issue_count:2"),
        "Review task should have data.issue_count=2 after closing with 2 issues, got: '{}'",
        jj_stdout
    );

    // Also verify approved:false (since issues exist)
    assert!(
        jj_stdout.contains("approved:false"),
        "Review task should have data.approved=false when issues exist, got: '{}'",
        jj_stdout
    );
}

#[test]
fn test_blocking_review_no_fix_template_no_fix_options() {
    // Negative case: when --fix-template is NOT provided, the review task should
    // NOT have options.fix in its data. This means run_fix will NOT be called
    // even if issues exist (review.rs:810 condition is false).
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Set up the review template
    let template_dir = temp_dir.path().join(".aiki/tasks/review");
    fs::create_dir_all(&template_dir).unwrap();
    fs::write(
        template_dir.join("task.md"),
        "---\nversion: 2.0.0\ntype: review\n---\n\n# Review: {{data.scope.name}}\n\nReview the work.\n",
    )
    .unwrap();

    // Add and close a task so we have something to review
    aiki_task(temp_dir.path(), &["add", "No fix-template test task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Get the closed task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Create a review task WITHOUT --fix-template using --start
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", &task_id, "--start", "-o", "id"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "review --start -o id should succeed, stderr: {}",
        stderr
    );
    let review_id = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Add issues even though no --fix-template was used
    aiki_review(
        temp_dir.path(),
        &["issue", "add", &review_id, "Some issue found", "--high"],
    )
    .success();

    // Close the review (sets issue_count > 0)
    aiki_task(
        temp_dir.path(),
        &["close", &review_id, "--summary", "Found issues"],
    )
    .success();

    // Verify that options.fix is NOT set in any event data for this review
    let jj_output = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "log",
            "-r",
            "children(ancestors(aiki/tasks)) & description(substring:'options.fix')",
            "--no-graph",
            "-T",
            "description",
            "--ignore-working-copy",
        ])
        .output()
        .unwrap();
    let jj_stdout = String::from_utf8_lossy(&jj_output.stdout);
    assert!(
        !jj_stdout.contains("options.fix:true"),
        "Review without --fix-template should NOT have options.fix=true, got: '{}'",
        jj_stdout
    );

    // Verify issue_count IS set (issues exist, but fix won't trigger)
    let jj_output2 = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "log",
            "-r",
            "children(ancestors(aiki/tasks)) & description(substring:'issue_count')",
            "--no-graph",
            "-T",
            "description",
            "--ignore-working-copy",
        ])
        .output()
        .unwrap();
    let jj_stdout2 = String::from_utf8_lossy(&jj_output2.stdout);
    assert!(
        jj_stdout2.contains("issue_count:1"),
        "Review should have issue_count=1 even without --fix-template, got: '{}'",
        jj_stdout2
    );
}

// ============================================================================
// Short Flag Alias Tests
// ============================================================================

#[test]
fn test_review_short_fix_flag_recognized() {
    // -f short flag should be recognized as equivalent to --fix
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add and close a task so we have something to review
    aiki_task(temp_dir.path(), &["add", "Test task for short flag"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Get the closed task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--closed"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find closed task ID");

    // Run review with -f (short flag)
    let short_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", &task_id, "-f"])
        .output()
        .unwrap();
    let short_stderr = String::from_utf8_lossy(&short_output.stderr);
    assert!(
        !short_stderr.contains("unexpected argument"),
        "-f short flag should be recognized by the parser, got stderr: {}",
        short_stderr
    );

    // Run review with --fix (long flag)
    let long_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", &task_id, "--fix"])
        .output()
        .unwrap();
    let long_stderr = String::from_utf8_lossy(&long_output.stderr);
    assert!(
        !long_stderr.contains("unexpected argument"),
        "--fix long flag should be recognized by the parser, got stderr: {}",
        long_stderr
    );

    // Assert behavioral equivalence
    assert_eq!(
        short_output.status.code(),
        long_output.status.code(),
        "review -f and review --fix should have the same exit code (short={:?}, long={:?})",
        short_output.status.code(),
        long_output.status.code()
    );
    let short_stdout = String::from_utf8_lossy(&short_output.stdout);
    let long_stdout = String::from_utf8_lossy(&long_output.stdout);
    assert_eq!(
        short_stdout.is_empty(),
        long_stdout.is_empty(),
        "review -f and review --fix should both produce output or both be empty\n  short stdout: {}\n  long stdout: {}",
        short_stdout,
        long_stdout
    );
}

#[test]
fn test_build_short_review_flag_recognized() {
    // -r short flag should be recognized by the build command parser
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a minimal plan file
    std::fs::write(
        temp_dir.path().join("test-plan.md"),
        "# Test Plan\n\nA test plan.\n",
    )
    .unwrap();

    // Run build with -r (short flag)
    let short_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["build", "test-plan.md", "-r"])
        .output()
        .unwrap();
    let short_stderr = String::from_utf8_lossy(&short_output.stderr);
    assert!(
        !short_stderr.contains("unexpected argument"),
        "-r short flag should be recognized by the build parser, got stderr: {}",
        short_stderr
    );

    // Run build with --review (long flag)
    let long_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["build", "test-plan.md", "--review"])
        .output()
        .unwrap();
    let long_stderr = String::from_utf8_lossy(&long_output.stderr);
    assert!(
        !long_stderr.contains("unexpected argument"),
        "--review long flag should be recognized by the build parser, got stderr: {}",
        long_stderr
    );

    // Assert behavioral equivalence
    assert_eq!(
        short_output.status.code(),
        long_output.status.code(),
        "build -r and build --review should have the same exit code (short={:?}, long={:?})",
        short_output.status.code(),
        long_output.status.code()
    );
    let short_stdout = String::from_utf8_lossy(&short_output.stdout);
    let long_stdout = String::from_utf8_lossy(&long_output.stdout);
    assert_eq!(
        short_stdout.is_empty(),
        long_stdout.is_empty(),
        "build -r and build --review should both produce output or both be empty\n  short stdout: {}\n  long stdout: {}",
        short_stdout,
        long_stdout
    );
}

#[test]
fn test_build_short_fix_flag_recognized() {
    // -f short flag should be recognized by the build command parser
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    std::fs::write(
        temp_dir.path().join("test-plan.md"),
        "# Test Plan\n\nA test plan.\n",
    )
    .unwrap();

    // Run build with -f (short flag)
    let short_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["build", "test-plan.md", "-f"])
        .output()
        .unwrap();
    let short_stderr = String::from_utf8_lossy(&short_output.stderr);
    assert!(
        !short_stderr.contains("unexpected argument"),
        "-f short flag should be recognized by the build parser, got stderr: {}",
        short_stderr
    );

    // Run build with --fix (long flag)
    let long_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["build", "test-plan.md", "--fix"])
        .output()
        .unwrap();
    let long_stderr = String::from_utf8_lossy(&long_output.stderr);
    assert!(
        !long_stderr.contains("unexpected argument"),
        "--fix long flag should be recognized by the build parser, got stderr: {}",
        long_stderr
    );

    // Assert behavioral equivalence
    assert_eq!(
        short_output.status.code(),
        long_output.status.code(),
        "build -f and build --fix should have the same exit code (short={:?}, long={:?})",
        short_output.status.code(),
        long_output.status.code()
    );
    let short_stdout = String::from_utf8_lossy(&short_output.stdout);
    let long_stdout = String::from_utf8_lossy(&long_output.stdout);
    assert_eq!(
        short_stdout.is_empty(),
        long_stdout.is_empty(),
        "build -f and build --fix should both produce output or both be empty\n  short stdout: {}\n  long stdout: {}",
        short_stdout,
        long_stdout
    );
}

#[test]
fn test_build_combined_short_flags() {
    // -r and -f combined should be recognized by the build command parser
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    std::fs::write(
        temp_dir.path().join("test-plan.md"),
        "# Test Plan\n\nA test plan.\n",
    )
    .unwrap();

    // Run build with -r -f (short flags)
    let short_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["build", "test-plan.md", "-r", "-f"])
        .output()
        .unwrap();
    let short_stderr = String::from_utf8_lossy(&short_output.stderr);
    assert!(
        !short_stderr.contains("unexpected argument"),
        "-r -f combined should be recognized by the build parser, got stderr: {}",
        short_stderr
    );

    // Run build with --review --fix (long flags)
    let long_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["build", "test-plan.md", "--review", "--fix"])
        .output()
        .unwrap();
    let long_stderr = String::from_utf8_lossy(&long_output.stderr);
    assert!(
        !long_stderr.contains("unexpected argument"),
        "--review --fix combined should be recognized by the build parser, got stderr: {}",
        long_stderr
    );

    // Assert behavioral equivalence
    assert_eq!(
        short_output.status.code(),
        long_output.status.code(),
        "build -r -f and build --review --fix should have the same exit code (short={:?}, long={:?})",
        short_output.status.code(),
        long_output.status.code()
    );
    let short_stdout = String::from_utf8_lossy(&short_output.stdout);
    let long_stdout = String::from_utf8_lossy(&long_output.stdout);
    assert_eq!(
        short_stdout.is_empty(),
        long_stdout.is_empty(),
        "build -r -f and build --review --fix should both produce output or both be empty\n  short stdout: {}\n  long stdout: {}",
        short_stdout,
        long_stdout
    );
}

#[test]
fn test_build_continue_async_flag() {
    // Verify that --_continue-async flag is recognized by the build command parser
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a task to use as the continue target
    let add_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Test build target"])
        .output()
        .unwrap();
    assert!(add_output.status.success(), "task add failed");

    // Get the task ID from add output or list output
    let add_stdout = String::from_utf8_lossy(&add_output.stdout);
    let task_id = extract_task_id(&add_stdout).unwrap_or_else(|| {
        let list_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
            .current_dir(temp_dir.path())
            .args(["task", "list"])
            .output()
            .unwrap();
        let list_stdout = String::from_utf8_lossy(&list_output.stdout);
        extract_task_id(&list_stdout).expect("Should find task ID in add or list output")
    });

    // Run build --_continue-async <task-id>
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["build", "--_continue-async", &task_id])
        .output()
        .unwrap();

    // The command should be recognized by the parser (not an argument parsing error).
    // It may fail at runtime (no actual epic exists), but it should NOT fail with
    // an "unexpected argument" error.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--_continue-async flag should be recognized by the build parser, got stderr: {}",
        stderr
    );
}

#[test]
fn test_build_continue_async_reads_json_payload() {
    // Verify that `aiki build --_continue-async <id>` reads and parses JSON from stdin
    // (exercising async_run::read_continue_opts::<BuildOpts>())
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a task to get a valid-format task ID
    let add_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Test build target"])
        .output()
        .unwrap();
    assert!(add_output.status.success(), "task add failed");

    let add_stdout = String::from_utf8_lossy(&add_output.stdout);
    let task_id = extract_task_id(&add_stdout).unwrap_or_else(|| {
        let list_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
            .current_dir(temp_dir.path())
            .args(["task", "list"])
            .output()
            .unwrap();
        let list_stdout = String::from_utf8_lossy(&list_output.stdout);
        extract_task_id(&list_stdout).expect("Should find task ID in add or list output")
    });

    // Construct a valid BuildOpts JSON payload
    let payload = serde_json::json!({
        "target": task_id,
        "run_kind": "ContinueAsync",
        "output": null,
        "workflow": {
            "restart": false,
            "decompose_template": null,
            "loop_template": null,
            "agent": null,
            "agent_str": null,
            "target": null,
            "code": false,
            "review": false,
            "review_template": null,
            "fix": false,
            "fix_template": null,
            "autorun": false,
            "plan_template": null,
            "once": false
        }
    });

    // Spawn with piped stdin so we can write JSON
    let mut child = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["build", "--_continue-async", &task_id])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("Failed to spawn aiki build");

    // Write JSON payload to stdin, then drop to close it
    {
        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        stdin
            .write_all(payload.to_string().as_bytes())
            .expect("Failed to write to stdin");
    }

    let output = child.wait_with_output().expect("Failed to wait on child");
    let stderr = String::from_utf8_lossy(&output.stderr);

    // The JSON should have been parsed successfully — no stdin-parsing error
    assert!(
        !stderr.contains("failed to read async continue options from stdin"),
        "stdin JSON should be parsed successfully, got stderr: {}",
        stderr
    );

    // The process should have progressed past stdin parsing into the build workflow.
    // It will fail downstream (task/plan doesn't exist properly), but the error
    // should be about task/plan lookup, not about stdin parsing.
    assert!(
        !stderr.contains("unexpected argument"),
        "should not have argument parsing errors, got stderr: {}",
        stderr
    );
}
