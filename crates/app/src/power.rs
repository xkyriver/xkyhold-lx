//! The computer's power, through systemd-logind on the system bus:
//! keeping it from suspending while a job runs, and turning it off after a
//! backup when the person asked for it.
//!
//! No setting of the system is changed. The block is a file descriptor that
//! logind hands out: it lasts while it is open and goes away when it is
//! closed, also if the program stops unexpectedly. By the system's default
//! (`LidSwitchIgnoreInhibited=yes`) closing a laptop's lid still suspends
//! it, which keeps a laptop in a bag from overheating.

use xkyhold_core::{Error, Result};
use zbus::blocking::Connection;
use zbus::zvariant::OwnedFd;

const LOGIN1: &str = "org.freedesktop.login1";
const PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";

/// Keeps the computer from suspending (when idle, or from a menu) while it
/// is held. Shown by `systemd-inhibit --list` with `why`.
pub struct Awake(#[allow(dead_code)] OwnedFd);

/// Asks logind to hold off sleep. `None` without logind: the job runs anyway.
pub fn keep_awake(why: &str) -> Option<Awake> {
    let bus = Connection::system().ok()?;
    let reply =
        bus.call_method(Some(LOGIN1), PATH, Some(MANAGER), "Inhibit", &("sleep", "XkyHold-lx", why, "block")).ok()?;
    reply.body().deserialize::<OwnedFd>().ok().map(Awake)
}

/// Can this session turn the computer off? "yes" or "challenge" (the
/// system asks for a password itself); not "no" or "na".
pub fn can_power_off() -> bool {
    let answer = || -> Option<String> {
        let bus = Connection::system().ok()?;
        let reply = bus.call_method(Some(LOGIN1), PATH, Some(MANAGER), "CanPowerOff", &()).ok()?;
        reply.body().deserialize::<String>().ok()
    };
    matches!(answer().as_deref(), Some("yes" | "challenge"))
}

/// Should the computer turn off now? Only when the person asked, and only
/// after a backup that went well: a failed or stopped backup, a restore or a
/// check leave it on, with the message on screen.
pub fn off_after(asked: bool, kind: &str, ok: bool) -> bool {
    asked && ok && kind == "backup"
}

/// Turns the computer off. Interactive: if the system wants a password (for
/// example with another person logged in), it asks in its own window.
pub fn power_off() -> Result<()> {
    // Development: test the countdown without turning anything off.
    #[cfg(debug_assertions)]
    if std::env::var_os("XKYHOLD_LX_NO_POWER_OFF").is_some() {
        crate::log("power off skipped (XKYHOLD_LX_NO_POWER_OFF)");
        return Ok(());
    }
    let bus = Connection::system().map_err(|e| Error::Usage(format!("power off: {e}")))?;
    bus.call_method(Some(LOGIN1), PATH, Some(MANAGER), "PowerOff", &(true,))
        .map(|_| ())
        .map_err(|e| Error::Usage(format!("power off: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs logind on the system bus (a normal Linux desktop):
    /// `cargo test -p xkyhold-app -- --ignored keeps_awake`.
    #[test]
    fn off_only_after_a_backup_that_went_well() {
        assert!(off_after(true, "backup", true));
        assert!(!off_after(false, "backup", true), "not asked");
        assert!(!off_after(true, "backup", false), "failed or stopped");
        assert!(!off_after(true, "restore", true));
        assert!(!off_after(true, "check", true));
    }

    #[test]
    #[ignore]
    fn keeps_awake_while_held_and_not_after() {
        let listed = || {
            let out = std::process::Command::new("systemd-inhibit").arg("--list").output().unwrap();
            String::from_utf8_lossy(&out.stdout).contains("XkyHold-lx test block")
        };
        let awake = keep_awake("XkyHold-lx test block").expect("logind answers");
        assert!(listed(), "the block is listed while held");
        drop(awake);
        assert!(!listed(), "and gone once let go");
    }
}
