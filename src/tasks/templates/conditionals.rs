//! Conditional logic for task templates
//!
//! Provides Tera-style conditional syntax for templates:
//! - `{% if condition %}...{% endif %}`
//! - `{% if condition %}...{% else %}...{% endif %}`
//! - `{% if condition %}...{% elif condition %}...{% else %}...{% endif %}`
//!
//! Supports:
//! - Comparison operators: `==`, `!=`, `>`, `<`, `>=`, `<=`
//! - Boolean operators: `and`, `or`, `not`
//! - Parentheses for grouping
//! - Truthy checks (variable existence)

use std::collections::HashMap;
use std::iter::Peekable;

/// Errors that can occur during conditional parsing or evaluation
#[derive(Debug, Clone, PartialEq)]
pub enum ConditionalError {
    /// Unclosed conditional block
    UnclosedBlock { line: usize },
    /// Unclosed loop block
    UnclosedLoop { line: usize },
    /// Unexpected token (e.g., {% else %} without {% if %})
    UnexpectedToken { token: String, line: usize },
    /// Invalid condition syntax
    InvalidCondition { condition: String, line: usize },
    /// Mismatched delimiters
    MismatchedDelimiters {
        expected: String,
        found: String,
        line: usize,
    },
    /// Single-brace syntax detected (migration needed)
    SingleBraceSyntax { variable: String, line: usize },
    /// Invalid loop syntax
    InvalidLoopSyntax { details: String, line: usize },
    /// Invalid loop variable name
    InvalidLoopVariable { name: String, line: usize },
}

impl std::fmt::Display for ConditionalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConditionalError::UnclosedBlock { line } => {
                write!(f, "Unclosed conditional block starting at line {}", line)
            }
            ConditionalError::UnclosedLoop { line } => {
                write!(
                    f,
                    "Unclosed loop starting at line {}. Expected {{% endfor %}}",
                    line
                )
            }
            ConditionalError::UnexpectedToken { token, line } => {
                write!(
                    f,
                    "Unexpected {{%% {} %}} at line {} without matching {{%% if %}}",
                    token, line
                )
            }
            ConditionalError::InvalidCondition { condition, line } => {
                write!(
                    f,
                    "Invalid condition at line {}: '{}'. Expected a valid expression, e.g.: 'var == \"value\"', 'var', '!var', 'a && b'",
                    line, condition
                )
            }
            ConditionalError::MismatchedDelimiters {
                expected,
                found,
                line,
            } => {
                write!(
                    f,
                    "Mismatched delimiters at line {}: expected '{}', found '{}'",
                    line, expected, found
                )
            }
            ConditionalError::SingleBraceSyntax { variable, line } => {
                write!(
                    f,
                    "Invalid template syntax at line {}: '{{{{{}}}}}'\n\n  Single-brace variable syntax is no longer supported.\n  Please update to double-brace syntax: '{{{{{{{}}}}}}}'",
                    line, variable, variable
                )
            }
            ConditionalError::InvalidLoopSyntax { details, line } => {
                write!(
                    f,
                    "Invalid loop syntax at line {}: {}. Expected: {{% for var in collection %}}",
                    line, details
                )
            }
            ConditionalError::InvalidLoopVariable { name, line } => {
                write!(
                    f,
                    "Invalid loop variable '{}' at line {}. Must match [a-z_][a-z0-9_]*",
                    name, line
                )
            }
        }
    }
}

impl std::error::Error for ConditionalError {}

/// Token types from the tokenizer
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// Plain text content
    Text(String),
    /// Variable reference: {{ var }}
    Variable(String),
    /// Control block: {% if/elif/else/endif %}
    ControlBlock(String),
}

/// Parsed template AST node
#[derive(Debug, Clone, PartialEq)]
pub enum TemplateNode {
    /// Plain text (to be output as-is)
    Text(String),
    /// Variable reference (to be substituted)
    Variable(String),
    /// Conditional block with branches
    Conditional {
        /// The if condition (raw expression string) and its content
        if_branch: (String, Vec<TemplateNode>),
        /// Optional elif branches
        elif_branches: Vec<(String, Vec<TemplateNode>)>,
        /// Optional else branch
        else_branch: Option<Vec<TemplateNode>>,
    },
    /// Loop block for iteration
    Loop {
        /// Loop variable name (e.g., "item")
        variable: String,
        /// Collection to iterate over (e.g., "source.comments")
        collection: String,
        /// Body to repeat for each item
        body: Vec<TemplateNode>,
        /// Optional else body for empty collections
        else_body: Option<Vec<TemplateNode>>,
    },
    /// Subtask reference for template composition
    SubtaskRef {
        /// Template name (e.g., "decompose", "review/plan")
        template_name: String,
        /// Key:value attributes (e.g., `needs-context:subtasks.explore`)
        attributes: HashMap<String, String>,
        /// Optional inline condition (e.g., `{% subtask decompose if data.needs_plan %}`)
        condition: Option<String>,
        /// Source line number for error reporting
        line: usize,
    },
}

/// Serialize template nodes back to template syntax
///
/// This preserves conditionals as `{% if %}` blocks instead of evaluating them,
/// allowing deferred evaluation (e.g., inside loop bodies that need iteration context).
pub fn nodes_to_template(nodes: &[TemplateNode]) -> String {
    let mut result = String::new();
    for node in nodes {
        result.push_str(&node_to_template(node));
    }
    result
}

/// Serialize a single template node to template syntax
fn node_to_template(node: &TemplateNode) -> String {
    match node {
        TemplateNode::Text(t) => t.clone(),
        TemplateNode::Variable(v) => format!("{{{{{}}}}}", v),
        TemplateNode::Conditional {
            if_branch,
            elif_branches,
            else_branch,
        } => {
            let mut result = String::new();
            result.push_str(&format!("{{% if {} %}}", &if_branch.0));
            result.push_str(&nodes_to_template(&if_branch.1));
            for (cond, content) in elif_branches {
                result.push_str(&format!("{{% elif {} %}}", cond));
                result.push_str(&nodes_to_template(content));
            }
            if let Some(else_content) = else_branch {
                result.push_str("{% else %}");
                result.push_str(&nodes_to_template(else_content));
            }
            result.push_str("{% endif %}");
            result
        }
        TemplateNode::Loop {
            variable,
            collection,
            body,
            else_body,
        } => {
            let mut result = String::new();
            result.push_str(&format!("{{% for {} in {} %}}", variable, collection));
            result.push_str(&nodes_to_template(body));
            if let Some(else_content) = else_body {
                result.push_str("{% else %}");
                result.push_str(&nodes_to_template(else_content));
            }
            result.push_str("{% endfor %}");
            result
        }
        TemplateNode::SubtaskRef {
            template_name,
            attributes,
            condition,
            ..
        } => {
            let mut parts = vec![template_name.clone()];
            // Serialize attributes, quoting values that contain spaces
            let mut sorted_keys: Vec<_> = attributes.keys().collect();
            sorted_keys.sort();
            for key in sorted_keys {
                let value = &attributes[key];
                if value.contains(' ') {
                    parts.push(format!("{}:\"{}\"", key, value));
                } else {
                    parts.push(format!("{}:{}", key, value));
                }
            }
            if let Some(cond) = condition {
                parts.push(format!("if {}", cond));
            }
            format!("{{% subtask {} %}}", parts.join(" "))
        }
    }
}

/// Context for evaluating conditions
#[derive(Debug, Clone, Default)]
pub struct EvalContext {
    /// All variables available for evaluation
    pub variables: HashMap<String, String>,
}

impl EvalContext {
    /// Create a new evaluation context
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a variable value
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.variables.insert(key.into(), value.into());
    }
}

