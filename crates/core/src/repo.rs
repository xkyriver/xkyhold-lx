//! A repository on top of a [`Destination`]: config, index, blobs, trees
//! and snapshots (FORMAT.md sections 5 to 9, FORMAT-v2.md sections 3 and 5).
//!
//! Writes follow the TypeScript engine: packs are uploaded first, an index
//! file is written every few packs (the checkpoint), and a snapshot is
//! written last, only after `flush`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};

use crate::bytes::{deflate, inflate, sha256_hex};
use crate::crypto::{
    Argon2Params, Credential, RepoConfig, RepoKeys, SEAL_OVERHEAD, WRITE_FORMAT, change_password, create_repository,
    random_id, unlock,
};
use crate::destination::{Destination, Dir};
use crate::pack::{BlobEntry, BlobType, FinishedPack, PACK_TARGET_SIZE, PackWriter, read_blob};
use crate::snapshot::{Snapshot, V1_AAD_PREFIX, V2_AAD_PREFIX};
use crate::tree::{Entry, decode_tree, encode_tree};
use crate::{Error, Result};

pub const INDEX_AAD: &str = "xkyhold/v1/index";
pub const CONFIG: &str = "config";

#[derive(Debug, Serialize, Deserialize)]
struct IndexFile {
    packs: Vec<IndexPack>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexPack {
    id: String,
    blobs: Vec<BlobEntry>,
}

#[derive(Clone)]
struct Located {
    pack: String,
    entry: BlobEntry,
}

fn json<T: serde::de::DeserializeOwned>(bytes: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|e| Error::Format(format!("{what}: {e}")))
}

fn short(id: &str) -> String {
    id.chars().take(12).collect()
}

pub struct Options {
    pub pack_size: usize,
    /// Write an index file after this many packs (the backup checkpoint).
    pub index_every: usize,
    /// How many packs may be uploading at once.
    pub uploads: usize,
    /// A local folder for copies of the (encrypted) index files, so that
    /// opening a repository downloads only the new ones. One sub-folder per
    /// repository id. `$XDG_CACHE_HOME/xkyhold-lx` in the app.
    pub cache: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self { pack_size: PACK_TARGET_SIZE, index_every: 8, uploads: 3, cache: None }
    }
}

/// What a finished upload reports back.
type Uploaded = Result<(String, Vec<BlobEntry>, u64)>;

/// Upload threads: packs go out while the backup keeps reading.
struct Uploader {
    tx: Option<SyncSender<FinishedPack>>,
    results: Receiver<Uploaded>,
    workers: Vec<JoinHandle<()>>,
    in_flight: usize,
}

impl Uploader {
    fn start<D: Destination + 'static>(dest: Arc<D>, threads: usize) -> Self {
        let (tx, rx) = sync_channel::<FinishedPack>(0);
        let (done_tx, results) = std::sync::mpsc::channel::<Uploaded>();
        let rx = Arc::new(Mutex::new(rx));
        let workers = (0..threads)
            .map(|_| {
                let (rx, done, dest) = (rx.clone(), done_tx.clone(), dest.clone());
                std::thread::spawn(move || {
                    loop {
                        let next = rx.lock().unwrap().recv();
                        let Ok(pack) = next else { break };
                        let len = pack.bytes.len() as u64;
                        let outcome =
                            dest.put(Dir::Packs, &pack.name, &pack.bytes).map(|()| (pack.name, pack.blobs, len));
                        if done.send(outcome).is_err() {
                            break;
                        }
                    }
                })
            })
            .collect();
        Self { tx: Some(tx), results, workers, in_flight: 0 }
    }
}

