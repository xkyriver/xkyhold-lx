//! What the window and the CLI share: where things live on this computer,
//! the settings file, opening a backup (Google Drive or a local folder)
//! with the keyring or a password, and running a backup end to end.

pub mod power;
pub mod scan;
pub mod schedule;
pub mod settings;
pub mod update;

use std::io::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use xkyhold_core::backup::{BackupOptions, Progress, backup};
use xkyhold_core::crypto::Credential;
use xkyhold_core::destination::Destination;
use xkyhold_core::lock::{Lock, LockKind};
use xkyhold_core::prune::{forget_older, prune};
use xkyhold_core::repo::{Options, Repository};
use xkyhold_core::restore::find;
use xkyhold_core::rules::{Exclusions, PRESETS};
use xkyhold_core::snapshot::Snapshot;
use xkyhold_core::time::{now_iso, now_ms};
use xkyhold_core::tree::{Entry, EntryKind};
use xkyhold_core::{Error, Result};
use xkyhold_dest_drive::{Client, Drive, DriveFolder, Http, RefreshingTokens, Ureq};
use xkyhold_dest_local::LocalFolder;
use xkyhold_secrets::{MemorySecrets, SecretService, SecretStore, google_refresh, repo_key};
use zeroize::Zeroizing;

pub use settings::{LastBackup, Settings, Target, What};

pub type Repo = Repository<Arc<dyn Destination>>;

/// Average month, for "keep backups for N months".
pub const MONTH_MS: i64 = 2_629_746_000;

pub fn home() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or(Error::OutsideHome("$HOME is not set".into()))?;
    if !home.is_absolute() || home == Path::new("/") {
        return Err(Error::OutsideHome(home.display().to_string()));
    }
    Ok(home)
}

/// The computer's name, recorded in each backup.
pub fn device() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|h| h.trim().to_owned()).unwrap_or_else(|_| "linux".into())
}

/// `$XDG_<…>_HOME/xkyhold-lx`, or the usual fallback inside the home folder.
pub fn xdg(var: &str, fallback: &str) -> Result<PathBuf> {
    match std::env::var_os(var) {
        Some(dir) if Path::new(&dir).is_absolute() => Ok(PathBuf::from(dir).join("xkyhold-lx")),
        _ => Ok(home()?.join(fallback).join("xkyhold-lx")),
    }
}

/// Appends a line to `$XDG_STATE_HOME/xkyhold-lx/xkyhold-lx.log` (and stderr).
/// Raw errors go here; people see actionable sentences.
pub fn log(line: &str) {
    let stamped = format!("[{}] {line}", now_iso());
    eprintln!("{stamped}");
    if let Ok(dir) = xdg("XDG_STATE_HOME", ".local/state") {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("xkyhold-lx.log")) {
            let _ = writeln!(f, "{stamped}");
        }
    }
}

pub fn cache_options() -> Options {
    Options { cache: xdg("XDG_CACHE_HOME", ".cache").ok(), ..Options::default() }
}

pub fn secrets() -> Result<SecretService> {
    SecretService::connect().map_err(|e| Error::Usage(e.to_string()))
}

pub fn client() -> Result<Client> {
    Client::from_env().ok_or_else(|| {
        Error::Usage("this build has no Google client; set XKYHOLD_LX_CLIENT_ID and XKYHOLD_LX_CLIENT_SECRET".into())
    })
}

/// A Drive client for the account connected on this computer.
pub fn drive_client(settings: &Settings, limit_kb: Option<u64>) -> Result<Drive> {
    let account = settings.account.as_deref().ok_or_else(|| Error::Session("not connected to Google Drive".into()))?;
    let key = google_refresh(account);
    let kept = match secrets() {
        Ok(store) => store.get(&key).map_err(|e| Error::Usage(e.to_string()))?,
        Err(_) => None,
    };
    let refresh = kept
        .or_else(|| session_secrets().get(&key).ok().flatten())
        .ok_or_else(|| Error::Session("not connected to Google Drive".into()))?;
    let refresh = String::from_utf8(refresh).map_err(|_| Error::Session("sign in to Google again".into()))?;
    let http: Arc<dyn Http> = Arc::new(Ureq::default());
    let mut drive = Drive::new(http.clone(), Arc::new(RefreshingTokens::new(http, client()?, refresh)));
    if let Some(kb) = limit_kb {
        drive.limit_uploads(kb * 1000);
    }
    Ok(drive)
}