/// Tokenize a template string into tokens
///
/// Recognizes:
/// - `{{` ... `}}` as Variable tokens
/// - `{%` ... `%}` as ControlBlock tokens
/// - Everything else as Text tokens
pub fn tokenize(input: &str) -> Result<Vec<Token>, ConditionalError> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    let mut current_text = String::new();
    let mut line = 1;

    while let Some(c) = chars.next() {
        if c == '\n' {
            current_text.push(c);
            line += 1;
            continue;
        }

        if c == '{' {
            match chars.peek() {
                Some(&'{') => {
                    // Start of variable: {{
                    chars.next();

                    // Flush any accumulated text
                    if !current_text.is_empty() {
                        tokens.push(Token::Text(std::mem::take(&mut current_text)));
                    }

                    // Read until }}
                    let mut var_content = String::new();
                    let start_line = line;
                    let mut found_close = false;

                    while let Some(c2) = chars.next() {
                        if c2 == '\n' {
                            line += 1;
                        }
                        if c2 == '}' {
                            if chars.peek() == Some(&'}') {
                                chars.next();
                                found_close = true;
                                break;
                            }
                        }
                        var_content.push(c2);
                    }

                    if !found_close {
                        return Err(ConditionalError::MismatchedDelimiters {
                            expected: "}}".to_string(),
                            found: "end of input".to_string(),
                            line: start_line,
                        });
                    }

                    tokens.push(Token::Variable(var_content.trim().to_string()));
                }
                Some(&'%') => {
                    // Start of control block: {%
                    chars.next();

                    // Flush any accumulated text
                    if !current_text.is_empty() {
                        tokens.push(Token::Text(std::mem::take(&mut current_text)));
                    }

                    // Read until %}
                    let mut block_content = String::new();
                    let start_line = line;
                    let mut found_close = false;

                    while let Some(c2) = chars.next() {
                        if c2 == '\n' {
                            line += 1;
                        }
                        if c2 == '%' {
                            if chars.peek() == Some(&'}') {
                                chars.next();
                                found_close = true;
                                break;
                            }
                        }
                        block_content.push(c2);
                    }

                    if !found_close {
                        return Err(ConditionalError::MismatchedDelimiters {
                            expected: "%}".to_string(),
                            found: "end of input".to_string(),
                            line: start_line,
                        });
                    }

                    tokens.push(Token::ControlBlock(block_content.trim().to_string()));
                }
                _ => {
                    // Check for single-brace variable syntax (deprecated)
                    // Look ahead to see if this might be {var}
                    let mut lookahead = String::new();
                    let mut temp_chars: Vec<char> = vec![c];
                    let mut found_single_close = false;

                    // Save current position by collecting chars until we see } or {{
                    for c2 in chars.by_ref() {
                        temp_chars.push(c2);
                        if c2 == '}' {
                            found_single_close = true;
                            break;
                        }
                        if c2 == '{' {
                            // Nested brace, not single-brace syntax
                            break;
                        }
                        if c2 == '\n' || c2 == ' ' || c2 == '\t' {
                            // Whitespace/newline means not a variable
                            break;
                        }
                        lookahead.push(c2);
                    }

                    // Check if this looks like a single-brace variable (e.g., {data.foo})
                    if found_single_close
                        && !lookahead.is_empty()
                        && lookahead
                            .chars()
                            .all(|ch| ch.is_alphanumeric() || ch == '.' || ch == '_')
                    {
                        return Err(ConditionalError::SingleBraceSyntax {
                            variable: lookahead,
                            line,
                        });
                    }

                    // Not a single-brace variable, add all chars to text
                    for tc in temp_chars {
                        if tc == '\n' {
                            line += 1;
                        }
                        current_text.push(tc);
                    }
                }
            }
        } else {
            current_text.push(c);
        }
    }

    // Flush remaining text
    if !current_text.is_empty() {
        tokens.push(Token::Text(current_text));
    }

    Ok(tokens)
}

/// Parse tokens into a template AST
pub fn parse(tokens: &[Token]) -> Result<Vec<TemplateNode>, ConditionalError> {
    let mut result = Vec::new();
    let mut iter = tokens.iter().peekable();
    let mut line = 1;

    while let Some(token) = iter.next() {
        // Track line numbers in text
        if let Token::Text(t) = token {
            line += t.chars().filter(|&c| c == '\n').count();
        }

        match token {
            Token::Text(t) => {
                result.push(TemplateNode::Text(t.clone()));
            }
            Token::Variable(v) => {
                result.push(TemplateNode::Variable(v.clone()));
            }
            Token::ControlBlock(block) => {
                let block_lower = block.to_lowercase();

                if block_lower.starts_with("if ") || block_lower == "if" {
                    // Parse if block
                    let condition_str = if block_lower == "if" {
                        ""
                    } else {
                        block[3..].trim()
                    };

                    validate_condition(condition_str, line)?;
                    let (if_content, elif_branches, else_branch) =
                        parse_if_body(&mut iter, &mut line)?;

                    result.push(TemplateNode::Conditional {
                        if_branch: (condition_str.to_string(), if_content),
                        elif_branches,
                        else_branch,
                    });
                } else if block_lower.starts_with("for ") {
                    // Parse for loop: {% for var in collection %}
                    let (variable, collection) = parse_for_header(&block[4..], line)?;
                    let (body, else_body) = parse_for_body(&mut iter, &mut line)?;

                    result.push(TemplateNode::Loop {
                        variable,
                        collection,
                        body,
                        else_body,
                    });
                } else if block_lower.starts_with("elif ") || block_lower == "elif" {
                    return Err(ConditionalError::UnexpectedToken {
                        token: "elif".to_string(),
                        line,
                    });
                } else if block_lower == "else" {
                    return Err(ConditionalError::UnexpectedToken {
                        token: "else".to_string(),
                        line,
                    });
                } else if block_lower == "endif" {
                    return Err(ConditionalError::UnexpectedToken {
                        token: "endif".to_string(),
                        line,
                    });
                } else if block_lower == "endfor" {
                    return Err(ConditionalError::UnexpectedToken {
                        token: "endfor".to_string(),
                        line,
                    });
                } else if block_lower.starts_with("subtask ") {
                    // Parse subtask reference: {% subtask <name> %} or {% subtask <name> if condition %}
                    let subtask_ref = parse_subtask_ref(&block[8..], line)?;
                    result.push(subtask_ref);
                }
                // Unknown control blocks are ignored (for future extensibility)
            }
        }
    }

    Ok(result)
}

/// Parse the body of an if block, collecting elif and else branches
fn parse_if_body<'a, I>(
    iter: &mut Peekable<I>,
    line: &mut usize,
) -> Result<
    (
        Vec<TemplateNode>,
        Vec<(String, Vec<TemplateNode>)>,
        Option<Vec<TemplateNode>>,
    ),
    ConditionalError,
>
where
    I: Iterator<Item = &'a Token>,
{
    let mut if_content: Vec<TemplateNode> = Vec::new();
    let mut elif_branches: Vec<(String, Vec<TemplateNode>)> = Vec::new();
    let mut else_branch: Option<Vec<TemplateNode>> = None;
    let start_line = *line;

    loop {
        let token = iter
            .next()
            .ok_or(ConditionalError::UnclosedBlock { line: start_line })?;

        // Track line numbers
        if let Token::Text(t) = token {
            *line += t.chars().filter(|&c| c == '\n').count();
        }

        match token {
            Token::Text(t) => {
                if else_branch.is_some() {
                    else_branch
                        .as_mut()
                        .unwrap()
                        .push(TemplateNode::Text(t.clone()));
                } else if !elif_branches.is_empty() {
                    elif_branches
                        .last_mut()
                        .unwrap()
                        .1
                        .push(TemplateNode::Text(t.clone()));
                } else {
                    if_content.push(TemplateNode::Text(t.clone()));
                }
            }
            Token::Variable(v) => {
                let node = TemplateNode::Variable(v.clone());
                if else_branch.is_some() {
                    else_branch.as_mut().unwrap().push(node);
                } else if !elif_branches.is_empty() {
                    elif_branches.last_mut().unwrap().1.push(node);
                } else {
                    if_content.push(node);
                }
            }
            Token::ControlBlock(block) => {
                let block_lower = block.to_lowercase();

                if block_lower == "endif" {
                    return Ok((if_content, elif_branches, else_branch));
                } else if block_lower.starts_with("elif ") || block_lower == "elif" {
                    if else_branch.is_some() {
                        return Err(ConditionalError::UnexpectedToken {
                            token: "elif after else".to_string(),
                            line: *line,
                        });
                    }

                    let condition_str = if block_lower == "elif" {
                        ""
                    } else {
                        block[5..].trim()
                    };
                    validate_condition(condition_str, *line)?;
                    elif_branches.push((condition_str.to_string(), Vec::new()));
                } else if block_lower == "else" {
                    if else_branch.is_some() {
                        return Err(ConditionalError::UnexpectedToken {
                            token: "multiple else".to_string(),
                            line: *line,
                        });
                    }
                    else_branch = Some(Vec::new());
                } else if block_lower.starts_with("if ") || block_lower == "if" {
                    // Nested if block
                    let condition_str = if block_lower == "if" {
                        ""
                    } else {
                        block[3..].trim()
                    };
                    validate_condition(condition_str, *line)?;
                    let (nested_if, nested_elif, nested_else) = parse_if_body(iter, line)?;

                    let nested_node = TemplateNode::Conditional {
                        if_branch: (condition_str.to_string(), nested_if),
                        elif_branches: nested_elif,
                        else_branch: nested_else,
                    };

                    if else_branch.is_some() {
                        else_branch.as_mut().unwrap().push(nested_node);
                    } else if !elif_branches.is_empty() {
                        elif_branches.last_mut().unwrap().1.push(nested_node);
                    } else {
                        if_content.push(nested_node);
                    }
                } else if block_lower.starts_with("subtask ") {
                    // Subtask reference inside conditional
                    let subtask_ref = parse_subtask_ref(&block[8..], *line)?;
                    if else_branch.is_some() {
                        else_branch.as_mut().unwrap().push(subtask_ref);
                    } else if !elif_branches.is_empty() {
                        elif_branches.last_mut().unwrap().1.push(subtask_ref);
                    } else {
                        if_content.push(subtask_ref);
                    }
                }
            }
        }
    }
}