impl Drop for Uploader {
    fn drop(&mut self) {
        self.tx.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

pub struct Repository<D: Destination + 'static> {
    pub dest: Arc<D>,
    pub keys: RepoKeys,
    pub config: RepoConfig,
    options: Options,
    index: HashMap<String, Located>,
    /// The index files this instance has read or written.
    index_names: HashSet<String>,
    writers: HashMap<BlobType, PackWriter>,
    /// Blobs in a pack still being filled: not stored yet.
    pending: HashSet<String>,
    /// Uploaded packs not yet recorded in an index file.
    unindexed: Vec<IndexPack>,
    /// Bytes of packs uploaded by this instance.
    pub uploaded_bytes: u64,
    /// In a mutex only so the repository can be shared between restore
    /// threads; it is used through `&mut self`, never locked in practice.
    uploader: Mutex<Option<Uploader>>,
    /// The first upload failure, reported at the next save or flush.
    upload_error: Option<Error>,
    /// Packs that hold data blobs. The others hold only trees and are read
    /// whole (see `tree_pack`).
    data_packs: HashSet<String>,
    /// Whole tree packs read so far, by name.
    tree_packs: Mutex<HashMap<String, Arc<Vec<u8>>>>,
}

impl<D: Destination + 'static> Repository<D> {
    fn new(dest: D, keys: RepoKeys, config: RepoConfig, options: Options) -> Self {
        Self {
            dest: Arc::new(dest),
            keys,
            config,
            options,
            index: HashMap::new(),
            index_names: HashSet::new(),
            data_packs: HashSet::new(),
            tree_packs: Mutex::new(HashMap::new()),
            writers: HashMap::new(),
            pending: HashSet::new(),
            unindexed: Vec::new(),
            uploaded_bytes: 0,
            uploader: Mutex::new(None),
            upload_error: None,
        }
    }

