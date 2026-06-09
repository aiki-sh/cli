use rhai::{Dynamic, Engine, Map, Scope, Token, AST};
use std::collections::{BTreeMap, HashMap};

/// Unified expression evaluator backed by Rhai.
///
/// Evaluates condition expressions using Rhai's expression-only mode.
/// Supports dotted variable access via nested object maps, `$var` prefix
/// stripping (deprecated), and word operator rewriting (`and`/`or`/`not`).
///
/// Compiled ASTs are cached so that repeated evaluation of the same expression
/// (e.g., hook conditions checked every turn) only parses once.
pub struct ExpressionEvaluator {
    engine: Engine,
    cache: HashMap<String, AST>,
}

impl std::fmt::Debug for ExpressionEvaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpressionEvaluator")
            .field("cached_expressions", &self.cache.len())
            .finish()
    }
}

impl Clone for ExpressionEvaluator {
    fn clone(&self) -> Self {
        // Create a fresh evaluator — the AST cache is an optimization
        // that will be rebuilt as expressions are evaluated.
        Self::new()
    }
}

impl ExpressionEvaluator {
    pub fn new() -> Self {
        let mut engine = Engine::new();
        // Return false for undefined variables so that:
        // - Simple undefined vars are falsy
        // - `not undefined_var` → `!false` → true
        // - Boolean operators short-circuit correctly with undefined vars
        // Remap reserved keywords (like "thread") to identifiers when used
        // as property names in dotted access (e.g., session.thread.tail).
        #[allow(deprecated)]
        engine.on_parse_token(|token, _pos, _state| match token {
            Token::Reserved(ref s) if &**s == "thread" => Token::Identifier(s.clone()),
            _ => token,
        });

        #[allow(deprecated)]
        engine.on_var(|name, _index, context| {
            if context.scope().contains(name) {
                Ok(None)
            } else {
                Ok(Some(Dynamic::from(false)))
            }
        });
        Self {
            engine,
            cache: HashMap::new(),
        }
    }

    /// Evaluate an expression string against a Rhai scope, returning a boolean.
    ///
    /// The expression is pre-processed to:
    /// - Strip `$` prefixes from variable names
    /// - Rewrite word operators (`and`, `or`, `not`) to symbols (`&&`, `||`, `!`)
    ///
    /// Compiled ASTs are cached by preprocessed expression text, so repeated
    /// evaluations of the same condition skip parsing.
    ///
    /// The result is coerced to bool using Rhai's truthiness rules:
    /// - `true` → true, `false` → false
    /// - `()` (unit) → false
    /// - `0` / `0.0` → false, non-zero → true
    /// - `""` → false, non-empty string → true
    /// - On evaluation error → false (lenient mode)
    pub fn evaluate(&mut self, expr: &str, scope: &mut Scope) -> Result<bool, String> {
        let processed = preprocess_expression(expr);

        // Ensure all referenced variables have defaults in scope so that
        // expressions like `!data.skip` work when `data` is undefined
        // (without this, `data` would resolve to `false` via on_var,
        // and `false.skip` would error before `!` could be applied).
        ensure_scope_defaults(&processed, scope);

        // Try cached AST first, fall back to compile + cache
        if let Some(ast) = self.cache.get(&processed) {
            match self.engine.eval_ast_with_scope::<Dynamic>(scope, ast) {
                Ok(result) => return Ok(dynamic_to_bool(&result)),
                Err(err) => {
                    eprintln!(
                        "[aiki] Warning: condition evaluation failed (defaulting to false): `{}` — {}",
                        expr.trim(),
                        err
                    );
                    return Ok(false);
                }
            }
        }

        match self.engine.compile_expression(&processed) {
            Ok(ast) => {
                let result = match self.engine.eval_ast_with_scope::<Dynamic>(scope, &ast) {
                    Ok(result) => Ok(dynamic_to_bool(&result)),
                    Err(err) => {
                        eprintln!(
                            "[aiki] Warning: condition evaluation failed (defaulting to false): `{}` — {}",
                            expr.trim(),
                            err
                        );
                        Ok(false)
                    }
                };
                self.cache.insert(processed, ast);
                result
            }
            Err(err) => {
                eprintln!(
                    "[aiki] Warning: condition evaluation failed (defaulting to false): `{}` — {}",
                    expr.trim(),
                    err
                );
                Ok(false)
            }
        }
    }
}

impl Default for ExpressionEvaluator {
    fn default() -> Self {
        Self::new()
    }
}