/// Parse the header of a for loop: "var in collection"
///
/// Returns (variable_name, collection_path)
fn parse_for_header(header: &str, line: usize) -> Result<(String, String), ConditionalError> {
    let header = header.trim();

    // Find " in " to split variable and collection
    let in_pos = header.find(" in ");
    let (variable, collection) = match in_pos {
        Some(pos) => {
            let var = header[..pos].trim();
            let coll = header[pos + 4..].trim();
            (var, coll)
        }
        None => {
            return Err(ConditionalError::InvalidLoopSyntax {
                details: format!("missing 'in' keyword in '{}'", header),
                line,
            });
        }
    };

    // Validate variable name using centralized validation
    if !crate::validation::is_valid_template_identifier(variable) {
        return Err(ConditionalError::InvalidLoopVariable {
            name: variable.to_string(),
            line,
        });
    }

    // Validate collection is not empty
    if collection.is_empty() {
        return Err(ConditionalError::InvalidLoopSyntax {
            details: "collection cannot be empty".to_string(),
            line,
        });
    }

    Ok((variable.to_string(), collection.to_string()))
}

/// Parse the body of a for loop, collecting nodes until {% endfor %}
///
/// Also handles the optional {% else %} block for empty collections.
fn parse_for_body<'a, I>(
    iter: &mut Peekable<I>,
    line: &mut usize,
) -> Result<(Vec<TemplateNode>, Option<Vec<TemplateNode>>), ConditionalError>
where
    I: Iterator<Item = &'a Token>,
{
    let mut body: Vec<TemplateNode> = Vec::new();
    let mut else_body: Option<Vec<TemplateNode>> = None;
    let start_line = *line;
    let mut in_else = false;

    loop {
        let token = iter
            .next()
            .ok_or(ConditionalError::UnclosedLoop { line: start_line })?;

        // Track line numbers
        if let Token::Text(t) = token {
            *line += t.chars().filter(|&c| c == '\n').count();
        }

        match token {
            Token::Text(t) => {
                let node = TemplateNode::Text(t.clone());
                if in_else {
                    else_body.as_mut().unwrap().push(node);
                } else {
                    body.push(node);
                }
            }
            Token::Variable(v) => {
                let node = TemplateNode::Variable(v.clone());
                if in_else {
                    else_body.as_mut().unwrap().push(node);
                } else {
                    body.push(node);
                }
            }
            Token::ControlBlock(block) => {
                let block_lower = block.to_lowercase();

                if block_lower == "endfor" {
                    return Ok((body, else_body));
                } else if block_lower == "else" {
                    if in_else {
                        return Err(ConditionalError::UnexpectedToken {
                            token: "multiple else in for loop".to_string(),
                            line: *line,
                        });
                    }
                    in_else = true;
                    else_body = Some(Vec::new());
                } else if block_lower.starts_with("if ") || block_lower == "if" {
                    // Nested if block
                    let condition_str = if block_lower == "if" {
                        ""
                    } else {
                        block[3..].trim()
                    };
                    validate_condition(condition_str, *line)?;
                    let (if_content, elif_branches, else_branch) = parse_if_body(iter, line)?;

                    let nested_node = TemplateNode::Conditional {
                        if_branch: (condition_str.to_string(), if_content),
                        elif_branches,
                        else_branch,
                    };

                    if in_else {
                        else_body.as_mut().unwrap().push(nested_node);
                    } else {
                        body.push(nested_node);
                    }
                } else if block_lower.starts_with("for ") {
                    // Nested for loop
                    let (variable, collection) = parse_for_header(&block[4..], *line)?;
                    let (nested_body, nested_else) = parse_for_body(iter, line)?;

                    let nested_node = TemplateNode::Loop {
                        variable,
                        collection,
                        body: nested_body,
                        else_body: nested_else,
                    };

                    if in_else {
                        else_body.as_mut().unwrap().push(nested_node);
                    } else {
                        body.push(nested_node);
                    }
                } else if block_lower == "endif" {
                    return Err(ConditionalError::UnexpectedToken {
                        token: "endif".to_string(),
                        line: *line,
                    });
                } else if block_lower.starts_with("elif ") || block_lower == "elif" {
                    return Err(ConditionalError::UnexpectedToken {
                        token: "elif".to_string(),
                        line: *line,
                    });
                } else if block_lower.starts_with("subtask ") {
                    // Subtask reference inside loop
                    let subtask_ref = parse_subtask_ref(&block[8..], *line)?;
                    if in_else {
                        else_body.as_mut().unwrap().push(subtask_ref);
                    } else {
                        body.push(subtask_ref);
                    }
                }
                // Unknown control blocks are passed through
            }
        }
    }
}

/// Parse a subtask reference: `<template_name>` or `<template_name> if <condition>`
///
/// Template name segments: `[a-z0-9][a-z0-9._-]*` separated by `/`
/// Examples: `decompose`, `review/plan`, `myorg/v2.0`
fn parse_subtask_ref(content: &str, line: usize) -> Result<TemplateNode, ConditionalError> {
    let content = content.trim();

    if content.is_empty() {
        return Err(ConditionalError::InvalidCondition {
            condition: "{% subtask %} requires a template name".to_string(),
            line,
        });
    }

    // Parse: <template_name> [key:value ...] [if <condition>]
    // Step 1: Extract template name (first whitespace-delimited token)
    let (template_name, rest) = match content.find(char::is_whitespace) {
        Some(pos) => (&content[..pos], content[pos..].trim_start()),
        None => (content, ""),
    };

    // Step 2: Parse key:value attributes and optional condition from rest
    let mut attributes = HashMap::new();
    let mut condition = None;
    let mut remaining = rest;

    while !remaining.is_empty() {
        // Check for "if " condition marker (bare "if" keyword, not "if-something:value")
        if remaining == "if" || remaining.starts_with("if ") {
            let cond_str = remaining[2..].trim();
            if cond_str.is_empty() {
                return Err(ConditionalError::InvalidCondition {
                    condition: "{% subtask %} has 'if' without a condition expression".to_string(),
                    line,
                });
            }
            validate_condition(cond_str, line)?;
            condition = Some(cond_str.to_string());
            break;
        }

        // Try to parse a key:value pair
        if let Some(colon_pos) = remaining.find(':') {
            let before_colon = &remaining[..colon_pos];
            // Key must not contain whitespace
            if before_colon.contains(char::is_whitespace) {
                return Err(ConditionalError::InvalidCondition {
                    condition: format!(
                        "Unexpected token '{}' in {{% subtask %}}. Expected key:value or 'if'",
                        before_colon
                            .split_whitespace()
                            .next()
                            .unwrap_or(before_colon)
                    ),
                    line,
                });
            }

            let key = before_colon.to_string();
            let after_colon = &remaining[colon_pos + 1..];

            if after_colon.starts_with('"') {
                // Quoted value - find closing quote
                if let Some(end_quote) = after_colon[1..].find('"') {
                    let value = after_colon[1..1 + end_quote].to_string();
                    attributes.insert(key, value);
                    remaining = after_colon[2 + end_quote..].trim_start();
                } else {
                    return Err(ConditionalError::InvalidCondition {
                        condition: format!(
                            "Unclosed quote in attribute value for key '{}' in {{% subtask %}}",
                            key
                        ),
                        line,
                    });
                }
            } else {
                // Unquoted value - read until whitespace
                let end = after_colon
                    .find(char::is_whitespace)
                    .unwrap_or(after_colon.len());
                let value = after_colon[..end].to_string();
                attributes.insert(key, value);
                remaining = after_colon[end..].trim_start();
            }
        } else {
            // Token without colon and not "if" - error
            let token = remaining.split_whitespace().next().unwrap_or(remaining);
            return Err(ConditionalError::InvalidCondition {
                condition: format!(
                    "Unexpected token '{}' in {{% subtask %}}. Expected key:value or 'if'",
                    token
                ),
                line,
            });
        }
    }

    // Validate template name: segments separated by `/`, each segment matches [a-z0-9][a-z0-9._-]*
    // Skip character-level validation for segments containing {{...}} interpolation
    let has_interpolation = template_name.contains("{{");
    let segments: Vec<&str> = template_name.split('/').collect();
    if segments.is_empty() || segments.iter().any(|s| s.is_empty()) {
        return Err(ConditionalError::InvalidCondition {
            condition: format!(
                "Invalid template name '{}' in {{% subtask %}}. Expected: namespace/template",
                template_name
            ),
            line,
        });
    }
    if !has_interpolation {
        for segment in &segments {
            let chars: Vec<char> = segment.chars().collect();
            if !chars[0].is_ascii_lowercase() && !chars[0].is_ascii_digit() {
                return Err(ConditionalError::InvalidCondition {
                    condition: format!(
                        "Invalid template name segment '{}' in {{% subtask %}}. Segments must start with [a-z0-9]",
                        segment
                    ),
                    line,
                });
            }
            if !chars.iter().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '.' || *c == '_' || *c == '-'
            }) {
                return Err(ConditionalError::InvalidCondition {
                    condition: format!(
                        "Invalid template name segment '{}' in {{% subtask %}}. Segments may contain [a-z0-9._-]",
                        segment
                    ),
                    line,
                });
            }
        }
    }

    Ok(TemplateNode::SubtaskRef {
        template_name: template_name.to_string(),
        attributes,
        condition,
        line,
    })
}

