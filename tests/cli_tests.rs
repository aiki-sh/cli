mod common;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;

/// Helper function to initialize a real Git repository
fn init_git_repo(path: &std::path::Path) {
    Command::new("git")
        .args(&["init"])
        .current_dir(path)
        .output()
        .expect("Failed to initialize Git repository");
}

#[test]
fn test_help_command() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("AI code review engine"))
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("init"));
}

#[test]
fn test_version_command() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.arg("--version");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("aiki"))
        .stdout(predicate::str::contains("0.1.0"));
}

#[test]
fn test_init_command_wiring() {
    // Verify the command executes without panicking (tests Clap wiring)
    // Using jj-lib directly, so no external JJ binary required.
    // Run in a temp dir — without an explicit cwd this would run `aiki init`
    // inside the source tree.
    let temp_dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.arg("init");
    common::hermetic_env(&mut cmd);

    let output = cmd.output().unwrap();

    // At minimum, the command should execute (not panic or have broken Clap wiring)
    assert!(
        output.status.code().is_some(),
        "Command should exit with a status code"
    );

    // If it failed, the error should be meaningful
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{}{}", stderr, stdout);

        // Should contain a helpful error message
        assert!(
            combined.contains("Failed to initialize")
                || combined.contains("Not in a Git repository")
                || combined.contains("Failed to get current directory"),
            "Error message should be meaningful, got: {}",
            combined
        );
    }
}

#[test]
fn test_init_in_git_repo() {
    // Using jj-lib directly, no external JJ binary required
    // Create a temporary directory for testing
    let temp_dir = tempfile::tempdir().unwrap();

    // Initialize a proper Git repository
    init_git_repo(temp_dir.path());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.arg("init");
    common::hermetic_env(&mut cmd);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Initializing Aiki"))
        .stdout(predicate::str::contains("✓ Initialized JJ repository"));
}

#[test]
fn test_init_not_in_git_repo() {
    // Test that init fails when not in a Git repository
    let temp_dir = tempfile::tempdir().unwrap();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.arg("init");
    common::hermetic_env(&mut cmd);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("Not in a Git repository"))
        .stderr(predicate::str::contains("Run 'git init' first"));
}

#[test]
fn test_init_from_subdirectory() {
    // Test that init works from a subdirectory of a Git repo
    let temp_dir = tempfile::tempdir().unwrap();

    // Initialize a proper Git repository
    init_git_repo(temp_dir.path());

    // Create a subdirectory
    let subdir = temp_dir.path().join("src").join("components");
    std::fs::create_dir_all(&subdir).unwrap();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(&subdir);
    cmd.arg("init");
    common::hermetic_env(&mut cmd);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Initializing Aiki"))
        .stdout(predicate::str::contains("✓ Initialized JJ repository"));

    // Verify .jj was created at the repository root, not in subdirectory
    assert!(temp_dir.path().join(".jj").exists());
    assert!(!subdir.join(".jj").exists());
}

#[test]
fn test_init_already_initialized() {
    // Test that init is idempotent when Aiki is already initialized
    let temp_dir = tempfile::tempdir().unwrap();

    // Initialize a proper Git repository
    init_git_repo(temp_dir.path());

    // Run init once
    let mut cmd1 = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd1.current_dir(temp_dir.path());
    cmd1.arg("init");
    common::hermetic_env(&mut cmd1);
    cmd1.assert().success();

    // Run init again - should be idempotent
    let mut cmd2 = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd2.current_dir(temp_dir.path());
    cmd2.arg("init");
    common::hermetic_env(&mut cmd2);

    cmd2.assert()
        .success()
        .stdout(predicate::str::contains("already initialized"));
}

#[test]
fn test_init_creates_aiki_directory_structure() {
    // Test that init configures the repository correctly
    // Note: In the new architecture, .aiki directory is only created if there's a previous hooks path
    let temp_dir = tempfile::tempdir().unwrap();

    // Initialize a proper Git repository
    init_git_repo(temp_dir.path());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.arg("init");
    common::hermetic_env(&mut cmd);

    cmd.assert().success().stdout(predicate::str::contains(
        "✓ Repository initialized successfully",
    ));

    // Verify JJ was initialized
    assert!(temp_dir.path().join(".jj").exists());

    // Verify Git config points to global hooks
    let output = Command::new("git")
        .args(&["config", "core.hooksPath"])
        .current_dir(temp_dir.path())
        .output()
        .unwrap();
    let hooks_path = String::from_utf8_lossy(&output.stdout);
    assert!(hooks_path.contains(".aiki/githooks"));
}

#[test]
fn test_init_with_existing_jj() {
    // Test that init works when JJ is already initialized
    let temp_dir = tempfile::tempdir().unwrap();

    // Initialize a proper Git repository
    init_git_repo(temp_dir.path());

    // Manually initialize JJ using jj-lib
    let config = jj_lib::config::StackedConfig::with_defaults();
    let settings = jj_lib::settings::UserSettings::from_config(config).unwrap();
    jj_lib::workspace::Workspace::init_external_git(
        &settings,
        temp_dir.path(),
        &temp_dir.path().join(".git"),
    )
    .unwrap();

    // Now run aiki init
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.arg("init");
    common::hermetic_env(&mut cmd);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("✓ Found existing JJ repository"))
        .stdout(predicate::str::contains(
            "✓ Repository initialized successfully",
        ));

    // Verify Git config points to global hooks
    let output = Command::new("git")
        .args(&["config", "core.hooksPath"])
        .current_dir(temp_dir.path())
        .output()
        .unwrap();
    let hooks_path = String::from_utf8_lossy(&output.stdout);
    assert!(hooks_path.contains(".aiki/githooks"));
}
