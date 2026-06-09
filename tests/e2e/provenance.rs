//! Provenance e2e tests: verify that agent sessions write [aiki] metadata
//! to JJ commit descriptions, and that `aiki task diff` can see the changes.

use super::*;
use std::time::Duration;
use tempfile::tempdir;

#[test]
#[ignore] // e2e: requires claude binary + API key
fn e2e_claude_provenance_on_trivial_change() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("claude") {
        eprintln!("Skipping: claude binary not available");
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    let task_id = create_task(repo, "e2e test: create hello.txt");
    set_task_instructions(
        repo,
        &task_id,
        "Create a file called hello.txt with the content 'hello from claude'.\n\
         Then run: aiki task close <your-task-id> --confidence 3 --summary 'Created hello.txt'\n\
         Your task ID is shown when you run `aiki task` or `aiki task list`.",
    );

    let (success, stdout, stderr) = aiki_run(repo, &task_id, Duration::from_secs(180));
    eprintln!("aiki run stdout: {stdout}");
    eprintln!("aiki run stderr: {stderr}");
    assert!(success, "aiki run failed for Claude");

    assert!(
        wait_for_task_closed(repo, &task_id, Duration::from_secs(30)),
        "Task was not closed after aiki run"
    );

    // Check file exists via jj (may be in absorbed workspace, not working copy)
    assert!(
        file_in_jj_history(repo, "hello.txt"),
        "hello.txt not found in jj history"
    );

    let commits = find_provenance_commits(repo, &task_id);
    assert!(
        !commits.is_empty(),
        "No commits found with task={task_id} provenance"
    );

    validate_provenance_fields(repo, &commits[0], &task_id);
}

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_provenance_on_trivial_change() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("codex") {
        eprintln!("Skipping: codex binary not available");
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    let task_id = create_task(repo, "e2e test: create hello.txt");
    set_task_instructions(
        repo,
        &task_id,
        "Create a file called hello.txt with the content 'hello from codex'.\n\
         Then run: aiki task close <your-task-id> --confidence 3 --summary 'Created hello.txt'\n\
         Your task ID is shown when you run `aiki task` or `aiki task list`.",
    );

    let (success, stdout, stderr) = aiki_run(repo, &task_id, Duration::from_secs(180));
    eprintln!("aiki run stdout: {stdout}");
    eprintln!("aiki run stderr: {stderr}");
    assert!(success, "aiki run failed for Codex");

    assert!(
        wait_for_task_closed(repo, &task_id, Duration::from_secs(30)),
        "Task was not closed after aiki run"
    );

    assert!(
        file_in_jj_history(repo, "hello.txt"),
        "hello.txt not found in jj history"
    );

    let commits = find_provenance_commits(repo, &task_id);
    assert!(
        !commits.is_empty(),
        "No commits found with task={task_id} provenance"
    );

    validate_provenance_fields(repo, &commits[0], &task_id);
}

/// Shared logic for task-diff tests: create two files via an agent, verify task diff output.
fn run_task_diff_test(agent: &str) {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    let task_id = create_task(repo, "e2e test: create two files");
    set_task_instructions(
        repo,
        &task_id,
        "Create two files: foo.txt with content 'foo' and bar.txt with content 'bar'.\n\
         Then run: aiki task close <your-task-id> --confidence 3 --summary 'Created files'\n\
         Your task ID is shown when you run `aiki task` or `aiki task list`.",
    );

    let (success, _, stderr) = aiki_run(repo, &task_id, Duration::from_secs(180));
    assert!(success, "aiki run failed with {agent}: {stderr}");
    assert!(
        wait_for_task_closed(repo, &task_id, Duration::from_secs(30)),
        "Task not closed"
    );

    let output = assert_cmd::Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args(["task", "diff", &task_id, "--name-only"])
        .output()
        .expect("Failed to run aiki task diff");

    let diff_output = String::from_utf8_lossy(&output.stdout);
    assert!(
        diff_output.contains("foo.txt"),
        "task diff missing foo.txt: {diff_output}"
    );
    assert!(
        diff_output.contains("bar.txt"),
        "task diff missing bar.txt: {diff_output}"
    );
}

#[test]
#[ignore] // e2e: requires claude binary + API key
fn e2e_claude_task_diff_shows_agent_changes() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("claude") {
        eprintln!("Skipping: claude binary not available");
        return;
    }
    run_task_diff_test("claude-code");
}

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_task_diff_shows_agent_changes() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("codex") {
        eprintln!("Skipping: codex binary not available");
        return;
    }
    run_task_diff_test("codex");
}
