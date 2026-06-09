use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

/// Spawn `aiki <args>` as a detached background process, piping `stdin_payload` to its stdin.
///
/// Uses `Stdio::piped()` for stdin so the caller can pass a payload (e.g. JSON)
/// to the child process. The stdin handle is dropped after writing to send EOF.
/// The child is not waited on.
pub fn spawn_with_stdin(cwd: &Path, args: &[&str], stdin_payload: &[u8]) -> Result<()> {
    let binary = crate::config::get_aiki_binary_path();

    let mut child = Command::new(&binary)
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn background aiki process: {binary}"))?;

    // Write payload to child's stdin, then drop to send EOF
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("child stdin handle is None"))?;
    use std::io::Write;
    if let Err(e) = stdin.write_all(stdin_payload) {
        // Drop stdin before killing so the child isn't blocked on a full pipe.
        drop(stdin);
        // Best-effort cleanup: if kill itself fails the orphan is a known limitation
        // (e.g. the child already exited or the OS refused the signal).
        let _ = child.kill();
        return Err(e).context("failed to write payload to child stdin");
    }
    drop(stdin);

    Ok(())
}

/// Spawn a `--_continue-async` background process, passing opts as JSON via stdin.
pub fn spawn_continue<T: Serialize>(cwd: &Path, args: &[&str], opts: &T) -> Result<()> {
    let payload = serde_json::to_string(opts)?;
    spawn_with_stdin(cwd, args, payload.as_bytes())
}

/// Read opts from stdin JSON (used by `--_continue-async` resume paths).
pub fn read_continue_opts<T: DeserializeOwned>() -> Result<T> {
    let opts = serde_json::from_reader(io::stdin().lock())
        .context("failed to read async continue options from stdin")?;
    Ok(opts)
}
