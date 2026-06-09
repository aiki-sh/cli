//! Task lifecycle e2e tests: subtask completion, parent task closure,
//! and background session termination when thread tail closes.

use super::*;
use std::time::Duration;
use tempfile::tempdir;

/// Strip ANSI escape codes from a string
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c == 'm' {
                in_escape = false;
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Helper: close a task with a summary
#[allow(dead_code)]
fn close_task(repo_path: &Path, task_id: &str, summary: &str) {
    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo_path)
        .args([
            "task", "close", task_id, "--confidence", "3", "--summary", summary,
        ])
        .output()
        .expect("Failed to close task");

    assert!(
        output.status.success(),
        "aiki task close failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Helper: start a task by ID and return stdout
fn start_task(repo_path: &Path, task_id: &str) -> String {
    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo_path)
        .args(["task", "start", task_id])
        .output()
        .expect("Failed to start task");

    assert!(
        output.status.success(),
        "aiki task start failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Helper: get task list output
fn get_task_list(repo_path: &Path) -> String {
    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo_path)
        .args(["task", "list"])
        .output()
        .expect("Failed to list tasks");

    String::from_utf8_lossy(&output.stdout).to_string()
}

// =============================================================================
// Subtask visibility tests (no agent required — pure CLI)
// =============================================================================

#[test]
#[ignore] // e2e: requires jj
fn e2e_start_by_id_shows_subtask_listing() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    // Create parent with subtasks
    let parent_id = create_task(repo, "Parent task");

    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args(["task", "add", "--subtask-of", &parent_id, "Subtask A"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args(["task", "add", "--subtask-of", &parent_id, "Subtask B"])
        .output()
        .unwrap();
    assert!(output.status.success());

    // Start parent by ID
    let start_output = start_task(repo, &parent_id);

    // Should show subtask listing
    assert!(
        start_output.contains("Subtasks (0/2"),
        "Missing subtask listing in start output: {start_output}"
    );
    assert!(
        start_output.contains("Subtask A"),
        "Missing Subtask A in start output: {start_output}"
    );
    assert!(
        start_output.contains("Subtask B"),
        "Missing Subtask B in start output: {start_output}"
    );
    // Should show tip to start first subtask
    assert!(
        start_output.contains("aiki task start"),
        "Missing start tip in start output: {start_output}"
    );
}

#[test]
#[ignore] // e2e: requires jj
fn e2e_start_quickstart_does_not_show_subtasks() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    // Quick-start (pass description, not ID)
    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args(["task", "start", "Quick task"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should be slim: just "Started <id>"
    assert!(stdout.starts_with("Started "), "Unexpected output: {stdout}");
    assert!(
        !stdout.contains("Subtasks"),
        "Quick-start should not show subtask listing: {stdout}"
    );
}

#[test]
#[ignore] // e2e: requires jj
fn e2e_subtasks_visible_in_ready_queue_when_parent_in_progress() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    // Create parent with subtasks
    let parent_id = create_task(repo, "Parent with children");

    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args(["task", "add", "--subtask-of", &parent_id, "Child A"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let _child_a_id = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .find(|w| w.len() >= 3 && w.chars().all(|c| matches!(c, 'k'..='z')))
        .unwrap()
        .to_string();

    // Start parent
    start_task(repo, &parent_id);

    // Task list should show children, not root tasks
    let list = get_task_list(repo);
    assert!(
        list.contains("Child A"),
        "Subtask should appear in ready queue: {list}"
    );
    // Parent should NOT appear in the Ready section (it's in-progress)
    let ready_section = list.split("Ready").nth(1).unwrap_or("");
    assert!(
        !ready_section.contains("Parent with children"),
        "Parent should not appear in ready queue: {list}"
    );
}

// =============================================================================
// Subtask completion + parent closure (agent-driven)
// =============================================================================

/// Shared logic: build a plan, then review the epic.
/// `reviewer` is passed as `--agent` to `aiki review`.
fn run_build_and_review(reviewer: &str) {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    // Create a plan file for aiki build
    let plan_dir = repo.join("ops").join("now");
    std::fs::create_dir_all(&plan_dir).unwrap();
    std::fs::write(
        plan_dir.join("e2e-greeting.md"),
        "---\nstatus: ready\n---\n\n\
         # Greeting Module\n\n\
         ## Summary\n\n\
         Create a simple greeting module with two files.\n\n\
         ## Requirements\n\n\
         1. Create `src/greet.py` with a function `greet(name)` that returns `Hello, {name}!`\n\
         2. Create `tests/test_greet.py` with a test that calls `greet('World')` and asserts the result\n\n\
         ## Acceptance Criteria\n\n\
         - `greet('World')` returns `'Hello, World!'`\n\
         - Test file exists and contains at least one assertion\n",
    )
    .unwrap();

    // Build the plan — creates an epic with subtasks and implements them
    let build_output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args(["build", "ops/now/e2e-greeting.md"])
        .timeout(Duration::from_secs(600))
        .output()
        .expect("Failed to run aiki build");

    let build_stdout = String::from_utf8_lossy(&build_output.stdout);
    let build_stderr = String::from_utf8_lossy(&build_output.stderr);
    eprintln!("Build stdout: {build_stdout}");
    eprintln!("Build stderr: {build_stderr}");
    assert!(
        build_output.status.success(),
        "aiki build failed: {build_stderr}"
    );

    // Find the epic ID: strip ANSI codes, look for "[<id>] Epic:" pattern
    let clean_stdout = strip_ansi(&build_stdout);
    let clean_stderr = strip_ansi(&build_stderr);

    let epic_id = clean_stdout
        .lines()
        .chain(clean_stderr.lines())
        .find_map(|line| {
            if let Some(bracket_start) = line.find('[') {
                let rest = &line[bracket_start + 1..];
                if let Some(bracket_end) = rest.find(']') {
                    let candidate = rest[..bracket_end].trim();
                    if candidate.len() >= 3
                        && candidate.chars().all(|c| matches!(c, 'k'..='z'))
                    {
                        return Some(candidate.to_string());
                    }
                }
            }
            if line.contains("task diff ") {
                line.split_whitespace()
                    .last()
                    .filter(|w| {
                        w.len() >= 3 && w.chars().all(|c| matches!(c, 'k'..='z'))
                    })
                    .map(|w| w.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            panic!(
                "Could not find epic ID in build output.\nStdout: {clean_stdout}\nStderr: {clean_stderr}"
            );
        });

    eprintln!("Epic ID: {epic_id}");

    // Review the epic with the specified agent
    let review_output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args(["review", &epic_id, "--agent", reviewer])
        .timeout(Duration::from_secs(300))
        .output()
        .expect("Failed to run aiki review");

    let stdout = String::from_utf8_lossy(&review_output.stdout);
    let stderr = String::from_utf8_lossy(&review_output.stderr);
    eprintln!("Review stdout: {stdout}");
    eprintln!("Review stderr: {stderr}");

    // Review must complete successfully
    assert!(
        review_output.status.success(),
        "aiki review failed with {reviewer}: {stderr}"
    );

    // Review output should show completion with issue count
    assert!(
        stdout.contains("Completed") || stdout.contains("approved") || stdout.contains("issues"),
        "Review output should show completion status: {stdout}"
    );
}

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_review_follows_subtask_workflow() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("codex") {
        eprintln!("Skipping: codex binary not available");
        return;
    }
    run_build_and_review("codex");
}

#[test]
#[ignore] // e2e: requires claude binary + API key
fn e2e_claude_review_follows_subtask_workflow() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("claude") {
        eprintln!("Skipping: claude binary not available");
        return;
    }
    run_build_and_review("claude-code");
}

// =============================================================================
// Background session ends when thread tail closes
// =============================================================================

/// Shared logic: run a task that creates done.txt and closes itself, verify session ends.
fn run_session_ends_on_close(agent_args: &[&str]) {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    let task_id = create_task(repo, "e2e test: create done.txt and close");
    set_task_instructions(
        repo,
        &task_id,
        "Create a file called done.txt with content 'done'.\n\
         Then close this task: aiki task close <your-task-id> --confidence 3 --summary 'Created done.txt'\n\
         Your task ID is shown when you run `aiki task list`.",
    );

    let mut args = vec!["run", &task_id];
    args.extend_from_slice(agent_args);

    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args(&args)
        .timeout(Duration::from_secs(180))
        .output()
        .expect("Failed to run aiki run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("aiki run stdout: {stdout}");
    eprintln!("aiki run stderr: {stderr}");

    assert!(
        output.status.success(),
        "aiki run should complete when task closes: {stderr}"
    );

    assert!(
        wait_for_task_closed(repo, &task_id, Duration::from_secs(10)),
        "Task should be closed"
    );
}

#[test]
#[ignore] // e2e: requires claude binary + API key
fn e2e_claude_background_session_ends_when_task_closes() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("claude") {
        eprintln!("Skipping: claude binary not available");
        return;
    }
    run_session_ends_on_close(&[]);
}

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_background_session_ends_when_task_closes() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("codex") {
        eprintln!("Skipping: codex binary not available");
        return;
    }
    run_session_ends_on_close(&["--agent", "codex"]);
}

// =============================================================================
// Loop: subtask orchestration exits cleanly via end_session
// =============================================================================

/// Shared logic: run `aiki loop` with a given agent on a parent with subtasks.
/// Verifies:
/// 1. All subtasks complete
/// 2. The loop command exits with success (no AgentSpawnFailed)
/// 3. Parent task is closed
fn run_loop_exits_cleanly(agent: &str, agent_flag: &str) {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    // Create parent with 2 subtasks
    let parent_id = create_task(repo, "Loop test parent");

    let sub1 = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args([
            "task",
            "add",
            "--subtask-of",
            &parent_id,
            "Step 1: echo hello",
            "-i",
            "Run 'echo hello' then close this task with confidence 4.",
            agent_flag,
        ])
        .output()
        .expect("Failed to add subtask 1");
    assert!(sub1.status.success());

    let sub2 = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args([
            "task",
            "add",
            "--subtask-of",
            &parent_id,
            "Step 2: echo world",
            "-i",
            "Run 'echo world' then close this task with confidence 4.",
            agent_flag,
        ])
        .output()
        .expect("Failed to add subtask 2");
    assert!(sub2.status.success());

    // Start the parent
    start_task(repo, &parent_id);

    // Run loop with the specified agent
    let output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args(["loop", &parent_id, agent_flag])
        .timeout(Duration::from_secs(180))
        .output()
        .expect("Failed to run aiki loop");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("[{agent}] loop stdout: {stdout}");
    eprintln!("[{agent}] loop stderr: {stderr}");

    // Loop should exit successfully (no AgentSpawnFailed)
    assert!(
        output.status.success(),
        "[{agent}] aiki loop should exit cleanly: {stderr}"
    );
    assert!(
        !stderr.contains("Failed to spawn agent"),
        "[{agent}] Should not report spawn failure: {stderr}"
    );

    // Parent task should be closed
    assert!(
        wait_for_task_closed(repo, &parent_id, Duration::from_secs(10)),
        "[{agent}] Parent task should be closed after loop completes"
    );
}

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_loop_exits_cleanly() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("codex") {
        eprintln!("Skipping: codex binary not available");
        return;
    }
    run_loop_exits_cleanly("codex", "--codex");
}

#[test]
#[ignore] // e2e: requires claude binary + API key
fn e2e_claude_loop_exits_cleanly() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("claude") {
        eprintln!("Skipping: claude binary not available");
        return;
    }
    run_loop_exits_cleanly("claude", "--claude");
}

// =============================================================================
// Build: codex builds a plan and changes are absorbed into the repo
// =============================================================================

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_build_absorbs_changes() {
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

    // Create a plan file
    let plan_dir = repo.join("ops").join("now");
    std::fs::create_dir_all(&plan_dir).unwrap();
    std::fs::write(
        plan_dir.join("e2e-greeting.md"),
        "---\nstatus: ready\n---\n\n\
         # Greeting Module\n\n\
         ## Summary\n\n\
         Create a simple greeting module with two files.\n\n\
         ## Requirements\n\n\
         1. Create `src/greet.py` with a function `greet(name)` that returns `Hello, {name}!`\n\
         2. Create `tests/test_greet.py` with a test that calls `greet('World')` and asserts the result\n\n\
         ## Acceptance Criteria\n\n\
         - `greet('World')` returns `'Hello, World!'`\n\
         - Test file exists and contains at least one assertion\n",
    )
    .unwrap();

    // Build with codex as the agent
    let build_output = Command::cargo_bin("aiki")
        .unwrap()
        .current_dir(repo)
        .args(["build", "ops/now/e2e-greeting.md", "--codex"])
        .timeout(Duration::from_secs(600))
        .output()
        .expect("Failed to run aiki build");

    let build_stdout = String::from_utf8_lossy(&build_output.stdout);
    let build_stderr = String::from_utf8_lossy(&build_output.stderr);
    eprintln!("Build stdout: {build_stdout}");
    eprintln!("Build stderr: {build_stderr}");
    assert!(
        build_output.status.success(),
        "aiki build --codex failed: {build_stderr}"
    );

    // Verify build changes were absorbed into the main working copy.
    // The plan asks for src/greet.py — check it exists in the repo root,
    // not just inside an isolated workspace.
    let greet_exists = repo.join("src").join("greet.py").exists();
    let test_exists = repo.join("tests").join("test_greet.py").exists();

    // List what's actually in the repo for debugging
    let files_output = std::process::Command::new("find")
        .args([".", "-name", "*.py", "-not", "-path", "./.jj/*"])
        .current_dir(repo)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    eprintln!("Python files in repo: {files_output}");

    assert!(
        greet_exists || test_exists,
        "No build artifacts found in repo — workspace changes were not absorbed.\n\
         Expected src/greet.py or tests/test_greet.py in {repo:?}",
        repo = repo,
    );
}
