use crate::agents::AgentSpawnOptions;
use crate::harnesses::runtime::CliArgs;
use std::path::{Path, PathBuf};

/// Check if the working directory is inside a git repository.
/// Walks up from `dir` looking for a `.git` directory or file.
fn has_git_repo(dir: &Path) -> bool {
    let mut current = Some(dir);
    while let Some(d) = current {
        if d.join(".git").exists() {
            return true;
        }
        current = d.parent();
    }
    false
}

/// If `dir` is a JJ workspace whose repo store lives elsewhere (e.g. a shared
/// store in the original repo), return the parent `.jj` directory that needs
/// to be writable. Codex's sandbox must be told about it via `--add-dir`.
fn jj_shared_store_dir(dir: &Path) -> Option<PathBuf> {
    let repo_file = dir.join(".jj/repo");
    let contents = std::fs::read_to_string(&repo_file).ok()?;
    let store_path = PathBuf::from(contents.trim());
    if store_path.starts_with(dir) {
        return None;
    }
    store_path.parent().map(|p| p.to_path_buf())
}

pub(super) fn args(opts: &AgentSpawnOptions) -> CliArgs {
    let mut args = CliArgs::new();
    args.push("exec");
    // Bypass sandbox: nested codex inherits parent's seatbelt which blocks API access.
    // TODO: replace with --profile once permission profiles are configured (see ops/now/fix-codex-run.md)
    args.push("--dangerously-bypass-approvals-and-sandbox");
    args.push(opts.task_prompt());
    // JJ flags follow the prompt, matching the runtime this replaces:
    // --skip-git-repo-check only when no `.git` ancestor exists, and
    // --add-dir when the JJ store lives outside the workspace.
    if !has_git_repo(&opts.cwd) {
        args.push("--skip-git-repo-check");
    }
    if let Some(store) = jj_shared_store_dir(&opts.cwd) {
        args.push("--add-dir");
        args.push(store);
    }
    args
}
