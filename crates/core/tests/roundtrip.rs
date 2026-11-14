//! Backup and restore of a test tree with metadata, in temporary folders
//! only. The criterion of Phase 2: the restored tree is identical in
//! content, permissions, links, times and extended attributes.

use std::collections::{BTreeMap, HashSet};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, Mode, Timespec, Timestamps, utimensat};
use xkyhold_core::backup::{BackupOptions, Progress, backup};
use xkyhold_core::crypto::{Argon2Params, Credential};
use xkyhold_core::destination::{Destination, MemoryDestination};
use xkyhold_core::repo::{Options, Repository};
use xkyhold_core::restore::{Conflict, RestoreOptions, restore};
use xkyhold_core::rules::Exclusions;
use xkyhold_core::tree::EntryKind;
use zeroize::Zeroizing;

const FAST: Argon2Params = Argon2Params { memory_kib: 1024, iterations: 1, parallelism: 1 };
const CHUNK: usize = 1000;

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

fn xattrs_supported(dir: &Path) -> bool {
    let probe = dir.join(".probe");
    std::fs::write(&probe, b"").unwrap();
    let ok = rustix::fs::setxattr(&probe, "user.probe", b"1", rustix::fs::XattrFlags::empty()).is_ok();
    std::fs::remove_file(probe).unwrap();
    ok
}

