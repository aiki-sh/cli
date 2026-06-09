use super::*;
use rhai::{Array, Dynamic, Map, Scope};
use std::collections::BTreeMap;

fn eval(expr: &str, vars: &[(&str, &str)]) -> bool {
    let mut evaluator = ExpressionEvaluator::new();
    let map: BTreeMap<String, String> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut scope = build_scope_from_flat(&map);
    evaluator.evaluate(expr, &mut scope).unwrap_or(false)
}

// === Boolean literals ===

#[test]
fn test_bool_true() {
    assert!(eval("true", &[]));
}

#[test]
fn test_bool_false() {
    assert!(!eval("false", &[]));
}

// === Numeric comparisons ===

#[test]
fn test_numeric_greater_than() {
    assert!(eval("x > 7", &[("x", "10")]));
    assert!(!eval("x > 7", &[("x", "5")]));
}

#[test]
fn test_numeric_greater_or_equal() {
    assert!(eval("x >= 7", &[("x", "7")]));
    assert!(eval("x >= 7", &[("x", "10")]));
    assert!(!eval("x >= 7", &[("x", "5")]));
}

#[test]
fn test_numeric_less_than() {
    assert!(eval("x < 3", &[("x", "1")]));
    assert!(!eval("x < 3", &[("x", "5")]));
}

#[test]
fn test_numeric_less_or_equal() {
    assert!(eval("x <= 3", &[("x", "3")]));
    assert!(eval("x <= 3", &[("x", "1")]));
    assert!(!eval("x <= 3", &[("x", "5")]));
}

#[test]
fn test_numeric_equals() {
    assert!(eval("x == 5", &[("x", "5")]));
    assert!(!eval("x == 5", &[("x", "3")]));
}

#[test]
fn test_numeric_not_equals() {
    assert!(eval("x != 5", &[("x", "3")]));
    assert!(!eval("x != 5", &[("x", "5")]));
}

// === String comparisons ===

