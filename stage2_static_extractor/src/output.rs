use std::fs;

use ra_ap_base_db::FileId;
use ra_ap_vfs::Vfs;

use crate::types::{ModuleDeps, ModuleFacts};

pub fn write_facts(vfs: &Vfs, file_id: FileId, facts: &ModuleFacts) {
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

pub fn write_deps(vfs: &Vfs, file_id: FileId, deps: &ModuleDeps) {
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
