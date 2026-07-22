use ra_ap_syntax::{AstNode, ast};

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
