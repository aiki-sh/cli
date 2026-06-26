//! Consumer-path integration tests for the staged `plugins/herdr/hooks.yaml`
//! plugin (`aiki-sh/aiki-plugin-herdr`).
//!
//! These tests drive the plugin through the REAL binary-stdin consumer path —
//! `aiki hooks stdin --agent <a> --event <E>` with a JSON payload piped on
//! stdin — exactly as a harness session hook would. They are NOT in-process
//! recorders: the event flows through `event_bus::dispatch` →
//! `handle_session_started`/`handle_session_resumed` → the `HookComposer`,
//! which loads the project's `.aiki/hooks.yml`, expands its
//! `include: [aiki-sh/aiki-plugin-herdr]`, and runs the plugin's
//! `session.started`/`session.resumed` shell action.
//!
//! The plugin is exercised via the EXACT staged file (`include_str!` of
//! `plugins/herdr/hooks.yaml`), installed into a per-test `$AIKI_HOME/plugins`
//! so the resolver finds it locally (no network fetch).
//!
//! Contract asserted (from plan 01 — this is its acceptance gate):
//! - `--source` is `herdr:claude` / `herdr:codex` (NEVER `aiki:*`).
//! - `--agent` matches the harness label, `--agent-session-id` matches the
//!   payload's session id.
//! - `--state` is NEVER sent (identity-only contract).
//! - Outside a herdr pane (no `HERDR_ENV`/`HERDR_PANE_ID`) the action no-ops:
//!   ZERO `report-agent-session` calls.

// `assert_cmd::Command::cargo_bin` is deprecated in favor of `cargo_bin_cmd!`,
// but the rest of the suite (common/mod.rs, end_to_end_tests.rs) still uses it;
// match that convention rather than diverge.
#![allow(deprecated)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::{init_git_repo, jj_available};
use tempfile::TempDir;

/// The staged plugin content, baked in at compile time so the test exercises
/// the real `plugins/herdr/hooks.yaml`, not a copy that can drift.
const STAGED_HERDR_PLUGIN: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../plugins/herdr/hooks.yaml"));

/// A stub `herdr` CLI placed on PATH. It appends its full argv (space-joined,
/// one line per invocation) to `$AIKI_TEST_HERDR_RECORD`, then exits 0 — so a
/// test can prove exactly what the plugin invoked.
const STUB_HERDR: &str = "#!/bin/sh\n\
: \"${AIKI_TEST_HERDR_RECORD:=/dev/null}\"\n\
printf '%s\\n' \"$*\" >> \"$AIKI_TEST_HERDR_RECORD\"\n\
exit 0\n";

/// An aiki/jj repo wired to the staged herdr plugin, with a stub `herdr` and
/// the built `aiki` on PATH and a hermetic, stable environment shared across
/// every `aiki` invocation in one test.
struct HerdrFixture {
    _base: TempDir,
    repo: PathBuf,
    aiki_home: PathBuf,
    home: PathBuf,
    config: PathBuf,
    records: PathBuf,
    path_value: String,
}

impl HerdrFixture {
    fn new() -> Self {
        let base = tempfile::tempdir().expect("create fixture base");
        let root = base.path();
        let repo = root.join("repo");
        let aiki_home = root.join("aiki");
        let home = root.join("home");
        let config = home.join(".config");
        let bin = root.join("bin");
        let records = root.join("records");
        for dir in [&repo, &aiki_home, &home, &config, &bin, &records] {
            fs::create_dir_all(dir).expect("create fixture dir");
        }
        // Canonicalize the repo so the per-user marker `aiki init` keys by its
        // resolved getcwd matches the init-v2 gate's lookup from the payload
        // `cwd` (which is `repo` verbatim) — otherwise the /var symlink alias on
        // macOS keys two markers and SessionStart resolves to Dormant.
        let repo = repo.canonicalize().expect("canonicalize fixture repo");

        fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Aiki Test\n\temail = test@example.com\n",
        )
        .expect("write gitconfig");

