//! A repository on top of a [`Destination`]: config, index, blobs, trees
//! and snapshots (FORMAT.md sections 5 to 9, FORMAT-v2.md sections 3 and 5).
//!
//! Writes follow the TypeScript engine: packs are uploaded first, an index
//! file is written every few packs (the checkpoint), and a snapshot is
//! written last, only after `flush`.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::bytes::{deflate, inflate, sha256_hex};
use crate::crypto::{
    Argon2Params, Credential, RepoConfig, RepoKeys, SEAL_OVERHEAD, WRITE_FORMAT, change_password, create_repository,
    random_id, unlock,
};
use crate::destination::{Destination, Dir};
use crate::pack::{BlobEntry, BlobType, PACK_TARGET_SIZE, PackWriter, read_blob};
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
}

impl Default for Options {
    fn default() -> Self {
        Self { pack_size: PACK_TARGET_SIZE, index_every: 8 }
    }
}

pub struct Repository<D: Destination> {
    pub dest: D,
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
}

impl<D: Destination> Repository<D> {
    fn new(dest: D, keys: RepoKeys, config: RepoConfig, options: Options) -> Self {
        Self {
            dest,
            keys,
            config,
            options,
            index: HashMap::new(),
            index_names: HashSet::new(),
            writers: HashMap::new(),
            pending: HashSet::new(),
            unindexed: Vec::new(),
            uploaded_bytes: 0,
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
        repo.load_index()?;
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

    pub fn load_index(&mut self) -> Result<()> {
        let names: Vec<String> = self.dest.list(Dir::Index, true)?.into_iter().map(|o| o.name).collect();
        let mut index = HashMap::new();
        for name in &names {
            let sealed = self.dest.get(Dir::Index, name, None)?;
            let file: IndexFile = json(&inflate(&self.keys.open(&sealed, INDEX_AAD)?)?, "index")?;
            for pack in file.packs {
                for entry in pack.blobs {
                    index.insert(entry.id.clone(), Located { pack: pack.id.clone(), entry });
                }
            }
        }
        self.index = index;
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
    pub fn save_blob(&mut self, kind: BlobType, plaintext: &[u8]) -> Result<String> {
        self.require_writable()?;
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

    fn close_pack(&mut self, kind: BlobType) -> Result<()> {
        let Some(writer) = self.writers.remove(&kind) else { return Ok(()) };
        if writer.count() == 0 {
            return Ok(());
        }
        let pack = writer.finish(&self.keys);
        self.dest.put(Dir::Packs, &pack.name, &pack.bytes)?;
        // Only now are these blobs stored.
        self.uploaded_bytes += pack.bytes.len() as u64;
        for blob in &pack.blobs {
            self.index.insert(blob.id.clone(), Located { pack: pack.name.clone(), entry: blob.clone() });
            self.pending.remove(&blob.id);
        }
        self.unindexed.push(IndexPack { id: pack.name, blobs: pack.blobs });
        if self.unindexed.len() >= self.options.index_every {
            self.write_index()?;
        }
        Ok(())
    }

    /// Uploads every open pack and writes the index.
    pub fn flush(&mut self) -> Result<()> {
        self.close_pack(BlobType::Data)?;
        self.close_pack(BlobType::Tree)?;
        self.write_index()
    }

    /// Forgets blobs that were queued but never stored. Must be called when
    /// a backup fails, or `has` would claim data that is not there.
    pub fn abandon(&mut self) {
        self.writers.clear();
        self.pending.clear();
    }

    pub fn load_blob(&self, kind: BlobType, id: &str) -> Result<Vec<u8>> {
        let located = self.index.get(id).ok_or_else(|| Error::MissingBlob(short(id)))?;
        if located.entry.kind != kind {
            return Err(Error::Authentication("blob has the wrong type"));
        }
        let sealed = match self.dest.get(Dir::Packs, &located.pack, Some((located.entry.offset, located.entry.length)))
        {
            Err(Error::NotFound(_)) => return Err(Error::MissingBlob(short(id))),
            other => other?,
        };
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
