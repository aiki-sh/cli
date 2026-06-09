//! Screen state tests for TUI views.
//!
//! Tests that view functions produce the correct output for each state
//! documented in ops/now/tui/screen-states.md. Each test constructs a
//! specific TaskGraph state, runs the pure view function, and asserts
//! structural properties of the output lines.
//!
//! Run with: cargo test --test tui_screen_states

use aiki::tasks::graph::EdgeStore;
use aiki::tasks::types::{FastHashMap, TaskComment, TaskOutcome, TaskPriority, TaskStatus};
use aiki::tasks::{Task, TaskGraph};
use aiki::tui::app::{LineStyle, SubtaskStatus, WindowState};
use aiki::tui::render::apply_dimming;
use chrono::{Duration, Utc};
use std::collections::HashMap;

// ── Helpers ────────────────────────────────────────────────────────────

fn make_task(id: &str, name: &str, status: TaskStatus) -> Task {
    Task {
        id: id.to_string(),
        name: name.to_string(),
        slug: None,
        task_type: None,
        status,
        priority: TaskPriority::P2,
        assignee: None,
        sources: Vec::new(),
        template: None,
        instructions: None,
        data: HashMap::new(),
        created_at: Utc::now(),
        started_at: None,
        claimed_by_session: None,
        last_session_id: None,
        stopped_reason: None,
        closed_outcome: if status == TaskStatus::Closed {
            Some(TaskOutcome::Done)
        } else {
            None
        },
        confidence: None,
        summary: None,
        turn_started: None,
        closed_at: None,
        turn_closed: None,
        turn_stopped: None,
        comments: Vec::new(),
    }
}

fn make_graph() -> TaskGraph {
    TaskGraph {
        tasks: FastHashMap::default(),
        edges: EdgeStore::default(),
        slug_index: FastHashMap::default(),
    }
}

fn window() -> WindowState {
    WindowState::new(80)
}

