use ra_ap_hir::{Adt, ModuleDef, Semantics};
use ra_ap_syntax::ast;

use crate::signature::format_function_signature;
use crate::visibility::is_pub;

/// Resolves the name of the struct type that an `impl` block targets,
/// but only if the type is defined in the same module.
///
/// # Returns
///
/// The struct's name as a `String`, or `None` if `self_ty` is not a path
/// type pointing to a struct in the current module.
pub fn resolve_impl_self_type_name(
    sema: &Semantics<ra_ap_ide::RootDatabase>,
    current_module: ra_ap_hir::Module,
    self_ty: &ast::Type,
) -> Option<String> {
    let ast::Type::PathType(path_ty) = self_ty else {
        return None;
    };

    let path = path_ty.path()?;
    let ra_ap_hir::PathResolution::Def(ModuleDef::Adt(Adt::Struct(struct_def))) =
        sema.resolve_path(&path)?
    else {
        return None;
    };

    (struct_def.module(sema.db) == current_module)
        .then(|| struct_def.name(sema.db).as_str().to_string())
}

/// Collects the formatted signatures of all public methods in an `impl` block.
///
/// # Parameters
///
/// * `i` — The `ast::Impl` node to scan.
///
/// # Returns
///
/// A vector of method signature strings.
pub fn collect_public_methods_from_impl(i: &ast::Impl) -> Vec<String> {
    let Some(assoc_list) = i.assoc_item_list() else {
        return Vec::new();
    };

    let mut methods = Vec::new();
    for item in assoc_list.assoc_items() {
        let item = match item {
            ast::AssocItem::Fn(f) => f,
            _ => continue,
        };

        if is_pub(&item) {
            methods.push(format_function_signature(&item));
        }
    }

    methods
}