/// Validate a condition string at parse time.
///
/// Checks that the condition is non-empty and is syntactically valid
/// Rhai expression syntax (after preprocessing). This catches typos and
/// malformed expressions early rather than silently evaluating to false.
fn validate_condition(condition_str: &str, line: usize) -> Result<(), ConditionalError> {
    let condition_str = condition_str.trim();

    if condition_str.is_empty() {
        return Err(ConditionalError::InvalidCondition {
            condition: "(empty)".to_string(),
            line,
        });
    }

    // Warn about deprecated $var syntax
    if crate::expressions::uses_dollar_syntax(condition_str) {
        eprintln!(
            // stderr-ok: template validation, never called during monitoring
            "[aiki] Warning: `$var` syntax is deprecated, use `var` instead: {}",
            condition_str
        );
    }

    // Pre-process the same way evaluation does, then try to compile
    let processed = crate::expressions::preprocess_expression(condition_str);
    let engine = rhai::Engine::new();
    if let Err(err) = engine.compile_expression(&processed) {
        return Err(ConditionalError::InvalidCondition {
            condition: format!("{} ({})", condition_str, err),
            line,
        });
    }

    Ok(())
}

/// Evaluate a condition expression string against a context using Rhai.
///
/// Supports `and`/`or`/`not` operators (rewritten to `&&`/`||`/`!`),
/// comparison operators, and dotted variable access.
///
/// Creates a fresh `ExpressionEvaluator` per call. For repeated evaluations
/// (e.g., during template rendering), use [`evaluate_condition_with`] with a
/// shared evaluator to benefit from AST caching.
#[cfg(test)]
pub fn evaluate_condition(condition_str: &str, ctx: &EvalContext) -> bool {
    let mut evaluator = crate::expressions::ExpressionEvaluator::new();
    evaluate_condition_with(condition_str, ctx, &mut evaluator)
}

/// Evaluate a condition using a shared `ExpressionEvaluator` for AST cache reuse.
fn evaluate_condition_with(
    condition_str: &str,
    ctx: &EvalContext,
    evaluator: &mut crate::expressions::ExpressionEvaluator,
) -> bool {
    let var_map: std::collections::BTreeMap<String, String> = ctx
        .variables
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut scope = crate::expressions::build_scope_from_flat(&var_map);
    evaluator
        .evaluate(condition_str, &mut scope)
        .unwrap_or(false)
}

/// Render a parsed template with the given context
///
/// Returns the rendered string with conditionals evaluated and variables left as-is
/// (for later substitution by the variable system)
pub fn render(nodes: &[TemplateNode], ctx: &EvalContext) -> String {
    let mut evaluator = crate::expressions::ExpressionEvaluator::new();
    render_with_loops(nodes, ctx, &HashMap::new(), &mut evaluator)
}

/// Internal render function that supports loop iteration context
fn render_with_loops(
    nodes: &[TemplateNode],
    ctx: &EvalContext,
    loop_vars: &HashMap<String, LoopItem>,
    evaluator: &mut crate::expressions::ExpressionEvaluator,
) -> String {
    let mut result = String::new();

    for node in nodes {
        match node {
            TemplateNode::Text(t) => {
                result.push_str(t);
            }
            TemplateNode::Variable(v) => {
                // Check if variable is a loop variable or loop metadata
                if let Some(value) = resolve_loop_variable(v, loop_vars) {
                    result.push_str(&value);
                } else {
                    // Leave variables as {{ var }} for later substitution
                    result.push_str("{{");
                    result.push_str(v);
                    result.push_str("}}");
                }
            }
            TemplateNode::Conditional {
                if_branch,
                elif_branches,
                else_branch,
            } => {
                // Evaluate conditions in order, considering loop variables
                let eval_ctx = make_loop_aware_context(ctx, loop_vars);
                if evaluate_condition_with(&if_branch.0, &eval_ctx, evaluator) {
                    result.push_str(&render_with_loops(&if_branch.1, ctx, loop_vars, evaluator));
                } else {
                    let mut matched = false;
                    for (cond_str, content) in elif_branches {
                        if evaluate_condition_with(cond_str, &eval_ctx, evaluator) {
                            result.push_str(&render_with_loops(content, ctx, loop_vars, evaluator));
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        if let Some(else_content) = else_branch {
                            result.push_str(&render_with_loops(
                                else_content,
                                ctx,
                                loop_vars,
                                evaluator,
                            ));
                        }
                    }
                }
            }
            TemplateNode::Loop {
                variable,
                collection,
                body,
                else_body,
            } => {
                // Get the collection items - for now we just pass through
                // The actual iteration is handled by the template resolver
                // Here we just emit the loop as markers for later processing
                result.push_str(&render_loop(
                    variable, collection, body, else_body, ctx, loop_vars, evaluator,
                ));
            }
            TemplateNode::SubtaskRef {
                template_name,
                attributes,
                condition,
                line,
            } => {
                // If there's an inline condition, evaluate it
                if let Some(cond_str) = condition {
                    let eval_ctx = make_loop_aware_context(ctx, loop_vars);
                    if !evaluate_condition_with(cond_str, &eval_ctx, evaluator) {
                        continue; // Condition false, skip this subtask ref
                    }
                }
                // Emit as a marker for the resolver to process later
                // Substitute any variables in the template name
                let mut resolved_name = template_name.clone();
                for (var_name, item) in loop_vars {
                    for (field, value) in &item.data {
                        let pattern = format!("{{{{{}.{}}}}}", var_name, field);
                        resolved_name = resolved_name.replace(&pattern, value);
                    }
                }
                // Substitute context variables (e.g., {{data.scope.kind}})
                for (key, value) in &ctx.variables {
                    let pattern = format!("{{{{{}}}}}", key);
                    resolved_name = resolved_name.replace(&pattern, value);
                }
                if attributes.is_empty() {
                    result.push_str(&format!(
                        "<!-- AIKI_SUBTASK_REF:{}:{} -->",
                        resolved_name, line
                    ));
                } else {
                    // Serialize attributes as key:val pairs separated by ;
                    // Percent-encode special chars in values to preserve spaces/semicolons/colons
                    let mut sorted_keys: Vec<_> = attributes.keys().collect();
                    sorted_keys.sort();
                    let attrs_str: Vec<String> = sorted_keys
                        .iter()
                        .map(|k| {
                            let v = &attributes[*k];
                            let encoded = v
                                .replace('%', "%25")
                                .replace(' ', "%20")
                                .replace(';', "%3B")
                                .replace(':', "%3A");
                            format!("{}:{}", k, encoded)
                        })
                        .collect();
                    result.push_str(&format!(
                        "<!-- AIKI_SUBTASK_REF:{}:{}:{} -->",
                        resolved_name,
                        line,
                        attrs_str.join(";")
                    ));
                }
            }
        }
    }

    // Apply whitespace cleanup rules
    cleanup_whitespace(&result)
}

/// Loop item containing the item data and loop metadata
#[derive(Debug, Clone)]
pub struct LoopItem {
    /// The current item's data (field -> value)
    pub data: HashMap<String, String>,
    /// Loop metadata
    pub iteration: usize, // 1-based iteration
    pub index: usize, // 0-based iteration
}

/// Resolve a variable that might be a loop variable or loop metadata
fn resolve_loop_variable(var: &str, loop_vars: &HashMap<String, LoopItem>) -> Option<String> {
    // Check for loop metadata: loop.index, loop.first, etc.
    if let Some(meta_key) = var.strip_prefix("loop.") {
        // Find the innermost loop (any key works, there should only be one active "loop")
        // Actually, loop.* always refers to the innermost loop
        if let Some((_, item)) = loop_vars.iter().next() {
            return match meta_key {
                "iteration" => Some(item.iteration.to_string()),
                "index" => Some(item.index.to_string()),
                _ => None,
            };
        }
        return None;
    }

    // Check for loop variable fields: item.field, comment.text, etc.
    let parts: Vec<&str> = var.splitn(2, '.').collect();
    if parts.len() == 2 {
        let var_name = parts[0];
        let field_name = parts[1];

        if let Some(item) = loop_vars.get(var_name) {
            return item.data.get(field_name).cloned();
        }
    }

    // Check if it's just the loop variable name (shouldn't happen in normal use)
    if loop_vars.contains_key(var) {
        // Return some representation - this case is unusual
        return None;
    }

    None
}

/// Create an EvalContext that includes loop variable values for condition evaluation
fn make_loop_aware_context(
    ctx: &EvalContext,
    loop_vars: &HashMap<String, LoopItem>,
) -> EvalContext {
    let mut new_ctx = ctx.clone();

    // Add loop metadata
    if let Some((_, item)) = loop_vars.iter().next() {
        new_ctx.set("loop.iteration", item.iteration.to_string());
        new_ctx.set("loop.index", item.index.to_string());
    }

    // Add loop variable fields
    for (var_name, item) in loop_vars {
        for (field, value) in &item.data {
            new_ctx.set(format!("{}.{}", var_name, field), value.clone());
        }
    }

    new_ctx
}

/// Render a loop - outputs markers that can be processed by the template resolver
///
/// For Phase 1, we emit the loop as structured markers. The template resolver
/// will handle actual iteration when processing templates.
///
/// IMPORTANT: We use `nodes_to_template` instead of `render_with_loops` for the body
/// to preserve conditionals as `{% if %}` syntax rather than evaluating them. This
/// allows conditionals inside loop bodies to be evaluated per-iteration in the resolver
/// when loop variables are available.
fn render_loop(
    variable: &str,
    collection: &str,
    body: &[TemplateNode],
    else_body: &Option<Vec<TemplateNode>>,
    _ctx: &EvalContext,
    _loop_vars: &HashMap<String, LoopItem>,
    _evaluator: &mut crate::expressions::ExpressionEvaluator,
) -> String {
    // Emit a special marker that the resolver can process
    // Format: <!-- AIKI_LOOP:var:collection --> body <!-- AIKI_ENDLOOP --> [<!-- AIKI_LOOPELSE --> else <!-- AIKI_ENDLOOPELSE -->]
    let mut result = String::new();

    result.push_str(&format!("<!-- AIKI_LOOP:{}:{} -->\n", variable, collection));

    // Serialize the body back to template syntax, preserving conditionals
    // for deferred evaluation during loop expansion
    result.push_str(&nodes_to_template(body));

    result.push_str("<!-- AIKI_ENDLOOP -->");

    // Include else body if present
    if let Some(else_nodes) = else_body {
        result.push_str("\n<!-- AIKI_LOOPELSE -->\n");
        result.push_str(&nodes_to_template(else_nodes));
        result.push_str("<!-- AIKI_ENDLOOPELSE -->");
    }

    result
}

/// Clean up whitespace introduced by conditional blocks
///
/// Rules (v1 - simple automatic strategy):
/// 1. Remove lines that contain only whitespace after conditional evaluation
/// 2. Collapse multiple consecutive blank lines to one
fn cleanup_whitespace(s: &str) -> String {
    let mut result = String::new();
    let mut prev_was_blank = false;

    for line in s.lines() {
        let is_blank = line.trim().is_empty();

        if is_blank {
            if !prev_was_blank {
                result.push('\n');
            }
            prev_was_blank = true;
        } else {
            if !result.is_empty() && !prev_was_blank {
                result.push('\n');
            }
            result.push_str(line);
            prev_was_blank = false;
        }
    }

    // Preserve trailing newline if original had one
    if s.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }

    result
}

