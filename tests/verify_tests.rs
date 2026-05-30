use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// Helper function to initialize a real Git repository
fn init_git_repo(path: &std::path::Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("Failed to initialize Git repository");

    // Set git user for the test repo
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(path)
        .output()
        .expect("Failed to set git user.name");

    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .expect("Failed to set git user.email");
}

/// Helper function to initialize a JJ repository
fn init_jj_repo(path: &std::path::Path) {
    // Initialize JJ with non-colocated storage (creates internal Git repo)
    // Don't initialize git first, as non-colocated mode fails with existing .git
    Command::new("jj")
        .args(["git", "init", "--no-colocate"])
        .current_dir(path)
        .output()
        .expect("Failed to initialize JJ repository");

    // Configure JJ user
    Command::new("jj")
        .args(["config", "set", "--repo", "user.name", "Test User"])
        .current_dir(path)
        .output()
        .expect("Failed to set jj user.name");

    Command::new("jj")
        .args(["config", "set", "--repo", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .expect("Failed to set jj user.email");
}

/// Helper function to create a test file with Aiki provenance metadata
fn create_test_change_with_provenance(path: &std::path::Path) {
    // Create a test file
    let test_file = path.join("test.txt");
    fs::write(&test_file, "Test content\n").unwrap();

    // Describe the change with Aiki metadata
    let description = r#"Test change with provenance

[aiki]
author=claude
author_type=agent
session=test-session-123
tool=Edit
confidence=High
method=Hook
[/aiki]"#;

    Command::new("jj")
        .args(["describe", "-m", description])
        .current_dir(path)
        .output()
        .expect("Failed to describe change");
}

#[test]
fn test_verify_unsigned_change_with_provenance() {
    let temp_dir = TempDir::new().unwrap();
    init_jj_repo(temp_dir.path());
    create_test_change_with_provenance(temp_dir.path());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.args(["verify", "@"]);

    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should succeed and show unsigned warning
    assert!(output.status.success());
    assert!(stdout.contains("Not signed") || stdout.contains("⚠"));
    assert!(stdout.contains("Metadata present") || stdout.contains("claude-code"));
}

#[test]
fn test_verify_change_without_provenance() {
    let temp_dir = TempDir::new().unwrap();
    init_jj_repo(temp_dir.path());

    // Create a change without Aiki metadata
    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, "Test content\n").unwrap();

    Command::new("jj")
        .args(["describe", "-m", "Regular commit without AI metadata"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to describe change");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.args(["verify", "@"]);

    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should succeed and show no AI metadata
    assert!(output.status.success());
    assert!(stdout.contains("No AI metadata") || stdout.contains("NOT AN AI CHANGE"));
}

#[test]
fn test_verify_invalid_revision() {
    let temp_dir = TempDir::new().unwrap();
    init_jj_repo(temp_dir.path());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.args(["verify", "nonexistent123"]);

    let output = cmd.output().unwrap();

    // Should fail with invalid revision error
    assert!(!output.status.success());
}

#[test]
fn test_verify_outside_jj_repo() {
    let temp_dir = TempDir::new().unwrap();

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.args(["verify", "@"]);

    let output = cmd.output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Should fail with "not in a JJ repository" error
    assert!(!output.status.success());
    assert!(stderr.contains("Not in a JJ repository") || stderr.contains("JJ repository"));
}

#[test]
fn test_verify_default_revision() {
    let temp_dir = TempDir::new().unwrap();
    init_jj_repo(temp_dir.path());
    create_test_change_with_provenance(temp_dir.path());

    // Call verify without specifying revision (should default to @)
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.arg("verify");

    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should succeed and verify the working copy
    assert!(output.status.success());
    assert!(stdout.contains("Verifying change"));
}

// Note: Testing actual signature verification requires GPG/SSH setup
// which is complex in CI. We test the unsigned case and structure here.
// Real signature verification would require:
// 1. Setting up a test GPG key or SSH key
// 2. Configuring JJ to use that key
// 3. Signing a change with `jj sign`
// 4. Verifying it shows as "good"
//
// This is better tested manually or in a more controlled environment
// with proper key management.

#[test]
fn test_verify_shows_signature_section() {
    let temp_dir = TempDir::new().unwrap();
    init_jj_repo(temp_dir.path());
    create_test_change_with_provenance(temp_dir.path());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.args(["verify", "@"]);

    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should show signature section in output
    assert!(stdout.contains("Signature:"));
    assert!(stdout.contains("Provenance:"));
    assert!(stdout.contains("Result:"));
}
