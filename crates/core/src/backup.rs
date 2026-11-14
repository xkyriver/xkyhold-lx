//! Backup of the home folder (FORMAT.md 10.1, FORMAT-v2.md 4 and 5).
//!
//! The walk goes through file descriptors: every folder is opened relative
//! to its parent with `O_NOFOLLOW`, so a symbolic link is never followed,
//! even if a folder is swapped for a link while the backup runs. Nothing
//! is ever written to the source.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::CString;
use std::io::Read;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, openat, readlinkat, statat};

use crate::bytes::to_base64;
use crate::crypto::CHUNK_SIZE;
use crate::destination::Destination;
use crate::pack::BlobType;
use crate::repo::Repository;
use crate::rules::{CACHEDIR_SIGNATURE, Exclusions, Matcher, Verdict};
use crate::snapshot::{Home, Root, Snapshot, Source, Stats};
use crate::tree::{Change, Entry, EntryKind, Meta, Xattr, valid_name};
use crate::{Error, Result};

/// The root name XkyHold-lx gives the home folder in a snapshot.
pub const HOME_ROOT: &str = "home";

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub files: u64,
    pub bytes: u64,
    pub skipped: u64,
    pub changed: u64,
    pub special: u64,
    pub symlinks: u64,
    pub mounts: u64,
    /// Files whose content was read (new or changed).
    pub files_read: u64,
    /// Path being read, relative to the home folder.
    pub current: String,
}

pub struct BackupOptions<'a> {
    pub device: String,
    /// Time of the snapshot, ISO 8601.
    pub time: String,
    /// The previous snapshot; unchanged files are taken from it unread.
    pub parent: Option<&'a Snapshot>,
    pub exclusions: Exclusions,
    /// Paths inside the home folder to back up; `None` is the whole folder.
    pub selection: Option<Vec<Vec<u8>>>,
    pub chunk_size: usize,
    /// Absolute paths of mount points; `None` reads `/proc/self/mountinfo`.
    pub mount_points: Option<HashSet<Vec<u8>>>,
    pub cancel: Option<&'a AtomicBool>,
}

impl BackupOptions<'_> {
    pub fn new(device: String, time: String) -> Self {
        Self {
            device,
            time,
            parent: None,
            exclusions: Exclusions::default(),
            selection: None,
            chunk_size: CHUNK_SIZE as usize,
            mount_points: None,
            cancel: None,
        }
    }
}

/// The chosen items, as a tree of names. A `whole` node takes everything
/// below it.
#[derive(Default, Debug)]
struct Selection {
    whole: bool,
    children: BTreeMap<Vec<u8>, Selection>,
}

impl Selection {
    fn build(paths: &[Vec<u8>]) -> Result<Self> {
        let mut root = Selection::default();
        for path in paths {
            if path.is_empty() {
                root.whole = true;
                continue;
            }
            let mut node = &mut root;
            for name in path.split(|&b| b == b'/') {
                if !valid_name(name) {
                    return Err(Error::OutsideHome(String::from_utf8_lossy(path).into_owned()));
                }
                node = node.children.entry(name.to_vec()).or_default();
            }
            node.whole = true;
        }
        Ok(root)
    }
}

fn is_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// Mount points listed by the kernel, as absolute paths.
pub fn mount_points() -> HashSet<Vec<u8>> {
    let Ok(text) = std::fs::read("/proc/self/mountinfo") else { return HashSet::new() };
    text.split(|&b| b == b'\n').filter_map(|line| line.split(|&b| b == b' ').nth(4)).map(unescape_octal).collect()
}

/// `/proc/self/mountinfo` writes space, tab, newline and backslash as `\ooo`.
fn unescape_octal(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\' && field.len() >= i + 4 {
            let digits = &field[i + 1..i + 4];
            if digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                out.push(digits.iter().fold(0u8, |acc, d| acc.wrapping_mul(8).wrapping_add(d - b'0')));
                i += 4;
                continue;
            }
        }
        out.push(field[i]);
        i += 1;
    }
    out
}

fn time_ns(sec: i64, nsec: u64) -> i128 {
    i128::from(sec) * 1_000_000_000 + i128::from(nsec)
}

