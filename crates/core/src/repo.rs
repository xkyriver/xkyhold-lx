//! Reading a repository: config, index, snapshots and trees (FORMAT.md,
//! sections 7 to 9). Writing comes with the format v2 work in Phase 2.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::bytes::inflate;
use crate::crypto::{Credential, RepoConfig, RepoKeys, unlock};
use crate::destination::{Destination, Dir};
use crate::pack::{BlobEntry, BlobType, read_blob};
use crate::{Error, Result};

pub const INDEX_AAD: &str = "xkyhold/v1/index";
pub const CONFIG: &str = "config";

pub fn snapshot_aad(id: &str) -> String {
    format!("xkyhold/v1/snapshot/{id}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TreeEntry {
    File {
        name: String,
        size: u64,
        /// Milliseconds since the Unix epoch.
        mtime: i64,
        content: Vec<String>,
    },
    Dir {
        name: String,
        subtree: String,
    },
}

impl TreeEntry {
    pub fn name(&self) -> &str {
        match self {
            TreeEntry::File { name, .. } | TreeEntry::Dir { name, .. } => name,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Tree {
    entries: Vec<TreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRoot {
    pub name: String,
    pub tree: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotStats {
    pub files: u64,
    pub bytes: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub time: String,
    pub device: String,
    pub roots: Vec<SnapshotRoot>,
    pub stats: SnapshotStats,
    pub parent: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IndexFile {
    packs: Vec<IndexPack>,
}

#[derive(Debug, Deserialize)]
struct IndexPack {
    id: String,
    blobs: Vec<BlobEntry>,
}

struct Located {
    pack: String,
    entry: BlobEntry,
}

fn json<T: serde::de::DeserializeOwned>(bytes: &[u8], what: &str) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|e| Error::Format(format!("{what}: {e}")))
}

pub struct Repository<D: Destination> {
    pub dest: D,
    pub keys: RepoKeys,
    pub config: RepoConfig,
    index: HashMap<String, Located>,
}

impl<D: Destination> Repository<D> {
    pub fn read_config(dest: &D) -> Result<RepoConfig> {
        let bytes = dest.get(Dir::Root, CONFIG, None)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Format("this is not an XkyHold backup".into()))
    }

    /// Opens an existing repository and loads its index.
    pub fn open(dest: D, credential: &Credential) -> Result<Self> {
        let config = Self::read_config(&dest)?;
        let keys = unlock(&config, credential)?;
        let mut repo = Self { dest, keys, config, index: HashMap::new() };
        repo.load_index()?;
        Ok(repo)
    }

    pub fn load_index(&mut self) -> Result<()> {
        self.index.clear();
        for object in self.dest.list(Dir::Index, true)? {
            let sealed = self.dest.get(Dir::Index, &object.name, None)?;
            let file: IndexFile = json(&inflate(&self.keys.open(&sealed, INDEX_AAD)?)?, "index")?;
            for pack in file.packs {
                for entry in pack.blobs {
                    self.index.insert(entry.id.clone(), Located { pack: pack.id.clone(), entry });
                }
            }
        }
        Ok(())
    }

    pub fn blob_count(&self) -> usize {
        self.index.len()
    }

    pub fn load_blob(&self, kind: BlobType, id: &str) -> Result<Vec<u8>> {
        let located = self.index.get(id).ok_or_else(|| Error::MissingBlob(id[..12.min(id.len())].into()))?;
        if located.entry.kind != kind {
            return Err(Error::Authentication("blob has the wrong type"));
        }
        let sealed = match self.dest.get(Dir::Packs, &located.pack, Some((located.entry.offset, located.entry.length)))
        {
            Err(Error::NotFound(_)) => return Err(Error::MissingBlob(id[..12.min(id.len())].into())),
            other => other?,
        };
        read_blob(&self.keys, &located.entry, &sealed)
    }

    pub fn load_tree(&self, id: &str) -> Result<Vec<TreeEntry>> {
        Ok(json::<Tree>(&self.load_blob(BlobType::Tree, id)?, "tree")?.entries)
    }

    pub fn load_snapshot(&self, id: &str) -> Result<Snapshot> {
        let sealed = self.dest.get(Dir::Snapshots, id, None)?;
        json(&inflate(&self.keys.open(&sealed, &snapshot_aad(id))?)?, "snapshot")
    }

    /// All snapshots, oldest first.
    pub fn list_snapshots(&self) -> Result<Vec<Snapshot>> {
        let mut snapshots = self
            .dest
            .list(Dir::Snapshots, true)?
            .iter()
            .map(|o| self.load_snapshot(&o.name))
            .collect::<Result<Vec<_>>>()?;
        snapshots.sort_by(|a, b| a.time.cmp(&b.time));
        Ok(snapshots)
    }
}
