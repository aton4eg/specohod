use std::path::Path;
use std::path::PathBuf;

use ra_ap_hir::{ModuleDef, Semantics};
use ra_ap_syntax::{AstNode, ast};
use ra_ap_vfs::{AbsPath, Vfs};

use crate::path_helpers::get_canonical_path;
use crate::types::{Dependency, ModuleDeps};
use crate::visibility::is_test_module;

/// Walks the syntax tree of a module and collects dependencies on external
/// (cross-module) paths and modules within the same workspace.
///
/// Recursively descends into child nodes. Test modules (`#[cfg(test)]`,
/// `mod test`, `mod tests`) are pruned top-down.
///
/// # Parameters
///
/// * `sema` — rust-analyzer `Semantics` for name resolution.
/// * `vfs` — Virtual file system.
/// * `workspace_root` — Root of the analyzed workspace.
/// * `deps` — Mutable `ModuleDeps` accumulator.
/// * `node` — Current syntax node to scan.
/// * `current_module` — Absolute path of the current file.
pub fn collect_external_paths(
    sema: &Semantics<ra_ap_ide::RootDatabase>,
    vfs: &Vfs,
    workspace_root: &Path,
    deps: &mut ModuleDeps,
    node: &ra_ap_syntax::SyntaxNode,
    current_module: &AbsPath,
) {
    let current_path: PathBuf = current_module.into();
    // Prune test modules top-down: if this node is a test module, its whole
    // subtree is skipped because we return before recursing into children.
    if is_test_module(node) {
        return; // completely ignore this AST branch
    }
    if let Some(path) = ast::Path::cast(node.clone())
        && let Some(ra_ap_hir::PathResolution::Def(def)) = sema.resolve_path(&path)
    {
        // Get the definition file path
        if let Some(file_path) = get_file_path_for_def(sema, vfs, def) {
            // Check if file is inside workspace
            if file_path.starts_with(workspace_root)
                && current_path != file_path
                && let Some(mod_path) = get_canonical_path(sema, def)
            {
                let mut spec_path = file_path.clone();
                spec_path.add_extension("spec.md");
                deps.dependency.insert(Dependency {
                    module_path: mod_path,
                    file_path,
                    spec_path,
                });
            }
        }
    }
    if let Some(module) = ast::Module::cast(node.clone())
        && let Some(hir_module) = sema.to_module_def(&module)
        && let Some(file_path) = get_file_path_for_module(sema, vfs, hir_module)
        && file_path.starts_with(workspace_root)
        && current_path != file_path
        && let Some(mod_path) = get_canonical_path(sema, ModuleDef::Module(hir_module))
    {
        let mut spec_path = file_path.clone();
        spec_path.add_extension("spec.md");
        deps.dependency.insert(Dependency {
            module_path: mod_path,
            file_path,
            spec_path,
        });
    }
    for child in node.children() {
        collect_external_paths(sema, vfs, workspace_root, deps, &child, current_module);
    }
}

/// Resolves a `ra_ap_hir::Module` to its file path via the VFS.
///
/// # Returns
///
/// The on-disk `PathBuf` of the module's source file, or `None` if it could
/// not be resolved.
pub fn get_file_path_for_module(
    sema: &Semantics<ra_ap_ide::RootDatabase>,
    vfs: &Vfs,
    module: ra_ap_hir::Module,
) -> Option<PathBuf> {
    let db = sema.db;
    if let Some(file_id) = module.as_source_file_id(db) {
        let vfs_path = vfs.file_path(file_id.file_id(db));
        vfs_path.as_path().map(|p| p.into())
    } else {
        None
    }
}

/// Resolves a `ModuleDef` (struct, trait, function, etc.) to its source file
/// path via the VFS.
///
/// # Returns
///
/// The `PathBuf` of the file containing the definition, or `None` if it could
/// not be resolved.
pub fn get_file_path_for_def(
    sema: &Semantics<ra_ap_ide::RootDatabase>,
    vfs: &Vfs,
    def: ModuleDef,
) -> Option<PathBuf> {
    let module = match def {
        ModuleDef::Module(m) => m,
        ModuleDef::Adt(adt) => adt.module(sema.db),
        ModuleDef::Trait(t) => t.module(sema.db),
        ModuleDef::Function(f) => f.module(sema.db),
        ModuleDef::Const(c) => c.module(sema.db),
        ModuleDef::Static(s) => s.module(sema.db),
        ModuleDef::TypeAlias(ta) => ta.module(sema.db),
        _ => return None,
    };
    get_file_path_for_module(sema, vfs, module)
}
