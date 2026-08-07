use std::fs;

use ra_ap_base_db::FileId;
use ra_ap_vfs::Vfs;
use serde::Serialize;

use crate::types::{ModuleDeps, ModuleFacts};

fn write_json_output<T: Serialize>(vfs: &Vfs, file_id: FileId, suffix: &str, data: &T) {
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
    let output = parent.join(format!("{}.{}.json", file_stem, suffix));
    let json = serde_json::to_string_pretty(data).expect("Serialization must succeed");
    if let Err(e) = fs::write(&output, json) {
        eprintln!("Failed to write {output}: {e}");
    }
}

/// Writes a module's facts (public API surface) to a `<stem>.facts.json` file
/// next to the source file.
pub fn write_facts(vfs: &Vfs, file_id: FileId, facts: &ModuleFacts) {
    write_json_output(vfs, file_id, "facts", facts)
}

/// Writes a module's dependencies to a `<stem>.deps.json` file next to the
/// source file.
pub fn write_deps(vfs: &Vfs, file_id: FileId, deps: &ModuleDeps) {
    write_json_output(vfs, file_id, "deps", deps)
}
