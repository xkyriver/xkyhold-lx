//! A repository in a local folder: an external disk, a mounted NAS
//! (FORMAT-v2.md section 9). Same layout as in Drive. Writes are atomic
//! (temporary name, `fsync`, rename, folder `fsync`); "delete" moves the
//! object into `trash/<dir>/` inside the repository.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use xkyhold_core::crypto::random_id;
use xkyhold_core::destination::{Destination, Dir, StoredObject};
use xkyhold_core::{Error, Result};

const TEMP_PREFIX: &str = ".tmp-";
pub const TRASH: &str = "trash";
pub const TRASH_DAYS: u64 = 30;

pub struct LocalFolder {
    root: PathBuf,
}

/// Object names are hex ids, or `config`; anything else is refused so a
/// name can never reach outside the repository folder.
fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty() && name.len() <= 128 && name.bytes().all(|b| b.is_ascii_alphanumeric());
    if ok { Ok(()) } else { Err(Error::Format(format!("invalid object name {name:?}"))) }
}

fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

impl LocalFolder {
    /// The repository folder; created on the first write.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn dir(&self, dir: Dir) -> PathBuf {
        match dir {
            Dir::Root => self.root.clone(),
            other => self.root.join(other.as_str()),
        }
    }

    /// Removes objects that have been in the trash for more than 30 days.
    pub fn empty_old_trash(&self) -> Result<usize> {
        let limit = SystemTime::now() - Duration::from_secs(TRASH_DAYS * 86_400);
        let mut removed = 0;
        let Ok(dirs) = fs::read_dir(self.root.join(TRASH)) else { return Ok(0) };
        for dir in dirs.flatten() {
            for item in fs::read_dir(dir.path())?.flatten() {
                if item.metadata()?.modified()? < limit {
                    fs::remove_file(item.path())?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

impl Destination for LocalFolder {
    fn put(&self, dir: Dir, name: &str, data: &[u8]) -> Result<()> {
        check_name(name)?;
        let folder = self.dir(dir);
        fs::create_dir_all(&folder)?;
        let temp = folder.join(format!("{TEMP_PREFIX}{}", random_id()));
        let written = (|| {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
            file.write_all(data)?;
            file.sync_all()?;
            fs::rename(&temp, folder.join(name))?;
            sync_dir(&folder)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        written
    }

    fn get(&self, dir: Dir, name: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>> {
        check_name(name)?;
        let path = self.dir(dir).join(name);
        let mut file = match File::open(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!("{}/{name}", dir.as_str())));
            }
            other => other?,
        };
        let mut out = Vec::new();
        match range {
            None => {
                file.read_to_end(&mut out)?;
            }
            Some((offset, length)) => {
                file.seek(SeekFrom::Start(offset))?;
                file.take(length).read_to_end(&mut out)?;
            }
        }
        Ok(out)
    }

    fn list(&self, dir: Dir, _fresh: bool) -> Result<Vec<StoredObject>> {
        let entries = match fs::read_dir(self.dir(dir)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            other => other?,
        };
        let mut out = Vec::new();
        for item in entries {
            let item = item?;
            let Ok(name) = item.file_name().into_string() else { continue };
            let md = item.metadata()?;
            if !md.is_file() || name.starts_with(TEMP_PREFIX) || check_name(&name).is_err() {
                continue;
            }
            out.push(StoredObject { name, size: md.len(), sha256: None });
        }
        Ok(out)
    }

    fn trash(&self, dir: Dir, name: &str) -> Result<()> {
        check_name(name)?;
        let source = self.dir(dir).join(name);
        let bin = self.root.join(TRASH).join(if dir == Dir::Root { "root" } else { dir.as_str() });
        fs::create_dir_all(&bin)?;
        let mut target = bin.join(name);
        if target.exists() {
            target = bin.join(format!("{name}-{}", random_id()));
        }
        match fs::rename(&source, &target) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!("{}/{name}", dir.as_str())));
            }
            other => other?,
        }
        // The trash keeps objects for 30 days from now.
        File::options().write(true).open(&target)?.set_modified(SystemTime::now())?;
        sync_dir(&bin)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_round_trip_and_trash_keeps_them() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = LocalFolder::new(tmp.path().join("XkyHold/test"));
        assert!(dest.list(Dir::Packs, true).unwrap().is_empty());
        dest.put(Dir::Packs, "abc123", b"0123456789").unwrap();
        dest.put(Dir::Root, "config", b"{}").unwrap();
        assert_eq!(dest.get(Dir::Packs, "abc123", Some((2, 3))).unwrap(), b"234");
        assert_eq!(dest.get(Dir::Root, "config", None).unwrap(), b"{}");
        assert_eq!(dest.list(Dir::Packs, true).unwrap()[0].size, 10);
        dest.trash(Dir::Packs, "abc123").unwrap();
        assert!(dest.list(Dir::Packs, true).unwrap().is_empty());
        assert!(tmp.path().join("XkyHold/test/trash/packs/abc123").exists());
        assert_eq!(dest.empty_old_trash().unwrap(), 0, "kept for 30 days");
        assert_eq!(dest.get(Dir::Packs, "abc123", None).unwrap_err().kind(), "not-found");
    }

    #[test]
    fn names_cannot_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = LocalFolder::new(tmp.path());
        for bad in ["../x", "a/b", "", ".tmp-x"] {
            assert!(dest.put(Dir::Packs, bad, b"x").is_err(), "{bad}");
        }
    }
}
