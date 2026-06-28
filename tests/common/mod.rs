//! Common test utilities shared across integration tests

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Env var naming the fixture a scripted fake agent should emit as its
/// transcript. Set together with [`FAKE_TRANSCRIPT_DEST_ENV`].
pub const FAKE_TRANSCRIPT_SRC_ENV: &str = "AIKI_FAKE_TRANSCRIPT_SRC";

/// Env var naming the path the scripted fake agent should write its transcript
/// to — i.e. the path the harness later reads (a Stop hook's `transcript_path`).
pub const FAKE_TRANSCRIPT_DEST_ENV: &str = "AIKI_FAKE_TRANSCRIPT_DEST";

/// Body of the fake `claude`/`codex` agent binaries.
///
/// Default behavior is the historical no-op (`exit 0`): no transcript, so most
/// integration tests that only assert spawn/argv behavior are unaffected.
///
/// **Scripted-transcript mode.** When both [`FAKE_TRANSCRIPT_SRC_ENV`] and
/// [`FAKE_TRANSCRIPT_DEST_ENV`] are set, the agent copies the named fixture to
/// the destination before exiting, simulating a real harness flushing its
/// transcript JSONL. An integration test can then drive a real extraction off
/// the committed golden fixtures (`tests/fixtures/tokens/`) instead of the
/// previous empty no-op, which exercised none of the token-extraction path.
const FAKE_AGENT_SCRIPT: &str = r#"#!/bin/sh
if [ -n "$AIKI_FAKE_TRANSCRIPT_SRC" ] && [ -n "$AIKI_FAKE_TRANSCRIPT_DEST" ]; then
  mkdir -p "$(dirname "$AIKI_FAKE_TRANSCRIPT_DEST")"
  cat "$AIKI_FAKE_TRANSCRIPT_SRC" > "$AIKI_FAKE_TRANSCRIPT_DEST"
fi
exit 0
"#;

/// Absolute path to a committed golden token transcript fixture under
/// `cli/tests/fixtures/tokens/`.
///
/// Resolved from `CARGO_MANIFEST_DIR` so it is stable regardless of the test's
/// working directory.
pub fn tokens_fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tokens")
        .join(name)
}

/// Configure a spawned `aiki` command so its fake agent emits `fixture` as the
/// transcript at `dest` (the path the harness will read).
pub fn scripted_transcript(cmd: &mut std::process::Command, fixture: &str, dest: &Path) {
    cmd.env(FAKE_TRANSCRIPT_SRC_ENV, tokens_fixture_path(fixture))
        .env(FAKE_TRANSCRIPT_DEST_ENV, dest);
}

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
            std::fs::write(&script, FAKE_AGENT_SCRIPT).expect("write fake agent");
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