/// Pre-process an expression before Rhai evaluation.
///
/// 1. Strip `$` prefix from variable references
/// 2. Rewrite word operators to symbols (whole-word only)
pub fn preprocess_expression(expr: &str) -> String {
    let stripped = strip_dollar_prefixes(expr);
    rewrite_word_operators(&stripped)
}

/// Check if an expression uses deprecated `$var` syntax.
///
/// Returns true if the expression contains `$` prefixed variable references
/// that would be stripped during preprocessing.
pub fn uses_dollar_syntax(expr: &str) -> bool {
    let stripped = strip_dollar_prefixes(expr);
    stripped != expr
}

/// Strip `$` prefix from variable references.
///
/// `$event.write` → `event.write`
/// `$task_count` → `task_count`
///
/// Only strips `$` when followed by a letter or underscore (not `$"string"`).
fn strip_dollar_prefixes(expr: &str) -> String {
    let mut result = String::with_capacity(expr.len());
    let chars: Vec<char> = expr.chars().collect();
    let mut i = 0;
    let mut in_string = false;
    let mut string_char = '"';

    while i < chars.len() {
        let c = chars[i];

        // Track string boundaries
        if !in_string && (c == '"' || c == '\'') {
            in_string = true;
            string_char = c;
            result.push(c);
            i += 1;
            continue;
        }
        if in_string && c == '\\' {
            // Skip escaped character (e.g. \" or \')
            result.push(c);
            i += 1;
            if i < chars.len() {
                result.push(chars[i]);
                i += 1;
            }
            continue;
        }
        if in_string && c == string_char {
            in_string = false;
            result.push(c);
            i += 1;
            continue;
        }
        if in_string {
            result.push(c);
            i += 1;
            continue;
        }

        // Strip $ when followed by letter or underscore
        if c == '$' && i + 1 < chars.len() && (chars[i + 1].is_alphabetic() || chars[i + 1] == '_')
        {
            // Skip the $
            i += 1;
            continue;
        }

        result.push(c);
        i += 1;
    }

    result
}