/// Process a template string: tokenize, parse, and render
///
/// This is the main entry point for conditional processing.
/// Variables are left as `{{var}}` for later substitution.
pub fn process_conditionals(template: &str, ctx: &EvalContext) -> Result<String, ConditionalError> {
    let tokens = tokenize(template)?;
    let ast = parse(&tokens)?;
    Ok(render(&ast, ctx))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== Tokenizer Tests =====

    #[test]
    fn test_tokenize_plain_text() {
        let tokens = tokenize("Hello world").unwrap();
        assert_eq!(tokens, vec![Token::Text("Hello world".to_string())]);
    }

    #[test]
    fn test_tokenize_variable() {
        let tokens = tokenize("Hello {{name}}!").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Text("Hello ".to_string()),
                Token::Variable("name".to_string()),
                Token::Text("!".to_string()),
            ]
        );
    }

    #[test]
    fn test_tokenize_control_block() {
        let tokens = tokenize("{% if x %}yes{% endif %}").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::ControlBlock("if x".to_string()),
                Token::Text("yes".to_string()),
                Token::ControlBlock("endif".to_string()),
            ]
        );
    }

    #[test]
    fn test_tokenize_mixed() {
        let tokens = tokenize("Hello {% if show %}{{name}}{% endif %}!").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Text("Hello ".to_string()),
                Token::ControlBlock("if show".to_string()),
                Token::Variable("name".to_string()),
                Token::ControlBlock("endif".to_string()),
                Token::Text("!".to_string()),
            ]
        );
    }

    #[test]
    fn test_tokenize_single_brace_error() {
        let result = tokenize("Hello {name}!");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ConditionalError::SingleBraceSyntax { .. }));
    }

    #[test]
    fn test_tokenize_unclosed_variable() {
        let result = tokenize("Hello {{name");
        assert!(result.is_err());
    }

    // ===== Parser Tests =====

    #[test]
    fn test_parse_simple_if() {
        let tokens = tokenize("{% if x %}yes{% endif %}").unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::Conditional {
                if_branch,
                elif_branches,
                else_branch,
            } => {
                assert_eq!(if_branch.0, "x");
                assert_eq!(if_branch.1.len(), 1);
                assert!(elif_branches.is_empty());
                assert!(else_branch.is_none());
            }
            _ => panic!("Expected conditional"),
        }
    }

    #[test]
    fn test_parse_if_else() {
        let tokens = tokenize("{% if x %}yes{% else %}no{% endif %}").unwrap();
        let ast = parse(&tokens).unwrap();
        match &ast[0] {
            TemplateNode::Conditional { else_branch, .. } => {
                assert!(else_branch.is_some());
            }
            _ => panic!("Expected conditional"),
        }
    }

    #[test]
    fn test_parse_if_elif_else() {
        let tokens = tokenize("{% if a %}A{% elif b %}B{% else %}C{% endif %}").unwrap();
        let ast = parse(&tokens).unwrap();
        match &ast[0] {
            TemplateNode::Conditional {
                elif_branches,
                else_branch,
                ..
            } => {
                assert_eq!(elif_branches.len(), 1);
                assert!(else_branch.is_some());
            }
            _ => panic!("Expected conditional"),
        }
    }

    #[test]
    fn test_eval_equality() {
        let mut ctx = EvalContext::new();
        ctx.set("data.type", "file");
        assert!(evaluate_condition("data.type == \"file\"", &ctx));
        assert!(!evaluate_condition("data.type == \"code\"", &ctx));
    }

    #[test]
    fn test_eval_numeric_comparison() {
        let mut ctx = EvalContext::new();
        ctx.set("data.count", "15");
        assert!(evaluate_condition("data.count > 10", &ctx));
        assert!(!evaluate_condition("data.count > 20", &ctx));
    }

    #[test]
    fn test_eval_and_or() {
        let mut ctx = EvalContext::new();
        ctx.set("a", "true");
        ctx.set("b", "true");
        assert!(evaluate_condition("a and b", &ctx));

        // a and b or c — with only a set, c missing
        let mut ctx2 = EvalContext::new();
        ctx2.set("a", "true");
        // b is missing (falsy), c is missing (falsy)
        assert!(!evaluate_condition("a and b or c", &ctx2));

        // a or b — a is set
        assert!(evaluate_condition("a or b", &ctx2));
    }

    #[test]
    fn test_eval_not() {
        let mut ctx = EvalContext::new();
        ctx.set("data.skip", "true");
        assert!(!evaluate_condition("not data.skip", &ctx));

        let ctx2 = EvalContext::new();
        assert!(evaluate_condition("not data.skip", &ctx2));
    }

    #[test]
    fn test_eval_exists_truthy() {
        let mut ctx = EvalContext::new();
        ctx.set("data.show", "true");
        assert!(evaluate_condition("data.show", &ctx));

        let ctx2 = EvalContext::new();
        assert!(!evaluate_condition("data.missing", &ctx2));
    }

    #[test]
    fn test_eval_parentheses() {
        let mut ctx = EvalContext::new();
        ctx.set("a", "true");
        ctx.set("c", "true");
        // a and (b or c) — b missing but c is true
        assert!(evaluate_condition("a and (b or c)", &ctx));
    }

    // ===== Integration Tests =====

    #[test]
    fn test_process_simple_if_true() {
        let mut ctx = EvalContext::new();
        ctx.set("show", "true");

        let result = process_conditionals("{% if show %}Hello{% endif %}", &ctx).unwrap();
        assert_eq!(result.trim(), "Hello");
    }

    #[test]
    fn test_process_simple_if_false() {
        let ctx = EvalContext::new();

        let result = process_conditionals("{% if show %}Hello{% endif %}", &ctx).unwrap();
        assert_eq!(result.trim(), "");
    }

    #[test]
    fn test_process_if_else() {
        let ctx = EvalContext::new();

        let result = process_conditionals("{% if show %}Yes{% else %}No{% endif %}", &ctx).unwrap();
        assert_eq!(result.trim(), "No");
    }

    #[test]
    fn test_process_preserves_variables() {
        let mut ctx = EvalContext::new();
        ctx.set("show", "true");

        let result = process_conditionals("{% if show %}Hello {{name}}{% endif %}", &ctx).unwrap();
        assert!(result.contains("{{name}}"));
    }

    #[test]
    fn test_process_equality_comparison() {
        let mut ctx = EvalContext::new();
        ctx.set("data.type", "file");

        let result = process_conditionals(
            "{% if data.type == \"file\" %}File review{% else %}Code review{% endif %}",
            &ctx,
        )
        .unwrap();
        assert_eq!(result.trim(), "File review");
    }

    #[test]
    fn test_process_nested_if() {
        let mut ctx = EvalContext::new();
        ctx.set("a", "true");
        ctx.set("b", "true");

        let template = r#"{% if a %}
outer
{% if b %}
inner
{% endif %}
{% endif %}"#;

        let result = process_conditionals(template, &ctx).unwrap();
        assert!(result.contains("outer"));
        assert!(result.contains("inner"));
    }

    #[test]
    fn test_multiline_template() {
        let mut ctx = EvalContext::new();
        ctx.set("data.target_type", "file");

        let template = r#"# Review: {{data.target_name}}

{% if data.target_type == "file" %}
Read the file at `{{data.path}}` to understand:
- What is this document about?
{% else %}
Examine the code changes:
1. View the diff
{% endif %}

## Quality check"#;

        let result = process_conditionals(template, &ctx).unwrap();
        assert!(result.contains("Read the file"));
        assert!(!result.contains("Examine the code"));
        assert!(result.contains("Quality check"));
        assert!(result.contains("{{data.target_name}}")); // Variables preserved
        assert!(result.contains("{{data.path}}")); // Variables preserved
    }

    // ===== ## Heading Preservation Tests =====
    // These tests verify that ## headings in various contexts are preserved as text

    #[test]
    fn test_hash_headings_in_block_quotes_preserved() {
        let ctx = EvalContext::new();

        let template = r#"> This is a block quote
> ## This is a heading inside the quote
> More quoted text"#;

        let result = process_conditionals(template, &ctx).unwrap();
        // ## inside block quote should be preserved as-is
        assert!(result.contains("## This is a heading inside the quote"));
    }

    #[test]
    fn test_hash_headings_in_indented_lists_preserved() {
        let ctx = EvalContext::new();

        let template = r#"- List item 1
  ## Indented heading (not a real heading)
- List item 2"#;

        let result = process_conditionals(template, &ctx).unwrap();
        // ## in indented context should be preserved
        assert!(result.contains("## Indented heading"));
    }

    #[test]
    fn test_hash_headings_in_fenced_code_preserved() {
        let ctx = EvalContext::new();

        let template = r#"```markdown
## This is inside a code block
Some code here
```"#;

        let result = process_conditionals(template, &ctx).unwrap();
        // ## inside fenced code block should be preserved
        assert!(result.contains("## This is inside a code block"));
    }

    #[test]
    fn test_hash_headings_with_conditionals() {
        let mut ctx = EvalContext::new();
        ctx.set("show", "true");

        let template = r#"{% if show %}
## Conditional heading

Content under heading
{% endif %}"#;

        let result = process_conditionals(template, &ctx).unwrap();
        assert!(result.contains("## Conditional heading"));
        assert!(result.contains("Content under heading"));
    }

    // ===== Error Line Number Tests =====

    #[test]
    fn test_error_line_number_unclosed_variable() {
        let template = "Line 1\nLine 2\n{{unclosed";
        let result = tokenize(template);
        assert!(result.is_err());
        match result.unwrap_err() {
            ConditionalError::MismatchedDelimiters { line, .. } => {
                assert_eq!(line, 3, "Error should report line 3");
            }
            other => panic!("Expected MismatchedDelimiters, got {:?}", other),
        }
    }

    #[test]
    fn test_error_line_number_unclosed_control_block() {
        let template = "Line 1\nLine 2\nLine 3\n{% if x";
        let result = tokenize(template);
        assert!(result.is_err());
        match result.unwrap_err() {
            ConditionalError::MismatchedDelimiters { line, .. } => {
                assert_eq!(line, 4, "Error should report line 4");
            }
            other => panic!("Expected MismatchedDelimiters, got {:?}", other),
        }
    }

    #[test]
    fn test_error_line_number_single_brace_syntax() {
        let template = "Line 1\n{old_syntax}\nLine 3";
        let result = tokenize(template);
        assert!(result.is_err());
        match result.unwrap_err() {
            ConditionalError::SingleBraceSyntax { line, variable } => {
                assert_eq!(line, 2, "Error should report line 2");
                assert_eq!(variable, "old_syntax");
            }
            other => panic!("Expected SingleBraceSyntax, got {:?}", other),
        }
    }

    #[test]
    fn test_error_line_number_unclosed_if() {
        let tokens = tokenize("Line 1\n{% if x %}\nContent\n").unwrap();
        let result = parse(&tokens);
        assert!(result.is_err());
        match result.unwrap_err() {
            ConditionalError::UnclosedBlock { line } => {
                // The error reports where the unclosed block started
                assert_eq!(line, 2, "Error should report line 2 where if block started");
            }
            other => panic!("Expected UnclosedBlock, got {:?}", other),
        }
    }

    #[test]
    fn test_error_line_number_unexpected_else() {
        let tokens = tokenize("{% else %}content{% endif %}").unwrap();
        let result = parse(&tokens);
        assert!(result.is_err());
        match result.unwrap_err() {
            ConditionalError::UnexpectedToken { token, .. } => {
                assert_eq!(token, "else");
            }
            other => panic!("Expected UnexpectedToken, got {:?}", other),
        }
    }

    #[test]
    fn test_complex_boolean_expression_line_tracking() {
        // For complex expressions like "{% if a and b or c %}", warnings about
        // undefined variables should reference the control block start line
        let template = "Line 1\nLine 2\n{% if undefined_a and undefined_b %}\nContent\n{% endif %}";

        // This should parse and render successfully (undefined vars are falsy, not errors)
        let ctx = EvalContext::new();
        let result = process_conditionals(template, &ctx);
        assert!(result.is_ok());
        // The conditional should evaluate to false (undefined vars are falsy)
        assert!(!result.unwrap().contains("Content"));
    }

    // ===== Loop Parser Tests =====

    #[test]
    fn test_tokenize_for_loop() {
        let tokens =
            tokenize("{% for item in source.comments %}{{item.text}}{% endfor %}").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::ControlBlock("for item in source.comments".to_string()),
                Token::Variable("item.text".to_string()),
                Token::ControlBlock("endfor".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_simple_for_loop() {
        let tokens = tokenize("{% for item in source.comments %}text{% endfor %}").unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::Loop {
                variable,
                collection,
                body,
                else_body,
            } => {
                assert_eq!(variable, "item");
                assert_eq!(collection, "source.comments");
                assert_eq!(body.len(), 1);
                assert!(else_body.is_none());
            }
            _ => panic!("Expected Loop"),
        }
    }

    #[test]
    fn test_parse_for_loop_with_else() {
        let tokens =
            tokenize("{% for item in source.comments %}content{% else %}empty{% endfor %}")
                .unwrap();
        let ast = parse(&tokens).unwrap();
        match &ast[0] {
            TemplateNode::Loop {
                variable,
                collection,
                body,
                else_body,
            } => {
                assert_eq!(variable, "item");
                assert_eq!(collection, "source.comments");
                assert!(!body.is_empty());
                assert!(else_body.is_some());
                let else_nodes = else_body.as_ref().unwrap();
                assert!(matches!(&else_nodes[0], TemplateNode::Text(t) if t == "empty"));
            }
            _ => panic!("Expected Loop"),
        }
    }

    #[test]
    fn test_parse_nested_for_loops() {
        let tokens =
            tokenize("{% for a in x %}{% for b in y %}inner{% endfor %}{% endfor %}").unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::Loop { variable, body, .. } => {
                assert_eq!(variable, "a");
                // Inner loop should be in body
                assert!(body
                    .iter()
                    .any(|n| matches!(n, TemplateNode::Loop { variable, .. } if variable == "b")));
            }
            _ => panic!("Expected Loop"),
        }
    }

    #[test]
    fn test_parse_for_with_if_inside() {
        let tokens =
            tokenize("{% for item in list %}{% if item.show %}yes{% endif %}{% endfor %}").unwrap();
        let ast = parse(&tokens).unwrap();
        match &ast[0] {
            TemplateNode::Loop { body, .. } => {
                assert!(body
                    .iter()
                    .any(|n| matches!(n, TemplateNode::Conditional { .. })));
            }
            _ => panic!("Expected Loop"),
        }
    }

    #[test]
    fn test_parse_for_header_valid() {
        let (var, coll) = parse_for_header("item in source.comments", 1).unwrap();
        assert_eq!(var, "item");
        assert_eq!(coll, "source.comments");

        let (var, coll) = parse_for_header("c in data.items", 1).unwrap();
        assert_eq!(var, "c");
        assert_eq!(coll, "data.items");

        let (var, coll) = parse_for_header("my_var in things", 1).unwrap();
        assert_eq!(var, "my_var");
        assert_eq!(coll, "things");
    }

    #[test]
    fn test_parse_for_header_invalid_no_in() {
        let result = parse_for_header("item source.comments", 1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ConditionalError::InvalidLoopSyntax { .. }));
    }

    #[test]
    fn test_parse_for_header_invalid_variable() {
        // Uppercase not allowed
        let result = parse_for_header("Item in source.comments", 1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ConditionalError::InvalidLoopVariable { .. }));

        // Starts with digit
        let result = parse_for_header("1item in source.comments", 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_for_header_empty_collection() {
        let result = parse_for_header("item in ", 1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ConditionalError::InvalidLoopSyntax { .. }));
    }

    #[test]
    fn test_unclosed_for_loop() {
        let tokens = tokenize("{% for item in list %}content").unwrap();
        let result = parse(&tokens);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ConditionalError::UnclosedLoop { .. }));
    }

    #[test]
    fn test_unexpected_endfor() {
        let tokens = tokenize("{% endfor %}").unwrap();
        let result = parse(&tokens);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ConditionalError::UnexpectedToken { token, .. } if token == "endfor")
        );
    }

    #[test]
    fn test_process_for_loop_output() {
        let template = "{% for item in source.comments %}## {{item.text}}{% endfor %}";
        let ctx = EvalContext::new();
        let result = process_conditionals(template, &ctx).unwrap();

        // Should contain loop markers for resolver processing
        assert!(result.contains("AIKI_LOOP:item:source.comments"));
        assert!(result.contains("AIKI_ENDLOOP"));
        assert!(result.contains("{{item.text}}")); // Variable preserved
    }

    #[test]
    fn test_process_for_loop_with_else_output() {
        let template = "{% for item in list %}content{% else %}no items{% endfor %}";
        let ctx = EvalContext::new();
        let result = process_conditionals(template, &ctx).unwrap();

        assert!(result.contains("AIKI_LOOP"));
        assert!(result.contains("AIKI_LOOPELSE"));
        assert!(result.contains("no items"));
        assert!(result.contains("AIKI_ENDLOOPELSE"));
    }

    #[test]
    fn test_multiline_for_loop() {
        let template = r#"# Subtasks

{% for item in source.comments %}
## Fix: {{item.file}}

{{item.text}}
{% endfor %}"#;

        let ctx = EvalContext::new();
        let result = process_conditionals(template, &ctx).unwrap();

        assert!(result.contains("# Subtasks"));
        assert!(result.contains("AIKI_LOOP:item:source.comments"));
        assert!(result.contains("## Fix: {{item.file}}"));
        assert!(result.contains("{{item.text}}"));
        assert!(result.contains("AIKI_ENDLOOP"));
    }

    #[test]
    fn test_conditionals_inside_loops_are_preserved() {
        // Tests that conditionals inside loop bodies are serialized back to template
        // syntax (preserved for later evaluation during loop expansion), NOT evaluated
        // immediately with an empty loop context.
        //
        // This is the key fix for: "conditionals inside loop bodies evaluated before
        // loop expansion"
        let template = r#"{% for item in source.comments %}
## {{item.name}}
{% if item.priority == "high" %}**HIGH PRIORITY**{% endif %}
{% endfor %}"#;

        let ctx = EvalContext::new();
        let result = process_conditionals(template, &ctx).unwrap();

        // Should have AIKI_LOOP markers
        assert!(
            result.contains("AIKI_LOOP:item:source.comments"),
            "Missing loop markers"
        );
        assert!(result.contains("AIKI_ENDLOOP"), "Missing endloop marker");

        // CRITICAL: Conditional should be PRESERVED as {% if %} syntax, NOT evaluated
        // If evaluated with empty context, it would be gone (falsey)
        assert!(
            result.contains("{% if item.priority == \"high\" %}"),
            "Conditional was evaluated instead of preserved! Got:\n{}",
            result
        );
        assert!(
            result.contains("**HIGH PRIORITY**"),
            "Conditional body missing"
        );
        assert!(result.contains("{% endif %}"), "Conditional endif missing");
    }

    // ===== SubtaskRef Tests =====

    #[test]
    fn test_tokenize_subtask_ref() {
        let tokens = tokenize("{% subtask decompose %}").unwrap();
        assert_eq!(
            tokens,
            vec![Token::ControlBlock("subtask decompose".to_string())]
        );
    }

    #[test]
    fn test_parse_subtask_ref_simple() {
        let tokens = tokenize("{% subtask decompose %}").unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::SubtaskRef {
                template_name,
                attributes,
                condition,
                line,
            } => {
                assert_eq!(template_name, "decompose");
                assert!(attributes.is_empty());
                assert!(condition.is_none());
                assert_eq!(*line, 1);
            }
            _ => panic!("Expected SubtaskRef"),
        }
    }

    #[test]
    fn test_parse_subtask_ref_with_condition() {
        let tokens = tokenize("{% subtask review/plan if data.file_type == \"plan\" %}").unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::SubtaskRef {
                template_name,
                condition,
                ..
            } => {
                assert_eq!(template_name, "review/plan");
                assert_eq!(condition.as_deref(), Some("data.file_type == \"plan\""));
            }
            _ => panic!("Expected SubtaskRef"),
        }
    }

    #[test]
    fn test_parse_subtask_ref_inside_conditional() {
        let tokens =
            tokenize("{% if data.needs_plan %}{% subtask decompose %}{% endif %}").unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::Conditional { if_branch, .. } => {
                assert!(if_branch.1.iter().any(|n| matches!(n, TemplateNode::SubtaskRef { template_name, .. } if template_name == "decompose")));
            }
            _ => panic!("Expected Conditional"),
        }
    }

    #[test]
    fn test_parse_subtask_ref_inside_loop() {
        let tokens = tokenize("{% for item in list %}{% subtask decompose %}{% endfor %}").unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::Loop { body, .. } => {
                assert!(body.iter().any(|n| matches!(n, TemplateNode::SubtaskRef { template_name, .. } if template_name == "decompose")));
            }
            _ => panic!("Expected Loop"),
        }
    }

    #[test]
    fn test_subtask_ref_invalid_empty_name() {
        let tokens = tokenize("{% subtask %}").unwrap();
        let result = parse(&tokens);
        // "subtask " won't match because there's only "subtask" without trailing space
        // The block content is "subtask" which doesn't start with "subtask " (no space after)
        // So it's treated as unknown control block
        assert!(result.is_ok()); // Unknown blocks are ignored
    }

    #[test]
    fn test_subtask_ref_template_name_validation() {
        // Valid names
        let result = parse_subtask_ref("decompose", 1);
        assert!(result.is_ok());

        let result = parse_subtask_ref("review/plan", 1);
        assert!(result.is_ok());

        let result = parse_subtask_ref("myorg/v2.0", 1);
        assert!(result.is_ok());

        let result = parse_subtask_ref("review-code", 1);
        assert!(result.is_ok());

        let result = parse_subtask_ref("aiki/my_template", 1);
        assert!(result.is_ok());

        // Invalid: uppercase
        let result = parse_subtask_ref("Aiki/Plan", 1);
        assert!(result.is_err());

        // Valid: interpolated template name
        let result = parse_subtask_ref("review/{{data.scope.kind}}", 1);
        assert!(result.is_ok());
        match result.unwrap() {
            TemplateNode::SubtaskRef { template_name, .. } => {
                assert_eq!(template_name, "review/{{data.scope.kind}}");
            }
            _ => panic!("Expected SubtaskRef"),
        }
    }

    #[test]
    fn test_process_subtask_ref_interpolated() {
        let mut ctx = EvalContext::new();
        ctx.set("data.scope.kind", "task");
        let result =
            process_conditionals("{% subtask review/{{data.scope.kind}} %}", &ctx).unwrap();
        assert!(
            result.contains("AIKI_SUBTASK_REF:review/task:"),
            "Expected interpolated template name, got: {}",
            result
        );
    }

    #[test]
    fn test_process_subtask_ref_interpolated_session() {
        let mut ctx = EvalContext::new();
        ctx.set("data.scope.kind", "session");
        let result =
            process_conditionals("{% subtask review/{{data.scope.kind}} %}", &ctx).unwrap();
        assert!(
            result.contains("AIKI_SUBTASK_REF:review/session:"),
            "Expected interpolated template name, got: {}",
            result
        );
    }

    #[test]
    fn test_subtask_ref_roundtrip() {
        // SubtaskRef should serialize back to template syntax correctly
        let node = TemplateNode::SubtaskRef {
            template_name: "decompose".to_string(),
            attributes: HashMap::new(),
            condition: None,
            line: 1,
        };
        assert_eq!(node_to_template(&node), "{% subtask decompose %}");

        let node_with_cond = TemplateNode::SubtaskRef {
            template_name: "review/plan".to_string(),
            attributes: HashMap::new(),
            condition: Some("data.type == \"plan\"".to_string()),
            line: 1,
        };
        assert_eq!(
            node_to_template(&node_with_cond),
            "{% subtask review/plan if data.type == \"plan\" %}"
        );
    }

    #[test]
    fn test_process_subtask_ref_unconditional() {
        let ctx = EvalContext::new();
        let result = process_conditionals("{% subtask decompose %}", &ctx).unwrap();
        assert!(result.contains("AIKI_SUBTASK_REF:decompose:"));
    }

    #[test]
    fn test_process_subtask_ref_conditional_true() {
        let mut ctx = EvalContext::new();
        ctx.set("data.file_type", "plan");
        let result = process_conditionals(
            "{% subtask review/plan if data.file_type == \"plan\" %}",
            &ctx,
        )
        .unwrap();
        assert!(result.contains("AIKI_SUBTASK_REF:review/plan:"));
    }

    #[test]
    fn test_process_subtask_ref_conditional_false() {
        let mut ctx = EvalContext::new();
        ctx.set("data.file_type", "code");
        let result = process_conditionals(
            "{% subtask review/plan if data.file_type == \"plan\" %}",
            &ctx,
        )
        .unwrap();
        assert!(!result.contains("AIKI_SUBTASK_REF"));
    }

    #[test]
    fn test_process_subtask_ref_inside_if_block() {
        let mut ctx = EvalContext::new();
        ctx.set("data.needs_plan", "true");
        let result = process_conditionals(
            "{% if data.needs_plan %}{% subtask decompose %}{% endif %}",
            &ctx,
        )
        .unwrap();
        assert!(result.contains("AIKI_SUBTASK_REF:decompose:"));

        // False branch
        let ctx2 = EvalContext::new();
        let result2 = process_conditionals(
            "{% if data.needs_plan %}{% subtask decompose %}{% endif %}",
            &ctx2,
        )
        .unwrap();
        assert!(!result2.contains("AIKI_SUBTASK_REF"));
    }

    #[test]
    fn test_parse_subtask_ref_with_attributes() {
        let tokens = tokenize("{% subtask review needs-context:subtasks.explore %}").unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::SubtaskRef {
                template_name,
                attributes,
                condition,
                ..
            } => {
                assert_eq!(template_name, "review");
                assert_eq!(
                    attributes.get("needs-context").map(|s| s.as_str()),
                    Some("subtasks.explore")
                );
                assert!(condition.is_none());
            }
            _ => panic!("Expected SubtaskRef"),
        }
    }

    #[test]
    fn test_parse_subtask_ref_with_quoted_attribute_and_condition() {
        let tokens = tokenize("{% subtask review key:\"quoted value\" if data.ready %}").unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::SubtaskRef {
                template_name,
                attributes,
                condition,
                ..
            } => {
                assert_eq!(template_name, "review");
                assert_eq!(
                    attributes.get("key").map(|s| s.as_str()),
                    Some("quoted value")
                );
                assert_eq!(condition.as_deref(), Some("data.ready"));
            }
            _ => panic!("Expected SubtaskRef"),
        }
    }

    #[test]
    fn test_parse_subtask_ref_with_multiple_attributes() {
        let tokens = tokenize(
            "{% subtask review needs-context:subtasks.explore priority:p0 if data.ready %}",
        )
        .unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::SubtaskRef {
                template_name,
                attributes,
                condition,
                ..
            } => {
                assert_eq!(template_name, "review");
                assert_eq!(attributes.len(), 2);
                assert_eq!(
                    attributes.get("needs-context").map(|s| s.as_str()),
                    Some("subtasks.explore")
                );
                assert_eq!(attributes.get("priority").map(|s| s.as_str()), Some("p0"));
                assert_eq!(condition.as_deref(), Some("data.ready"));
            }
            _ => panic!("Expected SubtaskRef"),
        }
    }

    #[test]
    fn test_subtask_ref_roundtrip_with_attributes() {
        let mut attrs = HashMap::new();
        attrs.insert("needs-context".to_string(), "subtasks.explore".to_string());
        let node = TemplateNode::SubtaskRef {
            template_name: "review".to_string(),
            attributes: attrs,
            condition: None,
            line: 1,
        };
        assert_eq!(
            node_to_template(&node),
            "{% subtask review needs-context:subtasks.explore %}"
        );
    }

    #[test]
    fn test_subtask_ref_roundtrip_with_quoted_attribute() {
        let mut attrs = HashMap::new();
        attrs.insert("key".to_string(), "value with spaces".to_string());
        let node = TemplateNode::SubtaskRef {
            template_name: "review".to_string(),
            attributes: attrs,
            condition: Some("data.ready".to_string()),
            line: 1,
        };
        assert_eq!(
            node_to_template(&node),
            "{% subtask review key:\"value with spaces\" if data.ready %}"
        );
    }

    #[test]
    fn test_process_subtask_ref_with_attributes_marker() {
        let ctx = EvalContext::new();
        let result =
            process_conditionals("{% subtask review needs-context:subtasks.explore %}", &ctx)
                .unwrap();
        assert!(
            result.contains("AIKI_SUBTASK_REF:review:"),
            "Expected subtask ref marker, got: {}",
            result
        );
        assert!(
            result.contains("needs-context:subtasks.explore"),
            "Expected attributes in marker, got: {}",
            result
        );
    }

    #[test]
    fn test_process_subtask_ref_quoted_value_percent_encoded() {
        let ctx = EvalContext::new();
        let result =
            process_conditionals("{% subtask review title:\"value with spaces\" %}", &ctx).unwrap();
        assert!(
            result.contains("title:value%20with%20spaces"),
            "Expected percent-encoded spaces in marker, got: {}",
            result
        );
    }

    #[test]
    fn test_subtask_ref_empty_if_condition_error() {
        // "{% subtask review if %}" with no condition expression should error
        let result = parse_subtask_ref("review if", 1);
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            ConditionalError::InvalidCondition { condition, .. } => {
                assert!(
                    condition.contains("without a condition expression"),
                    "Expected error about missing condition, got: {}",
                    condition
                );
            }
            _ => panic!("Expected InvalidCondition error, got: {:?}", err),
        }
    }

    #[test]
    fn test_subtask_ref_unclosed_quote_error() {
        let result = parse_subtask_ref("review key:\"unclosed value", 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_subtask_ref_unknown_token_error() {
        let result = parse_subtask_ref("review badtoken", 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_process_subtask_ref_mixed_with_static() {
        let ctx = EvalContext::new();
        let template = r#"## Setup environment
Install dependencies.

{% subtask decompose %}

## Execute plan
Run each plan subtask."#;

        let result = process_conditionals(template, &ctx).unwrap();
        assert!(result.contains("## Setup environment"));
        assert!(result.contains("AIKI_SUBTASK_REF:decompose:"));
        assert!(result.contains("## Execute plan"));
    }

    #[test]
    fn test_parse_subtask_ref_three_attributes_no_condition() {
        let tokens = tokenize("{% subtask review a:1 b:2 c:3 %}").unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::SubtaskRef {
                template_name,
                attributes,
                condition,
                ..
            } => {
                assert_eq!(template_name, "review");
                assert_eq!(attributes.len(), 3);
                assert_eq!(attributes.get("a").map(|s| s.as_str()), Some("1"));
                assert_eq!(attributes.get("b").map(|s| s.as_str()), Some("2"));
                assert_eq!(attributes.get("c").map(|s| s.as_str()), Some("3"));
                assert!(condition.is_none());
            }
            _ => panic!("Expected SubtaskRef"),
        }
    }

    #[test]
    fn test_parse_subtask_ref_quoted_value_strips_quotes() {
        let tokens = tokenize("{% subtask review key:\"value with spaces\" %}").unwrap();
        let ast = parse(&tokens).unwrap();
        assert_eq!(ast.len(), 1);
        match &ast[0] {
            TemplateNode::SubtaskRef {
                attributes,
                condition,
                ..
            } => {
                assert_eq!(
                    attributes.get("key").map(|s| s.as_str()),
                    Some("value with spaces")
                );
                assert!(condition.is_none());
            }
            _ => panic!("Expected SubtaskRef"),
        }
    }

    #[test]
    fn test_subtask_ref_roundtrip_three_attributes() {
        let mut attrs = HashMap::new();
        attrs.insert("a".to_string(), "1".to_string());
        attrs.insert("b".to_string(), "2".to_string());
        attrs.insert("c".to_string(), "3".to_string());
        let node = TemplateNode::SubtaskRef {
            template_name: "review".to_string(),
            attributes: attrs,
            condition: None,
            line: 1,
        };
        // Keys are sorted alphabetically in serialization
        assert_eq!(node_to_template(&node), "{% subtask review a:1 b:2 c:3 %}");
    }
}
