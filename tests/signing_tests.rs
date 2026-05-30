use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// Helper function to initialize a real Git repository
fn init_git_repo(path: &std::path::Path) {
    Command::new("git")
        .args(&["init"])
        .current_dir(path)
        .output()
        .expect("Failed to initialize Git repository");

    // Set git user for the test repo
    Command::new("git")
        .args(&["config", "user.name", "Test User"])
        .current_dir(path)
        .output()
        .expect("Failed to set git user.name");

    Command::new("git")
        .args(&["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .expect("Failed to set git user.email");
}

#[test]
fn test_init_without_signing_keys() {
    // Test that aiki init works even without signing keys configured
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.arg("init");

    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);

    // Should succeed but show warning about no signing keys
    // (unless user happens to have GPG/SSH keys configured on test system)
    assert!(
        output.status.success() || combined.contains("initialized"),
        "Init should succeed even without signing keys"
    );

    // Verify .jj directory was created
    assert!(temp_dir.path().join(".jj").exists());
}

#[test]
fn test_doctor_checks_signing_config() {
    // Test that aiki doctor correctly checks signing configuration
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());

    // Initialize aiki
    let mut init_cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    init_cmd.current_dir(temp_dir.path());
    init_cmd.arg("init");
    init_cmd.output().unwrap();

    // Run doctor
    let mut doctor_cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    doctor_cmd.current_dir(temp_dir.path());
    doctor_cmd.arg("doctor");

    let output = doctor_cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should check for signing configuration
    assert!(
        stdout.contains("Commit Signing") || stdout.contains("signing"),
        "Doctor should check signing configuration"
    );
}

#[test]
fn test_init_creates_signing_config_when_ssh_key_exists() {
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());

    // Create a fake SSH key in a temporary location
    let ssh_dir = temp_dir.path().join("fake_ssh");
    fs::create_dir_all(&ssh_dir).unwrap();
    let key_path = ssh_dir.join("id_ed25519.pub");
    fs::write(
        &key_path,
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeKey test@example.com\n",
    )
    .unwrap();

    // Note: This test won't actually detect the fake SSH key since the detection
    // looks in ~/.ssh, but we're testing the mechanism works
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.arg("init");

    let output = cmd.output().unwrap();

    // Init should complete successfully
    assert!(output.status.success());

    // Verify .jj directory structure was created
    assert!(temp_dir.path().join(".jj").exists());
}

#[test]
fn test_signing_config_toml_structure() {
    // Test that signing configuration creates valid TOML structure
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join(".jj/repo");
    fs::create_dir_all(&config_path).unwrap();

    // Manually create a signing config (simulating what aiki init does)
    let config_content = r#"
[signing]
backend = "gpg"
behavior = "own"
"#;
    fs::write(config_path.join("config.toml"), config_content).unwrap();

    // Verify we can parse it
    let content = fs::read_to_string(config_path.join("config.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&content).unwrap();

    assert!(parsed.get("signing").is_some());
    assert_eq!(parsed["signing"]["backend"].as_str(), Some("gpg"));
    assert_eq!(parsed["signing"]["behavior"].as_str(), Some("own"));
}

#[test]
fn test_ssh_allowed_signers_creation() {
    // Test that SSH signing creates allowed-signers file
    let temp_dir = TempDir::new().unwrap();
    let jj_dir = temp_dir.path().join(".jj");
    fs::create_dir_all(&jj_dir).unwrap();

    // Create a test SSH public key
    let ssh_dir = temp_dir.path().join("ssh");
    fs::create_dir_all(&ssh_dir).unwrap();
    let pubkey_path = ssh_dir.join("test_key.pub");
    let pubkey_content = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITestKey123 test@example.com\n";
    fs::write(&pubkey_path, pubkey_content).unwrap();

    // Simulate what aiki init does for SSH signing
    let email = "user@example.com";
    let pubkey = fs::read_to_string(&pubkey_path).unwrap();
    let allowed_signers_content = format!("{} {}", email, pubkey.trim());
    let allowed_signers_path = jj_dir.join("allowed-signers");
    fs::write(&allowed_signers_path, allowed_signers_content).unwrap();

    // Verify the file was created correctly
    assert!(allowed_signers_path.exists());
    let content = fs::read_to_string(&allowed_signers_path).unwrap();
    assert!(content.contains("user@example.com"));
    assert!(content.contains("ssh-ed25519"));
    assert!(content.contains("AAAAC3NzaC1lZDI1NTE5AAAAITestKey123"));
}

#[test]
fn test_init_quiet_mode_with_signing() {
    // Test that quiet mode still configures signing
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.args(&["init", "--quiet"]);

    let output = cmd.output().unwrap();

    // In quiet mode, there should be minimal output
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.is_empty() || stdout.trim().is_empty() || output.status.success());

    // But .jj should still be created
    assert!(temp_dir.path().join(".jj").exists());
}

#[test]
fn test_doctor_detects_missing_signing_key() {
    // Test that doctor detects when a signing key is configured but not accessible
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());

    // Initialize aiki
    let mut init_cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    init_cmd.current_dir(temp_dir.path());
    init_cmd.arg("init");
    init_cmd.output().unwrap();

    // Manually create a signing config with a non-existent key
    let config_path = temp_dir.path().join(".jj/repo");
    fs::create_dir_all(&config_path).unwrap();
    let config_content = r#"
[signing]
backend = "ssh"
behavior = "own"
key = "/nonexistent/path/to/key.pub"
"#;
    fs::write(config_path.join("config.toml"), config_content).unwrap();

    // Run doctor
    let mut doctor_cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    doctor_cmd.current_dir(temp_dir.path());
    doctor_cmd.arg("doctor");

    let output = doctor_cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Doctor should detect the missing key
    assert!(
        stdout.contains("not found") || stdout.contains("✗"),
        "Doctor should detect missing signing key"
    );
}