/// Assert a line contains expected text.
fn assert_line_contains(lines: &[aiki::tui::app::Line], text: &str) {
    assert!(
        lines.iter().any(|l| l.text.contains(text)),
        "Expected a line containing {:?}, got:\n{}",
        text,
        lines
            .iter()
            .map(|l| format!("  {:?}", l.text))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// Assert no line contains forbidden text.
fn assert_no_line_contains(lines: &[aiki::tui::app::Line], text: &str) {
    assert!(
        !lines.iter().any(|l| l.text.contains(text)),
        "Expected NO line containing {:?}, but found one",
        text,
    );
}

/// Count lines matching a predicate.
fn count_lines<F: Fn(&aiki::tui::app::Line) -> bool>(
    lines: &[aiki::tui::app::Line],
    f: F,
) -> usize {
    lines.iter().filter(|l| f(l)).count()
}

// ═══════════════════════════════════════════════════════════════════════
// Flow 1: `aiki run <id>`
// ═══════════════════════════════════════════════════════════════════════

/// State 1.0a: Loading — task not yet in graph
#[test]
fn flow1_state_0a_loading() {
    let graph = make_graph();
    let lines = aiki::tui::screens::task_run::view(&graph, "nonexistent", &window());

    // Should show loading spinner
    assert_eq!(lines.len(), 1);
    assert_line_contains(&lines, "Reading task graph");
    assert!(matches!(
        lines[0].style,
        LineStyle::PhaseHeader { active: true }
    ));
}

/// State 1.0d: Session starting — task is Open
#[test]
fn flow1_state_0d_session_starting() {
    let mut graph = make_graph();
    let task = make_task("task1", "Fix auth bug", TaskStatus::Open);
    graph.tasks.insert("task1".to_string(), task);

    let lines = aiki::tui::screens::task_run::view(&graph, "task1", &window());

    // Should show spinner + "starting session..."
    assert!(matches!(
        lines[0].style,
        LineStyle::PhaseHeader { active: true }
    ));
    assert_line_contains(&lines, "starting session");
}

/// State 1.1: Active leaf task with heartbeat
#[test]
fn flow1_state_1_active_leaf() {
    let mut graph = make_graph();
    let mut task = make_task("task1", "Fix auth bug", TaskStatus::InProgress);
    task.started_at = Some(Utc::now() - Duration::seconds(12));
    task.data
        .insert("agent_type".to_string(), "claude-code".to_string());
    task.comments.push(TaskComment {
        id: Some("hb1".into()),
        text: "Reading the existing implementation...".into(),
        timestamp: Utc::now(),
        data: {
            let mut m = HashMap::new();
            m.insert("type".to_string(), "heartbeat".to_string());
            m
        },
    });
    graph.tasks.insert("task1".to_string(), task);

    let lines = aiki::tui::screens::task_run::view(&graph, "task1", &window());

    // Should have active phase header + heartbeat child
    assert!(matches!(
        lines[0].style,
        LineStyle::PhaseHeader { active: true }
    ));
    assert_line_contains(&lines, "Reading the existing implementation");
}

/// State 1.2: Done leaf task
#[test]
fn flow1_state_2_done_leaf() {
    let mut graph = make_graph();
    let mut task = make_task("task1", "Fix auth bug", TaskStatus::Closed);
    task.started_at = Some(Utc::now() - Duration::minutes(2));
    task.closed_at = Some(Utc::now());
    task.summary = Some("Added null check before token access".to_string());
    graph.tasks.insert("task1".to_string(), task);

    let lines = aiki::tui::screens::task_run::view(&graph, "task1", &window());

    // Should have completed phase header
    assert_line_contains(&lines, "task completed");
    assert_line_contains(&lines, "null check");
    // Should have hint
    assert_line_contains(&lines, "aiki task show");
    // Phase header should NOT be active
    assert!(matches!(
        lines[0].style,
        LineStyle::PhaseHeader { active: false }
    ));
}

/// State 1.3: Failed leaf task
#[test]
fn flow1_state_3_failed_leaf() {
    let mut graph = make_graph();
    let mut task = make_task("task1", "Fix auth bug", TaskStatus::Stopped);
    task.started_at = Some(Utc::now() - Duration::minutes(1));
    task.stopped_reason = Some("cargo check found 3 compilation errors".to_string());
    graph.tasks.insert("task1".to_string(), task);

    let lines = aiki::tui::screens::task_run::view(&graph, "task1", &window());

    assert_line_contains(&lines, "task failed");
    assert_line_contains(&lines, "compilation errors");
    assert!(matches!(lines[0].style, LineStyle::PhaseHeaderFailed));
}

/// State 1.6: Parent task with subtasks in progress
#[test]
fn flow1_state_6_parent_subtasks_in_progress() {
    let mut graph = make_graph();

    let mut parent = make_task("parent1", "Fix review issues", TaskStatus::InProgress);
    parent.started_at = Some(Utc::now() - Duration::seconds(45));
    parent
        .data
        .insert("agent_type".to_string(), "claude-code".to_string());
    graph.tasks.insert("parent1".to_string(), parent);

    let mut s1 = make_task("sub1", "Fix null check", TaskStatus::InProgress);
    s1.started_at = Some(Utc::now() - Duration::seconds(32));
    graph.tasks.insert("sub1".to_string(), s1);
    graph.edges.add("sub1", "parent1", "subtask-of");

    let s2 = make_task("sub2", "Add error handling", TaskStatus::Open);
    graph.tasks.insert("sub2".to_string(), s2);
    graph.edges.add("sub2", "parent1", "subtask-of");

    let s3 = make_task("sub3", "Remove unused import", TaskStatus::Open);
    graph.tasks.insert("sub3".to_string(), s3);
    graph.edges.add("sub3", "parent1", "subtask-of");

    let lines = aiki::tui::screens::task_run::view(&graph, "parent1", &window());

    // Should have progress text
    assert_line_contains(&lines, "0/3 subtasks completed");
    // Should have subtask table
    assert!(lines
        .iter()
        .any(|l| matches!(l.style, LineStyle::SubtaskHeader)));
    // Should have active subtask
    assert!(lines.iter().any(|l| matches!(
        l.style,
        LineStyle::Subtask {
            status: SubtaskStatus::Active
        }
    )));
    // Should have pending subtasks
    assert!(
        count_lines(&lines, |l| matches!(
            l.style,
            LineStyle::Subtask {
                status: SubtaskStatus::Pending | SubtaskStatus::PendingUnassigned
            }
        )) >= 2
    );
}

/// State 1.8: Parent task — all subtasks done
#[test]
fn flow1_state_8_parent_all_done() {
    let mut graph = make_graph();

    let mut parent = make_task("parent1", "Fix review issues", TaskStatus::Closed);
    parent.started_at = Some(Utc::now() - Duration::minutes(3));
    parent.closed_at = Some(Utc::now());
    parent.summary = Some("3/3 subtasks completed".to_string());
    graph.tasks.insert("parent1".to_string(), parent);

    for (i, name) in ["Fix null check", "Add error handling", "Remove import"]
        .iter()
        .enumerate()
    {
        let mut s = make_task(&format!("sub{}", i), name, TaskStatus::Closed);
        s.started_at = Some(Utc::now() - Duration::seconds(60));
        s.closed_at = Some(Utc::now() - Duration::seconds(10));
        graph.tasks.insert(format!("sub{}", i), s);
        graph
            .edges
            .add(&format!("sub{}", i), "parent1", "subtask-of");
    }

    let lines = aiki::tui::screens::task_run::view(&graph, "parent1", &window());

    // Should have completed header
    assert_line_contains(&lines, "task completed");
    // All subtasks should be done
    let done_count = count_lines(&lines, |l| {
        matches!(
            l.style,
            LineStyle::Subtask {
                status: SubtaskStatus::Done
            }
        )
    });
    assert_eq!(done_count, 3);
    // Should have hint
    assert_line_contains(&lines, "aiki task show");
}

/// State 1.9: Parent task — subtask failed
#[test]
fn flow1_state_9_parent_subtask_failed() {
    let mut graph = make_graph();

    let mut parent = make_task("parent1", "Fix review issues", TaskStatus::Stopped);
    parent.started_at = Some(Utc::now() - Duration::minutes(2));
    parent.stopped_reason = Some("1 subtask failed".to_string());
    graph.tasks.insert("parent1".to_string(), parent);

    let mut s1 = make_task("sub1", "Fix null check", TaskStatus::Closed);
    s1.started_at = Some(Utc::now() - Duration::seconds(90));
    s1.closed_at = Some(Utc::now() - Duration::seconds(30));
    graph.tasks.insert("sub1".to_string(), s1);
    graph.edges.add("sub1", "parent1", "subtask-of");

    let mut s2 = make_task("sub2", "Add error handling", TaskStatus::Stopped);
    s2.started_at = Some(Utc::now() - Duration::seconds(82));
    s2.stopped_reason = Some("compilation error".to_string());
    graph.tasks.insert("sub2".to_string(), s2);
    graph.edges.add("sub2", "parent1", "subtask-of");

    let mut s3 = make_task("sub3", "Remove unused import", TaskStatus::Closed);
    s3.started_at = Some(Utc::now() - Duration::seconds(30));
    s3.closed_at = Some(Utc::now() - Duration::seconds(6));
    graph.tasks.insert("sub3".to_string(), s3);
    graph.edges.add("sub3", "parent1", "subtask-of");

    let lines = aiki::tui::screens::task_run::view(&graph, "parent1", &window());

    // Should show failed header
    assert_line_contains(&lines, "task failed");
    assert!(matches!(lines[0].style, LineStyle::PhaseHeaderFailed));
    // Should have one failed subtask
    assert_eq!(
        count_lines(&lines, |l| matches!(
            l.style,
            LineStyle::Subtask {
                status: SubtaskStatus::Failed
            }
        )),
        1
    );
    // Should have two done subtasks
    assert_eq!(
        count_lines(&lines, |l| matches!(
            l.style,
            LineStyle::Subtask {
                status: SubtaskStatus::Done
            }
        )),
        2
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Flow 2: `aiki build <plan>` — build without review
// ═══════════════════════════════════════════════════════════════════════

/// State 2.1: Plan loaded (epic exists but no decompose yet)
#[test]
fn flow2_state_1_plan_loaded() {
    let mut graph = make_graph();
    let epic = make_task("epic1", "Epic: Mutex", TaskStatus::InProgress);
    graph.tasks.insert("epic1".to_string(), epic);

    let lines = aiki::tui::screens::build::view(&graph, "epic1", "ops/now/plan.md", &window());

    // Should have plan phase (always first)
    assert_line_contains(&lines, "plan");
    assert_line_contains(&lines, "plan.md");
    // Should have section header
    assert_line_contains(&lines, "Initial Build");
    // Should NOT have decompose phase yet
    assert_no_line_contains(&lines, "decompose");
}

/// State 2.4: Decompose active
#[test]
fn flow2_state_4_decompose_active() {
    let mut graph = make_graph();
    let epic = make_task("epic1", "Epic: Mutex", TaskStatus::InProgress);
    graph.tasks.insert("epic1".to_string(), epic);

    let mut decompose = make_task("decomp1", "Decompose plan", TaskStatus::InProgress);
    decompose.task_type = Some("decompose".to_string());
    decompose.started_at = Some(Utc::now() - Duration::seconds(32));
    decompose
        .data
        .insert("agent_type".to_string(), "claude-code".to_string());
    decompose.comments.push(TaskComment {
        id: Some("hb1".into()),
        text: "Reading plan and creating subtasks...".into(),
        timestamp: Utc::now(),
        data: {
            let mut m = HashMap::new();
            m.insert("type".to_string(), "heartbeat".to_string());
            m
        },
    });
    graph.tasks.insert("decomp1".to_string(), decompose);
    graph.edges.add("decomp1", "epic1", "subtask-of");

    let mut lines = aiki::tui::screens::build::view(&graph, "epic1", "ops/now/plan.md", &window());

    // Plan phase should be dimmed (earlier phase)
    apply_dimming(&mut lines);
    assert!(
        lines[0].dimmed,
        "Plan phase should be dimmed when decompose is active"
    );

    // Decompose should be active
    assert_line_contains(&lines, "decompose");
    let decompose_header = lines.iter().find(|l| l.text.contains("decompose")).unwrap();
    assert!(
        matches!(
            decompose_header.style,
            LineStyle::PhaseHeader { active: true }
        ),
        "Decompose header should be active"
    );
}

/// State 2.5: Decompose done, subtasks populated
#[test]
fn flow2_state_5_decompose_done_subtasks() {
    let mut graph = make_graph();
    let epic = make_task("epic1", "Epic: Mutex", TaskStatus::InProgress);
    graph.tasks.insert("epic1".to_string(), epic);

    let mut decompose = make_task("decomp1", "Decompose plan", TaskStatus::Closed);
    decompose.task_type = Some("decompose".to_string());
    decompose.started_at = Some(Utc::now() - Duration::seconds(64));
    decompose.closed_at = Some(Utc::now());
    graph.tasks.insert("decomp1".to_string(), decompose);
    graph.edges.add("decomp1", "epic1", "subtask-of");

    // Add work subtasks
    for (i, name) in ["Add helper", "Lock writes", "Delete bookmark"]
        .iter()
        .enumerate()
    {
        let s = make_task(&format!("work{}", i), name, TaskStatus::Open);
        graph.tasks.insert(format!("work{}", i), s);
        graph
            .edges
            .add(&format!("work{}", i), "epic1", "subtask-of");
    }

    let lines = aiki::tui::screens::build::view(&graph, "epic1", "ops/now/plan.md", &window());

    // Decompose should show as done
    assert_line_contains(&lines, "decompose");
    assert_line_contains(&lines, "3 subtasks created");
    // Subtask table should be present
    assert!(lines
        .iter()
        .any(|l| matches!(l.style, LineStyle::SubtaskHeader)));
    assert!(lines
        .iter()
        .any(|l| matches!(l.style, LineStyle::Separator)));
}

/// State 2.8: Mid-build — subtasks in various states
#[test]
fn flow2_state_8_mid_build() {
    let mut graph = make_graph();
    let mut epic = make_task("epic1", "Epic: Mutex", TaskStatus::InProgress);
    epic.started_at = Some(Utc::now() - Duration::minutes(3));
    graph.tasks.insert("epic1".to_string(), epic);

    // Decompose done
    let mut decompose = make_task("decomp1", "Decompose", TaskStatus::Closed);
    decompose.task_type = Some("decompose".to_string());
    decompose.started_at = Some(Utc::now() - Duration::minutes(2));
    decompose.closed_at = Some(Utc::now() - Duration::seconds(90));
    graph.tasks.insert("decomp1".to_string(), decompose);
    graph.edges.add("decomp1", "epic1", "subtask-of");

    // Work subtasks: 1 done, 1 active, 2 pending
    let mut w1 = make_task("w1", "Add helper", TaskStatus::Closed);
    w1.started_at = Some(Utc::now() - Duration::seconds(120));
    w1.closed_at = Some(Utc::now() - Duration::seconds(64));
    graph.tasks.insert("w1".to_string(), w1);
    graph.edges.add("w1", "epic1", "subtask-of");

    let mut w2 = make_task("w2", "Lock writes", TaskStatus::InProgress);
    w2.started_at = Some(Utc::now() - Duration::seconds(28));
    w2.claimed_by_session = Some("sess1".to_string());
    w2.data
        .insert("agent_type".to_string(), "claude-code".to_string());
    graph.tasks.insert("w2".to_string(), w2);
    graph.edges.add("w2", "epic1", "subtask-of");

    let w3 = make_task("w3", "Lock conversations", TaskStatus::Open);
    graph.tasks.insert("w3".to_string(), w3);
    graph.edges.add("w3", "epic1", "subtask-of");

    let w4 = make_task("w4", "Delete bookmark", TaskStatus::Open);
    graph.tasks.insert("w4".to_string(), w4);
    graph.edges.add("w4", "epic1", "subtask-of");

    let lines = aiki::tui::screens::build::view(&graph, "epic1", "ops/now/plan.md", &window());

    // Subtask table should show mixed states
    assert_eq!(
        count_lines(&lines, |l| matches!(
            l.style,
            LineStyle::Subtask {
                status: SubtaskStatus::Done
            }
        )),
        1,
        "Should have 1 done subtask"
    );
    assert_eq!(
        count_lines(&lines, |l| matches!(
            l.style,
            LineStyle::Subtask {
                status: SubtaskStatus::Active
            }
        )),
        1,
        "Should have 1 active subtask"
    );
    // Pending subtasks (either Pending or PendingUnassigned)
    assert!(
        count_lines(&lines, |l| matches!(
            l.style,
            LineStyle::Subtask {
                status: SubtaskStatus::Pending | SubtaskStatus::PendingUnassigned
            }
        )) >= 2,
        "Should have at least 2 pending subtasks"
    );
}

/// State 2.10: Build complete
#[test]
fn flow2_state_10_build_complete() {
    let mut graph = make_graph();
    let mut epic = make_task("epic1", "Epic: Mutex", TaskStatus::Closed);
    epic.started_at = Some(Utc::now() - Duration::minutes(55));
    epic.closed_at = Some(Utc::now());
    graph.tasks.insert("epic1".to_string(), epic);

    let lines = aiki::tui::screens::build::view(&graph, "epic1", "ops/now/plan.md", &window());

    // Should have summary
    assert_line_contains(&lines, "build completed");
    assert_line_contains(&lines, "plan.md");
    // Should have hint
    assert_line_contains(&lines, "aiki task diff");
}

// ═══════════════════════════════════════════════════════════════════════
// Flow 2 progressive dimming
// ═══════════════════════════════════════════════════════════════════════

/// When decompose is active, plan phase should be dimmed
#[test]
fn flow2_dimming_plan_dimmed_during_decompose() {
    let mut graph = make_graph();
    let epic = make_task("epic1", "Epic: Mutex", TaskStatus::InProgress);
    graph.tasks.insert("epic1".to_string(), epic);

    let mut decompose = make_task("decomp1", "Decompose", TaskStatus::InProgress);
    decompose.task_type = Some("decompose".to_string());
    decompose.started_at = Some(Utc::now());
    graph.tasks.insert("decomp1".to_string(), decompose);
    graph.edges.add("decomp1", "epic1", "subtask-of");

    let mut lines = aiki::tui::screens::build::view(&graph, "epic1", "ops/now/plan.md", &window());
    apply_dimming(&mut lines);

    // Plan phase (group 0) should be dimmed
    let plan_lines: Vec<_> = lines.iter().filter(|l| l.group == 0).collect();
    assert!(!plan_lines.is_empty(), "Should have plan phase lines");
    assert!(
        plan_lines.iter().all(|l| l.dimmed),
        "All plan lines should be dimmed"
    );

    // Decompose phase should NOT be dimmed
    let decompose_lines: Vec<_> = lines
        .iter()
        .filter(|l| {
            l.text.contains("decompose")
                || (l.group > 0 && !matches!(l.style, LineStyle::SectionHeader | LineStyle::Blank))
        })
        .collect();
    // At least the decompose header should not be dimmed
    let header = lines.iter().find(|l| l.text.contains("decompose"));
    if let Some(h) = header {
        assert!(!h.dimmed, "Active decompose header should not be dimmed");
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Flow 3: `aiki build <plan> -f` — build with review + fix
// ═══════════════════════════════════════════════════════════════════════

/// State 3.13: Review finds issues, fix cycle starts
#[test]
fn flow3_review_with_issues() {
    let mut graph = make_graph();
    let mut epic = make_task("epic1", "Epic: Mutex", TaskStatus::InProgress);
    epic.started_at = Some(Utc::now() - Duration::minutes(50));
    graph.tasks.insert("epic1".to_string(), epic);

    // Completed review with issues
    let mut review = make_task("review1", "Review", TaskStatus::Closed);
    review.task_type = Some("review".to_string());
    review.started_at = Some(Utc::now() - Duration::seconds(90));
    review.closed_at = Some(Utc::now());
    review
        .data
        .insert("agent_type".to_string(), "codex".to_string());
    review.comments.push(TaskComment {
        id: Some("i1".into()),
        text: "acquire_named_lock uses wrong error variant".into(),
        timestamp: Utc::now(),
        data: {
            let mut m = HashMap::new();
            m.insert("type".to_string(), "issue".to_string());
            m.insert("severity".to_string(), "high".to_string());
            m
        },
    });
    review.comments.push(TaskComment {
        id: Some("i2".into()),
        text: "fs::create_dir_all error silently swallowed".into(),
        timestamp: Utc::now(),
        data: {
            let mut m = HashMap::new();
            m.insert("type".to_string(), "issue".to_string());
            m.insert("severity".to_string(), "medium".to_string());
            m
        },
    });
    graph.tasks.insert("review1".to_string(), review);
    graph.edges.add("review1", "epic1", "subtask-of");

    let lines = aiki::tui::screens::build::view(&graph, "epic1", "ops/now/plan.md", &window());

    // Review phase should show issues
    assert_line_contains(&lines, "review");
    assert_line_contains(&lines, "Found 2 issues");
    // Issue text should be in the output
    assert_line_contains(&lines, "acquire_named_lock");
    assert_line_contains(&lines, "create_dir_all");
    // Issues should use Issue style
    assert!(lines.iter().any(|l| matches!(l.style, LineStyle::Issue)));
}

/// State 3.12: Review approved — no issues
#[test]
fn flow3_review_approved() {
    let mut graph = make_graph();
    let mut epic = make_task("epic1", "Epic: Mutex", TaskStatus::InProgress);
    epic.started_at = Some(Utc::now() - Duration::minutes(50));
    graph.tasks.insert("epic1".to_string(), epic);

    // Completed review with no issues
    let mut review = make_task("review1", "Review", TaskStatus::Closed);
    review.task_type = Some("review".to_string());
    review.started_at = Some(Utc::now() - Duration::minutes(11));
    review.closed_at = Some(Utc::now());
    review
        .data
        .insert("agent_type".to_string(), "codex".to_string());
    graph.tasks.insert("review1".to_string(), review);
    graph.edges.add("review1", "epic1", "subtask-of");

    let lines = aiki::tui::screens::build::view(&graph, "epic1", "ops/now/plan.md", &window());

    // Should show approved
    assert_line_contains(&lines, "approved");
    // Should NOT show issue list
    assert!(
        !lines.iter().any(|l| matches!(l.style, LineStyle::Issue)),
        "Should have no issue lines when approved"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Build TUI should NOT show intermediate output
// ═══════════════════════════════════════════════════════════════════════

/// The build view should never show task_run loading output.
/// The build screen renders decompose as a phase — it should not
/// fall through to task_run's loading_lines().
#[test]
fn build_view_never_shows_task_run_loading() {
    // Empty graph (no epic yet) — simulates the moment TUI starts
    // before JJ has propagated the epic task
    let graph = make_graph();
    let lines =
        aiki::tui::screens::build::view(&graph, "nonexistent", "ops/now/plan.md", &window());

    // Should still render plan phase (always present)
    assert_line_contains(&lines, "plan");
    // Should NOT show "Reading task graph..." (that's task_run's loading state)
    assert_no_line_contains(&lines, "Reading task graph");
}

/// Build view always starts with plan phase, never with a spinner-only loading state
#[test]
fn build_view_starts_with_plan_phase() {
    let mut graph = make_graph();
    let epic = make_task("epic1", "Epic: Mutex", TaskStatus::Open);
    graph.tasks.insert("epic1".to_string(), epic);

    let lines = aiki::tui::screens::build::view(&graph, "epic1", "ops/now/plan.md", &window());

    // First line should be the plan phase header
    assert!(
        lines[0].text.contains("plan"),
        "First line should be plan phase"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Review screen (Flow 4-7)
// ═══════════════════════════════════════════════════════════════════════

/// Review screen: active review
#[test]
fn review_screen_active() {
    let mut graph = make_graph();
    let mut review = make_task("review1", "Review", TaskStatus::InProgress);
    review
        .data
        .insert("agent_type".to_string(), "codex".to_string());
    review.started_at = Some(Utc::now() - Duration::seconds(15));
    review.comments.push(TaskComment {
        id: Some("hb1".into()),
        text: "Reviewing plan...".into(),
        timestamp: Utc::now(),
        data: {
            let mut m = HashMap::new();
            m.insert("type".to_string(), "heartbeat".to_string());
            m
        },
    });
    graph.tasks.insert("review1".to_string(), review);

    let lines = aiki::tui::screens::review::view(&graph, "review1", "ops/now/plan.md", &window());

    // Should show active review
    assert_line_contains(&lines, "review");
    assert_line_contains(&lines, "Reviewing plan");
}

/// Review screen: done with issues
#[test]
fn review_screen_done_with_issues() {
    let mut graph = make_graph();
    let mut review = make_task("review1", "Review", TaskStatus::Closed);
    review
        .data
        .insert("agent_type".to_string(), "codex".to_string());
    review.started_at = Some(Utc::now() - Duration::seconds(90));
    review.closed_at = Some(Utc::now());
    review.comments.push(TaskComment {
        id: Some("i1".into()),
        text: "get_repo_root shells out to jj root on every call".into(),
        timestamp: Utc::now(),
        data: {
            let mut m = HashMap::new();
            m.insert("type".to_string(), "issue".to_string());
            m.insert("severity".to_string(), "high".to_string());
            m
        },
    });
    graph.tasks.insert("review1".to_string(), review);

    let lines = aiki::tui::screens::review::view(&graph, "review1", "task:epic123", &window());

    assert_line_contains(&lines, "Found 1 issue");
    assert_line_contains(&lines, "get_repo_root");
    assert!(lines.iter().any(|l| matches!(l.style, LineStyle::Issue)));
}

/// Review screen: approved (no issues)
#[test]
fn review_screen_approved() {
    let mut graph = make_graph();
    let mut review = make_task("review1", "Review", TaskStatus::Closed);
    review
        .data
        .insert("agent_type".to_string(), "codex".to_string());
    review.started_at = Some(Utc::now() - Duration::seconds(45));
    review.closed_at = Some(Utc::now());
    graph.tasks.insert("review1".to_string(), review);

    let lines = aiki::tui::screens::review::view(&graph, "review1", "task:epic123", &window());

    assert_line_contains(&lines, "approved");
    assert!(!lines.iter().any(|l| matches!(l.style, LineStyle::Issue)));
}

// ═══════════════════════════════════════════════════════════════════════
// Fix screen (Flow 8)
// ═══════════════════════════════════════════════════════════════════════

/// Fix screen: fix plan active
#[test]
fn fix_screen_fix_plan_active() {
    let mut graph = make_graph();
    let mut fix_parent = make_task("fix1", "Fix issues", TaskStatus::InProgress);
    fix_parent.task_type = Some("fix".to_string());
    fix_parent.started_at = Some(Utc::now() - Duration::seconds(8));
    graph.tasks.insert("fix1".to_string(), fix_parent);

    let lines = aiki::tui::screens::fix::view(&graph, "fix1", "review1", &window());

    // Should render something for the fix screen
    assert!(!lines.is_empty(), "Fix screen should produce output");
}
