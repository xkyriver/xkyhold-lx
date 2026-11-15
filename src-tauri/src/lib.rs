//! The XkyHold-lx window. Tauri commands here stay thin: they call the
//! engine (`xkyhold-core`) through the glue shared with the CLI
//! (`xkyhold-app`) and pass errors to the UI as a `kind` string. Raw error
//! text goes to the log; the UI shows a sentence for each kind.
//!
//! Long work (backup, restore, check) runs on its own thread and reports
//! through the events `progress` and `job-done`.

#[cfg(debug_assertions)]
mod probe;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use xkyhold_app::scan::{self, Folder};
use xkyhold_app::{FolderState, LastBackup, Repo, Settings, Target, What};
use xkyhold_core::Error;
use xkyhold_core::bytes::{from_base64, to_base64};
use xkyhold_core::crypto::{Credential, DEFAULT_ARGON2};
use xkyhold_core::destination::Destination;
use xkyhold_core::repo::Repository;
use xkyhold_core::restore::{Conflict, RestoreOptions, restore};
use xkyhold_core::snapshot::Snapshot;
use xkyhold_core::time::now_iso;
use xkyhold_core::tree::EntryKind;
use xkyhold_dest_drive::DriveFolder;
use zeroize::Zeroizing;

/// An error as the UI sees it: a kind to pick a sentence, and the raw text
/// for the log.
#[derive(Debug, Serialize)]
pub struct UiError {
    kind: String,
    message: String,
}

impl From<Error> for UiError {
    fn from(e: Error) -> Self {
        let kind = match &e {
            // Errors from the glue that the UI words differently.
            Error::Usage(m) if m.contains("keyring") => "keyring",
            Error::Usage(m) if m.contains("Google client") => "no-client",
            Error::Usage(m) if m.contains("systemctl") => "systemd",
            _ => e.kind(),
        };
        xkyhold_app::log(&format!("{kind}: {e}"));
        UiError { kind: kind.into(), message: e.to_string() }
    }
}

fn ui(kind: &str, message: &str) -> UiError {
    xkyhold_app::log(&format!("{kind}: {message}"));
    UiError { kind: kind.into(), message: message.into() }
}

type Res<T> = std::result::Result<T, UiError>;

/// The open backup.
struct Session {
    repo: Repo,
    dest: Arc<dyn Destination>,
    repo_path: Option<PathBuf>,
}

#[derive(Default)]
struct AppState {
    /// Held by a job for as long as it runs.
    session: Mutex<Option<Session>>,
    /// The kind of job running, if any.
    job: Mutex<Option<&'static str>>,
    cancel: Arc<AtomicBool>,
    scan_cancel: Mutex<Arc<AtomicBool>>,
    /// Stops waiting for the browser during a Google sign-in.
    signin_cancel: Mutex<Arc<AtomicBool>>,
}

impl AppState {
    /// The session, unless a job holds it.
    fn session(&self) -> Res<std::sync::MutexGuard<'_, Option<Session>>> {
        self.session.try_lock().map_err(|_| ui("busy", "a job is running"))
    }

    fn running(&self) -> Option<&'static str> {
        *self.job.lock().unwrap()
    }
}

/// Runs blocking work off the async runtime's threads.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Res<T> + Send + 'static) -> Res<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| ui("other", &e.to_string()))?
}

#[derive(Serialize)]
struct Status {
    version: &'static str,
    home: String,
    settings: Settings,
    /// A backup is set up on this computer.
    configured: bool,
    /// It is open (unlocked) in this window.
    open: bool,
    /// It accepts new backups (format 2).
    writable: bool,
    remembered: bool,
    job: Option<&'static str>,
    /// A backup runs on this computer outside this window (the timer, the CLI).
    running_here: bool,
    /// The hourly timer is installed.
    timer: bool,
    google_client: bool,
    keyring: bool,
    /// How "Update" works here: "appimage", "appman", "deb", "rpm" or "other".
    install: &'static str,
}

/// How this program was installed, found once.
fn install() -> &'static xkyhold_app::update::Install {
    static INSTALL: std::sync::OnceLock<xkyhold_app::update::Install> = std::sync::OnceLock::new();
    INSTALL.get_or_init(xkyhold_app::update::installed)
}

fn install_kind() -> &'static str {
    use xkyhold_app::update::Install;
    match install() {
        Install::AppImage { .. } => "appimage",
        Install::AppMan { .. } => "appman",
        Install::Deb => "deb",
        Install::Rpm => "rpm",
        Install::Other => "other",
    }
}

