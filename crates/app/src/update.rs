//! New versions (docs/UPDATES.md 4). The window looks at most once a day for
//! `latest.json` in the latest GitHub release and tells the person; it never
//! updates by itself, and the scheduled pass never looks.
//!
//! The request carries no identifier: it downloads one public file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use xkyhold_core::bytes::from_base64;
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

/// The public key every release is signed with (docs/UPDATES.md 4.5), as
/// `tauri signer generate` writes it: the minisign public key file, in
/// base64. The private key stays with the owner. Empty until it exists:
/// then no version is announced and nothing is installed.
pub const PUBLIC_KEY: &str = "";

/// A test build (docs/UPDATES.md, phase U4): built with
/// `XKYHOLD_LX_TEST_UPDATE_URL` and `XKYHOLD_LX_TEST_UPDATE_KEY`, it reads
/// updates from that address with that key and never touches the systemd
/// timer. `scripts/release` refuses to publish one.
const TEST_URL: Option<&str> = option_env!("XKYHOLD_LX_TEST_UPDATE_URL");
const TEST_KEY: Option<&str> = option_env!("XKYHOLD_LX_TEST_UPDATE_KEY");

pub fn test_build() -> bool {
    TEST_URL.is_some()
}

/// The key to check releases with. Development builds may use a test key.
pub fn public_key() -> Result<String> {
    #[cfg(debug_assertions)]
    if let Ok(key) = std::env::var("XKYHOLD_LX_UPDATE_KEY") {
        return Ok(key);
    }
    if let Some(key) = TEST_KEY {
        return Ok(key.to_owned());
    }
    if PUBLIC_KEY.is_empty() {
        return Err(Error::Usage("this build has no key to check updates with".into()));
    }
    Ok(PUBLIC_KEY.to_owned())
}

fn unwrap_text(b64: &str, what: &str) -> Result<String> {
    let bytes = from_base64(b64.trim()).map_err(|_| Error::Format(format!("{what}: not base64")))?;
    String::from_utf8(bytes).map_err(|_| Error::Format(format!("{what}: not text")))
}

/// Checks a minisign signature (in the base64 form `tauri signer` writes)
/// of `data` with `key`, and that it was made for a file called `name`:
/// one signed file cannot stand in for another. Never accepts the legacy,
/// non-prehashed form.
pub fn verify(key: &str, signature: &str, data: &[u8], name: &str) -> Result<()> {
    let bad = |why: &str| Error::Format(format!("signature of {name}: {why}"));
    let key = PublicKey::decode(&unwrap_text(key, "public key")?).map_err(|e| bad(&e.to_string()))?;
    let sig = Signature::decode(&unwrap_text(signature, "signature")?).map_err(|e| bad(&e.to_string()))?;
    key.verify(data, &sig, false).map_err(|e| bad(&e.to_string()))?;
    // The trusted comment is covered by the signature: "timestamp:…\tfile:<name>".
    if !sig.trusted_comment().split('\t').any(|part| part == format!("file:{name}")) {
        return Err(bad("made for another file"));
    }
    Ok(())
}

