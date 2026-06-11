//! Common test utilities shared across integration tests

#![allow(dead_code)]

use std::path::Path;
use std::time::{Duration, Instant};

/// Shared hermetic home for every `aiki` spawned through [`aiki_cmd`].
///
/// One home per test binary mirrors the A0 baseline environment (one temp
/// `AIKI_HOME` per suite run): init-created global state stays visible to
/// later commands in the same test, while the developer's real `~/.aiki`
/// stays untouched — in particular `task close` cannot trip the
/// requires-confidence gate on the developer's live agent session.
static SHARED_TEST_HOME: std::sync::LazyLock<std::path::PathBuf> =
    std::sync::LazyLock::new(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("create shared test home");
        let base = dir.path().to_path_buf();
        let home = base.join("home");
        std::fs::create_dir_all(base.join("aiki")).expect("create shared aiki home");
        std::fs::create_dir_all(home.join(".config")).expect("create shared config dir");
        std::fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Aiki Test\n\temail = test@example.com\n",
        )
        .expect("write shared gitconfig");
        // Fake agent binaries: any `aiki` command that spawns an agent
        // (`aiki run`, `aiki build --review --fix`, …) must resolve these
        // instead of live CLIs — a real spawn burns tokens and runs for
        // unbounded wall clock (see ops/now/fix-refactor.md A8a).
        let fake_bin = base.join("fake-bin");
        std::fs::create_dir_all(&fake_bin).expect("create fake-bin dir");
        for agent in ["claude", "codex"] {
            let script = fake_bin.join(agent);
            std::fs::write(&script, "#!/bin/sh\nexit 0\n").expect("write fake agent");
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }
        // Keep the directory for the whole test-binary lifetime.
        std::mem::forget(dir);
        base
    });

/// Construct an `aiki` Command pre-wired to the shared hermetic home, with
/// fake `claude`/`codex` binaries shadowing any real agent CLIs on PATH.
///
/// Use this instead of `Command::new(cargo_bin!("aiki"))` in integration
/// tests so spawned binaries never read or write the real machine state and
/// never start live agents.
pub fn aiki_cmd() -> std::process::Command {
    let base = &*SHARED_TEST_HOME;
    let home = base.join("home");
    let path_value = format!(
        "{}:{}",
        base.join("fake-bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.env("AIKI_HOME", base.join("aiki"))
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("JJ_USER", "Aiki Test")
        .env("JJ_EMAIL", "test@example.com")
        .env("PATH", path_value);
    cmd
}

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
