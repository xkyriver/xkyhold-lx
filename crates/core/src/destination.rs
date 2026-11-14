//! Where a repository lives. The engine only talks to this trait; Google
//! Drive and a local folder are implementations, the in-memory one is for
//! tests.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::{Error, Result};

/// The folders of a repository (`Root` is where `config` lives).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dir {
    Root,
    Packs,
    Index,
    Snapshots,
    Locks,
}

impl Dir {
    pub fn as_str(self) -> &'static str {
        match self {
            Dir::Root => "",
            Dir::Packs => "packs",
            Dir::Index => "index",
            Dir::Snapshots => "snapshots",
            Dir::Locks => "locks",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "" => Dir::Root,
            "packs" => Dir::Packs,
            "index" => Dir::Index,
            "snapshots" => Dir::Snapshots,
            "locks" => Dir::Locks,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub name: String,
    pub size: u64,
    /// Hex SHA-256 of the content, when the destination can report it.
    pub sha256: Option<String>,
}

pub trait Destination: Send + Sync {
    fn put(&self, dir: Dir, name: &str, data: &[u8]) -> Result<()>;
    /// Reads a whole object, or `length` bytes from `offset`.
    fn get(&self, dir: Dir, name: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>>;
    /// `fresh` asks the storage again instead of trusting a cache.
    fn list(&self, dir: Dir, fresh: bool) -> Result<Vec<StoredObject>>;
    /// Moves an object to the trash. Never deletes permanently.
    fn trash(&self, dir: Dir, name: &str) -> Result<()>;
}

impl<T: Destination + ?Sized> Destination for std::sync::Arc<T> {
    fn put(&self, dir: Dir, name: &str, data: &[u8]) -> Result<()> {
        (**self).put(dir, name, data)
    }
    fn get(&self, dir: Dir, name: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>> {
        (**self).get(dir, name, range)
    }
    fn list(&self, dir: Dir, fresh: bool) -> Result<Vec<StoredObject>> {
        (**self).list(dir, fresh)
    }
    fn trash(&self, dir: Dir, name: &str) -> Result<()> {
        (**self).trash(dir, name)
    }
}

fn not_found(dir: Dir, name: &str) -> Error {
    if dir == Dir::Root {
        Error::NotFound(name.to_owned())
    } else {
        Error::NotFound(format!("{}/{name}", dir.as_str()))
    }
}

/// An in-memory destination for tests.
#[derive(Default)]
pub struct MemoryDestination {
    pub objects: Mutex<BTreeMap<(Dir, String), Vec<u8>>>,
    pub trashed: Mutex<Vec<(Dir, String)>>,
}

impl Destination for MemoryDestination {
    fn put(&self, dir: Dir, name: &str, data: &[u8]) -> Result<()> {
        self.objects.lock().unwrap().insert((dir, name.to_owned()), data.to_vec());
        Ok(())
    }

    fn get(&self, dir: Dir, name: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>> {
        let objects = self.objects.lock().unwrap();
        let data = objects.get(&(dir, name.to_owned())).ok_or_else(|| not_found(dir, name))?;
        Ok(match range {
            None => data.clone(),
            Some((offset, length)) => {
                let start = usize::try_from(offset).unwrap_or(usize::MAX).min(data.len());
                let end = start.saturating_add(usize::try_from(length).unwrap_or(usize::MAX)).min(data.len());
                data[start..end].to_vec()
            }
        })
    }

    fn list(&self, dir: Dir, _fresh: bool) -> Result<Vec<StoredObject>> {
        Ok(self
            .objects
            .lock()
            .unwrap()
            .iter()
            .filter(|((d, _), _)| *d == dir)
            .map(|((_, name), data)| StoredObject {
                name: name.clone(),
                size: data.len() as u64,
                sha256: Some(crate::bytes::sha256_hex(data)),
            })
            .collect())
    }

    fn trash(&self, dir: Dir, name: &str) -> Result<()> {
        self.objects.lock().unwrap().remove(&(dir, name.to_owned())).ok_or_else(|| not_found(dir, name))?;
        self.trashed.lock().unwrap().push((dir, name.to_owned()));
        Ok(())
    }
}
