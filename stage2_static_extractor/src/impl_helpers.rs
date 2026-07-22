use ra_ap_hir::{Adt, ModuleDef, Semantics};
use ra_ap_syntax::ast;

use crate::signature::format_function_signature;
use crate::types::StructInfo;
use crate::visibility::is_pub;

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

pub fn enrich_struct_with_public_methods(
    _sema: &Semantics<ra_ap_ide::RootDatabase>,
    i: &ast::Impl,
    s: &mut StructInfo,
) {
    s.methods.extend(collect_public_methods_from_impl(i));
}

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

    //Some(format!("pub struct {}{}{{{}}};", name, generics, public_fields.join(", ")))
}
