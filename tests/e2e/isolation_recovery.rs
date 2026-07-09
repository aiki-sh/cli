//! Isolation failure-mode e2e tests (ops/now/isolation plans 02–12).
//!
//! The other e2e suites prove the happy path; these prove the SAFETY
//! behavior the isolation-fixes branch exists for:
//!
//! 1. **Crash recovery** — an agent SIGKILLed mid-turn must not lose its
//!    already-written work: the next aiki invocation recovers it into the
//!    main repo (or a recovery bookmark / quarantine surfaces it).
//! 2. **Concurrent absorption** — two live sessions absorbing back-to-back
//!    must both survive on disk (the stale-snapshot revert + session-start
//!    race incidents).
//! 3. **Stale-worker watchdog** — a live-but-silent agent is killed, its
//!    task returns to a restartable (stopped) state, and `aiki run` exits
//!    with an error instead of waiting forever.
//!
//! Each scenario has a per-harness wrapper so `TESTFILTER=e2e_claude` /
//! `e2e_codex` in the harness rig picks them up.

use super::*;
use std::time::Duration;
use tempfile::tempdir;

/// Read `<repo>/.aiki/repo-id` (written by `aiki init`).
fn repo_id(repo: &Path) -> String {
    std::fs::read_to_string(repo.join(".aiki/repo-id"))
        .expect("repo-id file")
        .trim()
        .to_string()
}

/// The session's isolated working copy: `/tmp/aiki/<repo-id>/<uuid>/main`,
/// falling back to the container root for a legacy layout.
fn session_workspace(repo: &Path, session_id: &str) -> std::path::PathBuf {
    let container = std::path::PathBuf::from("/tmp/aiki")
        .join(repo_id(repo))
        .join(session_id);
    let main_slot = container.join("main");
    if main_slot.exists() {
        main_slot
    } else {
        container
    }
}

/// Agent PID recorded in the test-hermetic session file.
fn session_agent_pid(repo: &Path, session_id: &str) -> Option<u32> {
    let path = crate::common::e2e_aiki_home(repo)
        .join("sessions")
        .join(session_id);
    let content = std::fs::read_to_string(path).ok()?;
    content
        .lines()
        .find_map(|l| l.trim().strip_prefix("parent_pid="))
        .and_then(|v| v.parse().ok())
}

