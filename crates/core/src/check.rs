//! Check (FORMAT.md 10.5).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

use crate::bytes::sha256_hex;
use crate::destination::{Destination, Dir};
use crate::prune::reachable;
use crate::repo::Repository;
use crate::{Error, Result};

/// How many blobs a full check downloads at the same time.
const CHECK_THREADS: usize = 6;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CheckReport {
    pub packs: usize,
    /// Packs named in the index that are not in the destination.
    pub missing_packs: Vec<String>,
    /// Packs whose content does not match their name.
    pub damaged_packs: Vec<String>,
    /// Blobs a snapshot needs that the index does not know.
    pub missing_blobs: usize,
    /// Blobs that failed to decrypt or verify (full check only).
    pub damaged_blobs: usize,
    pub blobs_checked: usize,
}

impl CheckReport {
    pub fn ok(&self) -> bool {
        self.missing_packs.is_empty()
            && self.damaged_packs.is_empty()
            && self.missing_blobs == 0
            && self.damaged_blobs == 0
    }
}

/// Quick check: every pack named in the index exists, and its checksum
/// (from the destination, or computed when it has none and `full` is set)
/// matches its name; every blob a snapshot needs is in the index. With
/// `full`, every needed blob is also downloaded and verified.
pub fn check<D: Destination + 'static>(
    repo: &Repository<D>,
    full: bool,
    cancel: Option<&AtomicBool>,
    on_progress: &mut dyn FnMut(usize),
) -> Result<CheckReport> {
    let mut report = CheckReport::default();
    let stored: HashMap<String, Option<String>> =
        repo.dest.list(Dir::Packs, true)?.into_iter().map(|o| (o.name, o.sha256)).collect();
    let packs = repo.packs();
    report.packs = packs.len();
    for name in packs.keys() {
        match stored.get(name) {
            None => report.missing_packs.push(name.clone()),
            Some(Some(sum)) if sum != name => report.damaged_packs.push(name.clone()),
            Some(None) if full => {
                if sha256_hex(&repo.dest.get(Dir::Packs, name, None)?) != *name {
                    report.damaged_packs.push(name.clone());
                }
            }
            Some(_) => {}
        }
    }
    let live = match reachable(repo, &repo.list_snapshots()?) {
        Ok(live) => live,
        Err(Error::MissingBlob(_) | Error::Authentication(_)) => {
            report.missing_blobs += 1;
            return Ok(report);
        }
        Err(e) => return Err(e),
    };
    let ids: Vec<&String> = live.iter().collect();
    let mut located = Vec::new();
    for id in ids {
        match repo.locate(id) {
            Some((_, entry)) => located.push((id.as_str(), entry.kind)),
            None => report.missing_blobs += 1,
        }
    }
    if !full {
        report.blobs_checked = located.len();
        on_progress(report.blobs_checked);
    } else {
        // Download and verify several blobs at a time.
        let next = AtomicUsize::new(0);
        let (tx, rx) = mpsc::channel::<Result<()>>();
        let mut fatal = None;
        std::thread::scope(|scope| {
            for _ in 0..CHECK_THREADS {
                let (tx, next, located) = (tx.clone(), &next, &located);
                scope.spawn(move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        let Some((id, kind)) = located.get(i) else { break };
                        if tx.send(repo.load_blob(*kind, id).map(|_| ())).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(tx);
            for outcome in rx {
                match outcome {
                    Ok(()) => {}
                    Err(Error::MissingBlob(_)) => report.missing_blobs += 1,
                    Err(Error::Authentication(_) | Error::Format(_)) => report.damaged_blobs += 1,
                    Err(e) => {
                        // Stop the others: the network or the destination failed.
                        next.store(usize::MAX / 2, Ordering::SeqCst);
                        fatal.get_or_insert(e);
                    }
                }
                report.blobs_checked += 1;
                on_progress(report.blobs_checked);
                if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    next.store(usize::MAX / 2, Ordering::SeqCst);
                    fatal.get_or_insert(Error::Aborted);
                }
            }
        });
        if let Some(e) = fatal {
            return Err(e);
        }
    }
    report.missing_packs.sort();
    report.damaged_packs.sort();
    Ok(report)
}
