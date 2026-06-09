//! Integration tests for task commands
//!
//! Tests the complete task workflow through the CLI interface.

mod common;

use assert_cmd::prelude::*;
use common::jj_available;
use predicates::prelude::*;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// Helper function to initialize a Git repository
fn init_git_repo(path: &std::path::Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("Failed to initialize Git repository");

    // Configure git user for commits
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

/// Extract short task ID from "Added <id>" output line
fn extract_short_id(output: &str) -> String {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("Added ") {
            // "Added abc1234 — name" or "Added abc1234"
            let id = rest.split_whitespace().next().unwrap_or("");
            return id.to_string();
        }
    }
    panic!("Could not find 'Added <id>' in output: {}", output);
}

/// Extract short task ID from "Started <id>" output line
fn extract_started_id(output: &str) -> String {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("Started ") {
            let id = rest.split_whitespace().next().unwrap_or("");
            return id.to_string();
        }
    }
    panic!("Could not find 'Started <id>' in output: {}", output);
}

/// Extract the first short task ID from a list output line matching `[pN] <id>  <name>`
fn extract_id_from_list(output: &str) -> String {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[p") {
            // Format: "[pN] <id>  <name>" or "[pN] <id> — <name>"
            if let Some(after_bracket) = trimmed.find("] ") {
                let rest = &trimmed[after_bracket + 2..];
                let id = rest.split_whitespace().next().unwrap_or("");
                if !id.is_empty() {
                    return id.to_string();
                }
            }
        }
    }
    panic!("Could not find task ID in list output: {}", output);
}

/// Extract a short task ID from list output whose line contains the given name
fn extract_id_from_list_by_name(output: &str, name: &str) -> String {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.contains(name) && trimmed.starts_with("[p") {
            if let Some(after_bracket) = trimmed.find("] ") {
                let rest = &trimmed[after_bracket + 2..];
                let id = rest.split_whitespace().next().unwrap_or("");
                if !id.is_empty() {
                    return id.to_string();
                }
            }
        }
    }
    panic!("Could not find task '{}' in list output: {}", name, output);
}

fn task_stdout(path: &std::path::Path, args: &[&str]) -> String {
    let output = aiki_task(path, args).success();
    String::from_utf8_lossy(&output.get_output().stdout).into_owned()
}

fn task_stdout_without_thread(path: &std::path::Path, args: &[&str]) -> String {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(path).arg("task").env_remove("AIKI_THREAD");
    for arg in args {
        cmd.arg(arg);
    }
    let output = cmd.output().expect("Failed to run aiki task command");
    assert!(
        output.status.success(),
        "aiki task {:?} failed: stdout={}\nstderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn jj_cmd(path: &Path) -> Command {
    let mut cmd = Command::new("jj");
    cmd.current_dir(path)
        .env("JJ_USER", "Test User")
        .env("JJ_EMAIL", "test@example.com");
    cmd
}

fn run_jj(path: &Path, args: &[&str]) -> String {
    let output = jj_cmd(path)
        .args(args)
        .output()
        .expect("Failed to run jj command");
    assert!(
        output.status.success(),
        "jj {:?} failed: stdout={}\nstderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn aiki_task_with_env(path: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(path).arg("task");
    for (key, value) in envs {
        cmd.env(key, value);
    }
    for arg in args {
        cmd.arg(arg);
    }
    let output = cmd.output().expect("Failed to run aiki task command");
    assert!(
        output.status.success(),
        "aiki task {:?} failed: stdout={}\nstderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn aiki_stdout_with_env(path: &Path, args: &[&str], envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(path);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    for arg in args {
        cmd.arg(arg);
    }
    let output = cmd.output().expect("Failed to run aiki command");
    assert!(
        output.status.success(),
        "aiki {:?} failed: stdout={}\nstderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn install_fake_agent_cli(path: &Path, binary_name: &str) -> String {
    let bin_dir = path.join("fake-bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let script_path = bin_dir.join(binary_name);
    fs::write(&script_path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = fs::metadata(&script_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();

    let current_path = env::var("PATH").unwrap_or_default();
    format!("{}:{}", bin_dir.display(), current_path)
}

#[test]
fn test_close_rejects_confidence_with_wont_do() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let task_id = extract_short_id(&task_stdout(temp_dir.path(), &["add", "Skip this task"]));
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &task_id,
            "--wont-do",
            "--confidence",
            "2",
            "--summary",
            "Skipping",
        ],
    )
    .failure()
    .stderr(predicate::str::contains(
        "--confidence cannot be used with --wont-do.",
    ));
}

#[test]
fn test_close_allows_confidence_for_multi_close() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let id1 = extract_short_id(&task_stdout(temp_dir.path(), &["add", "Task one"]));
    let id2 = extract_short_id(&task_stdout(temp_dir.path(), &["add", "Task two"]));

    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &id1,
            &id2,
            "--confidence",
            "3",
            "--summary",
            "Done",
        ],
    )
    .success();
}

#[test]
fn test_closed_list_and_show_handle_optional_confidence() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let confident_id = extract_short_id(&task_stdout(temp_dir.path(), &["add", "Confident task"]));
    let legacy_id = extract_short_id(&task_stdout(temp_dir.path(), &["add", "Legacy task"]));

    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &confident_id,
            "--confidence",
            "3",
            "--summary",
            "Confident close",
        ],
    )
    .success();
    aiki_task(
        temp_dir.path(),
        &["close", &legacy_id, "--summary", "Legacy close"],
    )
    .success();

    let closed_output = task_stdout_without_thread(temp_dir.path(), &["list", "--all", "--closed"]);
    assert!(closed_output.contains("Confident task"));
    assert!(closed_output.contains("Legacy task"));
    let closed_tasks = tasks_section(&closed_output);
    assert!(closed_tasks.contains("[p2]"));
    assert!(closed_tasks.contains("Confident task [c3]"));
    assert!(closed_tasks.contains("  ↳ Confident close"));
    assert!(closed_tasks.contains("Legacy task"));
    assert!(closed_tasks.contains("  ↳ Legacy close"));

    let legacy_line = closed_tasks
        .lines()
        .find(|line| line.contains("Legacy task"))
        .expect("legacy task line missing from closed list");
    assert!(!legacy_line.contains("[c"));

    let filtered_output = task_stdout_without_thread(
        temp_dir.path(),
        &["list", "--all", "--closed", "--max-confidence", "3"],
    );
    let filtered_tasks = tasks_section(&filtered_output);
    assert!(filtered_tasks.contains("Confident task [c3]"));
    assert!(filtered_tasks.contains("  ↳ Confident close"));
    assert!(!filtered_tasks.contains("Legacy task"));

    let confident_show = task_stdout(temp_dir.path(), &["show", &confident_id]);
    assert!(confident_show.contains("Confidence: 3 (high)"));

    let legacy_show = task_stdout(temp_dir.path(), &["show", &legacy_id]);
    assert!(!legacy_show.contains("Confidence:"));
}

fn extract_tldr_task_id(output: &str) -> String {
    for line in output.lines() {
        if let Some(prefix) = line.split("tldr task ").nth(1) {
            let id = prefix
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches("...");
            if !id.is_empty() {
                return id.to_string();
            }
        }
    }
    panic!("Could not find tldr task id in output: {}", output);
}

/// Extract the filtered "Tasks" section from list output, excluding the
/// context footer (Ready/In Progress queues appended after the filtered results).
fn tasks_section(output: &str) -> &str {
    // The context footer starts with a blank line followed by a status header
    // like "Ready (N):" or "In Progress:". Split at the first such boundary.
    for footer in &[
        "\nReady (",
        "\nReady:\n",
        "\nIn Progress:",
        "\nIn Progress (",
    ] {
        if let Some(pos) = output.find(footer) {
            return &output[..pos];
        }
    }
    output
}

fn extract_full_id_from_show(output: &str) -> String {
    for line in output.lines() {
        if let Some(id) = line.strip_prefix("ID: ") {
            return id.to_string();
        }
    }
    panic!("Could not find 'ID: <id>' in output: {}", output);
}

// ============================================================================
// Phase 1: Core Workflow Tests
// ============================================================================

#[test]
fn test_task_list_empty() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    aiki_task(temp_dir.path(), &["list"])
        .success()
        .stdout(predicate::str::contains("Ready (0):"));
}

#[test]
fn test_task_add_basic() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    aiki_task(temp_dir.path(), &["add", "Fix auth bug"])
        .success()
        .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_with_priority_p0() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = aiki_task(temp_dir.path(), &["add", "Critical bug", "--p0"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let task_id = extract_short_id(&stdout);

    // Verify priority via show
    aiki_task(temp_dir.path(), &["show", &task_id])
        .success()
        .stdout(predicate::str::contains("Priority: p0"));
}

#[test]
fn test_task_add_with_priority_p1() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = aiki_task(temp_dir.path(), &["add", "High priority task", "--p1"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let task_id = extract_short_id(&stdout);

    // Verify priority via show
    aiki_task(temp_dir.path(), &["show", &task_id])
        .success()
        .stdout(predicate::str::contains("Priority: p1"));
}

#[test]
fn test_task_add_with_priority_p3() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = aiki_task(temp_dir.path(), &["add", "Low priority task", "--p3"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let task_id = extract_short_id(&stdout);

    // Verify priority via show
    aiki_task(temp_dir.path(), &["show", &task_id])
        .success()
        .stdout(predicate::str::contains("Priority: p3"));
}

#[test]
fn test_task_list_after_add() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task
    aiki_task(temp_dir.path(), &["add", "Test task"]).success();

    // List should show the task
    aiki_task(temp_dir.path(), &["list"])
        .success()
        .stdout(predicate::str::contains("Ready (1):"))
        .stdout(predicate::str::contains("Test task"));
}

#[test]
fn test_task_start_from_ready_queue() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task
    aiki_task(temp_dir.path(), &["add", "Task to start"]).success();

    // Start with no ID should start from ready queue
    aiki_task(temp_dir.path(), &["start"])
        .success()
        .stdout(predicate::str::contains("Started"))
        .stdout(predicate::str::contains("Task to start"));
}

#[test]
fn test_task_start_shows_in_context() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add and start a task
    aiki_task(temp_dir.path(), &["add", "Working task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();

    // List should show it in context's in_progress
    aiki_task(temp_dir.path(), &["list"])
        .success()
        .stdout(predicate::str::contains("In Progress:"))
        .stdout(predicate::str::contains("Working task"));
}

#[test]
fn test_task_stop_current() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add and start a task
    aiki_task(temp_dir.path(), &["add", "Task to stop"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();

    // Stop the current task
    aiki_task(temp_dir.path(), &["stop"])
        .success()
        .stdout(predicate::str::contains("Stopped"))
        .stdout(predicate::str::contains("Task to stop"));
}

#[test]
fn test_task_stop_with_reason() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add and start a task
    aiki_task(temp_dir.path(), &["add", "Task with blocker"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();

    // Stop with reason
    aiki_task(
        temp_dir.path(),
        &["stop", "--reason", "Need design decision"],
    )
    .success()
    .stdout(predicate::str::contains("Stopped"));
}

#[test]
fn test_task_close_current() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add and start a task
    aiki_task(temp_dir.path(), &["add", "Task to complete"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();

    // Close the current task
    aiki_task(temp_dir.path(), &["close", "--summary", "Test completed"])
        .success()
        .stdout(predicate::str::contains("Closed"));
}

#[test]
fn test_task_close_wont_do() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task and extract its short ID
    let add_output = aiki_task(temp_dir.path(), &["add", "Task to abandon"]).success();
    let add_stdout = String::from_utf8_lossy(&add_output.get_output().stdout);
    let short_id = add_stdout
        .strip_prefix("Added ")
        .and_then(|s| s.split_whitespace().next())
        .expect("Should extract short ID from add output");

    aiki_task(temp_dir.path(), &["start"]).success();

    // Close as won't do
    aiki_task(
        temp_dir.path(),
        &["close", "--wont-do", "--summary", "Not implementing"],
    )
    .success()
    .stdout(predicate::str::contains("Closed"));

    // Verify the outcome persisted as wont_do via show
    let show_output = aiki_task(temp_dir.path(), &["show", short_id]).success();
    let show_stdout = String::from_utf8_lossy(&show_output.get_output().stdout);
    assert!(
        show_stdout.contains("closed (wont_do)"),
        "Task should have wont_do outcome after --wont-do close, got: {}",
        show_stdout
    );
}

#[test]
fn test_task_close_with_outcome_done() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task and extract its short ID
    let add_output = aiki_task(temp_dir.path(), &["add", "Task with explicit done"]).success();
    let add_stdout = String::from_utf8_lossy(&add_output.get_output().stdout);
    let short_id = add_stdout
        .strip_prefix("Added ")
        .and_then(|s| s.split_whitespace().next())
        .expect("Should extract short ID from add output");

    aiki_task(temp_dir.path(), &["start"]).success();

    // Close with --outcome done (explicit)
    aiki_task(
        temp_dir.path(),
        &["close", "--outcome", "done", "--summary", "Done explicitly"],
    )
    .success()
    .stdout(predicate::str::contains("Closed"));

    // Verify the outcome persisted as done via show
    let show_output = aiki_task(temp_dir.path(), &["show", short_id]).success();
    let show_stdout = String::from_utf8_lossy(&show_output.get_output().stdout);
    assert!(
        show_stdout.contains("closed (done)"),
        "Task should have done outcome after --outcome done close, got: {}",
        show_stdout
    );
}

#[test]
fn test_task_close_with_outcome_wont_do() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add a task and extract its short ID
    let add_output = aiki_task(temp_dir.path(), &["add", "Task with outcome wont_do"]).success();
    let add_stdout = String::from_utf8_lossy(&add_output.get_output().stdout);
    let short_id = add_stdout
        .strip_prefix("Added ")
        .and_then(|s| s.split_whitespace().next())
        .expect("Should extract short ID from add output");

    aiki_task(temp_dir.path(), &["start"]).success();

    // Close with --outcome wont_do
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            "--outcome",
            "wont_do",
            "--summary",
            "Won't do via outcome",
        ],
    )
    .success()
    .stdout(predicate::str::contains("Closed"));

    // Verify the outcome persisted as wont_do via show
    let show_output = aiki_task(temp_dir.path(), &["show", short_id]).success();
    let show_stdout = String::from_utf8_lossy(&show_output.get_output().stdout);
    assert!(
        show_stdout.contains("closed (wont_do)"),
        "Task should have wont_do outcome after --outcome wont_do close, got: {}",
        show_stdout
    );
}

