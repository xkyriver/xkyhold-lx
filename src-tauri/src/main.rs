// Prevents an extra console window on Windows; harmless on Linux.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    // The hourly systemd timer: no window, no WebKit.
    if std::env::args_os().nth(1).is_some_and(|a| a == "--scheduled") {
        return match xkyhold_app::schedule::run_scheduled() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("xkyhold-lx: {e}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    #[cfg(target_os = "linux")]
    ensure_webkit_env();
    xkyhold_lx_lib::run();
    std::process::ExitCode::SUCCESS
}

/// WebKitGTK's DMABUF renderer fails with the NVIDIA proprietary driver: the
/// window stays blank, or Wayland closes it with "Error 71 (Protocol error)".
/// The variable must be set before WebKit starts. Setting it in this process
/// needs `unsafe` (`std::env::set_var`), so when the driver is loaded and the
/// variable is unset we replace this process with itself, variable included.
/// `exec` keeps the PID, so launchers and systemd see one process. An existing
/// value wins, so `WEBKIT_DISABLE_DMABUF_RENDERER=0` turns the workaround off.
#[cfg(target_os = "linux")]
fn ensure_webkit_env() {
    use std::os::unix::process::CommandExt;
    const VAR: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
    if std::env::var_os(VAR).is_some() || !std::path::Path::new("/sys/module/nvidia").exists() {
        return;
    }
    // current_exe, not /proc/self/exe: exec of the latter renames the process
    // to "exe". Inside an AppImage this is the mounted binary, which is right.
    let Ok(exe) = std::env::current_exe() else { return };
    let mut args = std::env::args_os();
    let mut cmd = std::process::Command::new(exe);
    if let Some(arg0) = args.next() {
        cmd.arg0(arg0);
    }
    // exec returns only when it fails; then carry on without the workaround.
    let err = cmd.args(args).env(VAR, "1").exec();
    eprintln!("xkyhold-lx: could not restart with {VAR}=1 ({err}); the window may stay blank");
}