#[tauri::command]
async fn status(state: State<'_, AppState>) -> Res<Status> {
    let settings = Settings::load()?;
    let (open, writable) = match state.session.try_lock() {
        Ok(s) => (s.is_some(), s.as_ref().is_some_and(|s| s.repo.writable())),
        Err(_) => (true, true),
    };
    let remembered = settings.repo_id.as_deref().is_some_and(xkyhold_app::key_remembered);
    Ok(Status {
        version: env!("CARGO_PKG_VERSION"),
        home: xkyhold_app::home()?.display().to_string(),
        configured: settings.backup.is_some(),
        settings,
        open,
        writable,
        remembered,
        job: state.running(),
        running_here: state.running().is_none() && xkyhold_app::schedule::running_here(),
        timer: xkyhold_app::schedule::timer_enabled(),
        google_client: xkyhold_dest_drive::Client::from_env().is_some(),
        keyring: xkyhold_secrets::SecretService::connect().is_ok(),
        install: install_kind(),
    })
}

/// Opens the configured backup with the key in the keyring. False when
/// the password is needed.
#[tauri::command]
async fn open_saved(state: State<'_, AppState>) -> Res<bool> {
    if state.session()?.is_some() {
        return Ok(true);
    }
    let session = blocking(|| {
        let settings = Settings::load()?;
        let target = settings.backup.clone().ok_or_else(|| ui("usage", "no backup set up"))?;
        let (dest, repo_path) = xkyhold_app::destination(&target, &settings, None)?;
        Ok(xkyhold_app::open_with_keyring(dest.clone())?.map(|repo| Session { repo, dest, repo_path }))
    })
    .await?;
    let opened = session.is_some();
    *state.session()? = session;
    Ok(opened)
}

#[tauri::command]
async fn sign_in(app: AppHandle, state: State<'_, AppState>) -> Res<String> {
    let cancel = Arc::new(AtomicBool::new(false));
    *state.signin_cancel.lock().unwrap() = cancel.clone();
    blocking(move || {
        let account = xkyhold_app::google_sign_in_until(
            &mut |url| {
                let _ = app.emit("signin-url", url);
                if std::process::Command::new("xdg-open").arg(url).spawn().is_err() {
                    xkyhold_app::log("xdg-open failed; the window shows the sign-in address");
                }
            },
            &cancel,
        )?;
        Settings::update(|s| {
            s.account = Some(account.clone());
            Ok(())
        })?;
        Ok(account)
    })
    .await
}

#[tauri::command]
async fn sign_out(state: State<'_, AppState>) -> Res<()> {
    let settings = Settings::update(|s| {
        if let Some(account) = s.account.take() {
            xkyhold_app::google_sign_out(&account)?;
        }
        Ok(())
    })?;
    // A Drive backup cannot be used without the account.
    if matches!(settings.backup, Some(Target::Drive { .. })) {
        *state.session()? = None;
    }
    Ok(())
}

#[tauri::command]
async fn drive_backups() -> Res<Vec<String>> {
    blocking(|| Ok(DriveFolder::list_repositories(&xkyhold_app::drive_client(&Settings::load()?, None)?)?)).await
}

/// What a folder chosen for a local backup holds.
#[tauri::command]
async fn inspect_folder(path: String) -> Res<&'static str> {
    let path = PathBuf::from(&path);
    if !path.is_absolute() {
        return Err(ui("usage", "the folder must be a full path"));
    }
    let home = xkyhold_app::home()?;
    if home.starts_with(&path) {
        return Ok("contains-home");
    }
    Ok(match xkyhold_app::inspect_folder(&path) {
        FolderState::Missing => "missing",
        FolderState::Empty => "empty",
        FolderState::Repository => "repository",
        FolderState::Other => "other",
    })
}

/// The system's folder chooser.
#[tauri::command]
async fn pick_folder(app: AppHandle) -> Res<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    blocking(move || {
        let picked = app.dialog().file().blocking_pick_folder();
        Ok(picked.and_then(|p| p.into_path().ok()).map(|p| p.display().to_string()))
    })
    .await
}

/// A password or the recovery key, typed in the window.
#[derive(Deserialize)]
struct Secret {
    password: Option<String>,
    recovery_key: Option<String>,
}

impl Secret {
    fn credential(self) -> Res<Credential> {
        match (self.password, self.recovery_key) {
            (_, Some(k)) => Ok(Credential::RecoveryKey(Zeroizing::new(k))),
            (Some(p), None) => Ok(Credential::Password(Zeroizing::new(p))),
            _ => Err(ui("usage", "no password given")),
        }
    }
}

