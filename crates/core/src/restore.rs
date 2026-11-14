//! Restore (FORMAT.md 10.2, FORMAT-v2.md section 6).
//!
//! Everything is created relative to an open folder, refusing to follow
//! symbolic links, so a restore never writes outside its target folder.
//! Files are written under a temporary name and renamed into place when
//! complete; metadata is applied last, folders after their contents.
//! The owner is never changed, setuid and setgid are never restored, and a
//! group is applied only when the restoring user belongs to it.

use std::ffi::CString;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

use rustix::fs::{
    AtFlags, FileType, Gid, Mode, OFlags, Timespec, Timestamps, XattrFlags, chownat, fchmod, fsetxattr, futimens,
    mkdirat, openat, renameat, statat, symlinkat, unlinkat, utimensat,
};

use crate::bytes::from_base64;
use crate::crypto::random_id;
use crate::destination::Destination;
use crate::pack::BlobType;
use crate::repo::Repository;
use crate::tree::{Entry, EntryKind, Meta, valid_name};
use crate::{Error, Result};

/// What to do when an item already exists in the target folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    Skip,
    Replace,
    /// Restore next to it, as "name (restored)".
    KeepBoth,
}

pub struct RestoreOptions<'a> {
    pub conflict: Conflict,
    pub cancel: Option<&'a AtomicBool>,
}

impl Default for RestoreOptions<'_> {
    fn default() -> Self {
        Self { conflict: Conflict::KeepBoth, cancel: None }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub files: u64,
    pub bytes: u64,
    pub folders: u64,
    pub symlinks: u64,
    /// Mount points recreated as empty folders.
    pub mounts: u64,
    /// Entries with a name that cannot be created safely.
    pub bad_names: u64,
    /// Entries of a type this version does not know.
    pub unknown: u64,
    /// Items left alone because they already existed.
    pub existing: u64,
    /// Extended attributes the target could not take.
    pub xattr_failures: u64,
    pub current: String,
}

fn cname(name: &[u8]) -> CString {
    CString::new(name).expect("validated names have no NUL")
}

fn errno(e: rustix::io::Errno) -> Error {
    Error::Io(e.into())
}

fn timestamps(meta: &Meta) -> Option<Timestamps> {
    let ns = meta.time_ns()?;
    let ts = Timespec {
        tv_sec: i64::try_from(ns.div_euclid(1_000_000_000)).ok()?,
        tv_nsec: ns.rem_euclid(1_000_000_000) as _,
    };
    Some(Timestamps { last_access: ts, last_modification: ts })
}

/// How many files of one folder are restored at the same time.
const FILE_THREADS: usize = 6;

/// Applies permissions, group, extended attributes and times. Returns how
/// many extended attributes the target refused.
fn apply_meta(fd: &OwnedFd, meta: &Meta, default_mode: u32, groups: &[u32]) -> Result<u64> {
    let mode = meta.mode.unwrap_or(default_mode) & 0o1777;
    fchmod(fd, Mode::from_raw_mode(mode)).map_err(errno)?;
    if let Some(gid) = group_to_apply(meta, groups) {
        // Owner stays; only the group changes.
        let _ = rustix::fs::fchown(fd, None, Some(Gid::from_raw(gid)));
    }
    let mut failures = 0;
    for attr in meta.xattrs.iter().flatten() {
        let ok = from_base64(&attr.value)
            .ok()
            .is_some_and(|value| fsetxattr(fd, attr.name.as_str(), &value, XattrFlags::empty()).is_ok());
        if !ok {
            failures += 1;
        }
    }
    if let Some(times) = timestamps(meta) {
        futimens(fd, &times).map_err(errno)?;
    }
    Ok(failures)
}

/// Writes one file under a temporary name, applies its metadata and
/// renames it into place. Returns the extended attributes refused.
#[allow(clippy::too_many_arguments)]
fn write_file<D: Destination + 'static>(
    repo: &Repository<D>,
    dir: &OwnedFd,
    name: &[u8],
    entry: &Entry,
    content: &[String],
    groups: &[u32],
    cancelled: &dyn Fn() -> Result<()>,
    sent: &dyn Fn(u64),
) -> Result<u64> {
    let temp = format!(".xkyhold-{}.tmp", random_id()).into_bytes();
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = openat(dir, cname(&temp).as_c_str(), flags, Mode::from_raw_mode(0o600)).map_err(errno)?;
    let written = (|| {
        let mut file = std::fs::File::from(fd.try_clone()?);
        for id in content {
            cancelled()?;
            let chunk = repo.load_blob(BlobType::Data, id)?;
            file.write_all(&chunk)?;
            sent(chunk.len() as u64);
        }
        let failures = apply_meta(&fd, &entry.meta, 0o644, groups)?;
        renameat(dir, cname(&temp).as_c_str(), dir, cname(name).as_c_str()).map_err(errno)?;
        Ok(failures)
    })();
    if written.is_err() {
        let _ = unlinkat(dir, cname(&temp).as_c_str(), AtFlags::empty());
    }
    written
}

