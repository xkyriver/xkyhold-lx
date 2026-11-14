//! Prune with a failure injected at every single write and trash in turn
//! (FORMAT.md 10.3): after each failure the repository still restores
//! every remaining snapshot, and the next prune finishes the job.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use xkyhold_core::backup::{BackupOptions, backup};
use xkyhold_core::check::check;
use xkyhold_core::crypto::{Argon2Params, Credential};
use xkyhold_core::destination::{Destination, Dir, MemoryDestination, StoredObject};
use xkyhold_core::lock::{Lock, LockKind};
use xkyhold_core::prune::prune;
use xkyhold_core::repo::{Options, Repository};
use xkyhold_core::restore::{RestoreOptions, restore};
use xkyhold_core::{Error, Result};
use zeroize::Zeroizing;

const FAST: Argon2Params = Argon2Params { memory_kib: 1024, iterations: 1, parallelism: 1 };

/// Fails the n-th write or trash (1-based) once armed; 0 means never.
#[derive(Default)]
struct Flaky {
    inner: MemoryDestination,
    fail_at: AtomicUsize,
    count: AtomicUsize,
}

impl Flaky {
    fn arm(&self, n: usize) {
        self.count.store(0, Ordering::SeqCst);
        self.fail_at.store(n, Ordering::SeqCst);
    }

    fn tick(&self) -> Result<()> {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.fail_at.load(Ordering::SeqCst) {
            return Err(Error::Io(std::io::Error::other("injected failure")));
        }
        Ok(())
    }
}

impl Destination for Flaky {
    fn put(&self, dir: Dir, name: &str, data: &[u8]) -> Result<()> {
        self.tick()?;
        self.inner.put(dir, name, data)
    }
    fn get(&self, dir: Dir, name: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>> {
        self.inner.get(dir, name, range)
    }
    fn list(&self, dir: Dir, fresh: bool) -> Result<Vec<StoredObject>> {
        self.inner.list(dir, fresh)
    }
    fn trash(&self, dir: Dir, name: &str) -> Result<()> {
        self.tick()?;
        self.inner.trash(dir, name)
    }
}

fn options(time: &str) -> BackupOptions<'static> {
    let mut o = BackupOptions::new("test".into(), time.into());
    o.chunk_size = 700;
    o.mount_points = Some(HashSet::new());
    o
}

fn credential() -> Credential {
    Credential::Password(Zeroizing::new("pw".into()))
}

