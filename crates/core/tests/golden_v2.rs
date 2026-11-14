//! Format version 2 test vectors (FORMAT-v2.md section 10).
//!
//! `tests/fixtures/golden-v2-repo.json` was written once by `generate`
//! (run by hand with `--ignored`) and is never regenerated: the generator
//! refuses to overwrite it. If `restores_golden_v2` fails, the change under
//! test would make existing version 2 backups unreadable.

use std::collections::{BTreeMap, HashSet};
use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use rustix::fs::{AtFlags, Timespec, Timestamps, XattrFlags, utimensat};
use serde_json::{Value, json};
use xkyhold_core::backup::{BackupOptions, backup};
use xkyhold_core::bytes::{from_base64, sha256_hex, to_base64};
use xkyhold_core::crypto::{Argon2Params, Credential};
use xkyhold_core::destination::{Destination, Dir, MemoryDestination};
use xkyhold_core::repo::{Options, Repository};
use xkyhold_core::restore::{RestoreOptions, restore};
use xkyhold_core::rules::Exclusions;
use zeroize::Zeroizing;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden-v2-repo.json");
const PASSWORD: &str = "correct horse battery staple";
const CHUNK: usize = 1024;
/// A POSIX ACL (`setfacl -m u:nobody:r`) as the kernel stores it.
const ACL: &str = "0200000001000600ffffffff02000400feff000004000400ffffffff10000400ffffffff20000400ffffffff";

fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x & 255) as u8
        })
        .collect()
}

