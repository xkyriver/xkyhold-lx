//! The Drive destination against a fake Drive in memory: the whole engine
//! on top of it, and the failures the real one has.

use std::collections::HashSet;
use std::sync::Arc;

use xkyhold_core::backup::{BackupOptions, backup};
use xkyhold_core::check::check;
use xkyhold_core::crypto::{Argon2Params, Credential};
use xkyhold_core::destination::{Destination, Dir};
use xkyhold_core::prune::{forget_older, prune};
use xkyhold_core::repo::{Options, Repository};
use xkyhold_core::restore::{RestoreOptions, restore};
use xkyhold_core::time::parse_iso;
use xkyhold_dest_drive::fake::FakeDrive;
use xkyhold_dest_drive::{Client, Drive, DriveFolder, RefreshingTokens};
use zeroize::Zeroizing;

const FAST: Argon2Params = Argon2Params { memory_kib: 1024, iterations: 1, parallelism: 1 };

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

fn drive(fake: &Arc<FakeDrive>) -> Drive {
    let client = Client { id: "id".into(), secret: "secret".into() };
    let tokens = Arc::new(RefreshingTokens::new(fake.clone(), client, "refresh".into()));
    let mut d = Drive::new(fake.clone(), tokens);
    d.delays = vec![std::time::Duration::ZERO; 5];
    d
}

fn folder(fake: &Arc<FakeDrive>, name: &str) -> DriveFolder {
    DriveFolder::open(drive(fake), name).unwrap()
}

fn options(time: &str) -> BackupOptions<'static> {
    let mut o = BackupOptions::new("test".into(), time.into());
    o.mount_points = Some(HashSet::new());
    o
}

fn pw() -> Credential {
    Credential::Password(Zeroizing::new("pw".into()))
}

#[test]
fn the_engine_works_on_drive() {
    let fake = Arc::new(FakeDrive::default());
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("Docs")).unwrap();
    for i in 0..5 {
        std::fs::write(home.path().join(format!("Docs/f{i}")), noise(3_000_000, i)).unwrap();
    }
    std::fs::write(home.path().join("small"), b"hello").unwrap();
    let (mut repo, _) = Repository::create(
        folder(&fake, "XkyHold-lx test"),
        "pw",
        FAST,
        "2026-11-14T00:00:00.000Z".into(),
        Options::default(),
    )
    .unwrap();
    let first = backup(&mut repo, home.path(), options("2026-01-01T00:00:00.000Z"), &mut |_| {}).unwrap();
    std::fs::remove_file(home.path().join("Docs/f0")).unwrap();
    let mut o = options("2026-11-14T00:00:00.000Z");
    o.parent = Some(&first);
    backup(&mut repo, home.path(), o, &mut |_| {}).unwrap();

    // The layout XkyHold web expects, under XkyHold/<name>.
    {
        let s = fake.0.lock().unwrap();
        let repo_dir = s.find_path(&["XkyHold", "XkyHold-lx test"]).unwrap();
        let names: HashSet<String> = s.children(&repo_dir.id).iter().map(|f| f.name.clone()).collect();
        assert_eq!(names, HashSet::from(["config", "packs", "index", "snapshots"].map(String::from)));
    }

    // A fresh session (another run) restores everything.
    let repo2 = Repository::open(folder(&fake, "XkyHold-lx test"), &pw(), Options::default()).unwrap();
    let snaps = repo2.list_snapshots().unwrap();
    assert_eq!(snaps.len(), 2);
    let out = tempfile::tempdir().unwrap();
    restore(
        &repo2,
        &repo2.load_tree(&snaps[1].roots[0].tree).unwrap(),
        out.path(),
        &RestoreOptions::default(),
        &mut |_| {},
    )
    .unwrap();
    for i in 1..5 {
        assert_eq!(std::fs::read(out.path().join(format!("Docs/f{i}"))).unwrap(), noise(3_000_000, i));
    }
    assert!(!out.path().join("Docs/f0").exists());

    // Retention and prune: the old snapshot and its data go to the trash.
    let mut repo3 = Repository::open(folder(&fake, "XkyHold-lx test"), &pw(), Options::default()).unwrap();
    assert_eq!(forget_older(&repo3, parse_iso("2026-06-01T00:00:00.000Z").unwrap()).unwrap(), 1);
    let pruned = prune(&mut repo3, 0.5).unwrap();
    assert!(pruned.packs_removed > 0, "{pruned:?}");
    let report = check(&repo3, true, &mut |_| {}).unwrap();
    assert!(report.ok(), "{report:?}");
    // Nothing deleted for good: trashed files are still there, marked.
    assert!(fake.0.lock().unwrap().files.iter().any(|f| f.trashed));
}

