mod args;
mod collect;
mod database;
mod impl_helpers;
mod output;
mod path_helpers;
mod signature;
mod struct_parser;
mod types;
mod visibility;

use std::collections::{HashMap, HashSet};

use clap::Parser;
use ra_ap_base_db::FileId;
use ra_ap_hir::{EditionedFileId, Semantics};
use ra_ap_ide::RootDatabase;
use ra_ap_syntax::ast::HasModuleItem;
use ra_ap_syntax::{AstNode, ast};
use ra_ap_vfs::{AbsPath, Vfs};

use args::Args;
use collect::collect_external_paths;
use database::load_db;
use impl_helpers::{collect_public_methods_from_impl, resolve_impl_self_type_name};
use output::{write_deps, write_facts};
use path_helpers::get_absolute_module_path;
use signature::format_function_signature;
use struct_parser::extract_struct_definition_with_public_fields;
use types::{ModuleDeps, ModuleFacts, StructInfo};
use visibility::is_pub;

fn process_file(
    sema: &Semantics<RootDatabase>,
    db: &RootDatabase,
    vfs: &Vfs,
    workspace_root: &std::path::Path,
    file_id: FileId,
) {
    let file_path = match vfs.file_path(file_id).as_path() {
        Some(p) => p,
        None => return,
    };

    // `vfs.iter()` yields every file loaded by rust-analyzer, including all
    // dependency crates from the cargo registry and the sysroot. Restrict work
    // (and, crucially, the `.facts.json`/`.deps.json` writes below) to files
    // that actually live inside the analyzed workspace.
    let file_pathbuf: std::path::PathBuf = file_path.into();
    if !file_pathbuf.starts_with(workspace_root) {
        return;
    }

    if file_path.components().any(|c| c.as_os_str() == "tests") {
        return;
    }

    let tree = sema.parse(EditionedFileId::current_edition(db, file_id));
    let module = match sema.file_to_module_def(file_id) {
        Some(m) => m,
        _ => return,
    };

    let module_path = get_absolute_module_path(sema, module);

    let mut facts = ModuleFacts {
        module_path: module_path.clone(),
        file_path: file_path.into(),
        public_structs: Vec::new(),
        public_enums: Vec::new(),
        public_traits: Vec::new(),
        public_functions: Vec::new(),
    };

    let mut list_of_structures: HashMap<String, StructInfo> = HashMap::new();
    let mut pending_methods: HashMap<String, Vec<String>> = HashMap::new();

    let mut deps = ModuleDeps {
        module_path,
        file_path: file_path.into(),
        dependency: HashSet::new(),
    };

    let utf8_path = match ra_ap_paths::Utf8PathBuf::from_path_buf(file_pathbuf) {
        Ok(p) => p,
        Err(non_utf8) => {
            eprintln!("Skipping non-UTF-8 path: {}", non_utf8.display());
            return;
        }
    };
    let abs_path = AbsPath::assert(utf8_path.as_path());
    collect_external_paths(
        sema,
        vfs,
        workspace_root,
        &mut deps,
        tree.syntax(),
        abs_path,
    );

    for item in tree.items() {
        match item {
            ast::Item::Impl(i) => {
                let Some(self_ty) = i
                    .self_ty()
                    .and_then(|ty| resolve_impl_self_type_name(sema, module, &ty))
                else {
                    continue;
                };

                if let Some(struct_rec) = list_of_structures.get_mut(&self_ty) {
                    struct_rec
                        .methods
                        .extend(collect_public_methods_from_impl(&i));
                } else {
                    pending_methods
                        .entry(self_ty)
                        .or_default()
                        .extend(collect_public_methods_from_impl(&i));
                }
            }
            ast::Item::Struct(s) if is_pub(&s) => {
                if let Some(mut struct_with_pubs) =
                    extract_struct_definition_with_public_fields(sema, &s)
                {
                    let struct_name = struct_with_pubs.name.clone();
                    if let Some(methods) = pending_methods.remove(&struct_name) {
                        struct_with_pubs.methods.extend(methods);
                    }
                    list_of_structures.insert(struct_name, struct_with_pubs);
                }
            }
            ast::Item::Enum(e) if is_pub(&e) => {
                facts.public_enums.push(e.to_string());
            }
            ast::Item::Trait(t) if is_pub(&t) => {
                facts.public_traits.push(t.to_string());
            }
            ast::Item::Fn(f) if is_pub(&f) => {
                facts.public_functions.push(format_function_signature(&f));
            }
            _ => {}
        }
    }

    for (_, struct_rec) in list_of_structures {
        facts.public_structs.push(struct_rec);
    }

    write_facts(vfs, file_id, &facts);
    write_deps(vfs, file_id, &deps);
}

fn main() {
    let args = Args::parse();
    let workspace_root = if args.manifest.is_file() {
        args.manifest
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    } else {
        args.manifest.clone()
    };
    // `vfs` reports absolute paths, so the workspace root used for filtering must
    // be absolute too; otherwise `starts_with` would reject every file. Fall back
    // to the original path if canonicalization fails (e.g. it does not exist).
    let workspace_root = workspace_root.canonicalize().unwrap_or(workspace_root);

    let (host, vfs, _workspace) = load_db(&args.manifest);
    let db: &RootDatabase = host.raw_database();

    ra_ap_hir::attach_db(db, || {
        let sema = Semantics::new(db);

        for (file_id, _) in vfs.iter() {
            process_file(&sema, db, &vfs, &workspace_root, file_id);
        }
    });
}
