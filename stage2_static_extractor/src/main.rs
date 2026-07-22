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
use ra_ap_syntax::ast::{HasModuleItem, HasName};
use ra_ap_syntax::{AstNode, ast};
use ra_ap_vfs::{AbsPath, Vfs};

use args::Args;
use collect::collect_external_paths;
use database::load_db;
use impl_helpers::{
    enrich_struct_with_public_methods, collect_public_methods_from_impl, resolve_impl_self_type_name,
};
use output::{write_deps, write_facts};
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

    if file_path.components().any(|c| c.as_os_str() == "tests") {
        return;
    }

    let tree = sema.parse(EditionedFileId::current_edition(db, file_id));
    let module = match sema.file_to_module_def(file_id) {
        Some(m) => m,
        _ => return,
    };

    let segments: Vec<String> = module
        .path_to_root(db)
        .into_iter()
        .rev()
        .filter_map(|m| m.name(db).map(|name| name.as_str().to_string()))
        .collect();

    let crate_name = module
        .krate(db)
        .display_name(db)
        .unwrap()
        .crate_name()
        .as_str()
        .to_string();
    let module_path = if segments.is_empty() {
        crate_name.to_string()
    } else {
        format!("{}::{}", crate_name, segments.join("::"))
    };

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

    let utf8_path =
        ra_ap_paths::Utf8PathBuf::from_path_buf(std::path::PathBuf::from(file_path)).unwrap();
    let abs_path = AbsPath::assert(utf8_path.as_path());
    collect_external_paths(sema, vfs, workspace_root, &mut deps, tree.syntax(), abs_path);

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
                    enrich_struct_with_public_methods(sema, &i, struct_rec);
                } else {
                    pending_methods
                        .entry(self_ty)
                        .or_default()
                        .extend(collect_public_methods_from_impl(&i));
                }
            }
            ast::Item::Struct(s) if is_pub(&s) => {
                if let Some(struct_with_pubs) =
                    extract_struct_definition_with_public_fields(sema, &s)
                {
                    let struct_name = s.name().unwrap().to_string();
                    let mut struct_with_pubs = struct_with_pubs;
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
        facts.public_structs.push(struct_rec.clone());
    }

    write_facts(vfs, file_id, &facts);
    write_deps(vfs, file_id, &deps);
}

fn main() {
    let args = Args::parse();
    let workspace_root = if args.manifest.is_file() {
        args.manifest.parent().unwrap().to_path_buf()
    } else {
        args.manifest.clone()
    };

    let (host, vfs, _workspace) = load_db(&args.manifest);
    let db: &RootDatabase = host.raw_database();

    ra_ap_hir::attach_db(db, || {
        let sema = Semantics::new(db);

        for (file_id, _) in vfs.iter() {
            process_file(&sema, db, &vfs, &workspace_root, file_id);
        }
    });
}