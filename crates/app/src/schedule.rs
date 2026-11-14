//! Automatic backups and reminders while the window is closed (PLAN.md 4.8).
//!
//! One systemd user timer wakes `xkyhold-lx --scheduled` every hour. Each
//! run decides from the settings: an automatic backup when one is due, else
//! a reminder notification when one is due, else nothing. Automatic backups
//! are opt-in; reminders are the default, as in XkyHold.
//!
//! The unit files live in the app's config folder and are linked into
//! systemd, so turning both off removes every trace.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use xkyhold_core::time::{iso, now_ms, parse_iso};
use xkyhold_core::{Error, Result};

use crate::{BackupDone, LastBackup, Settings, human, xdg};

pub const SERVICE: &str = "xkyhold-lx.service";
pub const TIMER: &str = "xkyhold-lx.timer";

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;

/// Held while a backup runs on this computer. Released when dropped, also
/// when the process dies, so it never goes stale.
pub struct LocalLock(#[allow(dead_code)] File);

fn lock_path() -> Result<PathBuf> {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if Path::new(&dir).is_absolute() => Ok(PathBuf::from(dir).join("xkyhold-lx.lock")),
        _ => {
            let dir = xdg("XDG_STATE_HOME", ".local/state")?;
            std::fs::create_dir_all(&dir)?;
            Ok(dir.join("backup.lock"))
        }
    }
}

impl LocalLock {
    /// Takes the lock, or fails with [`Error::RunningHere`] at once.
    pub fn take() -> Result<Self> {
        Self::take_at(&lock_path()?)
    }

    fn take_at(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self(file)),
            Err(std::fs::TryLockError::WouldBlock) => Err(Error::RunningHere),
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }
}

/// Is a backup running on this computer right now (in another process)?
pub fn running_here() -> bool {
    matches!(LocalLock::take(), Err(Error::RunningHere))
}

/// What the hourly run should do.
#[derive(Debug, PartialEq, Eq)]
pub enum Due {
    Backup,
    Remind,
    Nothing,
}

/// The time between automatic backups, a little short so that an hourly
/// timer does not drift a whole hour later each time.
fn interval(automatic: &str) -> Option<i64> {
    match automatic {
        "hourly" => Some(HOUR - 5 * 60_000),
        "daily" => Some(DAY - HOUR),
        "weekly" => Some(7 * DAY - HOUR),
        _ => None,
    }
}

fn since(time: Option<&str>, now: i64) -> Option<i64> {
    time.and_then(parse_iso).map(|t| now - t)
}

pub fn due(s: &Settings, now: i64) -> Due {
    if s.backup.is_none() {
        return Due::Nothing;
    }
    let last = since(s.last_backup.as_ref().map(|l| l.time.as_str()), now);
    if let Some(every) = interval(&s.automatic) {
        return if last.is_none_or(|l| l >= every) { Due::Backup } else { Due::Nothing };
    }
    if s.remind_days == 0 {
        return Due::Nothing;
    }
    let overdue = last.is_none_or(|l| l >= i64::from(s.remind_days) * DAY - HOUR);
    let quiet = since(s.last_reminded.as_deref(), now).is_some_and(|r| r < 20 * HOUR);
    if overdue && !quiet { Due::Remind } else { Due::Nothing }
}

fn portuguese(s: &Settings) -> bool {
    match s.language.as_deref() {
        Some(l) => l == "pt-PT",
        None => std::env::var("LANG").is_ok_and(|l| l.starts_with("pt")),
    }
}

/// A desktop notification. Missing `notify-send` only loses the message.
fn notify(title: &str, body: &str) {
    let shown = Command::new("notify-send").args(["--app-name=XkyHold-lx", title, body]).status();
    if !shown.is_ok_and(|s| s.success()) {
        crate::log(&format!("notification not shown: {title}: {body}"));
    }
}

fn remind(s: &Settings, now: i64) {
    let pt = portuguese(s);
    let days = since(s.last_backup.as_ref().map(|l| l.time.as_str()), now).map(|l| l / DAY);
    let body = match (days, pt) {
        (None, false) => "You have not made a backup yet. Open XkyHold-lx to make one.".to_owned(),
        (None, true) => "Ainda não fizeste nenhuma cópia de segurança. Abre o XkyHold-lx para fazer uma.".to_owned(),
        (Some(d), false) => {
            format!("Your last backup was {} ago. Open XkyHold-lx to make one.", plural(d, "day", "days"))
        }
        (Some(d), true) => {
            format!(
                "A última cópia de segurança foi há {}. Abre o XkyHold-lx para fazer uma.",
                plural(d, "dia", "dias")
            )
        }
    };
    notify(if pt { "Está na altura de fazer uma cópia" } else { "Time for a backup" }, &body);
}

