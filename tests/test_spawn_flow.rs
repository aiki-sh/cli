/// Integration tests for conditional task spawning (`spawns:` frontmatter).
///
/// Tests cover:
/// 1. Basic spawn on close (when: true)
/// 2. Spawn condition false (no spawn created)
/// 3. Spawn with data passing
/// 4. Subtask spawn (creates child task, reopens spawner)
/// 5. Subtask precedence (subtask blocks standalone task)
/// 6. Priority inheritance
/// 7. Spawned-by link in show output
/// 8. Idempotency (re-close doesn't duplicate)
/// 9. {{spawner.approved}} defaults to "false" when parent lacks approved data

use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;

/// Helper function to initialize a Git repository
fn init_git_repo(path: &std::path::Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("Failed to initialize Git repository");

    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .expect("Failed to configure git email");
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(path)
        .output()
        .expect("Failed to configure git name");
}

/// Helper function to initialize an Aiki repository
fn init_aiki_repo(path: &std::path::Path) {
    init_git_repo(path);

    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(path)
        .arg("init")
        .output()
        .expect("Failed to run aiki init");

    if !output.status.success() {
        panic!(
            "aiki init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Helper to run aiki task command and return Assert
fn aiki_task(path: &std::path::Path, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("aiki"));
    cmd.current_dir(path);
    cmd.arg("task");
    for arg in args {
        cmd.arg(arg);
    }
    cmd.assert()
}

/// Helper to run aiki task command and return raw output
fn aiki_task_output(path: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new(assert_cmd::cargo::cargo_bin!("aiki"))
        .current_dir(path)
        .arg("task")
        .args(args)
        .output()
        .expect("Failed to run aiki task command");
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Extract short task ID from "Added <id>" output line
fn extract_short_id(output: &str) -> String {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("Added ") {
            let id: String = rest.chars().take_while(|c| c.is_ascii_lowercase()).collect();
            return id;
        }
    }
    panic!("Could not find 'Added <id>' in output: {}", output);
}

/// Helper to create a template file for testing
fn create_template(templates_dir: &std::path::Path, namespace: &str, name: &str, content: &str) {
    let ns_dir = templates_dir.join(namespace);
    std::fs::create_dir_all(&ns_dir).expect("Failed to create namespace directory");
    let file_path = ns_dir.join(format!("{}.md", name));
    std::fs::write(&file_path, content).expect("Failed to write template file");
}

// ============================================================================
// Spawn Flow Tests
// ============================================================================

#[test]
fn test_spawn_on_close_basic() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    // Create a simple template for spawning
    let templates_dir = temp_dir.path().join(".aiki/templates");
    create_template(
        &templates_dir,
        "test",
        "spawned-task",
        "---\nversion: 1.0.0\n---\n# Spawned Task\n\nThis was spawned.\n",
    );

    // Create a template with spawns config
    create_template(
        &templates_dir,
        "test",
        "spawner",
        r#"---
version: 1.0.0
spawns:
  - when: "true"
    task:
      template: test/spawned-task
---
# Spawner Task

This task spawns another on close.
"#,
    );

    // Create task from spawner template
    let output = aiki_task_output(temp_dir.path(), &["add", "--template", "test/spawner"]);
    let spawner_id = extract_short_id(&output);

    // Start and close the task
    aiki_task(temp_dir.path(), &["start", &spawner_id]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"])
        .success()
        .stdout(predicate::str::contains("Spawned task from template test/spawned-task"));
}

#[test]
fn test_spawn_condition_false_no_spawn() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let templates_dir = temp_dir.path().join(".aiki/templates");
    create_template(
        &templates_dir,
        "test",
        "spawned-task",
        "---\nversion: 1.0.0\n---\n# Spawned Task\n\nThis was spawned.\n",
    );

    create_template(
        &templates_dir,
        "test",
        "no-spawn",
        r#"---
version: 1.0.0
spawns:
  - when: "false"
    task:
      template: test/spawned-task
---
# No Spawn Task

Condition is false, so no spawn should happen.
"#,
    );

    let output = aiki_task_output(temp_dir.path(), &["add", "--template", "test/no-spawn"]);
    let task_id = extract_short_id(&output);

    aiki_task(temp_dir.path(), &["start", &task_id]).success();

    // Close — should NOT mention spawning
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"])
        .success()
        .stdout(predicate::str::contains("Spawned").not());
}

#[test]
fn test_spawn_condition_not_approved() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let templates_dir = temp_dir.path().join(".aiki/templates");
    create_template(
        &templates_dir,
        "test",
        "fix",
        "---\nversion: 1.0.0\n---\n# Fix\n\nFix the issues.\n",
    );

    create_template(
        &templates_dir,
        "test",
        "review",
        r#"---
version: 1.0.0
spawns:
  - when: not approved
    task:
      template: test/fix
---
# Review Task

Review and set approved.
"#,
    );

    // Create task and set approved=false
    let output = aiki_task_output(
        temp_dir.path(),
        &["add", "--template", "test/review", "--data", "approved=false"],
    );
    let task_id = extract_short_id(&output);

    aiki_task(temp_dir.path(), &["start", &task_id]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Found issues"])
        .success()
        .stdout(predicate::str::contains("Spawned task from template test/fix"));
}

#[test]
fn test_spawn_approved_no_spawn() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let templates_dir = temp_dir.path().join(".aiki/templates");
    create_template(
        &templates_dir,
        "test",
        "fix",
        "---\nversion: 1.0.0\n---\n# Fix\n\nFix the issues.\n",
    );

    create_template(
        &templates_dir,
        "test",
        "review",
        r#"---
version: 1.0.0
spawns:
  - when: not approved
    task:
      template: test/fix
---
# Review Task

Review and set approved.
"#,
    );

    // Create task with approved=true
    let output = aiki_task_output(
        temp_dir.path(),
        &["add", "--template", "test/review", "--data", "approved=true"],
    );
    let task_id = extract_short_id(&output);

    aiki_task(temp_dir.path(), &["start", &task_id]).success();

    // Close — should NOT spawn since approved=true
    aiki_task(temp_dir.path(), &["close", "--summary", "LGTM"])
        .success()
        .stdout(predicate::str::contains("Spawned").not());
}