#[test]
fn test_string_equals() {
    assert!(eval(r#"status == "done""#, &[("status", "done")]));
    assert!(!eval(r#"status == "done""#, &[("status", "pending")]));
}

#[test]
fn test_string_not_equals() {
    assert!(eval(r#"name != "test""#, &[("name", "prod")]));
    assert!(!eval(r#"name != "test""#, &[("name", "test")]));
}

// === Logical operators (symbol form) ===

#[test]
fn test_logical_and() {
    assert!(eval(
        "x > 5 && approved",
        &[("x", "10"), ("approved", "true")]
    ));
    assert!(!eval(
        "x > 5 && approved",
        &[("x", "10"), ("approved", "false")]
    ));
    assert!(!eval(
        "x > 5 && approved",
        &[("x", "3"), ("approved", "true")]
    ));
}

#[test]
fn test_logical_or() {
    assert!(eval("a || b", &[("a", "true"), ("b", "false")]));
    assert!(eval("a || b", &[("a", "false"), ("b", "true")]));
    assert!(!eval("a || b", &[("a", "false"), ("b", "false")]));
}

#[test]
fn test_logical_not() {
    assert!(eval("!done", &[("done", "false")]));
    assert!(!eval("!done", &[("done", "true")]));
}

// === Word operators (and/or/not) ===

#[test]
fn test_word_and() {
    assert!(eval(
        "x > 5 and approved",
        &[("x", "10"), ("approved", "true")]
    ));
    assert!(!eval(
        "x > 5 and approved",
        &[("x", "3"), ("approved", "true")]
    ));
}

#[test]
fn test_word_or() {
    assert!(eval("a or b", &[("a", "true"), ("b", "false")]));
    assert!(!eval("a or b", &[("a", "false"), ("b", "false")]));
}

#[test]
fn test_word_not() {
    assert!(eval("not done", &[("done", "false")]));
    assert!(!eval("not done", &[("done", "true")]));
}

// === Word operator boundary safety ===

#[test]
fn test_word_operator_not_in_identifiers() {
    // "band" should not become "b&&"
    assert!(eval("band", &[("band", "true")]));
    // "orange" should not become "||ange"
    assert!(eval("orange", &[("orange", "true")]));
    // "notice" should not become "!ice"
    assert!(eval("notice", &[("notice", "true")]));
}

// === Dotted field access ===

#[test]
fn test_dotted_field_access() {
    assert!(eval(
        r#"event.task.type == "review""#,
        &[("event.task.type", "review")]
    ));
}

#[test]
fn test_nested_field_truthy() {
    assert!(eval("event.write", &[("event.write", "true")]));
    assert!(!eval("event.write", &[("event.write", "false")]));
}

// === Dollar-prefix stripping ===

#[test]
fn test_dollar_prefix_stripped() {
    assert!(eval("$event.write", &[("event.write", "true")]));
}

#[test]
fn test_dollar_prefix_in_comparison() {
    assert!(eval(
        r#"$event.task.type == "review""#,
        &[("event.task.type", "review")]
    ));
}

#[test]
fn test_dollar_prefix_both_sides() {
    assert!(eval(
        "$event.task.id == $session.task.id",
        &[("event.task.id", "abc123"), ("session.task.id", "abc123")]
    ));
    assert!(!eval(
        "$event.task.id == $session.task.id",
        &[("event.task.id", "abc123"), ("session.task.id", "xyz789")]
    ));
}

// === Truthiness ===

#[test]
fn test_truthy_non_empty_string() {
    assert!(eval("x", &[("x", "hello")]));
}

#[test]
fn test_falsy_empty_string() {
    assert!(!eval("x", &[("x", "")]));
}

#[test]
fn test_falsy_false_string() {
    // "false" as a string is coerced to bool false
    assert!(!eval("x", &[("x", "false")]));
}

#[test]
fn test_truthy_nonzero() {
    assert!(eval("x", &[("x", "42")]));
}

#[test]
fn test_falsy_zero() {
    assert!(!eval("x", &[("x", "0")]));
}

// === Missing variables (lenient) ===

#[test]
fn test_missing_variable_returns_false() {
    assert!(!eval("nonexistent", &[]));
}

#[test]
fn test_not_missing_variable_returns_true() {
    // `not undefined_var` should be true (undefined is falsy, negated = true)
    assert!(eval("not x", &[]));
}

#[test]
fn test_not_missing_dotted_variable_returns_true() {
    // `not data.skip` where data is not defined should be true
    assert!(eval("not data.skip", &[]));
}

// === Complex expressions ===

#[test]
fn test_complex_expression() {
    assert!(eval(
        r#"a > 5 && (b == "yes" || c)"#,
        &[("a", "10"), ("b", "no"), ("c", "true")]
    ));
    assert!(!eval(
        r#"a > 5 && (b == "yes" || c)"#,
        &[("a", "10"), ("b", "no"), ("c", "false")]
    ));
}

#[test]
fn test_complex_hook_condition() {
    // Real-world hook condition from hooks.yaml (bare names, no $ prefix)
    assert!(eval(
        r#"event.task.id == session.thread.tail && session.mode == "interactive""#,
        &[
            ("event.task.id", "abc"),
            ("session.thread.tail", "abc"),
            ("session.mode", "interactive")
        ]
    ));
}

// === Preprocessing unit tests ===

#[test]
fn test_strip_dollar_prefixes() {
    assert_eq!(strip_dollar_prefixes("$event.write"), "event.write");
    assert_eq!(
        strip_dollar_prefixes("$event.task.id == $session.task.id"),
        "event.task.id == session.task.id"
    );
    assert_eq!(strip_dollar_prefixes(r#"$x == "hello""#), r#"x == "hello""#);
    // Don't strip $ inside strings
    assert_eq!(
        strip_dollar_prefixes(r#"x == "$event""#),
        r#"x == "$event""#
    );
}

#[test]
fn test_rewrite_word_operators() {
    assert_eq!(rewrite_word_operators("a and b"), "a && b");
    assert_eq!(rewrite_word_operators("a or b"), "a || b");
    assert_eq!(rewrite_word_operators("not a"), "!a");
    // Word boundaries
    assert_eq!(rewrite_word_operators("band"), "band");
    assert_eq!(rewrite_word_operators("orange"), "orange");
    assert_eq!(rewrite_word_operators("notice"), "notice");
    assert_eq!(rewrite_word_operators("android"), "android");
    // Inside strings - should not rewrite
    assert_eq!(rewrite_word_operators(r#"x == "and""#), r#"x == "and""#);
}

// === build_scope_from_flat ===

#[test]
fn test_build_scope_simple() {
    let vars: BTreeMap<String, String> =
        [("x".to_string(), "42".to_string())].into_iter().collect();
    let scope = build_scope_from_flat(&vars);
    assert_eq!(scope.len(), 1);
}

#[test]
fn test_build_scope_nested() {
    let vars: BTreeMap<String, String> = [
        ("event.task.type".to_string(), "review".to_string()),
        ("event.write".to_string(), "true".to_string()),
    ]
    .into_iter()
    .collect();
    let scope = build_scope_from_flat(&vars);
    // Should create one top-level "event" map
    assert_eq!(scope.len(), 1);
}

// === coerce_to_dynamic ===

#[test]
fn test_coerce_bool() {
    let d = coerce_to_dynamic("true");
    assert!(d.is_bool());
    assert_eq!(d.as_bool().unwrap(), true);

    let d = coerce_to_dynamic("false");
    assert!(d.is_bool());
    assert_eq!(d.as_bool().unwrap(), false);
}

#[test]
fn test_coerce_int() {
    let d = coerce_to_dynamic("42");
    assert!(d.is_int());
    assert_eq!(d.as_int().unwrap(), 42);
}

#[test]
fn test_coerce_float() {
    let d = coerce_to_dynamic("3.14");
    assert!(d.is_float());
}

#[test]
fn test_coerce_string() {
    let d = coerce_to_dynamic("hello");
    assert!(d.is_string());
}

#[test]
fn test_coerce_empty_string() {
    let d = coerce_to_dynamic("");
    assert!(d.is_string());
    assert_eq!(d.into_string().unwrap(), "");
}

// === Array truthiness ===

#[test]
fn test_empty_array_is_falsy() {
    let mut evaluator = ExpressionEvaluator::new();
    let mut scope = Scope::new();
    let items: Array = vec![];
    scope.push("items", items);

    assert!(!evaluator.evaluate("items", &mut scope).unwrap());
}

#[test]
fn test_non_empty_array_is_truthy() {
    let mut evaluator = ExpressionEvaluator::new();
    let mut scope = Scope::new();
    let items: Array = vec![Dynamic::from(1)];
    scope.push("items", items);

    assert!(evaluator.evaluate("items", &mut scope).unwrap());
}

// === Array indexing (Rhai-native, requires typed scope) ===

#[test]
fn test_array_indexing() {
    let mut evaluator = ExpressionEvaluator::new();
    let mut scope = Scope::new();

    // Build a subtasks array: [{approved: false}, {approved: false}, {approved: true}]
    let subtasks: Array = vec![
        {
            let mut m: Map = BTreeMap::new();
            m.insert("approved".into(), Dynamic::from(false));
            Dynamic::from_map(m)
        },
        {
            let mut m: Map = BTreeMap::new();
            m.insert("approved".into(), Dynamic::from(false));
            Dynamic::from_map(m)
        },
        {
            let mut m: Map = BTreeMap::new();
            m.insert("approved".into(), Dynamic::from(true));
            Dynamic::from_map(m)
        },
    ];
    scope.push("subtasks", subtasks);

    assert!(evaluator
        .evaluate("subtasks[2].approved", &mut scope)
        .unwrap());
    assert!(!evaluator
        .evaluate("subtasks[0].approved", &mut scope)
        .unwrap());
}

#[test]
fn test_array_length() {
    let mut evaluator = ExpressionEvaluator::new();
    let mut scope = Scope::new();
    let items: Array = vec![Dynamic::from(1), Dynamic::from(2), Dynamic::from(3)];
    scope.push("items", items);

    assert!(evaluator.evaluate("items.len() >= 3", &mut scope).unwrap());
    assert!(!evaluator.evaluate("items.len() > 3", &mut scope).unwrap());
}

// === Escaped quotes in strings ===

#[test]
fn test_strip_dollar_prefixes_escaped_quotes() {
    // $var inside a string with escaped quotes should not be stripped
    assert_eq!(
        strip_dollar_prefixes(r#"x == "she said \"$event\" ok""#),
        r#"x == "she said \"$event\" ok""#
    );
    // Escaped quote in single-quoted string
    assert_eq!(
        strip_dollar_prefixes(r"x == 'it\'s $var here'"),
        r"x == 'it\'s $var here'"
    );
}

#[test]
fn test_rewrite_word_operators_escaped_quotes() {
    // "and" inside a string with escaped quotes should not be rewritten
    assert_eq!(
        rewrite_word_operators(r#"x == "she said \"and\" ok" and y"#),
        r#"x == "she said \"and\" ok" && y"#
    );
    // "or" after escaped quote should stay inside string
    assert_eq!(
        rewrite_word_operators(r#"x == "test \"or\" value""#),
        r#"x == "test \"or\" value""#
    );
}

#[test]
fn test_ensure_scope_defaults_escaped_quotes() {
    // Variable names inside strings with escaped quotes should not get defaults
    let expr = r#"x == "has \"data\" inside""#;
    let mut scope = Scope::new();
    scope.push("x", "hello".to_string());
    ensure_scope_defaults(expr, &mut scope);
    // "data" is inside a string literal, should not be added to scope
    assert!(!scope.contains("data"));
}

// === Existing Map with missing sub-paths ===

#[test]
fn test_missing_subpath_on_existing_map() {
    // Reproduces: `data.options.fix` when `data` exists (from `data.scope.kind`)
    // but `data.options` does not. Previously caused a Rhai error:
    //   "Unknown property 'fix' - a getter is not registered for type '()'"
    assert!(!eval(
        "data.options.fix",
        &[
            ("data.scope.kind", "task"),
            ("data.scope.name", "my-review")
        ],
    ));
}

#[test]
fn test_not_missing_subpath_on_existing_map() {
    // Negated form: `not data.options.fix` should be true when path is absent
    assert!(eval("not data.options.fix", &[("data.scope.kind", "task")],));
}

#[test]
fn test_present_subpath_on_existing_map() {
    // When the path IS set, it should evaluate to true
    assert!(eval(
        "data.options.fix",
        &[("data.scope.kind", "task"), ("data.options.fix", "true")],
    ));
}

// === Compile-once-eval-many ===

#[test]
fn test_compile_once_eval_many() {
    let mut evaluator = ExpressionEvaluator::new();
    let expr = "x > threshold";

    // Evaluate the same expression with different variable values
    for i in 0..5 {
        let vars: BTreeMap<String, String> = [
            ("x".to_string(), i.to_string()),
            ("threshold".to_string(), "3".to_string()),
        ]
        .into_iter()
        .collect();
        let mut scope = build_scope_from_flat(&vars);
        let result = evaluator.evaluate(expr, &mut scope).unwrap();
        assert_eq!(result, i > 3, "Expected x={} > 3 to be {}", i, i > 3);
    }
}
