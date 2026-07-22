use std::path::Path;

use ra_ap_ide::AnalysisHost;
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace};
use ra_ap_paths::Utf8PathBuf;
use ra_ap_project_model::{ProjectManifest, ProjectWorkspace};
use ra_ap_vfs::{AbsPathBuf, Vfs};
use rustc_hash::FxHashMap;

pub fn load_db(manifest_path: &Path) -> (AnalysisHost, Vfs, ProjectWorkspace) {
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