/// Puts the hourly timer in step with the settings. A system without
/// systemd still works; only reminders and automatic backups with the
/// window closed are missing.
fn sync_timer() {
    // A test build never touches the timer (docs/UPDATES.md, phase U4).
    if xkyhold_app::update::test_build() {
        return;
    }
    // Development: a window on a test home must not touch the timer of the
    // person's real one (the units have the same name).
    #[cfg(debug_assertions)]
    if std::env::var_os("XKYHOLD_LX_NO_TIMER").is_some() {
        return;
    }
    match Settings::load().and_then(|s| xkyhold_app::schedule::sync_units(&s)) {
        Ok(()) => {}
        Err(e) => xkyhold_app::log(&format!("the timer could not be updated: {e}")),
    }
}

/// Records the backup as this computer's and keeps it open.
fn adopt(state: &AppState, target: Target, session: Session, remember: bool) -> Res<()> {
    let latest = xkyhold_app::latest_of_this_computer(&session.repo)?;
    Settings::update(|s| {
        if s.repo_id.as_deref() != Some(&session.repo.config.repo_id) {
            s.last_backup = latest.as_ref().map(|l| LastBackup {
                time: l.time.clone(),
                files: l.stats.files,
                bytes: l.stats.bytes,
                skipped: l.stats.skipped,
            });
            // Back to a backup this computer made: keep choosing as it did.
            if s.what == What::default()
                && let Some(l) = &latest
            {
                s.what = xkyhold_app::what_from_snapshot(l);
            }
        }
        s.backup = Some(target);
        s.repo_id = Some(session.repo.config.repo_id.clone());
        Ok(())
    })?;
    sync_timer();
    if remember && let Err(e) = xkyhold_app::remember_key(&session.repo, true) {
        // The backup works without it; the password is asked each time.
        xkyhold_app::log(&format!("could not keep the key in the keyring: {e}"));
    }
    *state.session()? = Some(session);
    Ok(())
}

/// Creates a new backup and returns its recovery key.
#[tauri::command]
async fn create_backup(state: State<'_, AppState>, target: Target, password: String, remember: bool) -> Res<String> {
    let t = target.clone();
    let (session, recovery) = blocking(move || {
        let settings = Settings::load()?;
        match &t {
            Target::Drive { name } => {
                let existing = DriveFolder::list_repositories(&xkyhold_app::drive_client(&settings, None)?)?;
                if existing.iter().any(|n| n == name) {
                    return Err(ui("exists", "a backup with this name already exists"));
                }
            }
            Target::Local { path } => match xkyhold_app::inspect_folder(path) {
                FolderState::Missing => std::fs::create_dir_all(path).map_err(Error::from)?,
                FolderState::Empty => {}
                FolderState::Repository => return Err(ui("exists", "this folder already holds a backup")),
                FolderState::Other => return Err(ui("not-empty", "this folder holds other files")),
            },
        }
        let (dest, repo_path) = xkyhold_app::destination(&t, &settings, None)?;
        let (repo, recovery) =
            Repository::create(dest.clone(), &password, DEFAULT_ARGON2, now_iso(), xkyhold_app::cache_options())?;
        Ok((Session { repo, dest, repo_path }, recovery.to_string()))
    })
    .await?;
    adopt(&state, target, session, remember)?;
    Ok(recovery)
}

/// Opens an existing backup with the password or recovery key and makes it
/// this computer's backup.
#[tauri::command]
async fn connect_backup(state: State<'_, AppState>, target: Target, secret: Secret, remember: bool) -> Res<bool> {
    let t = target.clone();
    let session = blocking(move || {
        let settings = Settings::load()?;
        let (dest, repo_path) = xkyhold_app::destination(&t, &settings, None)?;
        let repo = xkyhold_app::open_with(dest.clone(), &secret.credential()?)?;
        Ok(Session { repo, dest, repo_path })
    })
    .await?;
    let writable = session.repo.writable();
    adopt(&state, target, session, remember)?;
    Ok(writable)
}

/// Opens this computer's backup with the password or recovery key.
#[tauri::command]
async fn unlock(state: State<'_, AppState>, secret: Secret, remember: bool) -> Res<()> {
    let target = Settings::load()?.backup.ok_or_else(|| ui("usage", "no backup set up"))?;
    connect_backup(state, target, secret, remember).await.map(|_| ())
}

#[derive(Serialize, Clone)]
struct ProgressEvent {
    kind: &'static str,
    files: u64,
    bytes: u64,
    current: String,
}

/// Sends progress at most a few times a second.
struct Throttle {
    app: AppHandle,
    last: Instant,
}

