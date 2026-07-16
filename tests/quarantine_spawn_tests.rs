//! Consumer-path tests for the quarantine spawn pre-flight and the
//! session-discovery timeout enrichment (test matrix rows (a)–(e)).
//!
//! Everything drives the real `aiki` binary through `aiki run`; run mode is
//! set at the real boundary (the spawned binary's stdout is a pipe, which is
//! headless by construction — the classifier never looks at stderr). The
//! quarantine status is forced through the debug-build
//! `AIKI_TEST_QUARANTINE_STATUS` seam so every row runs on any platform under
//! plain `cargo test`; the interactive (PTY) direction is covered by the pure
//! decision-table unit tests in `cli/src/agents/runtime/cli.rs`.

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
        .expect("run aiki init");
    assert!(
        output.status.success(),
        "aiki init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Create a claude-assigned task with instructions, returning its short ID.
fn add_task(path: &Path) -> String {
    let output = common::aiki_cmd()
        .current_dir(path)
        .args([
            "task",
            "add",
            "Quarantine spawn test task",
            "-i",
            "Exercise the spawn pre-flight.",
            "--assignee",
            "claude-code",
        ])
        .output()
        .expect("aiki task add");
    assert!(
        output.status.success(),
        "task add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    // "Added <short-id>" — a unique prefix, which every task command accepts.
    combined
        .split_whitespace()
        .skip_while(|w| !w.starts_with("Added"))
        .nth(1)
        .unwrap_or_else(|| panic!("no task id in add output: {combined}"))
        .trim_end_matches(|c: char| !c.is_ascii_lowercase())
        .to_string()
}

fn run_task(path: &Path, task_id: &str, envs: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = common::aiki_cmd();
    cmd.current_dir(path)
        .args(["run", task_id, "--claude", "--async"]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().expect("aiki run")
}

fn task_show(path: &Path, task_id: &str) -> String {
    let output = common::aiki_cmd()
        .current_dir(path)
        .args(["task", "show", task_id])
        .output()
        .expect("aiki task show");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Rows (a) + (d): a headless run (stdout piped by construction) against a
/// binary with a well-formed pending quarantine xattr fails fast with the
/// typed error — including the fix line — instead of warning and hanging to
/// the discovery timeout; the reservation rolls back so the task is not
/// stranded; and the failure is not masked as AgentNotInstalled.
#[test]
fn headless_pending_fails_fast_with_typed_error_and_releases_task() {
    let temp = tempfile::tempdir().unwrap();
    init_aiki_repo(temp.path());
    let task_id = add_task(temp.path());

    let start = std::time::Instant::now();
    let output = run_task(
        temp.path(),
        &task_id,
        &[("AIKI_TEST_QUARANTINE_STATUS", "pending")],
    );
    let elapsed = start.elapsed();

    assert!(!output.status.success(), "run must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("claude is quarantined by macOS Gatekeeper and cannot launch unattended"),
        "typed BinaryQuarantined message expected, got: {stderr}"
    );
    assert!(
        stderr.contains("aiki doctor --fix --quarantined --claude"),
        "fix line expected, got: {stderr}"
    );
    assert!(
        !stderr.contains("is not available"),
        "must not be masked as AgentNotInstalled: {stderr}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "fail-fast must not wait out the 30s discovery timeout (took {elapsed:?})"
    );

    // The claim rolled back: the task is back to open (ready), not stuck
    // Reserved or InProgress.
    let show = task_show(temp.path(), &task_id);
    assert!(
        show.contains("Status: open"),
        "task must be released back to open/ready, got: {show}"
    );
}

/// Row (b): the escape hatch downgrades the headless hard failure to a
/// warning and the spawn proceeds — the stub agent really runs (it copies the
/// scripted transcript, which is the proof of exec).
#[test]
fn skip_env_downgrades_headless_pending_to_warning_and_spawn_proceeds() {
    let temp = tempfile::tempdir().unwrap();
    init_aiki_repo(temp.path());
    let task_id = add_task(temp.path());

    let src = temp.path().join("transcript-src.jsonl");
    std::fs::write(&src, "{}\n").unwrap();
    let dest = temp.path().join("proof-of-exec.jsonl");

    let output = run_task(
        temp.path(),
        &task_id,
        &[
            ("AIKI_TEST_QUARANTINE_STATUS", "pending"),
            ("AIKI_SKIP_QUARANTINE_CHECK", "1"),
            ("AIKI_TEST_SESSION_DISCOVERY_TIMEOUT_MS", "1500"),
            (common::FAKE_TRANSCRIPT_SRC_ENV, src.to_str().unwrap()),
            (common::FAKE_TRANSCRIPT_DEST_ENV, dest.to_str().unwrap()),
        ],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[aiki] Warning: claude is quarantined by macOS Gatekeeper"),
        "hard failure must downgrade to the warning, got: {stderr}"
    );
    assert!(
        !stderr.contains("cannot launch unattended"),
        "no hard failure with the escape hatch set: {stderr}"
    );

    // The stub agent ran to completion: it copied src → dest before exiting.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !dest.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "stub agent never ran (spawn did not proceed); stderr: {stderr}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Row (e), Pending case: the stub registers no session, so discovery
    // times out and the error names quarantine as the probable cause.
    assert!(
        stderr.contains("Probable cause: claude is quarantined by macOS Gatekeeper"),
        "enriched timeout message expected, got: {stderr}"
    );
    assert!(
        stderr.contains("aiki doctor --fix --quarantined --claude"),
        "fix command expected in timeout message: {stderr}"
    );
}

/// Row (e), Undetermined case: an uninterpretable attribute soft-warns at
/// spawn (never a hard failure, even headless) and the discovery timeout
/// hedges to a possible cause.
#[test]
fn undetermined_warns_and_timeout_hedges_possible_cause() {
    let temp = tempfile::tempdir().unwrap();
    init_aiki_repo(temp.path());
    let task_id = add_task(temp.path());

    let output = run_task(
        temp.path(),
        &task_id,
        &[
            ("AIKI_TEST_QUARANTINE_STATUS", "undetermined"),
            ("AIKI_TEST_SESSION_DISCOVERY_TIMEOUT_MS", "1000"),
        ],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("[aiki] Warning: claude carries a macOS quarantine attribute"),
        "soft warning expected, got: {stderr}"
    );
    assert!(
        !stderr.contains("cannot launch unattended"),
        "undetermined must never hard-fail: {stderr}"
    );
    assert!(
        stderr.contains("Possible cause: claude carries a macOS quarantine attribute"),
        "hedged (possible, not probable) timeout message expected, got: {stderr}"
    );
    assert!(
        stderr.contains("aiki doctor --fix --quarantined --claude"),
        "fix command expected: {stderr}"
    );
}

/// Row (c), CheckFailed leg + row (e) generic leg: a failed inspection
/// proceeds silently (no warning, no error) and the discovery timeout keeps
/// today's generic message.
#[test]
fn checkfailed_proceeds_silently_and_timeout_stays_generic() {
    let temp = tempfile::tempdir().unwrap();
    init_aiki_repo(temp.path());
    let task_id = add_task(temp.path());

    let output = run_task(
        temp.path(),
        &task_id,
        &[
            ("AIKI_TEST_QUARANTINE_STATUS", "checkfailed"),
            ("AIKI_TEST_SESSION_DISCOVERY_TIMEOUT_MS", "1000"),
        ],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("[aiki] Warning"),
        "checkfailed must proceed silently, got: {stderr}"
    );
    assert!(
        stderr.contains("Session UUID not discovered within timeout"),
        "generic timeout message must be unchanged, got: {stderr}"
    );
    assert!(
        !stderr.contains("Probable cause") && !stderr.contains("Possible cause"),
        "no quarantine enrichment for CheckFailed: {stderr}"
    );
}

/// Row (e), clean leg: with no quarantine attribute at all, the discovery
/// timeout message is exactly today's generic one.
#[test]
fn unquarantined_timeout_message_is_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    init_aiki_repo(temp.path());
    let task_id = add_task(temp.path());

    let output = run_task(
        temp.path(),
        &task_id,
        &[("AIKI_TEST_SESSION_DISCOVERY_TIMEOUT_MS", "1000")],
    );

    assert!(!output.status.success(), "discovery must time out");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Session UUID not discovered within timeout"),
        "generic timeout message expected, got: {stderr}"
    );
    assert!(
        !stderr.contains("quarantine") && !stderr.contains("Gatekeeper"),
        "no quarantine mention on a clean binary: {stderr}"
    );
}
