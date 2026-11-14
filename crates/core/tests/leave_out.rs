//! What the suggestions leave out of a real backup.

use std::collections::HashSet;
use std::sync::Arc;

use xkyhold_core::backup::{BackupOptions, backup};
use xkyhold_core::crypto::Argon2Params;
use xkyhold_core::destination::MemoryDestination;
use xkyhold_core::repo::{Options, Repository};
use xkyhold_core::rules::Exclusions;
use xkyhold_core::tree::EntryKind;

const FAST: Argon2Params = Argon2Params { memory_kib: 1024, iterations: 1, parallelism: 1 };

/// "Rust build folders" leaves out a `target` folder only beside a
/// `Cargo.toml`; any other folder called `target` is kept.
#[test]
fn a_target_folder_is_left_out_only_beside_cargo_toml() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    for d in ["code/app/target/debug", "Photos/target", "code/odd/target"] {
        std::fs::create_dir_all(h.join(d)).unwrap();
    }
    std::fs::write(h.join("code/app/Cargo.toml"), b"[package]").unwrap();
    std::fs::write(h.join("code/app/target/debug/app"), b"binary").unwrap();
    std::fs::write(h.join("Photos/target/beach.jpg"), b"jpeg").unwrap();
    // A folder called Cargo.toml is not a Cargo.toml.
    std::fs::create_dir(h.join("code/odd/Cargo.toml")).unwrap();
    std::fs::write(h.join("code/odd/target/x"), b"x").unwrap();

    let (mut repo, _) =
        Repository::create(Arc::new(MemoryDestination::default()), "pw", FAST, "t".into(), Options::default()).unwrap();
    let mut o = BackupOptions::new("pc".into(), "2026-11-14T10:00:00.000Z".into());
    o.mount_points = Some(HashSet::new());
    o.exclusions = Exclusions { presets: vec!["rust-target".into()], ..Exclusions::default() };
    let snap = backup(&mut repo, h, o, &mut |_| {}).unwrap();

    let names = |tree: &str| -> Vec<(String, String)> {
        repo.load_tree(tree)
            .unwrap()
            .into_iter()
            .map(|e| {
                let sub = match &e.kind {
                    EntryKind::Dir { subtree, .. } => subtree.clone(),
                    _ => String::new(),
                };
                (e.display_name(), sub)
            })
            .collect()
    };
    let find = |list: &[(String, String)], name: &str| list.iter().find(|(n, _)| n == name).map(|(_, s)| s.clone());
    let top = names(&snap.roots[0].tree);
    let code = names(&find(&top, "code").unwrap());
    let app: Vec<String> = names(&find(&code, "app").unwrap()).into_iter().map(|(n, _)| n).collect();
    assert_eq!(app, ["Cargo.toml"], "the build folder is left out");
    let photos: Vec<String> = names(&find(&top, "Photos").unwrap()).into_iter().map(|(n, _)| n).collect();
    assert_eq!(photos, ["target"], "a folder that is only called target is kept");
    let odd: Vec<String> = names(&find(&code, "odd").unwrap()).into_iter().map(|(n, _)| n).collect();
    assert_eq!(odd, ["Cargo.toml", "target"]);
}