#[test]
fn test_task_close_with_invalid_outcome() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add and start a task
    aiki_task(temp_dir.path(), &["add", "Task with invalid outcome"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();

    // Close with invalid --outcome should fail
    aiki_task(
        temp_dir.path(),
        &["close", "--outcome", "invalid", "--summary", "Bad outcome"],
    )
    .failure()
    .stderr(predicate::str::contains("Invalid outcome: 'invalid'"))
    .stderr(predicate::str::contains("done, wont_do"));
}

// ============================================================================
// Phase 2: Hierarchical Tasks Tests
// ============================================================================

#[test]
fn test_task_add_with_parent() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add parent task and get its ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Parent task"])
        .output()
        .expect("Failed to add parent task");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parent_id = extract_short_id(&stdout);

    // Add child task
    aiki_task(
        temp_dir.path(),
        &["add", "Child task", "--parent", &parent_id],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_subtasks_use_independent_ids() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add parent task
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Parent"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parent_id = extract_short_id(&stdout);

    // Add first child and verify the parent show output lists it as a subtask
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "First child", "--parent", &parent_id])
        .output()
        .unwrap();

    // Add second child
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Second child", "--parent", &parent_id])
        .output()
        .unwrap();

    // Verify subtasks exist via show on parent
    let show_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "show", &parent_id])
        .output()
        .unwrap();
    let show_stdout = String::from_utf8_lossy(&show_output.stdout);
    assert!(
        show_stdout.contains("First child"),
        "Should list first child subtask, got: {}",
        show_stdout
    );
    assert!(
        show_stdout.contains("Second child"),
        "Should list second child subtask, got: {}",
        show_stdout
    );
}

// ============================================================================
// Phase 3: Status Filters Tests
// ============================================================================

#[test]
fn test_task_list_all() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create tasks in different states
    aiki_task(temp_dir.path(), &["add", "Open task"]).success();
    aiki_task(temp_dir.path(), &["add", "To be closed"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Test completed"]).success();

    // --all should show all tasks including closed
    aiki_task(temp_dir.path(), &["list", "--all"])
        .success()
        .stdout(predicate::str::contains("Open task"))
        .stdout(predicate::str::contains("To be closed"));
}

#[test]
fn test_task_list_open_filter() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create two tasks - we'll close the first one and keep second open
    // Note: start without ID starts oldest task (first in queue)
    aiki_task(temp_dir.path(), &["add", "Task to close"]).success();
    aiki_task(temp_dir.path(), &["add", "Task to keep open"]).success();
    aiki_task(temp_dir.path(), &["start"]).success(); // Starts "Task to close" (oldest)
    aiki_task(temp_dir.path(), &["close", "--summary", "Test completed"]).success(); // Closes "Task to close"

    // --open should only show open tasks
    let output = aiki_task(temp_dir.path(), &["list", "--open"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);

    assert!(
        stdout.contains("Task to keep open"),
        "Should contain the task that's still open, got: {}",
        stdout
    );
}

#[test]
fn test_task_list_in_progress_filter() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create tasks
    aiki_task(temp_dir.path(), &["add", "Open task"]).success();
    aiki_task(temp_dir.path(), &["add", "In progress task"]).success();

    // Start the first task from ready queue
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_task_id = extract_id_from_list(&stdout);

    aiki_task(temp_dir.path(), &["start", &first_task_id]).success();

    // --in-progress should only show in-progress tasks
    aiki_task(temp_dir.path(), &["list", "--in-progress"])
        .success()
        .stdout(predicate::str::contains("Tasks (1):"));
}

#[test]
fn test_task_list_stopped_filter() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create and stop a task
    aiki_task(temp_dir.path(), &["add", "Stopped task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["stop", "--reason", "blocked"]).success();

    // --stopped should show stopped tasks
    aiki_task(temp_dir.path(), &["list", "--stopped"])
        .success()
        .stdout(predicate::str::contains("Stopped task"));
}

#[test]
fn test_task_list_closed_filter() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create and close a task
    aiki_task(temp_dir.path(), &["add", "Closed task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Test completed"]).success();

    // --closed should show closed tasks
    aiki_task(temp_dir.path(), &["list", "--closed"])
        .success()
        .stdout(predicate::str::contains("Closed task"));
}

#[test]
fn test_task_list_outcome_filters_regression() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let done_add = task_stdout(temp_dir.path(), &["add", "Done task"]);
    let done_id = extract_short_id(&done_add);
    aiki_task(temp_dir.path(), &["start", &done_id]).success();
    aiki_task(
        temp_dir.path(),
        &["close", &done_id, "--summary", "Done summary"],
    )
    .success();

    let wont_do_add = task_stdout(temp_dir.path(), &["add", "Won't do task"]);
    let wont_do_id = extract_short_id(&wont_do_add);
    aiki_task(temp_dir.path(), &["start", &wont_do_id]).success();
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &wont_do_id,
            "--wont-do",
            "--summary",
            "Won't do summary",
        ],
    )
    .success();

    aiki_task(temp_dir.path(), &["add", "Open task"]).success();

    let done_stdout = task_stdout(temp_dir.path(), &["list", "--done", "--all"]);
    let done_tasks = tasks_section(&done_stdout);
    assert!(done_tasks.contains("Tasks (1):"), "{}", done_stdout);
    assert!(done_tasks.contains("Done task"), "{}", done_stdout);
    assert!(done_tasks.contains("Done summary"), "{}", done_stdout);
    assert!(!done_tasks.contains("Won't do task"), "{}", done_stdout);
    assert!(!done_tasks.contains("Open task"), "{}", done_stdout);

    let wont_do_stdout = task_stdout(temp_dir.path(), &["list", "--wont-do", "--all"]);
    let wont_do_tasks = tasks_section(&wont_do_stdout);
    assert!(wont_do_tasks.contains("Tasks (1):"), "{}", wont_do_stdout);
    assert!(
        wont_do_tasks.contains("Won't do task"),
        "{}",
        wont_do_stdout
    );
    assert!(
        wont_do_tasks.contains("Won't do summary"),
        "{}",
        wont_do_stdout
    );
    assert!(!wont_do_tasks.contains("Done task"), "{}", wont_do_stdout);
    assert!(!wont_do_tasks.contains("Open task"), "{}", wont_do_stdout);

    let both_stdout = task_stdout(temp_dir.path(), &["list", "--done", "--wont-do", "--all"]);
    let both_tasks = tasks_section(&both_stdout);
    assert!(both_tasks.contains("Tasks (2):"), "{}", both_stdout);
    assert!(both_tasks.contains("Done task"), "{}", both_stdout);
    assert!(both_tasks.contains("Won't do task"), "{}", both_stdout);
    assert!(both_tasks.contains("Done summary"), "{}", both_stdout);
    assert!(both_tasks.contains("Won't do summary"), "{}", both_stdout);
    assert!(!both_tasks.contains("Open task"), "{}", both_stdout);
}

#[test]
fn test_task_list_status_done_matches_done_filter() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let done_add = task_stdout(temp_dir.path(), &["add", "Status done task"]);
    let done_id = extract_short_id(&done_add);
    aiki_task(temp_dir.path(), &["start", &done_id]).success();
    aiki_task(
        temp_dir.path(),
        &["close", &done_id, "--summary", "Closed via done"],
    )
    .success();

    let wont_do_add = task_stdout(temp_dir.path(), &["add", "Status won't do task"]);
    let wont_do_id = extract_short_id(&wont_do_add);
    aiki_task(temp_dir.path(), &["start", &wont_do_id]).success();
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &wont_do_id,
            "--wont-do",
            "--summary",
            "Closed via won't do",
        ],
    )
    .success();

    let stdout = task_stdout(temp_dir.path(), &["list", "--status", "done", "--all"]);
    let done_tasks = tasks_section(&stdout);
    assert!(done_tasks.contains("Tasks (1):"), "{}", stdout);
    assert!(done_tasks.contains("Status done task"), "{}", stdout);
    assert!(done_tasks.contains("Closed via done"), "{}", stdout);
    assert!(!done_tasks.contains("Status won't do task"), "{}", stdout);
}

#[test]
fn test_task_list_descendant_of_accepts_full_and_short_ids() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let parent_add = task_stdout(temp_dir.path(), &["add", "Parent task"]);
    let parent_short_id = extract_short_id(&parent_add);
    let parent_show = task_stdout(temp_dir.path(), &["show", &parent_short_id]);
    let parent_full_id = extract_full_id_from_show(&parent_show);

    let child_add = task_stdout(
        temp_dir.path(),
        &["add", "Child task", "--parent", &parent_short_id],
    );
    let child_short_id = extract_short_id(&child_add);

    task_stdout(
        temp_dir.path(),
        &["add", "Grandchild task", "--parent", &child_short_id],
    );
    task_stdout(
        temp_dir.path(),
        &["add", "Sibling task", "--parent", &parent_short_id],
    );
    task_stdout(temp_dir.path(), &["add", "Unrelated task"]);

    let full_stdout = task_stdout(
        temp_dir.path(),
        &["list", "--all", "--descendant-of", &parent_full_id],
    );
    let full_tasks = tasks_section(&full_stdout);
    assert!(full_tasks.contains("Tasks (3):"), "{}", full_stdout);
    assert!(full_tasks.contains("Child task"), "{}", full_stdout);
    assert!(full_tasks.contains("Grandchild task"), "{}", full_stdout);
    assert!(full_tasks.contains("Sibling task"), "{}", full_stdout);
    assert!(!full_tasks.contains("Parent task"), "{}", full_stdout);
    assert!(!full_tasks.contains("Unrelated task"), "{}", full_stdout);

    let short_stdout = task_stdout(
        temp_dir.path(),
        &["list", "--all", "--descendant-of", &parent_short_id],
    );
    let short_tasks = tasks_section(&short_stdout);
    assert!(short_tasks.contains("Tasks (3):"), "{}", short_stdout);
    assert!(short_tasks.contains("Child task"), "{}", short_stdout);
    assert!(short_tasks.contains("Grandchild task"), "{}", short_stdout);
    assert!(short_tasks.contains("Sibling task"), "{}", short_stdout);
    assert!(!short_tasks.contains("Parent task"), "{}", short_stdout);
    assert!(!short_tasks.contains("Unrelated task"), "{}", short_stdout);
}

#[test]
fn test_task_list_descendant_of_unknown_ancestor_errors() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    aiki_task(
        temp_dir.path(),
        &["list", "--all", "--descendant-of", "zzznotarealtaskid"],
    )
    .failure()
    .stderr(predicate::str::contains("Task not found"));
}

// ============================================================================
// Phase 3: Multiple Blocked Flags Tests
// ============================================================================

