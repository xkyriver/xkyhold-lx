//! The command line end to end, in a temporary home folder with a local
//! repository inside it (which must leave itself out of the backup).

use std::path::Path;
use std::process::{Command, Output};

fn cli(home: &Path, args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_xkyhold-lx-cli"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("XKYHOLD_LX_REPO", home.join("Backups/XkyHold/test"))
        .env("XKYHOLD_LX_PASSWORD", "correct horse battery staple")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?} failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn init_backup_list_restore_check_forget() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join("Documents")).unwrap();
    std::fs::create_dir_all(home.join(".config/app")).unwrap();
    std::fs::write(home.join("Documents/notes.txt"), b"hello").unwrap();
    std::fs::write(home.join(".config/app/settings"), b"dark=true").unwrap();
    std::os::unix::fs::symlink("Documents/notes.txt", home.join("notes")).unwrap();

    assert!(text(&cli(&home, &["init"])).contains("Recovery key"));
    let backed = text(&cli(&home, &["backup"]));
    assert!(backed.contains("3 files") || backed.contains("2 files"), "{backed}");

    let ls = text(&cli(&home, &["ls", "latest"]));
    assert!(ls.contains(".config") && ls.contains("notes -> Documents/notes.txt"), "{ls}");
    let inside = text(&cli(&home, &["ls", "latest", "Backups/XkyHold"]));
    assert!(inside.trim().is_empty(), "the repository leaves itself out:\n{inside}");

    cli(&home, &["restore", "latest", "--to", "restored"]);
    assert_eq!(std::fs::read(home.join("restored/.config/app/settings")).unwrap(), b"dark=true");
    assert_eq!(std::fs::read_link(home.join("restored/notes")).unwrap(), Path::new("Documents/notes.txt"));

    cli(&home, &["restore", "latest", "Documents/notes.txt", "--to", "one"]);
    assert_eq!(std::fs::read(home.join("one/notes.txt")).unwrap(), b"hello");

    assert!(text(&cli(&home, &["check", "--full"])).contains("complete"));
    assert!(text(&cli(&home, &["status"])).contains("Format 2"));
    assert!(text(&cli(&home, &["forget", "--keep-months", "3"])).contains("0 old backups removed"));

    // Restoring outside the home folder is refused.
    let out = Command::new(env!("CARGO_BIN_EXE_xkyhold-lx-cli"))
        .args(["restore", "latest", "--to", "/tmp/elsewhere"])
        .env("HOME", &home)
        .env("XKYHOLD_LX_REPO", home.join("Backups/XkyHold/test"))
        .env("XKYHOLD_LX_PASSWORD", "correct horse battery staple")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("inside your home folder"));
}
