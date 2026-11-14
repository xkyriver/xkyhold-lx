//! Check (FORMAT.md 10.5).

use std::collections::HashMap;

use crate::bytes::sha256_hex;
use crate::destination::{Destination, Dir};
use crate::prune::reachable;
use crate::repo::Repository;
use crate::{Error, Result};

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
    for id in &live {
        let Some((_, entry)) = repo.locate(id) else {
            report.missing_blobs += 1;
            continue;
        };
        if full {
            match repo.load_blob(entry.kind, id) {
                Ok(_) => {}
                Err(Error::MissingBlob(_)) => report.missing_blobs += 1,
                Err(Error::Authentication(_) | Error::Format(_)) => report.damaged_blobs += 1,
                Err(e) => return Err(e),
            }
        }
        report.blobs_checked += 1;
        on_progress(report.blobs_checked);
    }
    report.missing_packs.sort();
    report.damaged_packs.sort();
    Ok(report)
}