/// `aiki run <task> --async -o id` → session UUID.
fn run_async_get_session(repo: &Path, task_id: &str, agent_args: &[&str]) -> String {
    let mut args = vec!["run", task_id, "--async", "-o", "id"];
    args.extend_from_slice(agent_args);
    let output = crate::common::e2e_aiki_agent(repo)
        .current_dir(repo)
        .args(&args)
        .timeout(Duration::from_secs(120))
        .output()
        .expect("aiki run --async");
    assert!(
        output.status.success(),
        "aiki run --async failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(!sid.is_empty(), "run --async -o id printed no session id");
    sid
}

/// Poll until `path` exists, up to `timeout`.
fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// `aiki recover` output for the repo (list of recovery bookmarks +
/// quarantined dirs). Used as the fallback assertion surface.
fn recover_list(repo: &Path) -> String {
    let output = crate::common::e2e_aiki(repo)
        .current_dir(repo)
        .args(["recover"])
        .output()
        .expect("aiki recover");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

// =============================================================================
// 1. Crash recovery: SIGKILL mid-turn must not lose written work
// =============================================================================

fn run_crash_recovery_preserves_work(agent_args: &[&str]) {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    let task_id = create_task(repo, "e2e crash: write file then stall");
    set_task_instructions(
        repo,
        &task_id,
        "First, create a file called precious.txt with exactly the content 'survives'. \
         After the file is created, run the shell command `sleep 120`. \
         After the sleep finishes, close this task.",
    );

    let session_id = run_async_get_session(repo, &task_id, agent_args);
    let ws = session_workspace(repo, &session_id);

    // Wait for the agent to write the file into its isolated workspace.
    assert!(
        wait_for_path(&ws.join("precious.txt"), Duration::from_secs(180)),
        "agent never wrote precious.txt into its workspace at {}",
        ws.display()
    );

    // Give the per-tool change.completed snapshot a beat to commit the file
    // into the shared store, then SIGKILL the agent mid-sleep — the harshest
    // interruption: no Stop hook, no session.ended, no absorb.
    std::thread::sleep(Duration::from_secs(5));
    let pid = session_agent_pid(repo, &session_id)
        .expect("session file should record the agent pid");
    let killed = process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("run kill");
    assert!(killed.success(), "kill -9 {pid} failed");

    // Recovery is triggered by the next aiki invocation's session pruning
    // (`aiki session list` runs cleanup_stale_sessions + prune_dead_pid_sessions,
    // which routes the dead session through recover_orphaned_workspaces).
    // Poll: the kill and the PID table can lag a moment.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut recovered = false;
    while Instant::now() < deadline {
        let _ = crate::common::e2e_aiki(repo)
            .current_dir(repo)
            .args(["session", "list"])
            .output();
        if repo.join("precious.txt").exists() {
            recovered = true;
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
    }

    if recovered {
        let content = std::fs::read_to_string(repo.join("precious.txt")).unwrap();
        assert!(
            content.contains("survives"),
            "recovered file has wrong content: {content:?}"
        );
    } else {
        // Absorption may legitimately fall back to preservation: the work
        // must then be discoverable — in jj history, on a recovery bookmark,
        // or in quarantine. Silent loss is the only failure.
        let in_history = file_in_jj_history(repo, "precious.txt");
        let recover_out = recover_list(repo);
        let surfaced = recover_out.contains("aiki/recovered/")
            || recover_out.contains("Quarantined workspace directories");
        assert!(
            in_history || surfaced,
            "killed session's work is GONE: not in main working copy, not in jj \
             history, and `aiki recover` surfaces nothing. recover output:\n{recover_out}"
        );
    }
}

#[test]
#[ignore] // e2e: requires claude binary + API key
fn e2e_claude_crash_recovery_preserves_work() {
    if !jj_available() || !agent_available("claude") {
        eprintln!("Skipping: jj/claude not available");
        return;
    }
    run_crash_recovery_preserves_work(&[]);
}

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_crash_recovery_preserves_work() {
    if !jj_available() || !agent_available("codex") {
        eprintln!("Skipping: jj/codex not available");
        return;
    }
    run_crash_recovery_preserves_work(&["--agent", "codex"]);
}

// =============================================================================
// 2. Concurrent sessions: both sessions' work survives absorption
// =============================================================================

fn run_concurrent_sessions_both_survive(agent_args: &[&str]) {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    let task_a = create_task(repo, "e2e concurrent A: create file_a.txt");
    set_task_instructions(
        repo,
        &task_a,
        "Create a file called file_a.txt with exactly the content 'from A'. \
         Do nothing else, then close this task with confidence 4.",
    );
    let task_b = create_task(repo, "e2e concurrent B: create file_b.txt");
    set_task_instructions(
        repo,
        &task_b,
        "Create a file called file_b.txt with exactly the content 'from B'. \
         Do nothing else, then close this task with confidence 4.",
    );

    // Launch both sessions concurrently — their session-start `jj new`,
    // per-tool snapshots, and end-of-session absorptions interleave. This is
    // the exact shape of the 2026-04-27 “absorbed changes silently reverted”
    // incident and the session-start stranding race (plans 05/08/13).
    let sid_a = run_async_get_session(repo, &task_a, agent_args);
    let sid_b = run_async_get_session(repo, &task_b, agent_args);

    // Wait for both sessions to complete and absorb.
    let wait = crate::common::e2e_aiki(repo)
        .current_dir(repo)
        .args(["session", "wait", &sid_a, &sid_b, "--timeout", "240"])
        .timeout(Duration::from_secs(260))
        .output()
        .expect("aiki session wait");
    eprintln!(
        "session wait: {}{}",
        String::from_utf8_lossy(&wait.stdout),
        String::from_utf8_lossy(&wait.stderr)
    );

    assert!(
        wait_for_task_closed(repo, &task_a, Duration::from_secs(30)),
        "task A not closed"
    );
    assert!(
        wait_for_task_closed(repo, &task_b, Duration::from_secs(30)),
        "task B not closed"
    );

    // THE regression assertion: both absorptions survive on disk. Before the
    // fixes, the second absorb (or a stale snapshot after it) could silently
    // revert the first session's files.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let a = repo.join("file_a.txt").exists();
        let b = repo.join("file_b.txt").exists();
        if a && b {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "concurrent absorption lost work: file_a.txt present={a}, \
                 file_b.txt present={b}. recover output:\n{}",
                recover_list(repo)
            );
        }
        // Nudge recovery/absorb for any straggler session.
        let _ = crate::common::e2e_aiki(repo)
            .current_dir(repo)
            .args(["session", "list"])
            .output();
        std::thread::sleep(Duration::from_secs(2));
    }

    assert_eq!(
        std::fs::read_to_string(repo.join("file_a.txt")).unwrap().trim(),
        "from A"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("file_b.txt")).unwrap().trim(),
        "from B"
    );
}

#[test]
#[ignore] // e2e: requires claude binary + API key
fn e2e_claude_concurrent_sessions_both_absorb() {
    if !jj_available() || !agent_available("claude") {
        eprintln!("Skipping: jj/claude not available");
        return;
    }
    run_concurrent_sessions_both_survive(&[]);
}

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_concurrent_sessions_both_absorb() {
    if !jj_available() || !agent_available("codex") {
        eprintln!("Skipping: jj/codex not available");
        return;
    }
    run_concurrent_sessions_both_survive(&["--agent", "codex"]);
}

// =============================================================================
// 3. Stale-worker watchdog: a silent worker is killed, task stays restartable
// =============================================================================