/// The groups the restoring user belongs to.
fn my_groups() -> Vec<u32> {
    let mut groups: Vec<u32> = rustix::process::getgroups().unwrap_or_default().iter().map(|g| g.as_raw()).collect();
    groups.push(rustix::process::getegid().as_raw());
    groups
}

/// The group to apply, if any: names win over numbers when the name exists
/// here, and only groups the restoring user belongs to are applied.
fn group_to_apply(meta: &Meta, mine: &[u32]) -> Option<u32> {
    let by_name = meta.group.as_deref().and_then(uzers::get_group_by_name).map(|g| g.gid());
    let gid = by_name.or(meta.gid)?;
    mine.contains(&gid).then_some(gid)
}

struct Restorer<'r, 'o, D: Destination + 'static> {
    repo: &'r Repository<D>,
    options: &'o RestoreOptions<'o>,
    report: RestoreReport,
    on_progress: &'o mut dyn FnMut(&RestoreReport),
    groups: Vec<u32>,
}

impl<D: Destination + 'static> Restorer<'_, '_, D> {
    fn check_cancel(&self) -> Result<()> {
        match self.options.cancel {
            Some(flag) if flag.load(Ordering::Relaxed) => Err(Error::Aborted),
            _ => Ok(()),
        }
    }

    /// Decides the name to create. `None`: leave the existing item alone.
    fn place(&mut self, dir: &OwnedFd, name: &[u8], want_dir: bool) -> Result<Option<(Vec<u8>, bool)>> {
        let existing = match statat(dir, cname(name).as_c_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => FileType::from_raw_mode(st.st_mode),
            Err(rustix::io::Errno::NOENT) => return Ok(Some((name.to_vec(), false))),
            Err(e) => return Err(errno(e)),
        };
        // Folders merge: their contents are checked one by one.
        if want_dir && existing == FileType::Directory {
            return Ok(Some((name.to_vec(), true)));
        }
        match self.options.conflict {
            Conflict::Skip => {
                self.report.existing += 1;
                Ok(None)
            }
            Conflict::Replace if existing == FileType::Directory => {
                // Never remove a whole folder to put a file in its place.
                self.report.existing += 1;
                Ok(None)
            }
            Conflict::Replace => Ok(Some((name.to_vec(), false))),
            Conflict::KeepBoth => {
                for n in 1.. {
                    let suffix = if n == 1 { " (restored)".to_owned() } else { format!(" (restored {n})") };
                    let candidate = [name, suffix.as_bytes()].concat();
                    match statat(dir, cname(&candidate).as_c_str(), AtFlags::SYMLINK_NOFOLLOW) {
                        Err(rustix::io::Errno::NOENT) => return Ok(Some((candidate, false))),
                        Err(e) => return Err(errno(e)),
                        Ok(_) => {}
                    }
                }
                unreachable!()
            }
        }
    }

    fn apply_meta(&mut self, fd: &OwnedFd, meta: &Meta, default_mode: u32) -> Result<()> {
        self.report.xattr_failures += apply_meta(fd, meta, default_mode, &self.groups)?;
        Ok(())
    }

    /// Restores the files of one folder, several at a time. Names are
    /// decided first, in order, so "keep both" never picks one name twice.
    fn restore_files(&mut self, dir: &OwnedFd, files: &[(&Entry, &[String], String)]) -> Result<()> {
        let mut jobs = Vec::new();
        for (entry, content, path) in files {
            if let Some((name, _)) = self.place(dir, &entry.name, false)? {
                jobs.push((name, *entry, *content, path.as_str()));
            }
        }
        enum Event {
            Bytes(u64),
            Done(String, Result<u64>),
        }
        let (repo, groups, user_cancel) = (self.repo, self.groups.clone(), self.options.cancel);
        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel::<Event>();
        let mut failure: Option<Error> = None;
        std::thread::scope(|scope| {
            for _ in 0..FILE_THREADS.min(jobs.len()) {
                let (tx, jobs, next, stop, groups) = (tx.clone(), &jobs, &next, &stop, &groups);
                scope.spawn(move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        if i >= jobs.len() || stop.load(Ordering::SeqCst) {
                            break;
                        }
                        let (name, entry, content, path) = &jobs[i];
                        let cancelled = || {
                            if stop.load(Ordering::SeqCst) || user_cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                                Err(Error::Aborted)
                            } else {
                                Ok(())
                            }
                        };
                        let result = write_file(repo, dir, name, entry, content, groups, &cancelled, &|n| {
                            let _ = tx.send(Event::Bytes(n));
                        });
                        let _ = tx.send(Event::Done((*path).to_owned(), result));
                    }
                });
            }
            drop(tx);
            for event in rx {
                match event {
                    Event::Bytes(n) => self.report.bytes += n,
                    Event::Done(path, Ok(xattr_failures)) => {
                        self.report.files += 1;
                        self.report.xattr_failures += xattr_failures;
                        self.report.current = path;
                    }
                    Event::Done(_, Err(err)) => {
                        stop.store(true, Ordering::SeqCst);
                        if failure.is_none() || matches!(failure, Some(Error::Aborted)) {
                            failure = Some(err);
                        }
                    }
                }
                (self.on_progress)(&self.report);
            }
        });
        match failure {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn restore_symlink(&mut self, dir: &OwnedFd, entry: &Entry, target: &[u8]) -> Result<()> {
        let Some((name, _)) = self.place(dir, &entry.name, false)? else { return Ok(()) };
        let temp = format!(".xkyhold-{}.tmp", random_id()).into_bytes();
        symlinkat(cname(target).as_c_str(), dir, cname(&temp).as_c_str()).map_err(errno)?;
        let done = (|| {
            if let Some(gid) = group_to_apply(&entry.meta, &self.groups) {
                let _ =
                    chownat(dir, cname(&temp).as_c_str(), None, Some(Gid::from_raw(gid)), AtFlags::SYMLINK_NOFOLLOW);
            }
            if let Some(times) = timestamps(&entry.meta) {
                utimensat(dir, cname(&temp).as_c_str(), &times, AtFlags::SYMLINK_NOFOLLOW).map_err(errno)?;
            }
            renameat(dir, cname(&temp).as_c_str(), dir, cname(&name).as_c_str()).map_err(errno)
        })();
        if done.is_err() {
            let _ = unlinkat(dir, cname(&temp).as_c_str(), AtFlags::empty());
        }
        done?;
        self.report.symlinks += 1;
        Ok(())
    }

    fn restore_dir(&mut self, dir: &OwnedFd, entry: &Entry, subtree: &str, path: &str) -> Result<()> {
        let Some((name, exists)) = self.place(dir, &entry.name, true)? else { return Ok(()) };
        if !exists {
            if self.options.conflict == Conflict::Replace && name == entry.name {
                // A file or link is in the way: Replace removes it.
                let _ = unlinkat(dir, cname(&name).as_c_str(), AtFlags::empty());
            }
            mkdirat(dir, cname(&name).as_c_str(), Mode::from_raw_mode(0o700)).map_err(errno)?;
        }
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = openat(dir, cname(&name).as_c_str(), flags, Mode::empty()).map_err(errno)?;
        let children = self.repo.load_tree(subtree)?;
        self.restore_entries(&fd, &children, path)?;
        // Last, so writing the contents does not change the folder's time.
        if !exists {
            self.apply_meta(&fd, &entry.meta, 0o755)?;
        }
        self.report.folders += 1;
        Ok(())
    }

    fn restore_entries(&mut self, dir: &OwnedFd, entries: &[Entry], path: &str) -> Result<()> {
        let child = |entry: &Entry| format!("{path}{}{}", if path.is_empty() { "" } else { "/" }, entry.display_name());
        let files: Vec<(&Entry, &[String], String)> = entries
            .iter()
            .filter(|e| valid_name(&e.name))
            .filter_map(|e| match &e.kind {
                EntryKind::File { content, .. } => Some((e, content.as_slice(), child(e))),
                _ => None,
            })
            .collect();
        self.restore_files(dir, &files)?;
        for entry in entries {
            self.check_cancel()?;
            if !valid_name(&entry.name) {
                self.report.bad_names += 1;
                continue;
            }
            let child = child(entry);
            match &entry.kind {
                EntryKind::File { .. } => {}
                EntryKind::Dir { subtree, mount } => {
                    if *mount {
                        self.report.mounts += 1;
                    }
                    self.restore_dir(dir, entry, subtree, &child)?
                }
                EntryKind::Symlink { target } => self.restore_symlink(dir, entry, target)?,
                EntryKind::Unknown(_) => self.report.unknown += 1,
            }
            if !matches!(entry.kind, EntryKind::File { .. }) {
                (self.on_progress)(&self.report);
            }
        }
        Ok(())
    }
}

