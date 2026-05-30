//! Spawn evaluation engine
//!
//! Evaluates `spawns:` conditions when a task closes and determines
//! which new tasks should be created.

use rhai::{Dynamic, Map, Scope};
use std::collections::{BTreeMap, HashMap};

use crate::expressions::ExpressionEvaluator;
use crate::tasks::graph::TaskGraph;
use crate::tasks::templates::spawn_config::SpawnEntry;
use crate::tasks::types::Task;

/// An action to take after spawn evaluation
#[derive(Debug, Clone)]
pub enum SpawnAction {
    /// Create a standalone task (no parent relationship)
    CreateTask {
        template: String,
        priority: Option<String>,
        assignee: Option<String>,
        data: HashMap<String, String>,
        spawn_index: usize,
        autorun: bool,
    },
    /// Create a subtask (spawner becomes parent)
    CreateSubtask {
        template: String,
        priority: Option<String>,
        assignee: Option<String>,
        data: HashMap<String, String>,
        spawn_index: usize,
        autorun: bool,
    },
}

/// Build a Rhai Scope from the post-transition task state.
///
/// The scope provides access to:
/// - `approved` → bool
/// - `status` → String (always "closed")
/// - `outcome` → String ("done" or "wont_do")
/// - `data.*` → nested map from task.data
/// - `comments` → array of comment texts
/// - `priority` → String
/// - `subtasks.{slug}.*` → nested map for subtasks with slugs
pub fn build_spawn_scope(task: &Task, graph: &TaskGraph) -> Scope<'static> {
    let mut scope = Scope::new();

    // approved: from task data, default false
    let approved = task
        .data
        .get("approved")
        .map(|v| v == "true")
        .unwrap_or(false);
    scope.push("approved", approved);

    // status: always "closed" (spawn conditions run post-close)
    scope.push("status", "closed".to_string());

    // outcome
    let outcome = task
        .closed_outcome
        .map(|o| o.to_string())
        .unwrap_or_else(|| "done".to_string());
    scope.push("outcome", outcome);

    // priority
    scope.push("priority", task.priority.to_string());

    // data.* as nested map (excluding internal fields)
    let mut data_map: Map = BTreeMap::new();
    for (key, value) in &task.data {
        if key.starts_with('_') {
            continue; // Skip internal fields like _spawns, _spawn_key
        }
        let dynamic_val = crate::expressions::coerce_to_dynamic(value);
        // Handle dotted keys by creating nested maps
        if let Some(dot_pos) = key.find('.') {
            let top = &key[..dot_pos];
            let rest = &key[dot_pos + 1..];
            let entry = data_map
                .entry(top.into())
                .or_insert_with(|| Dynamic::from_map(Map::new()));
            if let Some(mut inner) = entry.write_lock::<Map>() {
                inner.insert(rest.into(), dynamic_val);
            }
        } else {
            data_map.insert(key.as_str().into(), dynamic_val);
        }
    }
    scope.push("data", data_map);

    // comments: array of comment texts
    let comments: Vec<Dynamic> = task
        .comments
        .iter()
        .map(|c| Dynamic::from(c.text.clone()))
        .collect();
    scope.push("comments", comments);

    // subtasks.{slug}.* for subtasks with slugs
    let children = graph.children_of(&task.id);
    let mut subtasks_map: Map = BTreeMap::new();
    for child in &children {
        if let Some(ref slug) = child.slug {
            let mut child_map: Map = BTreeMap::new();
            child_map.insert("status".into(), Dynamic::from(child.status.to_string()));
            let child_approved = child
                .data
                .get("approved")
                .map(|v| v == "true")
                .unwrap_or(false);
            child_map.insert("approved".into(), Dynamic::from(child_approved));
            if let Some(ref outcome) = child.closed_outcome {
                child_map.insert("outcome".into(), Dynamic::from(outcome.to_string()));
            }
            // child data
            let mut child_data_map: Map = BTreeMap::new();
            for (key, value) in &child.data {
                if !key.starts_with('_') {
                    child_data_map.insert(key.as_str().into(), crate::expressions::coerce_to_dynamic(value));
                }
            }
            child_map.insert("data".into(), Dynamic::from_map(child_data_map));
            child_map.insert("priority".into(), Dynamic::from(child.priority.to_string()));

            subtasks_map.insert(slug.as_str().into(), Dynamic::from_map(child_map));
        }
    }
    scope.push("subtasks", subtasks_map);

    scope
}

