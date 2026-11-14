//! Retention and prune (FORMAT.md 10.3). The caller holds the prune lock.
//!
//! Order of writes: new packs and their index, then one index that no
//! longer mentions the packs about to go, then the old index files, and
//! only then the packs. An interruption leaves extra files, never missing
//! ones; the next prune finishes the job.

use std::collections::{HashMap, HashSet};

use crate::destination::{Destination, Dir};
use crate::repo::Repository;
use crate::snapshot::Snapshot;
use crate::time::parse_iso;
use crate::tree::EntryKind;
use crate::{Error, Result};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PruneResult {
    pub packs_removed: usize,
    pub packs_rewritten: usize,
    pub bytes_freed: u64,
}

/// Every blob the snapshots need, trees and data alike. Stops on an
/// unreadable tree or an unknown entry type: nothing is removed on a guess
/// (FORMAT-v2.md 4.7).
pub fn reachable<D: Destination + 'static>(repo: &Repository<D>, snapshots: &[Snapshot]) -> Result<HashSet<String>> {
    let mut live = HashSet::new();
    let mut todo: Vec<String> = snapshots.iter().flat_map(|s| s.roots.iter().map(|r| r.tree.clone())).collect();
    while let Some(tree) = todo.pop() {
        if !live.insert(tree.clone()) {
            continue;
        }
        for entry in repo.load_tree(&tree)? {
            match entry.kind {
                EntryKind::Dir { subtree, .. } => todo.push(subtree),
                EntryKind::File { content, .. } => live.extend(content),
                EntryKind::Symlink { .. } => {}
                EntryKind::Unknown(kind) => {
                    return Err(Error::Format(format!("unknown entry type \"{kind}\"; nothing was removed")));
                }
            }
        }
    }
    Ok(live)
}

/// Frees the space of data no remaining snapshot needs. Packs where less
/// than `repack_below` of the bytes are still used are rewritten.
pub fn prune<D: Destination + 'static>(repo: &mut Repository<D>, repack_below: f64) -> Result<PruneResult> {
    if !repo.writable() {
        return Err(Error::ReadOnly);
    }
    // Another device may have backed up since this one last looked.
    repo.load_index()?;
    let live = reachable(repo, &repo.list_snapshots()?)?;
    let stored: HashMap<String, u64> =
        repo.dest.list(Dir::Packs, true)?.into_iter().map(|o| (o.name, o.size)).collect();
    let known = repo.packs();
    // With the lock held, a pack no index mentions is left over from a
    // backup that never finished.
    let mut remove: HashSet<String> = stored.keys().filter(|n| !known.contains_key(*n)).cloned().collect();
    let mut rewritten = 0;
    let mut names: Vec<&String> = known.keys().collect();
    names.sort();
    for name in names {
        if !stored.contains_key(name) {
            continue;
        }
        let blobs = &known[name];
        let used: Vec<_> = blobs.iter().filter(|b| live.contains(&b.id)).collect();
        if used.is_empty() {
            remove.insert(name.clone());
            continue;
        }
        let total: u64 = blobs.iter().map(|b| b.length).sum();
        let used_bytes: u64 = used.iter().map(|b| b.length).sum();
        if used_bytes as f64 / total as f64 >= repack_below {
            continue;
        }
        for blob in used {
            let data = repo.load_blob(blob.kind, &blob.id)?;
            repo.rewrite_blob(blob.kind, &blob.id, &data)?;
        }
        remove.insert(name.clone());
        rewritten += 1;
    }
    if remove.is_empty() {
        return Ok(PruneResult::default());
    }
    let result = (|| {
        repo.flush()?;
        repo.drop_packs(&remove);
        repo.consolidate_index()?;
        let mut freed = 0;
        let mut sorted: Vec<&String> = remove.iter().collect();
        sorted.sort();
        for name in sorted {
            repo.dest.trash(Dir::Packs, name)?;
            freed += stored.get(name).copied().unwrap_or(0);
        }
        Ok(PruneResult { packs_removed: remove.len(), packs_rewritten: rewritten, bytes_freed: freed })
    })();
    if result.is_err() {
        repo.abandon();
    }
    result
}

/// Retention: moves to the trash the snapshots older than `keep_since_ms`,
/// always keeping the most recent one. Returns how many were removed.
pub fn forget_older<D: Destination + 'static>(repo: &Repository<D>, keep_since_ms: i64) -> Result<usize> {
    let snapshots = repo.list_snapshots()?;
    let Some(newest) = snapshots.last().map(|s| s.id.clone()) else { return Ok(0) };
    let mut removed = 0;
    for s in &snapshots {
        if s.id != newest && parse_iso(&s.time).is_some_and(|t| t < keep_since_ms) {
            repo.trash_snapshot(&s.id)?;
            removed += 1;
        }
    }
    Ok(removed)
}
