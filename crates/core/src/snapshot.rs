//! Snapshots of format versions 1 and 2 (FORMAT.md section 9,
//! FORMAT-v2.md section 5).

use serde::{Deserialize, Serialize};

use crate::tree::Meta;

pub const V1_AAD_PREFIX: &str = "xkyhold/v1/snapshot/";
pub const V2_AAD_PREFIX: &str = "xkyhold/v2/snapshot/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Home {
    pub path: String,
    pub user: String,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub kind: String,
    /// Relative to `home.path`; "" is the home folder itself.
    pub path: String,
}

/// One exclusion rule in force for a root (FORMAT-v2.md 5.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patterns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_bytes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub name: String,
    pub tree: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left_out: Option<Vec<Rule>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Stats {
    pub files: u64,
    pub bytes: u64,
    pub skipped: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub special: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symlinks: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mounts: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// 2 for version 2 snapshots; absent in version 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<u32>,
    pub id: String,
    pub time: String,
    pub device: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home: Option<Home>,
    pub roots: Vec<Root>,
    pub stats: Stats,
    pub parent: Option<String>,
}

impl Snapshot {
    pub fn version(&self) -> u32 {
        self.format.unwrap_or(1)
    }
}