#[test]
fn test_task_stop_with_blocked() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create and start a task
    aiki_task(temp_dir.path(), &["add", "Task with blocker"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();

    // Stop with --blocked creates a blocker task
    aiki_task(
        temp_dir.path(),
        &["stop", "--blocked", "Need API credentials"],
    )
    .success()
    .stdout(predicate::str::contains("Stopped"));

    // The blocker task should appear in list (use --all to bypass session-based filtering)
    aiki_task(temp_dir.path(), &["list", "--all"])
        .success()
        .stdout(predicate::str::contains("Need API credentials"));
}

#[test]
fn test_task_stop_with_multiple_blocked() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create and start a task
    aiki_task(temp_dir.path(), &["add", "Complex blocker task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();

    // Stop with multiple --blocked flags
    aiki_task(
        temp_dir.path(),
        &[
            "stop",
            "--blocked",
            "Need API credentials",
            "--blocked",
            "Need design review",
        ],
    )
    .success();

    // Both blocker tasks should appear in list (use --all to bypass session-based filtering)
    aiki_task(temp_dir.path(), &["list", "--all"])
        .success()
        .stdout(predicate::str::contains("Need API credentials"))
        .stdout(predicate::str::contains("Need design review"));
}

// ============================================================================
// Phase 4: Show, Update, Comment, Reopen Tests
// ============================================================================

#[test]
fn test_task_show_basic() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a task
    let add_output = aiki_task(temp_dir.path(), &["add", "Task to show"]).success();
    let add_stdout = String::from_utf8_lossy(&add_output.get_output().stdout);
    let task_id = extract_short_id(&add_stdout);

    // Show the task
    aiki_task(temp_dir.path(), &["show", &task_id])
        .success()
        .stdout(predicate::str::contains("Task: Task to show"));
}

#[test]
fn test_task_show_current() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create and start a task
    aiki_task(temp_dir.path(), &["add", "Current task"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();

    // Show without ID shows current task
    aiki_task(temp_dir.path(), &["show"])
        .success()
        .stdout(predicate::str::contains("Task: Current task"));
}

#[test]
fn test_task_set_name() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a task and extract short ID from "Added <id>" output
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Original name"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_short_id(&stdout);

    // Set the name
    aiki_task(
        temp_dir.path(),
        &["set", &task_id, "--name", "Updated name"],
    )
    .success()
    .stdout(predicate::str::contains("Updated name"));

    // Verify the name changed
    aiki_task(temp_dir.path(), &["show", &task_id])
        .success()
        .stdout(predicate::str::contains("Updated name"));
}

#[test]
fn test_task_set_priority() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a task (default P2) and extract short ID
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Priority task"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_short_id(&stdout);

    // Set to P0
    aiki_task(temp_dir.path(), &["set", &task_id, "--p0"]).success();

    // Verify the priority changed
    aiki_task(temp_dir.path(), &["show", &task_id])
        .success()
        .stdout(predicate::str::contains("p0"));
}

#[test]
fn test_task_unset_assignee() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a task with assignee
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Assigned task", "--assignee", "claude-code"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_short_id(&stdout);

    // Unset assignee — should produce confirmation with "Cleared" and field name
    aiki_task(temp_dir.path(), &["unset", &task_id, "--assignee"])
        .success()
        .stdout(predicate::str::contains("Cleared"))
        .stdout(predicate::str::contains("assignee"));
}

#[test]
fn test_task_unset_rejects_name() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "My task"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_short_id(&stdout);

    // Attempt to unset name — should fail (no --name flag exists)
    aiki_task(temp_dir.path(), &["unset", &task_id])
        .success()
        .stdout(predicate::str::contains("No fields specified"));
}

#[test]
fn test_task_unset_rejects_priority() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "My task"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_short_id(&stdout);

    // Attempt to unset priority — should fail (no --priority flag exists)
    aiki_task(temp_dir.path(), &["unset", &task_id])
        .success()
        .stdout(predicate::str::contains("No fields specified"));
}

#[test]
fn test_task_unset_rejects_unknown_field() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "My task"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_short_id(&stdout);

    // Attempt to unset with unknown flag — should fail at CLI parsing level
    aiki_task(temp_dir.path(), &["unset", &task_id, "--foobar"]).failure(); // Clap will reject unknown flags
}

#[test]
fn test_task_set_rejects_empty_data_value() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "My task"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let task_id = extract_short_id(&stdout);

    // Start the task first
    aiki_task(temp_dir.path(), &["start", &task_id]).success();

    // Attempt to set data with empty value
    aiki_task(temp_dir.path(), &["set", &task_id, "--data", "key="])
        .success()
        .stdout(predicate::str::contains("aiki task unset"));
}

#[test]
fn test_task_comment() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a task
    let add_output = aiki_task(temp_dir.path(), &["add", "Task with comment"]).success();
    let add_stdout = String::from_utf8_lossy(&add_output.get_output().stdout);
    let task_id = extract_short_id(&add_stdout);

    // Add a comment (comment add <ID> <TEXT>)
    aiki_task(
        temp_dir.path(),
        &["comment", "add", &task_id, "This is a test comment"],
    )
    .success()
    .stdout(predicate::str::contains("Comment added."));
}

#[test]
fn test_task_comment_with_data() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a task
    let add_output = aiki_task(temp_dir.path(), &["add", "Task with structured comment"]).success();
    let add_stdout = String::from_utf8_lossy(&add_output.get_output().stdout);
    let task_id = extract_short_id(&add_stdout);

    // Add a comment with structured data
    aiki_task(
        temp_dir.path(),
        &[
            "comment",
            "add",
            &task_id,
            "Potential null pointer dereference",
            "--data",
            "file=src/auth.ts",
            "--data",
            "line=42",
            "--data",
            "severity=error",
        ],
    )
    .success()
    .stdout(predicate::str::contains("Comment added."));

    // Verify task show displays the comment
    aiki_task(temp_dir.path(), &["show", &task_id])
        .success()
        .stdout(predicate::str::contains(
            "Potential null pointer dereference",
        ));

    // Verify the data fields are persisted in jj task events
    // Read ALL events on the aiki/tasks branch (bookmark points to the tip)
    let output = Command::new("jj")
        .current_dir(temp_dir.path())
        .args([
            "log",
            "-r",
            "aiki/tasks",
            "--no-graph",
            "-T",
            "description",
            "--ignore-working-copy",
        ])
        .output()
        .expect("Failed to run jj log");

    let contents = String::from_utf8_lossy(&output.stdout);

    // Check that all data fields are stored in the event
    assert!(
        contents.contains("data=file:src/auth.ts"),
        "Should contain file data field, got: {}",
        contents
    );
    assert!(
        contents.contains("data=line:42"),
        "Should contain line data field, got: {}",
        contents
    );
    assert!(
        contents.contains("data=severity:error"),
        "Should contain severity data field, got: {}",
        contents
    );
}

#[test]
fn test_task_start_reopen() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create, start, and close a task
    let add_output = aiki_task(temp_dir.path(), &["add", "Task to reopen"]).success();
    let add_stdout = String::from_utf8_lossy(&add_output.get_output().stdout);
    let task_id = extract_short_id(&add_stdout);

    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Test completed"]).success();

    // Reopen and start the task
    aiki_task(
        temp_dir.path(),
        &[
            "start",
            &task_id,
            "--reopen",
            "--reason",
            "Found another bug",
        ],
    )
    .success()
    .stdout(predicate::str::contains("Started"))
    .stdout(predicate::str::contains("Task to reopen"));
}

// ============================================================================
// Workflow Tests: Auto-stop on Start
// ============================================================================

#[test]
fn test_task_start_does_not_stop_other_tasks() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create two tasks
    aiki_task(temp_dir.path(), &["add", "First task"]).success();
    aiki_task(temp_dir.path(), &["add", "Second task"]).success();

    // Start first task
    aiki_task(temp_dir.path(), &["start"]).success();

    // Get second task ID from list (in ready queue)
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let second_task_id = extract_id_from_list_by_name(&stdout, "Second task");

    // Start second task - should NOT auto-stop first (no stopped output)
    aiki_task(temp_dir.path(), &["start", &second_task_id])
        .success()
        .stdout(predicate::str::contains("Stopped").not());

    // Verify both tasks are now in progress
    let list_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--in-progress"])
        .output()
        .unwrap();
    let list_stdout = String::from_utf8_lossy(&list_output.stdout);
    assert!(
        list_stdout.contains("First task") && list_stdout.contains("Second task"),
        "Both tasks should be in progress, got: {}",
        list_stdout
    );
}

// ============================================================================
// Workflow Tests: Context Contract
// ============================================================================

#[test]
fn test_context_always_shows_ready_queue() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create tasks
    aiki_task(temp_dir.path(), &["add", "Task 1"]).success();
    aiki_task(temp_dir.path(), &["add", "Task 2"]).success();

    // Every command should have context with ready queue
    for cmd in [vec!["list"], vec!["list", "--all"], vec!["list", "--open"]] {
        let output = aiki_task(temp_dir.path(), &cmd).success();
        let stdout = String::from_utf8_lossy(&output.get_output().stdout);
        assert!(
            stdout.contains("Ready ("),
            "Command {:?} should have ready queue count, got: {}",
            cmd,
            stdout
        );
    }
}

#[test]
fn test_list_filter_preserves_context_ready_queue() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create open and closed tasks
    aiki_task(temp_dir.path(), &["add", "Open task"]).success();
    aiki_task(temp_dir.path(), &["add", "To close"]).success();
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Test completed"]).success();

    // When filtering by --closed, the context should still show actual ready queue
    let output = aiki_task(temp_dir.path(), &["list", "--closed"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);

    // Context should show ready count
    assert!(
        stdout.contains("Ready ("),
        "Context should show ready count, got: {}",
        stdout
    );
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_task_start_nonexistent() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Quick-start feature: if input isn't a task ID, it's treated as a description
    // and a new task is created with that name
    aiki_task(temp_dir.path(), &["start", "nonexistent"])
        .success()
        .stdout(predicate::str::contains("Started"));
}

#[test]
fn test_task_close_nonexistent() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    aiki_task(temp_dir.path(), &["close", "nonexistent"])
        .failure()
        .stderr(predicate::str::contains("Task not found"));
}

#[test]
fn test_task_stop_when_none_in_progress() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add task but don't start
    aiki_task(temp_dir.path(), &["add", "Not started"]).success();

    // Error cases return exit code 0 but with error output
    aiki_task(temp_dir.path(), &["stop"])
        .success()
        .stdout(predicate::str::contains("Error:"))
        .stdout(predicate::str::contains("No task in progress to stop"));
}

#[test]
fn test_task_close_when_none_in_progress() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Add task but don't start
    aiki_task(temp_dir.path(), &["add", "Not started"]).success();

    // Note: Error cases return exit code 0 but with XML error output
    aiki_task(temp_dir.path(), &["close", "--summary", "Test completed"])
        .success()
        .stdout(predicate::str::contains("Error:"))
        .stdout(predicate::str::contains("No task in progress to close"));
}

#[test]
fn test_task_add_invalid_parent() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Error for invalid parent is returned on stderr with exit code 1
    aiki_task(
        temp_dir.path(),
        &["add", "Orphan child", "--parent", "nonexistent"],
    )
    .failure()
    .stderr(predicate::str::contains("Task not found"));
}

#[test]
fn test_parent_auto_starts_when_all_subtasks_closed() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create parent task
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Parent task"])
        .output()
        .expect("Failed to add parent task");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parent_id = extract_short_id(&stdout);

    // Create two subtasks (they get full IDs, linked via subtask-of edges)
    let output1 = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Subtask 1", "--parent", &parent_id])
        .output()
        .expect("Failed to add subtask 1");
    let stdout1 = String::from_utf8_lossy(&output1.stdout);
    let subtask1_id = extract_short_id(&stdout1);

    let output2 = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Subtask 2", "--parent", &parent_id])
        .output()
        .expect("Failed to add subtask 2");
    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    let subtask2_id = extract_short_id(&stdout2);

    // Start and close subtask 1
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &subtask1_id])
        .output()
        .expect("Failed to start subtask 1");

    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "close", &subtask1_id, "--summary", "Done"])
        .output()
        .expect("Failed to close subtask 1");

    // Start and close subtask 2 - this should trigger parent auto-start
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &subtask2_id])
        .output()
        .expect("Failed to start subtask 2");

    // Close subtask 2 - this should trigger parent auto-start
    let close_stdout = task_stdout(
        temp_dir.path(),
        &["close", &subtask2_id, "--summary", "All done"],
    );

    // Verify the close output reports the parent auto-start directly.
    assert!(
        close_stdout.contains("Started") && close_stdout.contains("Parent task"),
        "Parent should auto-start when all subtasks closed. Output: {}",
        close_stdout
    );
}

// ============================================================================
// Declarative Subtasks (Template with subtasks: source.comments)
// ============================================================================

/// Helper to create a template file for testing
fn create_template(templates_dir: &std::path::Path, namespace: &str, name: &str, content: &str) {
    let ns_dir = templates_dir.join(namespace);
    std::fs::create_dir_all(&ns_dir).expect("Failed to create namespace directory");
    let file_path = ns_dir.join(format!("{}.md", name));
    std::fs::write(&file_path, content).expect("Failed to write template file");
}

#[test]
fn test_template_add_creates_static_subtasks() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a template with static subtasks
    let templates_dir = temp_dir.path().join(".aiki/tasks");
    create_template(
        &templates_dir,
        "test",
        "static-review",
        r#"---
version: 1.0.0
description: Review with static subtasks
---

# Review: {{data.scope}}

Review the code in {{data.scope}}.

# Subtasks

## Analyze code

Look at the code structure.

## Write summary

Document your findings.
"#,
    );

    // Create task from template
    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "--template",
            "test/static-review",
            "--data",
            "scope=src/auth.rs",
        ],
    )
    .success()
    .stdout(predicate::str::contains("Added"));

    // List should show the parent and subtasks
    aiki_task(temp_dir.path(), &["list", "--all"])
        .success()
        .stdout(predicate::str::contains("Review: src/auth.rs"))
        .stdout(predicate::str::contains("Analyze code"))
        .stdout(predicate::str::contains("Write summary"));
}

