//! What is left out of a backup (PLAN.md 4.6, FORMAT-v2.md 5.4).

use crate::bytes::to_base64;
use crate::snapshot::Rule;

/// Signature that marks a cache folder (https://bford.info/cachedir/).
pub const CACHEDIR_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// A suggestion the user can switch on, shown with the space it uses.
pub struct Preset {
    pub id: &'static str,
    pub patterns: &'static [&'static str],
}

pub const PRESETS: &[Preset] = &[
    Preset { id: "node-modules", patterns: &["**/node_modules"] },
    Preset { id: "rust-target", patterns: &["**/target"] },
    Preset { id: "python-venv", patterns: &["**/.venv", "**/venv"] },
    Preset {
        id: "browser-caches",
        patterns: &[".mozilla/firefox/*/cache2", ".config/google-chrome/*/Cache", ".config/chromium/*/Cache"],
    },
    Preset { id: "flatpak-caches", patterns: &[".var/app/*/cache"] },
    Preset { id: "containers", patterns: &[".local/share/containers"] },
    Preset { id: "steam", patterns: &[".local/share/Steam", ".steam"] },
    Preset { id: "vm-images", patterns: &["**/*.qcow2", "**/*.vdi", "**/*.vmdk", ".local/share/libvirt/images"] },
    Preset { id: "downloads", patterns: &["Downloads"] },
];

/// The exclusion settings of one backup. Paths and patterns are relative
/// to the home folder.
#[derive(Debug, Clone, Default)]
pub struct Exclusions {
    /// Ids from [`PRESETS`] the user switched on.
    pub presets: Vec<String>,
    /// Folders and files unticked in the leave-out tree.
    pub paths: Vec<Vec<u8>>,
    /// Glob patterns the user wrote.
    pub patterns: Vec<String>,
    /// Mount points the user chose to cross.
    pub cross_mounts: Vec<Vec<u8>>,
    /// The repository itself, when it is a folder inside the home folder.
    pub repository: Option<Vec<u8>>,
}

/// Always left out, whatever the settings.
pub const DEFAULTS: &[(&str, &[&str])] = &[
    ("cache", &[".cache"]),
    ("trash", &[".local/share/Trash"]),
    // The window's web view keeps its data under the app identifier.
    ("app-state", &[".cache/xkyhold-lx", ".local/state/xkyhold-lx", ".local/share/com.xkyriver.xkyhold-lx"]),
];

/// Whether a path is kept or left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    LeaveOut,
}

pub struct Matcher {
    globs: Vec<Vec<u8>>,
    paths: Vec<Vec<u8>>,
    cross: Vec<Vec<u8>>,
    rules: Vec<Rule>,
}

impl Matcher {
    pub fn new(ex: &Exclusions) -> Self {
        let mut rules = Vec::new();
        let mut globs: Vec<Vec<u8>> = Vec::new();
        let rule = |kind: &str| Rule { kind: kind.into(), id: None, patterns: None, path: None, path_bytes: None };
        for (id, patterns) in DEFAULTS {
            globs.extend(patterns.iter().map(|p| p.as_bytes().to_vec()));
            rules.push(Rule {
                id: Some((*id).into()),
                patterns: Some(patterns.iter().map(|p| (*p).into()).collect()),
                ..rule("default")
            });
        }
        let mut paths = ex.paths.clone();
        if let Some(repo) = &ex.repository {
            paths.push(repo.clone());
            rules.push(Rule {
                id: Some("repository".into()),
                patterns: Some(vec![String::from_utf8_lossy(repo).into_owned()]),
                ..rule("default")
            });
        }
        rules.push(Rule { id: Some("cachedir-tag".into()), ..rule("default") });
        rules.push(Rule { id: Some("other-file-systems".into()), ..rule("default") });
        for preset in PRESETS.iter().filter(|p| ex.presets.iter().any(|id| id == p.id)) {
            let patterns: Vec<String> = preset.patterns.iter().map(|p| (*p).to_owned()).collect();
            globs.extend(patterns.iter().map(|p| p.as_bytes().to_vec()));
            rules.push(Rule { id: Some(preset.id.into()), patterns: Some(patterns), ..rule("preset") });
        }
        for path in &ex.paths {
            rules.push(path_rule("path", path));
        }
        if !ex.patterns.is_empty() {
            globs.extend(ex.patterns.iter().map(|p| p.as_bytes().to_vec()));
            rules.push(Rule { patterns: Some(ex.patterns.clone()), ..rule("pattern") });
        }
        for path in &ex.cross_mounts {
            rules.push(path_rule("cross-mount", path));
        }
        Self { globs, paths, cross: ex.cross_mounts.clone(), rules }
    }