impl Throttle {
    fn new(app: &AppHandle) -> Self {
        Self { app: app.clone(), last: Instant::now() - Duration::from_secs(1) }
    }

    fn send(&mut self, kind: &'static str, files: u64, bytes: u64, current: &str) {
        if self.last.elapsed() >= Duration::from_millis(200) {
            self.last = Instant::now();
            let _ = self.app.emit("progress", ProgressEvent { kind, files, bytes, current: current.into() });
        }
    }
}

/// Starts `work` on a thread with the open backup. `job-done` carries its
/// summary or error.
fn start_job(
    app: &AppHandle,
    state: &AppState,
    kind: &'static str,
    work: impl FnOnce(&mut Session, &AtomicBool, &mut Throttle) -> Res<Value> + Send + 'static,
) -> Res<()> {
    {
        let mut job = state.job.lock().unwrap();
        if job.is_some() {
            return Err(ui("busy", "a job is running"));
        }
        if state.session()?.is_none() {
            return Err(ui("locked", "the backup is not open"));
        }
        *job = Some(kind);
    }
    state.cancel.store(false, Ordering::Relaxed);
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let result = {
            let mut guard = state.session.lock().unwrap();
            let session = guard.as_mut().expect("checked above");
            let mut throttle = Throttle::new(&app);
            work(session, &state.cancel, &mut throttle)
        };
        *state.job.lock().unwrap() = None;
        let done = match result {
            Ok(summary) => json!({ "kind": kind, "ok": true, "summary": summary }),
            Err(e) => json!({ "kind": kind, "ok": false, "error": e }),
        };
        let _ = app.emit("job-done", done);
    });
    Ok(())
}

#[tauri::command]
async fn backup_now(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    let settings = Settings::load()?;
    start_job(&app, &state, "backup", move |s, cancel, progress| {
        let app = progress.app.clone();
        let done = xkyhold_app::run_backup(
            &mut s.repo,
            s.dest.clone(),
            s.repo_path.as_deref(),
            &settings.what,
            settings.keep_months,
            Some(cancel),
            &mut |p| progress.send("backup", p.files, p.bytes, &p.current),
            &mut || {
                let event = ProgressEvent { kind: "cleanup", files: 0, bytes: 0, current: String::new() };
                let _ = app.emit("progress", event);
            },
        )?;
        let st = &done.snapshot.stats;
        Settings::update(|s| {
            s.last_backup = Some(LastBackup {
                time: done.snapshot.time.clone(),
                files: st.files,
                bytes: st.bytes,
                skipped: st.skipped,
            });
            Ok(())
        })?;
        if let Some(e) = &done.cleanup_error {
            xkyhold_app::log(&format!("removing old backups failed: {e}"));
        }
        Ok(json!({
            "files": st.files, "bytes": st.bytes, "skipped": st.skipped,
            "changed": st.changed, "mounts": st.mounts, "uploaded": done.uploaded,
            "forgotten": done.forgotten, "cleanup_failed": done.cleanup_error.is_some(),
        }))
    })
}

/// The browser may never come back (closed, or the wrong one opened).
#[tauri::command]
fn cancel_sign_in(state: State<'_, AppState>) {
    state.signin_cancel.lock().unwrap().store(true, Ordering::Relaxed);
}

#[tauri::command]
fn stop_job(state: State<'_, AppState>) {
    state.cancel.store(true, Ordering::Relaxed);
}

#[derive(Serialize)]
struct SnapInfo {
    id: String,
    time: String,
    device: String,
    files: u64,
    bytes: u64,
    skipped: u64,
    /// Made by XkyHold-lx from a home folder (can go back to its place).
    home: bool,
}

#[tauri::command]
async fn snapshots(state: State<'_, AppState>) -> Res<Vec<SnapInfo>> {
    let guard = state.session()?;
    let session = guard.as_ref().ok_or_else(|| ui("locked", "the backup is not open"))?;
    let list = session.repo.list_snapshots()?;
    Ok(list
        .into_iter()
        .rev()
        .map(|s| SnapInfo {
            home: is_home(&s),
            id: s.id,
            time: s.time,
            device: s.device,
            files: s.stats.files,
            bytes: s.stats.bytes,
            skipped: s.stats.skipped,
        })
        .collect())
}

fn is_home(s: &Snapshot) -> bool {
    s.home.is_some() && s.roots.len() == 1 && s.roots[0].source.as_ref().is_some_and(|r| r.kind == "home")
}

#[derive(Serialize)]
struct EntryInfo {
    /// The name's bytes in base64, to ask for exactly this entry.
    id: String,
    name: String,
    kind: &'static str,
    size: u64,
    mtime: Option<i64>,
}