/// Signs in to Google in the browser, keeps the refresh token in the
/// keyring (in memory for this run when there is none) and returns the account's email address. `show` gets the
/// sign-in address (to open it, or to print it).
pub fn google_sign_in(show: &mut dyn FnMut(&str)) -> Result<String> {
    google_sign_in_until(show, &AtomicBool::new(false))
}

/// [`google_sign_in`] that stops waiting for the browser once `cancel` is set.
pub fn google_sign_in_until(show: &mut dyn FnMut(&str), cancel: &AtomicBool) -> Result<String> {
    let client = client()?;
    let http = Ureq::default();
    let signed = xkyhold_dest_drive::sign_in_until(&http, &client, show, cancel)?;
    let http: Arc<dyn Http> = Arc::new(http);
    let tokens = Arc::new(RefreshingTokens::new(http.clone(), client, signed.refresh.clone()));
    let account = Drive::new(http, tokens).about_email()?;
    let key = google_refresh(&account);
    if let Err(e) =
        secrets().and_then(|s| s.set(&key, signed.refresh.as_bytes()).map_err(|e| Error::Usage(e.to_string())))
    {
        // No keyring: the session lasts while this program runs.
        log(&format!("Google session kept in memory only: {e}"));
        let _ = session_secrets().set(&key, signed.refresh.as_bytes());
    }
    Ok(account)
}

pub fn google_sign_out(account: &str) -> Result<()> {
    let key = google_refresh(account);
    let _ = session_secrets().delete(&key);
    match secrets() {
        Ok(store) => store.delete(&key).map_err(|e| Error::Usage(e.to_string())),
        Err(_) => Ok(()),
    }
}

/// Secrets for this run only, when the computer has no keyring.
fn session_secrets() -> &'static MemorySecrets {
    static SESSION: std::sync::OnceLock<MemorySecrets> = std::sync::OnceLock::new();
    SESSION.get_or_init(MemorySecrets::default)
}

/// The destination of a backup, and its local path when it is a folder
/// on this computer (left out of the backup when it lies inside home).
pub fn destination(
    target: &Target,
    settings: &Settings,
    limit_kb: Option<u64>,
) -> Result<(Arc<dyn Destination>, Option<PathBuf>)> {
    match target {
        Target::Drive { name } => {
            let folder = DriveFolder::open(drive_client(settings, limit_kb)?, name)?;
            folder.warm()?;
            Ok((Arc::new(folder), None))
        }
        Target::Local { path } => {
            let path = std::path::absolute(path)?;
            Ok((Arc::new(LocalFolder::new(&path)), Some(path)))
        }
    }
}

/// What a local folder holds, before using it for a backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderState {
    Missing,
    Empty,
    Repository,
    /// Holds other files: not used, to keep backups apart from other data.
    Other,
}

pub fn inspect_folder(path: &Path) -> FolderState {
    match std::fs::read_dir(path) {
        Err(_) => FolderState::Missing,
        Ok(mut entries) => {
            // A new repository holds only `config` until its first backup.
            if path.join("config").is_file() {
                FolderState::Repository
            } else if entries.next().is_none() {
                FolderState::Empty
            } else {
                FolderState::Other
            }
        }
    }
}

/// Opens with the key kept in the keyring, if there is one that fits.
pub fn open_with_keyring(dest: Arc<dyn Destination>) -> Result<Option<Repo>> {
    let config = Repository::read_config(&dest)?;
    let Ok(store) = SecretService::connect() else { return Ok(None) };
    let Ok(Some(master)) = store.get(&repo_key(&config.repo_id)) else { return Ok(None) };
    match Repository::open(dest, &Credential::MasterKey(Zeroizing::new(master)), cache_options()) {
        Err(Error::WrongKey) => Ok(None),
        other => other.map(Some),
    }
}

pub fn open_with(dest: Arc<dyn Destination>, credential: &Credential) -> Result<Repo> {
    Repository::open(dest, credential, cache_options())
}

