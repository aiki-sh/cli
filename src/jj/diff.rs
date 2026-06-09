use super::jj_cmd;
use super::workspace::JJWorkspace;
use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use std::path::{Path, PathBuf};

/// Get list of deleted file paths from JJ, filtered by command arguments
///
/// Parses `jj diff -r @ --summary` and extracts all D (deleted) lines,
/// then filters to only include paths that match the command arguments.
/// This prevents re-emitting earlier deletes from the same working copy change.
///
/// Returns paths relative to shell_cwd, not workspace root, so downstream
/// consumers can join them onto event.cwd without path confusion.
///
/// # Arguments
/// * `shell_cwd` - Directory where the shell command was executed
/// * `command_args` - Raw command arguments (relative to shell_cwd)
pub fn get_deleted_paths(shell_cwd: &Path, command_args: &[String]) -> Result<Vec<String>> {
    // Find workspace root and normalize command args to be workspace-relative
    let workspace = JJWorkspace::find(shell_cwd)?;
    let workspace_root = workspace.workspace_root();
    let normalized_args =
        normalize_paths_to_workspace_root(command_args, shell_cwd, workspace_root)?;

    let output = run_jj_diff_summary(workspace_root)?;

    let all_deleted: Vec<String> = output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with("D ") {
                Some(line[2..].trim().to_string())
            } else {
                None
            }
        })
        .collect();

    // Filter to only include paths mentioned in command or their descendants
    // Both all_deleted (from JJ) and normalized_args are now workspace-relative
    let filtered = filter_paths_by_command(&all_deleted, &normalized_args);

    // Rebase filtered paths to be relative to shell_cwd, not workspace root
    rebase_paths_to_cwd(&filtered, shell_cwd, workspace_root)
}

/// Get move operations (source -> destination pairs) from JJ, filtered by destination
///
/// Parses `jj diff -r @ --summary` and extracts all R (rename/move) lines,
/// then filters to only include moves whose destination matches the command's
/// destination argument. This handles chained renames correctly because JJ
/// tracks from the baseline, not incrementally between commands.
///
/// Returns (source, destination) tuples with paths relative to shell_cwd, not
/// workspace root, so downstream consumers can join them onto event.cwd.
///
/// # Arguments
/// * `shell_cwd` - Directory where the shell command was executed
/// * `command_args` - Raw command arguments (relative to shell_cwd)
pub fn get_move_operations(
    shell_cwd: &Path,
    command_args: &[String],
) -> Result<Vec<(String, String)>> {
    // Find workspace root and normalize command args to be workspace-relative
    let workspace = JJWorkspace::find(shell_cwd)?;
    let workspace_root = workspace.workspace_root();

    // For mv commands, the last argument is the destination.
    // We filter by DESTINATION, not source, because JJ tracks renames from the
    // baseline (last commit), not incrementally between commands.
    //
    // Example of why source-based filtering fails:
    //   mv foo bar          # JJ shows: R {foo => bar}
    //   mv bar existing_dir # JJ shows: R {foo => existing_dir/bar}
    //                       # Source is "foo", but command arg is "bar" - no match!
    //
    // Destination-based filtering works because the destination always reflects
    // the current command's target:
    //   mv bar existing_dir # Dest arg: "existing_dir", JJ dest: "existing_dir/bar"
    //                       # "existing_dir/bar" starts with "existing_dir" - match!
    let dest_arg: String = command_args.last().cloned().unwrap_or_default();
    let normalized_dest =
        normalize_paths_to_workspace_root(&[dest_arg], shell_cwd, workspace_root)?
            .into_iter()
            .next()
            .unwrap_or_default();

    // If destination is outside workspace, normalized_dest will be empty.
    // Return empty vec so caller falls back to syntactic detection.
    // (Otherwise path.starts_with("") matches everything, emitting all JJ moves)
    if normalized_dest.is_empty() {
        return Ok(vec![]);
    }

    let output = run_jj_diff_summary(workspace_root)?;

    let all_moves: Vec<(String, String)> = output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if let Some(content) = line.strip_prefix("R ") {
                parse_move_line(content)
            } else {
                None
            }
        })
        .collect();

    // Filter by DESTINATION: match if JJ's dest equals or is under the command's dest dir
    let filtered_moves: Vec<(String, String)> = all_moves
        .into_iter()
        .filter(|(_source, dest)| {
            let dest_path = std::path::Path::new(dest);
            let arg_path =
                std::path::Path::new(normalized_dest.trim_end_matches('/').trim_end_matches('\\'));
            dest_path == arg_path || dest_path.starts_with(arg_path)
        })
        .collect();

    // Rebase both source and destination to be relative to shell_cwd
    let sources: Vec<String> = filtered_moves.iter().map(|(s, _)| s.clone()).collect();
    let destinations: Vec<String> = filtered_moves.iter().map(|(_, d)| d.clone()).collect();

    let rebased_sources = rebase_paths_to_cwd(&sources, shell_cwd, workspace_root)?;
    let rebased_destinations = rebase_paths_to_cwd(&destinations, shell_cwd, workspace_root)?;

    Ok(rebased_sources
        .into_iter()
        .zip(rebased_destinations)
        .collect())
}

