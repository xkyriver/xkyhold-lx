//! Compatibility with repositories written by XkyHold 1.0 (format v1).
//!
//! The fixtures in `tests/fixtures/` are byte-for-byte copies of
//! `src/engine/golden-v1*.json` in the XkyHold repository. They were
//! generated once by the TypeScript engine and must never be regenerated:
//! if this test fails, the change under test would make existing backups
//! unreadable.

use std::collections::BTreeMap;

use serde_json::Value;
use xkyhold_core::bytes::{from_base64, sha256_hex};
use xkyhold_core::crypto::{Credential, RepoConfig, unlock};
use xkyhold_core::destination::{Destination, Dir, MemoryDestination};
use xkyhold_core::pack::{BlobEntry, BlobType, read_blob, read_pack_header};
use xkyhold_core::repo::{Options, Repository};
use xkyhold_core::tree::EntryKind;
use zeroize::Zeroizing;

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn credentials(golden: &Value) -> [(&'static str, Credential); 2] {
    [
        ("password", Credential::Password(Zeroizing::new(golden["password"].as_str().unwrap().into()))),
        ("recovery key", Credential::RecoveryKey(Zeroizing::new(golden["recoveryKey"].as_str().unwrap().into()))),
    ]
}

/// Deterministic pseudo-random bytes, same as `noise()` in the TS tests.
fn noise(length: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..length)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x & 255) as u8
        })
        .collect()
}

#[test]
fn opens_with_password_and_recovery_key_and_derives_the_same_blob_ids() {
    let golden = fixture("golden-v1.json");
    let config: RepoConfig = serde_json::from_value(golden["config"].clone()).unwrap();
    for (name, credential) in credentials(&golden) {
        let keys = unlock(&config, &credential).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(keys.blob_id(b"abc"), golden["blobIdOfAbc"].as_str().unwrap(), "{name}");
    }
}

#[test]
fn config_survives_a_round_trip_through_serde() {
    let golden = fixture("golden-v1.json");
    let config: RepoConfig = serde_json::from_value(golden["config"].clone()).unwrap();
    assert_eq!(serde_json::to_value(&config).unwrap(), golden["config"]);
}

#[test]
fn wrong_password_is_refused() {
    let golden = fixture("golden-v1.json");
    let config: RepoConfig = serde_json::from_value(golden["config"].clone()).unwrap();
    let wrong = Credential::Password(Zeroizing::new("correct horse battery stapler".into()));
    assert_eq!(unlock(&config, &wrong).err().map(|e| e.kind()), Some("wrong-key"));
}

#[test]
fn reads_the_stored_pack() {
    let golden = fixture("golden-v1.json");
    let config: RepoConfig = serde_json::from_value(golden["config"].clone()).unwrap();
    let keys = unlock(&config, &credentials(&golden)[0].1).unwrap();
    let pack = from_base64(golden["pack"]["base64"].as_str().unwrap()).unwrap();
    assert_eq!(sha256_hex(&pack), golden["pack"]["name"].as_str().unwrap());

    let blobs = read_pack_header(&keys, &pack).unwrap();
    let expected: Vec<BlobEntry> = serde_json::from_value(golden["pack"]["blobs"].clone()).unwrap();
    assert_eq!(blobs, expected);
    // Same JSON field order as the TS engine writes.
    assert_eq!(serde_json::to_value(&blobs).unwrap(), golden["pack"]["blobs"]);

    let read = |e: &BlobEntry| read_blob(&keys, e, &pack[e.offset as usize..(e.offset + e.length) as usize]).unwrap();
    assert_eq!(blobs[0].kind, BlobType::Data);
    assert_eq!(read(&blobs[0]), b"The quick brown fox jumps over the lazy dog");
    let tree: Value = serde_json::from_slice(&read(&blobs[1])).unwrap();
    assert_eq!(tree["entries"][0]["name"], "fox.txt");
}

#[test]
fn a_tampered_pack_is_refused() {
    let golden = fixture("golden-v1.json");
    let config: RepoConfig = serde_json::from_value(golden["config"].clone()).unwrap();
    let keys = unlock(&config, &credentials(&golden)[0].1).unwrap();
    let mut pack = from_base64(golden["pack"]["base64"].as_str().unwrap()).unwrap();
    let blobs = read_pack_header(&keys, &pack).unwrap();
    let e = &blobs[0];
    pack[e.offset as usize + 20] ^= 1;
    let err = read_blob(&keys, e, &pack[e.offset as usize..(e.offset + e.length) as usize]).unwrap_err();
    assert_eq!(err.kind(), "corrupt");
}

fn load_repo(golden: &Value) -> MemoryDestination {
    let dest = MemoryDestination::default();
    for (key, value) in golden["objects"].as_object().unwrap() {
        let (dir, name) = key.split_once('/').unwrap();
        dest.put(Dir::parse(dir).unwrap(), name, &from_base64(value.as_str().unwrap()).unwrap()).unwrap();
    }
    dest
}

/// Restores a tree into a flat map like the TS test's `MemoryTarget.flatten()`:
/// files map to their bytes, empty folders to `None` with a trailing slash.
fn flatten<D: Destination>(
    repo: &Repository<D>,
    tree: &str,
    prefix: &str,
    out: &mut BTreeMap<String, Option<Vec<u8>>>,
) {
    let entries = repo.load_tree(tree).unwrap();
    if entries.is_empty() {
        out.insert(prefix.to_owned(), None);
    }
    for entry in entries {
        let name = entry.display_name();
        match entry.kind {
            EntryKind::File { content, size } => {
                let data: Vec<u8> = content.iter().flat_map(|id| repo.load_blob(BlobType::Data, id).unwrap()).collect();
                assert_eq!(data.len() as u64, size);
                out.insert(format!("{prefix}{name}"), Some(data));
            }
            EntryKind::Dir { subtree, .. } => flatten(repo, &subtree, &format!("{prefix}{name}/"), out),
            other => panic!("unexpected entry {other:?}"),
        }
    }
}

#[test]
fn restores_every_file_of_the_golden_repository() {
    let golden = fixture("golden-v1-repo.json");
    for (name, credential) in credentials(&golden) {
        let repo = Repository::open(load_repo(&golden), &credential, Options::default())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let snapshots = repo.list_snapshots().unwrap();
        assert_eq!(snapshots.len(), 1);
        let snapshot = &snapshots[0];
        assert_eq!(snapshot.id, golden["snapshotId"].as_str().unwrap());
        assert_eq!(snapshot.time, "2026-11-11T15:00:00.000Z");
        assert_eq!(snapshot.device, "golden");
        assert_eq!(snapshot.parent, None);
        assert_eq!(snapshot.version(), 1);
        assert!(!repo.writable(), "version 1 repositories are read-only here");
        assert_eq!((snapshot.stats.files, snapshot.stats.bytes, snapshot.stats.skipped), (4, 3711, 0));

        let mut files = BTreeMap::new();
        flatten(&repo, &snapshot.roots[0].tree, "Documents/", &mut files);
        let expected = BTreeMap::from([
            ("Documents/notes.txt".to_owned(), Some(b"hello world".to_vec())),
            ("Documents/empty.txt".to_owned(), Some(Vec::new())),
            ("Documents/noise.bin".to_owned(), Some(noise(3000, 7))),
            ("Documents/Photos/a.jpg".to_owned(), Some(noise(700, 8))),
            ("Documents/Empty folder/".to_owned(), None),
        ]);
        assert_eq!(files, expected, "{name}");
    }
}
