//! The Rust half of the cross round trip with the TypeScript engine. Run
//! through `scripts/cross-ts.sh`, which sets `CROSS_DIR`.

use std::collections::{BTreeMap, HashSet};
use std::os::unix::fs::symlink;

use serde_json::{Value, json};
use xkyhold_core::backup::{BackupOptions, backup};
use xkyhold_core::bytes::{from_base64, sha256_hex, to_base64};
use xkyhold_core::crypto::{Argon2Params, Credential};
use xkyhold_core::destination::{Destination, Dir, MemoryDestination};
use xkyhold_core::repo::{Options, Repository};
use xkyhold_core::restore::{RestoreOptions, restore};
use xkyhold_core::tree::EntryKind;
use zeroize::Zeroizing;

fn dir() -> String {
    std::env::var("CROSS_DIR").expect("run through scripts/cross-ts.sh")
}

#[test]
#[ignore = "run by scripts/cross-ts.sh"]
fn rust_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home.join("Documents/Sub")).unwrap();
    std::fs::create_dir(home.join("Empty")).unwrap();
    let mut files = BTreeMap::new();
    for (path, data) in [
        ("Documents/a.txt", b"hello from Rust".to_vec()),
        ("Documents/big.bin", (0..20_000u32).map(|i| (i * 7 % 251) as u8).collect()),
        ("Documents/Sub/é.txt", b"accented".to_vec()),
        ("empty.txt", Vec::new()),
    ] {
        std::fs::write(home.join(path), &data).unwrap();
        files.insert(path.to_owned(), sha256_hex(&data));
    }
    symlink("Documents/a.txt", home.join("link")).unwrap();
    let fast = Argon2Params { memory_kib: 1024, iterations: 1, parallelism: 1 };
    let options = Options { pack_size: 4096, index_every: 2 };
    let (mut repo, _) =
        Repository::create(MemoryDestination::default(), "pw", fast, "2026-11-14T00:00:00.000Z".into(), options)
            .unwrap();
    let mut o = BackupOptions::new("rust".into(), "2026-11-14T00:00:00.000Z".into());
    o.chunk_size = 4096;
    o.mount_points = Some(HashSet::new());
    backup(&mut repo, home, o, &mut |_| {}).unwrap();
    let objects: BTreeMap<String, String> = repo
        .dest
        .objects
        .lock()
        .unwrap()
        .iter()
        .map(|((d, n), data)| (format!("{}/{n}", d.as_str()), to_base64(data)))
        .collect();
    let fx = json!({ "password": "pw", "objects": objects, "files": files, "links": { "link": "Documents/a.txt" } });
    std::fs::write(format!("{}/rust-repo.json", dir()), fx.to_string()).unwrap();
}

#[test]
#[ignore = "run by scripts/cross-ts.sh"]
fn rust_reads() {
    let fx: Value = serde_json::from_str(&std::fs::read_to_string(format!("{}/ts-repo.json", dir())).unwrap()).unwrap();
    let dest = MemoryDestination::default();
    for (key, value) in fx["objects"].as_object().unwrap() {
        let (d, n) = key.split_once('/').unwrap();
        dest.put(Dir::parse(d).unwrap(), n, &from_base64(value.as_str().unwrap()).unwrap()).unwrap();
    }
    let repo = Repository::open(dest, &Credential::Password(Zeroizing::new("pw".into())), Options::default()).unwrap();
    assert!(!repo.writable(), "format 1 is read-only here");
    let snaps = repo.list_snapshots().unwrap();
    assert_eq!((snaps.len(), snaps[0].version()), (1, 1));
    let root = &snaps[0].roots[0];
    let out = tempfile::tempdir().unwrap();
    std::fs::create_dir(out.path().join(&root.name)).unwrap();
    let entries = repo.load_tree(&root.tree).unwrap();
    restore(&repo, &entries, &out.path().join(&root.name), &RestoreOptions::default(), &mut |_| {}).unwrap();
    for (path, want) in fx["files"].as_object().unwrap() {
        let data = std::fs::read(out.path().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(sha256_hex(&data), want["sha256"].as_str().unwrap(), "{path}");
        let md = std::fs::metadata(out.path().join(path)).unwrap();
        let ms = md.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        assert_eq!(ms, want["mtime"].as_u64().unwrap(), "{path} keeps its time");
    }
    assert!(out.path().join("Documents/Empty").is_dir());
    assert!(entries.iter().all(|e| !matches!(e.kind, EntryKind::Unknown(_))));
}