/// Keeps (or removes) this backup's key in the keyring.
pub fn remember_key(repo: &Repo, remember: bool) -> Result<()> {
    let store = secrets()?;
    let key = repo_key(&repo.config.repo_id);
    let done = if remember { store.set(&key, repo.keys.master_key()) } else { store.delete(&key) };
    done.map_err(|e| Error::Usage(e.to_string()))
}

pub fn key_remembered(repo_id: &str) -> bool {
    SecretService::connect().ok().and_then(|s| s.get(&repo_key(repo_id)).ok().flatten()).is_some()
}

pub fn forget_key(repo_id: &str) -> Result<()> {
    secrets()?.delete(&repo_key(repo_id)).map_err(|e| Error::Usage(e.to_string()))
}

/// A path inside the home folder, relative to it. Refuses anything outside.
pub fn inside_home(home: &Path, path: &Path) -> Result<Vec<u8>> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { home.join(path) };
    let mut parts: Vec<&[u8]> = Vec::new();
    for c in absolute.strip_prefix(home).map_err(|_| Error::OutsideHome(path.display().to_string()))?.components() {
        match c {
            std::path::Component::Normal(name) => parts.push(name.as_bytes()),
            std::path::Component::CurDir => {}
            _ => return Err(Error::OutsideHome(path.display().to_string())),
        }
    }
    Ok(parts.join(&b'/'))
}

/// The exclusion rules for `what`, with the repository itself left out
/// when it is a folder inside home.
pub fn exclusions(home: &Path, what: &What, repo_path: Option<&Path>) -> Result<Exclusions> {
    let rel = |paths: &[String]| paths.iter().map(|p| inside_home(home, Path::new(p))).collect::<Result<Vec<_>>>();
    for id in &what.presets {
        if !PRESETS.iter().any(|p| p.id == id) {
            return Err(Error::Usage(format!("unknown preset \"{id}\"")));
        }
    }
    Ok(Exclusions {
        presets: what.presets.clone(),
        paths: rel(&what.leave_out)?,
        patterns: what.patterns.clone(),
        cross_mounts: rel(&what.cross_mounts)?,
        repository: repo_path.and_then(|p| inside_home(home, p).ok()),
    })
}

/// The outcome of [`run_backup`].
pub struct BackupDone {
    pub snapshot: Snapshot,
    /// Bytes sent by this backup.
    pub uploaded: u64,
    /// Old backups removed afterwards, by the retention setting.
    pub forgotten: usize,
    /// A failure while removing old backups; the new backup is safe.
    pub cleanup_error: Option<Error>,
}

/// Backs up the home folder by `what`, under the repository lock, then
/// removes backups older than `keep_months` (0 keeps everything).
#[allow(clippy::too_many_arguments)]
pub fn run_backup(
    repo: &mut Repo,
    dest: Arc<dyn Destination>,
    repo_path: Option<&Path>,
    what: &What,
    keep_months: u32,
    cancel: Option<&AtomicBool>,
    on_progress: &mut dyn FnMut(&Progress),
    on_cleanup: &mut dyn FnMut(),
) -> Result<BackupDone> {
    let home = home()?;
    let exclusions = exclusions(&home, what, repo_path)?;
    let selection = if what.only.is_empty() {
        None
    } else {
        Some(what.only.iter().map(|p| inside_home(&home, Path::new(p))).collect::<Result<Vec<_>>>()?)
    };
    // One backup at a time on this computer: the window, the timer, the CLI.
    let _here = schedule::LocalLock::take()?;
    let device = device();
    // The repository counts uploads for as long as it is open.
    let sent_before = repo.uploaded_bytes;
    let mut lock = Lock::acquire(dest.clone(), LockKind::Backup, &device)?;
    let result = (|| {
        repo.refresh_index()?;
        let parent = latest_of(repo, &device, &home)?;
        let mut options = BackupOptions::new(device.clone(), now_iso());
        options.parent = parent.as_ref();
        options.exclusions = exclusions;
        options.selection = selection;
        options.cancel = cancel;
        backup(repo, &home, options, on_progress)
    })();
    lock.release();
    let snapshot = result?;
    let uploaded = repo.uploaded_bytes - sent_before;
    let (forgotten, cleanup_error) = if keep_months == 0 {
        (0, None)
    } else {
        on_cleanup();
        match forget(repo, dest, repo_path, keep_months) {
            Ok(n) => (n, None),
            Err(e) => (0, Some(e)),
        }
    };
    Ok(BackupDone { snapshot, uploaded, forgotten, cleanup_error })
}

