use std::{collections::HashSet, path::PathBuf};

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ModuleFacts {
    pub package_path: String,
    pub file_path: PathBuf,
    pub public_symbols: Symbols,
}

#[derive(Debug, Serialize)]
pub struct ModuleDeps {
    pub package_path: String,
    pub file_path: PathBuf,
    pub dependencies: HashSet<Dependency>,
}

#[derive(Debug, Serialize, Hash, Eq, PartialEq)]
pub struct Dependency {
    pub package_path: String,
    pub file_path: PathBuf,
    pub spec_path: PathBuf,
}

#[derive(Debug, Default, Serialize)]
pub struct Symbols {
    pub functions: Vec<FunctionInfo>,
    pub methods: Vec<MethodInfo>,
    pub structs: Vec<StructInfo>,
    pub types: Vec<TypeInfo>,
}

#[derive(Debug, Serialize, Clone)]
pub struct FunctionInfo {
    pub name: String,
    pub signature: String,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub doc: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct MethodInfo {
    pub name: String,
    pub signature: String,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub doc: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct StructInfo {
    pub name: String,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub doc: String,
    pub fields: Vec<FieldInfo>,
}

#[derive(Debug, Serialize, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub doc: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct TypeInfo {
    pub name: String,
    pub signature: String,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub doc: String,
}
