//! Pack files (FORMAT.md, sections 5 and 6): encrypted blobs back to back,
//! followed by an encrypted header that lists them.

use serde::{Deserialize, Serialize};

use crate::bytes::{deflate, inflate};
use crate::crypto::RepoKeys;
use crate::{Error, Result};

pub const MAGIC: &[u8; 4] = b"XKH1";
pub const HEADER_AAD: &str = "xkyhold/v1/pack-header";
const LENGTH_FIELD: usize = 4;
pub const PACK_TARGET_SIZE: usize = 16 * 1024 * 1024;

pub const ENCODING_RAW: u32 = 0;
pub const ENCODING_DEFLATE: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlobType {
    Data,
    Tree,
}

impl BlobType {
    pub fn as_str(self) -> &'static str {
        match self {
            BlobType::Data => "data",
            BlobType::Tree => "tree",
        }
    }
}

/// Where a blob sits inside a pack. Field order matches what the
/// TypeScript engine writes, so headers and indexes serialize the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: BlobType,
    /// Position of the encrypted blob inside the pack.
    pub offset: u64,
    /// Length of the encrypted blob.
    pub length: u64,
    /// Length of the plaintext.
    pub raw_length: u64,
    pub encoding: u32,
}

pub fn blob_aad(kind: BlobType, id: &str) -> String {
    format!("xkyhold/v1/blob/{}/{id}", kind.as_str())
}

#[derive(Serialize, Deserialize)]
struct Header {
    blobs: Vec<BlobEntry>,
}

/// Reads the blob list from a whole pack. Used to rebuild a lost index.
pub fn read_pack_header(keys: &RepoKeys, pack: &[u8]) -> Result<Vec<BlobEntry>> {
    if pack.len() < MAGIC.len() + LENGTH_FIELD || &pack[..MAGIC.len()] != MAGIC {
        return Err(Error::Format("this is not an XkyHold pack".into()));
    }
    let tail = pack.len() - LENGTH_FIELD;
    let header_length = u32::from_le_bytes(pack[tail..].try_into().expect("4 bytes")) as usize;
    let start = tail
        .checked_sub(header_length)
        .filter(|&s| s >= MAGIC.len())
        .ok_or_else(|| Error::Format("pack header is damaged".into()))?;
    let json = inflate(&keys.open(&pack[start..tail], HEADER_AAD)?)?;
    let header: Header = serde_json::from_slice(&json).map_err(|e| Error::Format(format!("pack header: {e}")))?;
    Ok(header.blobs)
}

/// Decrypts one blob. `sealed` is exactly bytes `[offset, offset + length)`
/// of the pack. The plaintext is checked against the blob id before it is
/// returned, so a caller never sees unverified data.
pub fn read_blob(keys: &RepoKeys, entry: &BlobEntry, sealed: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() as u64 != entry.length {
        return Err(Error::Authentication("blob has the wrong length"));
    }
    let body = keys.open(sealed, &blob_aad(entry.kind, &entry.id))?;
    let plaintext = match entry.encoding {
        ENCODING_RAW => body,
        ENCODING_DEFLATE => inflate(&body)?,
        other => return Err(Error::Format(format!("unknown blob encoding {other}"))),
    };
    if plaintext.len() as u64 != entry.raw_length || keys.blob_id(&plaintext) != entry.id {
        return Err(Error::Authentication("blob content does not match its id"));
    }
    Ok(plaintext)
}

/// Encodes a blob body for a pack: trees are compressed, data is not.
pub fn encode_body(kind: BlobType, plaintext: &[u8]) -> (u32, Vec<u8>) {
    match kind {
        BlobType::Tree => (ENCODING_DEFLATE, deflate(plaintext)),
        BlobType::Data => (ENCODING_RAW, plaintext.to_vec()),
    }
}