/// Builds the test home folder. Returns whether xattrs are in play.
fn build_home(home: &Path) -> bool {
    let xattrs = xattrs_supported(home);
    let w = |p: &str, data: &[u8], mode: u32| {
        let path = home.join(p);
        std::fs::write(&path, data).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    std::fs::create_dir_all(home.join("Documents/Work")).unwrap();
    std::fs::create_dir_all(home.join(".config/app")).unwrap();
    std::fs::create_dir(home.join("Empty")).unwrap();
    std::fs::create_dir_all(home.join(".cache/thumbs")).unwrap();
    std::fs::create_dir_all(home.join("build/out")).unwrap();
    std::fs::write(home.join("build/CACHEDIR.TAG"), b"Signature: 8a477f597d28d172789f06886806bc55\n").unwrap();
    std::fs::create_dir(home.join("Disk")).unwrap();
    w("Disk/on-other-disk.txt", b"not here", 0o644);

    w("notes.txt", b"hello world", 0o644);
    w("run.sh", b"#!/bin/sh\necho hi\n", 0o755);
    w(".config/app/secret", b"key", 0o600);
    w("empty.txt", b"", 0o644);
    w("Documents/big.bin", &noise(2500, 7), 0o640);
    w("Documents/Work/plan.md", b"# plan", 0o644);
    w("setuid-tool", b"x", 0o4755);
    w(".cache/thumbs/a.png", b"cached", 0o644);
    w("build/out/a.o", b"obj", 0o644);
    std::fs::write(home.join(std::ffi::OsStr::from_bytes(b"bad\xffname")), b"latin1").unwrap();
    symlink("notes.txt", home.join("link")).unwrap();
    symlink("nowhere/at/all", home.join("dangling")).unwrap();
    symlink("/etc/passwd", home.join("outside")).unwrap();
    rustix::fs::mknodat(rustix::fs::CWD, &home.join("fifo"), rustix::fs::FileType::Fifo, Mode::from_raw_mode(0o600), 0)
        .unwrap();
    std::fs::set_permissions(home.join("Empty"), std::fs::Permissions::from_mode(0o700)).unwrap();
    if xattrs {
        rustix::fs::setxattr(
            &home.join("notes.txt"),
            "user.xdg.origin.url",
            b"https://example.org",
            rustix::fs::XattrFlags::empty(),
        )
        .unwrap();
    }
    // Times last, folders after their contents; one before 1970.
    for (i, p) in ["notes.txt", "run.sh", ".config/app/secret", "empty.txt", "Documents/big.bin", "link", "dangling"]
        .iter()
        .enumerate()
    {
        set_time(&home.join(p), 1_794_400_000 + i as i64, 123_456_789 + i as i64);
    }
    set_time(&home.join("Documents/Work/plan.md"), -86_400, 5);
    for p in ["Documents/Work", "Documents", "Empty", ".config/app", ".config"] {
        set_time(&home.join(p), 1_794_000_000, 42);
    }
    xattrs
}

#[derive(Debug, PartialEq)]
enum Node {
    File { data: Vec<u8>, mode: u32, mtime: (i64, i64), xattr: Option<Vec<u8>> },
    Dir { mode: u32, mtime: (i64, i64) },
    Link { target: Vec<u8>, mtime: (i64, i64) },
}

/// Everything a restore should reproduce, by path.
fn snapshot_of(root: &Path, skip: &[&str]) -> BTreeMap<Vec<u8>, Node> {
    fn walk(root: &Path, dir: &Path, skip: &[&str], out: &mut BTreeMap<Vec<u8>, Node>) {
        for item in std::fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().as_os_str().as_bytes().to_vec();
            if skip.iter().any(|s| rel == s.as_bytes()) {
                continue;
            }
            let md = std::fs::symlink_metadata(&path).unwrap();
            let mtime = (md.mtime(), md.mtime_nsec());
            let node = if md.file_type().is_symlink() {
                Node::Link { target: std::fs::read_link(&path).unwrap().as_os_str().as_bytes().to_vec(), mtime }
            } else if md.is_dir() {
                walk(root, &path, skip, out);
                Node::Dir { mode: md.mode() & 0o7777, mtime }
            } else if md.is_file() {
                let xattr = rustix::fs::getxattr(&path, "user.xdg.origin.url", &mut [0u8; 256][..]).ok().map(|n| {
                    let mut buf = [0u8; 256];
                    rustix::fs::getxattr(&path, "user.xdg.origin.url", &mut buf[..]).unwrap();
                    buf[..n].to_vec()
                });
                Node::File { data: std::fs::read(&path).unwrap(), mode: md.mode() & 0o7777, mtime, xattr }
            } else {
                continue; // FIFOs and the like are never stored
            };
            out.insert(rel, node);
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, skip, &mut out);
    out
}

fn options<'a>(time: &str, parent: Option<&'a xkyhold_core::snapshot::Snapshot>, disk: &Path) -> BackupOptions<'a> {
    let mut o = BackupOptions::new("test".into(), time.into());
    o.parent = parent;
    o.chunk_size = CHUNK;
    o.mount_points = Some(HashSet::from([disk.as_os_str().as_bytes().to_vec()]));
    o
}

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    out: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let out = tmp.path().join("restored");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&out).unwrap();
    Fixture { home, out, _tmp: tmp }
}

fn new_repo() -> Repository<MemoryDestination> {
    let options = Options { pack_size: 4096, index_every: 2 };
    Repository::create(MemoryDestination::default(), "pw", FAST, "2026-11-14T00:00:00Z".into(), options).unwrap().0
}

