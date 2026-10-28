// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;
mod engine;
mod library;
mod prefs;
mod soloist;
mod tray;
mod ui;

use std::io::Write;
use std::sync::{Arc, Mutex};

use egui_software_backend::{SoftwareBackendAppConfiguration, run_app_with_software_backend};

fn main() {
    let icon = Arc::new(egui::IconData { rgba: tray::icon_rgba(64), width: 64, height: 64 });
    // The UI is drawn on the CPU: for a window this simple it's cheap, and it keeps
    // the GPU driver (often 100+ MB of memory) out of the process entirely.
    let result = match claim_single_instance() {
        Ok(()) => {
            init_log();
            let prefs = prefs::Prefs::load();
            tray::CLOSE_TO_TRAY.store(prefs.close_to_tray, std::sync::atomic::Ordering::SeqCst);
            if prefs.start_with_windows {
                // Keeps the startup entry pointing at this exe, even if it was moved.
                prefs::set_run_at_startup(true, prefs.start_minimized);
            }
            let minimized = std::env::args().any(|a| a == "--minimized");
            let config = SoftwareBackendAppConfiguration::new()
                .title(Some("Spoty".into()))
                .icon(Some(icon))
                .inner_size(Some(egui::vec2(960.0, 600.0)))
                .min_inner_size(Some(egui::vec2(640.0, 420.0)));
            run_app_with_software_backend(config, move |ctx| ui::App::new(ctx, minimized))
        }
        Err(other) => {
            let config = SoftwareBackendAppConfiguration::new()
                .title(Some("Spoty is already running".into()))
                .icon(Some(icon))
                .inner_size(Some(egui::vec2(380.0, 150.0)))
                .resizable(Some(false));
            run_app_with_software_backend(config, move |ctx| ui::AlreadyRunning::new(ctx, other))
        }
    };
    if let Err(e) = result {
        log::error!("Spoty failed to start: {e}");
    }
}

/// Only one Spoty may run at a time: two would fight over the same Soloist player.
/// Returns the open Spoty window (as an address) when another copy is already running.
fn claim_single_instance() -> Result<(), usize> {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;

    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let name = wide(r"Local\Spoty-single-instance");
    unsafe {
        // The mutex is intentionally never closed; Windows releases it when Spoty exits.
        CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if GetLastError() != ERROR_ALREADY_EXISTS {
            return Ok(());
        }
        // "Window Class" is the class name of the window Spoty draws into.
        Err(FindWindowW(wide("Window Class").as_ptr(), wide("Spoty").as_ptr()) as usize)
    }
}

/// Set SPOTY_LOG=info (or debug/trace) to write a log to %APPDATA%\Spoty\spoty.log.
fn init_log() {
    struct FileLog(Mutex<std::fs::File>);
    impl log::Log for FileLog {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::max_level()
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                let _ = writeln!(self.0.lock().unwrap(), "{:<5} {}: {}", r.level(), r.target(), r.args());
            }
        }
        fn flush(&self) {}
    }

    let Some(level) = std::env::var("SPOTY_LOG").ok().and_then(|l| l.parse().ok()) else { return };
    let _ = std::fs::create_dir_all(auth::data_dir());
    let Ok(file) = std::fs::File::create(auth::data_dir().join("spoty.log")) else { return };
    if log::set_boxed_logger(Box::new(FileLog(Mutex::new(file)))).is_ok() {
        log::set_max_level(level);
    }
}
