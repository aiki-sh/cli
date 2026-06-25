//! Init-state machine: the single source of truth for whether aiki is active
//! in a given directory.
//!
//! Two predicates compose into four states:
//!
//! | `.aiki/` | marker | state            |
//! |----------|--------|------------------|
//! | no       | no     | `NotAikiRepo`    |
//! | yes      | no     | `Dormant`        |
//! | yes      | yes    | `Active`         |
//! | no       | yes    | `OrphanedMarker` |
//!
//! - `.aiki/` is a checked-in, per-repo artifact created by `aiki init`.
//! - The **per-user enable marker** lives under the aiki global home at
//!   `~/.aiki/.init/repos<repo-root>/enabled`. It is per-user (not checked in)
//!   so that teammates who clone a repo containing `.aiki/` are not silently
//!   enrolled — each user opts in by running `aiki init`.
//!
//! This module is consumed by the hook gate (`run_stdin`), the CLI gate,
//! `aiki doctor`, the lazy marker reaper, and `aiki remove`. Match
//! exhaustiveness on [`InitState`] is the contract: adding a fifth state
//! forces every consumer to update.
//!
//! ## No canonicalization (deliberate)
//!
//! The POSIX shell gate keys the marker by the raw `$PWD`-walked path and
//! `sh` has no portable `realpath`, so this module also uses raw (un-resolved)
//! paths. If Rust canonicalized but bash could not, the two layers would key
//! the marker differently and tracking would silently die mid-session under a
//! symlinked cwd. The residual cost is that a repo reached through two
//! different paths (a symlinked checkout, or `/tmp` vs `/private/tmp`) keys two
//! markers — a documented Phase-1 limitation.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Whether — and how — aiki is active in a directory.
///
/// See the [module docs](self) for the predicate table that produces each
/// variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitState {
    /// `.aiki/` present and this user has enabled the repo. Aiki runs.
    Active { root: PathBuf },
    /// `.aiki/` present but this user has not opted in (cloned-but-not-enabled).
    Dormant { root: PathBuf },
    /// A per-user marker exists but the repo's `.aiki/` is gone (e.g. a
    /// teammate ran `aiki remove --shared` and pushed). Safe to reap.
    OrphanedMarker { root: PathBuf },
    /// Not an aiki repo: no `.aiki/` and no marker anywhere up the tree.
    NotAikiRepo,
}

/// Build the per-user enable-marker path for a repo root.
///
/// Layout mirrors the repo's absolute path under `<global-home>/.init/repos/`
/// and appends a literal `enabled`. Pure path concatenation — no encoding, no
/// canonicalization — so it matches the bash gate's
/// `${AIKI_HOME:-$HOME/.aiki}/.init/repos$repo_root/enabled` byte-for-byte.
///
/// | Repo root              | Marker file                                       |
/// |------------------------|---------------------------------------------------|
/// | `/Users/me/code/aiki`  | `~/.aiki/.init/repos/Users/me/code/aiki/enabled`  |
#[must_use]
pub fn marker_path(repo_root: &Path) -> PathBuf {
    // Strip the leading `/` so `Path::join` appends instead of resetting to an
    // absolute path. Use the aiki global home (honors `$AIKI_HOME`) so this
    // matches the bash gate's `${AIKI_HOME:-$HOME/.aiki}` exactly — do NOT
    // hardcode `dirs::home_dir()`.
    let stripped = repo_root.strip_prefix("/").unwrap_or(repo_root);
    crate::global::global_aiki_dir()
        .join(".init/repos")
        .join(stripped)
        .join("enabled")
}