#[test]
fn test_template_add_with_dynamic_subtasks() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a template with dynamic subtasks using inline loops
    let templates_dir = temp_dir.path().join(".aiki/tasks");
    create_template(
        &templates_dir,
        "test",
        "followup",
        r#"---
version: 1.0.0
description: Followup with dynamic subtasks from comments
---

# Followup: {{data.scope}}

Fix all issues identified in the review.

# Subtasks

{% for item in source.comments %}
## Fix: {{item.file}}

{{item.text}}
{% endfor %}
"#,
    );

    // Create a source task
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Code review"])
        .output()
        .expect("Failed to add source task");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let source_task_id = extract_short_id(&stdout);

    // Add comments to the source task with structured data
    // Comment 1: file=auth.rs
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args([
            "task",
            "comment",
            "add",
            &source_task_id,
            "Missing null check",
            "--data",
            "file=auth.rs",
        ])
        .output()
        .expect("Failed to add comment 1");

    // Comment 2: file=utils.rs
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args([
            "task",
            "comment",
            "add",
            &source_task_id,
            "Unused import",
            "--data",
            "file=utils.rs",
        ])
        .output()
        .expect("Failed to add comment 2");

    // Create task from template with --source task:<id>
    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "--template",
            "test/followup",
            "--data",
            "scope=auth-module",
            "--source",
            &format!("task:{}", source_task_id),
        ],
    )
    .success()
    .stdout(predicate::str::contains("Added"));

    // List should show the parent and dynamically created subtasks
    let output = aiki_task(temp_dir.path(), &["list", "--all"])
        .success()
        .get_output()
        .stdout
        .clone();
    let list_output = String::from_utf8_lossy(&output);

    assert!(
        list_output.contains("Followup: auth-module"),
        "Should have parent task"
    );
    assert!(
        list_output.contains("Fix: auth.rs"),
        "Should have subtask for auth.rs"
    );
    assert!(
        list_output.contains("Fix: utils.rs"),
        "Should have subtask for utils.rs"
    );
}

#[test]
fn test_template_dynamic_subtasks_requires_source() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a template with dynamic subtasks using inline loops
    let templates_dir = temp_dir.path().join(".aiki/tasks");
    create_template(
        &templates_dir,
        "test",
        "dynamic",
        r#"---
version: 1.0.0
---

# Task

Do work.

# Subtasks

{% for item in source.comments %}
## Fix: {{item.text}}

{{item.text}}
{% endfor %}
"#,
    );

    // Creating without --source task:<id> should succeed but have no subtasks
    // (inline loops with no data source produce no subtasks)
    aiki_task(temp_dir.path(), &["add", "--template", "test/dynamic"])
        .success()
        .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_template_unknown_data_source_fails() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a template with unknown data source using inline loop
    let templates_dir = temp_dir.path().join(".aiki/tasks");
    create_template(
        &templates_dir,
        "test",
        "unknown-source",
        r#"---
version: 1.0.0
---

# Task

Do work.

# Subtasks

{% for item in source.unknown_source %}
## Fix

{{item.text}}
{% endfor %}
"#,
    );

    // Create a source task
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Source task"])
        .output()
        .expect("Failed to add source task");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let source_task_id = extract_short_id(&stdout);

    // Creating with unknown data source should fail
    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "--template",
            "test/unknown-source",
            "--source",
            &format!("task:{}", source_task_id),
        ],
    )
    .failure()
    .stderr(predicate::str::contains("Unknown"));
}

#[test]
fn test_template_static_subtasks_honor_frontmatter_sources() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a source task to reference
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Original review"])
        .output()
        .expect("Failed to add source task");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let source_task_id = extract_short_id(&stdout);

    // Create a template with static subtasks that have frontmatter sources
    let templates_dir = temp_dir.path().join(".aiki/tasks");
    create_template(
        &templates_dir,
        "test",
        "followup-static",
        &format!(
            r#"---
version: 1.0.0
description: Followup with static subtasks that have sources
---

# Followup: {{{{data.scope}}}}

Fix issues identified in the review.

# Subtasks

## Fix auth issue
---
sources:
  - task:{}
---

Fix the authentication bug.

## Fix validation issue
---
sources:
  - task:{}
---

Fix the validation issue.
"#,
            source_task_id, source_task_id
        ),
    );

    // Create task from template
    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "--template",
            "test/followup-static",
            "--data",
            "scope=auth-module",
        ],
    )
    .success();

    // Show the subtasks to verify sources are included
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "list", "--all"])
        .output()
        .expect("Failed to list tasks");
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check that subtasks exist
    assert!(
        stdout.contains("Fix auth issue"),
        "Should have subtask 'Fix auth issue', got: {}",
        stdout
    );

    // Find the subtask ID from the list output
    let subtask_id = extract_id_from_list_by_name(&stdout, "Fix auth issue");

    // Show the subtask to verify it has the source
    let show_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "show", &subtask_id])
        .output()
        .expect("Failed to show subtask");
    let show_stdout = String::from_utf8_lossy(&show_output.stdout);

    // The subtask should have the source from frontmatter
    // Format: "- Source: task:<id>"
    assert!(
        show_stdout.contains(&format!("task:{}", source_task_id)),
        "Subtask should have source from frontmatter. Output: {}",
        show_stdout
    );
}

// ============================================================================
// Link Flag Tests
// ============================================================================

#[test]
fn test_task_add_with_blocked_by() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create blocker task
    let output = aiki_task(temp_dir.path(), &["add", "Blocker task"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let blocker_id = extract_short_id(&stdout);

    // Create task blocked by the first
    aiki_task(
        temp_dir.path(),
        &["add", "Blocked task", "--blocked-by", &blocker_id],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_with_multiple_blocked_by() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create two blocker tasks
    let output1 = aiki_task(temp_dir.path(), &["add", "Blocker 1"]).success();
    let stdout1 = String::from_utf8_lossy(&output1.get_output().stdout);
    let blocker1_id = extract_short_id(&stdout1);

    let output2 = aiki_task(temp_dir.path(), &["add", "Blocker 2"]).success();
    let stdout2 = String::from_utf8_lossy(&output2.get_output().stdout);
    let blocker2_id = extract_short_id(&stdout2);

    // Create task blocked by both
    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Doubly blocked",
            "--blocked-by",
            &blocker1_id,
            "--blocked-by",
            &blocker2_id,
        ],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_with_supersedes() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create old task
    let output = aiki_task(temp_dir.path(), &["add", "Old approach"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let old_id = extract_short_id(&stdout);

    // Create replacement
    aiki_task(
        temp_dir.path(),
        &["add", "New approach", "--supersedes", &old_id],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_with_subtask_of() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create parent task
    let output = aiki_task(temp_dir.path(), &["add", "Parent task"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let parent_id = extract_short_id(&stdout);

    // Create subtask using --subtask-of
    aiki_task(
        temp_dir.path(),
        &["add", "Child task", "--subtask-of", &parent_id],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_with_sourced_from() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    aiki_task(
        temp_dir.path(),
        &["add", "Fix bug", "--sourced-from", "file:ops/now/design.md"],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_with_multiple_sourced_from() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Fix bug",
            "--sourced-from",
            "file:ops/now/design.md",
            "--sourced-from",
            "file:ops/now/review.md",
        ],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_parent_alias_works() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create parent
    let output = aiki_task(temp_dir.path(), &["add", "Parent task"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let parent_id = extract_short_id(&stdout);

    // --parent is hidden alias for --subtask-of
    aiki_task(
        temp_dir.path(),
        &["add", "Child via parent", "--parent", &parent_id],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_source_alias_works() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // --source is hidden alias for --sourced-from
    aiki_task(
        temp_dir.path(),
        &["add", "Fix bug", "--source", "file:design.md"],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_both_subtask_of_and_parent_errors() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = aiki_task(temp_dir.path(), &["add", "Parent task"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let parent_id = extract_short_id(&stdout);

    // Both --subtask-of and --parent should error
    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Child",
            "--subtask-of",
            &parent_id,
            "--parent",
            &parent_id,
        ],
    )
    .failure();
}

#[test]
fn test_task_add_both_sourced_from_and_source_errors() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Both --sourced-from and --source should error
    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Fix bug",
            "--sourced-from",
            "file:a.md",
            "--source",
            "file:b.md",
        ],
    )
    .failure();
}

#[test]
fn test_task_add_with_multiple_link_kinds() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create blocker task
    let output = aiki_task(temp_dir.path(), &["add", "Blocker"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let blocker_id = extract_short_id(&stdout);

    // Create task with both --blocked-by and --sourced-from
    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Complex task",
            "--blocked-by",
            &blocker_id,
            "--sourced-from",
            "file:ops/now/design.md",
        ],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_link_with_source_alias() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = aiki_task(temp_dir.path(), &["add", "A task"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let task_id = extract_short_id(&stdout);

    // --source is hidden alias for --sourced-from on link
    aiki_task(
        temp_dir.path(),
        &["link", &task_id, "--source", "file:design.md"],
    )
    .success();
}

#[test]
fn test_task_link_with_parent_alias() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let out1 = aiki_task(temp_dir.path(), &["add", "Parent"]).success();
    let stdout1 = String::from_utf8_lossy(&out1.get_output().stdout);
    let parent_id = extract_short_id(&stdout1);

    let out2 = aiki_task(temp_dir.path(), &["add", "Child"]).success();
    let stdout2 = String::from_utf8_lossy(&out2.get_output().stdout);
    let child_id = extract_short_id(&stdout2);

    // --parent is hidden alias for --subtask-of on link
    aiki_task(
        temp_dir.path(),
        &["link", &child_id, "--parent", &parent_id],
    )
    .success();
}

#[test]
fn test_task_link_both_sourced_from_and_source_errors() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = aiki_task(temp_dir.path(), &["add", "A task"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let task_id = extract_short_id(&stdout);

    aiki_task(
        temp_dir.path(),
        &[
            "link",
            &task_id,
            "--sourced-from",
            "file:a.md",
            "--source",
            "file:b.md",
        ],
    )
    .failure();
}

#[test]
fn test_task_unlink_with_source_alias() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create task with source
    let output = aiki_task(
        temp_dir.path(),
        &["add", "Task with source", "--source", "file:design.md"],
    )
    .success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let task_id = extract_short_id(&stdout);

    // Unlink using --source alias
    aiki_task(
        temp_dir.path(),
        &["unlink", &task_id, "--source", "file:design.md"],
    )
    .success();
}

// ── Complete link flags tests ──────────────────────────────────────────

#[test]
fn test_task_add_with_implements() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Plan: Auth system",
            "--implements",
            "file:ops/now/auth-plan.md",
        ],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_with_orchestrates() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a plan task to orchestrate
    let output = aiki_task(temp_dir.path(), &["add", "Plan task"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let plan_id = extract_short_id(&stdout);

    aiki_task(
        temp_dir.path(),
        &["add", "Build: Auth", "--orchestrates", &plan_id],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

// Note: --scoped-to is not yet implemented in the CLI, so those tests are skipped

#[test]
fn test_task_add_with_depends_on() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create dependency task
    let output = aiki_task(temp_dir.path(), &["add", "Unit tests"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let dep_id = extract_short_id(&stdout);

    aiki_task(
        temp_dir.path(),
        &["add", "Integration tests", "--depends-on", &dep_id],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_with_multiple_depends_on() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output1 = aiki_task(temp_dir.path(), &["add", "Dep 1"]).success();
    let stdout1 = String::from_utf8_lossy(&output1.get_output().stdout);
    let dep1_id = extract_short_id(&stdout1);

    let output2 = aiki_task(temp_dir.path(), &["add", "Dep 2"]).success();
    let stdout2 = String::from_utf8_lossy(&output2.get_output().stdout);
    let dep2_id = extract_short_id(&stdout2);

    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Final task",
            "--depends-on",
            &dep1_id,
            "--depends-on",
            &dep2_id,
        ],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_add_with_all_new_link_types() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create tasks for link targets
    let output1 = aiki_task(temp_dir.path(), &["add", "Blocker"]).success();
    let stdout1 = String::from_utf8_lossy(&output1.get_output().stdout);
    let blocker_id = extract_short_id(&stdout1);

    let output2 = aiki_task(temp_dir.path(), &["add", "Dependency"]).success();
    let stdout2 = String::from_utf8_lossy(&output2.get_output().stdout);
    let dep_id = extract_short_id(&stdout2);

    // Create task with multiple link kinds at once (scoped-to removed - not yet in CLI)
    aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Complex task",
            "--blocked-by",
            &blocker_id,
            "--depends-on",
            &dep_id,
            "--implements",
            "file:ops/now/plan.md",
        ],
    )
    .success()
    .stdout(predicate::str::contains("Added"));
}

#[test]
fn test_task_start_quickstart_with_implements() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    aiki_task(
        temp_dir.path(),
        &[
            "start",
            "Plan: Auth",
            "--implements",
            "file:ops/now/auth-plan.md",
        ],
    )
    .success()
    .stdout(predicate::str::contains("Started"));
}

#[test]
fn test_task_start_quickstart_with_depends_on() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = aiki_task(temp_dir.path(), &["add", "Prerequisite"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let dep_id = extract_short_id(&stdout);

    aiki_task(
        temp_dir.path(),
        &["start", "Dependent task", "--depends-on", &dep_id],
    )
    .success()
    .stdout(predicate::str::contains("Started"));
}

// Note: --scoped-to is not yet implemented in the CLI, so test_task_start_quickstart_with_scoped_to is skipped

#[test]
fn test_task_start_existing_with_new_link_flags() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create dep task
    let output1 = aiki_task(temp_dir.path(), &["add", "Dep task"]).success();
    let stdout1 = String::from_utf8_lossy(&output1.get_output().stdout);
    let dep_id = extract_short_id(&stdout1);

    // Create task to start
    let output2 = aiki_task(temp_dir.path(), &["add", "Main task"]).success();
    let stdout2 = String::from_utf8_lossy(&output2.get_output().stdout);
    let task_id = extract_short_id(&stdout2);

    // Start with link flags (scoped-to removed - not yet in CLI)
    aiki_task(
        temp_dir.path(),
        &["start", &task_id, "--depends-on", &dep_id],
    )
    .success()
    .stdout(predicate::str::contains("Started"));
}

#[test]
fn test_task_link_with_depends_on() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output1 = aiki_task(temp_dir.path(), &["add", "Task A"]).success();
    let stdout1 = String::from_utf8_lossy(&output1.get_output().stdout);
    let task_a_id = extract_short_id(&stdout1);

    let output2 = aiki_task(temp_dir.path(), &["add", "Task B"]).success();
    let stdout2 = String::from_utf8_lossy(&output2.get_output().stdout);
    let task_b_id = extract_short_id(&stdout2);

    // Link B depends-on A
    aiki_task(
        temp_dir.path(),
        &["link", &task_b_id, "--depends-on", &task_a_id],
    )
    .success();
}

#[test]
fn test_task_unlink_with_depends_on() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output1 = aiki_task(temp_dir.path(), &["add", "Task A"]).success();
    let stdout1 = String::from_utf8_lossy(&output1.get_output().stdout);
    let task_a_id = extract_short_id(&stdout1);

    let output2 = aiki_task(temp_dir.path(), &["add", "Task B"]).success();
    let stdout2 = String::from_utf8_lossy(&output2.get_output().stdout);
    let task_b_id = extract_short_id(&stdout2);

    // Link then unlink
    aiki_task(
        temp_dir.path(),
        &["link", &task_b_id, "--depends-on", &task_a_id],
    )
    .success();

    aiki_task(
        temp_dir.path(),
        &["unlink", &task_b_id, "--depends-on", &task_a_id],
    )
    .success();
}

#[test]
fn test_add_output_id_returns_bare_task_id() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = aiki_task(
        temp_dir.path(),
        &["add", "Test output id", "--output", "id"],
    )
    .success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let id = stdout.trim();

    // Should be exactly a 32-char lowercase alpha string
    assert_eq!(
        id.len(),
        32,
        "Expected 32-char ID, got '{}' (len={})",
        id,
        id.len()
    );
    assert!(
        id.chars().all(|c| c.is_ascii_lowercase()),
        "Expected all lowercase letters, got '{}'",
        id
    );
    // Should NOT contain "Added:" prefix
    assert!(
        !stdout.contains("Added"),
        "Output should be bare ID, got '{}'",
        stdout
    );
}

#[test]
fn test_dedup_guard_prevents_duplicate_subtask() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create parent task
    let output = aiki_task(temp_dir.path(), &["add", "Dedup parent", "--output", "id"]).success();
    let parent_id = String::from_utf8_lossy(&output.get_output().stdout)
        .trim()
        .to_string();

    // Add subtask with name "Same name"
    let output1 = aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Same name",
            "--subtask-of",
            &parent_id,
            "--output",
            "id",
        ],
    )
    .success();
    let id1 = String::from_utf8_lossy(&output1.get_output().stdout)
        .trim()
        .to_string();

    // Add subtask with same name again — should return existing ID
    let output2 = aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Same name",
            "--subtask-of",
            &parent_id,
            "--output",
            "id",
        ],
    )
    .success();
    let id2 = String::from_utf8_lossy(&output2.get_output().stdout)
        .trim()
        .to_string();

    // Same ID both times
    assert_eq!(
        id1, id2,
        "Dedup should return same ID, got '{}' vs '{}'",
        id1, id2
    );
}

#[test]
fn test_dedup_guard_allows_recreation_after_close() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create parent task
    let output = aiki_task(temp_dir.path(), &["add", "Reopen parent", "--output", "id"]).success();
    let parent_id = String::from_utf8_lossy(&output.get_output().stdout)
        .trim()
        .to_string();

    // Add subtask
    let output1 = aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Closeable",
            "--subtask-of",
            &parent_id,
            "--output",
            "id",
        ],
    )
    .success();
    let id1 = String::from_utf8_lossy(&output1.get_output().stdout)
        .trim()
        .to_string();

    // Start and close the subtask
    aiki_task(temp_dir.path(), &["start", &id1]).success();
    aiki_task(temp_dir.path(), &["close", &id1, "--summary", "done"]).success();

    // Add subtask with same name again — should create a NEW one (closed doesn't dedup)
    let output2 = aiki_task(
        temp_dir.path(),
        &[
            "add",
            "Closeable",
            "--subtask-of",
            &parent_id,
            "--output",
            "id",
        ],
    )
    .success();
    let id2 = String::from_utf8_lossy(&output2.get_output().stdout)
        .trim()
        .to_string();

    assert_ne!(
        id1, id2,
        "Should create new subtask after closing original, got same ID '{}'",
        id1
    );
}

