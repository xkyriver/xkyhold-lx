//! The XkyHold-lx window. Tauri commands here stay thin: they call the
//! engine in `xkyhold-core` and pass errors to the UI as a `kind` string.

use serde::Serialize;

#[derive(Serialize)]
struct Hello {
    version: &'static str,
    engine_formats: &'static [u32],
}

/// Phase 0 prototype: proves the UI can call into Rust.
#[tauri::command]
fn hello() -> Hello {
    Hello { version: env!("CARGO_PKG_VERSION"), engine_formats: xkyhold_core::crypto::READABLE_FORMATS }
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![hello])
        .run(tauri::generate_context!())
        .expect("error while running XkyHold-lx");
}
