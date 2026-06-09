use aiki::tasks::{
    id::generate_task_id,
    storage::{ensure_tasks_branch, read_events, write_event},
    types::{TaskEvent, TaskOutcome, TaskPriority},
};
use chrono::Utc;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use tempfile::TempDir;

/// Setup a temporary JJ repository for benchmarking
fn setup_temp_repo() -> TempDir {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Initialize jj repo
    std::process::Command::new("jj")
        .current_dir(temp_dir.path())
        .args(["git", "init"])
        .output()
        .expect("Failed to init jj repo");

    // Initialize the tasks branch
    ensure_tasks_branch(temp_dir.path()).expect("Failed to ensure tasks branch");

    temp_dir
}

/// Benchmark creating a single task with generated ID
fn bench_create_task(c: &mut Criterion) {
    let mut group = c.benchmark_group("create_task");

    group.bench_function("generated_id", |b| {
        b.iter_batched(
            || setup_temp_repo(),
            |temp_dir| {
                let cwd = temp_dir.path();
                let task_id = generate_task_id(black_box("Benchmark task"));
                let event = TaskEvent::Created {
                    task_id,
                    name: black_box("Benchmark task".to_string()),
                    slug: None,
                    task_type: None,
                    priority: TaskPriority::P2,
                    assignee: None,
                    sources: Vec::new(),
                    template: None,
                    working_copy: None,
                    instructions: None,
                    data: std::collections::HashMap::new(),
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to create task");
            },
            criterion::BatchSize::LargeInput,
        );
    });

    group.finish();
}

/// Benchmark writing different types of events
fn bench_write_events(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_event");

    // Benchmark Created event
    group.bench_function("created", |b| {
        b.iter_batched(
            || setup_temp_repo(),
            |temp_dir| {
                let cwd = temp_dir.path();
                let event = TaskEvent::Created {
                    task_id: black_box("test123".to_string()),
                    name: black_box("Test task".to_string()),
                    slug: None,
                    task_type: None,
                    priority: TaskPriority::P2,
                    assignee: None,
                    sources: Vec::new(),
                    template: None,
                    working_copy: None,
                    instructions: None,
                    data: std::collections::HashMap::new(),
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to write event");
            },
            criterion::BatchSize::LargeInput,
        );
    });

    // Benchmark Started event
    group.bench_function("started", |b| {
        b.iter_batched(
            || {
                let temp_dir = setup_temp_repo();
                let cwd = temp_dir.path();

                // Pre-create a task
                let task_id = generate_task_id("Task to start");
                let event = TaskEvent::Created {
                    task_id: task_id.clone(),
                    name: "Task to start".to_string(),
                    slug: None,
                    task_type: None,
                    priority: TaskPriority::P2,
                    assignee: None,
                    sources: Vec::new(),
                    template: None,
                    working_copy: None,
                    instructions: None,
                    data: std::collections::HashMap::new(),
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to create task");

                (temp_dir, task_id)
            },
            |(temp_dir, task_id): (TempDir, String)| {
                let cwd = temp_dir.path();
                let event = TaskEvent::Started {
                    task_ids: black_box(vec![task_id]),
                    agent_type: black_box("claude-code".to_string()),
                    session_id: None,
                    turn_id: None,
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to write event");
            },
            criterion::BatchSize::LargeInput,
        );
    });

    // Benchmark Stopped event
    group.bench_function("stopped", |b| {
        b.iter_batched(
            || {
                let temp_dir = setup_temp_repo();
                let cwd = temp_dir.path();

                // Pre-create a task
                let task_id = generate_task_id("Task to stop");
                let event = TaskEvent::Created {
                    task_id: task_id.clone(),
                    name: "Task to stop".to_string(),
                    slug: None,
                    task_type: None,
                    priority: TaskPriority::P2,
                    assignee: None,
                    sources: Vec::new(),
                    template: None,
                    working_copy: None,
                    instructions: None,
                    data: std::collections::HashMap::new(),
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to create task");

                (temp_dir, task_id)
            },
            |(temp_dir, task_id): (TempDir, String)| {
                let cwd = temp_dir.path();
                let event = TaskEvent::Stopped {
                    task_ids: black_box(vec![task_id]),
                    reason: Some(black_box("Need input".to_string())),
                    turn_id: None,
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to write event");
            },
            criterion::BatchSize::LargeInput,
        );
    });

    // Benchmark Closed event
    group.bench_function("closed", |b| {
        b.iter_batched(
            || {
                let temp_dir = setup_temp_repo();
                let cwd = temp_dir.path();

                // Pre-create a task
                let task_id = generate_task_id("Task to close");
                let event = TaskEvent::Created {
                    task_id: task_id.clone(),
                    name: "Task to close".to_string(),
                    slug: None,
                    task_type: None,
                    priority: TaskPriority::P2,
                    assignee: None,
                    sources: Vec::new(),
                    template: None,
                    working_copy: None,
                    instructions: None,
                    data: std::collections::HashMap::new(),
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to create task");

                (temp_dir, task_id)
            },
            |(temp_dir, task_id): (TempDir, String)| {
                let cwd = temp_dir.path();
                let event = TaskEvent::Closed {
                    task_ids: black_box(vec![task_id]),
                    outcome: TaskOutcome::Done,
                    summary: None,
                    turn_id: None,
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to write event");
            },
            criterion::BatchSize::LargeInput,
        );
    });

    group.finish();
}

/// Benchmark reading events with varying numbers of tasks
fn bench_read_events(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_events");

    for num_tasks in [1, 5, 10, 25, 50].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_tasks),
            num_tasks,
            |b, &num_tasks| {
                let temp_dir = setup_temp_repo();
                let cwd = temp_dir.path();

                // Pre-create tasks
                for i in 0..num_tasks {
                    let task_id = generate_task_id(&format!("Task {}", i));
                    let event = TaskEvent::Created {
                        task_id,
                        name: format!("Task {}", i),
                        slug: None,
                        task_type: None,
                        priority: TaskPriority::P2,
                        assignee: None,
                        sources: Vec::new(),
                        template: None,
                        working_copy: None,
                        instructions: None,
                        data: std::collections::HashMap::new(),
                        timestamp: Utc::now(),
                    };
                    write_event(cwd, &event).expect("Failed to create task");
                }

                b.iter(|| {
                    read_events(black_box(cwd)).expect("Failed to read events");
                });
            },
        );
    }

    group.finish();
}

/// Benchmark sequential task creation (realistic workflow)
fn bench_sequential_tasks(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequential_tasks");

    for num_tasks in [5, 10, 25].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_tasks),
            num_tasks,
            |b, &num_tasks| {
                b.iter_batched(
                    || setup_temp_repo(),
                    |temp_dir| {
                        let cwd = temp_dir.path();
                        for i in 0..num_tasks {
                            let task_id = generate_task_id(&format!("Sequential task {}", i));
                            let event = TaskEvent::Created {
                                task_id,
                                name: black_box(format!("Sequential task {}", i)),
                                slug: None,
                                task_type: None,
                                priority: TaskPriority::P2,
                                assignee: None,
                                sources: Vec::new(),
                                template: None,
                                working_copy: None,
                                instructions: None,
                                data: std::collections::HashMap::new(),
                                timestamp: Utc::now(),
                            };
                            write_event(cwd, &event).expect("Failed to create task");
                        }
                    },
                    criterion::BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark task lifecycle (create -> start -> stop -> close)
fn bench_task_lifecycle(c: &mut Criterion) {
    c.bench_function("task_lifecycle", |b| {
        b.iter_batched(
            || setup_temp_repo(),
            |temp_dir| {
                let cwd = temp_dir.path();

                // Create task
                let task_id = generate_task_id(black_box("Lifecycle task"));
                let event = TaskEvent::Created {
                    task_id: task_id.clone(),
                    name: black_box("Lifecycle task".to_string()),
                    slug: None,
                    task_type: None,
                    priority: TaskPriority::P2,
                    assignee: None,
                    sources: Vec::new(),
                    template: None,
                    working_copy: None,
                    instructions: None,
                    data: std::collections::HashMap::new(),
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to create task");

                // Start task
                let event = TaskEvent::Started {
                    task_ids: vec![task_id.clone()],
                    agent_type: "claude-code".to_string(),
                    session_id: None,
                    turn_id: None,
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to start task");

                // Stop task
                let event = TaskEvent::Stopped {
                    task_ids: vec![task_id.clone()],
                    reason: Some("Paused".to_string()),
                    turn_id: None,
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to stop task");

                // Close task
                let event = TaskEvent::Closed {
                    task_ids: vec![task_id],
                    outcome: TaskOutcome::Done,
                    summary: None,
                    turn_id: None,
                    timestamp: Utc::now(),
                };
                write_event(cwd, &event).expect("Failed to close task");
            },
            criterion::BatchSize::LargeInput,
        );
    });
}

criterion_group!(
    benches,
    bench_create_task,
    bench_write_events,
    bench_read_events,
    bench_sequential_tasks,
    bench_task_lifecycle
);
criterion_main!(benches);