fn set_time(path: &Path, sec: i64, nsec: i64) {
    let ts = Timespec { tv_sec: sec, tv_nsec: nsec as _ };
    let c = CString::new(path.as_os_str().as_bytes()).unwrap();
    utimensat(
        rustix::fs::CWD,
        c.as_c_str(),
        &Timestamps { last_access: ts, last_modification: ts },
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .unwrap();
}

/// What a restore must produce, path by path, independent of the engine.
fn describe(root: &Path) -> BTreeMap<Vec<u8>, Value> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<Vec<u8>, Value>) {
        let mut items: Vec<_> = std::fs::read_dir(dir).unwrap().map(|i| i.unwrap().path()).collect();
        items.sort();
        for path in items {
            let rel = path.strip_prefix(root).unwrap().as_os_str().as_bytes().to_vec();
            let md = std::fs::symlink_metadata(&path).unwrap();
            let mut v = json!({ "path": to_base64(&rel), "mtime": [md.mtime(), md.mtime_nsec()] });
            if md.file_type().is_symlink() {
                v["type"] = json!("symlink");
                v["target"] = json!(to_base64(std::fs::read_link(&path).unwrap().as_os_str().as_bytes()));
            } else if md.is_dir() {
                walk(root, &path, out);
                v["type"] = json!("dir");
                v["mode"] = json!(md.mode() & 0o7777);
            } else {
                let data = std::fs::read(&path).unwrap();
                v["type"] = json!("file");
                v["mode"] = json!(md.mode() & 0o7777);
                v["size"] = json!(data.len());
                v["sha256"] = json!(sha256_hex(&data));
                let mut xattrs = BTreeMap::new();
                for name in ["user.xdg.origin.url", "system.posix_acl_access"] {
                    let mut buf = vec![0u8; 4096];
                    if let Ok(n) = rustix::fs::lgetxattr(&path, name, &mut buf[..]) {
                        xattrs.insert(name, to_base64(&buf[..n]));
                    }
                }
                v["xattrs"] = json!(xattrs);
            }
            out.insert(rel, v);
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
#[ignore = "writes the frozen fixture; run once by hand"]
fn generate() {
    assert!(!Path::new(FIXTURE).exists(), "golden-v2-repo.json exists and must never be regenerated");
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mk = |p: &str| std::fs::create_dir_all(home.join(p)).unwrap();
    let w = |p: &[u8], data: &[u8], mode: u32| {
        let path = home.join(OsStr::from_bytes(p));
        std::fs::write(&path, data).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    for d in ["Documents", "Empty", "Private", "Disk", "Data", "code/node_modules/x", "Downloads", ".cache"] {
        mk(d);
    }
    w(b"run.sh", b"#!/bin/sh\necho hello\n", 0o755);
    w(b"secret.key", b"not for others", 0o600);
    w(b"empty.txt", b"", 0o644);
    w(b"Documents/big.bin", &noise(3000, 2), 0o644);
    w(b"Documents/old.txt", b"from the sixties", 0o644);
    w(b"bad\xffname", b"latin-1 name", 0o644);
    w(b"x\xfe", b"first invalid", 0o644);
    w(b"x\xff", b"second invalid, same display form", 0o644);
    w(b"notes.txt", b"hello world", 0o644);
    w(b"acl.txt", b"shared with nobody", 0o640);
    w(b"setuid-tool", b"#!/bin/sh\n", 0o4755);
    w(b"Private/diary.txt", b"left out by path", 0o600);
    w(b"Downloads/arch.iso", b"left out by pattern", 0o644);
    w(b"code/node_modules/x/index.js", b"left out by preset", 0o644);
    w(b".cache/thumb", b"left out by default", 0o644);
    w(b"Disk/elsewhere.txt", b"not crossed", 0o644);
    w(b"Data/crossed.txt", b"on a disk the user chose to cross", 0o644);
    symlink("notes.txt", home.join("link")).unwrap();
    symlink("does/not/exist", home.join("dangling")).unwrap();
    rustix::fs::setxattr(
        home.join("notes.txt"),
        "user.xdg.origin.url",
        b"https://example.org/notes",
        XattrFlags::empty(),
    )
    .unwrap();
    rustix::fs::setxattr(
        home.join("acl.txt"),
        "system.posix_acl_access",
        &hex::decode(ACL).unwrap(),
        XattrFlags::empty(),
    )
    .unwrap();
    std::fs::set_permissions(home.join("Empty"), std::fs::Permissions::from_mode(0o700)).unwrap();
    for (i, p) in [
        "run.sh",
        "secret.key",
        "empty.txt",
        "Documents/big.bin",
        "notes.txt",
        "acl.txt",
        "setuid-tool",
        "link",
        "dangling",
    ]
    .iter()
    .enumerate()
    {
        set_time(&home.join(p), 1_794_400_000 + i as i64 * 60, 100_000_001 + i as i64 * 111_111);
    }
    set_time(&home.join("Documents/old.txt"), -86_400 * 365, 999_999_999);
    for p in ["Documents", "Empty", "Data", "Disk"] {
        set_time(&home.join(p), 1_794_000_000, 500);
    }

    let dest = MemoryDestination::default();
    let fast = Argon2Params { memory_kib: 1024, iterations: 2, parallelism: 1 };
    let options = Options { pack_size: 4096, index_every: 2, ..Options::default() };
    let (mut repo, recovery) =
        Repository::create(dest, PASSWORD, fast, "2026-11-14T12:00:00.000Z".into(), options).unwrap();
    let mut o = BackupOptions::new("golden".into(), "2026-11-14T12:00:00.000Z".into());
    o.chunk_size = CHUNK;
    o.mount_points = Some(HashSet::from([
        home.join("Disk").as_os_str().as_bytes().to_vec(),
        home.join("Data").as_os_str().as_bytes().to_vec(),
    ]));
    o.exclusions = Exclusions {
        presets: vec!["node-modules".into()],
        paths: vec![b"Private".to_vec()],
        patterns: vec!["**/*.iso".into()],
        cross_mounts: vec![b"Data".to_vec()],
        repository: None,
    };
    let snap = backup(&mut repo, &home, o, &mut |_| {}).unwrap();

    // The expected restore, built from the source and the rules of the spec.
    let mut expected = describe(&home);
    for gone in [
        &b"Private"[..],
        b"Private/diary.txt",
        b"Downloads/arch.iso",
        b"code/node_modules",
        b"code/node_modules/x",
        b"code/node_modules/x/index.js",
        b".cache",
        b".cache/thumb",
        b"Disk/elsewhere.txt",
    ] {
        expected.remove(gone).unwrap_or_else(|| panic!("{}", String::from_utf8_lossy(gone)));
    }
    let tool = expected.get_mut(&b"setuid-tool"[..]).unwrap();
    tool["mode"] = json!(0o755); // setuid is never restored
    // Folders whose contents were left out changed time in the source when
    // those contents were made; their stored time is what is restored.
    for p in [&b"code"[..], b"Downloads", b"Disk"] {
        expected.get_mut(p).unwrap()["mtime"] = Value::Null;
    }

    let objects: BTreeMap<String, String> = repo
        .dest
        .objects
        .lock()
        .unwrap()
        .iter()
        .map(|((dir, name), data)| (format!("{}/{name}", dir.as_str()), to_base64(data)))
        .collect();
    let fixture = json!({
        "note": "Format v2 compatibility fixture (FORMAT-v2.md section 10). Written once by XkyHold-lx on 2026-11-14; never regenerate. Paths, names and link targets are base64 of their bytes. An expected mtime of null means 'not compared' (a folder whose left-out contents changed it in the source). The chunk size used is in chunkSize.",
        "password": PASSWORD,
        "recoveryKey": *recovery,
        "chunkSize": CHUNK,
        "snapshotId": snap.id,
        "expected": expected.values().collect::<Vec<_>>(),
        "objects": objects,
    });
    std::fs::write(FIXTURE, serde_json::to_string_pretty(&fixture).unwrap() + "\n").unwrap();
}

fn load() -> Value {
    serde_json::from_str(&std::fs::read_to_string(FIXTURE).expect("golden-v2-repo.json")).unwrap()
}

fn open(golden: &Value, credential: Credential) -> Repository<MemoryDestination> {
    let dest = MemoryDestination::default();
    for (key, value) in golden["objects"].as_object().unwrap() {
        let (dir, name) = key.split_once('/').unwrap();
        dest.put(Dir::parse(dir).unwrap(), name, &from_base64(value.as_str().unwrap()).unwrap()).unwrap();
    }
    Repository::open(dest, &credential, Options::default()).unwrap()
}

#[test]
fn restores_golden_v2() {
    let golden = load();
    for credential in [
        Credential::Password(Zeroizing::new(golden["password"].as_str().unwrap().into())),
        Credential::RecoveryKey(Zeroizing::new(golden["recoveryKey"].as_str().unwrap().into())),
    ] {
        let repo = open(&golden, credential);
        assert!(repo.writable());
        let snaps = repo.list_snapshots().unwrap();
        assert_eq!(snaps.len(), 1);
        let snap = &snaps[0];
        assert_eq!((snap.id.as_str(), snap.version()), (golden["snapshotId"].as_str().unwrap(), 2));
        let root = &snap.roots[0];
        let kinds: HashSet<&str> = root.left_out.as_ref().unwrap().iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(kinds, HashSet::from(["default", "preset", "path", "pattern", "cross-mount"]));
        assert_eq!(snap.stats.mounts, Some(1));

        let out = tempfile::tempdir().unwrap();
        let entries = repo.load_tree(&root.tree).unwrap();
        restore(&repo, &entries, out.path(), &RestoreOptions::default(), &mut |_| {}).unwrap();
        let got = describe(out.path());
        let expected: Vec<Value> = golden["expected"].as_array().unwrap().clone();
        assert_eq!(
            got.len(),
            expected.len(),
            "{:?}",
            got.keys().map(|k| String::from_utf8_lossy(k)).collect::<Vec<_>>()
        );
        for want in expected {
            let path = from_base64(want["path"].as_str().unwrap()).unwrap();
            let mut have =
                got.get(&path).unwrap_or_else(|| panic!("missing {}", String::from_utf8_lossy(&path))).clone();
            if want["mtime"].is_null() {
                have["mtime"] = Value::Null;
            }
            // Extended attributes are compared only where this file system takes them.
            if want.get("xattrs").is_some() && have["xattrs"] != want["xattrs"] {
                let supported = rustix::fs::setxattr(out.path(), "user.probe", b"1", XattrFlags::empty()).is_ok();
                assert!(!supported, "{}: {have} != {want}", String::from_utf8_lossy(&path));
                have["xattrs"] = want["xattrs"].clone();
            }
            assert_eq!(have, want, "{}", String::from_utf8_lossy(&path));
        }
    }
}
