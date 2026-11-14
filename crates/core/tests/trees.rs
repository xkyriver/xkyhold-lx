//! Reading the trees of a backup: whole tree packs, not one request per
//! folder. Found in the real proof on Google Drive, where a quick check of a
//! 28 000-folder backup did not finish in ten minutes.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use xkyhold_core::Result;
use xkyhold_core::backup::{BackupOptions, backup};
use xkyhold_core::check::check;
use xkyhold_core::crypto::{Argon2Params, Credential};
use xkyhold_core::destination::{Destination, Dir, MemoryDestination, StoredObject};
use xkyhold_core::repo::{Options, Repository};
use xkyhold_core::restore::{RestoreOptions, restore};
use zeroize::Zeroizing;

const FAST: Argon2Params = Argon2Params { memory_kib: 1024, iterations: 1, parallelism: 1 };

/// Counts the pack reads, whole and by range.
#[derive(Default)]
struct Counting {
    inner: MemoryDestination,
    whole: AtomicUsize,
    ranged: AtomicUsize,
}

impl Destination for Counting {
    fn put(&self, dir: Dir, name: &str, data: &[u8]) -> Result<()> {
        self.inner.put(dir, name, data)
    }
    fn get(&self, dir: Dir, name: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>> {
        if dir == Dir::Packs {
            let counter = if range.is_some() { &self.ranged } else { &self.whole };
            counter.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.get(dir, name, range)
    }
    fn list(&self, dir: Dir, fresh: bool) -> Result<Vec<StoredObject>> {
        self.inner.list(dir, fresh)
    }
    fn trash(&self, dir: Dir, name: &str) -> Result<()> {
        self.inner.trash(dir, name)
    }
}

fn options(time: &str) -> BackupOptions<'static> {
    let mut o = BackupOptions::new("test".into(), time.into());
    o.mount_points = Some(HashSet::new());
    o
}

#[test]
fn trees_come_in_whole_packs_and_from_the_cache() {
    let home = tempfile::tempdir().unwrap();
    for i in 0..200 {
        let dir = home.path().join(format!("d{i:03}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("f"), format!("file {i}")).unwrap();
    }
    let cache = tempfile::tempdir().unwrap();
    let opts = || Options { cache: Some(cache.path().to_path_buf()), ..Options::default() };
    let dest = Arc::new(Counting::default());
    let (mut repo, _) = Repository::create(dest.clone(), "pw", FAST, "t".into(), opts()).unwrap();
    backup(&mut repo, home.path(), options("2026-11-14T10:00:00.000Z"), &mut |_| {}).unwrap();

    // A fresh session reads all 201 trees (check walks every one).
    let credential = Credential::Password(Zeroizing::new("pw".into()));
    let repo = Repository::open(dest.clone(), &credential, opts()).unwrap();
    dest.whole.store(0, Ordering::SeqCst);
    dest.ranged.store(0, Ordering::SeqCst);
    assert!(check(&repo, false, None, &mut |_| {}).unwrap().ok());
    let whole = dest.whole.load(Ordering::SeqCst);
    assert!((1..=3).contains(&whole), "{whole} whole pack reads");
    assert_eq!(dest.ranged.load(Ordering::SeqCst), 0, "no tree read one by one");

    // Another session: the trees come from the local cache.
    let repo = Repository::open(dest.clone(), &credential, opts()).unwrap();
    dest.whole.store(0, Ordering::SeqCst);
    let snap = repo.list_snapshots().unwrap().pop().unwrap();
    let out = tempfile::tempdir().unwrap();
    let entries = repo.load_tree(&snap.roots[0].tree).unwrap();
    let report = restore(&repo, &entries, out.path(), &RestoreOptions::default(), &mut |_| {}).unwrap();
    assert_eq!(report.files, 200);
    assert_eq!(dest.whole.load(Ordering::SeqCst), 0, "tree packs came from the cache");
    // File contents are still read by range.
    assert!(dest.ranged.load(Ordering::SeqCst) > 0);
    assert_eq!(std::fs::read(out.path().join("d007/f")).unwrap(), b"file 7");

    // A damaged cached pack is ignored and read again.
    for item in std::fs::read_dir(cache.path().join(&repo.config.repo_id).join("trees")).unwrap() {
        std::fs::write(item.unwrap().path(), b"garbage").unwrap();
    }
    let repo = Repository::open(dest.clone(), &credential, opts()).unwrap();
    dest.whole.store(0, Ordering::SeqCst);
    assert!(check(&repo, false, None, &mut |_| {}).unwrap().ok());
    assert!(dest.whole.load(Ordering::SeqCst) >= 1);
}
