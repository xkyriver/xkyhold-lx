# XkyHold-lx

Encrypted backup of your Linux home folder to your own Google Drive or to a
local folder. The Linux member of the [XkyHold](https://xkyhold.xkyriver.com)
family: same encryption, same repository format and same look, without the
limits of a browser.

XkyHold-lx backs up everything inside `$HOME` — hidden files, `~/.config`,
`~/.ssh`, permissions, modification times and symbolic links — and restores
it to where it came from or to another folder. Backups are encrypted on your
computer before they leave it; nobody else, Google included, can read them.

**Status:** 1.0.0. Download the AppImage, `.deb` or `.rpm` from the
[releases](https://github.com/xkyriver/xkyhold-lx/releases); how to install:
<https://xkyhold.xkyriver.com/linux/>.

## Scope

- Only `$HOME` and what is inside it. XkyHold-lx refuses paths outside it,
  never follows symbolic links while backing up, and never runs as root. It
  is not a system backup.
- Linux only.
- Repositories written by XkyHold-lx can be restored in the XkyHold web app.

## Layout

| Path | What |
|---|---|
| `crates/core` | Backup engine: crypto, packs, index, snapshots, backup, restore, prune, locks, check |
| `crates/dest-local` | Destination in a local folder |
| `crates/dest-drive` | Destination in Google Drive |
| `crates/cli` | `xkyhold-lx-cli` command line |
| `src-tauri` | `xkyhold-lx` window (Tauri 2) |
| `ui` | Interface (Svelte 5, Vite, TypeScript) |
| `packaging` | systemd user units, desktop entry, icons, metainfo |
| `docs` | Plan (`PLAN.md`, in Portuguese) |

The repository format is specified in `docs/FORMAT.md` of the XkyHold
repository; this engine implements it.

## Development

Requires Rust (version pinned in `rust-toolchain.toml`), Node 26 and the
WebKitGTK 4.1 development libraries.

```sh
npm install
npm run dev          # the window, with the UI hot-reloading
cargo test           # engine tests
```

## License

AGPL-3.0-only. See `LICENSE`.