fn mtime_ns(st: &Stat) -> i128 {
    time_ns(st.st_mtime, st.st_mtime_nsec)
}

fn ctime_ns(st: &Stat) -> i128 {
    time_ns(st.st_ctime, st.st_ctime_nsec)
}

fn change_of(st: &Stat) -> Change {
    let (ctime, ctime_nanos) = crate::tree::split_ns(ctime_ns(st));
    Change { ctime, ctime_nanos, ino: st.st_ino.to_string() }
}

fn cstring(name: &[u8]) -> CString {
    CString::new(name).expect("names from the kernel have no NUL")
}

/// Extended attributes worth keeping (FORMAT-v2.md 4.5).
fn keep_xattr(name: &[u8]) -> bool {
    name.starts_with(b"user.") || name == b"system.posix_acl_access" || name == b"system.posix_acl_default"
}

const XATTR_MAX: usize = 64 * 1024;

/// Reads the kept extended attributes of `name` inside `dir`, without
/// following a symbolic link: through `/proc/self/fd`, anchored at `dir`.
fn read_xattrs(dir: BorrowedFd<'_>, name: &[u8]) -> Option<Vec<Xattr>> {
    use std::os::fd::AsRawFd;
    let mut path = format!("/proc/self/fd/{}/", dir.as_raw_fd()).into_bytes();
    path.extend_from_slice(name);
    let path = CString::new(path).ok()?;
    let mut list = vec![0u8; XATTR_MAX];
    let n = rustix::fs::llistxattr(path.as_c_str(), &mut list[..]).ok()?;
    let mut out = Vec::new();
    for attr in list[..n].split(|&b| b == 0).filter(|a| !a.is_empty() && keep_xattr(a)) {
        let Ok(attr_name) = std::str::from_utf8(attr) else { continue };
        let mut value = vec![0u8; XATTR_MAX];
        if let Ok(len) = rustix::fs::lgetxattr(path.as_c_str(), attr, &mut value[..]) {
            out.push(Xattr { name: attr_name.to_owned(), value: to_base64(&value[..len]) });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    (!out.is_empty()).then_some(out)
}

#[derive(Default)]
struct Names {
    users: HashMap<u32, Option<String>>,
    groups: HashMap<u32, Option<String>>,
}

impl Names {
    fn user(&mut self, uid: u32) -> Option<String> {
        self.users
            .entry(uid)
            .or_insert_with(|| uzers::get_user_by_uid(uid).map(|u| u.name().to_string_lossy().into_owned()))
            .clone()
    }

    fn group(&mut self, gid: u32) -> Option<String> {
        self.groups
            .entry(gid)
            .or_insert_with(|| uzers::get_group_by_gid(gid).map(|g| g.name().to_string_lossy().into_owned()))
            .clone()
    }
}

struct Walker<'r, 'o, D: Destination + 'static> {
    repo: &'r mut Repository<D>,
    matcher: Matcher,
    mounts: HashSet<Vec<u8>>,
    home: Vec<u8>,
    chunk_size: usize,
    cancel: Option<&'o AtomicBool>,
    progress: Progress,
    on_progress: &'o mut dyn FnMut(&Progress),
    names: Names,
    empty_tree: Option<String>,
}

impl<D: Destination + 'static> Walker<'_, '_, D> {
    fn check_cancel(&self) -> Result<()> {
        match self.cancel {
            Some(flag) if flag.load(Ordering::Relaxed) => Err(Error::Aborted),
            _ => Ok(()),
        }
    }

    fn report(&mut self) {
        (self.on_progress)(&self.progress);
    }

    fn meta(&mut self, st: &Stat, with_mode: bool) -> Meta {
        let mut meta = Meta {
            mode: with_mode.then_some(st.st_mode & 0o7777),
            uid: Some(st.st_uid),
            gid: Some(st.st_gid),
            user: self.names.user(st.st_uid),
            group: self.names.group(st.st_gid),
            ..Meta::default()
        };
        meta.set_time_ns(mtime_ns(st));
        meta
    }

    fn empty_tree(&mut self) -> Result<String> {
        if let Some(id) = &self.empty_tree {
            return Ok(id.clone());
        }
        let id = self.repo.save_tree(&mut [])?;
        self.empty_tree = Some(id.clone());
        Ok(id)
    }

    fn previous(&self, tree: Option<&str>) -> HashMap<Vec<u8>, Entry> {
        tree.and_then(|id| self.repo.load_tree(id).ok())
            .map(|entries| entries.into_iter().map(|e| (e.name.clone(), e)).collect())
            .unwrap_or_default()
    }

    fn has_cachedir_tag(dir: BorrowedFd<'_>) -> bool {
        let Ok(fd) = openat(dir, c"CACHEDIR.TAG", OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
        else {
            return false;
        };
        let mut head = vec![0u8; CACHEDIR_SIGNATURE.len()];
        std::fs::File::from(fd).read_exact(&mut head).is_ok() && head == CACHEDIR_SIGNATURE
    }

    /// Stores one folder and returns its tree id. `required` folders (the
    /// home folder and chosen ones) fail the backup if unreadable.
    fn walk_dir(
        &mut self,
        dir: &OwnedFd,
        rel: &[u8],
        old_tree: Option<&str>,
        sel: Option<&Selection>,
    ) -> Result<String> {
        let old = self.previous(old_tree);
        let mut names: Vec<Vec<u8>> = Vec::new();
        let listing = Dir::read_from(dir).map_err(|e| self.read_error(rel, e))?;
        for item in listing {
            let item = item.map_err(|e| self.read_error(rel, e))?;
            let name = item.file_name().to_bytes();
            if name != b"." && name != b".." {
                names.push(name.to_vec());
            }
        }
        names.sort();

        let mut entries = Vec::new();
        for name in names {
            self.check_cancel()?;
            let child_sel = match sel {
                None => None,
                Some(s) if s.whole => None,
                Some(s) => match s.children.get(&name) {
                    Some(child) => Some(child),
                    None => continue,
                },
            };
            let child_rel = if rel.is_empty() { name.clone() } else { [rel, b"/", &name].concat() };
            if self.matcher.check(&child_rel) == Verdict::LeaveOut {
                continue;
            }
            // A chosen item, or a folder on the way to one, must be readable.
            let required = child_sel.is_some();
            let st = match statat(dir, cstring(&name).as_c_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(st) => st,
                Err(e) if required => return Err(self.read_error(&child_rel, e)),
                Err(_) => {
                    self.progress.skipped += 1;
                    continue;
                }
            };
            let before = old.get(&name);
            let entry = match FileType::from_raw_mode(st.st_mode) {
                FileType::Directory => match self.store_dir(dir, &name, &child_rel, &st, before, child_sel)? {
                    Some(entry) => entry,
                    None => continue,
                },
                FileType::RegularFile => match self.store_file(dir, &name, &child_rel, &st, before)? {
                    Some(entry) => entry,
                    None if required => {
                        return Err(Error::Read {
                            path: String::from_utf8_lossy(&child_rel).into_owned(),
                            source: std::io::Error::other("unreadable"),
                        });
                    }
                    None => {
                        self.progress.skipped += 1;
                        continue;
                    }
                },
                FileType::Symlink => {
                    let Ok(target) = readlinkat(dir, cstring(&name).as_c_str(), Vec::new()) else {
                        self.progress.skipped += 1;
                        continue;
                    };
                    self.progress.symlinks += 1;
                    let meta = self.meta(&st, false);
                    Entry { name: name.clone(), kind: EntryKind::Symlink { target: target.into_bytes() }, meta }
                }
                _ => {
                    self.progress.special += 1;
                    continue;
                }
            };
            entries.push(entry);
            self.report();
        }
        self.repo.save_tree(&mut entries)
    }

    fn read_error(&self, rel: &[u8], source: rustix::io::Errno) -> Error {
        Error::Read { path: String::from_utf8_lossy(rel).into_owned(), source: source.into() }
    }

    fn store_dir(
        &mut self,
        parent: &OwnedFd,
        name: &[u8],
        rel: &[u8],
        st: &Stat,
        before: Option<&Entry>,
        sel: Option<&Selection>,
    ) -> Result<Option<Entry>> {
        let mut meta = self.meta(st, true);
        meta.xattrs = read_xattrs(parent.as_fd(), name);
        let mut absolute = self.home.clone();
        absolute.push(b'/');
        absolute.extend_from_slice(rel);
        if self.mounts.contains(&absolute) && !self.matcher.crosses(rel) {
            self.progress.mounts += 1;
            let subtree = self.empty_tree()?;
            return Ok(Some(Entry { name: name.to_vec(), kind: EntryKind::Dir { subtree, mount: true }, meta }));
        }
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = match openat(parent, cstring(name).as_c_str(), flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(e) if sel.is_some() => return Err(self.read_error(rel, e)),
            Err(_) => {
                self.progress.skipped += 1;
                return Ok(None);
            }
        };
        if Self::has_cachedir_tag(fd.as_fd()) {
            return Ok(None);
        }
        let old_subtree = match before.map(|e| &e.kind) {
            Some(EntryKind::Dir { subtree, mount: false }) => Some(subtree.as_str()),
            _ => None,
        };
        let subtree = match self.walk_dir(&fd, rel, old_subtree, sel) {
            Ok(id) => id,
            Err(Error::Read { .. }) if sel.is_none() => {
                // An unreadable folder inside the home folder is skipped and
                // counted; anything else (destination, cancel) stops the backup.
                self.progress.skipped += 1;
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        Ok(Some(Entry { name: name.to_vec(), kind: EntryKind::Dir { subtree, mount: false }, meta }))
    }

    fn store_file(
        &mut self,
        dir: &OwnedFd,
        name: &[u8],
        rel: &[u8],
        st: &Stat,
        before: Option<&Entry>,
    ) -> Result<Option<Entry>> {
        let size = st.st_size as u64;
        if let Some(Entry { kind: EntryKind::File { size: old_size, content }, meta: old_meta, .. }) = before
            && *old_size == size
            && old_meta.time_ns() == Some(mtime_ns(st))
            && old_meta.change == Some(change_of(st))
            && content.iter().all(|id| self.repo.has(id))
        {
            // Unchanged content; permissions and attributes are read again.
            let mut meta = self.meta(st, true);
            meta.xattrs = read_xattrs(dir.as_fd(), name);
            meta.change = Some(change_of(st));
            self.progress.files += 1;
            self.progress.bytes += size;
            let kind = EntryKind::File { size, content: content.clone() };
            return Ok(Some(Entry { name: name.to_vec(), kind, meta }));
        }

        self.progress.current = String::from_utf8_lossy(rel).into_owned();
        self.progress.files_read += 1;
        self.report();
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK;
        let Ok(fd) = openat(dir, cstring(name).as_c_str(), flags, Mode::empty()) else { return Ok(None) };
        let mut stored = None;
        for attempt in 0..2 {
            let Ok(start) = fstat(&fd) else { return Ok(None) };
            if start.st_ino != st.st_ino || start.st_dev != st.st_dev {
                return Ok(None); // swapped since it was listed
            }
            let content = match self.read_content(&fd, rel) {
                Ok(c) => c,
                Err(Error::Read { .. }) => return Ok(None),
                Err(e) => return Err(e),
            };
            let end = fstat(&fd).ok();
            let unchanged = end.is_some_and(|e| {
                e.st_size == start.st_size && mtime_ns(&e) == mtime_ns(&start) && ctime_ns(&e) == ctime_ns(&start)
            });
            let read: u64 = content.1;
            stored = Some((content.0, read, start));
            if unchanged {
                break;
            }
            if attempt == 1 {
                self.progress.changed += 1;
            }
        }
        let (content, read, start) = stored.expect("at least one attempt");
        // Record the time from before the read, so a file that was still
        // changing is read again next time.
        let mut meta = self.meta(&start, true);
        meta.xattrs = read_xattrs(dir.as_fd(), name);
        meta.change = Some(change_of(&start));
        self.progress.files += 1;
        self.progress.bytes += read;
        Ok(Some(Entry { name: name.to_vec(), kind: EntryKind::File { size: read, content }, meta }))
    }

    /// Reads a file from the start in chunks; returns the blob ids and the
    /// number of bytes read.
    fn read_content(&mut self, fd: &OwnedFd, rel: &[u8]) -> Result<(Vec<String>, u64)> {
        let file = fd.try_clone().map_err(|e| Error::Read { path: lossy(rel), source: e })?;
        let mut file = std::fs::File::from(file);
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(0)).map_err(|e| Error::Read { path: lossy(rel), source: e })?;
        let mut content = Vec::new();
        let mut total = 0u64;
        let mut chunk = vec![0u8; self.chunk_size];
        loop {
            self.check_cancel()?;
            let mut filled = 0;
            while filled < chunk.len() {
                match file.read(&mut chunk[filled..]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(Error::Read { path: lossy(rel), source: e }),
                }
            }
            if filled == 0 {
                break;
            }
            content.push(self.repo.save_blob(BlobType::Data, &chunk[..filled])?);
            total += filled as u64;
            if filled < chunk.len() {
                break;
            }
        }
        Ok((content, total))
    }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Backs up the home folder (or the chosen items inside it) and writes a
/// version 2 snapshot. `home` must be the user's home folder.
pub fn backup<D: Destination + 'static>(
    repo: &mut Repository<D>,
    home: &Path,
    options: BackupOptions<'_>,
    on_progress: &mut dyn FnMut(&Progress),
) -> Result<Snapshot> {
    if is_root() {
        return Err(Error::Root);
    }
    if !repo.writable() {
        return Err(Error::ReadOnly);
    }
    if !home.is_absolute() || home == Path::new("/") {
        return Err(Error::OutsideHome(home.display().to_string()));
    }
    let selection = options.selection.as_deref().map(Selection::build).transpose()?;
    let home_fd = openat(rustix::fs::CWD, home, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())
        .map_err(|e| Error::Read { path: home.display().to_string(), source: e.into() })?;
    let home_st = fstat(&home_fd).map_err(std::io::Error::from)?;
    let home_bytes = home.as_os_str().as_bytes().to_vec();
    let mounts = options.mount_points.clone().unwrap_or_else(mount_points);
    let matcher = Matcher::new(&options.exclusions);
    let left_out = matcher.rules();

    let parent_tree = options.parent.and_then(|p| {
        p.roots
            .iter()
            .find(|r| r.source.as_ref().is_some_and(|s| s.kind == "home" && s.path.is_empty()))
            .map(|r| r.tree.clone())
    });

    let mut walker = Walker {
        repo,
        matcher,
        mounts,
        home: home_bytes,
        chunk_size: options.chunk_size,
        cancel: options.cancel,
        progress: Progress::default(),
        on_progress,
        names: Names::default(),
        empty_tree: None,
    };
    let result = (|| {
        let sel = selection.as_ref().filter(|s| !s.whole);
        let tree = walker.walk_dir(&home_fd, b"", parent_tree.as_deref(), sel)?;
        let root_meta = {
            let mut meta = walker.meta(&home_st, true);
            meta.xattrs = None;
            meta
        };
        walker.repo.flush()?;
        let p = &walker.progress;
        let stats = Stats {
            files: p.files,
            bytes: p.bytes,
            skipped: p.skipped,
            changed: Some(p.changed),
            special: Some(p.special),
            symlinks: Some(p.symlinks),
            mounts: Some(p.mounts),
        };
        let uid = rustix::process::geteuid().as_raw();
        let gid = rustix::process::getegid().as_raw();
        let user = walker.names.user(uid).unwrap_or_else(|| uid.to_string());
        walker.repo.save_snapshot(Snapshot {
            format: Some(2),
            id: String::new(),
            time: options.time.clone(),
            device: options.device.clone(),
            home: Some(Home { path: lossy(&walker.home), user, uid, gid }),
            roots: vec![Root {
                name: HOME_ROOT.into(),
                tree,
                source: Some(Source { kind: "home".into(), path: String::new() }),
                meta: Some(root_meta),
                left_out: Some(left_out),
            }],
            stats,
            parent: options.parent.map(|p| p.id.clone()),
        })
    })();
    if result.is_err() {
        walker.repo.abandon();
    }
    result
}
