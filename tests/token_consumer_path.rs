//! Consumer-path integration test for the token pipeline
//! (token-tracking-fixes plan, Phase 1 / finding C1 + D1).
//!
//! This drives a **scripted Claude transcript end to end through the real
//! binary** and asserts the displayed total is non-zero — the gap D1 calls out:
//! the parser-only unit tests never touch the consumer path, so the always-0
//! display (C1) went uncaught.
//!
//! The pipeline exercised, all through `aiki hooks stdin` against the built
//! binary (not in-process helpers):
//!
//! 1. **Session + focus.** A `SessionStart` hook records the session; `aiki task
//!    start` then claims a task for that session (so the turn has a focused task
//!    to attribute to).
//! 2. **Extraction.** The scripted fake agent (Phase 0 infra) emits the committed
//!    golden Claude transcript; a `Stop` hook points at it and runs the REAL
//!    `parse_transcript_lines` extraction (streaming-snapshot dedup by message
//!    id + multi-call sum).
//! 3. **Record + bridge.** `handle_turn_completed` records the tokens onto a
//!    task-tagged history `Response` event and the C1 bridge
//!    (`token_rollup::record_turn_tokens`) writes the denormalized rollup onto
//!    `task.data["tokens"]`.
//! 4. **Consumer read.** The test reads back exactly what the display surfaces
//!    read — `task.data["tokens"]` (build TUI agent-stats, `aiki tldr`, run
//!    summary) and the public rollup readers
//!    (`token_rollup::direct_token_totals` / `subtree_total`) — and asserts a
//!    non-zero total. This is the consumer path, not the parser in isolation.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{fake_agent_path, scripted_transcript};
use tempfile::TempDir;

use aiki::tasks::token_rollup;
use aiki::tasks::{materialize_graph, read_events, Task, TaskGraph, TaskStatus};

/// Total billed tokens the current extraction produces for the committed Claude
/// fixture `claude_streaming_and_tool_use.jsonl`, summed across the turn's three
/// distinct API calls after the streaming-snapshot pair (`msg_01A`) is deduped
/// by message id to its finalized usage:
///
/// | call    | input | output | cache_read | cache_created |
/// |---------|------:|-------:|-----------:|--------------:|
/// | msg_01A |     4 |    178 |       8693 |         16911 |
/// | msg_02B | 12000 |     95 |      16915 |             0 |
/// | msg_03C | 12500 |    210 |      17000 |             0 |
/// | **sum** | 24504 |    483 |      42608 |         16911 |
///
/// `TokenUsage::total()` is the sum of all four disjoint buckets:
/// 24504 + 483 + 42608 + 16911 = 84506.
///
/// This is the *current* extraction output (the A1 message-id dedup has landed).
/// Later phases that change extraction should update this value deliberately; the
/// load-bearing assertion for Phase 1 is simply that the displayed total is
/// non-zero (it was always 0 before the C1 bridge).
const EXPECTED_TOTAL: u64 = 84_506;

/// The golden Claude transcript with a streaming-snapshot + finalized pair and a
/// genuine multi-tool-use turn (see `tests/fixtures/tokens/`).
const CLAUDE_FIXTURE: &str = "claude_streaming_and_tool_use.jsonl";

/// An aiki/jj repo with a hermetic, stable environment shared across every
/// `aiki` invocation in one test, and the built `aiki` on PATH (the core
/// `session.started` flow shells out to `aiki init --quiet`).
struct TokenFixture {
    _base: TempDir,
    scratch: PathBuf,
    repo: PathBuf,
    aiki_home: PathBuf,
    home: PathBuf,
    config: PathBuf,
    path_value: String,
}

impl TokenFixture {
    fn new() -> Self {
        let base = tempfile::tempdir().expect("create fixture base");
        let root = base.path().to_path_buf();
        let repo = root.join("repo");
        let aiki_home = root.join("aiki");
        let home = root.join("home");
        let config = home.join(".config");
        let bin = root.join("bin");
        let scratch = root.join("scratch");
        for dir in [&repo, &aiki_home, &home, &config, &bin, &scratch] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        // Canonicalize the repo so the per-user init marker (keyed by resolved
        // getcwd) matches the init-v2 gate's lookup from the payload `cwd` — on
        // macOS the /var symlink alias would otherwise key two markers and
        // SessionStart would resolve Dormant (no session recorded).
        let repo = repo.canonicalize().expect("canonicalize fixture repo");

        std::fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = Aiki Test\n\temail = test@example.com\n",
        )
        .expect("write gitconfig");

        // The built `aiki` must be resolvable on PATH: the core session.started
        // flow runs `shell: aiki init --quiet` (on_failure: stop).
        let aiki_bin: &Path = assert_cmd::cargo::cargo_bin!("aiki");
        std::os::unix::fs::symlink(aiki_bin, bin.join("aiki")).expect("symlink aiki onto PATH");
        let mut perms = std::fs::metadata(bin.join("aiki")).unwrap().permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(bin.join("aiki"), perms);

        let path_value = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let fixture = Self {
            _base: base,
            scratch,
            repo,
            aiki_home,
            home,
            config,
            path_value,
        };

        common::init_git_repo(&fixture.repo);
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

