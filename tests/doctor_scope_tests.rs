//! Consumer-path tests for `aiki doctor` agent scoping flags.
//!
//! Drives the real binary and asserts on user-boundary output:
//! - Row (f): `--claude --codex` runs both agents' check sections as a union
//!   and no repo-wide section.
//! - Row (g): an agent-scoped run skips unrelated checks AND mutations — a
//!   stale per-user marker survives `doctor --claude` with and without
//!   `--fix`, while an unscoped run still reaps it.
//! - Row (p): `--quarantined` without `--fix` is rejected at flag-parse time
//!   with a usage error naming the requirement and the valid form; no checks
//!   run and no mutations occur.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Hermetic environment for one doctor invocation: fresh `HOME`, `AIKI_HOME`,
/// and working directory, so the spawned binary never reads or writes real
/// machine state. The temp dir is held for the struct's lifetime.
struct DoctorEnv {
    tmp: tempfile::TempDir,
}

impl DoctorEnv {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("create tempdir");
        for sub in ["home/.config", "aiki", "work"] {
            std::fs::create_dir_all(tmp.path().join(sub)).expect("create env dir");
        }
        Self { tmp }
    }

    fn aiki_home(&self) -> PathBuf {
        self.tmp.path().join("aiki")
    }

    fn doctor(&self, args: &[&str]) -> Output {
        let home = self.tmp.path().join("home");
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
        cmd.arg("doctor")
            .args(args)
            .current_dir(self.tmp.path().join("work"))
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("AIKI_HOME", self.aiki_home());
        cmd.output().expect("run aiki doctor")
    }

    /// Plant a per-user marker for a repo root that does not exist, mirroring
    /// `repos::marker_path`: `<AIKI_HOME>/.init/repos<repo_root>/enabled`.
    /// The unscoped doctor preamble reaps it; scoped runs must not touch it.
    fn plant_stale_marker(&self) -> PathBuf {
        let gone_root = self.tmp.path().join("gone-repo");
        let stripped = gone_root.strip_prefix("/").unwrap_or(&gone_root);
        let marker = self
            .aiki_home()
            .join(".init/repos")
            .join(stripped)
            .join("enabled");
        std::fs::create_dir_all(marker.parent().unwrap()).expect("create marker dir");
        std::fs::write(&marker, "").expect("write marker");
        marker
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Section output that must be absent from any agent-scoped run: jj, git,
/// aiki dir, editor config, other harnesses, plugins, templates.
const REPO_WIDE_MARKERS: &[&str] = &[
    "Prerequisites:",
    "Repository:",
    "JJ workspace",
    "Git repository",
    "Aiki directory",
    "Git hooks",
    "Cursor hooks",
    "Zed editor",
    "Gemini",
    "Local Configuration:",
    "Agent Instructions:",
    "Hookfile:",
    "Plugins:",
    "Templates:",
];

fn assert_no_repo_wide_sections(stdout: &str) {
    for marker in REPO_WIDE_MARKERS {
        assert!(
            !stdout.contains(marker),
            "scoped doctor output must not contain {marker:?}, got:\n{stdout}"
        );
    }
}

// ---------------------------------------------------------------------------
// Row (f): agent-filter union
// ---------------------------------------------------------------------------

#[test]
fn doctor_claude_codex_union_runs_both_agents_and_no_repo_wide_sections() {
    let env = DoctorEnv::new();
    let output = env.doctor(&["--claude", "--codex"]);
    assert!(output.status.success(), "doctor should exit 0");

    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("Claude Code hooks"),
        "expected Claude Code hooks check, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Codex hooks"),
        "expected Codex hooks check, got:\n{stdout}"
    );
    assert!(
        stdout.contains("OTel receiver"),
        "expected Codex OTel receiver check, got:\n{stdout}"
    );
    assert!(
        stdout.contains("ACP Agent Binaries:"),
        "expected agent binary checks, got:\n{stdout}"
    );
    assert_no_repo_wide_sections(&stdout);
}

// ---------------------------------------------------------------------------
// Row (g): scoped doctor skips unrelated checks AND mutations
// ---------------------------------------------------------------------------

#[test]
fn doctor_claude_scope_skips_codex_checks_and_leaves_stale_marker() {
    let env = DoctorEnv::new();
    let marker = env.plant_stale_marker();

    let output = env.doctor(&["--claude"]);
    assert!(output.status.success(), "doctor should exit 0");

    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("Claude Code hooks"),
        "expected Claude Code hooks check, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("Codex hooks"),
        "claude-scoped doctor must not run Codex checks, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("OTel receiver"),
        "claude-scoped doctor must not run the OTel check, got:\n{stdout}"
    );
    assert_no_repo_wide_sections(&stdout);
    assert!(
        marker.exists(),
        "claude-scoped doctor must not reap stale markers"
    );
}

#[test]
fn doctor_claude_scope_with_fix_leaves_stale_marker() {
    let env = DoctorEnv::new();
    let marker = env.plant_stale_marker();

    let output = env.doctor(&["--claude", "--fix"]);
    assert!(output.status.success(), "doctor --fix should exit 0");

    let stdout = stdout_of(&output);
    assert_no_repo_wide_sections(&stdout);
    assert!(
        marker.exists(),
        "claude-scoped doctor --fix must not reap stale markers"
    );
}

#[test]
fn doctor_unscoped_reaps_stale_marker() {
    let env = DoctorEnv::new();
    let marker = env.plant_stale_marker();

    let output = env.doctor(&[]);
    assert!(output.status.success(), "doctor should exit 0");

    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("Reaped 1 stale aiki marker(s)"),
        "unscoped doctor should report the reap, got:\n{stdout}"
    );
    assert!(
        !marker.exists(),
        "unscoped doctor must reap the stale marker"
    );
}

// ---------------------------------------------------------------------------
// Row (p): --quarantined without --fix is a flag-parse usage error
// ---------------------------------------------------------------------------

fn assert_quarantined_usage_error(env: &DoctorEnv, args: &[&str], marker: &Path) {
    let output = env.doctor(args);
    assert!(
        !output.status.success(),
        "doctor {args:?} must exit nonzero"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("required arguments were not provided") && stderr.contains("--fix"),
        "usage error must name the --fix requirement, got:\n{stderr}"
    );
    assert!(
        stderr.contains("aiki doctor --fix --quarantined [--<agent>]"),
        "usage error must name the valid form, got:\n{stderr}"
    );

    // Rejected at flag-parse time: no checks ran, no mutations occurred.
    let stdout = stdout_of(&output);
    assert!(
        stdout.is_empty(),
        "no checks may run on a usage error, got stdout:\n{stdout}"
    );
    assert!(
        marker.exists(),
        "a usage error must not reap stale markers"
    );
}

#[test]
fn doctor_quarantined_without_fix_is_usage_error() {
    let env = DoctorEnv::new();
    let marker = env.plant_stale_marker();
    assert_quarantined_usage_error(&env, &["--quarantined"], &marker);
}

#[test]
fn doctor_quarantined_codex_without_fix_is_usage_error() {
    let env = DoctorEnv::new();
    let marker = env.plant_stale_marker();
    assert_quarantined_usage_error(&env, &["--quarantined", "--codex"], &marker);
}

#[test]
fn doctor_fix_quarantined_parses() {
    let env = DoctorEnv::new();
    let output = env.doctor(&["--fix", "--quarantined", "--claude"]);
    assert!(
        output.status.success(),
        "the valid form must parse and run, stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