/// Removes backups older than `keep_months` (always keeps the latest) and
/// frees their space. Returns how many were removed.
pub fn forget(
    repo: &mut Repo,
    dest: Arc<dyn Destination>,
    repo_path: Option<&Path>,
    keep_months: u32,
) -> Result<usize> {
    let since = now_ms() - i64::from(keep_months) * MONTH_MS;
    let mut lock = Lock::acquire(dest, LockKind::Prune, &device())?;
    let result = forget_older(repo, since).and_then(|n| if n > 0 { prune(repo, 0.5).map(|_| n) } else { Ok(0) });
    lock.release();
    let n = result?;
    if let Some(path) = repo_path {
        LocalFolder::new(path).empty_old_trash()?;
    }
    Ok(n)
}

/// The listing of a folder inside a backup, given as a path of names.
/// Version 1 backups hold several folders: the first name picks one.
pub fn entries(repo: &Repo, snap: &Snapshot, path: &[&[u8]]) -> Result<Vec<Entry>> {
    let root = snap.roots.first().ok_or_else(|| Error::Format("backup without folders".into()))?;
    let mut names = path.to_vec();
    let tree = if snap.roots.len() > 1 || root.source.is_none() {
        let first = names.first().copied().unwrap_or_default();
        match snap.roots.iter().find(|r| r.name.as_bytes() == first) {
            Some(r) => {
                names.remove(0);
                r.tree.clone()
            }
            None if names.is_empty() => {
                return Ok(snap
                    .roots
                    .iter()
                    .map(|r| Entry {
                        name: r.name.as_bytes().to_vec(),
                        kind: EntryKind::Dir { subtree: r.tree.clone(), mount: false },
                        meta: Default::default(),
                    })
                    .collect());
            }
            None => return Err(Error::NotFound(String::from_utf8_lossy(first).into_owned())),
        }
    } else {
        root.tree.clone()
    };
    if names.is_empty() {
        return repo.load_tree(&tree);
    }
    match find(repo, &tree, &names)? {
        Some(Entry { kind: EntryKind::Dir { subtree, .. }, .. }) => repo.load_tree(&subtree),
        Some(entry) => Ok(vec![entry]),
        None => Err(Error::NotFound(String::from_utf8_lossy(&names.join(&b'/')).into_owned())),
    }
}

/// The latest backup this computer made of this home folder, if any.
/// Another user of the same computer has a home of their own: their
/// backups are not this one's.
pub fn latest_of_this_computer(repo: &Repo) -> Result<Option<Snapshot>> {
    latest_of(repo, &device(), &home()?)
}

fn latest_of(repo: &Repo, device: &str, home: &Path) -> Result<Option<Snapshot>> {
    let home = home.to_string_lossy();
    Ok(repo
        .list_snapshots()?
        .into_iter()
        .rev()
        .find(|s| s.device == device && s.home.as_ref().is_some_and(|h| h.path == home)))
}

/// The choices of what to save, as a backup recorded them (FORMAT-v2.md
/// 5.4), to carry them over when this computer connects to it again.
pub fn what_from_snapshot(snap: &Snapshot) -> What {
    let mut what = What::default();
    let Some(rules) = snap.roots.first().and_then(|r| r.left_out.as_ref()) else { return what };
    for rule in rules {
        match (rule.kind.as_str(), &rule.id, &rule.path) {
            ("preset", Some(id), _) => what.presets.push(id.clone()),
            ("path", _, Some(path)) => what.leave_out.push(path.clone()),
            ("cross-mount", _, Some(path)) => what.cross_mounts.push(path.clone()),
            ("pattern", _, _) => what.patterns.extend(rule.patterns.clone().unwrap_or_default()),
            _ => {}
        }
    }
    what
}

/// Picks a backup by id, the start of an id, or `latest`.
pub fn pick(repo: &Repo, wanted: &str) -> Result<Snapshot> {
    let snapshots = repo.list_snapshots()?;
    let found = if wanted == "latest" {
        snapshots.last().cloned()
    } else {
        let matching: Vec<_> = snapshots.iter().filter(|s| s.id.starts_with(wanted)).collect();
        if matching.len() > 1 {
            return Err(Error::Usage(format!("\"{wanted}\" matches more than one backup")));
        }
        matching.first().map(|s| (*s).clone())
    };
    found.ok_or_else(|| Error::NotFound(format!("backup \"{wanted}\"")))
}

