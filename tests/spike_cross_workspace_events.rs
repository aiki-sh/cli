//! Spike: validate that task events written from an isolated workspace
//! are visible from the main workspace in real-time (before absorption).
//!
//! Setup:
//!   1. Create a temp JJ repo (acts as "main")
//!   2. Create an isolated workspace ("ws-run1")
//!   3. Write task events from ws-run1
//!   4. Read events + materialize graph from main
//!   5. Verify main sees ws-run1's events

use aiki::tasks::{
    graph::materialize_graph,
    id::generate_task_id,
    storage::{ensure_tasks_branch, read_events, write_event},
    types::{TaskEvent, TaskPriority, TaskStatus},
};
use chrono::Utc;
use std::path::Path;
use std::time::Instant;

fn setup_repo() -> tempfile::TempDir {
    let temp_dir = tempfile::TempDir::new().expect("create temp dir");
    let cwd = temp_dir.path();

    std::process::Command::new("jj")
        .current_dir(cwd)
        .args(["git", "init"])
        .output()
        .expect("init jj");

    ensure_tasks_branch(cwd).expect("ensure tasks branch");
    temp_dir
}

fn create_workspace(repo_dir: &Path, name: &str) -> std::path::PathBuf {
    let ws_path = repo_dir.join(format!("ws-{name}"));
    let output = std::process::Command::new("jj")
        .current_dir(repo_dir)
        .args(["workspace", "add", ws_path.to_str().unwrap(), "--name", name])
        .output()
        .expect("create workspace");
    assert!(
        output.status.success(),
        "workspace add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ws_path
}

fn create_task(cwd: &Path, name: &str) -> String {
    let task_id = generate_task_id(name);
    let event = TaskEvent::Created {
        task_id: task_id.clone(),
        name: name.to_string(),
        slug: None,
        task_type: None,
        priority: TaskPriority::P2,
        assignee: None,
        sources: Vec::new(),
        template: None,
        instructions: None,
        data: std::collections::HashMap::new(),
        timestamp: Utc::now(),
    };
    write_event(cwd, &event).expect("write created");
    task_id
}

fn start_task(cwd: &Path, task_id: &str) {
    let event = TaskEvent::Started {
        task_ids: vec![task_id.to_string()],
        agent_type: "claude-code".to_string(),
        session_id: Some("test-session".to_string()),
        turn_id: None,
        working_copy: None,
        instructions: None,
        timestamp: Utc::now(),
    };
    write_event(cwd, &event).expect("write started");
}

#[test]
fn spike_main_sees_isolated_workspace_events() {
    let repo = setup_repo();
    let main_cwd = repo.path();
    let ws_run1 = create_workspace(main_cwd, "run1");

    println!("\n=== Phase 1: Write events from main, verify from main ===");
    let main_task = create_task(main_cwd, "Main task");
    start_task(main_cwd, &main_task);

    let events = read_events(main_cwd).expect("read from main");
    let graph = materialize_graph(&events);
    println!(
        "  Main wrote 2 events, main reads {} events, {} tasks",
        events.len(),
        graph.tasks.len()
    );
    assert_eq!(events.len(), 2, "main should see its own 2 events");
    assert_eq!(graph.tasks.len(), 1, "main should see 1 task");

    println!("\n=== Phase 2: Write events from ws-run1, read from MAIN ===");
    let ws_task1 = create_task(&ws_run1, "Workspace task 1");
    start_task(&ws_run1, &ws_task1);
    let ws_task2 = create_task(&ws_run1, "Workspace task 2");

    let t0 = Instant::now();
    let events = read_events(main_cwd).expect("read from main after ws writes");
    let graph = materialize_graph(&events);
    let elapsed = t0.elapsed();

    println!(
        "  ws-run1 wrote 3 more events, main reads {} events, {} tasks ({:?})",
        events.len(),
        graph.tasks.len(),
        elapsed
    );

    // THE KEY ASSERTIONS: does main see ws-run1's events?
    assert_eq!(
        events.len(),
        5,
        "main should see all 5 events (2 from main + 3 from ws-run1)"
    );
    assert_eq!(
        graph.tasks.len(),
        3,
        "main should see 3 tasks (1 from main + 2 from ws-run1)"
    );

    // Verify task states
    let main_t = graph.tasks.get(&main_task).expect("main task in graph");
    assert_eq!(main_t.status, TaskStatus::InProgress);

    let ws_t1 = graph.tasks.get(&ws_task1).expect("ws task 1 in graph");
    assert_eq!(ws_t1.status, TaskStatus::InProgress);

    let ws_t2 = graph.tasks.get(&ws_task2).expect("ws task 2 in graph");
    assert_eq!(ws_t2.status, TaskStatus::Open);

    println!("\n=== Phase 3: Write more from ws-run1, read from ws-run1 itself ===");
    start_task(&ws_run1, &ws_task2);

    let events_from_ws = read_events(&ws_run1).expect("read from ws-run1");
    let graph_from_ws = materialize_graph(&events_from_ws);

    println!(
        "  ws-run1 reads {} events, {} tasks",
        events_from_ws.len(),
        graph_from_ws.tasks.len()
    );

    // Both should see the same thing
    let events_from_main = read_events(main_cwd).expect("read from main again");
    let graph_from_main = materialize_graph(&events_from_main);

    assert_eq!(
        events_from_ws.len(),
        events_from_main.len(),
        "ws-run1 and main should see the same number of events"
    );
    assert_eq!(
        graph_from_ws.tasks.len(),
        graph_from_main.tasks.len(),
        "ws-run1 and main should see the same number of tasks"
    );

    println!("\n=== Phase 4: Concurrent scenario — interleaved writes ===");
    // Main writes
    let interleaved_main = create_task(main_cwd, "Interleaved from main");
    // ws-run1 writes
    let interleaved_ws = create_task(&ws_run1, "Interleaved from ws");
    // Main writes again
    start_task(main_cwd, &interleaved_main);
    // ws-run1 writes again
    start_task(&ws_run1, &interleaved_ws);

    let final_events = read_events(main_cwd).expect("final read from main");
    let final_graph = materialize_graph(&final_events);

    println!(
        "  After interleaved writes: {} events, {} tasks",
        final_events.len(),
        final_graph.tasks.len()
    );

    assert_eq!(final_graph.tasks.len(), 5, "should see all 5 tasks");

    // Verify all tasks are in expected states
    for (id, task) in &final_graph.tasks {
        println!(
            "  Task {}: {} — {:?}",
            &id[..8],
            task.name,
            task.status
        );
        assert_eq!(
            task.status,
            TaskStatus::InProgress,
            "all tasks should be in progress"
        );
    }

    println!("\n=== PASS: Main workspace sees all events from isolated workspaces in real-time ===\n");
}
