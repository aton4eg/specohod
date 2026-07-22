use ra_ap_hir::{Adt, ModuleDef, Semantics};

pub fn get_absolute_module_path(
    sema: &Semantics<ra_ap_ide::RootDatabase>,
    module: ra_ap_hir::Module,
) -> String {
    let db = sema.db;
    let mut segments = Vec::new();
    for m in module.path_to_root(db).into_iter().rev() {
        if let Some(name) = m.name(db) {
            segments.push(name.as_str().to_string());
        } else {
            segments.push(
                m.krate(db)
                    .display_name(db)
                    .unwrap()
                    .crate_name()
                    .to_string(),
            );
        }
    }
    if segments.is_empty() {
        "missing".to_string()
    } else {
        segments.join("::")
    }
}

pub fn get_canonical_path(sema: &Semantics<ra_ap_ide::RootDatabase>, def: ModuleDef) -> Option<String> {
    let db = sema.db;
    match def {
        ModuleDef::Module(m) => Some(get_absolute_module_path(sema, m)),
        ModuleDef::Adt(adt) => match adt {
            Adt::Struct(s) => Some(get_absolute_module_path(sema, s.module(db))),
            Adt::Enum(e) => Some(get_absolute_module_path(sema, e.module(db))),
            Adt::Union(u) => Some(get_absolute_module_path(sema, u.module(db))),
        },
        ModuleDef::Trait(t) => Some(get_absolute_module_path(sema, t.module(db))),
        ModuleDef::Function(f) => Some(get_absolute_module_path(sema, f.module(db))),
        _ => None,
    }
}
