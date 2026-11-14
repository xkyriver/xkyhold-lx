//! Advisory locks (FORMAT.md 10.4). Drive has no atomic "create if
//! absent", so each side writes its lock first and looks second.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::crypto::random_id;
use crate::destination::{Destination, Dir};
use crate::time::{iso, now_ms, parse_iso};
use crate::{Error, Result};

pub const STALE_AFTER_MS: i64 = 30 * 60_000;
pub const REFRESH_EVERY_MS: u64 = 5 * 60_000;
const REMOVE_AFTER_MS: i64 = 24 * 60 * 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockKind {
    Backup,
    Prune,
}

#[derive(Serialize, Deserialize)]
struct LockFile {
    kind: LockKind,
    device: String,
    time: String,
}

pub struct Lock {
    dest: Arc<dyn Destination>,
    name: Arc<Mutex<String>>,
    stop: Option<(mpsc::Sender<()>, JoinHandle<()>)>,
    released: AtomicBool,
}

fn write(dest: &dyn Destination, kind: LockKind, device: &str, now: i64) -> Result<String> {
    let name = random_id();
    let file = LockFile { kind, device: device.to_owned(), time: iso(now) };
    dest.put(Dir::Locks, &name, &serde_json::to_vec(&file).expect("serializable"))?;
    Ok(name)
}

impl Lock {
    /// Takes a lock or fails with [`Error::Busy`]. A backup gives way to a
    /// running prune; a prune gives way to anything running.
    pub fn acquire(dest: Arc<dyn Destination>, kind: LockKind, device: &str) -> Result<Self> {
        Self::acquire_at(dest, kind, device, now_ms, Some(Duration::from_millis(REFRESH_EVERY_MS)))
    }

    pub fn acquire_at(
        dest: Arc<dyn Destination>,
        kind: LockKind,
        device: &str,
        now: fn() -> i64,
        refresh: Option<Duration>,
    ) -> Result<Self> {
        let name = write(&*dest, kind, device, now())?;
        let mut lock = Lock {
            dest: dest.clone(),
            name: Arc::new(Mutex::new(name.clone())),
            stop: None,
            released: AtomicBool::new(false),
        };
        let check = (|| {
            for other in dest.list(Dir::Locks, true)? {
                if other.name == name {
                    continue;
                }
                let Ok(bytes) = dest.get(Dir::Locks, &other.name, None) else { continue };
                let Ok(theirs) = serde_json::from_slice::<LockFile>(&bytes) else { continue };
                let age = parse_iso(&theirs.time).map(|t| now() - t).unwrap_or(i64::MAX);
                if age >= STALE_AFTER_MS {
                    if age > REMOVE_AFTER_MS {
                        let _ = dest.trash(Dir::Locks, &other.name);
                    }
                    continue;
                }
                if kind == LockKind::Prune || theirs.kind == LockKind::Prune {
                    return Err(Error::Busy);
                }
            }
            Ok(())
        })();
        if let Err(err) = check {
            lock.release();
            return Err(err);
        }
        if let Some(every) = refresh {
            let (tx, rx) = mpsc::channel::<()>();
            let (dest, current, device) = (dest.clone(), lock.name.clone(), device.to_owned());
            let handle = std::thread::spawn(move || {
                while let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(every) {
                    // Objects are write-once: a new lock, then drop the old.
                    if let Ok(fresh) = write(&*dest, kind, &device, now()) {
                        let previous = std::mem::replace(&mut *current.lock().unwrap(), fresh);
                        let _ = dest.trash(Dir::Locks, &previous);
                    }
                }
            });
            lock.stop = Some((tx, handle));
        }
        Ok(lock)
    }

    pub fn release(&mut self) {
        if let Some((tx, handle)) = self.stop.take() {
            drop(tx);
            let _ = handle.join();
        }
        if !self.released.swap(true, Ordering::SeqCst) {
            let _ = self.dest.trash(Dir::Locks, &self.name.lock().unwrap());
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::MemoryDestination;

    #[test]
    fn backups_share_and_prune_waits() {
        let dest: Arc<dyn Destination> = Arc::new(MemoryDestination::default());
        let a = Lock::acquire_at(dest.clone(), LockKind::Backup, "a", now_ms, None).unwrap();
        let b = Lock::acquire_at(dest.clone(), LockKind::Backup, "b", now_ms, None).unwrap();
        assert_eq!(
            Lock::acquire_at(dest.clone(), LockKind::Prune, "c", now_ms, None).err().map(|e| e.kind()),
            Some("busy")
        );
        assert_eq!(dest.list(Dir::Locks, true).unwrap().len(), 2, "the refused lock is gone");
        drop((a, b));
        let mut p = Lock::acquire_at(dest.clone(), LockKind::Prune, "c", now_ms, None).unwrap();
        assert_eq!(
            Lock::acquire_at(dest.clone(), LockKind::Backup, "a", now_ms, None).err().map(|e| e.kind()),
            Some("busy")
        );
        p.release();
        assert!(dest.list(Dir::Locks, true).unwrap().is_empty());
    }

    #[test]
    fn stale_locks_are_ignored_and_old_ones_removed() {
        let dest: Arc<dyn Destination> = Arc::new(MemoryDestination::default());
        fn long_ago() -> i64 {
            now_ms() - 2 * REMOVE_AFTER_MS
        }
        let old = Lock::acquire_at(dest.clone(), LockKind::Prune, "dead", long_ago, None).unwrap();
        std::mem::forget(old); // a device that died while pruning
        let _ok = Lock::acquire_at(dest.clone(), LockKind::Backup, "a", now_ms, None).unwrap();
        assert_eq!(dest.list(Dir::Locks, true).unwrap().len(), 1, "the dead lock was trashed");
    }
}
