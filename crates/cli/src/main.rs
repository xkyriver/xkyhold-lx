//! `xkyhold-lx-cli`: back up and restore the home folder from the command
//! line. Used by the automatic backup (systemd user timer) and by people
//! who prefer a terminal.
//!
//! The password comes from `XKYHOLD_LX_PASSWORD` or is asked on the
//! terminal; it is never stored.

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use xkyhold_core::backup::{BackupOptions, backup};
use xkyhold_core::check::check;
use xkyhold_core::crypto::{Credential, DEFAULT_ARGON2};
use xkyhold_core::destination::Destination;
use xkyhold_core::lock::{Lock, LockKind};
use xkyhold_core::prune::{forget_older, prune};
use xkyhold_core::repo::{Options, Repository};
use xkyhold_core::restore::{Conflict, RestoreOptions, find, restore};
use xkyhold_core::rules::{Exclusions, PRESETS};
use xkyhold_core::snapshot::Snapshot;
use xkyhold_core::time::{now_iso, now_ms};
use xkyhold_core::tree::{Entry, EntryKind};
use xkyhold_core::{Error, Result};
use xkyhold_dest_local::LocalFolder;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(name = "xkyhold-lx-cli", version, about = "Encrypted backup of your home folder")]
struct Cli {
    /// The backup: a folder on this computer (an external disk, a NAS).
    #[arg(long, global = true, env = "XKYHOLD_LX_REPO")]
    repo: Option<PathBuf>,
    /// Open with the recovery key instead of the password.
    #[arg(long, global = true)]
    recovery_key: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new backup and show its recovery key.
    Init,
    /// Back up your home folder, or only some items inside it.
    Backup {
        /// Back up only these items (paths inside your home folder).
        #[arg(long = "only")]
        only: Vec<PathBuf>,
        /// Leave out these items (paths inside your home folder).
        #[arg(long = "leave-out")]
        leave_out: Vec<PathBuf>,
        /// Switch on a suggested exclusion (see `presets`).
        #[arg(long = "preset")]
        presets: Vec<String>,
        /// Leave out what matches a pattern (`**/*.iso`).
        #[arg(long = "pattern")]
        patterns: Vec<String>,
        /// Back up a disk mounted inside your home folder.
        #[arg(long = "cross-mount")]
        cross_mounts: Vec<PathBuf>,
    },
    /// List the backups made.
    Snapshots,
    /// List what a backup holds.
    Ls {
        /// Snapshot id (or the start of it), or `latest`.
        snapshot: String,
        /// A folder inside the backup.
        path: Option<String>,
    },
    /// Restore from a backup into a folder inside your home folder.
    Restore {
        snapshot: String,
        /// Items to restore (paths inside the backup); everything by default.
        paths: Vec<String>,
        /// Where to restore. Restoring into your home folder itself must be asked for explicitly.
        #[arg(long)]
        to: PathBuf,
        #[arg(long, value_enum, default_value_t = ConflictArg::KeepBoth)]
        conflict: ConflictArg,
    },
    /// Check that the backup is complete and intact.
    Check {
        /// Also download and verify every piece of data.
        #[arg(long)]
        full: bool,
    },
    /// Remove backups older than some months (always keeps the latest) and free their space.
    Forget {
        #[arg(long)]
        keep_months: u32,
    },
    /// Show the state of the backup.
    Status,
    /// List the suggested exclusions.
    Presets,
}

#[derive(Clone, Copy, ValueEnum)]
enum ConflictArg {
    Skip,
    Replace,
    KeepBoth,
}

type Repo = Repository<Arc<dyn Destination>>;

fn home() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or(Error::OutsideHome("$HOME is not set".into()))?;
    if !home.is_absolute() || home == Path::new("/") {
        return Err(Error::OutsideHome(home.display().to_string()));
    }
    Ok(home)
}

fn device() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|h| h.trim().to_owned()).unwrap_or_else(|_| "linux".into())
}

fn destination(cli: &Cli) -> Result<(Arc<dyn Destination>, Option<PathBuf>)> {
    let Some(path) = &cli.repo else {
        return Err(Error::Usage("say where the backup is with --repo or XKYHOLD_LX_REPO".into()));
    };
    let path = std::path::absolute(path)?;
    Ok((Arc::new(LocalFolder::new(&path)), Some(path)))
}

fn ask(prompt: &str, env: &str) -> Result<Zeroizing<String>> {
    if let Ok(value) = std::env::var(env) {
        return Ok(Zeroizing::new(value));
    }
    Ok(Zeroizing::new(rpassword::prompt_password(prompt)?))
}

fn credential(cli: &Cli) -> Result<Credential> {
    Ok(if cli.recovery_key {
        Credential::RecoveryKey(ask("Recovery key: ", "XKYHOLD_LX_RECOVERY_KEY")?)
    } else {
        Credential::Password(ask("Password: ", "XKYHOLD_LX_PASSWORD")?)
    })
}