fn plural(n: i64, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// The sentence for an automatic backup that could not run.
fn problem_text(e: &Error, pt: bool) -> &'static str {
    match (e, pt) {
        (Error::Usage(m), false) if m == NEEDS_KEY => {
            "XkyHold-lx needs to keep your backup's key on this computer. In Settings, tick “Open this backup on this computer without the password”."
        }
        (Error::Usage(m), true) if m == NEEDS_KEY => {
            "O XkyHold-lx precisa de guardar a chave da cópia neste computador. Nas Definições, assinala «Abrir esta cópia neste computador sem a palavra-passe»."
        }
        (Error::Session(_), false) => "Your Google session expired. Open XkyHold-lx and sign in again.",
        (Error::Session(_), true) => "A tua sessão Google expirou. Abre o XkyHold-lx e entra de novo.",
        (Error::Io(_) | Error::NotFound(_), false) => {
            "The backup could not be reached. If it is on a disk, connect it: the next automatic backup runs by itself."
        }
        (Error::Io(_) | Error::NotFound(_), true) => {
            "Não foi possível chegar à cópia de segurança. Se estiver num disco, liga-o: a próxima cópia automática corre sozinha."
        }
        (Error::Network(_), false) => "Google Drive could not be reached for a while. Check the connection.",
        (Error::Network(_), true) => "Há algum tempo que não é possível contactar o Google Drive. Verifica a ligação.",
        (_, false) => "Open XkyHold-lx and press “Back up now” to see what is wrong.",
        (_, true) => "Abre o XkyHold-lx e carrega em «Fazer cópia agora» para ver o que se passa.",
    }
}

const NEEDS_KEY: &str = "the backup's key is not in the keyring";

/// Problems are shown at most once a day. A network that is down is only
/// worth a message when backups have been failing for two days.
fn report_problem(e: &Error, s: &Settings, now: i64) {
    crate::log(&format!("automatic backup failed: {} ({e})", e.kind()));
    if since(s.last_problem.as_deref(), now).is_some_and(|p| p < 20 * HOUR) {
        return;
    }
    let last = since(s.last_backup.as_ref().map(|l| l.time.as_str()), now);
    if matches!(e, Error::Network(_)) && last.is_some_and(|l| l < 2 * DAY) {
        return;
    }
    let pt = portuguese(s);
    notify(
        if pt { "A cópia automática não correu" } else { "The automatic backup could not run" },
        problem_text(e, pt),
    );
    let stamp = iso(now);
    let _ = Settings::update(|s| {
        s.last_problem = Some(stamp);
        Ok(())
    });
}

fn report_done(done: &BackupDone, s: &Settings) {
    let st = &done.snapshot.stats;
    let pt = portuguese(s);
    crate::log(&format!("automatic backup done: {} files, {} sent", st.files, done.uploaded));
    if st.skipped > 0 {
        let body = if pt {
            format!(
                "Não foi possível ler {} e ficaram fora desta cópia.",
                plural(st.skipped as i64, "ficheiro", "ficheiros")
            )
        } else {
            format!(
                "{} could not be read and were left out of this backup.",
                plural(st.skipped as i64, "file", "files")
            )
        };
        notify(if pt { "Cópia automática concluída" } else { "Automatic backup done" }, &body);
    } else if s.notify_success {
        let body = if pt {
            format!("{}, {} enviados.", plural(st.files as i64, "ficheiro", "ficheiros"), human(done.uploaded))
        } else {
            format!("{}, {} sent.", plural(st.files as i64, "file", "files"), human(done.uploaded))
        };
        notify(if pt { "Cópia automática concluída" } else { "Automatic backup done" }, &body);
    }
}

fn backup_now(s: &Settings) -> Result<BackupDone> {
    let target = s.backup.clone().ok_or_else(|| Error::Usage("no backup set up".into()))?;
    let (dest, repo_path) = crate::destination(&target, s, None)?;
    let mut repo = crate::open_with_keyring(dest.clone())?.ok_or_else(|| Error::Usage(NEEDS_KEY.into()))?;
    let done = crate::run_backup(
        &mut repo,
        dest,
        repo_path.as_deref(),
        &s.what,
        s.keep_months,
        None,
        &mut |_| {},
        &mut || {},
    )?;
    let st = &done.snapshot.stats;
    let last = LastBackup { time: done.snapshot.time.clone(), files: st.files, bytes: st.bytes, skipped: st.skipped };
    Settings::update(|s| {
        s.last_backup = Some(last);
        s.last_problem = None;
        Ok(())
    })?;
    Ok(done)
}