/// Evaluate a Rhai expression and return the Dynamic result (not coerced to bool).
///
/// Used for data value evaluation where we need the actual value, not a boolean.
/// Unlike condition evaluation, undefined variables cause errors (not silent false),
/// so that bare strings like "urgent" fail and can be treated as literals.
fn evaluate_expression_dynamic(
    _evaluator: &mut ExpressionEvaluator,
    expr: &str,
    scope: &mut Scope,
) -> Result<Dynamic, String> {
    // Reuse the evaluator's preprocessing
    let processed = crate::expressions::preprocess_expression(expr);

    // Create a temporary engine — no undefined-var fallback so bare words error out
    let engine = rhai::Engine::new();

    match engine.compile_expression(&processed) {
        Ok(ast) => match engine.eval_ast_with_scope::<Dynamic>(scope, &ast) {
            Ok(result) => Ok(result),
            Err(err) => Err(format!("evaluation error: {}", err)),
        },
        Err(err) => Err(format!("compile error: {}", err)),
    }
}

/// Convert a Dynamic value to a JSON-compatible element string.
///
/// Unlike `dynamic_to_string`, this always produces valid JSON tokens:
/// strings are quoted, making it safe for use inside arrays and maps.
fn dynamic_to_json_element(val: &Dynamic) -> Result<String, String> {
    if val.is_bool() {
        Ok(val.as_bool().unwrap().to_string())
    } else if val.is_int() {
        Ok(val.as_int().unwrap().to_string())
    } else if val.is_float() {
        Ok(val.as_float().unwrap().to_string())
    } else if val.is_string() {
        let s = val.clone().into_string().unwrap();
        Ok(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
    } else if val.is_array() {
        let arr = val.read_lock::<rhai::Array>().unwrap();
        let items: Vec<String> = arr
            .iter()
            .filter_map(|v| dynamic_to_json_element(v).ok())
            .collect();
        Ok(format!("[{}]", items.join(",")))
    } else if val.is_map() {
        let map = val.read_lock::<Map>().unwrap();
        let items: Vec<String> = map
            .iter()
            .filter_map(|(k, v)| {
                dynamic_to_json_element(v)
                    .ok()
                    .map(|vs| format!("\"{}\":{}", k, vs))
            })
            .collect();
        Ok(format!("{{{}}}", items.join(",")))
    } else {
        Err(format!("unsupported type: {}", val.type_name()))
    }
}

/// Convert a Dynamic value to a String for storage.
///
/// Scalars are stored as plain strings (no quoting).
/// Arrays and Maps are serialized as valid JSON.
/// Returns Err for unsupported types.
fn dynamic_to_string(val: &Dynamic) -> Result<String, String> {
    if val.is_bool() {
        Ok(val.as_bool().unwrap().to_string())
    } else if val.is_int() {
        Ok(val.as_int().unwrap().to_string())
    } else if val.is_float() {
        Ok(val.as_float().unwrap().to_string())
    } else if val.is_string() {
        Ok(val.clone().into_string().unwrap())
    } else if val.is_array() || val.is_map() {
        dynamic_to_json_element(val)
    } else {
        Err(format!("unsupported type: {}", val.type_name()))
    }
}

/// Convert a `serde_yaml::Value` to a Rhai `Dynamic` without Rhai evaluation.
///
/// Used for YAML Sequence/Mapping values that should be passed through
/// directly as Array/Map data.
fn yaml_value_to_dynamic(value: &serde_yaml::Value) -> Dynamic {
    match value {
        serde_yaml::Value::Null => Dynamic::UNIT,
        serde_yaml::Value::Bool(b) => Dynamic::from(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Dynamic::from(i)
            } else if let Some(f) = n.as_f64() {
                Dynamic::from(f)
            } else {
                Dynamic::from(n.to_string())
            }
        }
        serde_yaml::Value::String(s) => Dynamic::from(s.clone()),
        serde_yaml::Value::Sequence(seq) => {
            let arr: Vec<Dynamic> = seq.iter().map(yaml_value_to_dynamic).collect();
            Dynamic::from_array(arr)
        }
        serde_yaml::Value::Mapping(map) => {
            let mut rhai_map: Map = BTreeMap::new();
            for (k, v) in map {
                if let Some(key_str) = k.as_str() {
                    rhai_map.insert(key_str.into(), yaml_value_to_dynamic(v));
                }
            }
            Dynamic::from_map(rhai_map)
        }
        serde_yaml::Value::Tagged(tagged) => yaml_value_to_dynamic(&tagged.value),
    }
}

