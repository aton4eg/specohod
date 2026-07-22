use serde::Serialize;

#[derive(Serialize)]
pub struct WorkspaceGraph {
    pub crates: Vec<CrateInfo>,
}

#[derive(Serialize)]
pub struct CrateInfo {
    pub name: String,
    pub root_dir: String,
    pub entry_points: Vec<String>,
    pub modules: Vec<ModuleInfo>,
}

#[derive(Serialize)]
pub struct ModuleInfo {
    pub file_path: String,
    pub module_path: String,
    pub submodules: Vec<ModuleInfo>,
}
