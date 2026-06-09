//! Visual verification tests for TUI iteration-2 fixes.
//!
//! Each test constructs a specific screen state, renders it through the full
//! pipeline (view → apply_dimming → render_to_string), and prints the ANSI
//! output for human inspection. Tests also make structural assertions.
//!
//! Run with:  cargo test --test tui_visual_verify -- --nocapture

use aiki::tasks::graph::EdgeStore;
use aiki::tasks::types::{FastHashMap, TaskComment, TaskOutcome, TaskPriority, TaskStatus};
use aiki::tasks::{Task, TaskGraph};
use aiki::tui::app::{Line, LineStyle, SubtaskStatus, WindowState};
use aiki::tui::components::{self, ChildLine, LaneData, SubtaskData};
use aiki::tui::render::{apply_dimming, render_to_string};
use aiki::tui::theme::{self, Theme};
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

fn print_section(title: &str) {
    println!("\n{}", "=".repeat(80));
    println!("  {}", title);
    println!("{}\n", "=".repeat(80));
}

fn render_and_print(title: &str, lines: &mut Vec<Line>) {
    let theme = Theme::dark();
    let output = render_to_string(lines, &theme);
    println!("--- {} ---", title);
    println!("{}", output);
    println!("--- end ---\n");
}

// ═══════════════════════════════════════════════════════════════════════
// Fix 1: Subtask table header — [id] Title (not 合 id Title)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn fix1_subtask_header_renders_bracket_format() {
    print_section("Fix 1: Subtask Header — [id] Title");

    let subtasks = vec![
        SubtaskData {
            name: "Add get_repo_root helper to jj/mod.rs".into(),
            status: SubtaskStatus::Done,
            elapsed: Some("56s".into()),
        },
        SubtaskData {
            name: "Lock task writes in tasks/storage.rs".into(),
            status: SubtaskStatus::Active,
            elapsed: Some("28s".into()),
        },
        SubtaskData {
            name: "Delete advance_bookmark from jj/mod.rs".into(),
            status: SubtaskStatus::Pending,
            elapsed: None,
        },
    ];

    let mut lines =
        components::subtask_table(0, "lkji3d", "Epic: Mutex for Task Writes", &subtasks, false);
    render_and_print("Subtask table with [id] header", &mut lines);

    // Structural assertion: header line uses SubtaskHeader, not PhaseHeader
    let header = lines.iter().find(|l| l.text.contains("lkji3d")).unwrap();
    assert!(
        matches!(header.style, LineStyle::SubtaskHeader),
        "Header should use SubtaskHeader style"
    );
    assert!(
        header.text.starts_with('['),
        "Header text should start with '[', got: {}",
        header.text
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Fix 2: Subtask text color varies by status
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn fix2_subtask_text_color_per_status() {
    print_section("Fix 2: Subtask Text Color Per Status");

    let subtasks = vec![
        SubtaskData {
            name: "Done task — text should be DIM".into(),
            status: SubtaskStatus::Done,
            elapsed: Some("56s".into()),
        },
        SubtaskData {
            name: "Active task — text should be FG".into(),
            status: SubtaskStatus::Active,
            elapsed: Some("28s".into()),
        },
        SubtaskData {
            name: "Pending task — text should be DIM".into(),
            status: SubtaskStatus::Pending,
            elapsed: None,
        },
        SubtaskData {
            name: "Failed task — text should be RED".into(),
            status: SubtaskStatus::Failed,
            elapsed: Some("1m29".into()),
        },
    ];

    let mut lines = components::subtask_table(0, "abc123", "Color Test", &subtasks, false);
    render_and_print(
        "Subtask colors: done=dim, active=fg, pending=dim, failed=red",
        &mut lines,
    );

    // Verify the Subtask lines have correct status variants
    let subtask_lines: Vec<_> = lines
        .iter()
        .filter(|l| matches!(l.style, LineStyle::Subtask { .. }))
        .collect();
    assert_eq!(subtask_lines.len(), 4);
    assert!(matches!(
        subtask_lines[0].style,
        LineStyle::Subtask {
            status: SubtaskStatus::Done
        }
    ));
    assert!(matches!(
        subtask_lines[1].style,
        LineStyle::Subtask {
            status: SubtaskStatus::Active
        }
    ));
    assert!(matches!(
        subtask_lines[2].style,
        LineStyle::Subtask {
            status: SubtaskStatus::Pending
        }
    ));
    assert!(matches!(
        subtask_lines[3].style,
        LineStyle::Subtask {
            status: SubtaskStatus::Failed
        }
    ));
}

// ═══════════════════════════════════════════════════════════════════════
// Fix 3: Lane block — hierarchical with ⎿ children
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn fix3_lane_block_hierarchical_structure() {
    print_section("Fix 3: Lane Block — Hierarchical ⎿ Children");

    // Active lane with heartbeat
    let lanes = vec![
        LaneData {
            number: 1,
            agent: "claude".into(),
            completed: 1,
            total: 2,
            failed: 0,
            heartbeat: Some("Writing lock function in storage.rs".into()),
            elapsed: Some("28s".into()),
            shutdown: false,
        },
        LaneData {
            number: 2,
            agent: "claude".into(),
            completed: 0,
            total: 3,
            failed: 0,
            heartbeat: None,
            elapsed: Some("3s".into()),
            shutdown: false,
        },
    ];
    let mut lines = components::loop_block(5, &lanes);
    render_and_print("Active loop with 2 lanes", &mut lines);

    // Verify structure
    assert!(matches!(
        lines[0].style,
        LineStyle::PhaseHeader { active: true }
    ));
    assert_eq!(lines[0].text, "loop");

    // Lane 1 header at indent 1
    assert_eq!(lines[1].text, "Lane 1 (claude)");
    assert_eq!(lines[1].indent, 1);
    assert!(matches!(lines[1].style, LineStyle::Child));

    // Lane 1 progress at indent 2
    assert_eq!(lines[2].text, "1/2 subtasks completed");
    assert_eq!(lines[2].indent, 2);

    // Lane 1 heartbeat at indent 2
    assert_eq!(lines[3].text, "Writing lock function in storage.rs");
    assert!(matches!(lines[3].style, LineStyle::ChildActive));

    // Blank between lanes
    assert!(matches!(lines[4].style, LineStyle::Blank));

    // Lane 2 header
    assert_eq!(lines[5].text, "Lane 2 (claude)");

    // Lane 2 starting session
    assert_eq!(lines[7].text, "starting session...");

    // Done lane version
    println!();
    let done_lanes = vec![LaneData {
        number: 1,
        agent: "claude".into(),
        completed: 2,
        total: 2,
        failed: 0,
        heartbeat: None,
        elapsed: None,
        shutdown: true,
    }];
    let mut done_lines = components::loop_block(5, &done_lanes);
    render_and_print("Done loop (shutdown lane)", &mut done_lines);

    assert!(matches!(
        done_lines[0].style,
        LineStyle::PhaseHeader { active: false }
    ));
    assert_eq!(done_lines[3].text, "Agent shutdown.");
}

#[test]
fn fix3_lane_block_with_failures() {
    print_section("Fix 3: Lane Block — With Failed Tasks");

    let lanes = vec![LaneData {
        number: 1,
        agent: "claude".into(),
        completed: 1,
        total: 3,
        failed: 2,
        heartbeat: Some("Retrying failed task...".into()),
        elapsed: Some("45s".into()),
        shutdown: false,
    }];
    let mut lines = components::loop_block(5, &lanes);
    render_and_print("Lane with failures (error line)", &mut lines);

    // Should have error line
    let error_line = lines
        .iter()
        .find(|l| matches!(l.style, LineStyle::ChildError))
        .expect("Should have an error line");
    assert!(error_line.text.contains("2 tasks failed"));

    // Progress should mention failures
    let progress = &lines[2];
    assert!(progress.text.contains("2 failed"));
}

// ═══════════════════════════════════════════════════════════════════════
// Fix 4: ◌ PendingUnassigned vs ○ Pending
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn fix4_pending_unassigned_symbol() {
    print_section("Fix 4: ◌ PendingUnassigned vs ○ Pending");

    let subtasks = vec![
        SubtaskData {
            name: "In active lane (should show ○)".into(),
            status: SubtaskStatus::Pending,
            elapsed: None,
        },
        SubtaskData {
            name: "Not in any lane (should show ◌)".into(),
            status: SubtaskStatus::PendingUnassigned,
            elapsed: None,
        },
        SubtaskData {
            name: "Assigned to session (should show ⧗)".into(),
            status: SubtaskStatus::Assigned,
            elapsed: None,
        },
    ];

    let mut lines = components::subtask_table(0, "test01", "Pending States", &subtasks, false);
    render_and_print("Three pending states: ○, ◌, ⧗", &mut lines);

    let subtask_lines: Vec<_> = lines
        .iter()
        .filter(|l| matches!(l.style, LineStyle::Subtask { .. }))
        .collect();
    assert!(matches!(
        subtask_lines[0].style,
        LineStyle::Subtask {
            status: SubtaskStatus::Pending
        }
    ));
    assert!(matches!(
        subtask_lines[1].style,
        LineStyle::Subtask {
            status: SubtaskStatus::PendingUnassigned
        }
    ));
    assert!(matches!(
        subtask_lines[2].style,
        LineStyle::Subtask {
            status: SubtaskStatus::Assigned
        }
    ));
}

// ═══════════════════════════════════════════════════════════════════════
// Fix 5: Blank lines between separators and content
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn fix5_subtask_table_blank_lines() {
    print_section("Fix 5: Blank Lines in Subtask Table");

    let subtasks = vec![SubtaskData {
        name: "Some work item".into(),
        status: SubtaskStatus::Active,
        elapsed: Some("12s".into()),
    }];

    let mut lines = components::subtask_table(0, "xyz789", "Blank Line Test", &subtasks, false);
    render_and_print("Table with blank lines after/before separators", &mut lines);

    // Verify structure: Separator → Blank → content... → Blank → Separator
    assert!(
        matches!(lines[0].style, LineStyle::Separator),
        "First line should be Separator"
    );
    assert!(
        matches!(lines[1].style, LineStyle::Blank),
        "Second line should be Blank"
    );
    let last = lines.len() - 1;
    assert!(
        matches!(lines[last].style, LineStyle::Separator),
        "Last line should be Separator"
    );
    assert!(
        matches!(lines[last - 1].style, LineStyle::Blank),
        "Second-to-last should be Blank"
    );

    println!(
        "Structure: {:?}",
        lines
            .iter()
            .map(|l| match l.style {
                LineStyle::Separator => "Sep",
                LineStyle::Blank => "Blank",
                LineStyle::SubtaskHeader => "Header",
                LineStyle::Subtask { .. } => "Subtask",
                LineStyle::Dim => "Dim",
                _ => "Other",
            })
            .collect::<Vec<_>>()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Fix 6: Heavy symbols ✔ and ✘ (not thin ✓ and ✗)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn fix6_heavy_symbols() {
    print_section("Fix 6: Heavy Symbols ✔ (U+2714) and ✘ (U+2718)");

    // Verify the actual constant values
    assert_eq!(
        theme::SYM_CHECK,
        "✔",
        "SYM_CHECK should be ✔ (U+2714 HEAVY CHECK MARK)"
    );
    assert_eq!(
        theme::SYM_FAILED,
        "✘",
        "SYM_FAILED should be ✘ (U+2718 HEAVY BALLOT X)"
    );

    // Verify they are NOT the thin variants
    assert_ne!(
        theme::SYM_CHECK,
        "✓",
        "SYM_CHECK should NOT be ✓ (U+2713 thin)"
    );
    assert_ne!(
        theme::SYM_FAILED,
        "✗",
        "SYM_FAILED should NOT be ✗ (U+2717 thin)"
    );

    // Verify Unicode codepoints explicitly
    assert_eq!(
        theme::SYM_CHECK.chars().next().unwrap() as u32,
        0x2714,
        "SYM_CHECK should be U+2714"
    );
    assert_eq!(
        theme::SYM_FAILED.chars().next().unwrap() as u32,
        0x2718,
        "SYM_FAILED should be U+2718"
    );

    // Show them in context
    let subtasks = vec![
        SubtaskData {
            name: "Completed work".into(),
            status: SubtaskStatus::Done,
            elapsed: Some("56s".into()),
        },
        SubtaskData {
            name: "Failed work".into(),
            status: SubtaskStatus::Failed,
            elapsed: Some("1m29".into()),
        },
    ];
    let mut lines = components::subtask_table(0, "sym001", "Symbol Weight Check", &subtasks, false);
    render_and_print("Heavy ✔ and ✘ in subtask table", &mut lines);

    println!(
        "SYM_CHECK = {:?} (U+{:04X})",
        theme::SYM_CHECK,
        theme::SYM_CHECK.chars().next().unwrap() as u32
    );
    println!(
        "SYM_FAILED = {:?} (U+{:04X})",
        theme::SYM_FAILED,
        theme::SYM_FAILED.chars().next().unwrap() as u32
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Fix 8: sum_agent_stats — raw values
// ═══════════════════════════════════════════════════════════════════════

// (This is tested in build.rs unit tests — sum_agent_stats is private.
// We verify the build summary renders correctly instead.)

#[test]
fn fix8_build_summary_renders_stats() {
    print_section("Fix 8: Build Summary With Agent Stats");

    // Create a completed build epic with children that have agent data
    let mut graph = make_graph();
    let mut epic = make_task(
        "epic123456789012345678901234",
        "Epic: Build app",
        TaskStatus::Closed,
    );
    epic.started_at = Some(Utc::now() - Duration::minutes(5));
    epic.closed_at = Some(Utc::now());

    graph.tasks.insert(epic.id.clone(), epic);

    let window = WindowState::new(80);
    let mut lines = aiki::tui::screens::build::view(
        &graph,
        "epic123456789012345678901234",
        "ops/now/plan.md",
        &window,
    );
    render_and_print("Build complete summary", &mut lines);

    // Should contain the summary phase header
    assert!(
        lines.iter().any(|l| l.text.contains("build completed")),
        "Should have 'build completed' in output"
    );
    // Should contain the hint
    assert!(
        lines.iter().any(|l| l.text.contains("aiki task diff")),
        "Should have diff hint"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Fix 9: Filter non-work subtasks
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn fix9_work_subtasks_exclude_infra_tasks() {
    print_section("Fix 9: get_work_subtasks Filters Non-Work Tasks");

    let mut graph = make_graph();
    let epic = make_task(
        "epic123456789012345678901234",
        "Epic: Build app",
        TaskStatus::InProgress,
    );
    let work1 = make_task(
        "work1_23456789012345678901234",
        "Implement feature A",
        TaskStatus::InProgress,
    );
    let work2 = make_task(
        "work2_23456789012345678901234",
        "Implement feature B",
        TaskStatus::Open,
    );
    let mut decompose = make_task(
        "decomp123456789012345678901234",
        "Decompose plan",
        TaskStatus::Closed,
    );
    decompose.task_type = Some("decompose".to_string());
    let mut review = make_task(
        "review123456789012345678901234",
        "Review changes",
        TaskStatus::Open,
    );
    review.task_type = Some("review".to_string());
    let mut fix = make_task(
        "fix12345678901234567890123456",
        "Fix issues",
        TaskStatus::Open,
    );
    fix.task_type = Some("fix".to_string());

    graph.tasks.insert(epic.id.clone(), epic);
    graph.tasks.insert(work1.id.clone(), work1);
    graph.tasks.insert(work2.id.clone(), work2);
    graph.tasks.insert(decompose.id.clone(), decompose);
    graph.tasks.insert(review.id.clone(), review);
    graph.tasks.insert(fix.id.clone(), fix);

    graph.edges.add(
        "work1_23456789012345678901234",
        "epic123456789012345678901234",
        "subtask-of",
    );
    graph.edges.add(
        "work2_23456789012345678901234",
        "epic123456789012345678901234",
        "subtask-of",
    );
    graph.edges.add(
        "decomp123456789012345678901234",
        "epic123456789012345678901234",
        "subtask-of",
    );
    graph.edges.add(
        "review123456789012345678901234",
        "epic123456789012345678901234",
        "subtask-of",
    );
    graph.edges.add(
        "fix12345678901234567890123456",
        "epic123456789012345678901234",
        "subtask-of",
    );

    let all = aiki::tui::screens::helpers::get_subtasks(&graph, "epic123456789012345678901234");
    let work =
        aiki::tui::screens::helpers::get_work_subtasks(&graph, "epic123456789012345678901234");

    println!(
        "All subtasks: {} (includes decompose, review, fix)",
        all.len()
    );
    println!("Work subtasks: {} (only actual work items)", work.len());
    for t in &work {
        println!("  - {} (type: {:?})", t.name, t.task_type);
    }

    assert_eq!(all.len(), 5, "All children should be 5");
    assert_eq!(work.len(), 2, "Work subtasks should be 2");
    assert!(
        work.iter().all(|t| t.task_type.is_none()),
        "Work subtasks should have no special task_type"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Full composite: Build screen with all fixes visible
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn composite_build_screen_mid_loop() {
    print_section("COMPOSITE: Build screen during loop with subtask table");

    // Build a graph that shows: plan (done) → decompose (done) → subtask table → loop (active)
    let mut graph = make_graph();
    let mut epic = make_task(
        "epic_composite_45678901234567",
        "Epic: Mutex for Task Writes",
        TaskStatus::InProgress,
    );
    epic.started_at = Some(Utc::now() - Duration::minutes(3));
    graph.tasks.insert(epic.id.clone(), epic);

    // Decompose (done)
    let mut decompose = make_task(
        "decomp_comp_456789012345678",
        "Decompose plan",
        TaskStatus::Closed,
    );
    decompose.task_type = Some("decompose".to_string());
    decompose.started_at = Some(Utc::now() - Duration::minutes(2));
    decompose.closed_at = Some(Utc::now() - Duration::seconds(90));
    decompose
        .data
        .insert("agent_type".to_string(), "claude".to_string());
    graph.tasks.insert(decompose.id.clone(), decompose);
    graph.edges.add(
        "decomp_comp_456789012345678",
        "epic_composite_45678901234567",
        "subtask-of",
    );

    // Work subtasks (various states)
    let mut w1 = make_task(
        "w1_comp_567890123456789012345",
        "Add get_repo_root helper",
        TaskStatus::Closed,
    );
    w1.started_at = Some(Utc::now() - Duration::seconds(120));
    w1.closed_at = Some(Utc::now() - Duration::seconds(64));
    graph.tasks.insert(w1.id.clone(), w1);
    graph.edges.add(
        "w1_comp_567890123456789012345",
        "epic_composite_45678901234567",
        "subtask-of",
    );

    let mut w2 = make_task(
        "w2_comp_567890123456789012345",
        "Lock task writes in storage.rs",
        TaskStatus::InProgress,
    );
    w2.started_at = Some(Utc::now() - Duration::seconds(28));
    w2.data
        .insert("agent_type".to_string(), "claude".to_string());
    w2.comments.push(TaskComment {
        id: Some("hb1".into()),
        text: "Writing lock function...".into(),
        timestamp: Utc::now(),
        data: {
            let mut m = HashMap::new();
            m.insert("type".to_string(), "heartbeat".to_string());
            m
        },
    });
    graph.tasks.insert(w2.id.clone(), w2);
    graph.edges.add(
        "w2_comp_567890123456789012345",
        "epic_composite_45678901234567",
        "subtask-of",
    );

    let w3 = make_task(
        "w3_comp_567890123456789012345",
        "Lock conversation writes",
        TaskStatus::Open,
    );
    graph.tasks.insert(w3.id.clone(), w3);
    graph.edges.add(
        "w3_comp_567890123456789012345",
        "epic_composite_45678901234567",
        "subtask-of",
    );

    let w4 = make_task(
        "w4_comp_567890123456789012345",
        "Delete advance_bookmark",
        TaskStatus::Open,
    );
    graph.tasks.insert(w4.id.clone(), w4);
    graph.edges.add(
        "w4_comp_567890123456789012345",
        "epic_composite_45678901234567",
        "subtask-of",
    );

    let window = WindowState::new(80);
    let mut lines = aiki::tui::screens::build::view(
        &graph,
        "epic_composite_45678901234567",
        "ops/now/tasks/mutex-for-task-writes.md",
        &window,
    );
    render_and_print("Full build screen mid-loop", &mut lines);

    // Key assertions
    assert!(lines.iter().any(|l| l.text.contains("plan")));
    assert!(lines.iter().any(|l| l.text.contains("Initial Build")));
    assert!(lines.iter().any(|l| l.text.contains("decompose")));
    // Subtask table should exist
    assert!(lines
        .iter()
        .any(|l| matches!(l.style, LineStyle::Separator)));
    assert!(lines
        .iter()
        .any(|l| matches!(l.style, LineStyle::SubtaskHeader)));
}

// ═══════════════════════════════════════════════════════════════════════
// Full composite: task run with subtasks (parent task)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn composite_task_run_parent_with_subtasks() {
    print_section("COMPOSITE: task run — parent with subtasks in progress");

    let mut graph = make_graph();
    let mut parent = make_task(
        "parent_run_67890123456789012",
        "Fix review issues",
        TaskStatus::InProgress,
    );
    parent.started_at = Some(Utc::now() - Duration::seconds(45));
    parent
        .data
        .insert("agent_type".to_string(), "claude".to_string());
    parent.comments.push(TaskComment {
        id: Some("hb1".into()),
        text: "Working on subtask 1...".into(),
        timestamp: Utc::now(),
        data: {
            let mut m = HashMap::new();
            m.insert("type".to_string(), "heartbeat".to_string());
            m
        },
    });
    graph.tasks.insert(parent.id.clone(), parent);

    let mut s1 = make_task(
        "sub1_run_678901234567890123456",
        "Fix null check in auth handler",
        TaskStatus::InProgress,
    );
    s1.started_at = Some(Utc::now() - Duration::seconds(32));
    graph.tasks.insert(s1.id.clone(), s1);
    graph.edges.add(
        "sub1_run_678901234567890123456",
        "parent_run_67890123456789012",
        "subtask-of",
    );

    let s2 = make_task(
        "sub2_run_678901234567890123456",
        "Add missing error handling in API client",
        TaskStatus::Open,
    );
    graph.tasks.insert(s2.id.clone(), s2);
    graph.edges.add(
        "sub2_run_678901234567890123456",
        "parent_run_67890123456789012",
        "subtask-of",
    );

    let s3 = make_task(
        "sub3_run_678901234567890123456",
        "Remove unused import in utils.rs",
        TaskStatus::Open,
    );
    graph.tasks.insert(s3.id.clone(), s3);
    graph.edges.add(
        "sub3_run_678901234567890123456",
        "parent_run_67890123456789012",
        "subtask-of",
    );

    let window = WindowState::new(80);
    let mut lines =
        aiki::tui::screens::task_run::view(&graph, "parent_run_67890123456789012", &window);
    render_and_print(
        "task run: parent with subtasks in progress (state 1.6)",
        &mut lines,
    );

    // Verify subtask table exists
    assert!(lines
        .iter()
        .any(|l| matches!(l.style, LineStyle::SubtaskHeader)));
    assert!(lines.iter().any(|l| matches!(
        l.style,
        LineStyle::Subtask {
            status: SubtaskStatus::Active
        }
    )));
}

// ═══════════════════════════════════════════════════════════════════════
// Progressive dimming: earlier phases dim when a later phase is active
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn progressive_dimming_works() {
    print_section("Progressive Dimming: Earlier phases dim");

    // Phase 0 (done) then Phase 1 (active)
    let mut lines = vec![];
    lines.extend(components::phase(
        0,
        "plan (claude)",
        None,
        false,
        vec![ChildLine::done("ops/now/plan.md", None)],
    ));
    lines.extend(components::phase(
        1,
        "decompose (claude)",
        None,
        true,
        vec![ChildLine::active_with_elapsed(
            "Reading plan...",
            Some("12s".into()),
        )],
    ));

    render_and_print("Before dimming", &mut lines);
    apply_dimming(&mut lines);
    render_and_print("After dimming (plan phase should be dim)", &mut lines);

    // Group 0 lines should be dimmed
    assert!(lines[0].dimmed, "Plan header should be dimmed");
    assert!(lines[1].dimmed, "Plan child should be dimmed");
    // Group 1 lines should NOT be dimmed
    assert!(!lines[2].dimmed, "Decompose header should not be dimmed");
    assert!(!lines[3].dimmed, "Decompose child should not be dimmed");
}

// ═══════════════════════════════════════════════════════════════════════
// Review screen with issues (verifies symbol usage in review.rs)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn review_screen_with_issues() {
    print_section("Review Screen: Issues + SYM_CHECK usage");

    let mut graph = make_graph();
    let mut review = make_task(
        "review_screen_789012345678901",
        "Review changes",
        TaskStatus::Closed,
    );
    review
        .data
        .insert("agent_type".to_string(), "codex".to_string());
    review.started_at = Some(Utc::now() - Duration::seconds(90));
    review.closed_at = Some(Utc::now());
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
    graph.tasks.insert(review.id.clone(), review);

    let window = WindowState::new(80);
    let mut lines = aiki::tui::screens::review::view(
        &graph,
        "review_screen_789012345678901",
        "task:epic123",
        &window,
    );
    render_and_print("Review screen with 2 issues", &mut lines);

    // Should have issue lines
    assert!(lines.iter().any(|l| matches!(l.style, LineStyle::Issue)));
    assert!(lines.iter().any(|l| l.text.contains("acquire_named_lock")));
}

#[test]
fn review_screen_approved() {
    print_section("Review Screen: Approved (uses SYM_CHECK)");

    let mut graph = make_graph();
    let mut review = make_task(
        "review_ok_67890123456789012345",
        "Review changes",
        TaskStatus::Closed,
    );
    review
        .data
        .insert("agent_type".to_string(), "claude".to_string());
    review.started_at = Some(Utc::now() - Duration::seconds(45));
    review.closed_at = Some(Utc::now());
    // No issue comments
    graph.tasks.insert(review.id.clone(), review);

    let window = WindowState::new(80);
    let mut lines = aiki::tui::screens::review::view(
        &graph,
        "review_ok_67890123456789012345",
        "task:epic123",
        &window,
    );
    render_and_print("Review approved (✔ approved)", &mut lines);

    // Should contain the heavy check mark
    assert!(
        lines.iter().any(|l| l.text.contains(theme::SYM_CHECK)),
        "Should use SYM_CHECK (✔) not thin ✓"
    );
}
