//! New versions (docs/UPDATES.md 4). The window looks at most once a day for
//! `latest.json` in the latest GitHub release and tells the person; it never
//! updates by itself, and the scheduled pass never looks.
//!
//! The request carries no identifier: it downloads one public file.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use xkyhold_core::time::{now_iso, now_ms, parse_iso};
use xkyhold_core::{Error, Result};

use crate::settings::Settings;

/// This program's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Where releases are published; the only pages the window opens for them.
pub const RELEASES: &str = "https://github.com/xkyriver/xkyhold-lx/releases/";
const LATEST_URL: &str = "https://github.com/xkyriver/xkyhold-lx/releases/latest/download/latest.json";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// `latest.json` is a few kilobytes; anything bigger is not it.
const MAX_LATEST: u64 = 256 * 1024;

/// `latest.json` as published with each release (docs/UPDATES.md 4.1).
#[derive(Debug, Clone, Deserialize)]
pub struct Latest {
    pub version: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub notes: BTreeMap<String, String>,
    pub page: String,
    #[serde(default)]
    pub files: BTreeMap<String, FileRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileRef {
    pub url: String,
    pub size: u64,
    pub signature: String,
}

/// A newer version, as kept in the settings and shown by the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Available {
    pub version: String,
    /// The release page; always under [`RELEASES`].
    pub page: String,
    #[serde(default)]
    pub notes_en: String,
    #[serde(default)]
    pub notes_pt: String,
}

/// `major.minor.patch`, with an optional leading `v`. Pre-releases and
/// anything else are not versions the window announces.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

/// Is `candidate` a version above `current`? Never a step back.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(c), Some(i)) => c > i,
        _ => false,
    }
}

impl Latest {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(|e| Error::Format(format!("latest.json: {e}")))
    }

    fn available(&self) -> Available {
        // A page anywhere else is not followed: the release list instead.
        let page = if self.page.starts_with(RELEASES) { self.page.clone() } else { format!("{RELEASES}latest") };
        let note = |lang: &str| self.notes.get(lang).cloned().unwrap_or_default();
        Available { version: self.version.clone(), page, notes_en: note("en"), notes_pt: note("pt-PT") }
    }
}

/// Where `latest.json` is read. Development builds may point elsewhere,
/// for testing with a file served on this computer.
fn latest_url() -> String {
    #[cfg(debug_assertions)]
    if let Ok(url) = std::env::var("XKYHOLD_LX_UPDATE_URL") {
        return url;
    }
    LATEST_URL.to_owned()
}

/// Downloads a small public file, following GitHub's redirect.
pub fn get(url: &str, limit: u64) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .user_agent(format!("xkyhold-lx/{CURRENT}"))
        .build()
        .into();
    let mut response = agent.get(url).call().map_err(|e| Error::Usage(format!("update check: {e}")))?;
    response.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| Error::Usage(format!("update check: {e}")))
}

pub fn fetch_latest() -> Result<Latest> {
    Latest::parse(&get(&latest_url(), MAX_LATEST)?)
}

/// What to show now: the version found, unless it is not newer than this
/// one (an update already installed) or was set aside.
pub fn shown(settings: &Settings) -> Option<Available> {
    let found = settings.update_latest.clone()?;
    (is_newer(&found.version, CURRENT) && settings.update_skipped.as_deref() != Some(found.version.as_str()))
        .then_some(found)
}

/// Is a look due? Once a day, when switched on.
pub fn due(settings: &Settings, now: i64) -> bool {
    settings.check_updates
        && settings.update_checked.as_deref().and_then(parse_iso).is_none_or(|last| now - last >= DAY_MS)
}

/// Looks for a new version if one is due (or `now`, from Settings), keeps
/// what it found and returns what to show. A failure keeps the last answer.
pub fn check(force: bool) -> Result<Option<Available>> {
    let settings = Settings::load()?;
    if !force && !due(&settings, now_ms()) {
        return Ok(shown(&settings));
    }
    let latest = fetch_latest()?;
    let found = is_newer(&latest.version, CURRENT).then(|| latest.available());
    let settings = Settings::update(|s| {
        s.update_checked = Some(now_iso());
        s.update_latest = found.clone();
        // Asked for in Settings: show it even if set aside before.
        if force {
            s.update_skipped = None;
        }
        Ok(())
    })?;
    Ok(shown(&settings))
}

/// "Not now": this version is not shown again; the next one is.
pub fn skip(version: &str) -> Result<()> {
    Settings::update(|s| {
        s.update_skipped = Some(version.to_owned());
        Ok(())
    })
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_only_go_up() {
        assert!(is_newer("1.0.2", "1.0.1"));
        assert!(is_newer("v1.10.0", "1.9.9"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("1.0.1", "1.0.1"));
        assert!(!is_newer("1.0.0", "1.0.1"), "never a step back");
        assert!(!is_newer("1.1.0-rc1", "1.0.0"), "no pre-releases");
        assert!(!is_newer("1.1", "1.0.0"));
        assert!(!is_newer("garbage", "1.0.0"));
    }

    #[test]
    fn latest_json_is_read_and_its_page_kept_on_github() {
        let text = br#"{"version":"1.1.0","date":"2026-11-20T10:00:00Z",
            "notes":{"en":"Faster","pt-PT":"Mais rapido"},
            "page":"https://github.com/xkyriver/xkyhold-lx/releases/tag/v1.1.0",
            "files":{"appimage-x86_64":{"url":"u","size":1,"signature":"s"}}}"#;
        let latest = Latest::parse(text).unwrap();
        let a = latest.available();
        assert_eq!((a.version.as_str(), a.notes_pt.as_str()), ("1.1.0", "Mais rapido"));
        assert!(a.page.ends_with("/tag/v1.1.0"));
        let elsewhere = Latest { page: "https://evil.example/".into(), ..latest };
        assert_eq!(elsewhere.available().page, format!("{RELEASES}latest"));
        assert!(Latest::parse(b"<html>").is_err());
    }

    #[test]
    fn shown_once_a_day_and_never_when_set_aside() {
        let now = parse_iso("2026-11-15T12:00:00.000Z").unwrap();
        let mut s = Settings::default();
        assert!(due(&s, now));
        s.update_checked = Some(xkyhold_core::time::iso(now - 2 * 60 * 60 * 1000));
        assert!(!due(&s, now));
        s.update_checked = Some(xkyhold_core::time::iso(now - DAY_MS));
        assert!(due(&s, now));
        s.check_updates = false;
        assert!(!due(&s, now));

        let found = Available {
            version: "99.0.0".into(),
            page: RELEASES.into(),
            notes_en: String::new(),
            notes_pt: String::new(),
        };
        s.update_latest = Some(found.clone());
        assert_eq!(shown(&s), Some(found));
        s.update_skipped = Some("99.0.0".into());
        assert_eq!(shown(&s), None);
        s.update_latest = Some(Available { version: CURRENT.into(), ..s.update_latest.clone().unwrap() });
        s.update_skipped = None;
        assert_eq!(shown(&s), None, "already installed");
    }
}