/// Walk up from `cwd` looking for a repo `.aiki/` directory.
///
/// Returns `Ok(Some(root))` for the nearest ancestor that contains a `.aiki/`
/// directory, or `Ok(None)` if none is found before the filesystem root.
///
/// **Skips aiki's own global home.** The global home (`global::global_aiki_dir()`,
/// default `~/.aiki`) is a directory literally named `.aiki` whose file set
/// mirrors a repo's `.aiki/`. Without this exclusion, every non-aiki directory
/// under `$HOME` would resolve to `$HOME` as a phantom root. The candidate is
/// rejected with **plain path equality** (not canonicalized) so it matches the
/// bash gate's `[ "$d/.aiki" != "$h" ]` string comparison exactly.
///
/// This anchors on `.aiki/` only (never on the marker), so callers that need a
/// real repo root — like [`crate::repos::RepoDetector::find_aiki_root`] — can
/// rely on the returned path containing a `.aiki/` directory.
pub fn find_aiki_root(cwd: &Path) -> Result<Option<PathBuf>> {
    let global_home = crate::global::global_aiki_dir();
    let mut current = cwd.to_path_buf();
    loop {
        let candidate = current.join(".aiki");
        if candidate.is_dir() && candidate != global_home {
            return Ok(Some(current));
        }
        if !current.pop() {
            // Reached the filesystem root without finding a repo `.aiki/`.
            return Ok(None);
        }
    }
}

