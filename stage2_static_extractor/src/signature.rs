use ra_ap_syntax::{AstNode, ast};

/// Formats the signature of a function AST node as a single string, omitting
/// the body and attributes.
///
/// # Parameters
///
/// * `f` — The `ast::Fn` node.
///
/// # Returns
///
/// The function signature (e.g. `pub fn foo(&self, x: i32) -> bool`).
pub fn format_function_signature(f: &ast::Fn) -> String {
    let mut parts = Vec::new();
    for child in f.syntax().children() {
        // Skip attributes, they are not part of signature
        if ast::Attr::can_cast(child.kind()) {
            continue;
        }
        // Stop when we reach to function body
        if ast::BlockExpr::can_cast(child.kind()) {
            break;
        }
        parts.push(child.text().to_string());
    }
    parts.join(" ")
}
