use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use ra_ap_hir::{Adt, EditionedFileId, ModuleDef, Semantics};
use ra_ap_ide::{AnalysisHost, RootDatabase};
use ra_ap_load_cargo::{load_workspace, LoadCargoConfig, ProcMacroServerChoice};
use ra_ap_paths::Utf8PathBuf;
use ra_ap_project_model::{ProjectManifest, ProjectWorkspace};
use ra_ap_syntax::ast::{HasAttrs, HasModuleItem, HasName};
use ra_ap_syntax::{ast, AstNode};
use ra_ap_vfs::{AbsPath, AbsPathBuf, Vfs};
use rustc_hash::FxHashMap;
use serde::Serialize;

#[derive(Parser)]
struct Args {
    #[arg(short, long)]
    manifest: PathBuf,
}

#[derive(Debug, Serialize)]
struct ModuleFacts {
    module_path: String,
    file_path: PathBuf,
    public_structs: Vec<StructInfo>,
    public_enums: Vec<String>,
    public_traits: Vec<String>,
    public_functions: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ModuleDeps {
    module_path: String,
    file_path: PathBuf,
    dependency: HashSet<Dependency>,
}

#[derive(Debug, Serialize, Hash, Eq, PartialEq)]
struct Dependency {
    module_path: String,
    file_path: PathBuf,
    spec_path: PathBuf,
}

#[derive(Debug, Serialize, Clone)]
struct StructInfo {
    name: String,
    fields: String,
    methods: Vec<String>,
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
            if let Some(file_path) = vfs.file_path(file_id).as_path() {
                if !file_path.components().any(|c| c.as_os_str() == "tests") {
                    let tree = sema.parse(EditionedFileId::current_edition(db, file_id));
                    let module = match sema.file_to_module_def(file_id) {
                        Some(m) => m,
                        _ => continue,
                    };

                    let segments: Vec<String> = module
                        .path_to_root(db)
                        .into_iter()
                        .rev() // go from root to current
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

                    collect_external_paths(
                        &sema,
                        &vfs,
                        &workspace_root,
                        &mut deps,
                        tree.syntax(),
                        file_path,
                    );

                    for item in tree.items() {
                        match item {
                            ast::Item::Impl(i) => {
                                let Some(self_ty) = i
                                    .self_ty()
                                    .and_then(|ty| impl_self_type_name(&sema, module, &ty))
                                else {
                                    continue;
                                };

                                if let Some(struct_rec) = list_of_structures.get_mut(&self_ty) {
                                    struct_definition_with_public_methods(&sema, &i, struct_rec);
                                } else {
                                    pending_methods
                                        .entry(self_ty)
                                        .or_default()
                                        .extend(public_methods_from_impl(&i));
                                }
                            }
                            ast::Item::Struct(s) if is_pub(&s) => {
                                if let Some(struct_with_pubs) =
                                    struct_definition_with_public_fields(&sema, &s)
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
                                facts.public_functions.push(function_signature(&f));
                            }
                            _ => {}
                        }
                    }

                    for (_, struct_rec) in list_of_structures {
                        facts.public_structs.push(struct_rec.clone());
                    }

                    write_facts(&vfs, file_id, &facts);
                    write_deps(&vfs, file_id, &deps);
                }
            }
        }
    });
}

fn load_db(manifest_path: &Path) -> (AnalysisHost, Vfs, ProjectWorkspace) {
    let utf8 = Utf8PathBuf::from(manifest_path.to_string_lossy().to_string());
    let abs = AbsPathBuf::assert(utf8);
    let manifest = ProjectManifest::discover_single(&abs).unwrap();
    let workspace = ProjectWorkspace::load(manifest, &Default::default(), &move |_| {}).unwrap();

    let config = LoadCargoConfig {
        load_out_dirs_from_check: true,
        with_proc_macro_server: ProcMacroServerChoice::Sysroot,
        prefill_caches: false,
        num_worker_threads: 4,
        proc_macro_processes: 4,
    };
    let extra_env = FxHashMap::default();
    let (db, vfs, _proc_macro) = load_workspace(workspace.clone(), &extra_env, &config).unwrap();
    let host = AnalysisHost::with_database(db);
    (host, vfs, workspace)
}