fn small() -> Options {
    Options { pack_size: 2048, index_every: 2, ..Options::default() }
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

/// Every remaining snapshot restores, and a full check passes.
fn assert_restorable(dest: &Arc<Flaky>) {
    let repo = Repository::open(dest.clone(), &credential(), small()).unwrap();
    let report = check(&repo, true, None, &mut |_| {}).unwrap();
    assert!(report.ok(), "{report:?}");
    for snap in repo.list_snapshots().unwrap() {
        let out = tempfile::tempdir().unwrap();
        restore(
            &repo,
            &repo.load_tree(&snap.roots[0].tree).unwrap(),
            out.path(),
            &RestoreOptions::default(),
            &mut |_| {},
        )
        .unwrap();
    }
}

#[test]
fn prune_survives_a_failure_at_every_step() {
    let home = tempfile::tempdir().unwrap();
    for i in 0..6 {
        std::fs::write(home.path().join(format!("f{i}")), noise(1500, i)).unwrap();
    }
    let dest = Arc::new(Flaky::default());
    let (mut repo, _) =
        Repository::create(dest.clone(), "pw", FAST, "2026-11-14T00:00:00.000Z".into(), small()).unwrap();
    let first = backup(&mut repo, home.path(), options("2026-01-01T00:00:00.000Z"), &mut |_| {}).unwrap();
    for i in 0..4 {
        std::fs::remove_file(home.path().join(format!("f{i}"))).unwrap();
    }
    std::fs::write(home.path().join("new"), noise(3000, 99)).unwrap();
    let mut o = options("2026-11-14T00:00:00.000Z");
    o.parent = Some(&first);
    backup(&mut repo, home.path(), o, &mut |_| {}).unwrap();
    repo.trash_snapshot(&first.id).unwrap();
    let packs_before = dest.list(Dir::Packs, true).unwrap().len();

    let mut failures = 0;
    for step in 1.. {
        dest.arm(step);
        let mut repo = Repository::open(dest.clone(), &credential(), small()).unwrap();
        let outcome = prune(&mut repo, 0.5);
        dest.arm(0);
        assert_restorable(&dest);
        match outcome {
            Err(_) => failures += 1,
            Ok(_) if dest.count.load(Ordering::SeqCst) < step => break, // no fault hit: finished
            Ok(_) => {}
        }
    }
    assert!(failures >= 3, "the injected failures reached several steps ({failures})");

    // Nothing left to do, and the dead data is gone.
    let mut repo = Repository::open(dest.clone(), &credential(), small()).unwrap();
    assert_eq!(prune(&mut repo, 0.5).unwrap().packs_removed, 0);
    assert_eq!(repo.dest.list(Dir::Index, true).unwrap().len(), 1, "one consolidated index");
    let packs_after = dest.list(Dir::Packs, true).unwrap().len();
    assert!(packs_after < packs_before, "{packs_after} < {packs_before}");
}

#[test]
fn prune_gives_way_to_a_running_backup() {
    let dest: Arc<dyn Destination> = Arc::new(MemoryDestination::default());
    let _backup = Lock::acquire_at(dest.clone(), LockKind::Backup, "other", xkyhold_core::time::now_ms, None).unwrap();
    let err = Lock::acquire_at(dest, LockKind::Prune, "me", xkyhold_core::time::now_ms, None).err().unwrap();
    assert_eq!(err.kind(), "busy");
}

#[test]
fn a_failed_upload_fails_the_backup_and_the_next_one_completes() {
    let home = tempfile::tempdir().unwrap();
    for i in 0..8 {
        std::fs::write(home.path().join(format!("f{i}")), noise(1500, i + 10)).unwrap();
    }
    for step in [1, 3, 5] {
        let dest = Arc::new(Flaky::default());
        let (mut repo, _) =
            Repository::create(dest.clone(), "pw", FAST, "2026-11-14T00:00:00.000Z".into(), small()).unwrap();
        dest.arm(step);
        assert!(backup(&mut repo, home.path(), options("2026-11-14T01:00:00.000Z"), &mut |_| {}).is_err());
        dest.arm(0);
        let snap = backup(&mut repo, home.path(), options("2026-11-14T02:00:00.000Z"), &mut |_| {}).unwrap();
        assert_eq!(snap.stats.files, 8);
        assert_restorable(&dest);
    }
}

#[test]
fn the_index_cache_saves_downloads_and_ignores_a_bad_copy() {
    #[derive(Default)]
    struct Counting {
        inner: MemoryDestination,
        index_gets: AtomicUsize,
    }
    impl Destination for Counting {
        fn put(&self, d: Dir, n: &str, data: &[u8]) -> Result<()> {
            self.inner.put(d, n, data)
        }
        fn get(&self, d: Dir, n: &str, r: Option<(u64, u64)>) -> Result<Vec<u8>> {
            if d == Dir::Index {
                self.index_gets.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.get(d, n, r)
        }
        fn list(&self, d: Dir, f: bool) -> Result<Vec<StoredObject>> {
            self.inner.list(d, f)
        }
        fn trash(&self, d: Dir, n: &str) -> Result<()> {
            self.inner.trash(d, n)
        }
    }
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("a"), noise(5000, 1)).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let with_cache = || Options { cache: Some(cache.path().to_path_buf()), ..small() };
    let dest = Arc::new(Counting::default());
    let (mut repo, _) =
        Repository::create(dest.clone(), "pw", FAST, "2026-11-14T00:00:00.000Z".into(), with_cache()).unwrap();
    backup(&mut repo, home.path(), options("2026-11-14T01:00:00.000Z"), &mut |_| {}).unwrap();
    let files = dest.list(Dir::Index, true).unwrap().len();
    assert!(files > 0);

    Repository::open(dest.clone(), &credential(), with_cache()).unwrap();
    assert_eq!(dest.index_gets.load(Ordering::SeqCst), files, "first open downloads every index file");
    Repository::open(dest.clone(), &credential(), with_cache()).unwrap();
    assert_eq!(dest.index_gets.load(Ordering::SeqCst), files, "second open reads the cache");

    // A damaged cached copy is noticed by its name and fetched again.
    let id = repo.config.repo_id.clone();
    let one = std::fs::read_dir(cache.path().join(&id).join("index")).unwrap().next().unwrap().unwrap().path();
    std::fs::write(&one, b"garbage").unwrap();
    Repository::open(dest.clone(), &credential(), with_cache()).unwrap();
    assert_eq!(dest.index_gets.load(Ordering::SeqCst), files + 1);
}

/// A backup stopped halfway keeps what it already uploaded: the next
/// backup sends only the rest, and the repository stays sound.
#[test]
fn a_stopped_backup_is_continued_by_the_next_one() {
    use std::sync::atomic::AtomicBool;
    let home = tempfile::tempdir().unwrap();
    for i in 0..20 {
        std::fs::write(home.path().join(format!("f{i:02}")), noise(3000, i)).unwrap();
    }
    let dest = Arc::new(Flaky::default());
    let one_upload = || Options { uploads: 1, ..small() };
    let (mut repo, _) = Repository::create(dest.clone(), "pw", FAST, "t".into(), one_upload()).unwrap();
    let stop = AtomicBool::new(false);
    let mut first = options("2026-01-01T00:00:00.000Z");
    first.cancel = Some(&stop);
    let mut seen = 0;
    let stopped = backup(&mut repo, home.path(), first, &mut |p| {
        seen = p.files;
        if p.files >= 10 {
            stop.store(true, Ordering::SeqCst);
        }
    });
    assert!(matches!(stopped, Err(Error::Aborted)), "{stopped:?}");
    assert!(seen >= 10);
    let sent_before = repo.uploaded_bytes;
    assert!(sent_before > 0);

    // A new session, as after closing the window.
    let mut repo = Repository::open(dest.clone(), &credential(), one_upload()).unwrap();
    let snap = backup(&mut repo, home.path(), options("2026-01-01T00:01:00.000Z"), &mut |_| {}).unwrap();
    assert_eq!(snap.stats.files, 20);
    let total: u64 = 20 * 3000;
    assert!(
        repo.uploaded_bytes < total - sent_before / 2,
        "sent {} again after {} were already uploaded",
        repo.uploaded_bytes,
        sent_before
    );
    assert_restorable(&dest);
}