/// The last part of a download address: the file's name.
fn file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

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

    /// `latest.json` checked against its signature before anything in it is
    /// believed: nobody else can announce a version.
    pub fn verified(json: &[u8], signature: &str, key: &str) -> Result<Self> {
        verify(key, signature, json, "latest.json")?;
        Self::parse(json)
    }

    /// Checks a downloaded package of this release (`kind` as in `files`,
    /// such as `appimage-x86_64`): its size, its signature, and that its
    /// name carries this version.
    pub fn verify_package(&self, kind: &str, data: &[u8], key: &str) -> Result<()> {
        let file = self.files.get(kind).ok_or_else(|| Error::NotFound(format!("{kind} in latest.json")))?;
        let name = file_name(&file.url);
        if data.len() as u64 != file.size {
            return Err(Error::Format(format!("{name}: {} bytes, expected {}", data.len(), file.size)));
        }
        if !name.contains(&self.version) {
            return Err(Error::Format(format!("{name} is not version {}", self.version)));
        }
        verify(key, &file.signature, data, name)
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
    TEST_URL.unwrap_or(LATEST_URL).to_owned()
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

/// `latest.json` and its signature, checked.
pub fn fetch_latest() -> Result<Latest> {
    let key = public_key()?;
    let url = latest_url();
    let json = get(&url, MAX_LATEST)?;
    let signature = String::from_utf8(get(&format!("{url}.sig"), 4096)?)
        .map_err(|_| Error::Format("latest.json.sig: not text".into()))?;
    Latest::verified(&json, &signature, &key)
}

/// How this program was installed, which decides what "Update" does
/// (docs/UPDATES.md 4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Install {
    /// An AppImage the person downloaded: replaced in place.
    AppImage {
        path: PathBuf,
    },
    /// An AppImage AppMan manages: replaced in place, then AppMan told.
    AppMan {
        path: PathBuf,
    },
    /// Installed from a package; the system's tool updates it (phase U5).
    Deb,
    Rpm,
    /// Run from a build folder, or anything else: the release page.
    Other,
}

impl Install {
    /// The `files` entry of `latest.json` this installation needs.
    pub fn package(&self) -> Option<&'static str> {
        match self {
            Install::AppImage { .. } | Install::AppMan { .. } => Some("appimage-x86_64"),
            Install::Deb => Some("deb-amd64"),
            Install::Rpm => Some("rpm-x86_64"),
            Install::Other => None,
        }
    }
}

/// Finds out how this program was installed. An AppImage says where it is
/// in `APPIMAGE`; AppMan keeps `AM-updater` and `version` next to it.
pub fn installed() -> Install {
    if let Some(path) = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_absolute() && p.is_file()) {
        let dir = path.parent().unwrap_or(Path::new("/"));
        return if dir.join("AM-updater").is_file() && dir.join("version").is_file() {
            Install::AppMan { path }
        } else {
            Install::AppImage { path }
        };
    }
    let Ok(exe) = std::env::current_exe() else { return Install::Other };
    let owned_by = |tool: &str, arg: &str| {
        std::process::Command::new(tool)
            .arg(arg)
            .arg(&exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    if owned_by("dpkg", "-S") {
        Install::Deb
    } else if owned_by("rpm", "-qf") {
        Install::Rpm
    } else {
        Install::Other
    }
}

/// Can this program replace itself? Its folder must be writable.
pub fn can_replace(path: &Path) -> bool {
    let Some(dir) = path.parent() else { return false };
    let probe = dir.join(format!(".xkyhold-lx-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Downloads a package of up to `size` bytes, reporting progress.
pub fn download(url: &str, size: u64, on_progress: &mut dyn FnMut(u64, u64)) -> Result<Vec<u8>> {
    use std::io::Read;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(600)))
        .user_agent(format!("xkyhold-lx/{CURRENT}"))
        .build()
        .into();
    let mut response = agent.get(url).call().map_err(|e| Error::Usage(format!("update download: {e}")))?;
    let mut reader = response.body_mut().with_config().limit(size + 1).reader();
    let mut data = Vec::with_capacity(size as usize);
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| Error::Usage(format!("update download: {e}")))?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        on_progress(data.len() as u64, size);
    }
    Ok(data)
}

/// Puts a verified AppImage in place of `path`. The new file is written
/// next to it, made executable and renamed over it in one step; the old
/// one stays as `<name>.old` until the new version opens
/// ([`tidy_after_update`]). The path does not change, so the systemd timer
/// still points at the right program.
pub fn replace_appimage(path: &Path, data: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let name = path.file_name().ok_or_else(|| Error::Usage("no file name".into()))?.to_string_lossy();
    let dir = path.parent().ok_or_else(|| Error::Usage("no folder".into()))?;
    let new = dir.join(format!(".{name}.new"));
    let old = dir.join(format!("{name}.old"));
    std::fs::write(&new, data)?;
    std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    let _ = std::fs::remove_file(&old);
    std::fs::hard_link(path, &old).or_else(|_| std::fs::copy(path, &old).map(|_| ()))?;
    if let Err(e) = std::fs::rename(&new, path) {
        let _ = std::fs::remove_file(&new);
        return Err(e.into());
    }
    Ok(())
}

