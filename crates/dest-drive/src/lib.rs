//! A repository in the user's Google Drive (FORMAT.md section 3): the
//! folder `XkyHold/<name>`, the `drive.file` scope, and an installed-app
//! OAuth client in the same Google Cloud project as XkyHold web, so both
//! apps see the same files. "Delete" always moves to the Drive trash.

pub mod auth;
pub mod drive;
pub mod fake;
pub mod http;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

pub use crate::auth::{Client, RefreshingTokens, SignedIn, Tokens, sign_in};
pub use crate::http::{Http, Ureq};

use xkyhold_core::destination::{Destination, Dir, StoredObject};
use xkyhold_core::{Error, Result};

pub use crate::drive::Drive;
use crate::drive::{DriveFile, FOLDER_MIME};

/// A Drive client on the real network.
pub fn connect(tokens: Arc<dyn Tokens>) -> Drive {
    Drive::new(Arc::new(Ureq::default()), tokens)
}

/// The app's folder in My Drive; repositories are its sub-folders.
pub const APP_FOLDER: &str = "XkyHold";
/// Above this size an upload is resumable; below, a single request.
const SMALL_UPLOAD: usize = 4 * 1024 * 1024;
const SUBFOLDERS: [Dir; 4] = [Dir::Packs, Dir::Index, Dir::Snapshots, Dir::Locks];

#[derive(Default)]
struct State {
    folders: HashMap<Dir, String>,
    /// name -> file, per folder, filled on first use.
    listings: HashMap<Dir, HashMap<String, DriveFile>>,
    /// What this session wrote and trashed. Drive's listings lag behind
    /// writes for a moment; without this a lock just written could not be
    /// released, and one just released would still look taken.
    written: HashMap<Dir, HashMap<String, DriveFile>>,
    trashed: HashMap<Dir, HashSet<String>>,
    /// Sub-folders that already exist, from one listing.
    existing: Option<HashMap<String, String>>,
}

pub struct DriveFolder {
    drive: Drive,
    repo_folder: String,
    state: Mutex<State>,
}

impl DriveFolder {
    /// The repositories in the user's Drive, by folder name.
    pub fn list_repositories(drive: &Drive) -> Result<Vec<String>> {
        let app = drive.ensure_folder(APP_FOLDER, "root")?;
        let mut names: Vec<String> = drive
            .list_children(&app)?
            .into_iter()
            .filter(|f| f.mime_type.as_deref() == Some(FOLDER_MIME))
            .map(|f| f.name)
            .collect();
        names.sort();
        Ok(names)
    }

    /// Opens the repository folder `XkyHold/<name>`, creating it if missing.
    pub fn open(drive: Drive, name: &str) -> Result<Self> {
        let app = drive.ensure_folder(APP_FOLDER, "root")?;
        let repo_folder = drive.ensure_folder(name, &app)?;
        let mut state = State::default();
        state.folders.insert(Dir::Root, repo_folder.clone());
        Ok(Self { drive, repo_folder, state: Mutex::new(state) })
    }

    pub fn drive(&self) -> &Drive {
        &self.drive
    }

    fn existing(&self) -> Result<HashMap<String, String>> {
        if let Some(e) = &self.state.lock().unwrap().existing {
            return Ok(e.clone());
        }
        let map: HashMap<String, String> = self
            .drive
            .list_children(&self.repo_folder)?
            .into_iter()
            .filter(|f| f.mime_type.as_deref() == Some(FOLDER_MIME))
            .map(|f| (f.name, f.id))
            .collect();
        self.state.lock().unwrap().existing = Some(map.clone());
        Ok(map)
    }

    /// The folder id for `dir`, and whether this session just created it.
    fn folder(&self, dir: Dir) -> Result<(String, bool)> {
        if let Some(id) = self.state.lock().unwrap().folders.get(&dir) {
            return Ok((id.clone(), false));
        }
        let (id, created) = match self.existing()?.get(dir.as_str()) {
            Some(id) => (id.clone(), false),
            None => (self.drive.create_folder(dir.as_str(), &self.repo_folder)?, true),
        };
        let mut state = self.state.lock().unwrap();
        state.folders.insert(dir, id.clone());
        if created {
            state.existing.get_or_insert_with(HashMap::new).insert(dir.as_str().into(), id.clone());
        }
        Ok((id, created))
    }