/// The hourly run. Returns an error only for the journal; the user hears
/// about problems through a notification.
pub fn run_scheduled() -> Result<()> {
    let s = Settings::load()?;
    let now = now_ms();
    match due(&s, now) {
        Due::Nothing => Ok(()),
        Due::Remind => {
            // Right after setting up there is no backup yet, and the user is
            // about to make one: start counting instead of reminding now.
            let just_set_up = s.last_backup.is_none() && s.last_reminded.is_none();
            if !just_set_up {
                remind(&s, now);
            }
            let stamp = iso(now);
            Settings::update(|s| {
                s.last_reminded = Some(stamp);
                Ok(())
            })?;
            Ok(())
        }
        Due::Backup => match backup_now(&s) {
            Ok(done) => {
                report_done(&done, &s);
                Ok(())
            }
            // The window or the command line is backing up right now.
            Err(Error::RunningHere) => Ok(()),
            Err(e) => {
                report_problem(&e, &s, now);
                Err(e)
            }
        },
    }
}

// --- The systemd units -----------------------------------------------------

fn units_dir() -> Result<PathBuf> {
    Ok(xdg("XDG_CONFIG_HOME", ".config")?.join("systemd"))
}

/// The program the timer runs: the AppImage itself when running from one
/// (its mounted copy disappears), else this binary.
fn program() -> Result<PathBuf> {
    match std::env::var_os("APPIMAGE") {
        Some(p) if Path::new(&p).is_absolute() => Ok(PathBuf::from(p)),
        _ => Ok(std::env::current_exe()?),
    }
}

/// `"…"` quoting for systemd's ExecStart and Environment.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%"))
}

/// The units skip themselves when the program is gone (uninstalled, or an
/// AppImage deleted or moved): a skipped unit is not a failed one, and a
/// timer whose condition is false is not started at the next login.
pub fn unit_files(program: &Path) -> (String, String) {
    let program = program.display().to_string();
    let condition = format!("ConditionPathExists={}\n", program.replace('%', "%%"));
    let mut env = String::new();
    // The same home and folders as the window, so the timer finds the same
    // settings, cache and log.
    for var in ["HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME"] {
        if let Ok(value) = std::env::var(var) {
            env.push_str(&format!("Environment={}\n", quoted(&format!("{var}={value}"))));
        }
    }
    let service = format!(
        "# Written by XkyHold-lx; changed or removed from its Settings.\n\
         [Unit]\n\
         Description=XkyHold-lx automatic backup and reminder\n\
         {condition}\n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={} --scheduled\n\
         {env}\
         Nice=10\n\
         IOSchedulingClass=idle\n",
        quoted(&program)
    );
    let timer = format!(
        "# Written by XkyHold-lx; changed or removed from its Settings.\n\
         [Unit]\n\
         Description=XkyHold-lx: see every hour whether a backup or a reminder is due\n\
         {condition}\n\
         [Timer]\n\
         OnCalendar=hourly\n\
         RandomizedDelaySec=5min\n\
         Persistent=true\n\n\
         [Install]\n\
         WantedBy=timers.target\n"
    );
    (service, timer)
}

/// `systemctl --user`, with the home and config folder of the user's
/// service manager, so links land where it looks for them.
fn systemctl(args: &[&str]) -> Result<std::process::Output> {
    let mut cmd = Command::new("systemctl");
    cmd.arg("--user").args(args);
    if let Ok(out) = Command::new("systemctl").args(["--user", "show-environment"]).output() {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((k, v)) = line.split_once('=')
                && (k == "HOME" || k == "XDG_CONFIG_HOME")
            {
                cmd.env(k, v);
            }
        }
    }
    let out = cmd.output().map_err(|e| Error::Usage(format!("systemctl could not run: {e}")))?;
    Ok(out)
}

fn ok(out: std::process::Output, what: &str) -> Result<()> {
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Usage(format!("systemctl {what}: {}", String::from_utf8_lossy(&out.stderr).trim())))
    }
}

pub fn timer_enabled() -> bool {
    systemctl(&["is-enabled", TIMER]).is_ok_and(|o| o.status.success())
}