        fixture
    }

    /// A hermetically-wired `aiki` command rooted in the fixture repo.
    fn aiki(&self) -> Command {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
        cmd.current_dir(&self.repo)
            .env("AIKI_HOME", &self.aiki_home)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("JJ_USER", "Aiki Test")
            .env("JJ_EMAIL", "test@example.com")
            .env("PATH", &self.path_value);
        cmd
    }

    /// Fire one `hooks stdin` event with `payload` piped on stdin (exactly as a
    /// harness session hook would), asserting the process exits cleanly.
    fn fire(&self, event: &str, payload: &str) {
        use std::io::Write;
        use std::process::Stdio;

        let mut child = self
            .aiki()
            .args(["hooks", "stdin", "--agent", "claude", "--event", event])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn hooks stdin");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("write payload");
        let out = child.wait_with_output().expect("wait hooks stdin");
        assert!(
            out.status.success(),
            "hooks stdin ({event}) failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    /// Quick-start a task (`aiki task start "<desc>"`) and return its full id.
    /// The task is claimed by the active session via PID-ancestry discovery —
    /// both this process and the `SessionStart` hook are children of the test
    /// runner. The CLI prints only a short id prefix, so resolve it against the
    /// freshly-read graph.
    fn start_task(&self, desc: &str) -> String {
        let out = self
            .aiki()
            .args(["task", "start", desc])
            .output()
            .expect("run task start");
        assert!(
            out.status.success(),
            "task start failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let prefix = extract_task_id_prefix(&stdout)
            .unwrap_or_else(|| panic!("no task id prefix in start output: {stdout}"));
        let graph = self.task_graph();
        let matches: Vec<&String> = graph
            .tasks
            .keys()
            .filter(|id| id.starts_with(&prefix))
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "prefix {prefix:?} resolves to exactly one task, got {matches:?}",
        );
        matches[0].clone()
    }

    /// The task graph read from the repo's task storage — the same source the
    /// display surfaces read.
    fn task_graph(&self) -> TaskGraph {
        let events = read_events(&self.repo).expect("read task events");
        materialize_graph(&events)
    }
}

/// Extract an aiki task id prefix (JJ reverse-hex, `k`–`z` only) from CLI
/// output. `aiki task start` prints a slim `Started <prefix>` confirmation, so
/// this finds the `k`–`z` token (3+ chars; the verb "Started" has out-of-range
/// letters and is rejected).
fn extract_task_id_prefix(s: &str) -> Option<String> {
    s.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_ascii_lowercase()))
        .find(|word| word.len() >= 3 && word.bytes().all(|b| (b'k'..=b'z').contains(&b)))
        .map(str::to_string)
}

fn claude_session_start(session_id: &str, cwd: &str) -> String {
    serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "cwd": cwd,
        "source": "startup",
    })
    .to_string()
}

fn claude_stop(session_id: &str, cwd: &str, transcript_path: &str) -> String {
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "cwd": cwd,
        "transcript_path": transcript_path,
    })
    .to_string()
}

/// Read the denormalized total the display surfaces read off a task.
fn displayed_total(task: &Task) -> Option<u64> {
    task.data
        .get(token_rollup::TOKENS_DATA_KEY)
        .and_then(|s| s.parse::<u64>().ok())
}

/// End to end: a scripted Claude transcript drives the real extraction →
/// record → C1 bridge → display, and the displayed per-task total is non-zero.
#[test]
fn claude_transcript_lights_up_displayed_token_total() {
    if !common::jj_available() {
        eprintln!("skipping: jj not installed");
        return;
    }

    let fx = TokenFixture::new();
    let repo = fx.repo.to_string_lossy().to_string();
    let session_id = "tok-consumer-session";

    // 1. Session begins so the started task can be claimed by it.
    fx.fire("SessionStart", &claude_session_start(session_id, &repo));

    // 2. Start a task — the focused task this turn's tokens attribute to.
    let task_id = fx.start_task("Wire token display");

    // Precondition: the task is in progress and claimed by the session. If this
    // fails, focus attribution can't happen and the rest of the test is moot.
    {
        let graph = fx.task_graph();
        let task = graph.tasks.get(&task_id).expect("task exists after start");
        assert_eq!(task.status, TaskStatus::InProgress, "task is in progress");
        assert!(
            task.claimed_by_session.is_some(),
            "task is claimed by the active session (PID-ancestry discovery)",
        );
    }

    // 3. Emit the known Claude transcript via the Phase 0 scripted fake agent,
    //    to the path the Stop hook will read.
    let transcript = fx.scratch.join("transcript.jsonl");
    let mut emit = Command::new(fake_agent_path("claude"));
    scripted_transcript(&mut emit, CLAUDE_FIXTURE, &transcript);
    assert!(
        emit.status().expect("run scripted fake agent").success(),
        "scripted fake agent emits the transcript",
    );
    assert!(transcript.exists(), "transcript was written");

    // 4. Stop hook: REAL extraction → record_response (task-tagged) → C1 bridge.
    fx.fire(
        "Stop",
        &claude_stop(session_id, &repo, transcript.to_str().unwrap()),
    );

    // 5. Consumer path. The display reads task.data["tokens"]; assert it is the
    //    extracted total and, above all, non-zero (it was always 0 pre-C1).
    let graph = fx.task_graph();
    let task = graph.tasks.get(&task_id).expect("task exists");
    let shown = displayed_total(task);
    assert_eq!(
        shown,
        Some(EXPECTED_TOTAL),
        "task.data['tokens'] (what build/tldr/run-summary display) is the extracted total",
    );
    assert!(
        shown.unwrap() > 0,
        "the displayed token total is non-zero (the Phase 1 acceptance bar)",
    );

    // The rollup readers (public, back the read-through display) agree: the
    // task-tagged history Response events sum to the same total for this task.
    let history = aiki::history::storage::read_events(&fx.aiki_home).expect("read history");
    let direct = token_rollup::direct_token_totals(&history);
    assert_eq!(
        direct.get(&task_id).copied(),
        Some(EXPECTED_TOTAL),
        "direct per-task rollup over tagged Response events matches the displayed total",
    );
    assert_eq!(
        token_rollup::subtree_total(&graph, &direct, &task_id),
        EXPECTED_TOTAL,
        "subtree rollup (leaf == own direct total) matches the displayed total",
    );
}
