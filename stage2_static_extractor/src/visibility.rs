use ra_ap_syntax::ast::{HasAttrs, HasName};
use ra_ap_syntax::{AstNode, ast};

/// Checks whether a syntax node is declared with any form of public visibility
/// (`pub`, `pub(crate)`, or `pub(super)`).
///
/// # Parameters
///
/// * `node` — Any AST node that implements `HasVisibility`.
///
/// # Returns
///
/// `true` if the node has an explicit public visibility modifier.
pub fn is_pub<N: ast::HasVisibility>(node: &N) -> bool {
    if let Some(vis) = node.visibility() {
        matches!(
            vis.kind(),
            ast::VisibilityKind::Pub
                | ast::VisibilityKind::PubCrate
                | ast::VisibilityKind::PubSuper
        )
    } else {
        false
    }
}

/// Returns `true` if `node` is itself a test module declaration (`mod test`,
/// `mod tests`, or a `#[cfg(test)]`-gated module).
///
/// This only inspects the node itself. Callers that walk the syntax tree
/// top-down can prune the whole subtree when this returns `true`, which avoids
/// the previous O(nodes × depth) behaviour of re-walking every ancestor chain.
pub fn is_test_module(node: &ra_ap_syntax::SyntaxNode) -> bool {
    let Some(module) = ast::Module::cast(node.clone()) else {
        return false;
    };

    if let Some(name) = module.name()
        && (name.text() == "test" || name.text() == "tests")
    {
        return true;
    }

    for attr in module.attrs() {
        if let Some(path) = attr.path()
            && path.syntax().text() == "cfg"
        {
            let text = attr.syntax().text().to_string();
            if text.contains("test") {
                return true;
            }
        }
    }

    false
}