#[test]
fn transient_errors_are_retried() {
    let fake = Arc::new(FakeDrive::default());
    let dest = folder(&fake, "XkyHold-lx test");
    fake.0.lock().unwrap().fail_next = 3;
    dest.put(Dir::Packs, "abc", b"data").unwrap();
    assert_eq!(dest.get(Dir::Packs, "abc", Some((1, 2))).unwrap(), b"at");
}

#[test]
fn an_expired_access_token_is_renewed_mid_backup() {
    let fake = Arc::new(FakeDrive::default());
    let home = tempfile::tempdir().unwrap();
    for i in 0..4 {
        std::fs::write(home.path().join(format!("f{i}")), noise(200_000, i)).unwrap();
    }
    let (mut repo, _) = Repository::create(
        folder(&fake, "XkyHold-lx test"),
        "pw",
        FAST,
        "2026-11-14T00:00:00.000Z".into(),
        Options { pack_size: 100_000, uploads: 1, ..Options::default() },
    )
    .unwrap();
    let tokens_before = fake.0.lock().unwrap().token_count;
    let mut expired = false;
    let fake2 = fake.clone();
    backup(&mut repo, home.path(), options("2026-11-14T01:00:00.000Z"), &mut |p| {
        // Google revokes the access token an hour in.
        if p.files_read == 2 && !expired {
            fake2.0.lock().unwrap().valid_token = "expired-elsewhere".into();
            expired = true;
        }
    })
    .unwrap();
    assert!(expired);
    assert!(fake.0.lock().unwrap().token_count > tokens_before, "a new access token was fetched");
    assert!(check(&repo, true, &mut |_| {}).unwrap().ok());
}

#[test]
fn a_revoked_sign_in_says_so() {
    let fake = Arc::new(FakeDrive::default());
    let dest = folder(&fake, "XkyHold-lx test");
    {
        let mut s = fake.0.lock().unwrap();
        s.valid_token = "gone".into();
        s.refresh_ok = false;
    }
    let err = dest.put(Dir::Packs, "abc", b"x").unwrap_err();
    assert_eq!(err.kind(), "session");
}

#[test]
fn an_interrupted_upload_resumes_where_the_server_stopped() {
    let fake = Arc::new(FakeDrive::default());
    let dest = folder(&fake, "XkyHold-lx test");
    let data = noise(20 * 1024 * 1024, 5);
    fake.0.lock().unwrap().drop_next_chunk = true;
    dest.put(Dir::Packs, "big", &data).unwrap();
    assert_eq!(dest.get(Dir::Packs, "big", None).unwrap(), data);
    let listed = dest.list(Dir::Packs, true).unwrap();
    assert_eq!(listed[0].sha256.as_deref(), Some(xkyhold_core::bytes::sha256_hex(&data).as_str()));
}

#[test]
fn listings_page_and_repositories_are_found() {
    let fake = Arc::new(FakeDrive::default());
    let dest = folder(&fake, "XkyHold-lx test one");
    for i in 0..7 {
        dest.put(Dir::Index, &format!("i{i}"), b"x").unwrap();
    }
    let again = folder(&fake, "XkyHold-lx test one");
    assert_eq!(again.list(Dir::Index, true).unwrap().len(), 7, "three pages of three");
    folder(&fake, "XkyHold-lx test two");
    assert_eq!(DriveFolder::list_repositories(&drive(&fake)).unwrap(), ["XkyHold-lx test one", "XkyHold-lx test two"]);
}

#[test]
fn uploads_keep_under_the_limit() {
    let fake = Arc::new(FakeDrive::default());
    let mut d = drive(&fake);
    d.limit_uploads(2_000_000);
    let dest = DriveFolder::open(d, "XkyHold-lx test").unwrap();
    let started = std::time::Instant::now();
    dest.put(Dir::Packs, "a", &noise(1_000_000, 1)).unwrap();
    dest.put(Dir::Packs, "b", &noise(1_000_000, 2)).unwrap();
    assert!(started.elapsed().as_millis() >= 950, "{:?}", started.elapsed());
}
