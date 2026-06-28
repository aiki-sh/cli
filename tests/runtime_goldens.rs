//! Golden PATH-shim baselines for the agent runtimes (fix-refactor B5a).
//!
//! These tests capture the spawn behavior of the runtimes at the process
//! boundary — argv and environment as the child actually receives them —
//! driven through the consumer path (`get_runtime` → `spawn_*`). They are
//! committed while the OLD per-agent runtimes are routed and must pass
//! unchanged after the registry cutover to the generic `CliAgentRuntime`
//! (fix-refactor B6), proving the cutover preserves spawn behavior.
//!
//! argv[0] is normalized via `file_name()`: the old runtimes invoke the bare
//! binary name while the generic runtime invokes the which-resolved absolute
//! path; both must resolve to the same binary.
//!
//! Every test mutates process-wide env (PATH, and the claude tests inject
//! CLAUDECODE so the absence assertion is not vacuously green), so all tests
//! serialize on a local mutex and restore env on drop.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aiki::agents::runtime::get_runtime;
use aiki::agents::{AgentSpawnOptions, AgentType};
use aiki::tasks::lanes::ThreadId;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(vars: &[(&'static str, Option<&str>)]) -> Self {
        let mut saved = Vec::new();
        for (key, value) in vars {
            saved.push((*key, std::env::var(key).ok()));
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        EnvGuard { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// Install a fake agent binary that dumps its argv (one per line, $0 first)
/// and full environment into `dump_dir`, then exits 0.
fn install_dumping_agent(tmp: &Path, binary_name: &str) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let bin_dir = tmp.join("fake-bin");
    let dump_dir = tmp.join("dump");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&dump_dir).unwrap();
    let script = bin_dir.join(binary_name);
    // NUL-delimited argv: the task prompt is multi-line, so any text
    // delimiter would split it.
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$0\" \"$@\" > \"{dump}/argv.txt\"\nenv > \"{dump}/env.txt\"\n",
            dump = dump_dir.display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();
    (bin_dir, dump_dir)
}

fn read_argv(dump_dir: &Path) -> Vec<String> {
    let raw = std::fs::read_to_string(dump_dir.join("argv.txt"))
        .expect("fake agent should have dumped argv");
    raw.split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn read_env(dump_dir: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(dump_dir.join("env.txt"))
        .expect("fake agent should have dumped env")
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn env_value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn shim_path(bin_dir: &Path) -> String {
    format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn options(workdir: &Path, task: &str) -> AgentSpawnOptions {
    AgentSpawnOptions::new(workdir, ThreadId::single(task.to_string()))
}

// ─────────────────────────── claude goldens ───────────────────────────

#[test]
fn golden_claude_blocking_argv_and_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    let (bin_dir, dump_dir) = install_dumping_agent(tmp.path(), "claude");

    // Inject the nesting guards: the runtime guarantees their ABSENCE in the
    // child via env_remove, which is only observable when the parent has
    // them set. Without this the absence assertion is vacuously green.
    let path = shim_path(&bin_dir);
    let _env = EnvGuard::set(&[
        ("PATH", Some(path.as_str())),
        ("CLAUDECODE", Some("1")),
        ("CLAUDE_CODE_ENTRYPOINT", Some("test-entry")),
    ]);

    let runtime = get_runtime(AgentType::ClaudeCode).expect("claude runtime resolves");
    let opts = options(workdir.path(), "goldenclaudetask");
    let result = runtime.spawn_blocking(&opts).expect("spawn_blocking ok");
    assert!(
        matches!(result, aiki::agents::AgentSessionResult::Completed { .. }),
        "fake agent exits 0, expected Completed, got {result:?}"
    );

    let argv = read_argv(&dump_dir);
    assert_eq!(
        Path::new(&argv[0]).file_name().unwrap().to_str().unwrap(),
        "claude",
        "argv[0] must resolve to the claude binary (normalized by file_name)"
    );
    assert_eq!(argv[1], "--print");
    assert_eq!(argv[2], "--dangerously-skip-permissions");
    assert_eq!(argv[3], opts.task_prompt(), "prompt is the final argument");
    assert_eq!(argv.len(), 4, "no extra arguments: {argv:?}");

    let env = read_env(&dump_dir);
    assert_eq!(env_value(&env, "AIKI_THREAD"), Some("goldenclaudetask"));
    assert_eq!(env_value(&env, "AIKI_SESSION_MODE"), Some("background"));
    assert_eq!(
        env_value(&env, "CLAUDECODE"),
        None,
        "nesting guard must be removed from the child env"
    );
    assert_eq!(
        env_value(&env, "CLAUDE_CODE_ENTRYPOINT"),
        None,
        "nesting guard must be removed from the child env"
    );
}

#[test]
fn golden_claude_monitored_mode_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    let (bin_dir, dump_dir) = install_dumping_agent(tmp.path(), "claude");

    let path = shim_path(&bin_dir);
    let _env = EnvGuard::set(&[("PATH", Some(path.as_str()))]);

    let runtime = get_runtime(AgentType::ClaudeCode).expect("claude runtime resolves");
    let opts = options(workdir.path(), "goldenmonitoredtask");
    let mut child = runtime.spawn_monitored(&opts).expect("spawn_monitored ok");
    let status = child.wait().expect("fake agent exits");
    assert!(status.success());

    let env = read_env(&dump_dir);
    assert_eq!(env_value(&env, "AIKI_THREAD"), Some("goldenmonitoredtask"));
    assert_eq!(
        env_value(&env, "AIKI_SESSION_MODE"),
        Some("monitored"),
        "monitored spawns carry mode=monitored"
    );
}

// ─────────────────────────── codex goldens ────────────────────────────

#[test]
fn golden_codex_blocking_no_git_ancestor() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    // A bare temp dir: no `.git` anywhere up the ancestor chain.
    let workdir = tempfile::tempdir().unwrap();
    let (bin_dir, dump_dir) = install_dumping_agent(tmp.path(), "codex");

    let path = shim_path(&bin_dir);
    let _env = EnvGuard::set(&[("PATH", Some(path.as_str()))]);

    let runtime = get_runtime(AgentType::Codex).expect("codex runtime resolves");
    let opts = options(workdir.path(), "goldencodextask");
    let result = runtime.spawn_blocking(&opts).expect("spawn_blocking ok");
    assert!(
        matches!(result, aiki::agents::AgentSessionResult::Completed { .. }),
        "fake agent exits 0, expected Completed, got {result:?}"
    );

    let argv = read_argv(&dump_dir);
    assert_eq!(
        Path::new(&argv[0]).file_name().unwrap().to_str().unwrap(),
        "codex"
    );
    assert_eq!(argv[1], "exec");
    assert_eq!(argv[2], "--dangerously-bypass-approvals-and-sandbox");
    assert_eq!(argv[3], "--dangerously-bypass-hook-trust");
    assert_eq!(argv[4], opts.task_prompt(), "prompt precedes the jj flags");
    assert_eq!(
        argv[5], "--skip-git-repo-check",
        "no .git ancestor: the skip flag is passed, after the prompt"
    );
    assert_eq!(argv.len(), 6, "no extra arguments: {argv:?}");

    let env = read_env(&dump_dir);
    assert_eq!(env_value(&env, "AIKI_THREAD"), Some("goldencodextask"));
    assert_eq!(env_value(&env, "AIKI_SESSION_MODE"), Some("background"));
}

#[test]
fn golden_codex_blocking_colocated_git_repo() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    // Colocated jj+git repo: a `.git` exists, so no --skip-git-repo-check;
    // the jj store lives inside the workdir, so no --add-dir either.
    let init = std::process::Command::new("jj")
        .args(["git", "init", "--colocate"])
        .current_dir(workdir.path())
        .output()
        .expect("jj available");
    assert!(init.status.success(), "jj git init --colocate failed");

    let (bin_dir, dump_dir) = install_dumping_agent(tmp.path(), "codex");
    let path = shim_path(&bin_dir);
    let _env = EnvGuard::set(&[("PATH", Some(path.as_str()))]);

    let runtime = get_runtime(AgentType::Codex).expect("codex runtime resolves");
    let opts = options(workdir.path(), "goldencolocatedtask");
    let result = runtime.spawn_blocking(&opts).expect("spawn_blocking ok");
    assert!(matches!(
        result,
        aiki::agents::AgentSessionResult::Completed { .. }
    ));

    let argv = read_argv(&dump_dir);
    assert_eq!(argv[1], "exec");
    assert_eq!(argv[2], "--dangerously-bypass-approvals-and-sandbox");
    assert_eq!(argv[3], "--dangerously-bypass-hook-trust");
    assert_eq!(argv[4], opts.task_prompt());
    assert_eq!(
        argv.len(),
        5,
        "with .git present and an in-tree jj store there are no jj flags: {argv:?}"
    );
}

#[test]
fn golden_codex_blocking_jj_workspace_external_store() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    // A jj workspace whose repo store lives in the main repo: `.jj/repo` in
    // the workspace is a plain-text pointer outside the workspace tree, so
    // codex must get --add-dir (and --skip-git-repo-check: workspaces have
    // no .git).
    let root = tempfile::tempdir().unwrap();
    let main_repo = root.path().join("main");
    let workspace = root.path().join("ws");
    std::fs::create_dir_all(&main_repo).unwrap();
    let init = std::process::Command::new("jj")
        .args(["git", "init"])
        .current_dir(&main_repo)
        .output()
        .expect("jj available");
    assert!(init.status.success(), "jj git init failed");
    let ws_add = std::process::Command::new("jj")
        .args(["workspace", "add", workspace.to_str().unwrap()])
        .current_dir(&main_repo)
        .output()
        .expect("jj workspace add runs");
    assert!(
        ws_add.status.success(),
        "jj workspace add failed: {}",
        String::from_utf8_lossy(&ws_add.stderr)
    );

    let (bin_dir, dump_dir) = install_dumping_agent(tmp.path(), "codex");
    let path = shim_path(&bin_dir);
    let _env = EnvGuard::set(&[("PATH", Some(path.as_str()))]);

    let runtime = get_runtime(AgentType::Codex).expect("codex runtime resolves");
    let opts = options(&workspace, "goldenworkspacetask");
    let result = runtime.spawn_blocking(&opts).expect("spawn_blocking ok");
    assert!(matches!(
        result,
        aiki::agents::AgentSessionResult::Completed { .. }
    ));

    let argv = read_argv(&dump_dir);
    assert_eq!(argv[1], "exec");
    assert_eq!(argv[2], "--dangerously-bypass-approvals-and-sandbox");
    assert_eq!(argv[3], "--dangerously-bypass-hook-trust");
    assert_eq!(argv[4], opts.task_prompt());
    assert_eq!(argv[5], "--skip-git-repo-check");
    assert_eq!(argv[6], "--add-dir");
    let added = PathBuf::from(&argv[7]);
    assert!(
        added.ends_with(".jj"),
        "--add-dir must point at the shared store's .jj dir, got {added:?}"
    );
    assert!(
        !added.starts_with(&workspace),
        "--add-dir target lives outside the workspace"
    );
    assert_eq!(argv.len(), 8, "no extra arguments: {argv:?}");
}