/// Rewrite word operators to symbols, respecting word boundaries.
///
/// `and` → `&&`, `or` → `||`, `not` → `!`
///
/// Only matches whole words to avoid `"band"` → `"b&&"`.
fn rewrite_word_operators(expr: &str) -> String {
    let mut result = String::with_capacity(expr.len());
    let bytes = expr.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_string = false;
    let mut string_char = b'"';

    while i < len {
        // Track string boundaries
        if !in_string && (bytes[i] == b'"' || bytes[i] == b'\'') {
            in_string = true;
            string_char = bytes[i];
            result.push(bytes[i] as char);
            i += 1;
            continue;
        }
        if in_string && bytes[i] == b'\\' {
            // Skip escaped character (e.g. \" or \')
            result.push(bytes[i] as char);
            i += 1;
            if i < len {
                result.push(bytes[i] as char);
                i += 1;
            }
            continue;
        }
        if in_string && bytes[i] == string_char {
            in_string = false;
            result.push(bytes[i] as char);
            i += 1;
            continue;
        }
        if in_string {
            result.push(bytes[i] as char);
            i += 1;
            continue;
        }

        // Check for "and" (word boundary on both sides)
        if i + 3 <= len && &bytes[i..i + 3] == b"and" && is_word_boundary(bytes, i, 3, len) {
            result.push_str("&&");
            i += 3;
            continue;
        }

        // Check for "or" (word boundary on both sides)
        if i + 2 <= len && &bytes[i..i + 2] == b"or" && is_word_boundary(bytes, i, 2, len) {
            result.push_str("||");
            i += 2;
            continue;
        }

        // Check for "not" followed by word boundary (prefix operator)
        if i + 3 <= len && &bytes[i..i + 3] == b"not" && is_word_boundary(bytes, i, 3, len) {
            result.push('!');
            i += 3;
            // Skip trailing space after "not" since ! doesn't need it
            if i < len && bytes[i] == b' ' {
                i += 1;
            }
            continue;
        }

        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

/// Check if a keyword at position `start` with length `kw_len` has word boundaries.
fn is_word_boundary(bytes: &[u8], start: usize, kw_len: usize, len: usize) -> bool {
    let before_ok = start == 0 || !is_ident_char(bytes[start - 1]);
    let after_ok = start + kw_len >= len || !is_ident_char(bytes[start + kw_len]);
    before_ok && after_ok
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Ensure all variable references in an expression have defaults in scope.
///
/// Extracts identifier tokens from the preprocessed expression and adds falsy
/// defaults for any not present in scope. Simple vars get `false`. Dotted paths
/// like `data.skip` get a nested Map with `false` at the leaf, so that the
/// full expression evaluates to `false` (and `!data.skip` evaluates to `true`).
fn ensure_scope_defaults(expr: &str, scope: &mut Scope) {
    let bytes = expr.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_string = false;
    let mut string_char = b'"';

    // Collect dotted paths per top-level identifier: "data" → ["skip", "type.name"]
    let mut needed: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();

    while i < len {
        // Track string boundaries
        if !in_string && (bytes[i] == b'"' || bytes[i] == b'\'') {
            in_string = true;
            string_char = bytes[i];
            i += 1;
            continue;
        }
        if in_string && bytes[i] == b'\\' {
            // Skip escaped character (e.g. \" or \')
            i += 2;
            continue;
        }
        if in_string && bytes[i] == string_char {
            in_string = false;
            i += 1;
            continue;
        }
        if in_string {
            i += 1;
            continue;
        }

        // Found start of identifier
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            // Collect the full dotted path: ident(.ident)*
            let mut segments = Vec::new();
            loop {
                let start = i;
                while i < len && is_ident_char(bytes[i]) {
                    i += 1;
                }
                segments.push(expr[start..i].to_string());
                if i < len
                    && bytes[i] == b'.'
                    && i + 1 < len
                    && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_')
                {
                    i += 1; // skip the dot
                } else {
                    break;
                }
            }

            let top = &segments[0];

            // Skip Rhai keywords
            if matches!(
                top.as_str(),
                "true" | "false" | "if" | "else" | "in" | "let" | "const" | "fn" | "this"
            ) {
                continue;
            }

            let rest: Vec<String> = segments[1..].to_vec();
            needed.entry(top.clone()).or_default().push(rest);

            continue;
        }

        i += 1;
    }

    // Add defaults for each referenced top-level variable
    for (top, paths) in &needed {
        let has_dotted = paths.iter().any(|p| !p.is_empty());

        if scope.contains(top) {
            // Top-level exists — merge missing sub-paths into the existing Map
            // so that e.g. `data.options.fix` doesn't error when `data` exists
            // (from `data.scope.kind`) but `data.options` does not.
            if has_dotted {
                if let Some(existing) = scope.get_value::<Dynamic>(top) {
                    if existing.is_map() {
                        let mut map = existing.cast::<Map>();
                        let mut modified = false;
                        for path in paths {
                            if path.is_empty() {
                                continue;
                            }
                            modified |= ensure_path_exists(&mut map, path);
                        }
                        if modified {
                            scope.push_dynamic(top.as_str(), Dynamic::from_map(map));
                        }
                    }
                }
            }
        } else {
            // Top-level missing — create a new entry
            if has_dotted {
                let mut map: Map = BTreeMap::new();
                for path in paths {
                    if path.is_empty() {
                        continue;
                    }
                    insert_false_at_path(&mut map, path);
                }
                scope.push_dynamic(top.as_str(), Dynamic::from_map(map));
            } else {
                scope.push_dynamic(top.as_str(), Dynamic::from(false));
            }
        }
    }
}

/// Insert `false` at a nested path within a Map.
///
/// For path `["skip"]`, sets `map["skip"] = false`.
/// For path `["task", "type"]`, sets `map["task"]["type"] = false`.
fn insert_false_at_path(map: &mut Map, path: &[String]) {
    if path.len() == 1 {
        let key = path[0].as_str();
        map.entry(key.into())
            .or_insert_with(|| Dynamic::from(false));
    } else if path.len() > 1 {
        let key = path[0].as_str();
        let entry = map
            .entry(key.into())
            .or_insert_with(|| Dynamic::from_map(Map::new()));
        if let Some(inner_map) = entry.write_lock::<Map>() {
            let mut inner = inner_map;
            insert_false_at_path(&mut inner, &path[1..]);
        }
    }
}

/// Ensure a nested path exists within an existing Map, inserting `false` at
/// the leaf only if the path (or any intermediate segment) is missing.
///
/// Returns `true` if any insertion was made.
fn ensure_path_exists(map: &mut Map, path: &[String]) -> bool {
    if path.is_empty() {
        return false;
    }
    let key = path[0].as_str();

    if path.len() == 1 {
        // Leaf — insert false only if absent
        if map.contains_key(key) {
            return false;
        }
        map.insert(key.into(), Dynamic::from(false));
        return true;
    }

    // Intermediate — ensure sub-map exists, then recurse
    if !map.contains_key(key) {
        // Missing intermediate: create a nested map with false at leaf
        let mut sub = Map::new();
        insert_false_at_path(&mut sub, &path[1..]);
        map.insert(key.into(), Dynamic::from_map(sub));
        return true;
    }

    // Key exists — try to recurse into it if it's a Map
    let entry = map.get_mut(key).unwrap();
    if let Some(mut inner) = entry.write_lock::<Map>() {
        ensure_path_exists(&mut inner, &path[1..])
    } else {
        // Existing value isn't a Map (e.g. it's a string/bool) — don't overwrite
        false
    }
}

/// Build a nested Rhai scope from flat key-value pairs.
///
/// Given `{"event.task.type": "review", "event.write": "true", "count": "5"}`,
/// builds scope with:
/// - `event` → Map { "task" → Map { "type" → "review" }, "write" → true }
/// - `count` → 5
///
/// Values are coerced: "true"/"false" → bool, numeric strings → INT/FLOAT,
/// everything else → String.
pub fn build_scope_from_flat(vars: &BTreeMap<String, String>) -> Scope<'static> {
    let mut scope = Scope::new();

    // Group by top-level key
    let mut top_level: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut simple: BTreeMap<String, String> = BTreeMap::new();

    for (key, value) in vars {
        if let Some(dot_pos) = key.find('.') {
            let top = &key[..dot_pos];
            let rest = &key[dot_pos + 1..];
            top_level
                .entry(top.to_string())
                .or_default()
                .insert(rest.to_string(), value.clone());
        } else {
            simple.insert(key.clone(), value.clone());
        }
    }

    // Push simple vars
    for (key, value) in &simple {
        scope.push_dynamic(key.as_str(), coerce_to_dynamic(value));
    }

    // Push nested vars as object maps
    for (top_key, nested) in &top_level {
        let map = build_nested_map(nested);
        scope.push_dynamic(top_key.as_str(), Dynamic::from_map(map));
    }

    scope
}

/// Build a nested Rhai Map from flat dotted keys.
///
/// Given `{"task.type": "review", "write": "true"}`, returns:
/// Map { "task" → Map { "type" → "review" }, "write" → true }
fn build_nested_map(vars: &BTreeMap<String, String>) -> Map {
    let mut map: Map = BTreeMap::new();

    // Group by first segment
    let mut sub_groups: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    for (key, value) in vars {
        if let Some(dot_pos) = key.find('.') {
            let first = &key[..dot_pos];
            let rest = &key[dot_pos + 1..];
            sub_groups
                .entry(first.to_string())
                .or_default()
                .insert(rest.to_string(), value.clone());
        } else {
            map.insert(key.as_str().into(), coerce_to_dynamic(value));
        }
    }

    for (key, nested) in &sub_groups {
        let nested_map = build_nested_map(nested);
        map.insert(key.as_str().into(), Dynamic::from_map(nested_map));
    }

    map
}

/// Coerce a string value to an appropriate Rhai Dynamic type.
///
/// - "true" / "false" → bool
/// - Integer-parseable → i64
/// - Float-parseable → f64
/// - Everything else → String
pub fn coerce_to_dynamic(s: &str) -> Dynamic {
    match s {
        "true" => Dynamic::from(true),
        "false" => Dynamic::from(false),
        "" => Dynamic::from(String::new()),
        _ => {
            if let Ok(i) = s.parse::<i64>() {
                Dynamic::from(i)
            } else if let Ok(f) = s.parse::<f64>() {
                Dynamic::from(f)
            } else {
                Dynamic::from(s.to_string())
            }
        }
    }
}

/// Coerce a Rhai Dynamic value to a boolean.
///
/// Truthiness rules:
/// - bool: direct value
/// - () (unit): false
/// - i64: 0 → false, non-zero → true
/// - f64: 0.0 → false, non-zero → true
/// - String: "" → false, "false" (case-insensitive) → false,
///   "0" → false, "null" (case-insensitive) → false, non-empty → true
/// - Map: always true (exists)
/// - Array: empty → false, non-empty → true
fn dynamic_to_bool(val: &Dynamic) -> bool {
    if val.is_bool() {
        val.as_bool().unwrap_or(false)
    } else if val.is_unit() {
        false
    } else if val.is_int() {
        val.as_int().unwrap_or(0) != 0
    } else if val.is_float() {
        val.as_float().unwrap_or(0.0) != 0.0
    } else if val.is_string() {
        let s = val.clone().into_string().unwrap_or_default();
        let s_lower = s.to_lowercase();
        !s.is_empty() && s_lower != "false" && s != "0" && s_lower != "null"
    } else if val.is_array() {
        val.read_lock::<rhai::Array>()
            .map(|arr| !arr.is_empty())
            .unwrap_or(false)
    } else {
        // Maps, etc. are truthy
        true
    }
}

#[cfg(test)]
mod tests;
