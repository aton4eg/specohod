use ra_ap_hir::{Adt, ModuleDef, Semantics};

/// Computes the fully-qualified module path (e.g. `crate::foo::bar`) for
/// a given `ra_ap_hir::Module`.
///
/// Walks the module's ancestry from root to the module itself.
///
/// # Parameters
///
/// * `sema` — rust-analyzer `Semantics`.
/// * `module` — The HIR module to resolve.
///
/// # Returns
///
/// The module path as a `String`, joined by `::`.
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
            // Root module: use the crate name, falling back to a placeholder for
            // crates without a display name instead of panicking.
            let crate_name = m
                .krate(db)
                .display_name(db)
                .map(|d| d.crate_name().to_string())
                .unwrap_or_else(|| "unknown_crate".to_string());
            segments.push(crate_name);
        }
    }
    if segments.is_empty() {
        "missing".to_string()
    } else {
        segments.join("::")
    }
}

/// Resolves a `ModuleDef` to its canonical module path string.
///
/// # Returns
///
/// The fully-qualified path for modules, ADTs, traits, and functions;
/// `None` for unsupported definition kinds.
pub fn get_canonical_path(
    sema: &Semantics<ra_ap_ide::RootDatabase>,
    def: ModuleDef,
) -> Option<String> {
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