    /// The rules to record in the snapshot.
    pub fn rules(&self) -> Vec<Rule> {
        self.rules.clone()
    }

    /// Is this path (relative to home) left out by a path or pattern rule?
    pub fn check(&self, path: &[u8]) -> Verdict {
        if self.paths.iter().any(|p| p == path) || self.globs.iter().any(|g| glob_match(g, path)) {
            Verdict::LeaveOut
        } else {
            Verdict::Keep
        }
    }

    pub fn crosses(&self, path: &[u8]) -> bool {
        self.cross.iter().any(|p| p == path)
    }
}

fn path_rule(kind: &str, path: &[u8]) -> Rule {
    let (text, bytes) = match std::str::from_utf8(path) {
        Ok(t) => (Some(t.to_owned()), None),
        Err(_) => (None, Some(to_base64(path))),
    };
    Rule { kind: kind.into(), id: None, patterns: None, path: text, path_bytes: bytes }
}

/// Matches a whole path against a pattern (FORMAT-v2.md 5.4): `*` and `?`
/// within one name, `**` any number of whole names, anchored at the root.
pub fn glob_match(pattern: &[u8], path: &[u8]) -> bool {
    let pat: Vec<&[u8]> = pattern.split(|&b| b == b'/').collect();
    let names: Vec<&[u8]> = path.split(|&b| b == b'/').collect();
    match_names(&pat, &names)
}

fn match_names(pat: &[&[u8]], names: &[&[u8]]) -> bool {
    match pat.split_first() {
        None => names.is_empty(),
        Some((&b"**", rest)) => (0..=names.len()).any(|skip| match_names(rest, &names[skip..])),
        Some((first, rest)) => !names.is_empty() && match_name(first, names[0]) && match_names(rest, &names[1..]),
    }
}

fn match_name(pat: &[u8], name: &[u8]) -> bool {
    match pat.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|skip| match_name(rest, &name[skip..])),
        Some((b'?', rest)) => !name.is_empty() && match_name(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && match_name(rest, &name[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_follow_the_spec() {
        assert!(glob_match(b".cache", b".cache"));
        assert!(!glob_match(b".cache", b"a/.cache"));
        assert!(glob_match(b"**/node_modules", b"node_modules"));
        assert!(glob_match(b"**/node_modules", b"work/app/node_modules"));
        assert!(!glob_match(b"**/node_modules", b"work/node_modules/x"));
        assert!(glob_match(b"**/*.iso", b"Downloads/arch.iso"));
        assert!(glob_match(b".var/app/*/cache", b".var/app/org.x.Y/cache"));
        assert!(!glob_match(b".var/app/*/cache", b".var/app/a/b/cache"));
        assert!(glob_match(b"file?.txt", b"file1.txt"));
    }

    #[test]
    fn rules_record_every_kind() {
        let m = Matcher::new(&Exclusions {
            presets: vec!["node-modules".into()],
            paths: vec![b"Downloads".to_vec()],
            patterns: vec!["**/*.iso".into()],
            cross_mounts: vec![b"Data".to_vec()],
            repository: Some(b"Backups/XkyHold".to_vec()),
        });
        assert_eq!(m.check(b"Backups/XkyHold"), Verdict::LeaveOut);
        let kinds: Vec<_> = m.rules().iter().map(|r| r.kind.clone()).collect();
        for k in ["default", "preset", "path", "pattern", "cross-mount"] {
            assert!(kinds.iter().any(|x| x == k), "{k}");
        }
        assert_eq!(m.check(b"Downloads"), Verdict::LeaveOut);
        assert_eq!(m.check(b".cache"), Verdict::LeaveOut);
        assert_eq!(m.check(b"code/x/node_modules"), Verdict::LeaveOut);
        assert_eq!(m.check(b"Documents"), Verdict::Keep);
        assert!(m.crosses(b"Data"));
    }
}