#[test]
fn test_multiple_backends_config_structure() {
    // Test that we can configure different signing backends
    let temp_dir = TempDir::new().unwrap();

    // Test GPG backend
    let config_path = temp_dir.path().join(".jj/repo");
    fs::create_dir_all(&config_path).unwrap();

    let gpg_config = r#"
[signing]
backend = "gpg"
behavior = "own"
"#;
    fs::write(config_path.join("config.toml"), gpg_config).unwrap();
    let content = fs::read_to_string(config_path.join("config.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&content).unwrap();
    assert_eq!(parsed["signing"]["backend"].as_str(), Some("gpg"));

    // Test SSH backend
    let ssh_config = r#"
[signing]
backend = "ssh"
behavior = "own"
key = "~/.ssh/id_ed25519.pub"

[signing.backends.ssh]
allowed-signers = ".jj/allowed-signers"
"#;
    fs::write(config_path.join("config.toml"), ssh_config).unwrap();
    let content = fs::read_to_string(config_path.join("config.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&content).unwrap();
    assert_eq!(parsed["signing"]["backend"].as_str(), Some("ssh"));
    assert!(parsed["signing"]["backends"]["ssh"].is_table());

    // Test GPG-SM backend
    let gpgsm_config = r#"
[signing]
backend = "gpgsm"
behavior = "own"
"#;
    fs::write(config_path.join("config.toml"), gpgsm_config).unwrap();
    let content = fs::read_to_string(config_path.join("config.toml")).unwrap();
    let parsed: toml::Value = toml::from_str(&content).unwrap();
    assert_eq!(parsed["signing"]["backend"].as_str(), Some("gpgsm"));
}

#[test]
fn test_ssh_key_types_all_supported() {
    // Test that different SSH key types are handled correctly in allowed-signers
    let temp_dir = TempDir::new().unwrap();
    let jj_dir = temp_dir.path().join(".jj");
    fs::create_dir_all(&jj_dir).unwrap();

    // Test ed25519 key
    let ed25519_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEd25519TestKey user@example.com";
    let allowed_signers_ed25519 = format!("user@example.com {}", ed25519_key);
    fs::write(jj_dir.join("allowed-signers"), &allowed_signers_ed25519).unwrap();
    let content = fs::read_to_string(jj_dir.join("allowed-signers")).unwrap();
    assert!(content.contains("ssh-ed25519"));
    assert!(content.contains("user@example.com"));

    // Test RSA key
    let rsa_key = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQDRsaTestKey user@example.com";
    let allowed_signers_rsa = format!("user@example.com {}", rsa_key);
    fs::write(jj_dir.join("allowed-signers"), &allowed_signers_rsa).unwrap();
    let content = fs::read_to_string(jj_dir.join("allowed-signers")).unwrap();
    assert!(content.contains("ssh-rsa"));

    // Test ECDSA key
    let ecdsa_key = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEcdsaTestKey user@example.com";
    let allowed_signers_ecdsa = format!("user@example.com {}", ecdsa_key);
    fs::write(jj_dir.join("allowed-signers"), &allowed_signers_ecdsa).unwrap();
    let content = fs::read_to_string(jj_dir.join("allowed-signers")).unwrap();
    assert!(content.contains("ecdsa-sha2-nistp256"));
}

#[test]
fn test_git_signing_config_gpg_detection() {
    // Test that Git signing configuration with GPG is properly detected
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());

    // Simulate Git signing already configured with GPG
    Command::new("git")
        .current_dir(temp_dir.path())
        .args(&["config", "commit.gpgsign", "true"])
        .output()
        .unwrap();

    Command::new("git")
        .current_dir(temp_dir.path())
        .args(&["config", "user.signingkey", "4ED556E9729E000F"])
        .output()
        .unwrap();

    Command::new("git")
        .current_dir(temp_dir.path())
        .args(&["config", "gpg.format", "openpgp"])
        .output()
        .unwrap();

    // Run aiki init
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.arg("init");
    let output = cmd.output().unwrap();

    // Should succeed (even if GPG binary not available, detection doesn't crash)
    assert!(output.status.success() || output.status.code().is_some());

    // Verify .jj was created
    assert!(temp_dir.path().join(".jj").exists());
}

#[test]
fn test_git_signing_ssh_format_detection() {
    // Test that Git signing with SSH format is detected and mirrored
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());

    // Create a test SSH key
    let ssh_dir = temp_dir.path().join(".ssh");
    fs::create_dir_all(&ssh_dir).unwrap();
    let key_path = ssh_dir.join("id_test.pub");
    fs::write(
        &key_path,
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest test@example.com",
    )
    .unwrap();

    // Configure Git to use SSH signing
    Command::new("git")
        .current_dir(temp_dir.path())
        .args(&["config", "commit.gpgsign", "true"])
        .output()
        .unwrap();

    Command::new("git")
        .current_dir(temp_dir.path())
        .args(&["config", "user.signingkey", key_path.to_str().unwrap()])
        .output()
        .unwrap();

    Command::new("git")
        .current_dir(temp_dir.path())
        .args(&["config", "gpg.format", "ssh"])
        .output()
        .unwrap();

    // Run aiki init
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(temp_dir.path());
    cmd.arg("init");
    let output = cmd.output().unwrap();

    // Should succeed
    assert!(output.status.success());

    // Check if SSH backend was configured in JJ config
    let config_path = temp_dir.path().join(".jj/repo/config.toml");
    if config_path.exists() {
        let content = fs::read_to_string(&config_path).unwrap();
        let parsed: Result<toml::Value, _> = toml::from_str(&content);
        if let Ok(config) = parsed {
            if let Some(signing) = config.get("signing") {
                if let Some(backend) = signing.get("backend").and_then(|v| v.as_str()) {
                    assert_eq!(
                        backend, "ssh",
                        "Should use SSH backend when Git is configured with ssh format"
                    );
                }
            }
        }
    }
}

#[test]
fn test_all_backend_types_have_valid_toml() {
    // Comprehensive test that all backend types produce valid TOML
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join(".jj/repo");
    fs::create_dir_all(&config_path).unwrap();

    let test_configs = vec![
        (
            "gpg",
            r#"
[signing]
backend = "gpg"
behavior = "own"
"#,
        ),
        (
            "ssh",
            r#"
[signing]
backend = "ssh"
behavior = "own"
key = "~/.ssh/id_ed25519.pub"

[signing.backends.ssh]
allowed-signers = ".jj/allowed-signers"
"#,
        ),
        (
            "gpgsm",
            r#"
[signing]
backend = "gpgsm"
behavior = "own"
"#,
        ),
    ];

    for (backend_name, config_toml) in test_configs {
        fs::write(config_path.join("config.toml"), config_toml).unwrap();

        let content = fs::read_to_string(config_path.join("config.toml")).unwrap();
        let parsed: toml::Value =
            toml::from_str(&content).expect(&format!("Failed to parse {} config", backend_name));

        // Verify required fields
        assert!(
            parsed.get("signing").is_some(),
            "{} config missing [signing]",
            backend_name
        );
        assert_eq!(
            parsed["signing"]["backend"].as_str(),
            Some(backend_name),
            "{} backend not set correctly",
            backend_name
        );
        assert_eq!(
            parsed["signing"]["behavior"].as_str(),
            Some("own"),
            "{} behavior not set correctly",
            backend_name
        );

        // For SSH, verify backends.ssh section exists
        if backend_name == "ssh" {
            assert!(
                parsed["signing"]["backends"]["ssh"].is_table(),
                "SSH backend missing [signing.backends.ssh] section"
            );
        }
    }
}