/// Restores `entries` (a folder's listing, or part of it) into `target`,
/// which must be an existing folder.
pub fn restore<D: Destination + 'static>(
    repo: &Repository<D>,
    entries: &[Entry],
    target: &Path,
    options: &RestoreOptions<'_>,
    on_progress: &mut dyn FnMut(&RestoreReport),
) -> Result<RestoreReport> {
    if rustix::process::geteuid().is_root() {
        return Err(Error::Root);
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let dir = openat(rustix::fs::CWD, target, flags, Mode::empty()).map_err(errno)?;
    let mut restorer = Restorer { repo, options, report: RestoreReport::default(), on_progress, groups: my_groups() };
    restorer.restore_entries(&dir, entries, "")?;
    let _ = rustix::fs::fsync(dir.as_fd());
    Ok(restorer.report)
}

/// Finds an entry by its path of names below a tree.
pub fn find<D: Destination + 'static>(repo: &Repository<D>, tree: &str, path: &[&[u8]]) -> Result<Option<Entry>> {
    let Some((first, rest)) = path.split_first() else { return Ok(None) };
    let Some(entry) = repo.load_tree(tree)?.into_iter().find(|e| e.name == *first) else { return Ok(None) };
    match (&entry.kind, rest.is_empty()) {
        (_, true) => Ok(Some(entry)),
        (EntryKind::Dir { subtree, .. }, false) => find(repo, subtree, rest),
        _ => Ok(None),
    }
}
