use std::collections::HashSet;
use std::path::PathBuf;

use serde::Serialize;

#[derive(Serialize)]
pub struct ModuleFacts {
    pub module_path: String,
    pub file_path: PathBuf,
    pub public_structs: Vec<StructInfo>,
    pub public_enums: Vec<String>,
    pub public_traits: Vec<String>,
    pub public_functions: Vec<String>,
}

#[derive(Serialize)]
pub struct ModuleDeps {
    pub module_path: String,
    pub file_path: PathBuf,
    pub dependency: HashSet<Dependency>,
}

#[derive(Debug, Serialize, Hash, Eq, PartialEq)]
pub struct Dependency {
    pub module_path: String,
    pub file_path: PathBuf,
    pub spec_path: PathBuf,
}

#[derive(Serialize, Clone)]
pub struct StructInfo {
    pub name: String,
    pub fields: String,
    pub methods: Vec<String>,
}