    fn listing(&self, dir: Dir, fresh: bool) -> Result<HashMap<String, DriveFile>> {
        if !fresh && let Some(l) = self.state.lock().unwrap().listings.get(&dir) {
            return Ok(l.clone());
        }
        let (folder, created) = self.folder(dir)?;
        // A folder this session just created is known to be empty.
        let files = if created { Vec::new() } else { self.drive.list_children(&folder)? };
        let mut state = self.state.lock().unwrap();
        let gone = state.trashed.get(&dir).cloned().unwrap_or_default();
        let mut map = HashMap::new();
        // Drive allows duplicate names; any one will do (FORMAT.md 3).
        for f in files {
            if f.mime_type.as_deref() != Some(FOLDER_MIME) && !gone.contains(&f.name) {
                map.insert(f.name.clone(), f);
            }
        }
        for (name, f) in state.written.get(&dir).cloned().unwrap_or_default() {
            map.entry(name).or_insert(f);
        }
        state.listings.insert(dir, map.clone());
        Ok(map)
    }

    /// Resolves every folder and listing up front, so the first backup or
    /// restore does not pay for them one request at a time.
    pub fn warm(&self) -> Result<()> {
        for dir in SUBFOLDERS.into_iter().filter(|d| *d != Dir::Locks) {
            self.listing(dir, false)?;
        }
        Ok(())
    }

    fn remember(&self, dir: Dir, name: &str, file: DriveFile) {
        let mut state = self.state.lock().unwrap();
        state.listings.entry(dir).or_default().insert(name.to_owned(), file.clone());
        state.written.entry(dir).or_default().insert(name.to_owned(), file);
        if let Some(t) = state.trashed.get_mut(&dir) {
            t.remove(name);
        }
    }
}

impl Destination for DriveFolder {
    fn put(&self, dir: Dir, name: &str, data: &[u8]) -> Result<()> {
        if let Some(existing) = self.listing(dir, false)?.get(name) {
            // Everything but `config` is named after its content or a random
            // id and never changes: if it is there, it is already right.
            if dir == Dir::Root {
                self.drive.replace_content(&existing.id, data)?;
            }
            return Ok(());
        }
        let (folder, _) = self.folder(dir)?;
        let mut file = if data.len() <= SMALL_UPLOAD {
            self.drive.upload_small(name, &folder, data)?
        } else {
            self.drive.upload_resumable(name, &folder, data)?
        };
        file.size = Some(data.len().to_string());
        self.remember(dir, name, file);
        Ok(())
    }

    fn get(&self, dir: Dir, name: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>> {
        let listing = self.listing(dir, false)?;
        let file = listing.get(name).ok_or_else(|| Error::NotFound(format!("{}/{name}", dir.as_str())))?;
        self.drive.download(&file.id, range)
    }

    fn list(&self, dir: Dir, fresh: bool) -> Result<Vec<StoredObject>> {
        Ok(self
            .listing(dir, fresh)?
            .into_values()
            .map(|f| StoredObject {
                size: f.size.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0),
                sha256: f.sha256_checksum,
                name: f.name,
            })
            .collect())
    }

    fn trash(&self, dir: Dir, name: &str) -> Result<()> {
        let listing = self.listing(dir, false)?;
        let file = listing.get(name).ok_or_else(|| Error::NotFound(format!("{}/{name}", dir.as_str())))?;
        self.drive.trash(&file.id)?;
        let mut state = self.state.lock().unwrap();
        if let Some(l) = state.listings.get_mut(&dir) {
            l.remove(name);
        }
        if let Some(w) = state.written.get_mut(&dir) {
            w.remove(name);
        }
        state.trashed.entry(dir).or_default().insert(name.to_owned());
        Ok(())
    }
}
