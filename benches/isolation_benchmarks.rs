use aiki::repo_id;
use aiki::session::isolation::{
    absorb_workspace, cleanup_workspace, create_isolated_workspace,
};
use criterion::{criterion_group, criterion_main, Criterion};
use std::fs;
use tempfile::TempDir;

/// Setup a temporary JJ repository with a repo-id for workspace isolation benchmarks.
///
/// Creates a real JJ repo, ensures it has a repo-id, and writes a sample file
/// so that workspace creation has something to fork from.
fn setup_temp_repo() -> TempDir {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Initialize jj repo
    std::process::Command::new("jj")
        .current_dir(temp_dir.path())
        .args(["git", "init"])
        .output()
        .expect("Failed to init jj repo");

    // Ensure repo-id exists (required by create_isolated_workspace)
    repo_id::ensure_repo_id(temp_dir.path()).expect("Failed to ensure repo-id");

    // Write a sample file so there's content in the repo
    fs::write(temp_dir.path().join("README.md"), "# Test repo\n").unwrap();

    // Snapshot so JJ tracks the file
    std::process::Command::new("jj")
        .current_dir(temp_dir.path())
        .args(["debug", "snapshot"])
        .output()
        .expect("Failed to snapshot");

    temp_dir
}

/// Benchmark creating an isolated workspace.
///
/// Measures the full create path: resolve parent change, run `jj workspace add`.
fn bench_create_workspace(c: &mut Criterion) {
    // Use AIKI_WORKSPACES_DIR to isolate benchmark workspaces from real ones
    let ws_base = TempDir::new().expect("Failed to create workspace base dir");
    std::env::set_var("AIKI_WORKSPACES_DIR", ws_base.path());

    c.bench_function("create_isolated_workspace", |b| {
        b.iter_batched(
            || {
                let temp_dir = setup_temp_repo();
                let session_uuid = uuid::Uuid::new_v4().to_string();
                (temp_dir, session_uuid)
            },
            |(temp_dir, session_uuid)| {
                let ws = create_isolated_workspace(temp_dir.path(), &session_uuid)
                    .expect("Failed to create workspace");

                // Return workspace for cleanup (TempDir drop handles repo)
                let _ = cleanup_workspace(temp_dir.path(), &ws);
            },
            criterion::BatchSize::LargeInput,
        );
    });
}

/// Benchmark absorbing a workspace back into main.
///
/// Setup creates a workspace and writes a file in it, then the benchmark
/// measures absorb_workspace (snapshot + rebase).
fn bench_absorb_workspace(c: &mut Criterion) {
    let ws_base = TempDir::new().expect("Failed to create workspace base dir");
    std::env::set_var("AIKI_WORKSPACES_DIR", ws_base.path());

    c.bench_function("absorb_workspace", |b| {
        b.iter_batched(
            || {
                let temp_dir = setup_temp_repo();
                let session_uuid = uuid::Uuid::new_v4().to_string();
                let ws = create_isolated_workspace(temp_dir.path(), &session_uuid)
                    .expect("Failed to create workspace");

                // Write a file in the workspace to simulate agent work
                fs::write(ws.path.join("agent-output.txt"), "some changes\n").unwrap();

                (temp_dir, ws)
            },
            |(temp_dir, ws)| {
                absorb_workspace(temp_dir.path(), &ws, None)
                    .expect("Failed to absorb workspace");

                // Cleanup after absorb
                let _ = cleanup_workspace(temp_dir.path(), &ws);
            },
            criterion::BatchSize::LargeInput,
        );
    });
}

/// Benchmark the full isolation lifecycle: create → write → absorb → cleanup.
///
/// This represents the real-world cost of an isolated session from start to finish.
fn bench_full_isolation_lifecycle(c: &mut Criterion) {
    let ws_base = TempDir::new().expect("Failed to create workspace base dir");
    std::env::set_var("AIKI_WORKSPACES_DIR", ws_base.path());

    c.bench_function("isolation_lifecycle", |b| {
        b.iter_batched(
            || {
                let temp_dir = setup_temp_repo();
                let session_uuid = uuid::Uuid::new_v4().to_string();
                (temp_dir, session_uuid)
            },
            |(temp_dir, session_uuid)| {
                // Create workspace
                let ws = create_isolated_workspace(temp_dir.path(), &session_uuid)
                    .expect("Failed to create workspace");

                // Simulate agent work
                fs::write(ws.path.join("result.txt"), "agent output\n").unwrap();

                // Absorb back to main
                absorb_workspace(temp_dir.path(), &ws, None)
                    .expect("Failed to absorb workspace");

                // Cleanup
                cleanup_workspace(temp_dir.path(), &ws)
                    .expect("Failed to cleanup workspace");
            },
            criterion::BatchSize::LargeInput,
        );
    });
}

criterion_group!(
    benches,
    bench_create_workspace,
    bench_absorb_workspace,
    bench_full_isolation_lifecycle
);
criterion_main!(benches);
