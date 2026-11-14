# Changelog

## 1.0.0 — 2026-11-14

First release of XkyHold-lx, the Linux app of [XkyHold](https://xkyhold.xkyriver.com):
encrypted backup of your home folder to your own Google Drive or to a folder
such as an external disk.

### What it does

- Backs up everything in your home folder: hidden files, settings, keys,
  permissions, modification times and symbolic links. Caches, the trash and
  the app's own files are always left out; you choose what else to leave out,
  with the size of each folder shown.
- Encrypts on your computer before anything is saved. Google Drive and the
  disk only see ciphertext; only your password or recovery key opens it.
- Sends only what changed after the first backup.
- Restores one file or everything, to a new folder, a disk or the original
  place (never overwriting silently).
- Reminds you when a backup is due, or backs up by itself with the window
  closed (a systemd user timer, opt-in).
- Checks a backup quickly or in full.
- Repository format 2 of XkyHold: backups made here can also be restored with
  XkyHold in a web browser, even on a phone.
- English and Portuguese (Portugal).

### Install

Packages for 64-bit Intel and AMD computers:

| File | For |
|---|---|
| `xkyhold-lx_1.0.0_amd64.AppImage` | Any distribution |
| `xkyhold-lx_1.0.0_amd64.deb` | Debian, Ubuntu, Mint |
| `xkyhold-lx-1.0.0-1.x86_64.rpm` | Fedora, openSUSE |

Instructions: <https://xkyhold.xkyriver.com/linux/>. `SHA256SUMS` lists the
checksum of each file.

A desktop keyring (GNOME Keyring or KWallet) is recommended: it keeps the
Google sign-in and, if you choose, the key of your backup. Automatic backups
need it. Without one, XkyHold-lx asks for your password each time and you sign
in to Google each time you open it.

### Before you uninstall

Open Settings and choose "Remove from this computer" first. Otherwise the
hourly timer stays behind in your user's systemd folder; it skips itself
while the program is missing, and wakes up again if you reinstall.

### Known limits

- Your home folder only: not a system backup, and it never runs as root.
- Work or school Google accounts may need the administrator to approve the
  app: <https://xkyhold.xkyriver.com/privacy.html#admins-lx>.
- The `.deb` and `.rpm` were checked for content but not yet installed on
  Debian or Fedora.
