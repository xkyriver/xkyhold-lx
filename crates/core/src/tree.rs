//! Tree entries of format versions 1 and 2 (FORMAT.md section 7,
//! FORMAT-v2.md section 4).
//!
//! Entries go through `serde_json::Value`, whose maps keep keys sorted, so
//! the same entries always serialize to the same bytes and an unchanged
//! folder keeps its blob id.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::bytes::{from_base64, to_base64};
use crate::{Error, Result};

/// One extended attribute (FORMAT-v2.md 4.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Xattr {
    pub name: String,
    /// Base64 of the value.
    pub value: String,
}

/// Change-detection data for the next backup (FORMAT-v2.md 4.6).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub ctime: i64,
    pub ctime_nanos: u32,
    /// Decimal string: inode numbers can exceed 2^53.
    pub ino: String,
}

/// Metadata common to every entry type (FORMAT-v2.md 4.1). All optional:
/// a version 1 entry has only `mtime`, and only on files.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime_nanos: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xattrs: Option<Vec<Xattr>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<Change>,
}

impl Meta {
    /// Splits nanoseconds since the epoch into `mtime` (ms, rounded towards
    /// minus infinity) and `mtimeNanos` (0–999 999).
    pub fn set_time_ns(&mut self, ns: i128) {
        let (ms, rest) = split_ns(ns);
        self.mtime = Some(ms);
        self.mtime_nanos = (rest != 0).then_some(rest);
    }

    /// The modification time in nanoseconds since the epoch, if known.
    pub fn time_ns(&self) -> Option<i128> {
        self.mtime.map(|ms| i128::from(ms) * 1_000_000 + i128::from(self.mtime_nanos.unwrap_or(0)))
    }
}

/// (milliseconds rounded towards minus infinity, remaining nanoseconds).
pub fn split_ns(ns: i128) -> (i64, u32) {
    let ms = ns.div_euclid(1_000_000);
    let rest = ns.rem_euclid(1_000_000);
    (i64::try_from(ms).expect("time in range"), rest as u32)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    File {
        size: u64,
        content: Vec<String>,
    },
    Dir {
        subtree: String,
        /// A mount point that was not crossed (FORMAT-v2.md 4.9).
        mount: bool,
    },
    Symlink {
        /// Raw bytes of the target.
        target: Vec<u8>,
    },
    /// A type this reader does not know (FORMAT-v2.md 4.7).
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The exact name on disk, as bytes.
    pub name: Vec<u8>,
    pub kind: EntryKind,
    pub meta: Meta,
}

impl Entry {
    /// The display form of the name (FORMAT-v2.md 4.1).
    pub fn display_name(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }

    fn to_value(&self) -> Value {
        let mut map = match serde_json::to_value(&self.meta).expect("serializable") {
            Value::Object(map) => map,
            _ => unreachable!("Meta is a struct"),
        };
        put_bytes(&mut map, "name", "nameBytes", &self.name);
        match &self.kind {
            EntryKind::File { size, content } => {
                map.insert("type".into(), json!("file"));
                map.insert("size".into(), json!(size));
                map.insert("content".into(), json!(content));
            }
            EntryKind::Dir { subtree, mount } => {
                map.insert("type".into(), json!("dir"));
                map.insert("subtree".into(), json!(subtree));
                if *mount {
                    map.insert("mount".into(), json!(true));
                }
            }
            EntryKind::Symlink { target } => {
                map.insert("type".into(), json!("symlink"));
                put_bytes(&mut map, "target", "targetBytes", target);
            }
            EntryKind::Unknown(_) => unreachable!("unknown entries are never written"),
        }
        Value::Object(map)
    }

    fn from_value(value: Value) -> Result<Self> {
        let bad = |what: &str| Error::Format(format!("tree entry: {what}"));
        let Value::Object(map) = value else { return Err(bad("not an object")) };
        let name = take_bytes(&map, "name", "nameBytes")?.ok_or_else(|| bad("no name"))?;
        let kind_name = map.get("type").and_then(Value::as_str).ok_or_else(|| bad("no type"))?;
        let kind = match kind_name {
            "file" => EntryKind::File {
                size: map.get("size").and_then(Value::as_u64).ok_or_else(|| bad("file without size"))?,
                content: serde_json::from_value(map.get("content").cloned().ok_or_else(|| bad("no content"))?)
                    .map_err(|e| bad(&e.to_string()))?,
            },
            "dir" => EntryKind::Dir {
                subtree: map.get("subtree").and_then(Value::as_str).ok_or_else(|| bad("no subtree"))?.to_owned(),
                mount: map.get("mount").and_then(Value::as_bool).unwrap_or(false),
            },
            "symlink" => EntryKind::Symlink {
                target: take_bytes(&map, "target", "targetBytes")?.ok_or_else(|| bad("symlink without target"))?,
            },
            other => EntryKind::Unknown(other.to_owned()),
        };
        let meta: Meta = serde_json::from_value(Value::Object(map)).map_err(|e| bad(&e.to_string()))?;
        Ok(Entry { name, kind, meta })
    }
}