/// Evaluate data values for a spawn entry.
///
/// Each value in the spawn config's data map is a Rhai expression evaluated
/// against the spawner's state. Returns the evaluated data, or Err if any
/// value fails to evaluate.
///
/// **String handling**: YAML strings are evaluated as Rhai expressions.
/// If evaluation fails (e.g., undefined variable, type error), the entire
/// spawn entry is skipped. To pass string literals, use Rhai string syntax
/// in YAML: `label: '"urgent"'`.
///
/// **Array/Map handling**: YAML sequences and mappings are converted directly
/// to their Dynamic equivalents without Rhai evaluation.
fn evaluate_spawn_data(
    evaluator: &mut ExpressionEvaluator,
    data: &HashMap<String, serde_yaml::Value>,
    scope: &mut Scope,
) -> Result<HashMap<String, String>, String> {
    let mut result = HashMap::new();

    for (key, value) in data {
        let result_str = match value {
            serde_yaml::Value::String(s) => {
                // Try to evaluate as Rhai expression first.
                let mut eval_scope = scope.clone();
                match evaluate_expression_dynamic(evaluator, s, &mut eval_scope) {
                    Ok(dynamic_val) => match dynamic_to_string(&dynamic_val) {
                        Ok(s) => s,
                        Err(e) => return Err(format!("key '{}': {}", key, e)),
                    },
                    Err(e) => {
                        // Rhai evaluation failed — skip the spawn entry.
                        // Per spec, undefined variables and type errors cause
                        // the entire spawn entry to be skipped (not silently
                        // substituted with a default). To pass string literals,
                        // use Rhai string syntax: label: '"urgent"' in YAML.
                        return Err(format!("key '{}': {}", key, e));
                    }
                }
            }
            serde_yaml::Value::Bool(b) => b.to_string(),
            serde_yaml::Value::Number(n) => n.to_string(),
            serde_yaml::Value::Sequence(_) | serde_yaml::Value::Mapping(_) => {
                // Convert YAML arrays/maps directly to Dynamic, then serialize.
                let dynamic = yaml_value_to_dynamic(value);
                match dynamic_to_string(&dynamic) {
                    Ok(s) => s,
                    Err(e) => return Err(format!("key '{}': {}", key, e)),
                }
            }
            _ => {
                return Err(format!(
                    "unsupported data value type for key '{}': {:?}",
                    key, value
                ));
            }
        };

        result.insert(key.clone(), result_str);
    }

    Ok(result)
}