/// Regression test: non-task-scoped reviews (plan/code) should succeed even
/// though `render_review_workflow` finds no `validates` edge and returns an
/// empty string.  This exercises the early-return path at review.rs:907-910.
#[test]
fn test_review_non_task_scope_succeeds() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create the review/plan template so the review command can resolve it
    let template_dir = temp_dir.path().join(".aiki/tasks/review");
    std::fs::create_dir_all(&template_dir).expect("Failed to create template dir");
    std::fs::write(
        template_dir.join("plan.md"),
        "---\nversion: 3.0.0\ntype: review\n---\n\n# Review: {{data.scope.name}}\n\nReview the plan.\n",
    )
    .expect("Failed to write plan template");

    // Commit the template so aiki can find it
    Command::new("git")
        .args(["add", ".aiki/tasks"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git add templates failed");
    Command::new("git")
        .args(["commit", "-m", "add review template"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit failed");

    // Create an .md file so the review target resolves as Plan scope
    std::fs::write(
        temp_dir.path().join("design.md"),
        "# Design\nSome plan content\n",
    )
    .expect("Failed to write design.md");

    // Commit the file so the repo has history
    Command::new("git")
        .args(["add", "design.md"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git add failed");
    Command::new("git")
        .args(["commit", "-m", "add design doc"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit failed");

    // Run a non-task review with --start (plan scope, no validates edge)
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["review", "design.md", "--start"])
        .output()
        .expect("Failed to run aiki review");

    assert!(
        output.status.success(),
        "aiki review --start on a plan-scoped file should succeed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Regression test: `aiki task show` with no task ID and no in-progress task
/// should produce an error message.
#[test]
fn test_show_without_id_returns_error() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // No tasks in progress — `aiki task show` with no ID should produce an error
    let output = aiki_task(temp_dir.path(), &["show"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);

    assert!(
        stdout.contains("No task ID provided"),
        "Expected 'No task ID provided' error, got: {}",
        stdout
    );
}

#[test]
fn test_show_includes_parent_short_id() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let parent_output = aiki_task(temp_dir.path(), &["add", "Parent task"]).success();
    let parent_stdout = String::from_utf8_lossy(&parent_output.get_output().stdout);
    let parent_id = extract_short_id(&parent_stdout);

    let child_output = aiki_task(
        temp_dir.path(),
        &["add", "Child task", "--subtask-of", &parent_id],
    )
    .success();
    let child_stdout = String::from_utf8_lossy(&child_output.get_output().stdout);
    let child_id = extract_short_id(&child_stdout);

    let output = aiki_task(temp_dir.path(), &["show", &child_id]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);

    assert!(
        stdout.contains(&format!("Parent: {} — Parent task", parent_id)),
        "Expected parent line in task show output, got: {}",
        stdout
    );
}

#[test]
fn test_show_output_summary_rejects_open_task() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a task (stays open/ready)
    let output = aiki_task(temp_dir.path(), &["add", "Test task"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let task_id = extract_short_id(&stdout);

    // --output summary on an open task should fail
    aiki_task(temp_dir.path(), &["show", &task_id, "--output", "summary"]).failure();
}

#[test]
fn test_show_output_summary_emits_summary_for_closed_task() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create and start a task
    let output = aiki_task(temp_dir.path(), &["add", "Test task"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let task_id = extract_short_id(&stdout);

    aiki_task(temp_dir.path(), &["start", &task_id]).success();

    // Close with a summary
    aiki_task(
        temp_dir.path(),
        &["close", &task_id, "--summary", "My test summary"],
    )
    .success();

    // --output summary on a closed task should succeed and print only the summary
    let output = aiki_task(temp_dir.path(), &["show", &task_id, "--output", "summary"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(
        stdout.contains("My test summary"),
        "Expected summary text in stdout, got: {}",
        stdout
    );
}

#[test]
fn test_show_output_summary_rejects_closed_task_without_summary() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a task
    let output = aiki_task(temp_dir.path(), &["add", "Test task"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    let task_id = extract_short_id(&stdout);

    // Close without a summary
    aiki_task(temp_dir.path(), &["close", &task_id]).success();

    // --output summary on a closed task without summary should fail
    aiki_task(temp_dir.path(), &["show", &task_id, "--output", "summary"])
        .failure()
        .stderr(predicate::str::contains("closed but has no summary"));
}

#[test]
fn test_run_requires_id_or_template() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Either task ID or --template must be provided",
        ));
}

#[test]
fn test_run_id_conflicts_with_template() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", "someid", "--template", "foo"])
        .assert()
        .failure();
    // Clap produces "cannot be used with" error
}

#[test]
fn test_run_data_requires_template() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", "someid", "--data", "key=value"])
        .assert()
        .failure();
    // Clap produces "required by" or "requires" error
}

#[test]
fn test_run_invalid_data_format() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Need to create the template first so it gets past template lookup
    let templates_dir = temp_dir.path().join(".aiki/tasks");
    create_template(
        &templates_dir,
        "test",
        "foo",
        "---\nversion: 1.0.0\ndescription: Test\n---\n# Test\nBody",
    );

    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args([
            "run",
            "--template",
            "test/foo",
            "--data",
            "invalid-no-equals",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid --data format"));
}

#[test]
fn test_run_template_creates_task() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let templates_dir = temp_dir.path().join(".aiki/tasks");
    create_template(
        &templates_dir,
        "test",
        "run-me",
        "---\nversion: 1.0.0\ndescription: Runnable test\n---\n# Run: {{data.key}}\nBody with {{data.key}}",
    );

    // This will create the task from template and then try to run it.
    // The run may fail at agent spawning, but we should see the task was created.
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["run", "--template", "test/run-me", "--data", "key=hello"])
        .output()
        .expect("Failed to run command");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Added:") && stderr.contains("created from template"),
        "Expected task creation message in stderr, got: {}",
        stderr
    );
}

// ============================================================================
// Autostart Regression Tests
// ============================================================================
// These tests cover the four autostart paths in run_close():
// 1. Spawn autorun (spawned tasks with autorun: true)
// 2. Blocking link autorun (find_autorun_candidates)
// 3. Parent auto-start (all subtasks closed → parent starts)
// 4. Next subtask auto-start (session-owned parent → next child starts)

// --- Path 3: Parent auto-start guards ---

#[test]
fn test_parent_autostart_skips_when_parent_already_in_progress() {
    // If the parent is already InProgress, closing the last subtask should NOT
    // re-auto-start it (the guard at run_close should skip).
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create parent + 1 subtask
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Parent already running"])
        .output()
        .unwrap();
    let parent_id = extract_short_id(&String::from_utf8_lossy(&output.stdout));

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Only subtask", "--parent", &parent_id])
        .output()
        .unwrap();
    let sub_id = extract_short_id(&String::from_utf8_lossy(&output.stdout));

    // Start parent (puts it InProgress)
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &parent_id])
        .output()
        .unwrap();

    // Start and close the only subtask
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &sub_id])
        .output()
        .unwrap();

    let close_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "close", &sub_id, "--summary", "Done"])
        .output()
        .unwrap();
    let close_stdout = String::from_utf8_lossy(&close_output.stdout);

    // The close output should NOT contain parent auto-start notice
    assert!(
        !close_stdout.contains("auto-started for review"),
        "Parent already InProgress should not be auto-started again. Output: {}",
        close_stdout
    );

    // Parent should still be InProgress (unchanged)
    let show_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "show", &parent_id])
        .output()
        .unwrap();
    let show_stdout = String::from_utf8_lossy(&show_output.stdout);
    assert!(
        show_stdout.contains("Status: in_progress"),
        "Parent should remain InProgress. Output: {}",
        show_stdout
    );
}