fn run_stale_worker_reaped(agent_args: &[&str]) {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    init_aiki_repo(repo);

    let task_id = create_task(repo, "e2e stale: long multi-step work");
    set_task_instructions(
        repo,
        &task_id,
        "Run the shell command `sleep 5`. Repeat it 60 times, one command at a \
         time, as separate shell invocations. Then close this task.",
    );

    // Blocking run with the watchdog threshold shortened to 20s, spawned as
    // a child so the test can freeze the agent mid-run. The agent is
    // SIGSTOPped (deterministic — an instruction-driven hang depends on
    // agent compliance and gets refused by tool guardrails), after which the
    // transcript goes silent and the watchdog must kill it and stop the
    // task; without the guard this run would block until the test deadline.
    let aiki_home = crate::common::e2e_aiki_home(repo);
    let mut args: Vec<&str> = vec!["run", &task_id];
    args.extend_from_slice(agent_args);
    let mut child = process::Command::new(env!("CARGO_BIN_EXE_aiki"))
        .current_dir(repo)
        .args(&args)
        .env("AIKI_HOME", &aiki_home)
        .env("JJ_USER", "Aiki Test")
        .env("JJ_EMAIL", "test@example.com")
        .env("AIKI_STALE_WORKER_TIMEOUT_SECS", "20")
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .spawn()
        .expect("spawn blocking aiki run");

    // Discover the session (hermetic home → the only session file is ours).
    let sessions_dir = aiki_home.join("sessions");
    let deadline = Instant::now() + Duration::from_secs(90);
    let (session_id, agent_pid) = loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("agent session never registered");
        }
        let found = std::fs::read_dir(&sessions_dir)
            .ok()
            .and_then(|entries| {
                entries.flatten().find_map(|e| {
                    let path = e.path();
                    if !path.is_file() {
                        return None;
                    }
                    let sid = path.file_name()?.to_str()?.to_string();
                    let pid = std::fs::read_to_string(&path)
                        .ok()?
                        .lines()
                        .find_map(|l| l.trim().strip_prefix("parent_pid="))
                        .and_then(|v| v.parse::<u32>().ok())?;
                    Some((sid, pid))
                })
            });
        if let Some(pair) = found {
            break pair;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    eprintln!("stale test: session {session_id} pid {agent_pid}");

    // Wait until the transcript RESOLVES AND HAS CONTENT before freezing:
    // the watchdog deliberately skips a worker whose transcript is missing
    // or empty (never kill one that is still initializing), so freezing the
    // agent before its first transcript entry would make the guard skip
    // forever. `aiki session transcript -o path` uses the same resolution
    // path as the watchdog.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = process::Command::new("kill")
                .args(["-9", &agent_pid.to_string()])
                .status();
            panic!("transcript for {session_id} never became resolvable/non-empty");
        }
        let out = crate::common::e2e_aiki(repo)
            .current_dir(repo)
            .args(["session", "transcript", &session_id, "-o", "path"])
            .output()
            .expect("aiki session transcript");
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty()
                && std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false)
            {
                break;
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    // A beat more so the last entry's timestamp is set, then freeze.
    std::thread::sleep(Duration::from_secs(2));
    let stopped = process::Command::new("kill")
        .args(["-STOP", &agent_pid.to_string()])
        .status()
        .expect("run kill -STOP");
    assert!(stopped.success(), "SIGSTOP {agent_pid} failed");

    // The watchdog polls every 2s with a 20s idle threshold; give it ample
    // room, then declare failure.
    let deadline = Instant::now() + Duration::from_secs(180);
    let status = loop {
        match child.try_wait().expect("try_wait aiki run") {
            Some(status) => break status,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = process::Command::new("kill")
                    .args(["-9", &agent_pid.to_string()])
                    .status();
                panic!("watchdog never reaped the frozen worker (aiki run still blocked)");
            }
            None => std::thread::sleep(Duration::from_secs(1)),
        }
    };

    // Cleanup safety net (the watchdog should already have SIGKILLed it).
    let _ = process::Command::new("kill")
        .args(["-9", &agent_pid.to_string()])
        .status();

    let mut stderr = String::new();
    use std::io::Read;
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    eprintln!("stale run stderr: {stderr}");

    assert!(
        !status.success(),
        "aiki run should fail when the worker stalls (watchdog), got success"
    );
    assert!(
        stderr.contains("stalled") || stderr.contains("Worker stalled"),
        "stderr should report the stall: {stderr}"
    );

    // The task must be STOPPED (restartable), not closed and not stuck
    // in-progress.
    let show = crate::common::e2e_aiki(repo)
        .current_dir(repo)
        .args(["task", "show", &task_id])
        .output()
        .expect("task show");
    let show_out = String::from_utf8_lossy(&show.stdout).to_lowercase();
    assert!(
        show_out.contains("stopped"),
        "task should be stopped after watchdog reap, got:\n{show_out}"
    );
}

#[test]
#[ignore] // e2e: requires claude binary + API key
fn e2e_claude_stale_worker_reaped() {
    if !jj_available() || !agent_available("claude") {
        eprintln!("Skipping: jj/claude not available");
        return;
    }
    run_stale_worker_reaped(&[]);
}

#[test]
#[ignore] // e2e: requires codex binary + API key
fn e2e_codex_stale_worker_reaped() {
    if !jj_available() || !agent_available("codex") {
        eprintln!("Skipping: jj/codex not available");
        return;
    }
    run_stale_worker_reaped(&["--agent", "codex"]);
}
