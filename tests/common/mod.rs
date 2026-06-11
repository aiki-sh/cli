//! Common test utilities shared across integration tests

#![allow(dead_code)]

use std::path::Path;
use std::time::{Duration, Instant};

/// Give a spawned `aiki` command a hermetic environment so machine-mutating
/// commands (`aiki init` in particular) cannot write to the real `~/.aiki`,
/// `~/.config`, or home directory.
///
/// Creates a fresh temp dir per call holding a fake `AIKI_HOME`, `HOME`, and
/// `XDG_CONFIG_HOME`, plus a `.gitconfig` and `JJ_USER`/`JJ_EMAIL` so git/jj
/// invoked by the spawned binary still find a user identity once `HOME`
/// moves. The temp dir is intentionally leaked: it must outlive the spawned
/// process, and the OS cleans the temp filesystem.
pub fn hermetic_env(cmd: &mut std::process::Command) {
    for (key, value) in hermetic_env_vars() {
        cmd.env(key, value);
    }
}

/// `assert_cmd::Command` flavor of [`hermetic_env`].
pub fn hermetic_env_assert(cmd: &mut assert_cmd::Command) {
    for (key, value) in hermetic_env_vars() {
        cmd.env(key, value);
    }
}

fn hermetic_env_vars() -> Vec<(&'static str, std::ffi::OsString)> {
    let dir = tempfile::tempdir().expect("create hermetic home");
    let home = dir.path().join("home");
    let aiki_home = dir.path().join("aiki");
    let config = home.join(".config");
    std::fs::create_dir_all(&aiki_home).expect("create hermetic aiki home");
    std::fs::create_dir_all(&config).expect("create hermetic config dir");
    std::fs::write(
        home.join(".gitconfig"),
        "[user]\n\tname = Aiki Test\n\temail = test@example.com\n",
    )
    .expect("write hermetic gitconfig");
    let vars = vec![
        ("AIKI_HOME", aiki_home.into_os_string()),
        ("HOME", home.into_os_string()),
        ("XDG_CONFIG_HOME", config.into_os_string()),
        ("JJ_USER", "Aiki Test".into()),
        ("JJ_EMAIL", "test@example.com".into()),
    ];
    std::mem::forget(dir);
    vars
}

/// Check if jj binary is available in PATH
pub fn jj_available() -> bool {
    std::process::Command::new("jj")
        .arg("--version")
        .output()
        .is_ok()
}

/// Initialize a Git repository at the given path
pub fn init_git_repo(path: &Path) {
    std::process::Command::new("git")
        .args(&["init"])
        .current_dir(path)
        .output()
        .expect("Failed to initialize Git repository");
}

/// Initialize a JJ workspace (colocated with git)
pub fn init_jj_workspace(path: &Path) -> anyhow::Result<()> {
    let output = std::process::Command::new("jj")
        .arg("git")
        .arg("init")
        .arg("--colocate")
        .current_dir(path)
        .output()?;

    if !output.status.success() {
        anyhow::bail!("Failed to initialize JJ workspace");
    }

    Ok(())
}

/// Wait for background thread to update commit description
///
/// Polls the JJ commit description until it contains the expected content,
/// or times out after the specified duration.
///
/// Returns true if the expected content was found, false if timed out.
pub fn wait_for_description_update(
    repo_path: &Path,
    expected_content: &str,
    timeout: Duration,
) -> bool {
    let start = Instant::now();

    while start.elapsed() < timeout {
        if let Ok(output) = std::process::Command::new("jj")
            .arg("log")
            .arg("-r")
            .arg("@")
            .arg("-T")
            .arg("description")
            .current_dir(repo_path)
            .output()
        {
            let description = String::from_utf8_lossy(&output.stdout);
            if description.contains(expected_content) {
                return true;
            }
        }

        // Poll every 50ms
        std::thread::sleep(Duration::from_millis(50));
    }

    false
}

/// Get the current commit description from JJ
pub fn get_commit_description(repo_path: &Path) -> String {
    let output = std::process::Command::new("jj")
        .arg("log")
        .arg("-r")
        .arg("@")
        .arg("-T")
        .arg("description")
        .current_dir(repo_path)
        .output()
        .expect("Failed to run jj log");

    String::from_utf8_lossy(&output.stdout).to_string()
}
