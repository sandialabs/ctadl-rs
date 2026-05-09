use std::collections::BTreeMap;
use tree_sitter::Node;

/// Frontend-local constant-string evaluator for resolving dynamic names
/// like $$var or $obj->$prop during lowering.
#[derive(Debug, Default)]
pub struct Evaluator {
    // Maps variable names (e.g., "$var") to their known constant string values.
    constants: BTreeMap<String, String>,
}

impl Evaluator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a variable assignment if the right-hand side is a constant string.
    pub fn assign(&mut self, var_name: String, value: String) {
        self.constants.insert(var_name, value);
    }

    /// Clear the current context (e.g., when entering a new function).
    pub fn clear(&mut self) {
        self.constants.clear();
    }

    /// Attempt to resolve a variable name to its constant string value.
    pub fn resolve(&self, var_name: &str) -> Option<&str> {
        self.constants.get(var_name).map(|s| s.as_str())
    }

    /// Evaluate an AST node to a string if possible.
    pub fn eval_node(&self, node: Node<'_>, source: &str) -> Option<String> {
        match node.kind() {
            "string" => {
                // E.g., "foo" or 'foo'
                // We should strip the quotes.
                let text = &source[node.start_byte()..node.end_byte()];
                if text.len() >= 2 {
                    let unquoted = &text[1..text.len() - 1];
                    Some(unquoted.to_string())
                } else {
                    None
                }
            }
            "variable_name" => {
                let text = &source[node.start_byte()..node.end_byte()];
                self.resolve(text).map(|s| s.to_string())
            }
            "name" => {
                let text = &source[node.start_byte()..node.end_byte()];
                Some(text.to_string())
            }
            _ => None,
        }
    }
}
