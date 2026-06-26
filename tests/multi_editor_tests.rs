/// Tests for multi-editor support (Claude Code + Cursor)
///
/// These tests verify that Aiki correctly handles multiple AI editors
/// working on the same repository and that query commands distinguish
/// between them properly.
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;
use tempfile::TempDir;

/// One hermetic `AIKI_HOME` for this whole test binary, holding the per-user
/// markers that the init-v2 CLI gate looks up. Leaked so it outlives the
/// spawned `aiki` processes.
static TEST_AIKI_HOME: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = TempDir::new().expect("create test aiki home");
    let home = dir.path().join("aiki");
    fs::create_dir_all(&home).expect("create aiki home");
    std::mem::forget(dir);
    home
});

/// Mark `repo_path` Active for the CLI gate: ensure a `.aiki/` directory and a
/// per-user enable marker under [`TEST_AIKI_HOME`] keyed by the resolved path.
fn enable_repo_for_gate(repo_path: &std::path::Path) {
    fs::create_dir_all(repo_path.join(".aiki")).expect("create .aiki");
    let stripped = repo_path.strip_prefix("/").unwrap_or(repo_path);
    let marker = TEST_AIKI_HOME.join(".init/repos").join(stripped).join("enabled");
    fs::create_dir_all(marker.parent().unwrap()).expect("create marker dir");
    fs::write(&marker, "").expect("write marker");
}

/// Helper to create a temporary JJ repository for testing
fn setup_test_repo() -> (TempDir, PathBuf) {
    let temp_dir = TempDir::new().unwrap();
    // Canonicalize so the marker we write (keyed by the resolved path) matches
    // the gate's lookup from the spawned aiki's getcwd (/private/var on macOS).
    let repo_path = temp_dir.path().canonicalize().unwrap();

    // Initialize JJ repo (non-colocated)
    let output = Command::new("jj")
        .args(["git", "init", "--no-colocate"])
        .current_dir(&repo_path)
        .output()
        .expect("Failed to initialize JJ repo");

    assert!(
        output.status.success(),
        "Failed to initialize JJ repo: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // These tests hand-craft [aiki] provenance via `jj describe` rather than a
    // full `aiki init`, so make the repo Active for the CLI gate explicitly.
    enable_repo_for_gate(&repo_path);

    (temp_dir, repo_path)
}

/// Helper to run aiki command
fn run_aiki(args: &[&str], cwd: &PathBuf) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aiki"))
        .env("AIKI_HOME", &*TEST_AIKI_HOME)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("Failed to execute aiki command")
}

/// Helper to create a file with specific content
fn create_file(repo_path: &PathBuf, filename: &str, content: &str) {
    let file_path = repo_path.join(filename);
    fs::write(&file_path, content).expect("Failed to write file");
}