fn is_pub<N: ast::HasVisibility>(node: &N) -> bool {
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

fn write_facts(vfs: &Vfs, file_id: ra_ap_base_db::FileId, facts: &ModuleFacts) {
    let vfs_path = vfs.file_path(file_id);
    let path = match vfs_path.as_path() {
        Some(p) => p.to_path_buf(),
        None => return,
    };
    let file_stem = match path.file_stem() {
        Some(s) => s.to_string(),
        None => return,
    };
    let parent = match path.parent() {
        Some(p) => p,
        None => return,
    };
    let output = parent.join(format!("{}.facts.json", file_stem));
    let json = serde_json::to_string_pretty(facts).unwrap();
    let _ = fs::write(output, json);
}

fn write_deps(vfs: &Vfs, file_id: ra_ap_base_db::FileId, deps: &ModuleDeps) {
    let vfs_path = vfs.file_path(file_id);
    let path = match vfs_path.as_path() {
        Some(p) => p.to_path_buf(),
        None => return,
    };
    let file_stem = match path.file_stem() {
        Some(s) => s.to_string(),
        None => return,
    };
    let parent = match path.parent() {
        Some(p) => p,
        None => return,
    };
    let output = parent.join(format!("{}.deps.json", file_stem));
    let json = serde_json::to_string_pretty(deps).unwrap();
    let _ = fs::write(output, json);
}

fn struct_definition_with_public_fields(
    _sema: &Semantics<RootDatabase>,
    s: &ast::Struct,
) -> Option<StructInfo> {
    let mut public_struct_info = StructInfo {
        name: s.name()?.to_string(),
        fields: "".to_string(),
        methods: Vec::new(),
    };
    let mut public_fields = Vec::new();

    match s.field_list() {
        Some(ast::FieldList::RecordFieldList(field_list)) => {
            for field in field_list.fields() {
                if is_pub(&field) {
                    public_fields.push(field.syntax().text().to_string());
                }
            }
        }
        Some(ast::FieldList::TupleFieldList(field_list)) => {
            for field in field_list.fields() {
                if is_pub(&field) {
                    public_fields.push(field.syntax().text().to_string());
                }
            }
        }
        None => {}
    }
    public_struct_info.fields = public_fields.join(", ");
    Some(public_struct_info)

    //Some(format!("pub struct {}{}{{{}}};", name, generics, public_fields.join(", ")))
}

fn collect_external_paths(
    sema: &Semantics<RootDatabase>,
    vfs: &Vfs,
    workspace_root: &Path,
    deps: &mut ModuleDeps,
    node: &ra_ap_syntax::SyntaxNode,
    current_module: &AbsPath,
) {
    let current_path: PathBuf = current_module.into();
    if is_inside_test_module(node) {
        return; // completely ignore this AST branch
    }
    if let Some(path) = ast::Path::cast(node.clone()) {
        if let Some(ra_ap_hir::PathResolution::Def(def)) = sema.resolve_path(&path) {
            // Get the definition file path
            if let Some(file_path) = file_path_for_def(sema, vfs, def) {
                // Check if file is inside workspace
                if file_path.starts_with(workspace_root) && current_path != file_path {
                    if let Some(mod_path) = canonical_path(sema, def) {
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
        }
    }
    if let Some(module) = ast::Module::cast(node.clone()) {
        if let Some(hir_module) = sema.to_module_def(&module) {
            if let Some(file_path) = file_path_for_module(sema, vfs, hir_module) {
                if file_path.starts_with(workspace_root) && current_path != file_path {
                    if let Some(mod_path) = canonical_path(sema, ModuleDef::Module(hir_module)) {
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
        }
    }
    for child in node.children() {
        collect_external_paths(sema, vfs, workspace_root, deps, &child, current_module);
    }
}

fn file_path_for_module(
    sema: &Semantics<RootDatabase>,
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

fn file_path_for_def(sema: &Semantics<RootDatabase>, vfs: &Vfs, def: ModuleDef) -> Option<PathBuf> {
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
    file_path_for_module(sema, vfs, module)
}

fn absolute_module_path(sema: &Semantics<RootDatabase>, module: ra_ap_hir::Module) -> String {
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

fn canonical_path(sema: &Semantics<RootDatabase>, def: ModuleDef) -> Option<String> {
    let db = sema.db;
    match def {
        ModuleDef::Module(m) => Some(absolute_module_path(sema, m)),
        ModuleDef::Adt(adt) => match adt {
            Adt::Struct(s) => Some(absolute_module_path(sema, s.module(db))),
            Adt::Enum(e) => Some(absolute_module_path(sema, e.module(db))),
            Adt::Union(u) => Some(absolute_module_path(sema, u.module(db))),
        },
        ModuleDef::Trait(t) => Some(absolute_module_path(sema, t.module(db))),
        ModuleDef::Function(f) => Some(absolute_module_path(sema, f.module(db))),
        _ => None,
    }
}

fn function_signature(f: &ast::Fn) -> String {
    let mut parts = Vec::new();
    for child in f.syntax().children() {
        // Skip attributes, they are not part of signature
        if ast::Attr::can_cast(child.kind()) {
            continue;
        }
        // Stop when we reach the function body
        if ast::BlockExpr::can_cast(child.kind()) {
            break;
        }
        parts.push(child.text().to_string());
    }
    parts.join(" ")
}

fn impl_self_type_name(
    sema: &Semantics<RootDatabase>,
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

fn is_inside_test_module(node: &ra_ap_syntax::SyntaxNode) -> bool {
    let mut current = node.clone();
    while let Some(parent) = current.parent() {
        if let Some(module) = ast::Module::cast(parent.clone()) {
            if let Some(name) = module.name() {
                if name.text() == "test" || name.text() == "tests" {
                    return true;
                }
            }
            for attr in module.attrs() {
                if let Some(path) = attr.path() {
                    if path.syntax().text() == "cfg" {
                        if let Some(tt) = attr.token_tree() {
                            if tt.syntax().text().to_string().contains("test")
                                || tt.syntax().text().to_string().contains("tests")
                            {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        current = parent; // now parent is not moved above
    }
    false
}

fn struct_definition_with_public_methods(
    _sema: &Semantics<RootDatabase>,
    i: &ast::Impl,
    s: &mut StructInfo,
) {
    s.methods.extend(public_methods_from_impl(i));
}

fn public_methods_from_impl(i: &ast::Impl) -> Vec<String> {
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
            methods.push(function_signature(&item));
        }
    }

    methods

    //Some(format!("pub struct {}{}{{{}}};", name, generics, public_fields.join(", ")))
}