fn decode_path(path: &[String]) -> Res<Vec<Vec<u8>>> {
    path.iter().map(|p| from_base64(p).map_err(UiError::from)).collect()
}

fn find_snapshot(repo: &Repo, id: &str) -> Res<Snapshot> {
    Ok(xkyhold_app::pick(repo, id)?)
}

#[tauri::command]
async fn entries(state: State<'_, AppState>, snapshot: String, path: Vec<String>) -> Res<Vec<EntryInfo>> {
    let guard = state.session()?;
    let session = guard.as_ref().ok_or_else(|| ui("locked", "the backup is not open"))?;
    let snap = find_snapshot(&session.repo, &snapshot)?;
    let names = decode_path(&path)?;
    let refs: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
    let mut list: Vec<EntryInfo> = xkyhold_app::entries(&session.repo, &snap, &refs)?
        .into_iter()
        .map(|e| {
            let (kind, size) = match &e.kind {
                EntryKind::File { size, .. } => ("file", *size),
                EntryKind::Dir { mount: true, .. } => ("mount", 0),
                EntryKind::Dir { .. } => ("dir", 0),
                EntryKind::Symlink { .. } => ("symlink", 0),
                EntryKind::Unknown(_) => ("unknown", 0),
            };
            EntryInfo { id: to_base64(&e.name), name: e.display_name(), kind, size, mtime: e.meta.mtime }
        })
        .collect();
    list.sort_by_key(|e| (e.kind != "dir", e.name.to_lowercase()));
    Ok(list)
}

/// Where a restore goes: the original place inside home, or a new folder
/// `<base>/<name>`, where the base is `~/XkyHold-restore` or a folder the
/// user chose (anywhere they can write, an external disk for example), and
/// the window names it by the backup's local date and time.
fn restore_target(
    home: &Path,
    snap: &Snapshot,
    dir: &[Vec<u8>],
    original: bool,
    name: Option<&str>,
    base: Option<&Path>,
) -> Res<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    if original {
        if !is_home(snap) {
            return Err(ui("usage", "this backup has no original place on this computer"));
        }
        let mut target = home.to_path_buf();
        for name in dir {
            target.push(std::ffi::OsStr::from_bytes(name));
        }
        return Ok(target);
    }
    let fallback = snap.time.get(..16).unwrap_or(&snap.time).replace('T', " ").replace(':', ".");
    let name = name.unwrap_or(&fallback);
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(ui("usage", "not a folder name"));
    }
    match base {
        Some(base) if !base.is_absolute() => Err(ui("usage", "the folder must be a full path")),
        Some(base) => Ok(base.join(name)),
        None => Ok(home.join("XkyHold-restore").join(name)),
    }
}

/// The chosen items that already exist in their original place.
#[tauri::command]
async fn restore_existing(
    state: State<'_, AppState>,
    snapshot: String,
    path: Vec<String>,
    ids: Vec<String>,
) -> Res<Vec<String>> {
    let guard = state.session()?;
    let session = guard.as_ref().ok_or_else(|| ui("locked", "the backup is not open"))?;
    let snap = find_snapshot(&session.repo, &snapshot)?;
    let target = restore_target(&xkyhold_app::home()?, &snap, &decode_path(&path)?, true, None, None)?;
    let mut found = Vec::new();
    for id in decode_path(&ids)? {
        use std::os::unix::ffi::OsStrExt;
        if target.join(std::ffi::OsStr::from_bytes(&id)).symlink_metadata().is_ok() {
            found.push(String::from_utf8_lossy(&id).into_owned());
        }
    }
    Ok(found)
}

