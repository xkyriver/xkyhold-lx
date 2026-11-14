//! Looking at the home folder for the "what to save" screens: the folders
//! inside a folder, the disks mounted inside home, and the space each
//! suggested exclusion would save. Never follows symbolic links.

use std::collections::{HashMap, HashSet};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use xkyhold_core::backup::mount_points;
use xkyhold_core::rules::{DEFAULTS, PRESETS, glob_match};
use xkyhold_core::{Error, Result};

/// Top-level folders that cloud storage apps keep in sync. They are backed
/// up by default; the window shows them as candidates to leave out.
pub const CLOUD_FOLDERS: &[&str] =
    &["Dropbox", "OneDrive", "Google Drive", "GoogleDrive", "pCloudDrive", "MEGA", "Nextcloud", "ownCloud"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub name: String,
    /// The name is valid UTF-8. Others are always backed up but cannot be
    /// chosen in the settings, which store text.
    pub text_name: bool,
    /// Another disk or file system, not crossed unless chosen.
    pub mount: bool,
    /// A cloud storage folder.
    pub cloud: bool,
    /// Always left out (caches, trash, this app's own state).
    pub always_out: bool,
}

fn always_out(rel: &[u8]) -> bool {
    DEFAULTS.iter().flat_map(|(_, patterns)| patterns.iter()).any(|p| glob_match(p.as_bytes(), rel))
}

fn join(parent: &[u8], name: &[u8]) -> Vec<u8> {
    if parent.is_empty() { name.to_vec() } else { [parent, b"/", name].concat() }
}

/// The folders directly inside `rel` (a path inside home; empty for home).
pub fn folders(home: &Path, rel: &str) -> Result<Vec<Folder>> {
    let rel_path = crate::inside_home(home, Path::new(rel))?;
    let dir = home.join(Path::new(std::ffi::OsStr::from_bytes(&rel_path)));
    let mounts = mount_points();
    let mut out = Vec::new();
    let read = std::fs::read_dir(&dir).map_err(|e| Error::Read { path: dir.display().to_string(), source: e })?;
    for entry in read.flatten() {
        // file_type does not follow symbolic links.
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name_os = entry.file_name();
        let name = name_os.as_bytes();
        let child = join(&rel_path, name);
        out.push(Folder {
            name: String::from_utf8_lossy(name).into_owned(),
            text_name: std::str::from_utf8(name).is_ok(),
            mount: mounts.contains(entry.path().as_os_str().as_bytes()),
            cloud: rel_path.is_empty() && CLOUD_FOLDERS.iter().any(|c| c.as_bytes() == name),
            always_out: always_out(&child),
        });
    }
    out.sort_by_key(|f| f.name.to_lowercase());
    Ok(out)
}

/// Disks and other file systems mounted inside home, relative to it.
pub fn mounts_inside(home: &Path) -> Vec<String> {
    let prefix = [home.as_os_str().as_bytes(), b"/"].concat();
    let mut found: Vec<String> = mount_points()
        .into_iter()
        .filter_map(|m| m.strip_prefix(prefix.as_slice()).map(|r| String::from_utf8_lossy(r).into_owned()))
        .filter(|r| !r.is_empty())
        .collect();
    found.sort();
    found
}

/// The space each suggested exclusion would save, by preset id. Walks the
/// whole home folder once (minus what is always left out and other disks),
/// so it runs in the background and stops when `cancel` is set.
pub fn preset_sizes(home: &Path, cancel: &AtomicBool) -> Result<HashMap<&'static str, u64>> {
    let mounts: HashSet<Vec<u8>> = mount_points();
    let mut sizes: HashMap<&'static str, u64> = PRESETS.iter().map(|p| (p.id, 0)).collect();
    let mut stack: Vec<Vec<u8>> = vec![Vec::new()];
    while let Some(rel) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Aborted);
        }
        let dir = home.join(std::ffi::OsStr::from_bytes(&rel));
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        for entry in read.flatten() {
            let child = join(&rel, entry.file_name().as_bytes());
            if always_out(&child) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let preset = PRESETS.iter().find(|p| {
                p.patterns.iter().any(|g| glob_match(g.as_bytes(), &child))
                    && p.beside.is_none_or(|file| dir.join(file).symlink_metadata().is_ok_and(|m| m.is_file()))
            });
            if let Some(preset) = preset {
                *sizes.get_mut(preset.id).expect("every preset") +=
                    if meta.is_dir() { tree_size(&entry.path(), &mounts, cancel) } else { meta.len() };
            } else if meta.is_dir() && !mounts.contains(entry.path().as_os_str().as_bytes()) {
                stack.push(child);
            }
        }
    }
    Ok(sizes)
}

/// Bytes in regular files below `dir`, on the same disk.
fn tree_size(dir: &Path, mounts: &HashSet<Vec<u8>>, cancel: &AtomicBool) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(read) = std::fs::read_dir(&d) else { continue };
        for entry in read.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                if !mounts.contains(entry.path().as_os_str().as_bytes()) {
                    stack.push(entry.path());
                }
            } else if meta.is_file() {
                total += meta.size();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_folders_and_sizes_presets() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        for d in ["Documents", "Dropbox", ".cache/x", "code/app/node_modules/lib", "Downloads"] {
            std::fs::create_dir_all(h.join(d)).unwrap();
        }
        std::fs::write(h.join("code/app/node_modules/lib/a.js"), vec![0u8; 1000]).unwrap();
        std::fs::write(h.join("Downloads/big.iso"), vec![0u8; 500]).unwrap();
        // A Rust build folder counts; a folder that only happens to be called target does not.
        for d in ["code/rs/target/debug", "Photos/target", "work/svc/target/classes"] {
            std::fs::create_dir_all(h.join(d)).unwrap();
        }
        std::fs::write(h.join("code/rs/Cargo.toml"), b"[package]").unwrap();
        std::fs::write(h.join("code/rs/target/debug/app"), vec![0u8; 300]).unwrap();
        std::fs::write(h.join("Photos/target/beach.jpg"), vec![0u8; 70]).unwrap();
        std::fs::write(h.join("work/svc/pom.xml"), b"<project/>").unwrap();
        std::fs::write(h.join("work/svc/target/classes/A.class"), vec![0u8; 40]).unwrap();
        std::fs::write(h.join("notes.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(h.join("Documents"), h.join("link")).unwrap();

        let top = folders(h, "").unwrap();
        let names: Vec<&str> = top.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, [".cache", "code", "Documents", "Downloads", "Dropbox", "Photos", "work"]);
        assert!(top[0].always_out);
        assert!(top[4].cloud);
        assert!(!top[2].cloud && !top[2].always_out);
        assert!(folders(h, "../").is_err());

        let sizes = preset_sizes(h, &AtomicBool::new(false)).unwrap();
        assert_eq!(sizes["node-modules"], 1000);
        assert_eq!(sizes["downloads"], 500);
        assert_eq!(sizes["rust-target"], 300);
        assert_eq!(sizes["maven-target"], 40);
        assert_eq!(sizes["steam"], 0);
    }
}
