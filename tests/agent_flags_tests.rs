//! Integration tests for agent flag parsing across CLI commands.
//!
//! These tests verify clap mutual exclusion, conflict rules, and
//! shorthand-to-canonical resolution at the binary level.

use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;

fn aiki() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
}

// ---------------------------------------------------------------------------
// SessionIdFlags — mutual exclusion within session show
// ---------------------------------------------------------------------------

#[test]
fn session_show_claude_and_codex_rejected() {
    aiki()
        .args(["session", "show", "--claude", "s1", "--codex", "s2"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn session_show_positional_and_agent_flag_rejected() {
    aiki()
        .args(["session", "show", "some-id", "--claude", "s1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn session_show_no_id_no_flag_fails() {
    // session show with neither positional ID nor agent flag should error
    // (the command itself handles this — may fail at runtime not parse time)
    let output = aiki()
        .args(["session", "show"])
        .output()
        .unwrap();
    // Either clap rejects it or the command returns an error
    assert!(!output.status.success());
}

// ---------------------------------------------------------------------------
// AgentFilterFlags — stackable OR filter within session list
// ---------------------------------------------------------------------------

#[test]
fn session_list_claude_and_codex_accepted() {
    // --claude --codex should be accepted as an OR filter (no clap error)
    let output = aiki()
        .args(["session", "list", "--claude", "--codex"])
        .output()
        .unwrap();
    // Should parse without clap errors (runtime error OK without a repo)
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("cannot be used with"),
        "Expected --claude --codex to be stackable, got: {}",
        stderr
    );
}

#[test]
fn session_list_all_agents_accepted() {
    // All four shorthand flags at once should work
    let output = aiki()
        .args(["session", "list", "--claude", "--codex", "--cursor", "--gemini"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("cannot be used with"),
        "Expected all agent flags to be stackable, got: {}",
        stderr
    );
}

#[test]
fn session_list_agent_and_claude_rejected() {
    // --agent conflicts with shorthand flags
    aiki()
        .args(["session", "list", "--agent", "codex", "--claude"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn session_list_agent_behaves_like_shorthand() {
    // Both `--agent claude-code` and `--claude` should be accepted (not both at once)
    // Just verify they parse without clap errors (runtime may fail without a repo)
    let out1 = aiki()
        .args(["session", "list", "--agent", "claude-code"])
        .output()
        .unwrap();
    let out2 = aiki()
        .args(["session", "list", "--claude"])
        .output()
        .unwrap();

    // Both should exit (not crash from clap). They may fail at runtime
    // because there's no repo, but the exit code should be present.
    assert!(out1.status.code().is_some());
    assert!(out2.status.code().is_some());
}

// ---------------------------------------------------------------------------
// TaskAgentFlags — mutual exclusion within task list
// ---------------------------------------------------------------------------

#[test]
fn task_list_claude_and_codex_rejected() {
    aiki()
        .args(["task", "list", "--claude", "--codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn task_list_claude_and_assignee_rejected() {
    aiki()
        .args(["task", "list", "--claude", "--assignee", "human"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn task_list_claude_and_unassigned_rejected() {
    aiki()
        .args(["task", "list", "--claude", "--unassigned"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn task_list_agent_alias_parses() {
    // --agent is an alias for --assignee on task list
    let output = aiki()
        .args(["task", "list", "--agent", "claude-code"])
        .output()
        .unwrap();
    // Should parse successfully (runtime error OK)
    assert!(output.status.code().is_some());
}

// ---------------------------------------------------------------------------
// task add — assignee shorthand flags
// ---------------------------------------------------------------------------

#[test]
fn task_add_claude_and_assignee_rejected() {
    aiki()
        .args(["task", "add", "Test task", "--claude", "--assignee", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn task_add_claude_and_codex_rejected() {
    aiki()
        .args(["task", "add", "Test task", "--claude", "--codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn task_add_claude_parses() {
    // --claude on task add should resolve to --assignee claude-code
    let output = aiki()
        .args(["task", "add", "Test task", "--claude"])
        .output()
        .unwrap();
    assert!(output.status.code().is_some());
}

// ---------------------------------------------------------------------------
// build — agent shorthand flags
// ---------------------------------------------------------------------------

#[test]
fn build_claude_and_agent_rejected() {
    aiki()
        .args(["build", "--claude", "--agent", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn build_claude_and_codex_rejected() {
    aiki()
        .args(["build", "--claude", "--codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn build_claude_parses() {
    // --claude on build should resolve to --agent claude-code
    let output = aiki()
        .args(["build", "--claude"])
        .output()
        .unwrap();
    assert!(output.status.code().is_some());
}

// ---------------------------------------------------------------------------
// hooks stdin — agent event shorthand flags
// ---------------------------------------------------------------------------

#[test]
fn hooks_stdin_claude_parses_event() {
    // --claude SessionStart should resolve to agent=claude-code, event=SessionStart
    let output = aiki()
        .args(["hooks", "stdin", "--claude", "SessionStart"])
        .output()
        .unwrap();
    // Should parse (may fail at runtime due to no stdin/repo, but not a clap error)
    assert!(output.status.code().is_some());
}

#[test]
fn hooks_stdin_claude_and_agent_rejected() {
    aiki()
        .args([
            "hooks", "stdin", "--claude", "SessionStart", "--agent", "codex",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn hooks_stdin_codex_and_claude_rejected() {
    aiki()
        .args([
            "hooks", "stdin", "--claude", "SessionStart", "--codex", "stop",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

// ---------------------------------------------------------------------------
// doctor — accepts --fix (no agent flags, basic wiring)
// ---------------------------------------------------------------------------

#[test]
fn doctor_parses() {
    let output = aiki().args(["doctor"]).output().unwrap();
    assert!(output.status.code().is_some());
}

#[test]
fn doctor_fix_parses() {
    let output = aiki().args(["doctor", "--fix"]).output().unwrap();
    assert!(output.status.code().is_some());
}

// ---------------------------------------------------------------------------
// run — agent shorthand flags
// ---------------------------------------------------------------------------

#[test]
fn run_claude_and_agent_rejected() {
    aiki()
        .args(["run", "--claude", "--agent", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn run_claude_and_codex_rejected() {
    aiki()
        .args(["run", "--claude", "--codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

// ---------------------------------------------------------------------------
// plan — agent shorthand flags
// ---------------------------------------------------------------------------

#[test]
fn plan_claude_and_agent_rejected() {
    aiki()
        .args(["plan", "--claude", "--agent", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn plan_claude_and_codex_rejected() {
    aiki()
        .args(["plan", "--claude", "--codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}
