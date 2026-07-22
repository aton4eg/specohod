use ra_ap_syntax::ast::{HasAttrs, HasName};
use ra_ap_syntax::{AstNode, ast};

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

pub fn is_inside_test_module(node: &ra_ap_syntax::SyntaxNode) -> bool {
    let mut current = node.clone();
    while let Some(parent) = current.parent() {
        if let Some(module) = ast::Module::cast(parent.clone()) {
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
                    if text.contains("test") || text.contains("tests") {
                        return true;
                    }
                }
            }
        }
        current = parent; // now parent is not moved above
    }
    false
}
