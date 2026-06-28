//! Consumer-path integration tests for the staged `plugins/herdr/hooks.yaml`
//! plugin (`aiki-sh/herdr`).
//!
//! These drive the plugin through the REAL binary-stdin consumer path —
//! `aiki hooks stdin --agent <a> --event <E>` with a JSON payload piped on
//! stdin — exactly as a harness session hook would. The event flows through
//! `event_bus::dispatch` → `handle_session_started`/`handle_session_resumed` →
//! the `HookComposer`, which loads the project's `.aiki/hooks.yml`, expands its
//! `include: [aiki-sh/aiki-plugin-herdr]`, and runs the plugin's
//! `session.started`/`session.resumed` shell action.
//!
//! The generic setup (hermetic repo, staged-plugin install, stub host binary,
//! `aiki hooks stdin` driver) now lives in [`common::PluginFixture`]; this file
//! is just the herdr `PluginSpec` plus herdr's identity-contract assertions.
//!
//! Contract asserted (from plan 01 — its acceptance gate):
//! - `--source` is `herdr:claude` / `herdr:codex` (NEVER `aiki:*`).
//! - `--agent` matches the harness label, `--agent-session-id` matches the
//!   payload's session id.
//! - `--state` is NEVER sent (identity-only contract).
//! - Outside a herdr pane (no `HERDR_ENV`/`HERDR_PANE_ID`) the action no-ops:
//!   ZERO `report-agent-session` calls.

mod common;

use std::path::Path;

use common::{jj_available, Guard, Marker, PluginFixture, PluginSpec};

/// The staged plugin content, baked in at compile time so the test exercises
/// the real `plugins/herdr/hooks.yaml`, not a copy that can drift.
const STAGED_HERDR_PLUGIN: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../plugins/herdr/hooks.yaml"));

/// The herdr plugin under test: staged file, a stub `herdr` on PATH, and the
/// "inside a pane" marker (`HERDR_ENV=1` + `HERDR_PANE_ID`).
fn herdr_spec() -> PluginSpec {
    PluginSpec {
        plugin_ref: "aiki-sh/aiki-plugin-herdr",
        staged_yaml: STAGED_HERDR_PLUGIN,
        stub_bins: &["herdr"],
        marker: Marker::Env(&[("HERDR_ENV", "1"), ("HERDR_PANE_ID", "test-pane-42")]),
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

    let fx = PluginFixture::new(herdr_spec());

    // session.started (source = "startup")
    let started = fx.fire(
        "claude-code",
        "SessionStart",
        &claude_payload(&fx.repo, "claude-sess-001", "startup"),
        Guard::Inside,
    );
    assert_identity_reported(&started, "herdr:claude", "claude", "claude-sess-001");

    // session.resumed (source = "resume")
    let resumed = fx.fire(
        "claude-code",
        "SessionStart",
        &claude_payload(&fx.repo, "claude-sess-002", "resume"),
        Guard::Inside,
    );
    assert_identity_reported(&resumed, "herdr:claude", "claude", "claude-sess-002");
}

#[test]
fn codex_reports_identity_on_start_and_resume() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let fx = PluginFixture::new(herdr_spec());

    // session.started (source = "startup")
    let started = fx.fire(
        "codex",
        "sessionStart",
        &codex_payload(&fx.repo, "codex-sess-001", "startup"),
        Guard::Inside,
    );
    assert_identity_reported(&started, "herdr:codex", "codex", "codex-sess-001");

    // session.resumed (source = "resume")
    let resumed = fx.fire(
        "codex",
        "sessionStart",
        &codex_payload(&fx.repo, "codex-sess-002", "resume"),
        Guard::Inside,
    );
    assert_identity_reported(&resumed, "herdr:codex", "codex", "codex-sess-002");
}

#[test]
fn no_report_when_not_under_herdr_pane() {
    if !jj_available() {
        eprintln!("Skipping test: jj binary not found in PATH");
        return;
    }

    let fx = PluginFixture::new(herdr_spec());

    // Same event, but WITHOUT the herdr-pane env: the shell self-guard must
    // exit 0 before calling `herdr` at all.
    let records = fx.fire(
        "claude-code",
        "SessionStart",
        &claude_payload(&fx.repo, "claude-sess-003", "startup"),
        Guard::Outside,
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