fn open(cli: &Cli) -> Result<Repo> {
    let (dest, _) = destination(cli)?;
    Repository::open(dest, &credential(cli)?, Options::default())
}

/// A path inside the home folder, relative to it. Refuses anything outside.
fn inside_home(home: &Path, path: &Path) -> Result<Vec<u8>> {
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

fn pick(repo: &Repo, wanted: &str) -> Result<Snapshot> {
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
    found.ok_or_else(|| Error::Usage(format!("no backup \"{wanted}\"")))
}

fn split_path(path: &str) -> Vec<&[u8]> {
    path.split('/').filter(|p| !p.is_empty()).map(str::as_bytes).collect()
}

fn root_and_entries(repo: &Repo, snap: &Snapshot, path: Option<&str>) -> Result<Vec<Entry>> {
    let root = snap.roots.first().ok_or_else(|| Error::Format("backup without folders".into()))?;
    let mut names = path.map(split_path).unwrap_or_default();
    // Version 1 backups hold several folders: the first name picks one.
    let tree = if snap.roots.len() > 1 || root.source.is_none() {
        let first = names.first().copied().unwrap_or_default();
        let chosen = snap.roots.iter().find(|r| r.name.as_bytes() == first);
        match chosen {
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
            None => {
                return Err(Error::Usage(format!("no folder \"{}\" in this backup", String::from_utf8_lossy(first))));
            }
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
        None => Err(Error::Usage(format!("nothing at \"{}\" in this backup", path.unwrap_or_default()))),
    }
}

fn human(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < units.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", units[unit]) }
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Presets => {
            for p in PRESETS {
                println!("{:16} {}", p.id, p.patterns.join("  "));
            }
        }
        Command::Init => {
            let (dest, _) = destination(cli)?;
            let password = ask("New password: ", "XKYHOLD_LX_PASSWORD")?;
            if std::env::var("XKYHOLD_LX_PASSWORD").is_err() && *ask("Repeat it: ", "-")? != *password {
                return Err(Error::Usage("the passwords are different".into()));
            }
            let (_, recovery) = Repository::create(dest, &password, DEFAULT_ARGON2, now_iso(), Options::default())?;
            println!(
                "Backup created.\n\nRecovery key (keep it away from this computer; it is shown only once):\n\n  {}\n",
                *recovery
            );
        }
        Command::Backup { only, leave_out, presets, patterns, cross_mounts } => {
            let home = home()?;
            let (dest, repo_path) = destination(cli)?;
            let mut repo = Repository::open(dest.clone(), &credential(cli)?, Options::default())?;
            let rel = |paths: &[PathBuf]| paths.iter().map(|p| inside_home(&home, p)).collect::<Result<Vec<_>>>();
            for id in presets {
                if !PRESETS.iter().any(|p| p.id == id) {
                    return Err(Error::Usage(format!("unknown preset \"{id}\"; see `presets`")));
                }
            }
            let exclusions = Exclusions {
                presets: presets.clone(),
                paths: rel(leave_out)?,
                patterns: patterns.clone(),
                cross_mounts: rel(cross_mounts)?,
                repository: repo_path.and_then(|p| inside_home(&home, &p).ok()),
            };
            let device = device();
            let mut lock = Lock::acquire(dest, LockKind::Backup, &device)?;
            repo.refresh_index()?;
            let parent = repo.list_snapshots()?.into_iter().rev().find(|s| s.device == device && s.home.is_some());
            let mut options = BackupOptions::new(device, now_iso());
            options.parent = parent.as_ref();
            options.exclusions = exclusions;
            options.selection = if only.is_empty() { None } else { Some(rel(only)?) };
            let snap = backup(&mut repo, &home, options, &mut |_| {});
            lock.release();
            let snap = snap?;
            let s = &snap.stats;
            println!(
                "Backup {} done: {} files ({}), {} sent.",
                &snap.id[..8],
                s.files,
                human(s.bytes),
                human(repo.uploaded_bytes)
            );
            for (count, what) in [
                (s.skipped, "could not be read and were skipped"),
                (s.changed.unwrap_or(0), "changed while being read (stored as read)"),
                (s.special.unwrap_or(0), "special files (sockets, pipes, devices) are never stored"),
                (s.mounts.unwrap_or(0), "other disks inside your home folder were not backed up"),
            ] {
                if count > 0 {
                    println!("  {count} {what}");
                }
            }
        }
        Command::Snapshots => {
            let repo = open(cli)?;
            for s in repo.list_snapshots()? {
                let roots: Vec<&str> = s.roots.iter().map(|r| r.name.as_str()).collect();
                println!(
                    "{}  {}  {:12}  {:>8} files  {:>9}  {}",
                    &s.id[..8],
                    s.time,
                    s.device,
                    s.stats.files,
                    human(s.stats.bytes),
                    roots.join(", ")
                );
            }
        }
        Command::Ls { snapshot, path } => {
            let repo = open(cli)?;
            let snap = pick(&repo, snapshot)?;
            for e in root_and_entries(&repo, &snap, path.as_deref())? {
                let (kind, size, extra) = match &e.kind {
                    EntryKind::File { size, .. } => ("-", human(*size), String::new()),
                    EntryKind::Dir { mount: true, .. } => {
                        ("d", String::new(), "  (another disk, not backed up)".into())
                    }
                    EntryKind::Dir { .. } => ("d", String::new(), String::new()),
                    EntryKind::Symlink { target } => {
                        ("l", String::new(), format!(" -> {}", String::from_utf8_lossy(target)))
                    }
                    EntryKind::Unknown(t) => ("?", String::new(), format!("  (unknown type {t})")),
                };
                let mode = e.meta.mode.map(|m| format!("{m:04o}")).unwrap_or_else(|| "    ".into());
                println!("{kind} {mode} {size:>9}  {}{extra}", e.display_name());
            }
        }
        Command::Restore { snapshot, paths, to, conflict } => {
            let home = home()?;
            let target = std::path::absolute(to)?;
            inside_home(&home, &target)?;
            let repo = open(cli)?;
            let snap = pick(&repo, snapshot)?;
            let mut entries = Vec::new();
            if paths.is_empty() {
                entries = root_and_entries(&repo, &snap, None)?;
            } else {
                for p in paths {
                    let names = split_path(p);
                    let (parent, last) = names.split_at(names.len().saturating_sub(1));
                    let parent = String::from_utf8_lossy(&parent.join(&b'/')).into_owned();
                    let listing = root_and_entries(&repo, &snap, (!parent.is_empty()).then_some(parent.as_str()))?;
                    let entry = listing.into_iter().find(|e| last.first().is_some_and(|l| e.name == *l));
                    entries.push(entry.ok_or_else(|| Error::Usage(format!("nothing at \"{p}\" in this backup")))?);
                }
            }
            std::fs::create_dir_all(&target)?;
            let conflict = match conflict {
                ConflictArg::Skip => Conflict::Skip,
                ConflictArg::Replace => Conflict::Replace,
                ConflictArg::KeepBoth => Conflict::KeepBoth,
            };
            let r = restore(&repo, &entries, &target, &RestoreOptions { conflict, cancel: None }, &mut |_| {})?;
            println!(
                "Restored {} files ({}), {} folders, {} links into {}.",
                r.files,
                human(r.bytes),
                r.folders,
                r.symlinks,
                target.display()
            );
            for (count, what) in [
                (r.existing, "items already existed and were left alone"),
                (r.bad_names, "items had names that cannot be created and were skipped"),
                (r.unknown, "items of a kind this version does not know were skipped"),
                (r.xattr_failures, "extended attributes could not be set here"),
            ] {
                if count > 0 {
                    println!("  {count} {what}");
                }
            }
        }
        Command::Check { full } => {
            let repo = open(cli)?;
            let report = check(&repo, *full, &mut |_| {})?;
            if report.ok() {
                println!(
                    "The backup is complete: {} packs, {} pieces of data checked.",
                    report.packs, report.blobs_checked
                );
            } else {
                println!("Problems found: {report:?}");
                return Err(Error::Authentication("the backup is damaged"));
            }
        }
        Command::Forget { keep_months } => {
            let (dest, path) = destination(cli)?;
            let mut repo = Repository::open(dest.clone(), &credential(cli)?, Options::default())?;
            let since = now_ms() - i64::from(*keep_months) * 2_629_746_000; // average month
            let mut lock = Lock::acquire(dest, LockKind::Prune, &device())?;
            let removed = forget_older(&repo, since);
            let result = removed.and_then(|n| prune(&mut repo, 0.5).map(|p| (n, p)));
            lock.release();
            let (n, p) = result?;
            if let Some(path) = path {
                LocalFolder::new(path).empty_old_trash()?;
            }
            println!("{n} old backups removed; {} freed.", human(p.bytes_freed));
        }
        Command::Status => {
            let repo = open(cli)?;
            let snapshots = repo.list_snapshots()?;
            println!(
                "Format {}{}; {} backups.",
                repo.config.format,
                if repo.writable() { "" } else { " (made by XkyHold for the browser: restore only)" },
                snapshots.len()
            );
            if let Some(last) = snapshots.last() {
                println!(
                    "Last: {} from {}, {} files ({}).",
                    last.time,
                    last.device,
                    last.stats.files,
                    human(last.stats.bytes)
                );
                if let Some(rules) = last.roots.first().and_then(|r| r.left_out.as_ref()) {
                    let left: Vec<String> = rules
                        .iter()
                        .map(|r| {
                            let what = r.path.clone().or_else(|| r.path_bytes.as_deref().map(|b| format!("bytes:{b}")));
                            format!(
                                "{}:{}",
                                r.kind,
                                r.id.clone()
                                    .or(what)
                                    .or_else(|| r.patterns.as_ref().map(|p| p.join(",")))
                                    .unwrap_or_default()
                            )
                        })
                        .collect();
                    println!("Left out: {}", left.join("  "));
                }
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xkyhold-lx-cli: {err}");
            ExitCode::FAILURE
        }
    }
}