#[test]
fn test_parent_autostart_skips_when_parent_already_closed() {
    // If the parent is already closed, closing its subtask should NOT
    // try to auto-start it.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create parent + 1 subtask
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Parent closed early"])
        .output()
        .unwrap();
    let parent_id = extract_short_id(&String::from_utf8_lossy(&output.stdout));

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Subtask A", "--parent", &parent_id])
        .output()
        .unwrap();
    let sub_id = extract_short_id(&String::from_utf8_lossy(&output.stdout));

    // Start and close the parent directly
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &parent_id])
        .output()
        .unwrap();
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "close", &parent_id, "--summary", "Closed early"])
        .output()
        .unwrap();

    // Now close the subtask
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &sub_id])
        .output()
        .unwrap();
    let close_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "close", &sub_id, "--summary", "Done"])
        .output()
        .unwrap();
    let close_stdout = String::from_utf8_lossy(&close_output.stdout);

    // Should not auto-start the already-closed parent
    assert!(
        !close_stdout.contains("auto-started for review"),
        "Already-closed parent should not be auto-started. Output: {}",
        close_stdout
    );
}

#[test]
fn test_parent_autostart_not_triggered_with_partial_close() {
    // If only some subtasks are closed, parent should NOT auto-start.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create parent + 2 subtasks
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Partial parent"])
        .output()
        .unwrap();
    let parent_id = extract_short_id(&String::from_utf8_lossy(&output.stdout));

    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Sub 1", "--parent", &parent_id])
        .output()
        .unwrap();

    let output2 = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Sub 2", "--parent", &parent_id])
        .output()
        .unwrap();
    let sub2_id = extract_short_id(&String::from_utf8_lossy(&output2.stdout));

    // Start parent, start and close only subtask 2
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &parent_id])
        .output()
        .unwrap();
    // Stop parent so it's no longer InProgress (otherwise the InProgress guard hides the test)
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "stop"])
        .output()
        .unwrap();

    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &sub2_id])
        .output()
        .unwrap();

    let close_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "close", &sub2_id, "--summary", "Done"])
        .output()
        .unwrap();
    let close_stdout = String::from_utf8_lossy(&close_output.stdout);

    // Only 1 of 2 subtasks closed — parent should NOT auto-start
    assert!(
        !close_stdout.contains("auto-started for review"),
        "Parent should not auto-start when not all subtasks are closed. Output: {}",
        close_stdout
    );
}

#[test]
fn test_parent_autostart_triggers_with_wontdo_subtasks() {
    // Parent should auto-start even if subtasks were closed as wont-do.
    // all_subtasks_closed() checks status == Closed regardless of outcome.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create parent + 2 subtasks
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Wontdo parent"])
        .output()
        .unwrap();
    let parent_id = extract_short_id(&String::from_utf8_lossy(&output.stdout));

    let out1 = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Sub done", "--parent", &parent_id])
        .output()
        .unwrap();
    let sub1_id = extract_short_id(&String::from_utf8_lossy(&out1.stdout));

    let out2 = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Sub wontdo", "--parent", &parent_id])
        .output()
        .unwrap();
    let sub2_id = extract_short_id(&String::from_utf8_lossy(&out2.stdout));

    // Start and close sub1 normally
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &sub1_id])
        .output()
        .unwrap();
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "close", &sub1_id, "--summary", "Done"])
        .output()
        .unwrap();

    // Close sub2 as wont-do
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &sub2_id])
        .output()
        .unwrap();
    let close_stdout = task_stdout(
        temp_dir.path(),
        &["close", &sub2_id, "--wont-do", "--summary", "Not needed"],
    );

    // Parent should be auto-started (both subtasks closed, regardless of outcome).
    assert!(
        close_stdout.contains("Started") && close_stdout.contains("Wontdo parent"),
        "Parent should auto-start even with wont-do subtasks. Output: {}",
        close_stdout
    );
}

#[test]
fn test_parent_autostart_skips_when_orchestrated() {
    // If the parent has an active orchestrator, closing the last subtask
    // should NOT auto-start the parent.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create parent + subtask + orchestrator
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Orchestrated parent"])
        .output()
        .unwrap();
    let parent_id = extract_short_id(&String::from_utf8_lossy(&output.stdout));

    let out_sub = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Sub orchestrated", "--parent", &parent_id])
        .output()
        .unwrap();
    let sub_id = extract_short_id(&String::from_utf8_lossy(&out_sub.stdout));

    // Create an orchestrator task that orchestrates the parent
    let out_orch = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Loop orchestrator"])
        .output()
        .unwrap();
    let orch_id = extract_short_id(&String::from_utf8_lossy(&out_orch.stdout));

    // Link orchestrator → parent
    aiki_task(
        temp_dir.path(),
        &["link", &orch_id, "--orchestrates", &parent_id],
    )
    .success();

    // Start the orchestrator (it's the active orchestrator)
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &orch_id])
        .output()
        .unwrap();

    // Start and close the subtask
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &sub_id])
        .output()
        .unwrap();

    let close_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "close", &sub_id, "--summary", "Done"])
        .output()
        .unwrap();
    let close_stdout = String::from_utf8_lossy(&close_output.stdout);

    // Parent should NOT auto-start because it has an active orchestrator
    assert!(
        !close_stdout.contains("auto-started for review"),
        "Orchestrated parent should not be auto-started. Output: {}",
        close_stdout
    );

    // Parent should still be in its original state (Open/Stopped, not InProgress)
    let show = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "show", &parent_id])
        .output()
        .unwrap();
    let show_stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        !show_stdout.contains("Status: in_progress"),
        "Orchestrated parent should not transition to InProgress. Output: {}",
        show_stdout
    );
}

// --- Path 2: Blocking link autorun (integration tests) ---

#[test]
fn test_blocking_link_autorun_starts_task_on_close() {
    // When task A is closed and task B has a blocked-by link to A with autorun,
    // B should be auto-started.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create task A (the blocker)
    let out_a = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Blocker task"])
        .output()
        .unwrap();
    let a_id = extract_short_id(&String::from_utf8_lossy(&out_a.stdout));

    // Create task B with --blocked-by A --autorun (creates link at add time)
    let out_b = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args([
            "task",
            "add",
            "Blocked autorun task",
            "--blocked-by",
            &a_id,
            "--autorun",
        ])
        .output()
        .unwrap();
    let b_id = extract_short_id(&String::from_utf8_lossy(&out_b.stdout));

    // Start and close A
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &a_id])
        .output()
        .unwrap();

    let close_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "close", &a_id, "--summary", "Done"])
        .output()
        .unwrap();
    let close_stdout = String::from_utf8_lossy(&close_output.stdout);

    // B should be auto-started
    assert!(
        close_stdout.contains("Auto-started (autorun)"),
        "Blocked task with autorun should be auto-started on blocker close. Output: {}",
        close_stdout
    );

    // Verify B is now InProgress
    let show = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "show", &b_id])
        .output()
        .unwrap();
    let show_stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        show_stdout.contains("Status: in_progress"),
        "Autorun task should be in_progress after blocker closes. Output: {}",
        show_stdout
    );
}

#[test]
fn test_blocking_link_no_autorun_does_not_start() {
    // When task A is closed and B has blocked-by without autorun, B should NOT auto-start.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create task A
    let out_a = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Blocker no-auto"])
        .output()
        .unwrap();
    let a_id = extract_short_id(&String::from_utf8_lossy(&out_a.stdout));

    // Create task B with --blocked-by A (NO --autorun)
    let out_b = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Blocked no-autorun", "--blocked-by", &a_id])
        .output()
        .unwrap();
    let b_id = extract_short_id(&String::from_utf8_lossy(&out_b.stdout));

    // Start and close A
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &a_id])
        .output()
        .unwrap();

    let close_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "close", &a_id, "--summary", "Done"])
        .output()
        .unwrap();
    let close_stdout = String::from_utf8_lossy(&close_output.stdout);

    // B should NOT auto-start (no autorun flag)
    assert!(
        !close_stdout.contains("Auto-started"),
        "Task without autorun should not be auto-started. Output: {}",
        close_stdout
    );

    // Verify B is still Open
    let show = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "show", &b_id])
        .output()
        .unwrap();
    let show_stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        show_stdout.contains("Status: open"),
        "Task without autorun should remain open. Output: {}",
        show_stdout
    );
}

#[test]
fn test_blocking_link_autorun_stays_blocked_with_multiple_blockers() {
    // C is blocked by A and B (both with autorun). Closing only A should NOT start C.
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create blocker tasks A and B
    let out_a = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Blocker A"])
        .output()
        .unwrap();
    let a_id = extract_short_id(&String::from_utf8_lossy(&out_a.stdout));

    let out_b = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "add", "Blocker B"])
        .output()
        .unwrap();
    let b_id = extract_short_id(&String::from_utf8_lossy(&out_b.stdout));

    // Create task C blocked by A with autorun
    let out_c = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args([
            "task",
            "add",
            "Blocked by both",
            "--blocked-by",
            &a_id,
            "--autorun",
        ])
        .output()
        .unwrap();
    let c_id = extract_short_id(&String::from_utf8_lossy(&out_c.stdout));

    // Add second blocked-by link via task link (no --autorun needed; C already has autorun on one link)
    aiki_task(temp_dir.path(), &["link", &c_id, "--blocked-by", &b_id]).success();

    // Close only A
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &a_id])
        .output()
        .unwrap();
    let close_output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "close", &a_id, "--summary", "Done"])
        .output()
        .unwrap();
    let close_stdout = String::from_utf8_lossy(&close_output.stdout);

    // C should NOT auto-start (still blocked by B)
    assert!(
        !close_stdout.contains("Auto-started"),
        "Task still blocked by B should not auto-start. Output: {}",
        close_stdout
    );

    // Now close B too
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "start", &b_id])
        .output()
        .unwrap();
    let close_output2 = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "close", &b_id, "--summary", "Done"])
        .output()
        .unwrap();
    let close_stdout2 = String::from_utf8_lossy(&close_output2.stdout);

    // NOW C should auto-start (all blockers closed)
    assert!(
        close_stdout2.contains("Auto-started (autorun)"),
        "Task should auto-start when all blockers are closed. Output: {}",
        close_stdout2
    );

    // Verify C is InProgress
    let show = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(temp_dir.path())
        .args(["task", "show", &c_id])
        .output()
        .unwrap();
    let show_stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        show_stdout.contains("Status: in_progress"),
        "C should be in_progress after all blockers closed. Output: {}",
        show_stdout
    );
}

// ============================================================================
// Regression: Outcome and Descendant Filters
// ============================================================================

#[test]
fn test_task_list_done_filter() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create two tasks: one closed-done, one closed-wont_do
    aiki_task(temp_dir.path(), &["add", "Done task"]).success();
    aiki_task(temp_dir.path(), &["add", "Wont do task"]).success();

    // Close first as done
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(
        temp_dir.path(),
        &["close", "--outcome", "done", "--summary", "Completed"],
    )
    .success();

    // Close second as wont_do
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(
        temp_dir.path(),
        &["close", "--wont-do", "--summary", "Skipped"],
    )
    .success();

    // --done should show only the done task
    let output = aiki_task(temp_dir.path(), &["list", "--done"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(
        stdout.contains("Done task"),
        "--done should include done task, got: {}",
        stdout
    );
    assert!(
        !stdout.contains("Wont do task"),
        "--done should exclude wont_do task, got: {}",
        stdout
    );
}

#[test]
fn test_task_list_wont_do_filter() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create two tasks: one closed-done, one closed-wont_do
    aiki_task(temp_dir.path(), &["add", "Done task"]).success();
    aiki_task(temp_dir.path(), &["add", "Wont do task"]).success();

    // Close first as done
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(
        temp_dir.path(),
        &["close", "--outcome", "done", "--summary", "Completed"],
    )
    .success();

    // Close second as wont_do
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(
        temp_dir.path(),
        &["close", "--wont-do", "--summary", "Skipped"],
    )
    .success();

    // --wont-do should show only the wont_do task
    let output = aiki_task(temp_dir.path(), &["list", "--wont-do"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(
        stdout.contains("Wont do task"),
        "--wont-do should include wont_do task, got: {}",
        stdout
    );
    assert!(
        !stdout.contains("Done task"),
        "--wont-do should exclude done task, got: {}",
        stdout
    );
}

#[test]
fn test_task_list_done_and_wont_do_combined() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create three tasks: one done, one wont_do, one still open
    aiki_task(temp_dir.path(), &["add", "Done task"]).success();
    aiki_task(temp_dir.path(), &["add", "Wont do task"]).success();
    aiki_task(temp_dir.path(), &["add", "Open task"]).success();

    // Close first as done
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(
        temp_dir.path(),
        &["close", "--outcome", "done", "--summary", "Completed"],
    )
    .success();

    // Close second as wont_do
    aiki_task(temp_dir.path(), &["start"]).success();
    aiki_task(
        temp_dir.path(),
        &["close", "--wont-do", "--summary", "Skipped"],
    )
    .success();

    // --done --wont-do should show both closed tasks (all outcomes)
    let output = aiki_task(temp_dir.path(), &["list", "--done", "--wont-do"]).success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(
        stdout.contains("Done task"),
        "--done --wont-do should include done task, got: {}",
        stdout
    );
    assert!(
        stdout.contains("Wont do task"),
        "--done --wont-do should include wont_do task, got: {}",
        stdout
    );
    // The filtered "Tasks" section should contain exactly the 2 closed tasks
    assert!(
        stdout.contains("Tasks (2):"),
        "--done --wont-do filtered section should show exactly 2 tasks, got: {}",
        stdout
    );
}

#[test]
fn test_task_diff_uses_started_working_copy_baseline_and_scopes_files() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    fs::write(temp_dir.path().join("tracked.txt"), "base\n").unwrap();
    fs::write(temp_dir.path().join("unrelated.txt"), "clean\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["file", "track", "tracked.txt", "unrelated.txt"],
    );
    run_jj(temp_dir.path(), &["describe", "-m", "base"]);
    run_jj(temp_dir.path(), &["new"]);

    let add_output = task_stdout(temp_dir.path(), &["add", "Scoped diff"]);
    let short_id = extract_short_id(&add_output);
    aiki_task(temp_dir.path(), &["start", &short_id]).success();
    let full_id = extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &short_id]));

    // Freeze the start snapshot so subsequent task work happens on descendants.
    run_jj(temp_dir.path(), &["new"]);

    fs::write(temp_dir.path().join("tracked.txt"), "intermediate\n").unwrap();
    fs::write(temp_dir.path().join("unrelated.txt"), "noise\n").unwrap();
    run_jj(temp_dir.path(), &["describe", "-m", "intermediate"]);
    run_jj(temp_dir.path(), &["new"]);

    fs::write(temp_dir.path().join("tracked.txt"), "final\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", full_id)],
    );
    run_jj(temp_dir.path(), &["new"]);

    let diff_output = aiki_task_with_env(temp_dir.path(), &["diff", &full_id], &[]);
    assert!(
        diff_output.contains("tracked.txt"),
        "task diff should include the task-touched file: {}",
        diff_output
    );
    assert!(
        diff_output.contains("-base"),
        "task diff should use the Started.working_copy baseline: {}",
        diff_output
    );
    assert!(
        diff_output.contains("+final"),
        "task diff should include the final task change: {}",
        diff_output
    );
    assert!(
        !diff_output.contains("unrelated.txt"),
        "task diff should be scoped away from unrelated files: {}",
        diff_output
    );
    assert!(
        !diff_output.contains("noise"),
        "task diff should exclude unrelated-file content: {}",
        diff_output
    );

    let name_only_output =
        aiki_task_with_env(temp_dir.path(), &["diff", "--name-only", &full_id], &[]);
    assert_eq!(name_only_output.trim(), "tracked.txt");
}