/// Walk up from `cwd` looking for a directory whose per-user marker exists.
///
/// Used only to detect an [`InitState::OrphanedMarker`] when no `.aiki/` was
/// found anywhere up the tree. Returns the nearest ancestor with a marker.
fn find_marker_root(cwd: &Path) -> Option<PathBuf> {
    let mut current = cwd.to_path_buf();
    loop {
        if marker_path(&current).is_file() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Classify a directory into its [`InitState`].
///
/// Single source of truth for the hook gate, CLI gate, doctor, the lazy
/// reaper, and `aiki remove`.
///
/// `.aiki/` takes precedence over a stray marker: a repo `.aiki/` found
/// anywhere up the tree resolves to `Active`/`Dormant` (matching the bash
/// gate's `.aiki/`-only walk). A marker only anchors a root when no `.aiki/`
/// exists at all — the genuine orphaned case — yielding `OrphanedMarker` so the
/// CLI gate and reaper can clean it up.
pub fn init_state(cwd: &Path) -> Result<InitState> {
    if let Some(root) = find_aiki_root(cwd)? {
        // `find_aiki_root` guarantees `root/.aiki` is a directory.
        return Ok(if marker_enabled_for(&root) {
            InitState::Active { root }
        } else {
            InitState::Dormant { root }
        });
    }
    // No `.aiki/` anywhere up the tree. A surviving marker means the repo's
    // `.aiki/` was removed out from under it.
    if let Some(root) = find_marker_root(cwd) {
        return Ok(InitState::OrphanedMarker { root });
    }
    Ok(InitState::NotAikiRepo)
}

/// Whether this user has enabled `root` — directly, or (when `root` is an
/// isolated JJ workspace) via the source repo's marker.
///
/// Aiki's per-session workspaces under
/// [`crate::session::isolation::workspaces_dir`] check out the repo's committed
/// `.aiki/` directory, so [`find_aiki_root`] anchors on the throwaway workspace
/// path. But `aiki init` writes the enable marker keyed to the *source* repo the
/// user ran it in, never the workspace. Without this fallback, every `aiki`
/// command invoked from inside an isolated workspace resolves to a
/// false-negative [`InitState::Dormant`] ("not enabled for this account") even
/// though the source repo is enabled.
///
/// [`find_repo_root_from_workspace`] returns `Some` only for a secondary JJ
/// workspace — its `.jj/repo` points at another repo's store. A primary repo's
/// `.jj/repo` is the store directory itself, so the call returns `None` and we
/// never re-key against a phantom source. Symlinked source paths remain a
/// Phase-1 limitation (see module docs): the resolver canonicalizes relative
/// `.jj/repo` targets, which can disagree with the un-canonicalized marker key.
///
/// [`find_repo_root_from_workspace`]: crate::session::isolation::find_repo_root_from_workspace
fn marker_enabled_for(root: &Path) -> bool {
    if marker_path(root).is_file() {
        return true;
    }
    crate::session::isolation::find_repo_root_from_workspace(root)
        .map(|source| marker_path(&source).is_file())
        .unwrap_or(false)
}

/// Walk the per-user marker registry (`<global-home>/.init/repos/`) and remove
/// markers whose repo `.aiki/` no longer exists — a teammate ran
/// `aiki remove --shared` and pushed the removal, or the repo was deleted.
/// Returns how many markers were reaped.
///
/// Best-effort: per-entry I/O errors are skipped. Called by `aiki init` (lazy)
/// and `aiki doctor`. The CLI gate also reaps the single marker it lands on, but
/// only this sweep cleans the whole registry. The walk only visits paths the
/// user has enabled, so the overhead is negligible.
/// Collect every `enabled` marker file under the registry root.
fn collect_marker_files(registry: &Path) -> Vec<PathBuf> {
    let mut markers = Vec::new();
    let mut stack = vec![registry.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name() == Some(std::ffi::OsStr::new("enabled")) {
                markers.push(path);
            }
        }
    }
    markers
}

/// Reconstruct the repo root of every per-user marker in the registry (each
/// `<registry>/<repo-path>/enabled` maps back to `/<repo-path>`). Used by
/// `aiki remove --shared --global` to tear down every enabled repo.
pub fn enabled_repo_roots() -> Vec<PathBuf> {
    let registry = crate::global::global_aiki_dir().join(".init/repos");
    collect_marker_files(&registry)
        .into_iter()
        .filter_map(|marker| {
            let rel = marker.parent()?.strip_prefix(&registry).ok()?;
            Some(Path::new("/").join(rel))
        })
        .collect()
}

pub fn reap_stale_markers() -> usize {
    let registry = crate::global::global_aiki_dir().join(".init/repos");
    if !registry.is_dir() {
        return 0;
    }

    let markers = collect_marker_files(&registry);
    let mut reaped = 0;
    for marker in markers {
        // Reconstruct the repo root: <registry>/<repo-path>/enabled → /<repo-path>.
        let Some(rel) = marker
            .parent()
            .and_then(|p| p.strip_prefix(&registry).ok())
        else {
            continue;
        };
        let repo_root = Path::new("/").join(rel);
        if repo_root.join(".aiki").is_dir() {
            continue; // Still a live aiki repo — keep the marker.
        }
        if std::fs::remove_file(&marker).is_ok() {
            reaped += 1;
            prune_empty_registry_dirs(marker.parent(), &registry);
        }
    }
    reaped
}

/// `rmdir` now-empty marker directories up to — but not including — the registry
/// root, so reaping one repo's marker doesn't disturb another's.
fn prune_empty_registry_dirs(start: Option<&Path>, registry: &Path) {
    let mut dir = start.map(Path::to_path_buf);
    while let Some(d) = dir {
        if d == *registry || !d.starts_with(registry) {
            break;
        }
        if std::fs::remove_dir(&d).is_err() {
            break; // Non-empty (another enabled repo shares it) — stop.
        }
        dir = d.parent().map(Path::to_path_buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Run `f` with `AIKI_HOME` pointed at `home`, serialized against other
    /// tests that touch the env var.
    fn with_aiki_home<F: FnOnce() -> R, R>(home: &Path, f: F) -> R {
        let _lock = crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var(crate::global::AIKI_HOME_ENV).ok();
        std::env::set_var(crate::global::AIKI_HOME_ENV, home);
        let result = f();
        match original {
            Some(v) => std::env::set_var(crate::global::AIKI_HOME_ENV, v),
            None => std::env::remove_var(crate::global::AIKI_HOME_ENV),
        }
        result
    }

    /// Create the per-user marker for `repo_root` under the active global home.
    fn write_marker(repo_root: &Path) {
        let path = marker_path(repo_root);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "").unwrap();
    }

    #[test]
    fn marker_path_exact_layout() {
        let home = PathBuf::from("/tmp/fake-home/.aiki");
        with_aiki_home(&home, || {
            assert_eq!(
                marker_path(Path::new("/Users/me/code/repo")),
                PathBuf::from("/tmp/fake-home/.aiki/.init/repos/Users/me/code/repo/enabled"),
            );
        });
    }

    #[test]
    fn marker_path_handles_spaces_percent_unicode() {
        let home = PathBuf::from("/tmp/fake-home/.aiki");
        with_aiki_home(&home, || {
            assert_eq!(
                marker_path(Path::new("/Users/me/My Stuff/100%/café")),
                PathBuf::from(
                    "/tmp/fake-home/.aiki/.init/repos/Users/me/My Stuff/100%/café/enabled"
                ),
            );
        });
    }

    /// The Rust `marker_path` and the bash gate's `$h/.init/repos$d/enabled`
    /// expansion must render the same string, since neither side canonicalizes.
    #[test]
    fn marker_path_matches_bash_expansion() {
        let home = PathBuf::from("/tmp/fake-home/.aiki");
        with_aiki_home(&home, || {
            for repo in [
                "/Users/me/code/repo",
                "/Users/me/My Stuff/repo",
                "/opt/srv/app",
            ] {
                let rust = marker_path(Path::new(repo));
                let bash = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(r#"printf '%s' "$h/.init/repos$d/enabled""#)
                    .env("h", &home)
                    .env("d", repo)
                    .output()
                    .unwrap();
                let bash_str = String::from_utf8(bash.stdout).unwrap();
                assert_eq!(rust.to_str().unwrap(), bash_str, "mismatch for {repo}");
            }
        });
    }

    #[test]
    fn active_when_aiki_dir_and_marker_present() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir(repo.path().join(".aiki")).unwrap();
        with_aiki_home(home.path(), || {
            write_marker(repo.path());
            assert_eq!(
                init_state(repo.path()).unwrap(),
                InitState::Active {
                    root: repo.path().to_path_buf()
                },
            );
        });
    }

    #[test]
    fn dormant_when_aiki_dir_but_no_marker() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir(repo.path().join(".aiki")).unwrap();
        with_aiki_home(home.path(), || {
            assert_eq!(
                init_state(repo.path()).unwrap(),
                InitState::Dormant {
                    root: repo.path().to_path_buf()
                },
            );
        });
    }

    #[test]
    fn orphaned_marker_when_marker_but_no_aiki_dir() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        // Note: no `.aiki/` created — only the marker survives.
        with_aiki_home(home.path(), || {
            write_marker(repo.path());
            assert_eq!(
                init_state(repo.path()).unwrap(),
                InitState::OrphanedMarker {
                    root: repo.path().to_path_buf()
                },
            );
        });
    }

    #[test]
    fn not_aiki_repo_when_neither_present() {
        let home = tempfile::tempdir().unwrap();
        let plain = tempfile::tempdir().unwrap();
        with_aiki_home(home.path(), || {
            assert_eq!(init_state(plain.path()).unwrap(), InitState::NotAikiRepo);
        });
    }

    #[test]
    fn walks_up_to_find_aiki_root_from_subdir() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir(repo.path().join(".aiki")).unwrap();
        let subdir = repo.path().join("src").join("deep");
        fs::create_dir_all(&subdir).unwrap();
        with_aiki_home(home.path(), || {
            write_marker(repo.path());
            assert_eq!(
                init_state(&subdir).unwrap(),
                InitState::Active {
                    root: repo.path().to_path_buf()
                },
            );
        });
    }

    /// Build an isolated-workspace-shaped directory pointing back at `source`.
    ///
    /// Mirrors what `aiki`'s session isolation lays down: a `.aiki/` checked out
    /// from the source repo plus a `.jj/repo` that locates the source repo's
    /// store. We write an **absolute** target so
    /// [`super::find_repo_root_from_workspace`] uses it verbatim (no
    /// canonicalization), keeping the resolved source byte-for-byte equal to the
    /// un-canonicalized `source` the test keyed its marker against.
    fn make_isolated_workspace(source: &Path) -> tempfile::TempDir {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join(".aiki")).unwrap();
        let jj_dir = workspace.path().join(".jj");
        fs::create_dir(&jj_dir).unwrap();
        let target = source.join(".jj").join("repo");
        fs::write(jj_dir.join("repo"), target.to_str().unwrap()).unwrap();
        workspace
    }

    /// Regression: running `aiki` from inside an isolated workspace must resolve
    /// to `Active` when the *source* repo is enabled. The workspace checks out
    /// the repo's committed `.aiki/`, so `find_aiki_root` anchors on the
    /// workspace path — but the enable marker is keyed to the source repo. Before
    /// the source-repo fallback this was a false-negative `Dormant`, surfaced to
    /// agents as "not enabled for this account".
    #[test]
    fn isolated_workspace_resolves_active_via_source_marker() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join(".aiki")).unwrap();
        fs::create_dir(source.path().join(".jj")).unwrap();
        let workspace = make_isolated_workspace(source.path());
        with_aiki_home(home.path(), || {
            // Marker exists only for the source repo, never the workspace path.
            write_marker(source.path());
            assert!(!marker_path(workspace.path()).is_file());
            assert_eq!(
                init_state(workspace.path()).unwrap(),
                InitState::Active {
                    root: workspace.path().to_path_buf()
                },
            );
        });
    }

    /// The source-repo fallback must not over-enable: an isolated workspace whose
    /// source repo was never enabled stays `Dormant`.
    #[test]
    fn isolated_workspace_stays_dormant_when_source_unenabled() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join(".aiki")).unwrap();
        fs::create_dir(source.path().join(".jj")).unwrap();
        let workspace = make_isolated_workspace(source.path());
        with_aiki_home(home.path(), || {
            // No marker for either the source repo or the workspace.
            assert_eq!(
                init_state(workspace.path()).unwrap(),
                InitState::Dormant {
                    root: workspace.path().to_path_buf()
                },
            );
        });
    }

    /// Regression: a non-aiki directory under a home that contains the global
    /// `~/.aiki/` must resolve to `NotAikiRepo`, not a phantom `Dormant` rooted
    /// at the home directory. This is the global-home collision guard.
    #[test]
    fn global_home_collision_resolves_to_not_aiki_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("home");
        let global_home = fake_home.join(".aiki");
        // The global home is a directory named `.aiki` with mirroring files.
        fs::create_dir_all(global_home.join(".init/repos")).unwrap();
        fs::create_dir_all(global_home.join("tasks")).unwrap();
        // A non-aiki project nested under the fake home.
        let project = fake_home.join("projects").join("foo");
        fs::create_dir_all(&project).unwrap();
        with_aiki_home(&global_home, || {
            // Without the global-home exclusion, the walk would stop at
            // `fake_home` (whose `.aiki` *is* the global home) and report it as
            // a Dormant root.
            assert_eq!(find_aiki_root(&project).unwrap(), None);
            assert_eq!(init_state(&project).unwrap(), InitState::NotAikiRepo);
        });
    }

    /// A path and an equivalent symlinked path key different markers. This
    /// documents the Phase-1 limitation (no canonicalization) rather than
    /// asserting the two unify.
    #[test]
    fn reaper_removes_stale_markers_and_keeps_live_ones() {
        let home = tempfile::tempdir().unwrap();
        // A live repo (has `.aiki/`) and a stale one (repo dir deleted).
        let live = tempfile::tempdir().unwrap();
        fs::create_dir(live.path().join(".aiki")).unwrap();
        let stale = tempfile::tempdir().unwrap();
        let stale_root = stale.path().to_path_buf();
        with_aiki_home(home.path(), || {
            write_marker(live.path());
            write_marker(&stale_root);
            // Delete the stale repo entirely, leaving an orphaned marker.
            drop(stale);

            let reaped = reap_stale_markers();

            assert_eq!(reaped, 1, "only the stale marker is reaped");
            assert!(marker_path(live.path()).is_file(), "live marker kept");
            assert!(!marker_path(&stale_root).is_file(), "stale marker gone");
            // The shared registry root must survive.
            assert!(home.path().join(".init/repos").is_dir());
        });
    }

    #[test]
    fn symlink_alias_keys_distinct_markers() {
        let home = PathBuf::from("/tmp/fake-home/.aiki");
        with_aiki_home(&home, || {
            let real = marker_path(Path::new("/private/tmp/work/repo"));
            let alias = marker_path(Path::new("/tmp/work/repo"));
            assert_ne!(real, alias);
        });
    }
}
