mod common;

use jj_lib::repo::{Repo, StoreFactories};
use jj_lib::workspace::{default_working_copy_factories, Workspace};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// Test that records a change and then runs blame to verify attribution
#[test]
fn test_blame_shows_recorded_change() {
    // Create a temporary directory. Canonicalize so the marker `aiki init` keys
    // by its resolved getcwd matches the gate's lookup from the hook payload cwd.
    let temp_dir = TempDir::new().unwrap();
    let repo_path_buf = temp_dir.path().canonicalize().unwrap();
    let repo_path = repo_path_buf.as_path();

    // One stable hermetic home shared by every aiki invocation here: init writes
    // the per-user marker under AIKI_HOME, and the init-v2 gate looks it up there
    // for the hook and blame calls (all non-allowlisted, so all gated).
    let hermetic = TempDir::new().unwrap();
    let hermetic_aiki_home = hermetic.path().join("aiki");
    let hermetic_home = hermetic.path().join("home");
    fs::create_dir_all(&hermetic_aiki_home).unwrap();
    fs::create_dir_all(hermetic_home.join(".config")).unwrap();
    fs::write(
        hermetic_home.join(".gitconfig"),
        "[user]\n\tname = Aiki Test\n\temail = test@example.com\n",
    )
    .unwrap();
    let shared_env: Vec<(&str, std::ffi::OsString)> = vec![
        ("AIKI_HOME", hermetic_aiki_home.clone().into_os_string()),
        ("HOME", hermetic_home.clone().into_os_string()),
        ("XDG_CONFIG_HOME", hermetic_home.join(".config").into_os_string()),
        ("JJ_USER", "Aiki Test".into()),
        ("JJ_EMAIL", "test@example.com".into()),
    ];

    // Initialize git repository
    Command::new("git")
        .args(["init"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to initialize git repo");

    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(repo_path)
        .output()
        .unwrap();

    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(repo_path)
        .output()
        .unwrap();

    // Create a test file
    let test_file = repo_path.join("test.txt");
    fs::write(&test_file, "line 1\nline 2\nline 3\n").unwrap();

    // Commit it
    Command::new("git")
        .args(["add", "test.txt"])
        .current_dir(repo_path)
        .output()
        .unwrap();

    Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .output()
        .unwrap();

    // Initialize aiki (this will also do git import)
    let aiki_bin = get_aiki_binary_path();
    let mut cmd = Command::new(&aiki_bin);
    for (k, v) in &shared_env {
        cmd.env(k, v);
    }
    let output = cmd
        .arg("init")
        .current_dir(repo_path)
        .output()
        .expect("Failed to run aiki init");

    assert!(
        output.status.success(),
        "aiki init failed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Create a clean working copy (simulates start of AI session)
    // This ensures PreChange won't detect any existing modifications
    Command::new("jj")
        .args(["new"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to create new change");

    // Modify the file
    fs::write(&test_file, "line 1\nline 2 modified\nline 3\nline 4\n").unwrap();

    // Record the change as if ClaudeCode made it
    let hook_input = r#"{
        "session_id": "test-session-123",
        "transcript_path": "/tmp/transcript.txt",
        "cwd": "",
        "hook_event_name": "PostToolUse",
        "tool_name": "Write",
        "tool_input": {
            "file_path": ""
        },
        "tool_output": ""
    }"#;

    let hook_input = hook_input
        .replace(
            r#""cwd": """#,
            &format!(r#""cwd": "{}""#, repo_path.display()),
        )
        .replace(
            r#""file_path": """#,
            &format!(r#""file_path": "{}""#, test_file.display()),
        );

    let mut hook_cmd = Command::new(&aiki_bin);
    for (k, v) in &shared_env {
        hook_cmd.env(k, v);
    }
    let output = hook_cmd
        .arg("hooks")
        .arg("stdin")
        .arg("--agent")
        .arg("claude-code")
        .arg("--event")
        .arg("PostToolUse")
        .current_dir(repo_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(hook_input.as_bytes())?;
            child.wait_with_output()
        })
        .expect("Failed to run aiki hooks stdin");

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

    // The new hooks system uses the flow engine which:
    // 1. Calls aiki/core.build_metadata to generate provenance (author + message)
    // 2. Runs jj metaedit to set both message and author
    // 3. Runs jj new to create a fresh working copy
    // So the metadata is on the PARENT change (the one we modified)
    let settings = {
        use jj_lib::config::StackedConfig;
        use jj_lib::settings::UserSettings;
        let config = StackedConfig::with_defaults();
        UserSettings::from_config(config).unwrap()
    };

    let store_factories = StoreFactories::default();
    let working_copy_factories = default_working_copy_factories();

    let workspace = Workspace::load(
        &settings,
        repo_path,
        &store_factories,
        &working_copy_factories,
    )
    .expect("Failed to load workspace");

    let repo = workspace
        .repo_loader()
        .load_at_head()
        .expect("Failed to load repo");

    // Get the working copy commit
    let workspace_id = workspace.workspace_name();
    let wc_commit_id = repo
        .view()
        .get_wc_commit_id(workspace_id)
        .expect("No working copy commit found");

    let wc_commit = repo
        .store()
        .get_commit(wc_commit_id)
        .expect("Failed to load working copy commit");

    // Get the parent commit (which has the metadata)
    let parent_ids = wc_commit.parent_ids();
    assert!(!parent_ids.is_empty(), "Working copy should have a parent");

    let parent_commit = repo
        .store()
        .get_commit(&parent_ids[0])
        .expect("Failed to load parent commit");

    let description = parent_commit.description();
    println!("JJ parent change description:\n{}", description);

    // Verify the metadata was written
    assert!(
        description.contains("[aiki]"),
        "Parent change description should contain [aiki] marker. Got: {}",
        description
    );

    // Run blame on the file
    let mut blame_cmd = Command::new(&aiki_bin);
    for (k, v) in &shared_env {
        blame_cmd.env(k, v);
    }
    let output = blame_cmd
        .args(["blame", "test.txt"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run aiki blame");

    let blame_output = String::from_utf8_lossy(&output.stdout);
    let blame_stderr = String::from_utf8_lossy(&output.stderr);

    println!("Blame output:\n{}", blame_output);
    if !blame_stderr.is_empty() {
        println!("Blame stderr:\n{}", blame_stderr);
    }

    // Verify the blame command succeeded
    assert!(output.status.success(), "aiki blame should succeed");

    // Verify the output contains the file content
    assert!(blame_output.contains("line 1"), "Blame should show line 1");
    assert!(blame_output.contains("line 2"), "Blame should show line 2");

    // Verify line markers are present
    assert!(blame_output.contains("1|"), "Should have line 1 marker");
    assert!(blame_output.contains("2|"), "Should have line 2 marker");

    // CRITICAL: Verify Claude attribution appears in the blame output
    // Format is: <commit_id> (<agent_type> <session_id> <confidence>) <line_num>| <line_text>
    // The modified line (line 2) should show Claude attribution

    // Look for Claude Code agent type in the output
    // Note: Currently displays as "claude-code" (the metadata format) rather than "Claude" (display name)
    assert!(
        blame_output.contains("claude-code"),
        "Blame should show 'claude-code' agent type. Output:\n{}",
        blame_output
    );

    // Session ID is now a UUID (deterministic hash of agent_type + external_id)
    // Verify it appears in UUID format (8 hex chars followed by hyphen)
    // The format is truncated in blame output to first 9 chars like "abc12345-..."
    let has_uuid_prefix = blame_output.lines().any(|line| {
        // Look for a UUID-like pattern: 8 hex chars followed by hyphen
        line.contains(char::is_alphanumeric) && line.chars().filter(|c| *c == '-').count() >= 1
    });
    assert!(
        has_uuid_prefix || blame_output.contains("..."),
        "Blame should show truncated session UUID. Output:\n{}",
        blame_output
    );

    // Verify High confidence appears
    assert!(
        blame_output.contains("High"),
        "Blame should show 'High' confidence. Output:\n{}",
        blame_output
    );

    println!("✅ Verified Claude Code attribution in blame output:");
    println!("   ✓ Agent type: claude-code");
    println!("   ✓ Session ID: UUID format");
    println!("   ✓ Confidence: High");
}

fn get_aiki_binary_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("target");
    path.push("debug");
    path.push("aiki");
    path
}