#[test]
fn test_task_diff_direct_on_working_copy_keeps_start_snapshot_and_filters_metadata() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let repo_id_path = temp_dir.path().join(".aiki/repo-id");
    assert!(
        repo_id_path.exists(),
        "aiki init should create .aiki/repo-id for this regression test"
    );

    fs::write(temp_dir.path().join("tracked.txt"), "base\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["file", "track", "tracked.txt", ".aiki/repo-id"],
    );
    run_jj(temp_dir.path(), &["describe", "-m", "base"]);
    run_jj(temp_dir.path(), &["new"]);

    let add_output = task_stdout(temp_dir.path(), &["add", "Direct @ diff"]);
    let short_id = extract_short_id(&add_output);
    aiki_task(temp_dir.path(), &["start", &short_id]).success();
    let full_id = extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &short_id]));

    fs::write(temp_dir.path().join("tracked.txt"), "final\n").unwrap();
    fs::write(&repo_id_path, "temporary-repo-id\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", full_id)],
    );

    let diff_output = aiki_task_with_env(temp_dir.path(), &["diff", &full_id], &[]);
    assert!(
        diff_output.contains("tracked.txt"),
        "task diff should include the tracked file changed after start: {}",
        diff_output
    );
    assert!(
        diff_output.contains("-base"),
        "task diff should stay anchored to the start snapshot even after rewriting @: {}",
        diff_output
    );
    assert!(
        diff_output.contains("+final"),
        "task diff should include the final working-copy rewrite content: {}",
        diff_output
    );
    assert!(
        !diff_output.contains(".aiki/repo-id"),
        "task diff should filter internal metadata paths: {}",
        diff_output
    );
    assert!(
        !diff_output.contains("temporary-repo-id"),
        "filtered internal metadata content should not leak into task diff output: {}",
        diff_output
    );

    let name_only_output =
        aiki_task_with_env(temp_dir.path(), &["diff", "--name-only", &full_id], &[]);
    assert_eq!(name_only_output.trim(), "tracked.txt");
}

#[test]
fn test_task_diff_empty_intersection_stays_scoped() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    fs::write(temp_dir.path().join("tracked.txt"), "base\n").unwrap();
    fs::write(temp_dir.path().join("unrelated.txt"), "clean\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["file", "track", "tracked.txt", "unrelated.txt"],
    );
    run_jj(temp_dir.path(), &["describe", "-m", "base"]);
    run_jj(temp_dir.path(), &["new"]);

    let add_output = task_stdout(temp_dir.path(), &["add", "Empty intersection diff"]);
    let short_id = extract_short_id(&add_output);
    aiki_task(temp_dir.path(), &["start", &short_id]).success();
    let full_id = extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &short_id]));

    run_jj(temp_dir.path(), &["new"]);

    fs::write(temp_dir.path().join("tracked.txt"), "task change\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", full_id)],
    );
    run_jj(temp_dir.path(), &["new"]);

    fs::write(temp_dir.path().join("unrelated.txt"), "noise\n").unwrap();
    run_jj(temp_dir.path(), &["describe", "-m", "non-task change"]);
    run_jj(temp_dir.path(), &["new"]);

    fs::write(temp_dir.path().join("tracked.txt"), "base\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", full_id)],
    );
    run_jj(temp_dir.path(), &["new"]);

    let diff_output = aiki_task_with_env(temp_dir.path(), &["diff", &full_id], &[]);
    assert!(
        diff_output.trim() == "No scoped changes.",
        "task diff should explain the empty scoped intersection in default mode: {}",
        diff_output
    );
    assert!(
        !diff_output.contains("unrelated.txt"),
        "task diff should not leak unrelated files when the scoped intersection is empty: {}",
        diff_output
    );
    assert!(
        !diff_output.contains("noise"),
        "task diff should not leak unrelated file content when the scoped intersection is empty: {}",
        diff_output
    );

    let name_only_output =
        aiki_task_with_env(temp_dir.path(), &["diff", "--name-only", &full_id], &[]);
    assert!(
        name_only_output.trim().is_empty(),
        "--name-only should stay empty when the scoped intersection is empty: {}",
        name_only_output
    );
}

#[test]
fn test_parent_task_diff_uses_pre_subtask_baseline_after_parent_autostart() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    fs::write(temp_dir.path().join("tracked.txt"), "base\n").unwrap();
    run_jj(temp_dir.path(), &["file", "track", "tracked.txt"]);
    run_jj(temp_dir.path(), &["describe", "-m", "base"]);
    run_jj(temp_dir.path(), &["new"]);

    let parent_short = extract_short_id(&task_stdout(temp_dir.path(), &["add", "Parent task"]));
    let parent_full =
        extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &parent_short]));

    let subtask1_short = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Subtask 1", "--parent", &parent_short],
    ));
    let subtask1_full =
        extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &subtask1_short]));

    let subtask2_short = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Subtask 2", "--parent", &parent_short],
    ));
    let subtask2_full =
        extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &subtask2_short]));

    aiki_task(temp_dir.path(), &["start", &subtask1_short]).success();
    run_jj(temp_dir.path(), &["new"]);
    fs::write(temp_dir.path().join("tracked.txt"), "subtask one\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", subtask1_full)],
    );
    run_jj(temp_dir.path(), &["new"]);
    aiki_task(
        temp_dir.path(),
        &["close", &subtask1_short, "--summary", "Done"],
    )
    .success();

    aiki_task(temp_dir.path(), &["start", &subtask2_short]).success();
    run_jj(temp_dir.path(), &["new"]);
    fs::write(temp_dir.path().join("tracked.txt"), "subtask two\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", subtask2_full)],
    );
    run_jj(temp_dir.path(), &["new"]);

    let close_output = task_stdout(
        temp_dir.path(),
        &["close", &subtask2_short, "--summary", "All done"],
    );
    assert!(
        close_output.contains("Parent task"),
        "closing the final subtask should auto-start the parent: {}",
        close_output
    );

    let parent_diff = aiki_task_with_env(temp_dir.path(), &["diff", &parent_full], &[]);
    assert!(
        parent_diff.contains("tracked.txt"),
        "parent diff should include descendant-touched files: {}",
        parent_diff
    );
    assert!(
        parent_diff.contains("-base"),
        "parent diff should anchor before subtask work, not at the auto-start snapshot: {}",
        parent_diff
    );
    assert!(
        parent_diff.contains("+subtask two"),
        "parent diff should include the final descendant content after auto-start: {}",
        parent_diff
    );
    assert!(
        !parent_diff.contains("No changes found"),
        "parent diff should not collapse to an empty result after auto-start: {}",
        parent_diff
    );
}

#[test]
fn test_task_diff_fallback_summary_scoping_handles_short_paths() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    fs::write(temp_dir.path().join("a"), "base\n").unwrap();
    fs::write(temp_dir.path().join("unrelated.txt"), "clean\n").unwrap();
    run_jj(temp_dir.path(), &["file", "track", "a", "unrelated.txt"]);
    run_jj(temp_dir.path(), &["describe", "-m", "base"]);
    run_jj(temp_dir.path(), &["new"]);

    let add_output = task_stdout(temp_dir.path(), &["add", "Fallback diff"]);
    let short_id = extract_short_id(&add_output);
    aiki_task(temp_dir.path(), &["start", &short_id]).success();
    let full_id = extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &short_id]));

    run_jj(temp_dir.path(), &["new"]);

    fs::write(temp_dir.path().join("unrelated.txt"), "noise\n").unwrap();
    run_jj(temp_dir.path(), &["describe", "-m", "intermediate"]);
    run_jj(temp_dir.path(), &["new"]);

    fs::write(temp_dir.path().join("a"), "final\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", full_id)],
    );
    run_jj(temp_dir.path(), &["new"]);

    let real_jj = String::from_utf8_lossy(
        &Command::new("which")
            .arg("jj")
            .output()
            .expect("Failed to resolve jj path")
            .stdout,
    )
    .trim()
    .to_string();
    assert!(!real_jj.is_empty(), "which jj returned an empty path");

    let bin_dir = temp_dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let wrapper_path = bin_dir.join("jj");
    fs::write(
        &wrapper_path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"log\" ]; then\n  for arg in \"$@\"; do\n    if [ \"$arg\" = \"--name-only\" ]; then\n      exit 1\n    fi\n  done\nfi\nexec \"{}\" \"$@\"\n",
            real_jj
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&wrapper_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&wrapper_path, perms).unwrap();

    let wrapped_path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let envs = [("PATH", wrapped_path.as_str())];

    let summary_output =
        aiki_task_with_env(temp_dir.path(), &["diff", "--summary", &full_id], &envs);
    assert!(
        summary_output.contains("M a"),
        "fallback summary parsing should keep short paths: {}",
        summary_output
    );
    assert!(
        !summary_output.contains("unrelated.txt"),
        "fallback scoping should exclude unrelated files: {}",
        summary_output
    );

    let name_only_output =
        aiki_task_with_env(temp_dir.path(), &["diff", "--name-only", &full_id], &envs);
    assert_eq!(name_only_output.trim(), "a");
}

#[test]
fn test_task_diff_fallback_summary_scoping_handles_renames() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    fs::write(temp_dir.path().join("old.txt"), "tracked\n").unwrap();
    fs::write(temp_dir.path().join("unrelated.txt"), "clean\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["file", "track", "old.txt", "unrelated.txt"],
    );
    run_jj(temp_dir.path(), &["describe", "-m", "base"]);
    run_jj(temp_dir.path(), &["new"]);

    let add_output = task_stdout(temp_dir.path(), &["add", "Fallback rename diff"]);
    let short_id = extract_short_id(&add_output);
    aiki_task(temp_dir.path(), &["start", &short_id]).success();
    let full_id = extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &short_id]));

    run_jj(temp_dir.path(), &["new"]);

    fs::write(temp_dir.path().join("unrelated.txt"), "noise\n").unwrap();
    run_jj(temp_dir.path(), &["describe", "-m", "intermediate"]);
    run_jj(temp_dir.path(), &["new"]);

    fs::rename(
        temp_dir.path().join("old.txt"),
        temp_dir.path().join("new.txt"),
    )
    .unwrap();
    run_jj(temp_dir.path(), &["file", "track", "new.txt"]);
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", full_id)],
    );
    run_jj(temp_dir.path(), &["new"]);

    let real_jj = String::from_utf8_lossy(
        &Command::new("which")
            .arg("jj")
            .output()
            .expect("Failed to resolve jj path")
            .stdout,
    )
    .trim()
    .to_string();
    assert!(!real_jj.is_empty(), "which jj returned an empty path");

    let bin_dir = temp_dir.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let wrapper_path = bin_dir.join("jj");
    fs::write(
        &wrapper_path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"log\" ]; then\n  for arg in \"$@\"; do\n    if [ \"$arg\" = \"--name-only\" ]; then\n      exit 1\n    fi\n  done\nfi\nexec \"{}\" \"$@\"\n",
            real_jj
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&wrapper_path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&wrapper_path, perms).unwrap();

    let wrapped_path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let envs = [("PATH", wrapped_path.as_str())];

    let summary_output =
        aiki_task_with_env(temp_dir.path(), &["diff", "--summary", &full_id], &envs);
    assert!(
        summary_output.contains("old.txt") && summary_output.contains("new.txt"),
        "fallback summary scoping should include both renamed paths: {}",
        summary_output
    );
    assert!(
        !summary_output.contains("unrelated.txt"),
        "fallback rename scoping should exclude unrelated files: {}",
        summary_output
    );

    let name_only_output =
        aiki_task_with_env(temp_dir.path(), &["diff", "--name-only", &full_id], &envs);
    let name_only_lines: Vec<&str> = name_only_output.lines().collect();
    assert_eq!(name_only_lines, vec!["old.txt", "new.txt"]);
}