/// "1.5 GB", in decimal units like the file managers.
pub fn human(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < units.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", units[unit]) }
}

/// The path as bytes, for comparisons with snapshot paths.
pub fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_home_refuses_escapes() {
        let home = Path::new("/home/u");
        assert_eq!(inside_home(home, Path::new("Documents/a")).unwrap(), b"Documents/a");
        assert_eq!(inside_home(home, Path::new("/home/u/x")).unwrap(), b"x");
        assert!(inside_home(home, Path::new("../v")).is_err());
        assert!(inside_home(home, Path::new("/etc")).is_err());
        assert!(inside_home(home, Path::new("a/../../v")).is_err());
    }

    #[test]
    fn what_comes_back_from_the_rules_a_backup_recorded() {
        use xkyhold_core::rules::Matcher;
        let ex = exclusions(
            Path::new("/home/u"),
            &What {
                leave_out: vec!["Videos".into()],
                presets: vec!["node-modules".into()],
                patterns: vec!["**/*.iso".into()],
                cross_mounts: vec!["Data".into()],
                only: vec![],
            },
            Some(Path::new("/home/u/Backups/XkyHold")),
        )
        .unwrap();
        let snap = Snapshot {
            format: Some(2),
            id: String::new(),
            time: String::new(),
            device: String::new(),
            home: None,
            roots: vec![xkyhold_core::snapshot::Root {
                name: "home".into(),
                tree: String::new(),
                source: None,
                meta: None,
                left_out: Some(Matcher::new(&ex).rules()),
            }],
            stats: Default::default(),
            parent: None,
        };
        let what = what_from_snapshot(&snap);
        assert_eq!(what.leave_out, ["Videos"]);
        assert_eq!(what.presets, ["node-modules"]);
        assert_eq!(what.patterns, ["**/*.iso"]);
        assert_eq!(what.cross_mounts, ["Data"]);
    }

    /// Two users of one computer share a backup: each sees only its own.
    #[test]
    fn the_latest_backup_is_of_this_home() {
        use xkyhold_core::backup::{BackupOptions, backup};
        use xkyhold_core::crypto::Argon2Params;
        use xkyhold_core::destination::MemoryDestination;
        let fast = Argon2Params { memory_kib: 1024, iterations: 1, parallelism: 1 };
        let dest: Arc<dyn Destination> = Arc::new(MemoryDestination::default());
        let (mut repo, _) = Repository::create(dest, "pw", fast, "t".into(), Options::default()).unwrap();
        let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for (home, time) in [(&one, "2026-11-14T10:00:00.000Z"), (&two, "2026-11-14T11:00:00.000Z")] {
            std::fs::write(home.path().join("f"), time).unwrap();
            let mut o = BackupOptions::new("pc".into(), time.into());
            o.mount_points = Some(std::collections::HashSet::new());
            backup(&mut repo, home.path(), o, &mut |_| {}).unwrap();
        }
        let of = |home: &Path, device: &str| latest_of(&repo, device, home).unwrap().map(|s| s.time);
        assert_eq!(of(one.path(), "pc").as_deref(), Some("2026-11-14T10:00:00.000Z"));
        assert_eq!(of(two.path(), "pc").as_deref(), Some("2026-11-14T11:00:00.000Z"));
        assert_eq!(of(Path::new("/home/new"), "pc"), None);
        assert_eq!(of(one.path(), "other"), None);
    }

    #[test]
    fn folder_states() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(inspect_folder(&dir.path().join("nope")), FolderState::Missing);
        assert_eq!(inspect_folder(dir.path()), FolderState::Empty);
        std::fs::write(dir.path().join("x"), b"x").unwrap();
        assert_eq!(inspect_folder(dir.path()), FolderState::Other);
        std::fs::remove_file(dir.path().join("x")).unwrap();
        std::fs::write(dir.path().join("config"), b"{}").unwrap();
        assert_eq!(inspect_folder(dir.path()), FolderState::Repository);
    }
}