fn put_bytes(map: &mut Map<String, Value>, text_key: &str, bytes_key: &str, bytes: &[u8]) {
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            map.insert(text_key.into(), json!(text));
        }
        Err(_) => {
            map.insert(text_key.into(), json!(String::from_utf8_lossy(bytes)));
            map.insert(bytes_key.into(), json!(to_base64(bytes)));
        }
    }
}

fn take_bytes(map: &Map<String, Value>, text_key: &str, bytes_key: &str) -> Result<Option<Vec<u8>>> {
    if let Some(b64) = map.get(bytes_key).and_then(Value::as_str) {
        return from_base64(b64).map(Some);
    }
    Ok(map.get(text_key).and_then(Value::as_str).map(|t| t.as_bytes().to_vec()))
}

/// A name that is safe to create inside a folder (FORMAT-v2.md 4.1).
pub fn valid_name(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.contains(&b'/') && !name.contains(&0)
}

/// Serializes a tree, entries sorted by name bytes.
pub fn encode_tree(entries: &mut [Entry]) -> Vec<u8> {
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let list: Vec<Value> = entries.iter().map(Entry::to_value).collect();
    serde_json::to_vec(&json!({ "entries": list })).expect("serializable")
}

pub fn decode_tree(bytes: &[u8]) -> Result<Vec<Entry>> {
    #[derive(Deserialize)]
    struct Tree {
        entries: Vec<Value>,
    }
    let tree: Tree = serde_json::from_slice(bytes).map_err(|e| Error::Format(format!("tree: {e}")))?;
    tree.entries.into_iter().map(Entry::from_value).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_round_towards_minus_infinity() {
        assert_eq!(split_ns(1_500_000_123), (1500, 123));
        assert_eq!(split_ns(-1), (-1, 999_999));
        assert_eq!(split_ns(-1_000_000), (-1, 0));
        let mut meta = Meta::default();
        meta.set_time_ns(-1);
        assert_eq!((meta.mtime, meta.mtime_nanos, meta.time_ns()), (Some(-1), Some(999_999), Some(-1)));
    }

    #[test]
    fn reads_version_1_entries() {
        let json = br#"{"entries":[{"name":"a.txt","type":"file","size":3,"mtime":1794400000000,"content":["x"]},
            {"name":"Photos","type":"dir","subtree":"y"}]}"#;
        let entries = decode_tree(json).unwrap();
        assert_eq!(entries[0].kind, EntryKind::File { size: 3, content: vec!["x".into()] });
        assert_eq!(entries[0].meta.mtime, Some(1_794_400_000_000));
        assert_eq!(entries[1].kind, EntryKind::Dir { subtree: "y".into(), mount: false });
    }

    #[test]
    fn version_2_entries_round_trip_including_bad_names() {
        let mut meta = Meta { mode: Some(0o755), uid: Some(1000), ..Meta::default() };
        meta.set_time_ns(1_794_400_000_123_456_789);
        let mut entries = vec![
            Entry { name: b"run.sh".to_vec(), kind: EntryKind::File { size: 0, content: vec![] }, meta },
            Entry {
                name: vec![b'a', 0xff],
                kind: EntryKind::Symlink { target: vec![0xfe, b'x'] },
                meta: Meta::default(),
            },
            Entry {
                name: b"mnt".to_vec(),
                kind: EntryKind::Dir { subtree: "e".into(), mount: true },
                meta: Meta::default(),
            },
        ];
        let bytes = encode_tree(&mut entries);
        assert_eq!(decode_tree(&bytes).unwrap(), entries);
        assert_eq!(encode_tree(&mut decode_tree(&bytes).unwrap()), bytes, "stable bytes");
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(r#""nameBytes":"Yf8=""#), "{text}");
        assert!(text.contains(r#""mtimeNanos":456789"#));
    }

    #[test]
    fn unknown_types_are_kept_as_unknown() {
        let entries = decode_tree(br#"{"entries":[{"name":"x","type":"socket","future":1}]}"#).unwrap();
        assert_eq!(entries[0].kind, EntryKind::Unknown("socket".into()));
    }

    #[test]
    fn names_are_validated() {
        for bad in [&b""[..], b".", b"..", b"a/b", b"a\0"] {
            assert!(!valid_name(bad));
        }
        assert!(valid_name(b".config") && valid_name(&[0xff]));
    }
}