/// Parse a JJ move/rename line
///
/// JJ outputs rename lines as: "R {old_path => new_path}"
/// The braces contain the old and new paths separated by " => ".
/// Paths with spaces are NOT quoted - they appear as-is within the braces.
///
/// Examples:
///   - R {old.txt => new.txt}
///   - R {src/old.rs => src/new.rs}
///   - R {file with spaces.txt => renamed.txt}
///   - R {file.txt => dir/file.txt}
fn parse_move_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();

    // Extract content between braces: "R {old => new}" -> "old => new"
    let braces_content = line.strip_prefix('{')?.strip_suffix('}')?;

    // Split on " => " separator
    let mut parts = braces_content.split(" => ");
    let source = parts.next()?.trim().to_string();
    let destination = parts.next()?.trim().to_string();

    Some((source, destination))
}

/// Run `jj diff -r @ --summary` and return stdout
///
/// Runs JJ from the workspace root directory.
/// Callers should use `JJWorkspace::find()` to get the workspace_root.
///
/// # Arguments
/// * `workspace_root` - JJ workspace root directory (contains .jj/)
///
/// # Errors
/// Returns error if JJ command fails (not installed, not a workspace, etc.)
/// Callers should handle errors and fall back to syntactic detection.
fn run_jj_diff_summary(workspace_root: &Path) -> Result<String> {
    let output = jj_cmd()
        .arg("diff")
        .arg("-r")
        .arg("@")
        .arg("--summary")
        .current_dir(workspace_root)
        .output()
        .context("Failed to execute jj command (is jj installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("jj diff command failed: {}", stderr);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Check if a string contains unambiguous glob metacharacters.
///
/// This is conservative: only `*` and `?` trigger glob mode because they are
/// unambiguously glob metacharacters. Characters like `[` and `{` are NOT
/// treated as glob indicators because they can appear in literal filenames
/// (e.g., `config[old].json` or `file{backup}.txt`).
///
/// The shell expands globs before commands run, so by the time we see the
/// command, patterns like `src/*.rs` have already been expanded to actual
/// file names. The main use case for glob support is when:
/// 1. The shell couldn't expand the pattern (no matches), or
/// 2. The pattern was quoted to prevent expansion
///
/// In both cases, `*` and `?` are reliable indicators of glob intent.
fn contains_glob_metachar(s: &str) -> bool {
    s.contains('*') || s.contains('?')
}

/// Normalize path separators to forward slashes for cross-platform glob matching.
///
/// globset expects forward slashes, but on Windows paths may use backslashes.
/// This ensures patterns like `src\*.rs` work correctly.
fn normalize_separators(s: &str) -> String {
    s.replace('\\', "/")
}

/// Filter paths to only include those matching command arguments
///
/// A path matches if it exactly matches a command argument OR is a descendant
/// of a directory argument (for operations like `rm -rf dir/`).
///
/// Supports glob patterns like `*.txt` or `src/{a,b}.rs` using the `globset`
/// crate for proper pattern matching. This prevents re-emitting unrelated files
/// from previous operations in the same working copy change.
///
/// NOTE: This function assumes both `paths` (from JJ) and `command_args` are
/// relative to the workspace root. Callers must normalize command_args before
/// calling this function (see `normalize_paths_to_workspace_root`).
///
/// Each shell command is processed independently, so prefix matching does not
/// cause duplicate reports across commands.
fn filter_paths_by_command(paths: &[String], command_args: &[String]) -> Vec<String> {
    // Empty args means all operands normalized outside workspace (e.g., `rm /tmp/foo`)
    // Return empty vec so caller falls back to syntactic detection
    if command_args.is_empty() {
        return vec![];
    }

    // Check if any argument contains actual glob metacharacters
    // (not just literal brackets/braces in filenames)
    let has_glob = command_args.iter().any(|arg| contains_glob_metachar(arg));

    if has_glob {
        // Build a glob matcher for all patterns
        // Normalize separators to forward slashes for cross-platform compatibility
        let mut builder = GlobSetBuilder::new();
        for arg in command_args {
            let normalized_arg = normalize_separators(arg);
            // Add the pattern itself
            if let Ok(glob) = Glob::new(&normalized_arg) {
                builder.add(glob);
            }
            // Also add pattern/** for directory matching (e.g., src/* matches src/foo/bar.rs)
            let dir_pattern = format!(
                "{}/**",
                normalized_arg.trim_end_matches('/').trim_end_matches('\\')
            );
            if let Ok(glob) = Glob::new(&dir_pattern) {
                builder.add(glob);
            }
        }

        // If glob compilation failed, fall back to returning empty vec
        let Ok(globset) = builder.build() else {
            return vec![];
        };

        // Filter paths using glob matching
        // Normalize path separators for cross-platform matching
        return paths
            .iter()
            .filter(|path| globset.is_match(normalize_separators(path)))
            .cloned()
            .collect();
    }

    // No globs - use exact/prefix matching
    paths
        .iter()
        .filter(|path| path_matches_command(path, command_args))
        .cloned()
        .collect()
}

/// Normalize command arguments to be relative to workspace root
///
/// When a shell command is run from a subdirectory (e.g., `cd src && rm foo.rs`),
/// the command arguments are relative to the shell's cwd, not the workspace root.
/// JJ's diff output always uses workspace-root-relative paths, so we need to
/// normalize the command arguments to match.
///
/// This function handles deleted files gracefully (canonicalization fails for non-existent paths)
/// and filters out paths outside the workspace instead of treating them as errors.
///
/// # Arguments
/// * `command_args` - Raw arguments from the shell command (relative to shell_cwd)
/// * `shell_cwd` - Directory where shell command was executed
/// * `workspace_root` - JJ workspace root directory
///
/// # Returns
/// Vector of workspace-relative paths (may be empty if all paths are outside workspace)
fn normalize_paths_to_workspace_root(
    command_args: &[String],
    shell_cwd: &Path,
    workspace_root: &Path,
) -> Result<Vec<String>> {
    // Canonicalize both shell_cwd and workspace_root to support symlinks
    let canonical_cwd = shell_cwd
        .canonicalize()
        .context("Failed to canonicalize shell cwd")?;
    let canonical_root = workspace_root
        .canonicalize()
        .context("Failed to canonicalize workspace root")?;

    // Collect successfully normalized paths, skip those outside workspace
    let normalized: Vec<String> = command_args
        .iter()
        .filter_map(|arg| {
            // Build absolute path from arg
            let raw_absolute = if Path::new(arg).is_absolute() {
                PathBuf::from(arg)
            } else {
                canonical_cwd.join(arg)
            };

            // Try to canonicalize the full path (works for existing files)
            // For deleted files, canonicalize the parent and append the filename
            // This handles symlinks and case-insensitive paths correctly
            let absolute = raw_absolute.canonicalize().unwrap_or_else(|_| {
                // File doesn't exist - try canonicalizing parent directory
                if let (Some(parent), Some(file_name)) =
                    (raw_absolute.parent(), raw_absolute.file_name())
                {
                    parent
                        .canonicalize()
                        .map(|p| p.join(file_name))
                        .unwrap_or(raw_absolute.clone())
                } else {
                    raw_absolute.clone()
                }
            });

            // Try to make workspace-relative using canonical paths
            // If strip_prefix fails (path outside workspace), skip this path
            absolute
                .strip_prefix(&canonical_root)
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .collect();

    Ok(normalized) // Return empty vec if nothing in workspace
}

/// Rebase workspace-relative paths to be relative to shell_cwd
///
/// JJ outputs paths relative to workspace root, but downstream consumers
/// expect paths relative to the cwd where the command was executed.
/// This function converts workspace-relative paths to cwd-relative paths.
///
/// # Arguments
/// * `workspace_paths` - Paths relative to workspace root (from JJ)
/// * `shell_cwd` - Directory where shell command was executed
/// * `workspace_root` - JJ workspace root directory
///
/// # Returns
/// Vector of paths relative to shell_cwd
///
/// # Example
/// ```ignore
/// workspace_root = /repo
/// shell_cwd = /repo/src
/// workspace_path = "src/foo.rs"
/// result = "foo.rs"  // Relative to /repo/src
/// ```
fn rebase_paths_to_cwd(
    workspace_paths: &[String],
    shell_cwd: &Path,
    workspace_root: &Path,
) -> Result<Vec<String>> {
    // Canonicalize both to handle symlinks
    let canonical_cwd = shell_cwd
        .canonicalize()
        .context("Failed to canonicalize shell cwd")?;
    let canonical_root = workspace_root
        .canonicalize()
        .context("Failed to canonicalize workspace root")?;

    // Get the relative path from workspace root to shell cwd
    let cwd_relative_to_root = canonical_cwd
        .strip_prefix(&canonical_root)
        .context("Shell cwd is not within workspace root")?;

    workspace_paths
        .iter()
        .map(|workspace_path| {
            let workspace_path = Path::new(workspace_path);

            // If cwd is workspace root, paths stay the same
            if cwd_relative_to_root.as_os_str().is_empty() {
                return Ok(workspace_path.to_string_lossy().to_string());
            }

            // Try to strip the cwd prefix from the workspace path
            // e.g., workspace_path="src/foo.rs", cwd_relative="src" -> "foo.rs"
            if let Ok(relative) = workspace_path.strip_prefix(cwd_relative_to_root) {
                Ok(relative.to_string_lossy().to_string())
            } else {
                // Path is outside the cwd directory or in a sibling, need to use ../ notation
                // Find common prefix and compute relative path from there
                let cwd_components: Vec<_> = cwd_relative_to_root.components().collect();
                let path_components: Vec<_> = workspace_path.components().collect();

                // Find the length of the common prefix
                let common_len = cwd_components
                    .iter()
                    .zip(path_components.iter())
                    .take_while(|(a, b)| a == b)
                    .count();

                let mut result = PathBuf::new();

                // Go up from cwd to common ancestor
                let levels_up = cwd_components.len() - common_len;
                for _ in 0..levels_up {
                    result.push("..");
                }

                // Append the remaining path components after the common prefix
                for component in &path_components[common_len..] {
                    result.push(component);
                }

                Ok(result.to_string_lossy().to_string())
            }
        })
        .collect()
}

/// Check if a path matches any command argument
///
/// A path matches if:
/// - It exactly equals a command argument
/// - It is a descendant of a directory argument (prefix match)
///
/// Uses Path semantics for cross-platform compatibility (handles both
/// forward and backslashes correctly on Windows).
///
/// Examples:
/// - path="src/foo.rs", arg="src/foo.rs" -> match (exact)
/// - path="src/foo.rs", arg="src/" -> match (directory)
/// - path="src/foo.rs", arg="src" -> match (directory without trailing slash)
fn path_matches_command(path: &str, command_args: &[String]) -> bool {
    let path = Path::new(path);

    command_args.iter().any(|arg| {
        let arg_path = Path::new(arg.trim_end_matches('/').trim_end_matches('\\'));

        // Exact match
        path == arg_path
        // OR path is under a directory argument (starts with)
        || path.starts_with(arg_path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_move_line() {
        // Standard rename
        assert_eq!(
            parse_move_line("{old.txt => new.txt}"),
            Some(("old.txt".to_string(), "new.txt".to_string()))
        );

        // Move to directory
        assert_eq!(
            parse_move_line("{file.txt => dir/file.txt}"),
            Some(("file.txt".to_string(), "dir/file.txt".to_string()))
        );

        // Path with spaces (no quotes in JJ output)
        assert_eq!(
            parse_move_line("{file with spaces.txt => renamed.txt}"),
            Some((
                "file with spaces.txt".to_string(),
                "renamed.txt".to_string()
            ))
        );

        // Destination with spaces
        assert_eq!(
            parse_move_line("{old.txt => path/with spaces/new.txt}"),
            Some((
                "old.txt".to_string(),
                "path/with spaces/new.txt".to_string()
            ))
        );

        // Nested paths
        assert_eq!(
            parse_move_line("{src/old.rs => src/new.rs}"),
            Some(("src/old.rs".to_string(), "src/new.rs".to_string()))
        );
    }

    #[test]
    fn test_get_deleted_paths_parsing() {
        let output = "D a.txt\nM file.txt\nR b.txt => renamed.txt";
        let deleted: Vec<String> = output
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.starts_with("D ") {
                    Some(line[2..].trim().to_string())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(deleted, vec!["a.txt"]);
    }

    #[test]
    fn test_get_move_operations_parsing() {
        let output = "D a.txt\nM file.txt\nR {b.txt => renamed.txt}\nR {old => new/path}";
        let moves: Vec<(String, String)> = output
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if let Some(content) = line.strip_prefix("R ") {
                    parse_move_line(content)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(
            moves,
            vec![
                ("b.txt".to_string(), "renamed.txt".to_string()),
                ("old".to_string(), "new/path".to_string())
            ]
        );
    }

    #[test]
    fn test_path_matches_command() {
        let args = vec!["src/foo.rs".to_string(), "lib.rs".to_string()];

        // Exact matches
        assert!(path_matches_command("src/foo.rs", &args));
        assert!(path_matches_command("lib.rs", &args));

        // No match
        assert!(!path_matches_command("other.rs", &args));

        // Directory matching (with trailing slash)
        let dir_args = vec!["src/".to_string()];
        assert!(path_matches_command("src/foo.rs", &dir_args));
        assert!(path_matches_command("src/nested/bar.rs", &dir_args));
        assert!(!path_matches_command("lib.rs", &dir_args));

        // Directory matching (without trailing slash)
        let dir_args = vec!["src".to_string()];
        assert!(path_matches_command("src/foo.rs", &dir_args));
        assert!(path_matches_command("src/nested/bar.rs", &dir_args));
        assert!(!path_matches_command("lib.rs", &dir_args));
    }

    #[test]
    fn test_filter_paths_by_command() {
        let paths = vec![
            "src/foo.rs".to_string(),
            "src/bar.rs".to_string(),
            "lib.rs".to_string(),
        ];

        // Filter to only src/foo.rs (exact match)
        let args = vec!["src/foo.rs".to_string()];
        let filtered = filter_paths_by_command(&paths, &args);
        assert_eq!(filtered, vec!["src/foo.rs"]);

        // Directory matching (matches all descendants)
        let args = vec!["src/".to_string()];
        let filtered = filter_paths_by_command(&paths, &args);
        assert_eq!(filtered, vec!["src/foo.rs", "src/bar.rs"]);

        // Multiple exact matches
        let args = vec!["src/foo.rs".to_string(), "lib.rs".to_string()];
        let filtered = filter_paths_by_command(&paths, &args);
        assert_eq!(filtered, vec!["src/foo.rs", "lib.rs"]);

        // Empty args = return empty vec (fallback to syntactic)
        let filtered = filter_paths_by_command(&paths, &[]);
        assert_eq!(filtered, Vec::<String>::new());
    }

    #[test]
    fn test_normalize_paths_to_workspace_root() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace_root = temp_dir.path();

        // Create subdirectory
        let subdir = workspace_root.join("src");
        std::fs::create_dir(&subdir).unwrap();

        // Test 1: Command from workspace root
        let args = vec!["src/foo.rs".to_string()];
        let normalized =
            normalize_paths_to_workspace_root(&args, workspace_root, workspace_root).unwrap();
        assert_eq!(normalized, vec!["src/foo.rs"]);

        // Test 2: Command from subdirectory (relative path)
        let args = vec!["foo.rs".to_string()];
        let normalized = normalize_paths_to_workspace_root(&args, &subdir, workspace_root).unwrap();
        assert_eq!(normalized, vec!["src/foo.rs"]);

        // Test 3: Multiple paths from subdirectory
        let args = vec!["foo.rs".to_string(), "bar.rs".to_string()];
        let normalized = normalize_paths_to_workspace_root(&args, &subdir, workspace_root).unwrap();
        assert_eq!(normalized, vec!["src/foo.rs", "src/bar.rs"]);

        // Test 4: Absolute path
        let absolute_path = workspace_root.join("lib.rs");
        let args = vec![absolute_path.to_string_lossy().to_string()];
        let normalized = normalize_paths_to_workspace_root(&args, &subdir, workspace_root).unwrap();
        assert_eq!(normalized, vec!["lib.rs"]);
    }

    #[test]
    fn test_normalize_paths_outside_workspace_filtered() {
        let workspace_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();

        let workspace_root = workspace_dir.path();
        let shell_cwd = workspace_root;

        // Create a file inside workspace
        let inside_file = workspace_root.join("inside.txt");
        std::fs::write(&inside_file, "test").unwrap();

        // Path outside workspace should be filtered out (not error)
        let outside_path = outside_dir.path().join("file.txt");
        let args = vec![
            outside_path.to_string_lossy().to_string(),
            "inside.txt".to_string(),
        ];
        let result = normalize_paths_to_workspace_root(&args, shell_cwd, workspace_root).unwrap();

        // Only the inside path should be included
        assert_eq!(result, vec!["inside.txt"]);
    }

    #[test]
    fn test_normalize_deleted_file_paths() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace_root = temp_dir.path();

        // Create and then delete a file to simulate deleted file scenario
        let file_path = workspace_root.join("deleted.txt");
        std::fs::write(&file_path, "test").unwrap();
        std::fs::remove_file(&file_path).unwrap();

        // Should still normalize the path even though file doesn't exist
        let args = vec!["deleted.txt".to_string()];
        let result =
            normalize_paths_to_workspace_root(&args, workspace_root, workspace_root).unwrap();
        assert_eq!(result, vec!["deleted.txt"]);
    }

    #[test]
    fn test_rebase_paths_to_cwd_from_root() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace_root = temp_dir.path();

        // Command from workspace root - paths stay the same
        let workspace_paths = vec!["src/foo.rs".to_string(), "lib.rs".to_string()];
        let result = rebase_paths_to_cwd(&workspace_paths, workspace_root, workspace_root).unwrap();
        assert_eq!(result, vec!["src/foo.rs", "lib.rs"]);
    }

    #[test]
    fn test_rebase_paths_to_cwd_from_subdir() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace_root = temp_dir.path();

        // Create subdirectory
        let subdir = workspace_root.join("src");
        std::fs::create_dir(&subdir).unwrap();

        // JJ reports "src/foo.rs" (workspace-relative)
        // Command ran from src/, so result should be "foo.rs" (cwd-relative)
        let workspace_paths = vec!["src/foo.rs".to_string(), "src/bar.rs".to_string()];
        let result = rebase_paths_to_cwd(&workspace_paths, &subdir, workspace_root).unwrap();
        assert_eq!(result, vec!["foo.rs", "bar.rs"]);
    }

    #[test]
    fn test_rebase_paths_to_cwd_outside_cwd() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace_root = temp_dir.path();

        // Create subdirectory
        let subdir = workspace_root.join("src");
        std::fs::create_dir(&subdir).unwrap();

        // JJ reports "lib.rs" (at workspace root)
        // Command ran from src/, so result should be "../lib.rs"
        let workspace_paths = vec!["lib.rs".to_string()];
        let result = rebase_paths_to_cwd(&workspace_paths, &subdir, workspace_root).unwrap();
        assert_eq!(result, vec!["../lib.rs"]);
    }

    #[test]
    fn test_rebase_paths_to_cwd_nested_subdir() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace_root = temp_dir.path();

        // Create nested subdirectory
        let nested = workspace_root.join("src/nested");
        std::fs::create_dir_all(&nested).unwrap();

        // JJ reports "src/nested/foo.rs" (workspace-relative)
        // Command ran from src/nested/, so result should be "foo.rs"
        let workspace_paths = vec!["src/nested/foo.rs".to_string()];
        let result = rebase_paths_to_cwd(&workspace_paths, &nested, workspace_root).unwrap();
        assert_eq!(result, vec!["foo.rs"]);

        // JJ reports "src/other.rs" (sibling directory)
        // Command ran from src/nested/, so result should be "../other.rs"
        let workspace_paths = vec!["src/other.rs".to_string()];
        let result = rebase_paths_to_cwd(&workspace_paths, &nested, workspace_root).unwrap();
        assert_eq!(result, vec!["../other.rs"]);

        // JJ reports "lib.rs" (at workspace root)
        // Command ran from src/nested/, so result should be "../../lib.rs"
        let workspace_paths = vec!["lib.rs".to_string()];
        let result = rebase_paths_to_cwd(&workspace_paths, &nested, workspace_root).unwrap();
        assert_eq!(result, vec!["../../lib.rs"]);
    }

    #[test]
    #[cfg(windows)]
    fn test_path_matches_command_windows_separators() {
        // Test that Windows backslash separators work correctly
        // On Windows, Path normalizes backslashes to forward slashes
        let args = vec!["src\\foo.rs".to_string(), "lib.rs".to_string()];

        // Forward slash in path should match backslash in arg (Path normalization)
        assert!(path_matches_command("src/foo.rs", &args));
        assert!(path_matches_command("lib.rs", &args));
        assert!(!path_matches_command("other.rs", &args));

        // Directory matching with backslashes
        let dir_args = vec!["src\\".to_string()];
        assert!(path_matches_command("src/foo.rs", &dir_args));
        assert!(path_matches_command("src/nested/bar.rs", &dir_args));
    }

    #[test]
    #[cfg(not(windows))]
    fn test_path_matches_command_unix_separators() {
        // On Unix, backslashes are valid filename characters, not path separators
        // This test verifies forward slash matching works correctly
        let args = vec!["src/foo.rs".to_string(), "lib.rs".to_string()];

        // Forward slashes work as path separators
        assert!(path_matches_command("src/foo.rs", &args));
        assert!(path_matches_command("lib.rs", &args));
        assert!(!path_matches_command("other.rs", &args));

        // Directory matching with forward slashes
        let dir_args = vec!["src/".to_string()];
        assert!(path_matches_command("src/foo.rs", &dir_args));
        assert!(path_matches_command("src/nested/bar.rs", &dir_args));
    }

    #[test]
    fn test_filter_paths_with_glob_patterns() {
        let paths = vec![
            "a.txt".to_string(),
            "b.txt".to_string(),
            "src/c.rs".to_string(),
            "src/d.rs".to_string(),
        ];

        // Star pattern - should match only .txt files
        let glob_args = vec!["*.txt".to_string()];
        let filtered = filter_paths_by_command(&paths, &glob_args);
        assert_eq!(filtered, vec!["a.txt", "b.txt"]);

        // Question mark pattern - should match single-char filenames
        let glob_args = vec!["?.txt".to_string()];
        let filtered = filter_paths_by_command(&paths, &glob_args);
        assert_eq!(filtered, vec!["a.txt", "b.txt"]);

        // Directory glob - should match all files in src/
        let glob_args = vec!["src/*".to_string()];
        let filtered = filter_paths_by_command(&paths, &glob_args);
        assert_eq!(filtered, vec!["src/c.rs", "src/d.rs"]);

        // Non-glob literal - should filter normally
        let literal_args = vec!["a.txt".to_string()];
        let filtered = filter_paths_by_command(&paths, &literal_args);
        assert_eq!(filtered, vec!["a.txt"]);

        // Note: [ab] and {a,b} are NOT treated as globs to avoid
        // false positives with filenames like config[old].json
    }

    #[test]
    fn test_contains_glob_metachar() {
        // * and ? are always glob characters
        assert!(contains_glob_metachar("*.txt"));
        assert!(contains_glob_metachar("file?.rs"));
        assert!(contains_glob_metachar("src/*.rs"));

        // [ and { are NOT treated as glob indicators (too common in filenames)
        assert!(!contains_glob_metachar("[ab].txt"));
        assert!(!contains_glob_metachar("{a,b}.txt"));
        assert!(!contains_glob_metachar("config[old].json"));
        assert!(!contains_glob_metachar("file{backup}.txt"));

        // Normal files without special chars
        assert!(!contains_glob_metachar("normal_file.txt"));
        assert!(!contains_glob_metachar("src/foo.rs"));
    }

    #[test]
    fn test_literal_brackets_in_filename() {
        // Filenames with literal brackets should NOT trigger glob mode
        let paths = vec![
            "config[old].json".to_string(),
            "config[new].json".to_string(),
            "normal.json".to_string(),
        ];

        // No * or ? means literal matching, so exact match works
        let args = vec!["config[old].json".to_string()];
        let filtered = filter_paths_by_command(&paths, &args);
        assert_eq!(filtered, vec!["config[old].json"]);
    }

    #[test]
    fn test_literal_braces_in_filename() {
        // Filenames with literal braces should NOT trigger glob mode
        let paths = vec![
            "file{backup}.txt".to_string(),
            "file{old}.txt".to_string(),
            "normal.txt".to_string(),
        ];

        // No * or ? means literal matching
        let args = vec!["file{backup}.txt".to_string()];
        let filtered = filter_paths_by_command(&paths, &args);
        assert_eq!(filtered, vec!["file{backup}.txt"]);
    }

    #[test]
    fn test_glob_with_backslash_separators() {
        // Windows-style paths with backslashes should work with glob matching
        let paths = vec![
            "src/foo.rs".to_string(),
            "src/bar.rs".to_string(),
            "lib/baz.rs".to_string(),
        ];

        // Windows-style glob pattern with backslashes
        let args = vec!["src\\*.rs".to_string()];
        let filtered = filter_paths_by_command(&paths, &args);
        assert_eq!(filtered, vec!["src/foo.rs", "src/bar.rs"]);

        // Backslash with question mark glob
        let args = vec!["src\\???.rs".to_string()];
        let filtered = filter_paths_by_command(&paths, &args);
        assert_eq!(filtered, vec!["src/foo.rs", "src/bar.rs"]);
    }

    #[test]
    fn test_normalize_separators() {
        assert_eq!(normalize_separators("src\\foo.rs"), "src/foo.rs");
        assert_eq!(normalize_separators("src/foo.rs"), "src/foo.rs");
        assert_eq!(normalize_separators("a\\b\\c\\d.txt"), "a/b/c/d.txt");
        assert_eq!(
            normalize_separators("no_separators.txt"),
            "no_separators.txt"
        );
    }
}
