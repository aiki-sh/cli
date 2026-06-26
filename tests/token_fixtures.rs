//! Token-tracking test scaffolding (token-tracking-fixes plan, Phase 0 / D1+D2).
//!
//! Two things land here so the later phases are test-guarded:
//!
//! 1. **Scripted-transcript fake agent.** The shared fake `claude`/`codex`
//!    binaries gained a scripted mode (see `common::FAKE_AGENT_SCRIPT`): given
//!    `AIKI_FAKE_TRANSCRIPT_SRC`/`AIKI_FAKE_TRANSCRIPT_DEST` they copy a fixture
//!    to the path the harness later reads, instead of the old empty no-op that
//!    exercised none of the extraction path. This test proves the mode works at
//!    the process boundary.
//! 2. **Golden fixtures** under `tests/fixtures/tokens/`. This test asserts each
//!    committed fixture is well-formed realistic provider output. The parsers
//!    are driven against the same files by unit tests in
//!    `cli/src/editors/{claude_code,codex}/events.rs`; the exact post-fix
//!    `TokenUsage` assertions land with the Phase 2 extractor fixes.

mod common;

use std::process::Command;

use common::{fake_agent_path, scripted_transcript, tokens_fixture_path};

/// The scripted fake agent emits the named fixture to the destination path
/// (simulating a harness flushing its transcript JSONL), byte-for-byte.
#[test]
fn scripted_fake_agent_emits_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    // A nested destination so we also cover the `mkdir -p` in the script.
    let dest = tmp.path().join("sessions").join("transcript.jsonl");

    let mut cmd = Command::new(fake_agent_path("claude"));
    scripted_transcript(&mut cmd, "claude_streaming_and_tool_use.jsonl", &dest);
    let status = cmd.status().expect("fake agent runs");
    assert!(status.success(), "scripted fake agent exits 0");

    let emitted = std::fs::read_to_string(&dest).expect("transcript was written");
    let fixture =
        std::fs::read_to_string(tokens_fixture_path("claude_streaming_and_tool_use.jsonl")).unwrap();
    assert_eq!(emitted, fixture, "emitted transcript matches the fixture");
}

/// Without the scripted env vars the fake agent stays a pure no-op: it writes
/// nothing and still exits 0 (the historical behavior other tests rely on).
#[test]
fn fake_agent_without_script_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("transcript.jsonl");

    // Only set DEST, not SRC: scripted mode requires both, so this is a no-op.
    let status = Command::new(fake_agent_path("codex"))
        .env(common::FAKE_TRANSCRIPT_DEST_ENV, &dest)
        .status()
        .expect("fake agent runs");
    assert!(status.success());
    assert!(!dest.exists(), "no transcript written without a fixture src");
}

/// Claude and Codex fixtures are valid JSONL: every non-empty line parses.
#[test]
fn jsonl_fixtures_are_well_formed() {
    for name in [
        "claude_streaming_and_tool_use.jsonl",
        "codex_multi_turn_resume.jsonl",
    ] {
        let content = std::fs::read_to_string(tokens_fixture_path(name)).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert!(!lines.is_empty(), "{name} is non-empty");
        for (i, line) in lines.iter().enumerate() {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("{name} line {} is not valid JSON: {e}", i + 1));
        }
    }
}

/// The Claude fixture carries the streaming-snapshot + finalized pair for the
/// same API call (the A1 double-count shape): two assistant entries with the
/// same `message.id`, identical cache buckets, differing only in `output_tokens`.
#[test]
fn claude_fixture_has_streaming_snapshot_pair() {
    let content =
        std::fs::read_to_string(tokens_fixture_path("claude_streaming_and_tool_use.jsonl")).unwrap();
    let mut by_id: std::collections::HashMap<String, Vec<serde_json::Value>> = Default::default();
    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let msg = &v["message"];
        let id = msg["id"].as_str().unwrap().to_string();
        by_id.entry(id).or_default().push(msg.clone());
    }

    let pair = by_id
        .values()
        .find(|entries| entries.len() == 2)
        .expect("a message id with both a snapshot and a finalized entry");
    let snap = &pair[0];
    let finalized = &pair[1];
    // Same call: identical cache buckets and input, differing output only.
    for bucket in ["input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"] {
        assert_eq!(
            snap["usage"][bucket], finalized["usage"][bucket],
            "snapshot and finalized share {bucket} (same API call)"
        );
    }
    assert_ne!(
        snap["usage"]["output_tokens"], finalized["usage"]["output_tokens"],
        "output grows from snapshot to finalized"
    );
}

/// The Codex fixture carries reasoning-output tokens (dropped today, A4) and a
/// resume boundary: a `turn_context` with a large cumulative total and no
/// preceding `token_count` (the A5 shape).
#[test]
fn codex_fixture_has_reasoning_and_resume_boundary() {
    let content =
        std::fs::read_to_string(tokens_fixture_path("codex_multi_turn_resume.jsonl")).unwrap();

    let mut seen_token_count = false;
    let mut resume_boundary_with_large_total = false;
    let mut reasoning_present = false;

    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    for (i, line) in lines.iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let ptype = v["payload"]["type"].as_str();
        if v["type"] == "turn_context" && !seen_token_count {
            // First turn_context precedes any token_count: a resume boundary.
            // The next token_count must already carry a large cumulative total.
            if let Some(next) = lines.get(i + 1) {
                let nv: serde_json::Value = serde_json::from_str(next).unwrap();
                let total = nv["payload"]["info"]["total_token_usage"]["input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
                if nv["payload"]["type"] == "token_count" && total > 10_000 {
                    resume_boundary_with_large_total = true;
                }
            }
        }
        if ptype == Some("token_count") {
            seen_token_count = true;
            let reasoning = &v["payload"]["info"]["total_token_usage"]["reasoning_output_tokens"];
            if reasoning.as_u64().unwrap_or(0) > 0 {
                reasoning_present = true;
            }
        }
    }

    assert!(
        resume_boundary_with_large_total,
        "fixture has a resume boundary with a large cumulative total and no preceding token_count"
    );
    assert!(reasoning_present, "fixture carries reasoning_output_tokens");
}

/// The ACP fixture is a `PromptResponse` carrying per-turn usage in `_meta`.
#[test]
fn acp_fixture_has_meta_usage() {
    let content = std::fs::read_to_string(tokens_fixture_path("acp_prompt_response_meta.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(v["stopReason"], "end_turn");
    let usage = &v["_meta"]["usage"];
    assert!(usage["input_tokens"].as_u64().is_some(), "_meta usage carries input");
    assert!(usage["output_tokens"].as_u64().is_some(), "_meta usage carries output");
}
