//! End-to-end test harness for aiki + agent pipelines.
//!
//! Tests are `#[ignore]` by default because they:
//! - Make real API calls (costs tokens)
//! - Require agent binaries (claude, codex) installed
//! - Require API keys configured
//! - Are slow (~30-60s per test)
//!
//! Run all e2e tests:  `cargo test --test e2e -- --ignored`
//! Run one suite:      `cargo test --test e2e -- --ignored provenance`

mod provenance;
mod session_thread;
mod task_lifecycle;

use assert_cmd::Command;
use std::path::Path;
use std::process;
use std::time::{Duration, Instant};

/// Check if jj binary is available in PATH
pub fn jj_available() -> bool {
    process::Command::new("jj")
        .arg("--version")
        .output()
        .is_ok()
}

/// Check if an agent binary is available in PATH
pub fn agent_available(name: &str) -> bool {
    process::Command::new(name)
        .arg("--version")
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Initialize a git repo at the given path
pub fn init_git_repo(path: &Path) {
    process::Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("Failed to initialize Git repository");
}

/// Run `aiki init` in a temp repo
pub fn init_aiki_repo(repo_path: &Path) {
    init_git_repo(repo_path);

    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo_path)
        .arg("init")
        .output()
        .expect("Failed to run aiki init");

    assert!(
        output.status.success(),
        "aiki init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run `aiki task add` and return the 32-char task ID
pub fn create_task(repo_path: &Path, description: &str) -> String {
    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo_path)
        .args(["task", "add", description])
        .output()
        .expect("Failed to create task");

    assert!(
        output.status.success(),
        "aiki task add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Output format: "Added <id> — Description" (id may be prefix or full 32 chars)
    stdout
        .split_whitespace()
        .find(|w| w.len() >= 3 && w.chars().all(|c| matches!(c, 'k'..='z')))
        .unwrap_or_else(|| panic!("Could not parse task ID from: {stdout}"))
        .to_string()
}

/// Set instructions on a task
pub fn set_task_instructions(repo_path: &Path, task_id: &str, instructions: &str) {
    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo_path)
        .args(["task", "set", task_id, "--instructions"])
        .write_stdin(instructions)
        .output()
        .expect("Failed to set task instructions");

    assert!(
        output.status.success(),
        "aiki task set --instructions failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run `aiki run <task-id>` synchronously, returns (success, stdout, stderr)
pub fn aiki_run(repo_path: &Path, task_id: &str, timeout: Duration) -> (bool, String, String) {
    let child = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo_path)
        .args(["run", task_id])
        .timeout(timeout)
        .output();

    match child {
        Ok(output) => (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ),
        Err(e) => (false, String::new(), format!("aiki run failed: {e}")),
    }
}

/// Query jj for commits with a specific task ID in provenance metadata
pub fn find_provenance_commits(repo_path: &Path, task_id: &str) -> Vec<String> {
    let query = format!("task={task_id}");
    let output = process::Command::new("jj")
        .args([
            "log",
            "-r",
            &format!("description(substring:\"{query}\")"),
            "--no-graph",
            "-T",
            "change_id ++ \"\\n\"",
        ])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run jj log");

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Check that a commit description contains expected provenance fields
pub fn validate_provenance_fields(repo_path: &Path, change_id: &str, task_id: &str) {
    let output = process::Command::new("jj")
        .args(["log", "-r", change_id, "--no-graph", "-T", "description"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run jj log");

    let desc = String::from_utf8_lossy(&output.stdout);

    assert!(
        desc.contains("[aiki]"),
        "Missing [aiki] block in commit {change_id}"
    );
    assert!(
        desc.contains(&format!("task={task_id}")),
        "Missing task= in commit {change_id}"
    );
    assert!(
        desc.contains("session="),
        "Missing session= in commit {change_id}"
    );
    assert!(
        desc.contains("author_type=agent"),
        "Missing author_type=agent in commit {change_id}"
    );
}

/// Check if a file exists anywhere in jj history (not just working copy)
pub fn file_in_jj_history(repo_path: &Path, filename: &str) -> bool {
    // Check working copy first
    if repo_path.join(filename).exists() {
        return true;
    }
    // Check if the file appears in any jj diff
    let output = process::Command::new("jj")
        .args(["log", "--no-graph", "-T", "change_id ++ \"\\n\"", "-r", "all()"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run jj log");

    let change_ids: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    for cid in change_ids {
        let diff = process::Command::new("jj")
            .args(["diff", "-r", &cid, "--name-only"])
            .current_dir(repo_path)
            .output()
            .expect("Failed to run jj diff");

        let diff_out = String::from_utf8_lossy(&diff.stdout);
        if diff_out.contains(filename) {
            return true;
        }
    }
    false
}

/// Wait for task to reach closed status, polling every second
pub fn wait_for_task_closed(repo_path: &Path, task_id: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let output = Command::cargo_bin("aiki")
            .unwrap()
            .current_dir(repo_path)
            .args(["task", "show", task_id])
            .output()
            .expect("Failed to show task");

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("Status: closed") {
            return true;
        }

        std::thread::sleep(Duration::from_secs(1));
    }
    false
}