/// AppMan's `version` file holds the address of the installed AppImage:
/// set it to the new one, so the next `appman -u` does not fetch it again.
pub fn tell_appman(path: &Path, url: &str) -> Result<()> {
    let version = path.parent().ok_or_else(|| Error::Usage("no folder".into()))?.join("version");
    std::fs::write(version, format!("{url}\n"))?;
    Ok(())
}

/// The new version opened: the old AppImage kept by [`replace_appimage`]
/// is no longer needed.
pub fn tidy_after_update() {
    if let Some(path) = std::env::var_os("APPIMAGE").map(PathBuf::from) {
        let mut old = path.into_os_string();
        old.push(".old");
        let _ = std::fs::remove_file(old);
    }
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

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/fixtures/update/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }
    fn text(name: &str) -> String {
        String::from_utf8(fixture(name)).unwrap()
    }

    /// Signed with a throwaway test key (never a release key).
    #[test]
    fn only_signed_releases_are_believed() {
        let key = text("test.key.pub");
        let json = fixture("latest.json");
        let latest = Latest::verified(&json, &text("latest.json.sig"), &key).unwrap();
        assert_eq!(latest.version, "9.9.9");

        // latest.json changed after signing: refused.
        let altered = String::from_utf8(json.clone()).unwrap().replace("9.9.9\",\"date", "9.9.10\",\"date");
        assert!(Latest::verified(altered.as_bytes(), &text("latest.json.sig"), &key).is_err());
        // Another file's signature: refused.
        let other = text("xkyhold-lx_9.9.9_amd64.AppImage.sig");
        assert!(Latest::verified(&json, &other, &key).is_err());
        // Another key: refused.
        let stranger = text("other.key.pub");
        assert!(Latest::verified(&json, &text("latest.json.sig"), &stranger).is_err());

        // The package: good, changed, truncated, or an older signed one.
        let good = fixture("xkyhold-lx_9.9.9_amd64.AppImage");
        latest.verify_package("appimage-x86_64", &good, &key).unwrap();
        let mut changed = good.clone();
        changed[0] ^= 1;
        assert!(latest.verify_package("appimage-x86_64", &changed, &key).is_err());
        assert!(latest.verify_package("appimage-x86_64", &good[..10], &key).is_err());
        let old = fixture("xkyhold-lx_0.9.0_amd64.AppImage");
        assert!(latest.verify_package("appimage-x86_64", &old, &key).is_err());
        // The old file with its own valid signature still is not this release's file.
        let old_sig = text("xkyhold-lx_0.9.0_amd64.AppImage.sig");
        assert!(verify(&key, &old_sig, &old, "xkyhold-lx_9.9.9_amd64.AppImage").is_err());
        verify(&key, &old_sig, &old, "xkyhold-lx_0.9.0_amd64.AppImage").unwrap();
        assert!(latest.verify_package("deb-amd64", &good, &key).is_err());
    }

    #[test]
    fn an_appimage_is_replaced_in_one_step_and_the_old_one_kept() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("xkyhold-lx");
        std::fs::write(&app, b"version 1").unwrap();
        assert!(can_replace(&app));
        replace_appimage(&app, b"version 2").unwrap();
        assert_eq!(std::fs::read(&app).unwrap(), b"version 2");
        assert_eq!(std::fs::read(dir.path().join("xkyhold-lx.old")).unwrap(), b"version 1");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&app).unwrap().permissions().mode() & 0o777, 0o755);
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left.len(), 2, "no temporary file left: {left:?}");

        std::fs::write(dir.path().join("version"), b"https://old\n").unwrap();
        tell_appman(&app, "https://new").unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("version")).unwrap(), "https://new\n");
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