/// Absolute path to one of the installed fake agent binaries (`claude`,
/// `codex`) shared via [`aiki_cmd`]. Forces the shared home to initialize, so
/// the scripted fake agent is on disk. Lets a test invoke the real installed
/// artifact (e.g. to exercise the scripted-transcript mode at the process
/// boundary).
pub fn fake_agent_path(name: &str) -> PathBuf {
    SHARED_TEST_HOME.join("fake-bin").join(name)
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

/// Derive one stable hermetic global home for an e2e test from its repo dir.
///
/// The home lives *beside* the repo temp dir (never inside it, so it can't
/// leak into jj/git history assertions) and is keyed to the repo's unique
/// tempdir name, so every `aiki` invocation in a single test resolves the
/// SAME `AIKI_HOME`. That isolates each test's global aiki state from the
/// developer's real `~/.aiki` and from other tests running in parallel —
/// concurrent writes to one shared global conversation repo serialize behind
/// a single write lock and were timing out session discovery.
///
/// Returns `(aiki_home, home, xdg_config_home)`.
fn e2e_home_dirs(repo: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let name = repo
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("e2e");
    let base = repo.with_file_name(format!("{name}-aiki-home"));
    let aiki_home = base.join("aiki");
    let home = base.join("home");
    let config = home.join(".config");
    std::fs::create_dir_all(&aiki_home).expect("create e2e aiki home");
    std::fs::create_dir_all(&config).expect("create e2e config dir");
    let gitconfig = home.join(".gitconfig");
    if !gitconfig.exists() {
        std::fs::write(
            &gitconfig,
            "[user]\n\tname = Aiki Test\n\temail = test@example.com\n",
        )
        .expect("write e2e gitconfig");
    }
    (aiki_home, home, config)
}

/// `aiki` command for pure-aiki e2e steps (`init`, `task add/set/close/start/
/// list/show`, `task diff`): fully hermetic `AIKI_HOME` + `HOME` so it never
/// reads or writes the developer's real global state. Shares one `AIKI_HOME`
/// per test via [`e2e_home_dirs`]. PATH is inherited so real `jj`/`git`
/// resolve, but these commands never spawn a live agent.
pub fn e2e_aiki(repo: &Path) -> assert_cmd::Command {
    let (aiki_home, home, config) = e2e_home_dirs(repo);
    let mut cmd = assert_cmd::Command::cargo_bin("aiki").unwrap();
    cmd.env("AIKI_HOME", aiki_home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", config)
        .env("JJ_USER", "Aiki Test")
        .env("JJ_EMAIL", "test@example.com");
    cmd
}

/// `aiki` command for e2e steps that spawn a LIVE agent (`run`, `build`,
/// `review`, `loop`): hermetic `AIKI_HOME` (the same per-test dir as
/// [`e2e_aiki`]) but the REAL `HOME`/PATH, so `claude`/`codex` find their
/// credentials and the developer's installed agent hooks. The spawned agent
/// inherits `AIKI_HOME`, so its hooks record `session.started` into the
/// test's isolated global dir — exactly where `discover_session_id` polls.
pub fn e2e_aiki_agent(repo: &Path) -> assert_cmd::Command {
    let (aiki_home, _home, _config) = e2e_home_dirs(repo);
    let mut cmd = assert_cmd::Command::cargo_bin("aiki").unwrap();
    cmd.env("AIKI_HOME", aiki_home)
        .env("JJ_USER", "Aiki Test")
        .env("JJ_EMAIL", "test@example.com");
    cmd
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

// ===========================================================================
// Integration-plugin test fixture (generalized from herdr_plugin_tests.rs)
//
// One parameterized fixture so every `<app>_plugin_tests.rs` (Tier-1, hermetic
// consumer-path) is a `PluginSpec` + assertions, not a copy of the herdr setup.
// See ops/next/integrations/testing-helpers-plan.md.
// ===========================================================================

/// Env var the recording stub appends its argv to. A test-only convention — the
/// shipped plugin never references it; only the stub binary does.
pub const HOST_RECORD_ENV: &str = "AIKI_TEST_HOST_RECORD";

/// Body of a stub host binary (`cmux`/`orca`/`superset`/`herdr`/`curl`): append
/// the full argv (space-joined, one line per call) to `$AIKI_TEST_HOST_RECORD`,
/// then exit 0 — so a test can prove exactly what the plugin invoked.
const STUB_RECORD: &str = "#!/bin/sh\n\
: \"${AIKI_TEST_HOST_RECORD:=/dev/null}\"\n\
printf '%s\\n' \"$*\" >> \"$AIKI_TEST_HOST_RECORD\"\n\
exit 0\n";

/// How a plugin's self-guard is toggled "inside the host" vs outside.
pub enum Marker {
    /// Env vars that, when set, mean "inside the host"; removed = outside.
    /// (cmux: `CMUX_WORKSPACE_ID`; superset: API key + org; aoe: `AOE_SESSION_ID`.)
    Env(&'static [(&'static str, &'static str)]),
    /// A file whose presence means "inside the host" (path is fixture-relative).
    /// `path_env` is set to the file's absolute path so a plugin can resolve its
    /// marker without a hardcoded OS path (orca: `ORCA_ENDPOINT_FILE`).
    File {
        path_env: &'static str,
        rel_path: &'static str,
        contents: &'static str,
    },
}

/// Whether a [`PluginFixture::fire`] runs with the host marker applied.
pub enum Guard {
    Inside,
    Outside,
}

/// Declarative description of a plugin under test.
pub struct PluginSpec {
    /// "namespace/name" — the install dir AND the `.aiki/hooks.yml` include ref
    /// (must match the plugin's own `name:`).
    pub plugin_ref: &'static str,
    /// `include_str!` of the staged `plugins/<app>/hooks.yaml`, so the test
    /// exercises the real shipped file (no drift).
    pub staged_yaml: &'static str,
    /// Host binaries the hook shells out to; each is stubbed on PATH to record
    /// argv (`["cmux"]`, `["orca"]`, `["curl"]`, …).
    pub stub_bins: &'static [&'static str],
    /// How to toggle the self-guard for `Guard::Inside`/`Outside`.
    pub marker: Marker,
}

/// A hermetic aiki/jj repo wired to a staged plugin, with stub host binaries and
/// the built `aiki` on PATH. Drives the plugin through the REAL `aiki hooks
/// stdin` consumer path.
pub struct PluginFixture {
    _base: tempfile::TempDir,
    root: PathBuf,
    pub repo: PathBuf,
    pub aiki_home: PathBuf,
    home: PathBuf,
    config: PathBuf,
    record: PathBuf,
    path_value: std::ffi::OsString,
    marker: Marker,
}

/// Write a recording stub `name` into `dir` (argv → `$AIKI_TEST_HOST_RECORD`).
pub fn write_host_stub(dir: &Path, name: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, STUB_RECORD).expect("write host stub");
    let mut perms = std::fs::metadata(&p).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&p, perms).expect("chmod host stub");
}

/// Install a staged plugin where the resolver finds it without a network fetch:
/// `$AIKI_HOME/plugins/<namespace>/<name>/hooks.yaml`.
pub fn install_plugin(aiki_home: &Path, plugin_ref: &str, staged_yaml: &str) {
    let (ns, name) = plugin_ref
        .split_once('/')
        .expect("plugin_ref must be namespace/name");
    let dir = aiki_home.join("plugins").join(ns).join(name);
    std::fs::create_dir_all(&dir).expect("create plugin dir");
    std::fs::write(dir.join("hooks.yaml"), staged_yaml).expect("install plugin");
}

/// Wire a plugin into a project's hookfile via a top-level include.
pub fn wire_include(repo: &Path, plugin_ref: &str) {
    let aiki_dir = repo.join(".aiki");
    std::fs::create_dir_all(&aiki_dir).expect("create .aiki dir");
    std::fs::write(
        aiki_dir.join("hooks.yml"),
        format!("include:\n  - {plugin_ref}\n"),
    )
    .expect("write .aiki/hooks.yml");
}

/// Read recorded host invocations (one per line); empty if the file is absent.
pub fn read_records(path: &Path) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => contents
            .lines()
            .map(|l| l.to_string())
            .filter(|l| !l.trim().is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Assert the self-guard no-op: the host was never invoked.
pub fn assert_no_calls(records: &[String]) {
    assert!(
        records.is_empty(),
        "expected zero host calls (self-guard no-op), got: {records:?}",
    );
}

/// Assert exactly one recorded call contains `needle`.
pub fn assert_invoked_once(records: &[String], needle: &str) {
    let hits = records.iter().filter(|l| l.contains(needle)).count();
    assert_eq!(
        hits, 1,
        "expected exactly one call containing {needle:?}, got {hits}: {records:?}",
    );
}

impl PluginFixture {
    pub fn new(spec: PluginSpec) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir().expect("create fixture base");
        let root = base.path().to_path_buf();
        let repo = root.join("repo");
        let aiki_home = root.join("aiki");
        let home = root.join("home");
        let config = home.join(".config");
        let bin = root.join("bin");
        let record = root.join("host-record");
        for dir in [&repo, &aiki_home, &home, &config, &bin] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        // Canonicalize the repo so the per-user marker `aiki init` keys by its
        // resolved getcwd matches the init-v2 gate's lookup from the payload
        // `cwd` — otherwise the /var symlink alias on macOS keys two markers.
        let repo = repo.canonicalize().expect("canonicalize fixture repo");

        std::fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Aiki Test\n\temail = test@example.com\n",
        )
        .expect("write gitconfig");

        // Stub each host binary on PATH (records argv).
        for name in spec.stub_bins {
            write_host_stub(&bin, name);
        }

        // The built `aiki` must be on PATH: the core `session.started` flow runs
        // `shell: aiki init --quiet` (on_failure: stop). If `aiki` can't be
        // resolved there, core stops and the user hooks.yml (the plugin) never
        // runs — masking the contract under test.
        let aiki_bin: &Path = assert_cmd::cargo::cargo_bin!("aiki");
        std::os::unix::fs::symlink(aiki_bin, bin.join("aiki")).expect("symlink aiki onto PATH");
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(&bin, perms);

        let mut path_value = std::ffi::OsString::from(bin.as_os_str());
        path_value.push(":");
        path_value.push(std::env::var_os("PATH").unwrap_or_default());

        let fixture = Self {
            _base: base,
            root,
            repo,
            aiki_home,
            home,
            config,
            record,
            path_value,
            marker: spec.marker,
        };

        // Initialize the repo as a real aiki/jj project so the core
        // session.started flow (jj new, `aiki init --quiet`) succeeds and the
        // plugin's after-block actually fires.
        init_git_repo(&fixture.repo);
        let out = fixture
            .aiki()
            .arg("init")
            .output()
            .expect("run aiki init");
        assert!(
            out.status.success(),
            "aiki init failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );

        install_plugin(&fixture.aiki_home, spec.plugin_ref, spec.staged_yaml);
        wire_include(&fixture.repo, spec.plugin_ref);

        fixture
    }

    /// A hermetically-wired `aiki` command (stable home, fake PATH) rooted in
    /// the fixture repo.
    pub fn aiki(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("aiki").unwrap();
        cmd.current_dir(&self.repo)
            .env("AIKI_HOME", &self.aiki_home)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("JJ_USER", "Aiki Test")
            .env("JJ_EMAIL", "test@example.com")
            .env("PATH", &self.path_value);
        cmd
    }

    /// Fire one `hooks stdin` event through the REAL consumer path and return
    /// every line the stub host binaries recorded (empty if never invoked).
    ///
    /// `guard` controls whether the host marker (env vars / a file) is applied,
    /// so a test can assert both the in-host argv and the out-of-host no-op.
    pub fn fire(&self, agent: &str, event: &str, payload: &str, guard: Guard) -> Vec<String> {
        let _ = std::fs::remove_file(&self.record);

        let mut cmd = self.aiki();
        cmd.args(["hooks", "stdin", "--agent", agent, "--event", event])
            .env(HOST_RECORD_ENV, &self.record)
            .write_stdin(payload.to_string());

        match (&self.marker, &guard) {
            (Marker::Env(vars), Guard::Inside) => {
                for (k, v) in *vars {
                    cmd.env(k, v);
                }
            }
            (Marker::Env(vars), Guard::Outside) => {
                for (k, _) in *vars {
                    cmd.env_remove(k);
                }
            }
            (
                Marker::File {
                    path_env,
                    rel_path,
                    contents,
                },
                Guard::Inside,
            ) => {
                let p = self.root.join(rel_path);
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).expect("create marker-file dir");
                }
                std::fs::write(&p, contents).expect("write marker file");
                cmd.env(path_env, &p);
            }
            (
                Marker::File {
                    path_env, rel_path, ..
                },
                Guard::Outside,
            ) => {
                let p = self.root.join(rel_path);
                let _ = std::fs::remove_file(&p);
                cmd.env(path_env, &p); // points at a now-absent file
            }
        }

        let out = cmd.output().expect("run hooks stdin");
        assert!(
            out.status.success(),
            "hooks stdin ({agent}/{event}) failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );

        read_records(&self.record)
    }
}