#[test]
fn test_tldr_renders_snapshot_scoped_epic_diff_and_fix_metadata() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());
    let fake_path = install_fake_agent_cli(temp_dir.path(), "codex");
    let envs = [("PATH", fake_path.as_str())];

    fs::write(temp_dir.path().join("tracked.txt"), "base\n").unwrap();
    fs::write(temp_dir.path().join("unrelated.txt"), "clean\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["file", "track", "tracked.txt", "unrelated.txt"],
    );
    run_jj(temp_dir.path(), &["describe", "-m", "base"]);
    run_jj(temp_dir.path(), &["new"]);

    let epic_short = extract_short_id(&task_stdout(temp_dir.path(), &["add", "Epic: TLDR scope"]));
    let epic_full =
        extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &epic_short]));
    let subtask_short = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Implement change", "--parent", &epic_short],
    ));
    let subtask_full =
        extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &subtask_short]));

    aiki_task(temp_dir.path(), &["start", &subtask_short]).success();
    run_jj(temp_dir.path(), &["new"]);
    fs::write(temp_dir.path().join("tracked.txt"), "feature\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", subtask_full)],
    );
    run_jj(temp_dir.path(), &["new"]);
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &subtask_short,
            "--confidence",
            "4",
            "--summary",
            "Implemented",
        ],
    )
    .success();
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &epic_short,
            "--confidence",
            "4",
            "--summary",
            "Epic done",
        ],
    )
    .success();

    let review_short = extract_short_id(&task_stdout(temp_dir.path(), &["add", "Review epic"]));
    let review_full =
        extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &review_short]));
    aiki_task(
        temp_dir.path(),
        &["link", &review_full, "--validates", &epic_full],
    )
    .success();
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &review_short,
            "--confidence",
            "4",
            "--summary",
            "Needs a fix",
        ],
    )
    .success();

    let fix_short = extract_short_id(&task_stdout(temp_dir.path(), &["add", "Fix follow-up"]));
    let fix_full = extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &fix_short]));
    aiki_task(
        temp_dir.path(),
        &["link", &fix_full, "--remediates", &review_full],
    )
    .success();
    aiki_task(temp_dir.path(), &["start", &fix_short]).success();
    run_jj(temp_dir.path(), &["new"]);
    fs::write(temp_dir.path().join("unrelated.txt"), "noise\n").unwrap();
    run_jj(temp_dir.path(), &["describe", "-m", "intermediate"]);
    run_jj(temp_dir.path(), &["new"]);
    fs::write(temp_dir.path().join("tracked.txt"), "feature+fix\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", fix_full)],
    );
    run_jj(temp_dir.path(), &["new"]);
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &fix_short,
            "--confidence",
            "4",
            "--summary",
            "Fixed review feedback",
        ],
    )
    .success();

    let tldr_output = aiki_stdout_with_env(
        temp_dir.path(),
        &["tldr", &epic_full, "--agent", "codex"],
        &envs,
    );
    let tldr_task_id = extract_tldr_task_id(&tldr_output);
    let instructions = aiki_stdout_with_env(
        temp_dir.path(),
        &["task", "show", &tldr_task_id, "--with-instructions"],
        &envs,
    );

    assert!(
        instructions.contains("<files-changed>\nM tracked.txt"),
        "TLDR payload should include the snapshot-scoped file list: {}",
        instructions
    );
    assert!(
        instructions.contains("<file-stats>") && instructions.contains("tracked.txt"),
        "TLDR payload should include scoped diff stats: {}",
        instructions
    );
    assert!(
        instructions.contains("\"files_changed\": [\n            \"tracked.txt\""),
        "review-history fix metadata should report the scoped tracked file: {}",
        instructions
    );
    assert!(
        !instructions.contains("unrelated.txt"),
        "snapshot-scoped TLDR data should exclude unrelated churn: {}",
        instructions
    );
}

#[test]
fn test_tldr_fallback_renders_legacy_tasks_without_started_snapshots() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());
    let fake_path = install_fake_agent_cli(temp_dir.path(), "codex");
    let envs = [("PATH", fake_path.as_str())];

    fs::write(temp_dir.path().join("tracked.txt"), "base\n").unwrap();
    run_jj(temp_dir.path(), &["file", "track", "tracked.txt"]);
    run_jj(temp_dir.path(), &["describe", "-m", "base"]);
    run_jj(temp_dir.path(), &["new"]);

    let epic_short = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Epic: Legacy fallback"],
    ));
    let epic_full =
        extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &epic_short]));
    let subtask_short = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Legacy subtask", "--parent", &epic_short],
    ));
    let subtask_full =
        extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &subtask_short]));

    fs::write(temp_dir.path().join("tracked.txt"), "legacy-epic\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", subtask_full)],
    );
    run_jj(temp_dir.path(), &["new"]);
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &subtask_short,
            "--confidence",
            "4",
            "--summary",
            "Legacy done",
        ],
    )
    .success();
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &epic_short,
            "--confidence",
            "4",
            "--summary",
            "Legacy epic done",
        ],
    )
    .success();

    let review_short = extract_short_id(&task_stdout(temp_dir.path(), &["add", "Legacy review"]));
    let review_full =
        extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &review_short]));
    aiki_task(
        temp_dir.path(),
        &["link", &review_full, "--validates", &epic_full],
    )
    .success();
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &review_short,
            "--confidence",
            "4",
            "--summary",
            "Legacy review done",
        ],
    )
    .success();

    let fix_short = extract_short_id(&task_stdout(temp_dir.path(), &["add", "Legacy fix"]));
    let fix_full = extract_full_id_from_show(&task_stdout(temp_dir.path(), &["show", &fix_short]));
    aiki_task(
        temp_dir.path(),
        &["link", &fix_full, "--remediates", &review_full],
    )
    .success();
    fs::write(temp_dir.path().join("tracked.txt"), "legacy-fix\n").unwrap();
    run_jj(
        temp_dir.path(),
        &["describe", "-m", &format!("task={}", fix_full)],
    );
    run_jj(temp_dir.path(), &["new"]);
    aiki_task(
        temp_dir.path(),
        &[
            "close",
            &fix_short,
            "--confidence",
            "4",
            "--summary",
            "Legacy fix done",
        ],
    )
    .success();

    let tldr_output = aiki_stdout_with_env(
        temp_dir.path(),
        &["tldr", &epic_full, "--agent", "codex"],
        &envs,
    );
    let tldr_task_id = extract_tldr_task_id(&tldr_output);
    let instructions = aiki_stdout_with_env(
        temp_dir.path(),
        &["task", "show", &tldr_task_id, "--with-instructions"],
        &envs,
    );

    assert!(
        instructions.contains("<diff>") && instructions.contains("tracked.txt"),
        "legacy TLDR fallback should still render a diff: {}",
        instructions
    );
    assert!(
        instructions.contains("<file-stats>") && !instructions.contains("File stats unavailable."),
        "legacy TLDR fallback should still render file stats: {}",
        instructions
    );
    assert!(
        instructions.contains("\"diff_stat\":") && !instructions.contains("\"diff_stat\": null"),
        "legacy review-history fallback should still render fix diff stats: {}",
        instructions
    );
}

// ── --instructions flag tests ───────────────────────────────────────────────

#[test]
fn test_task_add_inline_instructions() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let task_id = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Task with instructions", "--instructions", "Do step 1 then step 2"],
    ));

    let show = task_stdout(temp_dir.path(), &["show", &task_id, "--with-instructions"]);
    assert!(
        show.contains("Do step 1 then step 2"),
        "Expected inline instructions in show output: {}",
        show
    );
}

#[test]
fn test_task_add_instructions_from_file() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let instr_file = temp_dir.path().join("instructions.md");
    fs::write(&instr_file, "# Steps\n1. Build\n2. Test\n").unwrap();

    let task_id = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Task from file", "--instructions", instr_file.to_str().unwrap()],
    ));

    let show = task_stdout(temp_dir.path(), &["show", &task_id, "--with-instructions"]);
    assert!(
        show.contains("# Steps"),
        "Expected file-based instructions in show output: {}",
        show
    );
    assert!(
        show.contains("1. Build"),
        "Expected file content in instructions: {}",
        show
    );
}

#[test]
fn test_task_start_quickstart_with_instructions() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let output = task_stdout(
        temp_dir.path(),
        &["start", "Quick task", "--instructions", "Run the linter"],
    );
    let task_id = extract_started_id(&output);

    let show = task_stdout(temp_dir.path(), &["show", &task_id, "--with-instructions"]);
    assert!(
        show.contains("Run the linter"),
        "Expected instructions set via quick-start: {}",
        show
    );
}

#[test]
fn test_task_start_existing_with_instructions() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let task_id = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Existing task"],
    ));

    task_stdout(
        temp_dir.path(),
        &["start", &task_id, "--instructions", "Override instructions"],
    );

    let show = task_stdout(temp_dir.path(), &["show", &task_id, "--with-instructions"]);
    assert!(
        show.contains("Override instructions"),
        "Expected instructions set on start of existing task: {}",
        show
    );
}

#[test]
fn test_task_set_inline_instructions() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let task_id = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Set instructions task"],
    ));

    task_stdout(
        temp_dir.path(),
        &["set", &task_id, "--instructions", "Updated instructions"],
    );

    let show = task_stdout(temp_dir.path(), &["show", &task_id, "--with-instructions"]);
    assert!(
        show.contains("Updated instructions"),
        "Expected updated instructions in show output: {}",
        show
    );
}

#[test]
fn test_task_set_instructions_from_file() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let task_id = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "File instructions task"],
    ));

    let instr_file = temp_dir.path().join("detailed.txt");
    fs::write(&instr_file, "Detailed instructions from file\n").unwrap();

    task_stdout(
        temp_dir.path(),
        &["set", &task_id, "--instructions", instr_file.to_str().unwrap()],
    );

    let show = task_stdout(temp_dir.path(), &["show", &task_id, "--with-instructions"]);
    assert!(
        show.contains("Detailed instructions from file"),
        "Expected file-based instructions via set: {}",
        show
    );
}

#[test]
fn test_task_set_instructions_overwrites_previous() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let task_id = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Overwrite test", "--instructions", "Original instructions"],
    ));

    task_stdout(
        temp_dir.path(),
        &["set", &task_id, "--instructions", "Replaced instructions"],
    );

    let show = task_stdout(temp_dir.path(), &["show", &task_id, "--with-instructions"]);
    assert!(
        show.contains("Replaced instructions"),
        "Expected replaced instructions: {}",
        show
    );
    assert!(
        !show.contains("Original instructions"),
        "Original instructions should be gone: {}",
        show
    );
}

#[test]
fn test_task_unset_instructions() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let task_id = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Unset test", "--instructions", "Will be removed"],
    ));

    // Verify instructions exist
    let show = task_stdout(temp_dir.path(), &["show", &task_id, "--with-instructions"]);
    assert!(
        show.contains("Will be removed"),
        "Instructions should be present before unset: {}",
        show
    );

    // Unset instructions
    task_stdout(temp_dir.path(), &["unset", &task_id, "--instructions"]);

    // Verify instructions are gone
    let show = task_stdout(temp_dir.path(), &["show", &task_id, "--with-instructions"]);
    assert!(
        !show.contains("Will be removed"),
        "Instructions should be cleared after unset: {}",
        show
    );
    assert!(
        !show.contains("### Instructions"),
        "Instructions header should not appear after unset: {}",
        show
    );
}

#[test]
fn test_task_show_without_with_instructions_hides_instructions() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let task_id = extract_short_id(&task_stdout(
        temp_dir.path(),
        &["add", "Hidden instructions test", "--instructions", "Secret instructions"],
    ));

    // Without --with-instructions, instructions should be hidden
    let show = task_stdout(temp_dir.path(), &["show", &task_id]);
    assert!(
        !show.contains("Secret instructions"),
        "Instructions should be hidden without --with-instructions: {}",
        show
    );
}