        // Stub `herdr` on PATH (records argv).
        let herdr = bin.join("herdr");
        fs::write(&herdr, STUB_HERDR).expect("write stub herdr");
        let mut perms = fs::metadata(&herdr).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&herdr, perms).expect("chmod stub herdr");

        // The built `aiki` must be on PATH: the core `session.started` flow runs
        // `shell: aiki init --quiet` (with on_failure: stop). If `aiki` can't be
        // resolved there, core stops and the user hooks.yml (the plugin) never
        // runs — which would mask the contract under test.
        let aiki_bin: &Path = assert_cmd::cargo::cargo_bin!("aiki");
        std::os::unix::fs::symlink(aiki_bin, bin.join("aiki")).expect("symlink aiki onto PATH");

        let path_value = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let fixture = Self {
            _base: base,
            repo,
            aiki_home,
            home,
            config,
            records,
            path_value,
        };

        // Initialize the repo as a real aiki/jj project so the core
        // session.started flow (jj new, `aiki init --quiet`) succeeds and the
        // plugin's after-block actually fires.
        init_git_repo(&fixture.repo);
        fixture
            .aiki()
            .arg("init")
            .output()
            .map(|o| {
                assert!(
                    o.status.success(),
                    "aiki init failed: stdout={} stderr={}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr),
                );
            })
            .expect("run aiki init");

        // Install the STAGED plugin where the resolver finds it without a
        // network fetch: $AIKI_HOME/plugins/{namespace}/{name}/hooks.yaml.
        let plugin_dir = fixture
            .aiki_home
            .join("plugins")
            .join("aiki-sh")
            .join("aiki-plugin-herdr");
        fs::create_dir_all(&plugin_dir).expect("create plugin dir");
        fs::write(plugin_dir.join("hooks.yaml"), STAGED_HERDR_PLUGIN).expect("install plugin");

        // Wire the plugin into the project's hookfile via a top-level include.
        let aiki_dir = fixture.repo.join(".aiki");
        fs::create_dir_all(&aiki_dir).expect("create .aiki dir");
        fs::write(
            aiki_dir.join("hooks.yml"),
            "include:\n  - aiki-sh/aiki-plugin-herdr\n",
        )
        .expect("write .aiki/hooks.yml");

        fixture
    }

    /// A hermetically-wired `aiki` command (stable home, fake PATH) rooted in
    /// the fixture repo.
    fn aiki(&self) -> assert_cmd::Command {
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

    /// Fire one `hooks stdin` event and return every line the stub `herdr`
    /// recorded (empty if it was never invoked).
    ///
    /// `under_herdr` controls whether the "inside a pane" env
    /// (`HERDR_ENV=1` + `HERDR_PANE_ID`) is set.
    fn fire(
        &self,
        tag: &str,
        agent: &str,
        event_arg: &str,
        payload: &str,
        under_herdr: bool,
    ) -> Vec<String> {
        let record = self.records.join(tag);
        let _ = fs::remove_file(&record);

        let mut cmd = self.aiki();
        cmd.args(["hooks", "stdin", "--agent", agent, "--event", event_arg])
            .env("AIKI_TEST_HERDR_RECORD", &record)
            .write_stdin(payload.to_string());

        if under_herdr {
            cmd.env("HERDR_ENV", "1").env("HERDR_PANE_ID", "test-pane-42");
        } else {
            cmd.env_remove("HERDR_ENV").env_remove("HERDR_PANE_ID");
        }

        let out = cmd.output().expect("run hooks stdin");
        assert!(
            out.status.success(),
            "hooks stdin ({agent}/{event_arg}) failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );

        read_records(&record)
    }
}

/// Read recorded herdr invocations (one per line); empty if the file is absent.
fn read_records(path: &Path) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(contents) => contents
            .lines()
            .map(|l| l.to_string())
            .filter(|l| !l.trim().is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Assert that exactly one identity report was recorded with the expected
/// source/agent/session-id and that `--state` was never sent.
fn assert_identity_reported(
    records: &[String],
    expected_source: &str,
    expected_agent: &str,
    expected_session_id: &str,
) {
    let reports: Vec<&String> = records
        .iter()
        .filter(|l| l.contains("report-agent-session"))
        .collect();
    assert_eq!(
        reports.len(),
        1,
        "expected exactly one report-agent-session call, got {}: {:?}",
        reports.len(),
        records,
    );

    let line = reports[0];
    assert!(
        line.contains(&format!("--source {expected_source}")),
        "report must use --source {expected_source}, got: {line}",
    );
    assert!(
        line.contains(&format!("--agent {expected_agent}")),
        "report must use --agent {expected_agent}, got: {line}",
    );
    assert!(
        line.contains(&format!("--agent-session-id {expected_session_id}")),
        "report must carry --agent-session-id {expected_session_id}, got: {line}",
    );

    // Identity-only contract: no lifecycle state is ever sent.
    for l in records {
        assert!(
            !l.contains("--state"),
            "plugin must NEVER send --state, got: {l}",
        );
    }
}

/// Build a Claude SessionStart payload with the given source ("startup" →
/// session.started, "resume" → session.resumed).
fn claude_payload(cwd: &Path, session_id: &str, source: &str) -> String {
    serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "source": source,
    })
    .to_string()
}