// One argument per choice in the window, as Tauri passes them.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn restore_start(
    app: AppHandle,
    state: State<'_, AppState>,
    snapshot: String,
    path: Vec<String>,
    ids: Vec<String>,
    original: bool,
    folder: Option<String>,
    to: Option<String>,
    conflict: String,
) -> Res<()> {
    let dir = decode_path(&path)?;
    let wanted = decode_path(&ids)?;
    let conflict = match conflict.as_str() {
        "skip" => Conflict::Skip,
        "replace" => Conflict::Replace,
        _ => Conflict::KeepBoth,
    };
    start_job(&app, &state, "restore", move |s, cancel, progress| {
        let home = xkyhold_app::home()?;
        let snap = find_snapshot(&s.repo, &snapshot)?;
        let refs: Vec<&[u8]> = dir.iter().map(Vec::as_slice).collect();
        let entries: Vec<_> =
            xkyhold_app::entries(&s.repo, &snap, &refs)?.into_iter().filter(|e| wanted.contains(&e.name)).collect();
        let target = restore_target(&home, &snap, &dir, original, folder.as_deref(), to.as_deref().map(Path::new))?;
        // The original place is always inside home; another folder is the user's choice.
        if original {
            xkyhold_app::inside_home(&home, &target)?;
        }
        std::fs::create_dir_all(&target).map_err(Error::from)?;
        let r = restore(&s.repo, &entries, &target, &RestoreOptions { conflict, cancel: Some(cancel) }, &mut |r| {
            progress.send("restore", r.files, r.bytes, &r.current)
        })?;
        Ok(json!({
            "files": r.files, "bytes": r.bytes, "folders": r.folders, "existing": r.existing,
            "bad_names": r.bad_names, "unknown": r.unknown, "xattr_failures": r.xattr_failures,
            "folder": target.display().to_string(),
        }))
    })
}

#[tauri::command]
async fn check_start(app: AppHandle, state: State<'_, AppState>, full: bool) -> Res<()> {
    start_job(&app, &state, "check", move |s, cancel, progress| {
        let report =
            xkyhold_core::check::check(&s.repo, full, Some(cancel), &mut |n| progress.send("check", n as u64, 0, ""))?;
        Ok(json!({
            "ok": report.ok(), "packs": report.packs, "blobs": report.blobs_checked,
            "missing_packs": report.missing_packs.len(), "damaged_packs": report.damaged_packs.len(),
            "missing_blobs": report.missing_blobs, "damaged_blobs": report.damaged_blobs,
        }))
    })
}

/// Settings the window changes; absent fields stay as they are.
#[derive(Deserialize)]
struct SettingsPatch {
    what: Option<What>,
    remind_days: Option<u32>,
    keep_months: Option<u32>,
    language: Option<String>,
    theme: Option<String>,
    automatic: Option<String>,
    notify_success: Option<bool>,
    check_updates: Option<bool>,
}

#[tauri::command]
async fn save_settings(patch: SettingsPatch) -> Res<Settings> {
    if let Some(a) = &patch.automatic
        && a != "off"
    {
        if !["hourly", "daily", "weekly"].contains(&a.as_str()) {
            return Err(ui("usage", "unknown frequency"));
        }
        // Automatic backups run without anyone to type the password.
        let id = Settings::load()?.repo_id.unwrap_or_default();
        if !xkyhold_app::key_remembered(&id) {
            return Err(ui("needs-key", "automatic backups need the key in the keyring"));
        }
    }
    let schedule_changed = patch.automatic.is_some() || patch.remind_days.is_some();
    if let Some(what) = &patch.what {
        // Refuse anything outside home now rather than at the next backup.
        let home = xkyhold_app::home()?;
        xkyhold_app::exclusions(&home, what, None)?;
        for p in &what.only {
            xkyhold_app::inside_home(&home, Path::new(p))?;
        }
    }
    let saved = Settings::update(|s| {
        if let Some(what) = patch.what {
            s.what = what;
        }
        if let Some(v) = patch.remind_days {
            s.remind_days = v;
        }
        if let Some(v) = patch.keep_months {
            s.keep_months = v;
        }
        if let Some(v) = patch.language {
            s.language = Some(v).filter(|l| l == "en" || l == "pt-PT");
        }
        if let Some(v) = patch.theme {
            s.theme = if ["light", "dark"].contains(&v.as_str()) { v } else { "system".into() };
        }
        if let Some(v) = patch.automatic {
            s.automatic = v;
        }
        if let Some(v) = patch.notify_success {
            s.notify_success = v;
        }
        if let Some(v) = patch.check_updates {
            s.check_updates = v;
        }
        Ok(())
    })?;
    if schedule_changed {
        let s = saved.clone();
        blocking(move || Ok(xkyhold_app::schedule::sync_units(&s)?)).await?;
    }
    Ok(saved)
}

