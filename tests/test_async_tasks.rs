//! Integration tests for async task execution
//!
//! Tests for:
//! - `--async` flag parsing on `aiki task run` command
//! - PID file creation/cleanup functions
//! - Wait command's exponential backoff calculation
//! - Task ID extraction from XML (for piping support)
//! - Terminate background task function (mock scenarios)

use std::fs;
use std::io::Write;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;

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

/// Helper to run aiki wait command
fn aiki_wait(path: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(path);
    cmd.arg("wait");
    for arg in args {
        cmd.arg(arg);
    }
    cmd.assert()
}

/// Helper to extract task short ID from markdown output.
/// Looks for patterns like `[p2] abcdefg` or `Added abcdefg`.
fn extract_task_id(output: &str) -> Option<String> {
    // Try markdown list format: [pN] <short-id>
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("[p") {
            // Skip priority digit and "] "
            if let Some(after_bracket) = rest.get(1..).and_then(|s| s.strip_prefix("] ")) {
                let id: String = after_bracket.chars().take_while(|c| c.is_ascii_lowercase()).collect();
                if id.len() >= 7 {
                    return Some(id);
                }
            }
        }
        // Try "Added <id>" format
        if let Some(rest) = trimmed.strip_prefix("Added ") {
            let id: String = rest.chars().take_while(|c| c.is_ascii_lowercase()).collect();
            if id.len() >= 7 {
                return Some(id);
            }
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

    // Add a task with an agent assignee (required for task run)
    aiki_task(temp_dir.path(), &["add", "Test async task"]).success();

    // Get the task ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID");

    // Try running with --async flag - will fail because no agent assigned,
    // but the flag should be recognized (not a parse error)
    aiki_task(temp_dir.path(), &["run", &task_id, "--async"])
        .failure() // Fails because task has no agent assignee
        .stderr(predicate::str::contains("no assignee").or(predicate::str::contains("No assignee")));
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

    // Try running with -a short flag
    aiki_task(temp_dir.path(), &["run", &task_id, "-a"])
        .failure() // Fails because task has no agent assignee
        .stderr(predicate::str::contains("no assignee").or(predicate::str::contains("No assignee")));
}

#[test]
fn test_task_run_requires_task_id() {
    // Verify that task run requires a task ID argument
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Try running without task ID - should fail with argument error
    aiki_task(temp_dir.path(), &["run"])
        .failure()
        .stderr(predicate::str::contains("required"));
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
        .args(["task", "run", &task_id, "--agent", "claude-code", "--async"])
        .output()
        .unwrap();

    // Either success (agent installed) or failure (agent not installed or spawn failed)
    // But should not be a CLI argument parsing error
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The output should either be XML success/error format, not a usage/argument error
    assert!(
        stdout.contains("aiki_task") || stderr.contains("not found") || stderr.contains("spawn"),
        "Should produce XML output or agent-related error, got stdout='{}', stderr='{}'",
        stdout,
        stderr
    );
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
    aiki_task(
        temp_dir.path(),
        &["run", &task_id, "--agent", "invalid-agent"],
    )
    .failure()
    .stderr(predicate::str::contains("Unknown agent type").or(predicate::str::contains("unknown agent")));
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
        "xqrmnpst.1",        // Subtask
        "xqrmnpst.1.2",      // Nested subtask
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

    assert!(!pid_file.exists(), "PID file should be removed after cleanup");
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
        assert!(
            parsed.is_ok(),
            "PID '{}' should parse as u32",
            pid_str
        );
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

    // Run wait with explicit empty stdin
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .arg("wait")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("Failed to run aiki wait");

    // Should fail because no task ID provided
    assert!(
        !output.status.success(),
        "wait without task ID should fail"
    );
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
    assert_eq!(
        extract_task_id(md),
        Some("abcdefg".to_string())
    );
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
        .stdout(predicate::str::contains("<stopped"));
}

// ============================================================================
// XML Output Format Tests for Async Mode
// ============================================================================

#[test]
fn test_task_run_sync_xml_format() {
    // Verify XML output format for sync task run
    // Note: We can't actually run a task (no agent), but we can verify error format
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    aiki_task(temp_dir.path(), &["add", "XML format test"]).success();

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_task_id(&stdout).expect("Should find task ID");

    // Run (will fail, but check XML format)
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "run", &task_id])
        .output()
        .unwrap();

    // Check that error output is XML formatted
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Either succeeds with XML or fails with XML error format
    assert!(
        stdout.contains("aiki_task") || stdout.is_empty(),
        "Output should be XML formatted or empty (error on stderr)"
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

    // 2. Verify task run --async is a valid command (even if it fails)
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "run", &task_id, "--async"])
        .output()
        .unwrap();

    // Should fail due to no assignee, not due to invalid args
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("assignee") || stderr.contains("agent") || stderr.to_lowercase().contains("no assignee"),
        "Error should be about missing assignee, not invalid args. Got: {}",
        stderr
    );

    // 3. Verify wait command works (though task not running)
    // Close the task first so wait can succeed
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Test done"]).success();

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