/// Build a Codex SessionStart payload (Codex uses `deny_unknown_fields`, so all
/// required fields must be present and no extras).
fn codex_payload(cwd: &Path, session_id: &str, source: &str) -> String {
    serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "cwd": cwd.to_string_lossy(),
        "source": source,
        "model": "o3",
        "permission_mode": "default",
        "transcript_path": null,
    })
    .to_string()
}

#[test]
fn claude_reports_identity_on_start_and_resume() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let fx = HerdrFixture::new();

    // session.started (source = "startup")
    let started = fx.fire(
        "claude-started",
        "claude-code",
        "SessionStart",
        &claude_payload(&fx.repo, "claude-sess-001", "startup"),
        true,
    );
    assert_identity_reported(&started, "herdr:claude", "claude", "claude-sess-001");

    // session.resumed (source = "resume")
    let resumed = fx.fire(
        "claude-resumed",
        "claude-code",
        "SessionStart",
        &claude_payload(&fx.repo, "claude-sess-002", "resume"),
        true,
    );
    assert_identity_reported(&resumed, "herdr:claude", "claude", "claude-sess-002");
}

#[test]
fn codex_reports_identity_on_start_and_resume() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let fx = HerdrFixture::new();

    // session.started (source = "startup")
    let started = fx.fire(
        "codex-started",
        "codex",
        "sessionStart",
        &codex_payload(&fx.repo, "codex-sess-001", "startup"),
        true,
    );
    assert_identity_reported(&started, "herdr:codex", "codex", "codex-sess-001");

    // session.resumed (source = "resume")
    let resumed = fx.fire(
        "codex-resumed",
        "codex",
        "sessionStart",
        &codex_payload(&fx.repo, "codex-sess-002", "resume"),
        true,
    );
    assert_identity_reported(&resumed, "herdr:codex", "codex", "codex-sess-002");
}

#[test]
fn no_report_when_not_under_herdr_pane() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let fx = HerdrFixture::new();

    // Same event, but WITHOUT the herdr-pane env: the shell self-guard must
    // exit 0 before calling `herdr` at all.
    let records = fx.fire(
        "claude-no-herdr",
        "claude-code",
        "SessionStart",
        &claude_payload(&fx.repo, "claude-sess-003", "startup"),
        false,
    );

    let reports = records
        .iter()
        .filter(|l| l.contains("report-agent-session"))
        .count();
    assert_eq!(
        reports, 0,
        "outside a herdr pane the plugin must not call herdr; got: {records:?}",
    );
}