#[test]
fn backup_then_restore_gives_an_identical_tree() {
    let f = fixture();
    let xattrs = build_home(&f.home);
    let before = snapshot_of(&f.home, &[]);
    let mut repo = new_repo();
    let mut last = Progress::default();
    let snap = backup(&mut repo, &f.home, options("2026-11-14T10:00:00Z", None, &f.home.join("Disk")), &mut |p| {
        last = p.clone()
    })
    .unwrap();

    assert_eq!(snapshot_of(&f.home, &[]), before, "backup never changes the source");
    assert_eq!(snap.version(), 2);
    assert_eq!(snap.stats.special, Some(1), "the FIFO is counted, not stored");
    assert_eq!(snap.stats.symlinks, Some(3));
    assert_eq!(snap.stats.mounts, Some(1));
    assert_eq!(snap.stats.skipped, 0);

    let root = &snap.roots[0];
    let tree = repo.load_tree(&root.tree).unwrap();
    let names: Vec<String> = tree.iter().map(|e| e.display_name()).collect();
    assert!(!names.contains(&".cache".into()), "default exclusion");
    assert!(!names.contains(&"build".into()), "CACHEDIR.TAG folder");
    let disk = tree.iter().find(|e| e.name == b"Disk").unwrap();
    assert!(matches!(&disk.kind, EntryKind::Dir { mount: true, .. }));
    assert!(root.left_out.as_ref().unwrap().iter().any(|r| r.id.as_deref() == Some("other-file-systems")));

    let mut report = Default::default();
    restore(&repo, &tree, &f.out, &RestoreOptions::default(), &mut |r| report = r.clone()).unwrap();

    let mut expected = snapshot_of(
        &f.home,
        &[
            ".cache",
            ".cache/thumbs",
            ".cache/thumbs/a.png",
            "build",
            "build/CACHEDIR.TAG",
            "build/out",
            "build/out/a.o",
            "Disk/on-other-disk.txt",
        ],
    );
    // Setuid is never restored.
    if let Some(Node::File { mode, .. }) = expected.get_mut(&b"setuid-tool"[..]) {
        *mode &= 0o1777;
    }
    // The mount point comes back as an empty folder.
    let restored = snapshot_of(&f.out, &[]);
    if !xattrs {
        eprintln!("note: this file system has no user xattrs; not compared");
    }
    assert_eq!(restored.keys().collect::<Vec<_>>(), expected.keys().collect::<Vec<_>>());
    // Not vacuous: the cases that matter are really there.
    assert!(matches!(restored[&b"setuid-tool"[..]], Node::File { mode: 0o755, .. }));
    assert!(matches!(restored[&b"Documents/Work/plan.md"[..]], Node::File { mtime: (-86_400, 5), .. }));
    assert!(matches!(&restored[&b"outside"[..]], Node::Link { target, .. } if target == b"/etc/passwd"));
    assert!(restored.contains_key(&b"bad\xffname"[..]));
    if xattrs {
        assert!(
            matches!(&restored[&b"notes.txt"[..]], Node::File { xattr: Some(v), .. } if v == b"https://example.org")
        );
    }
    assert_eq!((report.files, report.symlinks, report.mounts), (8, 3, 1));
    for (path, node) in &expected {
        let got = &restored[path];
        match (node, got) {
            // The "Disk" mount point keeps its mode; its time changed when its
            // content was created in the source, which is not restored.
            (Node::Dir { mode, .. }, Node::Dir { mode: m, .. }) if path == b"Disk" => assert_eq!(mode, m),
            _ => assert_eq!(node, got, "{}", String::from_utf8_lossy(path)),
        }
    }
}

#[test]
fn an_unchanged_home_uploads_nothing_and_a_rewritten_file_is_noticed() {
    let f = fixture();
    build_home(&f.home);
    let mut repo = new_repo();
    let disk = f.home.join("Disk");
    let first = backup(&mut repo, &f.home, options("2026-11-14T10:00:00Z", None, &disk), &mut |_| {}).unwrap();
    let uploaded = repo.uploaded_bytes;

    let mut read = 0;
    let second =
        backup(&mut repo, &f.home, options("2026-11-14T11:00:00Z", Some(&first), &disk), &mut |p| read = p.files_read)
            .unwrap();
    assert_eq!(read, 0, "no file read again");
    assert_eq!(repo.uploaded_bytes, uploaded, "nothing uploaded");
    assert_eq!(second.roots[0].tree, first.roots[0].tree);
    assert_eq!(second.parent.as_deref(), Some(first.id.as_str()));

    // Same size, time set back: only ctime tells.
    let notes = f.home.join("notes.txt");
    std::fs::write(&notes, b"HELLO WORLD").unwrap();
    set_time(&notes, 1_794_400_000, 123_456_789);
    let third =
        backup(&mut repo, &f.home, options("2026-11-14T12:00:00Z", Some(&second), &disk), &mut |p| read = p.files_read)
            .unwrap();
    assert_eq!(read, 1);
    assert_ne!(third.roots[0].tree, second.roots[0].tree);
}