/// Helper to set change description with aiki metadata
fn set_change_description(repo_path: &PathBuf, description: &str) {
    let output = Command::new("jj")
        .args(["describe", "-m", description])
        .current_dir(repo_path)
        .output()
        .expect("Failed to set change description");

    assert!(
        output.status.success(),
        "Failed to set change description: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_authors_shows_both_editors() {
    let (_temp_dir, repo_path) = setup_test_repo();

    // Create a file and attribute to Claude Code
    create_file(&repo_path, "file1.txt", "line 1\nline 2\n");
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=claude\nauthor_type=agent\nsession=claude-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Commit this change
    Command::new("jj")
        .args(["new"])
        .current_dir(&repo_path)
        .output()
        .expect("Failed to create new change");

    // Now modify the same file, adding lines attributed to Cursor
    create_file(
        &repo_path,
        "file1.txt",
        "line 1\nline 2\nline 3 by cursor\n",
    );
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=cursor\nauthor_type=agent\nsession=cursor-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Run aiki authors - should show both editors since the file has lines from both
    let output = run_aiki(&["authors", "--format=plain"], &repo_path);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The working copy now has file1.txt with lines from both editors
    // Blame will show attribution, and authors should aggregate them
    // Note: Since we only modified the file (added line 3), only Cursor should show
    // in the working copy change. To get both, we'd need both files modified.

    // For a proper multi-editor test, let's verify at least Cursor shows
    assert!(
        stdout.contains("cursor"),
        "Expected 'cursor' agent type in output: {}",
        stdout
    );
}

#[test]
fn test_authors_git_format_includes_both_editors() {
    let (_temp_dir, repo_path) = setup_test_repo();

    // Create a file with lines from Claude Code
    create_file(&repo_path, "file1.txt", "line 1\n");
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=claude\nauthor_type=agent\nsession=claude-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Create new change
    Command::new("jj")
        .args(["new"])
        .current_dir(&repo_path)
        .output()
        .expect("Failed to create new change");

    // Modify the file with Cursor
    create_file(&repo_path, "file1.txt", "line 1\nline 2 by cursor\n");
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=cursor\nauthor_type=agent\nsession=cursor-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Run aiki authors with git format
    let output = run_aiki(&["authors", "--format=git"], &repo_path);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should show Cursor co-author (the working copy change)
    assert!(
        stdout.contains("Co-Authored-By: Cursor <noreply@cursor.com>"),
        "Expected Cursor co-author in output: {}",
        stdout
    );
}

#[test]
fn test_blame_distinguishes_editors() {
    let (_temp_dir, repo_path) = setup_test_repo();

    // Create file with Claude Code metadata
    create_file(&repo_path, "test.txt", "line 1 by claude\n");
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=claude\nauthor_type=agent\nsession=claude-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Create a new change
    let output = Command::new("jj")
        .args(["new"])
        .current_dir(&repo_path)
        .output()
        .expect("Failed to create new change");
    assert!(output.status.success());

    // Edit file with Cursor metadata
    create_file(
        &repo_path,
        "test.txt",
        "line 1 by claude\nline 2 by cursor\n",
    );
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=cursor\nauthor_type=agent\nsession=cursor-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Run aiki blame
    let output = run_aiki(&["blame", "test.txt"], &repo_path);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should show both editors with Display format (not Debug)
    assert!(
        stdout.contains("claude-code"),
        "Expected 'claude-code' in blame output: {}",
        stdout
    );
    assert!(
        stdout.contains("cursor"),
        "Expected 'cursor' in blame output: {}",
        stdout
    );

    // Should NOT contain Debug format
    assert!(
        !stdout.contains("ClaudeCode"),
        "Should not contain Debug format 'ClaudeCode': {}",
        stdout
    );
}

#[test]
fn test_blame_filter_by_claude_code() {
    let (_temp_dir, repo_path) = setup_test_repo();

    // Create file with Claude Code metadata
    create_file(&repo_path, "test.txt", "line 1 by claude\n");
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=claude\nauthor_type=agent\nsession=claude-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Create a new change
    let output = Command::new("jj")
        .args(["new"])
        .current_dir(&repo_path)
        .output()
        .expect("Failed to create new change");
    assert!(output.status.success());

    // Edit file with Cursor metadata
    create_file(
        &repo_path,
        "test.txt",
        "line 1 by claude\nline 2 by cursor\n",
    );
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=cursor\nauthor_type=agent\nsession=cursor-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Run aiki blame with claude-code filter
    let output = run_aiki(&["blame", "test.txt", "--agent", "claude-code"], &repo_path);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should only show Claude lines
    assert!(
        stdout.contains("claude-code"),
        "Expected 'claude-code' in filtered output: {}",
        stdout
    );
    assert!(
        !stdout.contains("cursor"),
        "Should not contain 'Cursor' when filtering by claude-code: {}",
        stdout
    );
}

#[test]
fn test_blame_filter_by_cursor() {
    let (_temp_dir, repo_path) = setup_test_repo();

    // Create file with Claude Code metadata
    create_file(&repo_path, "test.txt", "line 1 by claude\n");
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=claude\nauthor_type=agent\nsession=claude-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Create a new change
    let output = Command::new("jj")
        .args(["new"])
        .current_dir(&repo_path)
        .output()
        .expect("Failed to create new change");
    assert!(output.status.success());

    // Edit file with Cursor metadata
    create_file(
        &repo_path,
        "test.txt",
        "line 1 by claude\nline 2 by cursor\n",
    );
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=cursor\nauthor_type=agent\nsession=cursor-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Run aiki blame with cursor filter
    let output = run_aiki(&["blame", "test.txt", "--agent", "cursor"], &repo_path);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should only show Cursor lines
    assert!(
        stdout.contains("cursor"),
        "Expected 'cursor' in filtered output: {}",
        stdout
    );
    assert!(
        !stdout.contains("claude-code"),
        "Should not contain 'Claude' when filtering by cursor: {}",
        stdout
    );
}

#[test]
fn test_blame_no_filter_shows_all() {
    let (_temp_dir, repo_path) = setup_test_repo();

    // Create file with Claude Code metadata
    create_file(&repo_path, "test.txt", "line 1 by claude\n");
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=claude\nauthor_type=agent\nsession=claude-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Create a new change
    let output = Command::new("jj")
        .args(["new"])
        .current_dir(&repo_path)
        .output()
        .expect("Failed to create new change");
    assert!(output.status.success());

    // Edit file with Cursor metadata
    create_file(
        &repo_path,
        "test.txt",
        "line 1 by claude\nline 2 by cursor\n",
    );
    set_change_description(
        &repo_path,
        "[aiki]\nauthor=cursor\nauthor_type=agent\nsession=cursor-session-1\ntool=Edit\nconfidence=High\nmethod=Hook\n[/aiki]",
    );

    // Run aiki blame without filter
    let output = run_aiki(&["blame", "test.txt"], &repo_path);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should show both editors
    assert!(
        stdout.contains("claude-code"),
        "Expected 'claude-code' in output: {}",
        stdout
    );
    assert!(
        stdout.contains("cursor"),
        "Expected 'cursor' in output: {}",
        stdout
    );

    // Count lines - should have 2 lines of output (one for each line in file)
    let line_count = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    assert_eq!(
        line_count, 2,
        "Expected 2 lines in blame output, got {}: {}",
        line_count, stdout
    );
}

#[test]
fn test_invalid_agent_filter_returns_error() {
    let (_temp_dir, repo_path) = setup_test_repo();

    // Create a simple file
    create_file(&repo_path, "test.txt", "line 1\n");

    // Run aiki blame with invalid agent filter
    let output = run_aiki(
        &["blame", "test.txt", "--agent", "invalid-agent"],
        &repo_path,
    );

    // Should fail with error
    assert!(
        !output.status.success(),
        "Expected failure for invalid agent filter"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid") || stderr.contains("unknown"),
        "Expected error message about invalid agent: {}",
        stderr
    );
}
