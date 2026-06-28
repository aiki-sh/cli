//! Consumer-path regression test for `aiki review` resilience.
//!
//! Before the retry+degrade fix, a review agent that ended its turn before
//! closing the review task — the common headless case — surfaced as a hard
//! `Failed to spawn agent: Agent process exited without completing task` with a
//! non-zero exit. The fix re-spawns the agent while it keeps closing review
//! phases and, if it still can't finish, degrades to a partial review instead of
//! erroring (see `workflow::steps::review`).
//!
//! This drives the REAL `aiki review` binary end to end. The hermetic fake agent
//! (`tests/common`) is a no-op `exit 0`: it never closes a review phase, so the
//! retry loop exhausts and the review takes the give-up → degrade path
//! deterministically. The load-bearing assertions: `aiki review` exits 0 and
//! never prints the raw spawn failure — this is the consumer path, not the
//! decision logic in isolation (those are unit-tested in `steps::review`).

mod common;

use std::path::Path;
use std::process::Command;

fn init_git_repo(path: &Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(path)
        .output()
        .expect("git config name");
}

fn init_aiki_repo(path: &Path) {
    init_git_repo(path);
    let output = common::aiki_cmd()
        .current_dir(path)
        .arg("init")
        .output()
        .expect("aiki init");
    assert!(
        output.status.success(),
        "aiki init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Pull the task ID (short prefix) from an `aiki task add` confirmation line,
/// e.g. `Added urosmsn` or `Added: <id> — name`. Reverse-hex IDs use `k`–`z`.
fn extract_task_id(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let rest = line
            .trim()
            .strip_prefix("Added: ")
            .or_else(|| line.trim().strip_prefix("Added "))?;
        let id: String = rest.chars().take_while(|c| matches!(c, 'k'..='z')).collect();
        (id.len() >= 3).then_some(id)
    })
}

#[test]
fn review_degrades_gracefully_when_agent_exits_early() {
    if !common::jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    // A task to review.
    let add = common::aiki_cmd()
        .current_dir(repo)
        .args([
            "task",
            "add",
            "Token rollup tweak",
            "-i",
            "Adjust the rollup math.",
        ])
        .output()
        .expect("task add");
    assert!(
        add.status.success(),
        "task add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let task_id = extract_task_id(&String::from_utf8_lossy(&add.stdout))
        .expect("task id from add output");

    // Review it with the no-op fake agent. The agent never closes a review
    // phase, so the retry loop exhausts and the review degrades to a partial
    // result instead of hard-failing.
    let review = common::aiki_cmd()
        .current_dir(repo)
        .args(["review", &task_id, "--agent", "claude-code"])
        .output()
        .expect("aiki review");

    let stdout = String::from_utf8_lossy(&review.stdout);
    let stderr = String::from_utf8_lossy(&review.stderr);

    // The reported symptom: the raw spawn failure. The fix must never surface it.
    assert!(
        !stdout.contains("exited without completing task")
            && !stderr.contains("exited without completing task"),
        "review surfaced the raw spawn failure.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("Failed to spawn agent")
            && !stderr.contains("Failed to spawn agent"),
        "review surfaced a spawn-failure error.\nstdout: {stdout}\nstderr: {stderr}"
    );

    // It must exit cleanly and report the degraded outcome.
    assert!(
        review.status.success(),
        "aiki review should exit 0 after degrading.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.to_lowercase().contains("incomplete")
            || stderr.to_lowercase().contains("incomplete"),
        "degraded review should report 'incomplete'.\nstdout: {stdout}\nstderr: {stderr}"
    );
}