/// Installs, updates or removes the timer to match the settings: it is
/// there while a backup is set up and automatic backups or reminders are
/// on.
pub fn sync_units(s: &Settings) -> Result<()> {
    let wanted = s.backup.is_some() && (interval(&s.automatic).is_some() || s.remind_days > 0);
    let dir = units_dir()?;
    let (service_path, timer_path) = (dir.join(SERVICE), dir.join(TIMER));
    if !wanted {
        if timer_enabled() || service_path.exists() {
            let _ = systemctl(&["disable", "--now", TIMER]);
            let _ = systemctl(&["disable", SERVICE]);
            let _ = std::fs::remove_file(&service_path);
            let _ = std::fs::remove_file(&timer_path);
            let _ = systemctl(&["daemon-reload"]);
            // Otherwise the manager lists the removed timer as failed.
            let _ = systemctl(&["reset-failed", TIMER, SERVICE]);
        }
        return Ok(());
    }
    let (service, timer) = unit_files(&program()?);
    let same = std::fs::read_to_string(&service_path).is_ok_and(|t| t == service)
        && std::fs::read_to_string(&timer_path).is_ok_and(|t| t == timer);
    // Inactive while enabled: its program was missing at login; now it is back.
    if same && timer_enabled() && systemctl(&["is-active", TIMER]).is_ok_and(|o| o.status.success()) {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&service_path, service)?;
    std::fs::write(&timer_path, timer)?;
    let (svc, tmr) = (service_path.display().to_string(), timer_path.display().to_string());
    ok(systemctl(&["link", "--force", &svc])?, "link")?;
    ok(systemctl(&["enable", "--force", "--now", &tmr])?, "enable")?;
    ok(systemctl(&["daemon-reload"])?, "daemon-reload")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Target;

    fn settings(automatic: &str, last_hours_ago: Option<i64>, now: i64) -> Settings {
        Settings {
            backup: Some(Target::Local { path: "/x".into() }),
            automatic: automatic.into(),
            last_backup: last_hours_ago.map(|h| LastBackup {
                time: iso(now - h * HOUR),
                files: 1,
                bytes: 1,
                skipped: 0,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn decides_what_is_due() {
        let now = parse_iso("2026-11-14T12:00:00.000Z").unwrap();
        assert_eq!(due(&settings("daily", None, now), now), Due::Backup);
        assert_eq!(due(&settings("daily", Some(23), now), now), Due::Backup);
        assert_eq!(due(&settings("daily", Some(22), now), now), Due::Nothing);
        assert_eq!(due(&settings("hourly", Some(1), now), now), Due::Backup);
        assert_eq!(due(&settings("weekly", Some(100), now), now), Due::Nothing);
        // Automatic off: the reminder, at most once a day.
        let mut s = settings("off", Some(30), now);
        assert_eq!(due(&s, now), Due::Remind);
        s.last_reminded = Some(iso(now - 2 * HOUR));
        assert_eq!(due(&s, now), Due::Nothing);
        s.last_reminded = Some(iso(now - 21 * HOUR));
        assert_eq!(due(&s, now), Due::Remind);
        s.remind_days = 7;
        assert_eq!(due(&s, now), Due::Nothing);
        s.remind_days = 0;
        assert_eq!(due(&s, now), Due::Nothing);
        // Nothing set up: nothing to do.
        assert_eq!(due(&Settings::default(), now), Due::Nothing);
    }

    #[test]
    fn units_quote_the_program_and_keep_the_home() {
        let (service, timer) = unit_files(Path::new("/opt/My Apps/xkyhold-lx"));
        assert!(service.contains("ExecStart=\"/opt/My Apps/xkyhold-lx\" --scheduled"), "{service}");
        assert!(service.contains("IOSchedulingClass=idle"));
        assert!(timer.contains("Persistent=true") && timer.contains("WantedBy=timers.target"));
        assert_eq!(quoted("a%b\"c"), "\"a%%b\\\"c\"");
    }

    /// Uninstalled: the units skip themselves instead of failing every hour.
    #[test]
    fn units_skip_themselves_without_the_program() {
        let (service, timer) = unit_files(Path::new("/home/u/Apps 100%/xkyhold-lx.AppImage"));
        let condition = "\nConditionPathExists=/home/u/Apps 100%%/xkyhold-lx.AppImage\n";
        for unit in [&service, &timer] {
            let (head, _) = unit.split_once("\n\n[").unwrap();
            assert!(head.starts_with("# Written") && head.contains("[Unit]"), "{unit}");
            assert!(head.contains(condition.trim_end()), "{unit}");
        }
        assert!(service.contains("ExecStart=\"/home/u/Apps 100%%/xkyhold-lx.AppImage\" --scheduled"));
    }

    #[test]
    fn one_backup_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let first = LocalLock::take_at(&path).unwrap();
        // Another open file description, as in another process.
        assert!(matches!(LocalLock::take_at(&path), Err(Error::RunningHere)));
        drop(first);
        assert!(LocalLock::take_at(&path).is_ok());
    }
}