#[test]
fn selection_and_rules_limit_what_is_stored() {
    let f = fixture();
    build_home(&f.home);
    let mut repo = new_repo();
    let mut o = options("2026-11-14T10:00:00Z", None, &f.home.join("Disk"));
    o.selection = Some(vec![b"Documents".to_vec(), b".config/app/secret".to_vec()]);
    o.exclusions = Exclusions { paths: vec![b"Documents/Work".to_vec()], ..Exclusions::default() };
    let snap = backup(&mut repo, &f.home, o, &mut |_| {}).unwrap();
    restore(&repo, &repo.load_tree(&snap.roots[0].tree).unwrap(), &f.out, &RestoreOptions::default(), &mut |_| {})
        .unwrap();
    let got: Vec<String> = snapshot_of(&f.out, &[]).keys().map(|k| String::from_utf8_lossy(k).into_owned()).collect();
    assert_eq!(got, [".config", ".config/app", ".config/app/secret", "Documents", "Documents/big.bin"]);
}

#[test]
fn paths_outside_home_are_refused() {
    let f = fixture();
    build_home(&f.home);
    let mut repo = new_repo();
    for bad in [&b"../etc"[..], b"/etc", b"Documents/../../x"] {
        let mut o = options("t", None, &f.home.join("Disk"));
        o.selection = Some(vec![bad.to_vec()]);
        let err = backup(&mut repo, &f.home, o, &mut |_| {}).unwrap_err();
        assert_eq!(err.kind(), "outside-home", "{}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn restore_never_writes_through_a_link_in_the_target() {
    let f = fixture();
    build_home(&f.home);
    let mut repo = new_repo();
    let snap = backup(&mut repo, &f.home, options("t", None, &f.home.join("Disk")), &mut |_| {}).unwrap();
    let tree = repo.load_tree(&snap.roots[0].tree).unwrap();
    let notes: Vec<_> = tree.iter().filter(|e| e.name == b"notes.txt").cloned().collect();

    let victim = f.out.parent().unwrap().join("victim.txt");
    std::fs::write(&victim, b"untouched").unwrap();
    symlink(&victim, f.out.join("notes.txt")).unwrap();

    for conflict in [Conflict::Skip, Conflict::KeepBoth, Conflict::Replace] {
        restore(&repo, &notes, &f.out, &RestoreOptions { conflict, cancel: None }, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"untouched", "{conflict:?}");
    }
    assert_eq!(std::fs::read(f.out.join("notes.txt (restored)")).unwrap(), b"hello world");
    // Replace swapped the link itself for the file.
    assert!(std::fs::symlink_metadata(f.out.join("notes.txt")).unwrap().is_file());
}

#[test]
fn a_version_1_repository_is_read_only() {
    let golden: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{}/tests/fixtures/golden-v1-repo.json", env!("CARGO_MANIFEST_DIR"))).unwrap(),
    )
    .unwrap();
    let dest = MemoryDestination::default();
    for (key, value) in golden["objects"].as_object().unwrap() {
        let (dir, name) = key.split_once('/').unwrap();
        let data = xkyhold_core::bytes::from_base64(value.as_str().unwrap()).unwrap();
        dest.put(xkyhold_core::destination::Dir::parse(dir).unwrap(), name, &data).unwrap();
    }
    let password = Credential::Password(Zeroizing::new(golden["password"].as_str().unwrap().into()));
    let mut repo = Repository::open(dest, &password, Options::default()).unwrap();
    let f = fixture();
    let err = backup(&mut repo, &f.home, options("t", None, &f.home), &mut |_| {}).unwrap_err();
    assert_eq!(err.kind(), "read-only");
}
