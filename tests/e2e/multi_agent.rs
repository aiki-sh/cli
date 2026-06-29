//! Multi-agent (cross-agent) e2e tests: one workflow that hands work between two
//! DIFFERENT harnesses (one codes, another reviews). These REQUIRE both claude
//! and codex installed, so they run only in the multi-agent rig
//! (`cli/tests/harness-rig` with `HARNESS=multi`, which installs both CLIs and
//! mounts both creds), never in a single-agent image. The single-agent `review`
//! tests in `task_lifecycle.rs` cover one-agent build+review; these cover the
//! genuine cross-agent handoff that a single-agent container cannot run.

use super::*;

/// claude builds the plan (coder), codex reviews the resulting epic (reviewer).
#[test]
#[ignore] // e2e: requires BOTH claude and codex + API keys
fn e2e_multi_claude_builds_codex_reviews() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("claude") {
        eprintln!("Skipping: claude binary not available");
        return;
    }
    if !agent_available("codex") {
        eprintln!("Skipping: codex binary not available");
        return;
    }
    crate::task_lifecycle::run_build_and_review("claude-code", "codex");
}

/// The mirror handoff: codex builds, claude reviews.
#[test]
#[ignore] // e2e: requires BOTH claude and codex + API keys
fn e2e_multi_codex_builds_claude_reviews() {
    if !jj_available() {
        eprintln!("Skipping: jj not available");
        return;
    }
    if !agent_available("claude") {
        eprintln!("Skipping: claude binary not available");
        return;
    }
    if !agent_available("codex") {
        eprintln!("Skipping: codex binary not available");
        return;
    }
    crate::task_lifecycle::run_build_and_review("codex", "claude-code");
}
