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
use std::sync::atomic::{AtomicBool, Ordering};

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
        let mode = meta.mode.unwrap_or(default_mode) & 0o1777;
        fchmod(fd, Mode::from_raw_mode(mode)).map_err(errno)?;
        if let Some(gid) = group_to_apply(meta, &self.groups) {
            // Owner stays; only the group changes.
            let _ = rustix::fs::fchown(fd, None, Some(Gid::from_raw(gid)));
        }
        for attr in meta.xattrs.iter().flatten() {
            let ok = from_base64(&attr.value)
                .ok()
                .is_some_and(|value| fsetxattr(fd, attr.name.as_str(), &value, XattrFlags::empty()).is_ok());
            if !ok {
                self.report.xattr_failures += 1;
            }
        }
        if let Some(times) = timestamps(meta) {
            futimens(fd, &times).map_err(errno)?;
        }
        Ok(())
    }

    fn restore_file(&mut self, dir: &OwnedFd, entry: &Entry, content: &[String], path: &str) -> Result<()> {
        let Some((name, _)) = self.place(dir, &entry.name, false)? else { return Ok(()) };
        let temp = format!(".xkyhold-{}.tmp", random_id()).into_bytes();
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = openat(dir, cname(&temp).as_c_str(), flags, Mode::from_raw_mode(0o600)).map_err(errno)?;
        let written = (|| {
            let mut file = std::fs::File::from(fd.try_clone()?);
            for id in content {
                self.check_cancel()?;
                let chunk = self.repo.load_blob(BlobType::Data, id)?;
                file.write_all(&chunk)?;
                self.report.bytes += chunk.len() as u64;
                (self.on_progress)(&self.report);
            }
            self.apply_meta(&fd, &entry.meta, 0o644)?;
            renameat(dir, cname(&temp).as_c_str(), dir, cname(&name).as_c_str()).map_err(errno)
        })();
        if written.is_err() {
            let _ = unlinkat(dir, cname(&temp).as_c_str(), AtFlags::empty());
        }
        written?;
        self.report.files += 1;
        self.report.current = path.to_owned();
        (self.on_progress)(&self.report);
        Ok(())
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
        for entry in entries {
            self.check_cancel()?;
            if !valid_name(&entry.name) {
                self.report.bad_names += 1;
                continue;
            }
            let child = format!("{path}{}{}", if path.is_empty() { "" } else { "/" }, entry.display_name());
            match &entry.kind {
                EntryKind::File { content, .. } => self.restore_file(dir, entry, content, &child)?,
                EntryKind::Dir { subtree, mount } => {
                    if *mount {
                        self.report.mounts += 1;
                    }
                    self.restore_dir(dir, entry, subtree, &child)?
                }
                EntryKind::Symlink { target } => self.restore_symlink(dir, entry, target)?,
                EntryKind::Unknown(_) => self.report.unknown += 1,
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