    pub fn read_config(dest: &D) -> Result<RepoConfig> {
        let bytes = dest.get(Dir::Root, CONFIG, None)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Format("this is not an XkyHold backup".into()))
    }

    /// Creates a format 2 repository in an empty destination.
    pub fn create(
        dest: D,
        password: &str,
        argon2: Argon2Params,
        now: String,
        options: Options,
    ) -> Result<(Self, zeroize::Zeroizing<String>)> {
        match Self::read_config(&dest) {
            Err(Error::NotFound(_)) => {}
            Ok(_) | Err(Error::Format(_)) => return Err(Error::Exists),
            Err(other) => return Err(other),
        }
        let created = create_repository(password, argon2, now)?;
        dest.put(Dir::Root, CONFIG, &serde_json::to_vec_pretty(&created.config).expect("serializable"))?;
        Ok((Self::new(dest, created.keys, created.config, options), created.recovery_key))
    }

    /// Opens an existing repository and loads its index.
    pub fn open(dest: D, credential: &Credential, options: Options) -> Result<Self> {
        let config = Self::read_config(&dest)?;
        let keys = unlock(&config, credential)?;
        let mut repo = Self::new(dest, keys, config, options);
        let loaded = repo.load_index();
        if matches!(credential, Credential::MasterKey(_)) {
            // A stored master key is only checked by what it decrypts.
            let wrong = |r: &Result<()>| matches!(r, Err(Error::Authentication(_)));
            if wrong(&loaded) {
                return Err(Error::WrongKey);
            }
            if let Some(first) = repo.dest.list(Dir::Snapshots, true)?.first()
                && wrong(&repo.load_snapshot(&first.name).map(|_| ()))
            {
                return Err(Error::WrongKey);
            }
        }
        loaded?;
        Ok(repo)
    }

    pub fn change_password(dest: &D, credential: &Credential, new_password: &str, argon2: Argon2Params) -> Result<()> {
        let config = Self::read_config(dest)?;
        if config.format != WRITE_FORMAT {
            return Err(Error::ReadOnly);
        }
        let updated = change_password(&config, credential, new_password, argon2)?;
        dest.put(Dir::Root, CONFIG, &serde_json::to_vec_pretty(&updated).expect("serializable"))
    }

    /// XkyHold-lx writes only into format 2 repositories (FORMAT-v2.md 7).
    pub fn writable(&self) -> bool {
        self.config.format == WRITE_FORMAT
    }

    fn require_writable(&self) -> Result<()> {
        if self.writable() { Ok(()) } else { Err(Error::ReadOnly) }
    }

    // --- Index ------------------------------------------------------------

    fn cache_dir(&self) -> Option<PathBuf> {
        self.options.cache.as_ref().map(|c| c.join(&self.config.repo_id).join("index"))
    }

    fn tree_cache_dir(&self) -> Option<PathBuf> {
        self.options.cache.as_ref().map(|c| c.join(&self.config.repo_id).join("trees"))
    }

    /// A whole pack of trees: from memory, the local cache (verified, since
    /// a pack is named by its SHA-256) or the destination in one request.
    /// Reading the trees of a backup one by one costs a request per folder,
    /// tens of thousands on Google Drive; their packs are only a few.
    fn tree_pack(&self, name: &str) -> Result<Arc<Vec<u8>>> {
        if let Some(pack) = self.tree_packs.lock().unwrap().get(name) {
            return Ok(pack.clone());
        }
        let cache = self.tree_cache_dir();
        let cached = cache.as_ref().and_then(|c| std::fs::read(c.join(name)).ok()).filter(|b| sha256_hex(b) == name);
        let bytes = match cached {
            Some(bytes) => bytes,
            None => {
                let bytes = self.dest.get(Dir::Packs, name, None)?;
                if sha256_hex(&bytes) != name {
                    return Err(Error::Authentication("pack does not match its name"));
                }
                if let Some(dir) = cache {
                    // Best effort: a cache that cannot be written only costs downloads.
                    let _ = std::fs::create_dir_all(&dir).and_then(|()| {
                        let temp = dir.join(format!(".tmp-{}", random_id()));
                        std::fs::write(&temp, &bytes)?;
                        std::fs::rename(&temp, dir.join(name))
                    });
                }
                bytes
            }
        };
        let pack = Arc::new(bytes);
        self.tree_packs.lock().unwrap().insert(name.to_owned(), pack.clone());
        Ok(pack)
    }

    /// Drops cached tree packs the index no longer mentions (pruned).
    fn prune_tree_cache(&self) {
        let Some(dir) = self.tree_cache_dir() else { return };
        let Ok(items) = std::fs::read_dir(&dir) else { return };
        let live: HashSet<&str> = self.index.values().map(|l| l.pack.as_str()).collect();
        for item in items.flatten() {
            if !live.contains(item.file_name().to_string_lossy().as_ref()) {
                let _ = std::fs::remove_file(item.path());
            }
        }
    }

    /// An index file, from the local cache when it holds a verified copy.
    fn index_file(&self, name: &str) -> Result<Vec<u8>> {
        let cache = self.cache_dir();
        if let Some(cached) = cache.as_ref().and_then(|c| std::fs::read(c.join(name)).ok())
            && sha256_hex(&cached) == name
        {
            return Ok(cached);
        }
        let sealed = self.dest.get(Dir::Index, name, None)?;
        if let Some(dir) = cache {
            // Best effort: a cache that cannot be written only costs downloads.
            let _ = std::fs::create_dir_all(&dir).and_then(|()| {
                let temp = dir.join(format!(".tmp-{}", random_id()));
                std::fs::write(&temp, &sealed)?;
                std::fs::rename(&temp, dir.join(name))
            });
        }
        Ok(sealed)
    }

    fn prune_cache(&self, keep: &[String]) {
        let Some(dir) = self.cache_dir() else { return };
        let Ok(items) = std::fs::read_dir(&dir) else { return };
        for item in items.flatten() {
            if !keep.iter().any(|k| *k == item.file_name().to_string_lossy()) {
                let _ = std::fs::remove_file(item.path());
            }
        }
    }

    pub fn load_index(&mut self) -> Result<()> {
        let names: Vec<String> = self.dest.list(Dir::Index, true)?.into_iter().map(|o| o.name).collect();
        let mut index = HashMap::new();
        for name in &names {
            let sealed = self.index_file(name)?;
            let file: IndexFile = json(&inflate(&self.keys.open(&sealed, INDEX_AAD)?)?, "index")?;
            for pack in file.packs {
                for entry in pack.blobs {
                    index.insert(entry.id.clone(), Located { pack: pack.id.clone(), entry });
                }
            }
        }
        self.data_packs =
            index.values().filter(|l: &&Located| l.entry.kind == BlobType::Data).map(|l| l.pack.clone()).collect();
        self.index = index;
        self.prune_cache(&names);
        self.prune_tree_cache();
        self.index_names = names.into_iter().collect();
        Ok(())
    }

    /// Re-reads the index if another device changed it since this instance
    /// last looked (FORMAT.md 10.4).
    pub fn refresh_index(&mut self) -> Result<()> {
        let names: HashSet<String> = self.dest.list(Dir::Index, true)?.into_iter().map(|o| o.name).collect();
        if names != self.index_names { self.load_index() } else { Ok(()) }
    }

    /// Is this blob stored, or in a pack being filled?
    pub fn has(&self, id: &str) -> bool {
        self.index.contains_key(id) || self.pending.contains(id)
    }

    pub fn blob_count(&self) -> usize {
        self.index.len()
    }

    /// Where a blob is stored: (pack name, entry).
    pub fn locate(&self, id: &str) -> Option<(&str, &BlobEntry)> {
        self.index.get(id).map(|l| (l.pack.as_str(), &l.entry))
    }

    /// The index grouped by pack.
    pub fn packs(&self) -> HashMap<String, Vec<BlobEntry>> {
        let mut packs: HashMap<String, Vec<BlobEntry>> = HashMap::new();
        for located in self.index.values() {
            packs.entry(located.pack.clone()).or_default().push(located.entry.clone());
        }
        packs
    }

    /// Forgets every blob that lives in one of these packs.
    pub fn drop_packs(&mut self, names: &HashSet<String>) {
        self.index.retain(|_, l| !names.contains(&l.pack));
        self.data_packs.retain(|p| !names.contains(p));
        self.tree_packs.lock().unwrap().retain(|p, _| !names.contains(p));
    }

    fn seal_index(&self, file: &IndexFile) -> (String, Vec<u8>) {
        let sealed = self.keys.seal(&deflate(&serde_json::to_vec(file).expect("serializable")), INDEX_AAD);
        (sha256_hex(&sealed), sealed)
    }

    fn write_index(&mut self) -> Result<()> {
        if self.unindexed.is_empty() {
            return Ok(());
        }
        let file = IndexFile { packs: std::mem::take(&mut self.unindexed) };
        let (name, sealed) = self.seal_index(&file);
        if let Err(err) = self.dest.put(Dir::Index, &name, &sealed) {
            self.unindexed = file.packs;
            return Err(err);
        }
        self.index_names.insert(name);
        Ok(())
    }

    /// Replaces all index files with one describing what this instance
    /// knows. The new file is written before any old one is trashed.
    pub fn consolidate_index(&mut self) -> Result<()> {
        if !self.pending.is_empty() || !self.unindexed.is_empty() {
            return Err(Error::Bug("flush() must run before consolidate_index()"));
        }
        let old: Vec<String> = self.dest.list(Dir::Index, true)?.into_iter().map(|o| o.name).collect();
        let mut packs: Vec<IndexPack> = self.packs().into_iter().map(|(id, blobs)| IndexPack { id, blobs }).collect();
        packs.sort_by(|a, b| a.id.cmp(&b.id));
        let (name, sealed) = self.seal_index(&IndexFile { packs });
        self.dest.put(Dir::Index, &name, &sealed)?;
        self.index_names.insert(name.clone());
        for other in old.into_iter().filter(|o| *o != name) {
            self.dest.trash(Dir::Index, &other)?;
            self.index_names.remove(&other);
        }
        Ok(())
    }

    // --- Blobs ------------------------------------------------------------

    /// Stores a blob unless the repository already has it. Returns its id.
    fn throw_if_upload_failed(&mut self) -> Result<()> {
        match self.upload_error.take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    pub fn save_blob(&mut self, kind: BlobType, plaintext: &[u8]) -> Result<String> {
        self.require_writable()?;
        self.throw_if_upload_failed()?;
        let id = self.keys.blob_id(plaintext);
        if !self.has(&id) {
            self.append(kind, &id, plaintext)?;
        }
        Ok(id)
    }

    /// Writes a blob the repository already has into a new pack (prune).
    pub fn rewrite_blob(&mut self, kind: BlobType, id: &str, plaintext: &[u8]) -> Result<()> {
        self.require_writable()?;
        if !self.pending.contains(id) {
            self.append(kind, id, plaintext)?;
        }
        Ok(())
    }

    fn append(&mut self, kind: BlobType, id: &str, plaintext: &[u8]) -> Result<()> {
        if let Some(writer) = self.writers.get(&kind)
            && writer.count() > 0
            && writer.size() + plaintext.len() + SEAL_OVERHEAD > self.options.pack_size
        {
            self.close_pack(kind)?;
        }
        self.pending.insert(id.to_owned());
        self.writers.entry(kind).or_default().add(&self.keys, kind, id, plaintext);
        Ok(())
    }

    /// Takes in a finished upload. Only now are its blobs stored.
    fn uploaded(&mut self, outcome: Uploaded) {
        match outcome {
            Ok((name, blobs, len)) => {
                self.uploaded_bytes += len;
                for blob in &blobs {
                    if blob.kind == BlobType::Data {
                        self.data_packs.insert(name.clone());
                    }
                    self.index.insert(blob.id.clone(), Located { pack: name.clone(), entry: blob.clone() });
                    self.pending.remove(&blob.id);
                }
                self.unindexed.push(IndexPack { id: name, blobs });
                if self.unindexed.len() >= self.options.index_every
                    && let Err(err) = self.write_index()
                {
                    self.upload_error.get_or_insert(err);
                }
            }
            Err(err) => {
                self.upload_error.get_or_insert(err);
            }
        }
    }

    /// Waits for one upload to finish, if any is running.
    fn wait_one(&mut self) {
        let Some(up) = self.uploader.get_mut().unwrap().as_mut() else { return };
        if up.in_flight == 0 {
            return;
        }
        let outcome = up.results.recv().unwrap_or(Err(Error::Bug("upload thread stopped")));
        up.in_flight -= 1;
        self.uploaded(outcome);
    }

    /// Takes in uploads that already finished, without waiting.
    fn collect_finished(&mut self) {
        while let Some(up) = self.uploader.get_mut().unwrap().as_mut() {
            match up.results.try_recv() {
                Ok(outcome) => {
                    up.in_flight -= 1;
                    self.uploaded(outcome);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }

    fn close_pack(&mut self, kind: BlobType) -> Result<()> {
        let Some(writer) = self.writers.remove(&kind) else { return Ok(()) };
        if writer.count() == 0 {
            return Ok(());
        }
        let pack = writer.finish(&self.keys);
        if self.options.uploads <= 1 {
            let len = pack.bytes.len() as u64;
            let outcome = self.dest.put(Dir::Packs, &pack.name, &pack.bytes).map(|()| (pack.name, pack.blobs, len));
            self.uploaded(outcome);
            return self.throw_if_upload_failed();
        }
        if self.uploader.get_mut().unwrap().is_none() {
            *self.uploader.get_mut().unwrap() = Some(Uploader::start(self.dest.clone(), self.options.uploads));
        }
        self.collect_finished();
        while self.uploader.get_mut().unwrap().as_ref().is_some_and(|u| u.in_flight >= self.options.uploads) {
            self.wait_one();
        }
        self.throw_if_upload_failed()?;
        let up = self.uploader.get_mut().unwrap().as_mut().expect("started above");
        up.tx.as_ref().expect("open").send(pack).map_err(|_| Error::Bug("upload thread stopped"))?;
        up.in_flight += 1;
        Ok(())
    }

    fn wait_all(&mut self) {
        while self.uploader.get_mut().unwrap().as_ref().is_some_and(|u| u.in_flight > 0) {
            self.wait_one();
        }
    }

    /// Uploads every open pack, waits for all uploads, and writes the index.
    pub fn flush(&mut self) -> Result<()> {
        self.close_pack(BlobType::Data)?;
        self.close_pack(BlobType::Tree)?;
        self.wait_all();
        self.throw_if_upload_failed()?;
        self.write_index()
    }

    /// Forgets blobs that were queued but never stored. Must be called when
    /// a backup fails, or `has` would claim data that is not there.
    ///
    /// Packs whose upload finished are still written to the index (best
    /// effort), so a stopped backup does not send them again next time.
    pub fn abandon(&mut self) {
        self.writers.clear();
        self.wait_all();
        self.pending.clear();
        self.upload_error = None;
        if !self.unindexed.is_empty() {
            let _ = self.write_index();
        }
    }

    pub fn load_blob(&self, kind: BlobType, id: &str) -> Result<Vec<u8>> {
        let located = self.index.get(id).ok_or_else(|| Error::MissingBlob(short(id)))?;
        if located.entry.kind != kind {
            return Err(Error::Authentication("blob has the wrong type"));
        }
        let missing = |e| match e {
            Error::NotFound(_) => Error::MissingBlob(short(id)),
            e => e,
        };
        if kind == BlobType::Tree && !self.data_packs.contains(&located.pack) {
            let pack = self.tree_pack(&located.pack).map_err(missing)?;
            let (start, end) = (located.entry.offset as usize, (located.entry.offset + located.entry.length) as usize);
            let sealed = pack.get(start..end).ok_or(Error::Authentication("blob lies outside its pack"))?;
            return read_blob(&self.keys, &located.entry, sealed);
        }
        let range = Some((located.entry.offset, located.entry.length));
        let sealed = self.dest.get(Dir::Packs, &located.pack, range).map_err(missing)?;
        read_blob(&self.keys, &located.entry, &sealed)
    }

    // --- Trees ------------------------------------------------------------

    pub fn save_tree(&mut self, entries: &mut [Entry]) -> Result<String> {
        let bytes = encode_tree(entries);
        self.save_blob(BlobType::Tree, &bytes)
    }

    pub fn load_tree(&self, id: &str) -> Result<Vec<Entry>> {
        decode_tree(&self.load_blob(BlobType::Tree, id)?)
    }

    // --- Snapshots --------------------------------------------------------

    /// Writes a version 2 snapshot (sealed with the v2 AAD). Everything it
    /// refers to must already be flushed.
    pub fn save_snapshot(&mut self, mut snapshot: Snapshot) -> Result<Snapshot> {
        self.require_writable()?;
        if !self.pending.is_empty() || !self.unindexed.is_empty() {
            return Err(Error::Bug("flush() must run before save_snapshot()"));
        }
        snapshot.format = Some(2);
        snapshot.id = random_id();
        let plain = deflate(&serde_json::to_vec(&snapshot).expect("serializable"));
        let sealed = self.keys.seal(&plain, &format!("{V2_AAD_PREFIX}{}", snapshot.id));
        self.dest.put(Dir::Snapshots, &snapshot.id, &sealed)?;
        Ok(snapshot)
    }

    /// Opens a snapshot with the v2 AAD first, then the v1 AAD
    /// (FORMAT-v2.md 3.2).
    pub fn load_snapshot(&self, id: &str) -> Result<Snapshot> {
        let sealed = self.dest.get(Dir::Snapshots, id, None)?;
        let (version, plain) = match self.keys.open(&sealed, &format!("{V2_AAD_PREFIX}{id}")) {
            Ok(plain) => (2, plain),
            Err(_) => (1, self.keys.open(&sealed, &format!("{V1_AAD_PREFIX}{id}"))?),
        };
        let snapshot: Snapshot = json(&inflate(&plain)?, "snapshot")?;
        if snapshot.version() != version || snapshot.id != id {
            return Err(Error::Authentication("snapshot does not match how it was sealed"));
        }
        Ok(snapshot)
    }

    /// Moves a snapshot to the trash. Its data stays until prune runs.
    pub fn trash_snapshot(&self, id: &str) -> Result<()> {
        self.require_writable()?;
        self.dest.trash(Dir::Snapshots, id)
    }

    /// All snapshots, oldest first.
    pub fn list_snapshots(&self) -> Result<Vec<Snapshot>> {
        let mut snapshots = self
            .dest
            .list(Dir::Snapshots, true)?
            .iter()
            .map(|o| o.name.clone())
            .collect::<HashSet<_>>()
            .iter()
            .map(|id| self.load_snapshot(id))
            .collect::<Result<Vec<_>>>()?;
        snapshots.sort_by(|a, b| a.time.cmp(&b.time));
        Ok(snapshots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::MemoryDestination;

    fn assert_sync<T: Sync>() {}

    #[test]
    fn a_repository_can_be_shared_between_threads() {
        assert_sync::<Repository<MemoryDestination>>();
    }
}