/// Evaluate spawn conditions and return actions to take.
///
/// For each SpawnEntry, evaluates the `when` condition. If true, evaluates
/// data values and produces a SpawnAction.
///
/// **Precedence rule**: If any `subtask` spawns are created, all `task` spawns
/// are skipped (even if their conditions are true).
pub fn evaluate_spawns(
    task: &Task,
    graph: &TaskGraph,
    spawns_config: &[SpawnEntry],
) -> Vec<SpawnAction> {
    let mut evaluator = ExpressionEvaluator::new();
    // Check for data.options.once flag - if set, skip all spawns
    if let Some(once) = task.data.get("options.once") {
        if once == "true" {
            return Vec::new();
        }
    }
    let scope = build_spawn_scope(task, graph);

    let mut task_actions: Vec<SpawnAction> = Vec::new();
    let mut subtask_actions: Vec<SpawnAction> = Vec::new();

    for (index, entry) in spawns_config.iter().enumerate() {
        // Evaluate the when condition
        let mut eval_scope = scope.clone();
        let condition_result = evaluator.evaluate(&entry.when, &mut eval_scope);
        let is_true = match condition_result {
            Ok(b) => b,
            Err(_) => {
                eprintln!(
                    "[aiki] Warning: spawn condition evaluation failed for task {}, spawn index {}: skipping",
                    task.id, index
                );
                continue;
            }
        };

        if !is_true {
            continue;
        }

        // Determine if this is a task or subtask spawn
        let (config, is_subtask) = if let Some(ref subtask_config) = entry.subtask {
            (subtask_config, true)
        } else if let Some(ref task_config) = entry.task {
            (task_config, false)
        } else {
            eprintln!(
                "[aiki] Warning: spawn entry {} for task {} has neither 'task' nor 'subtask': skipping",
                index, task.id
            );
            continue;
        };

        // Evaluate data values
        let evaluated_data = if config.data.is_empty() {
            HashMap::new()
        } else {
            let mut data_scope = scope.clone();
            match evaluate_spawn_data(&mut evaluator, &config.data, &mut data_scope) {
                Ok(data) => data,
                Err(e) => {
                    eprintln!(
                        "[aiki] Warning: spawn data evaluation failed for task {}, spawn index {}: {} — skipping",
                        task.id, index, e
                    );
                    continue;
                }
            }
        };

        let action = if is_subtask {
            SpawnAction::CreateSubtask {
                template: config.template.clone(),
                priority: config.priority.clone(),
                assignee: config.assignee.clone(),
                data: evaluated_data,
                spawn_index: index,
                autorun: config.autorun,
            }
        } else {
            SpawnAction::CreateTask {
                template: config.template.clone(),
                priority: config.priority.clone(),
                assignee: config.assignee.clone(),
                data: evaluated_data,
                spawn_index: index,
                autorun: config.autorun,
            }
        };

        if is_subtask {
            subtask_actions.push(action);
        } else {
            task_actions.push(action);
        }
    }

    // Subtask precedence: if any subtasks, skip all standalone tasks
    if !subtask_actions.is_empty() {
        subtask_actions
    } else {
        task_actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::materialize_graph;
    use crate::tasks::templates::spawn_config::{SpawnEntry, SpawnTaskConfig};
    use crate::tasks::types::{TaskEvent, TaskOutcome, TaskPriority, TaskStatus};
    use chrono::Utc;

    fn make_closed_task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            name: "Test task".to_string(),
            slug: None,
            task_type: None,
            status: TaskStatus::Closed,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: HashMap::new(),
            created_at: Utc::now(),
            started_at: None,
            claimed_by_session: None,
            last_session_id: None,
            stopped_reason: None,
            closed_outcome: Some(TaskOutcome::Done),
            summary: None,
            turn_started: None,
            turn_closed: None,
            turn_stopped: None,
            comments: Vec::new(),
        }
    }

    fn empty_graph() -> TaskGraph {
        materialize_graph(&[])
    }

    #[test]
    fn test_evaluate_simple_not_approved() {
        let mut task = make_closed_task("spawner");
        task.data.insert("approved".to_string(), "false".to_string());

        let graph = empty_graph();
        let spawns = vec![SpawnEntry {
            when: "not approved".to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/fix".to_string(),
                priority: None,
                assignee: None,
                autorun: false,
                data: HashMap::new(),
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SpawnAction::CreateTask { template, .. } => assert_eq!(template, "aiki/fix"),
            _ => panic!("Expected CreateTask"),
        }
    }

    #[test]
    fn test_evaluate_approved_no_spawn() {
        let mut task = make_closed_task("spawner");
        task.data.insert("approved".to_string(), "true".to_string());

        let graph = empty_graph();
        let spawns = vec![SpawnEntry {
            when: "not approved".to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/fix".to_string(),
                priority: None,
                assignee: None,
                autorun: false,
                data: HashMap::new(),
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_evaluate_data_condition() {
        let mut task = make_closed_task("spawner");
        task.data
            .insert("issues_found".to_string(), "5".to_string());

        let graph = empty_graph();
        let spawns = vec![SpawnEntry {
            when: "data.issues_found > 3".to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/follow-up".to_string(),
                priority: None,
                assignee: None,
                autorun: false,
                data: HashMap::new(),
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn test_subtask_precedence() {
        let mut task = make_closed_task("spawner");
        task.data.insert("approved".to_string(), "false".to_string());
        task.data
            .insert("needs_breakdown".to_string(), "true".to_string());

        let graph = empty_graph();
        let spawns = vec![
            SpawnEntry {
                when: "not approved".to_string(),
                task: Some(SpawnTaskConfig {
                    template: "aiki/fix".to_string(),
                    priority: None,
                    assignee: None,
                    autorun: false,
                    data: HashMap::new(),
                }),
                subtask: None,
            },
            SpawnEntry {
                when: "data.needs_breakdown".to_string(),
                task: None,
                subtask: Some(SpawnTaskConfig {
                    template: "aiki/analysis".to_string(),
                    priority: None,
                    assignee: None,
                    autorun: false,
                    data: HashMap::new(),
                }),
            },
        ];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        // Only subtask should be returned (subtask precedence)
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SpawnAction::CreateSubtask { template, .. } => assert_eq!(template, "aiki/analysis"),
            _ => panic!("Expected CreateSubtask"),
        }
    }

    #[test]
    fn test_data_value_evaluation() {
        let mut task = make_closed_task("spawner");
        task.data
            .insert("issues_found".to_string(), "7".to_string());

        let graph = empty_graph();
        let mut spawn_data = HashMap::new();
        spawn_data.insert(
            "issue_count".to_string(),
            serde_yaml::Value::from("data.issues_found"),
        );
        spawn_data.insert(
            "max_iterations".to_string(),
            serde_yaml::Value::from(3),
        );

        let spawns = vec![SpawnEntry {
            when: "true".to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/fix".to_string(),
                priority: Some("p0".to_string()),
                assignee: None,
                autorun: false,
                data: spawn_data,
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SpawnAction::CreateTask {
                data, priority, ..
            } => {
                assert_eq!(data.get("issue_count"), Some(&"7".to_string()));
                assert_eq!(data.get("max_iterations"), Some(&"3".to_string()));
                assert_eq!(priority, &Some("p0".to_string()));
            }
            _ => panic!("Expected CreateTask"),
        }
    }

    #[test]
    fn test_data_bare_word_skips_spawn_entry() {
        // A bare word like "urgent" is not a defined Rhai variable.
        // Per spec, evaluation errors cause the spawn entry to be skipped.
        // To pass string literals, use Rhai string syntax: label: '"urgent"'
        let task = make_closed_task("spawner");
        let graph = empty_graph();

        let mut spawn_data = HashMap::new();
        spawn_data.insert(
            "label".to_string(),
            serde_yaml::Value::from("urgent"), // bare word → Rhai error → skip
        );

        let spawns = vec![SpawnEntry {
            when: "true".to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/fix".to_string(),
                priority: None,
                assignee: None,
                autorun: false,
                data: spawn_data,
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        // Spawn entry should be skipped because "urgent" fails Rhai evaluation
        assert!(actions.is_empty());
    }

    #[test]
    fn test_data_rhai_string_literal() {
        // To pass a literal string, use a Rhai string expression: '"urgent"'
        // In YAML: label: '"urgent"'
        let task = make_closed_task("spawner");
        let graph = empty_graph();

        let mut spawn_data = HashMap::new();
        spawn_data.insert(
            "label".to_string(),
            serde_yaml::Value::from("\"urgent\""), // Rhai string literal
        );
        spawn_data.insert(
            "max_iterations".to_string(),
            serde_yaml::Value::from(3),
        );

        let spawns = vec![SpawnEntry {
            when: "true".to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/fix".to_string(),
                priority: None,
                assignee: None,
                autorun: false,
                data: spawn_data,
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SpawnAction::CreateTask { data, .. } => {
                assert_eq!(data.get("label"), Some(&"urgent".to_string()));
                assert_eq!(data.get("max_iterations"), Some(&"3".to_string()));
            }
            _ => panic!("Expected CreateTask"),
        }
    }

    #[test]
    fn test_multiple_spawns_both_true() {
        let task = make_closed_task("spawner");
        let graph = empty_graph();
        let spawns = vec![
            SpawnEntry {
                when: "true".to_string(),
                task: Some(SpawnTaskConfig {
                    template: "aiki/fix".to_string(),
                    priority: None,
                    assignee: None,
                    autorun: false,
                    data: HashMap::new(),
                }),
                subtask: None,
            },
            SpawnEntry {
                when: "true".to_string(),
                task: Some(SpawnTaskConfig {
                    template: "aiki/urgent-fix".to_string(),
                    priority: None,
                    assignee: None,
                    autorun: false,
                    data: HashMap::new(),
                }),
                subtask: None,
            },
        ];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 2);
    }

    #[test]
    fn test_condition_error_skips_entry() {
        let task = make_closed_task("spawner");
        let graph = empty_graph();
        let spawns = vec![
            // Invalid expression syntax
            SpawnEntry {
                when: "invalid $$$ syntax".to_string(),
                task: Some(SpawnTaskConfig {
                    template: "aiki/fix".to_string(),
                    priority: None,
                    assignee: None,
                    autorun: false,
                    data: HashMap::new(),
                }),
                subtask: None,
            },
            // Valid expression
            SpawnEntry {
                when: "true".to_string(),
                task: Some(SpawnTaskConfig {
                    template: "aiki/follow-up".to_string(),
                    priority: None,
                    assignee: None,
                    autorun: false,
                    data: HashMap::new(),
                }),
                subtask: None,
            },
        ];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        // Only the valid one should succeed
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SpawnAction::CreateTask { template, .. } => assert_eq!(template, "aiki/follow-up"),
            _ => panic!("Expected CreateTask"),
        }
    }

    #[test]
    fn test_spawn_index_tracking() {
        let task = make_closed_task("spawner");
        let graph = empty_graph();
        let spawns = vec![
            SpawnEntry {
                when: "false".to_string(), // won't trigger
                task: Some(SpawnTaskConfig {
                    template: "aiki/a".to_string(),
                    priority: None,
                    assignee: None,
                    autorun: false,
                    data: HashMap::new(),
                }),
                subtask: None,
            },
            SpawnEntry {
                when: "true".to_string(), // index 1
                task: Some(SpawnTaskConfig {
                    template: "aiki/b".to_string(),
                    priority: None,
                    assignee: None,
                    autorun: false,
                    data: HashMap::new(),
                }),
                subtask: None,
            },
            SpawnEntry {
                when: "true".to_string(), // index 2
                task: Some(SpawnTaskConfig {
                    template: "aiki/c".to_string(),
                    priority: None,
                    assignee: None,
                    autorun: false,
                    data: HashMap::new(),
                }),
                subtask: None,
            },
        ];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 2);
        match &actions[0] {
            SpawnAction::CreateTask { spawn_index, .. } => assert_eq!(*spawn_index, 1),
            _ => panic!("Expected CreateTask"),
        }
        match &actions[1] {
            SpawnAction::CreateTask { spawn_index, .. } => assert_eq!(*spawn_index, 2),
            _ => panic!("Expected CreateTask"),
        }
    }

    #[test]
    fn test_subtask_access_in_conditions() {
        // Build a graph with a parent and a subtask with slug
        let events = vec![
            TaskEvent::Created {
                task_id: "parent".to_string(),
                name: "Parent".to_string(),
                slug: None,
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
            TaskEvent::Created {
                task_id: "parent.1".to_string(),
                name: "Review subtask".to_string(),
                slug: Some("review".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: {
                    let mut d = HashMap::new();
                    d.insert("approved".to_string(), "false".to_string());
                    d
                },
                timestamp: Utc::now(),
            },
            TaskEvent::Closed {
                task_ids: vec!["parent.1".to_string()],
                outcome: TaskOutcome::Done,
                summary: None,
                turn_id: None,
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);

        // Create the parent task for scope building
        let mut parent_task = make_closed_task("parent");
        parent_task.data.insert("approved".to_string(), "false".to_string());

        let spawns = vec![SpawnEntry {
            when: "not subtasks.review.approved".to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/fix".to_string(),
                priority: None,
                assignee: None,
                autorun: false,
                data: HashMap::new(),
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&parent_task, &graph, &spawns);
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn test_outcome_condition() {
        let mut task = make_closed_task("spawner");
        task.closed_outcome = Some(TaskOutcome::Done);

        let graph = empty_graph();
        let spawns = vec![SpawnEntry {
            when: r#"outcome == "done""#.to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/follow-up".to_string(),
                priority: None,
                assignee: None,
                autorun: false,
                data: HashMap::new(),
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 1);
    }

    #[test]
    fn test_wont_do_no_spawn() {
        let mut task = make_closed_task("spawner");
        task.closed_outcome = Some(TaskOutcome::WontDo);

        let graph = empty_graph();
        let spawns = vec![SpawnEntry {
            when: r#"outcome == "done""#.to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/follow-up".to_string(),
                priority: None,
                assignee: None,
                autorun: false,
                data: HashMap::new(),
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_data_yaml_array_value() {
        let task = make_closed_task("spawner");
        let graph = empty_graph();

        let mut spawn_data = HashMap::new();
        // YAML sequence: items: [foo, bar, baz]
        spawn_data.insert(
            "items".to_string(),
            serde_yaml::Value::Sequence(vec![
                serde_yaml::Value::from("foo"),
                serde_yaml::Value::from("bar"),
                serde_yaml::Value::from("baz"),
            ]),
        );

        let spawns = vec![SpawnEntry {
            when: "true".to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/fix".to_string(),
                priority: None,
                assignee: None,
                autorun: false,
                data: spawn_data,
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SpawnAction::CreateTask { data, .. } => {
                let items = data.get("items").unwrap();
                assert_eq!(items, r#"["foo","bar","baz"]"#);
            }
            _ => panic!("Expected CreateTask"),
        }
    }

    #[test]
    fn test_data_yaml_map_value() {
        let task = make_closed_task("spawner");
        let graph = empty_graph();

        let mut inner_map = serde_yaml::Mapping::new();
        inner_map.insert(
            serde_yaml::Value::from("host"),
            serde_yaml::Value::from("localhost"),
        );
        inner_map.insert(
            serde_yaml::Value::from("port"),
            serde_yaml::Value::from(8080),
        );

        let mut spawn_data = HashMap::new();
        spawn_data.insert(
            "config".to_string(),
            serde_yaml::Value::Mapping(inner_map),
        );

        let spawns = vec![SpawnEntry {
            when: "true".to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/fix".to_string(),
                priority: None,
                assignee: None,
                autorun: false,
                data: spawn_data,
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SpawnAction::CreateTask { data, .. } => {
                let config = data.get("config").unwrap();
                // Map serialized as JSON object
                assert!(config.contains("\"host\":\"localhost\""));
                assert!(config.contains("\"port\":8080"));
            }
            _ => panic!("Expected CreateTask"),
        }
    }

    #[test]
    fn test_spawn_action_carries_autorun_true() {
        let task = make_closed_task("spawner");
        let graph = empty_graph();

        let spawns = vec![SpawnEntry {
            when: "true".to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/fix".to_string(),
                priority: None,
                assignee: None,
                autorun: true,
                data: HashMap::new(),
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SpawnAction::CreateTask { autorun, .. } => {
                assert!(*autorun, "autorun should be true");
            }
            _ => panic!("Expected CreateTask"),
        }
    }

    #[test]
    fn test_spawn_action_carries_autorun_false() {
        let task = make_closed_task("spawner");
        let graph = empty_graph();

        let spawns = vec![SpawnEntry {
            when: "true".to_string(),
            task: Some(SpawnTaskConfig {
                template: "aiki/fix".to_string(),
                priority: None,
                assignee: None,
                autorun: false,
                data: HashMap::new(),
            }),
            subtask: None,
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SpawnAction::CreateTask { autorun, .. } => {
                assert!(!*autorun, "autorun should be false");
            }
            _ => panic!("Expected CreateTask"),
        }
    }

    #[test]
    fn test_subtask_spawn_carries_autorun() {
        let task = make_closed_task("spawner");
        let graph = empty_graph();

        let spawns = vec![SpawnEntry {
            when: "true".to_string(),
            task: None,
            subtask: Some(SpawnTaskConfig {
                template: "aiki/analysis".to_string(),
                priority: None,
                assignee: None,
                autorun: true,
                data: HashMap::new(),
            }),
        }];

        let actions = evaluate_spawns(&task, &graph, &spawns);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            SpawnAction::CreateSubtask { autorun, .. } => {
                assert!(*autorun, "subtask autorun should be true");
            }
            _ => panic!("Expected CreateSubtask"),
        }
    }
}
