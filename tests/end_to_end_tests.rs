mod common;

use assert_cmd::Command;
use common::{init_git_repo, jj_available};
use predicates::prelude::*;
use std::fs;
use std::time::Instant;
use tempfile::tempdir;

#[test]
#[allow(deprecated)] // cargo_bin deprecated but replacement cargo_bin! macro not yet documented
fn test_complete_workflow_init_to_provenance_tracking() {
    // Skip if jj not available
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let temp_dir = tempdir().unwrap();
    let repo_path = temp_dir.path();

    // Step 1: Initialize Git repository
    init_git_repo(repo_path);
    assert!(
        repo_path.join(".git").exists(),
        "Git repository not created"
    );

    // Step 2: Run aiki init (no plugin copying needed - using global hooks)
    let mut cmd = Command::cargo_bin("aiki").unwrap();
    cmd.current_dir(repo_path).arg("init");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Initializing Aiki"))
        .stdout(predicate::str::contains("✓ Initialized JJ repository"))
        .stdout(predicate::str::contains(
            "✓ Repository initialized successfully",
        ));

    // Step 4: Verify JJ was initialized
    assert!(
        repo_path.join(".jj").exists(),
        "JJ repository not initialized"
    );

    // Step 5: Verify Git config points to global hooks
    let git_config_output = std::process::Command::new("git")
        .args(&["config", "core.hooksPath"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to check git config");
    let hooks_path = String::from_utf8_lossy(&git_config_output.stdout);
    assert!(
        hooks_path.contains(".aiki/githooks"),
        "Git hooks path should point to global hooks"
    );

    // Note: In the new architecture, .aiki directory is only created if there's a previous hooks path
    // Plugin configuration is now global (in ~/.claude/settings.json) not per-repo

    // Step 7: Create and track a test file
    let test_file = repo_path.join("test.rs");
    fs::write(&test_file, "fn main() {\n    println!(\"Hello\");\n}").unwrap();

    // Add file to git (jj tracks git-tracked files)
    std::process::Command::new("git")
        .args(&["add", "test.rs"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to git add");

    std::process::Command::new("git")
        .args(&["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to git commit");

    // Step 8: Simulate a real edit (like Claude Code would make)
    fs::write(
        &test_file,
        "fn main() {\n    println!(\"Hello, World!\");\n}",
    )
    .unwrap();

    // Step 9: Call aiki hooks stdin (simulates Claude Code PostToolUse hook)
    // The hook system uses the event bus and flow engine for provenance recording.
    let hook_input = serde_json::json!({
        "session_id": "test-session-e2e",
        "transcript_path": "/path/to/transcript.json",
        "cwd": repo_path.to_string_lossy(),
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": {
            "file_path": test_file.to_string_lossy(),
            "old_string": "println!(\"Hello\")",
            "new_string": "println!(\"Hello, World!\")"
        },
        "tool_output": ""
    });

    // Measure hook performance
    let start = Instant::now();
    let mut hooks_cmd = Command::cargo_bin("aiki").unwrap();
    let output = hooks_cmd
        .arg("hooks")
        .arg("stdin")
        .arg("--agent")
        .arg("claude-code")
        .arg("--event")
        .arg("PostToolUse")
        .write_stdin(serde_json::to_string(&hook_input).unwrap())
        .current_dir(repo_path)
        .output()
        .expect("Failed to run hooks stdin");

    let elapsed = start.elapsed();

    // Debug: Print hooks stdin output
    println!(
        "hooks stdin stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    println!(
        "hooks stdin stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    println!("hooks stdin status: {}", output.status);

    assert!(output.status.success(), "hooks stdin should succeed");

    println!(
        "⏱️  Hook execution time: {:.2}ms",
        elapsed.as_secs_f64() * 1000.0
    );

    // Note: With synchronous execution, the hook blocks until complete
    // After completion, `jj new` has created a new working copy change,
    // so the metadata is on the parent change (@-)

    // Debug: Show operation log first
    let op_log_output = std::process::Command::new("jj")
        .arg("op")
        .arg("log")
        .arg("--limit")
        .arg("10")
        .current_dir(repo_path)
        .output()
        .expect("Failed to run jj op log");
    println!(
        "Operation log:\n{}",
        String::from_utf8_lossy(&op_log_output.stdout)
    );

    // Debug: Show the log at the operation right after our hook finished
    // Use --at-op to prevent auto-snapshotting from modifying the state
    let log_at_hook_output = std::process::Command::new("jj")
        .arg("log")
        .arg("--at-op")
        .arg("@") // The head operation after our hook
        .arg("-r")
        .arg("all()")
        .arg("-T")
        .arg("change_id.short() ++ \" \" ++ description.first_line()")
        .current_dir(repo_path)
        .output()
        .expect("Failed to run jj log at op");
    println!(
        "Log immediately after hook (at op @):\n{}",
        String::from_utf8_lossy(&log_at_hook_output.stdout)
    );

    // Debug: Show full log (this will auto-snapshot)
    let log_output = std::process::Command::new("jj")
        .arg("log")
        .arg("-r")
        .arg("all()")
        .arg("-T")
        .arg("change_id.short() ++ \" \" ++ description.first_line()")
        .current_dir(repo_path)
        .output()
        .expect("Failed to run jj log");
    println!(
        "Full jj log (after auto-snapshot):\n{}",
        String::from_utf8_lossy(&log_output.stdout)
    );

    // Show what @ actually is
    let at_output = std::process::Command::new("jj")
        .arg("log")
        .arg("-r")
        .arg("@")
        .arg("-T")
        .arg(r#"change_id.short() ++ " parents: " ++ parents.map(|p| p.change_id().short()).join(", ")"#)
        .current_dir(repo_path)
        .output()
        .expect("Failed to run jj log @");
    println!(
        "@ (working copy) parents: {}",
        String::from_utf8_lossy(&at_output.stdout)
    );

    // Step 10: Verify provenance was recorded in parent change description
    let output = std::process::Command::new("jj")
        .arg("log")
        .arg("-r")
        .arg("@-")
        .arg("-T")
        .arg("description")
        .current_dir(repo_path)
        .output()
        .expect("Failed to run jj log");

    let description = String::from_utf8_lossy(&output.stdout);

    // Debug: Print the actual description
    println!("Parent change (@-) description: '{}'", description);

    // Parse provenance metadata from description
    assert!(
        description.contains("[aiki]"),
        "Description should contain [aiki] marker. Got: '{}'",
        description
    );
    assert!(
        description.contains("author=claude"),
        "Description should contain author=claude"
    );
    // Session ID is a truncated UUID v5 hash (8 hex chars from agent_type + external_id)
    assert!(
        description.contains("session="),
        "Description should contain session= field"
    );
    // Extract session ID and verify it's an 8-char hex string
    let session_line = description
        .lines()
        .find(|line| line.contains("session="))
        .expect("Should have session line");
    let session_id = session_line
        .split('=')
        .nth(1)
        .expect("Should have session value");
    assert!(
        session_id.len() == 8 && session_id.chars().all(|c| c.is_ascii_hexdigit()),
        "Session ID should be 8 hex characters (truncated UUID v5). Got: '{}'",
        session_id
    );
    assert!(
        description.contains("tool=Edit"),
        "Description should contain tool=Edit"
    );
    assert!(
        description.contains("confidence=High"),
        "Description should contain confidence=High"
    );
    assert!(
        description.contains("method=Hook"),
        "Description should contain method=Hook"
    );

    // Step 11: Verify the actual file contains the edited content
    let final_content = fs::read_to_string(&test_file).unwrap();
    assert!(
        final_content.contains("Hello, World!"),
        "File should contain edited content"
    );

    println!("✅ End-to-end test passed!");
    println!("  ✓ aiki init configured repository correctly");
    println!("  ✓ JJ repository initialized");
    println!("  ✓ Git hooks configured to use global hooks");
    println!("  ✓ File was edited: test.rs");
    println!("  ✓ hooks stdin completed successfully");
    println!("  ✓ Provenance metadata embedded in parent change description");
    println!("  ✓ Metadata format validated");
    println!("  ✓ File content verified: {:?}", final_content.trim());
    println!("  ✓ jj new created new working copy change");
}