#[tauri::command]
async fn change_password(state: State<'_, AppState>, secret: Secret, new_password: String) -> Res<()> {
    let dest = {
        let guard = state.session()?;
        guard.as_ref().ok_or_else(|| ui("locked", "the backup is not open"))?.dest.clone()
    };
    blocking(move || {
        Repository::change_password(&dest, &secret.credential()?, &new_password, DEFAULT_ARGON2)?;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn set_remember(state: State<'_, AppState>, remember: bool) -> Res<()> {
    {
        let guard = state.session()?;
        let session = guard.as_ref().ok_or_else(|| ui("locked", "the backup is not open"))?;
        xkyhold_app::remember_key(&session.repo, remember)?;
    }
    // Without the key, automatic backups cannot run: back to reminders.
    if !remember {
        Settings::update(|s| {
            s.automatic = "off".into();
            Ok(())
        })?;
        sync_timer();
    }
    Ok(())
}

/// Removes this computer's settings, the backup's key and the Google
/// sign-in. The backup itself stays where it is.
#[tauri::command]
async fn forget_computer(state: State<'_, AppState>) -> Res<()> {
    if state.running().is_some() {
        return Err(ui("busy", "a job is running"));
    }
    let settings = Settings::load()?;
    if let Some(id) = &settings.repo_id {
        xkyhold_app::forget_key(id)?;
    }
    if let Some(account) = &settings.account {
        xkyhold_app::google_sign_out(account)?;
    }
    Settings::update(|s| {
        *s = Settings { language: s.language.take(), theme: std::mem::take(&mut s.theme), ..Settings::default() };
        Ok(())
    })?;
    sync_timer();
    *state.session()? = None;
    Ok(())
}

#[derive(Serialize)]
struct FolderInfo {
    name: String,
    text_name: bool,
    mount: bool,
    cloud: bool,
    always_out: bool,
}

#[tauri::command]
async fn folders(path: String) -> Res<Vec<FolderInfo>> {
    let list = scan::folders(&xkyhold_app::home()?, &path)?;
    Ok(list
        .into_iter()
        .map(|Folder { name, text_name, mount, cloud, always_out }| FolderInfo {
            name,
            text_name,
            mount,
            cloud,
            always_out,
        })
        .collect())
}

#[tauri::command]
async fn mounts() -> Res<Vec<String>> {
    Ok(scan::mounts_inside(&xkyhold_app::home()?))
}

/// Counts the space of each suggested exclusion; the answer comes as the
/// `sizes` event. A new request stops the previous count.
#[tauri::command]
async fn count_sizes(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    let cancel = Arc::new(AtomicBool::new(false));
    std::mem::replace(&mut *state.scan_cancel.lock().unwrap(), cancel.clone()).store(true, Ordering::Relaxed);
    let home = xkyhold_app::home()?;
    std::thread::spawn(move || {
        if let Ok(sizes) = scan::preset_sizes(&home, &cancel) {
            let _ = app.emit("sizes", sizes);
        }
    });
    Ok(())
}

/// Opens a folder in the file manager (one the window just restored into).
#[tauri::command]
async fn open_folder(path: String) -> Res<()> {
    if !Path::new(&path).is_dir() {
        return Err(ui("usage", "not a folder"));
    }
    std::process::Command::new("xdg-open").arg(&path).spawn().map_err(Error::from)?;
    Ok(())
}

/// A newer version to show, if any (docs/UPDATES.md 4.2). Without `now`
/// it looks at most once a day, and a failure shows nothing; with `now`
/// (Settings → "Check now") a failure is reported.
#[tauri::command]
async fn update_check(now: bool) -> Res<Option<xkyhold_app::update::Available>> {
    blocking(move || match xkyhold_app::update::check(now) {
        Ok(found) => Ok(found),
        Err(e) if now => Err(ui("update-check", &e.to_string())),
        Err(e) => {
            xkyhold_app::log(&format!("update check: {e}"));
            Ok(Settings::load().ok().as_ref().and_then(xkyhold_app::update::shown))
        }
    })
    .await
}

#[derive(Clone, Serialize)]
struct UpdateProgress {
    done: u64,
    total: u64,
}

/// "Update" (docs/UPDATES.md 4.4). An AppImage, also under AppMan, is
/// replaced in place after its signature checks out; then the window offers
/// to restart. Elsewhere, for now, the release page opens. Never while a
/// job runs here or a backup runs in another process. Returns whether the
/// new version is in place.
#[tauri::command]
async fn update_install(app: AppHandle, state: State<'_, AppState>) -> Res<bool> {
    use xkyhold_app::update::{self, Install};
    if state.running().is_some() {
        return Err(ui("busy", "a job is running"));
    }
    let how = install().clone();
    let path = match &how {
        Install::AppImage { path } | Install::AppMan { path } if update::can_replace(path) => path.clone(),
        _ => {
            let page = Settings::load()?
                .update_latest
                .map(|a| a.page)
                .unwrap_or_else(|| format!("{}latest", update::RELEASES));
            open_release(page).await?;
            return Ok(false);
        }
    };
    blocking(move || {
        // A backup by the timer or the CLI, or another window: wait.
        let _here = xkyhold_app::schedule::LocalLock::take()?;
        let latest = update::fetch_latest().map_err(|e| ui("update-check", &e.to_string()))?;
        if !update::is_newer(&latest.version, update::CURRENT) {
            return Err(ui("update-none", "no newer version"));
        }
        let kind = how.package().expect("an AppImage");
        let file = latest.files.get(kind).ok_or_else(|| ui("update-check", "no AppImage in latest.json"))?;
        let mut last = 0;
        let data = update::download(&file.url, file.size, &mut |done, total| {
            // A few hundred events for a 100 MB file, not one per read.
            if done - last >= 512 * 1024 || done == total {
                last = done;
                let _ = app.emit("update-progress", UpdateProgress { done, total });
            }
        })
        .map_err(|e| ui("update-download", &e.to_string()))?;
        latest
            .verify_package(kind, &data, &update::public_key()?)
            .map_err(|e| ui("update-signature", &e.to_string()))?;
        update::replace_appimage(&path, &data).map_err(|e| ui("update-install", &e.to_string()))?;
        if matches!(how, Install::AppMan { .. }) {
            update::tell_appman(&path, &file.url).map_err(|e| ui("update-install", &e.to_string()))?;
        }
        xkyhold_app::log(&format!("updated to {} in place: {}", latest.version, path.display()));
        Ok(true)
    })
    .await
}

/// After an update: starts the new version and closes this one. The new
/// process must not inherit anything that points into this AppImage's
/// mount, which goes away when this one closes.
#[tauri::command]
fn update_restart(app: AppHandle) -> Res<()> {
    use xkyhold_app::update::Install;
    let (Install::AppImage { path } | Install::AppMan { path }) = install() else {
        return Err(ui("usage", "not an AppImage"));
    };
    let mount = std::env::var("APPDIR").unwrap_or_default();
    let mut cmd = std::process::Command::new(path);
    cmd.env_clear();
    for (key, value) in std::env::vars() {
        if ["APPDIR", "APPIMAGE", "ARGV0", "OWD"].contains(&key.as_str()) {
            continue;
        }
        if mount.is_empty() || !value.contains(&mount) {
            cmd.env(&key, &value);
        } else if value.contains(':') {
            // A list such as PATH: keep the parts outside the mount.
            let kept: Vec<&str> = value.split(':').filter(|p| !p.starts_with(&mount)).collect();
            if !kept.is_empty() {
                cmd.env(&key, kept.join(":"));
            }
        }
    }
    cmd.spawn().map_err(Error::from)?;
    app.state::<AppState>().cancel.store(true, Ordering::Relaxed);
    app.exit(0);
    Ok(())
}

/// "Not now" on a new version.
#[tauri::command]
async fn update_skip(version: String) -> Res<()> {
    Ok(xkyhold_app::update::skip(&version)?)
}

/// Opens a release page in the browser; nothing outside the releases.
#[tauri::command]
async fn open_release(url: String) -> Res<()> {
    if !url.starts_with(xkyhold_app::update::RELEASES) || url.contains(char::is_whitespace) {
        return Err(ui("usage", "not a release page"));
    }
    std::process::Command::new("xdg-open").arg(&url).spawn().map_err(Error::from)?;
    Ok(())
}

/// The window may close; when a job runs it asks first.
#[tauri::command]
fn close_now(app: AppHandle) {
    app.state::<AppState>().cancel.store(true, Ordering::Relaxed);
    app.exit(0);
}

pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.state::<AppState>().running().is_some()
            {
                api.prevent_close();
                let _ = window.emit("close-requested", ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            status,
            open_saved,
            sign_in,
            sign_out,
            cancel_sign_in,
            drive_backups,
            inspect_folder,
            pick_folder,
            create_backup,
            connect_backup,
            unlock,
            backup_now,
            stop_job,
            snapshots,
            entries,
            restore_existing,
            restore_start,
            check_start,
            save_settings,
            change_password,
            set_remember,
            forget_computer,
            folders,
            mounts,
            count_sizes,
            open_folder,
            close_now,
            update_check,
            update_skip,
            open_release,
            update_install,
            update_restart,
        ]);
    #[cfg(debug_assertions)]
    let builder = builder.setup(|app| {
        probe::start(app.handle());
        Ok(())
    });
    // Keep the timer pointing at this program (it may have moved or been
    // updated since the timer was written).
    std::thread::spawn(sync_timer);
    // This version opened: the AppImage it replaced is no longer needed.
    xkyhold_app::update::tidy_after_update();
    builder.run(tauri::generate_context!()).expect("error while running XkyHold-lx");
}