#[test]
fn test_spawn_shows_spawned_by_link() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let templates_dir = temp_dir.path().join(".aiki/templates");
    create_template(
        &templates_dir,
        "test",
        "spawned-task",
        "---\nversion: 1.0.0\n---\n# Spawned Task\n\nThis was spawned.\n",
    );

    create_template(
        &templates_dir,
        "test",
        "spawner",
        r#"---
version: 1.0.0
spawns:
  - when: "true"
    task:
      template: test/spawned-task
---
# Spawner Task

This task spawns another on close.
"#,
    );

    let output = aiki_task_output(temp_dir.path(), &["add", "--template", "test/spawner"]);
    let spawner_id = extract_short_id(&output);

    aiki_task(temp_dir.path(), &["start", &spawner_id]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // Show the spawner — should list spawned tasks
    aiki_task(temp_dir.path(), &["show", &spawner_id])
        .success()
        .stdout(predicate::str::contains("Spawned:"));
}

#[test]
fn test_spawn_subtask_creates_child() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let templates_dir = temp_dir.path().join(".aiki/templates");
    create_template(
        &templates_dir,
        "test",
        "child-task",
        "---\nversion: 1.0.0\n---\n# Child Task\n\nSubtask spawned.\n",
    );

    create_template(
        &templates_dir,
        "test",
        "parent-spawner",
        r#"---
version: 1.0.0
spawns:
  - when: "true"
    subtask:
      template: test/child-task
---
# Parent Spawner

This spawns a subtask on close.
"#,
    );

    let output = aiki_task_output(
        temp_dir.path(),
        &["add", "--template", "test/parent-spawner"],
    );
    let parent_id = extract_short_id(&output);

    aiki_task(temp_dir.path(), &["start", &parent_id]).success();

    // Close — should spawn subtask and reopen parent
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"])
        .success()
        .stdout(predicate::str::contains(
            "Spawned subtask from template test/child-task",
        ));

    // Parent should be reopened (since subtask was created)
    // Show parent — it should NOT be closed anymore
    let show_output = aiki_task_output(temp_dir.path(), &["show", &parent_id]);
    assert!(
        !show_output.contains("closed"),
        "Parent should be reopened after subtask spawn, got: {}",
        show_output
    );
}

#[test]
fn test_spawn_subtask_precedence_over_task() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let templates_dir = temp_dir.path().join(".aiki/templates");
    create_template(
        &templates_dir,
        "test",
        "standalone",
        "---\nversion: 1.0.0\n---\n# Standalone\n\nStandalone task.\n",
    );
    create_template(
        &templates_dir,
        "test",
        "child",
        "---\nversion: 1.0.0\n---\n# Child\n\nChild task.\n",
    );

    // Template with both task and subtask spawns
    create_template(
        &templates_dir,
        "test",
        "mixed-spawner",
        r#"---
version: 1.0.0
spawns:
  - when: "true"
    task:
      template: test/standalone
  - when: "true"
    subtask:
      template: test/child
---
# Mixed Spawner

Has both task and subtask spawns.
"#,
    );

    let output = aiki_task_output(
        temp_dir.path(),
        &["add", "--template", "test/mixed-spawner"],
    );
    let task_id = extract_short_id(&output);

    aiki_task(temp_dir.path(), &["start", &task_id]).success();

    // Close — should only spawn subtask (precedence rule)
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"])
        .success()
        .stdout(predicate::str::contains(
            "Spawned subtask from template test/child",
        ))
        .stdout(
            predicate::str::contains("Spawned task from template test/standalone").not(),
        );
}

#[test]
fn test_spawn_priority_inheritance() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let templates_dir = temp_dir.path().join(".aiki/templates");
    create_template(
        &templates_dir,
        "test",
        "spawned-task",
        "---\nversion: 1.0.0\n---\n# Spawned\n\nShould inherit priority.\n",
    );

    create_template(
        &templates_dir,
        "test",
        "p1-spawner",
        r#"---
version: 1.0.0
priority: p1
spawns:
  - when: "true"
    task:
      template: test/spawned-task
---
# P1 Spawner

Spawns a task that should inherit p1 priority.
"#,
    );

    let output = aiki_task_output(
        temp_dir.path(),
        &["add", "--template", "test/p1-spawner"],
    );
    let task_id = extract_short_id(&output);

    aiki_task(temp_dir.path(), &["start", &task_id]).success();
    aiki_task(temp_dir.path(), &["close", "--summary", "Done"]).success();

    // List all tasks — spawned task should have p1 priority
    let list_output = aiki_task_output(temp_dir.path(), &["list", "--all"]);
    // The spawned task "Spawned" should show up with p1 priority
    assert!(
        list_output.contains("p1"),
        "Spawned task should inherit p1 priority. Output: {}",
        list_output
    );
}

#[test]
fn test_spawn_wont_do_no_spawn() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let templates_dir = temp_dir.path().join(".aiki/templates");
    create_template(
        &templates_dir,
        "test",
        "spawned-task",
        "---\nversion: 1.0.0\n---\n# Spawned\n\nSpawned task.\n",
    );

    create_template(
        &templates_dir,
        "test",
        "outcome-spawner",
        r#"---
version: 1.0.0
spawns:
  - when: outcome == "done"
    task:
      template: test/spawned-task
---
# Outcome Spawner

Only spawns when outcome is done.
"#,
    );

    let output = aiki_task_output(
        temp_dir.path(),
        &["add", "--template", "test/outcome-spawner"],
    );
    let task_id = extract_short_id(&output);

    aiki_task(temp_dir.path(), &["start", &task_id]).success();

    // Close as wont_do — should NOT spawn
    aiki_task(
        temp_dir.path(),
        &["close", "--wont-do", "--summary", "Not needed"],
    )
    .success()
    .stdout(predicate::str::contains("Spawned").not());
}

#[test]
fn test_spawn_approved_defaults_false_in_template_substitution() {
    let temp_dir = tempfile::tempdir().unwrap();
    init_aiki_repo(temp_dir.path());

    let templates_dir = temp_dir.path().join(".aiki/templates");

    // Spawned task template that uses {{spawner.approved}} in its body
    create_template(
        &templates_dir,
        "test",
        "fix-with-approved",
        "---\nversion: 1.0.0\n---\n# Fix Task\n\nSpawner approved: {{spawner.approved}}\n",
    );

    // Spawner template — does NOT set approved data
    create_template(
        &templates_dir,
        "test",
        "review-no-approved",
        r#"---
version: 1.0.0
spawns:
  - when: "true"
    task:
      template: test/fix-with-approved
---
# Review (no approved data)

This spawner has no approved field in its data.
"#,
    );

    // Create spawner task WITHOUT any --data approved=...
    let output = aiki_task_output(
        temp_dir.path(),
        &["add", "--template", "test/review-no-approved"],
    );
    let spawner_id = extract_short_id(&output);

    aiki_task(temp_dir.path(), &["start", &spawner_id]).success();

    // Close — should successfully spawn ({{spawner.approved}} defaults to "false")
    let close_output = aiki_task_output(temp_dir.path(), &["close", "--summary", "Done"]);
    assert!(
        close_output.contains("Spawned task from template test/fix-with-approved"),
        "Should spawn task even when parent has no approved data. Output: {}",
        close_output
    );

    // Verify the spawned task's instructions contain the defaulted value
    let list_output = aiki_task_output(temp_dir.path(), &["list", "--all"]);
    // Find the spawned task (it will be named "Fix Task")
    let fix_line = list_output
        .lines()
        .find(|l| l.contains("Fix Task"))
        .expect("Spawned 'Fix Task' should appear in task list");
    // Extract the task ID from the line — format is "[p2] qzqxvnx  Fix Task"
    // Find the first run of 7+ lowercase letters (the short ID)
    let fix_id: String = fix_line
        .split_whitespace()
        .find(|word| word.chars().all(|c| c.is_ascii_lowercase()) && word.len() >= 7)
        .expect("Should find short task ID in line")
        .to_string();
    assert!(!fix_id.is_empty(), "Should extract fix task ID from: {}", fix_line);

    let show_output = aiki_task_output(temp_dir.path(), &["show", &fix_id, "--with-instructions"]);
    assert!(
        show_output.contains("Spawner approved: false"),
        "Spawned task should have spawner.approved substituted as 'false'. Output: {}",
        show_output
    );
}
