//! User preferences, saved as %APPDATA%\Spoty\settings.json.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::auth::data_dir;

#[derive(Clone, PartialEq)]
pub struct Prefs {
    /// The window's X button hides Spoty to the tray instead of quitting.
    pub close_to_tray: bool,
    pub start_with_windows: bool,
    /// When started with Windows, go straight to the tray.
    pub start_minimized: bool,
    /// Limit for Soloist's audio cache, in MB (Soloist's minimum is 100).
    pub cache_mb: u32,
    /// The name Spotify apps show for this player.
    pub device_name: String,
    /// Volume Soloist starts at, 0–100.
    pub default_volume: u8,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            close_to_tray: true,
            start_with_windows: false,
            start_minimized: true,
            cache_mb: 500,
            device_name: "Spoty".into(),
            default_volume: 50,
        }
    }
}

fn file() -> PathBuf {
    data_dir().join("settings.json")
}

impl Prefs {
    pub fn load() -> Prefs {
        let d = Prefs::default();
        let Some(v) = std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else {
            return d;
        };
        Prefs {
            close_to_tray: v["close_to_tray"].as_bool().unwrap_or(d.close_to_tray),
            start_with_windows: v["start_with_windows"].as_bool().unwrap_or(d.start_with_windows),
            start_minimized: v["start_minimized"].as_bool().unwrap_or(d.start_minimized),
            cache_mb: v["cache_mb"].as_u64().map_or(d.cache_mb, |m| m.clamp(100, 100_000) as u32),
            device_name: v["device_name"].as_str().filter(|n| !n.trim().is_empty()).map_or(d.device_name, str::to_string),
            default_volume: v["default_volume"].as_u64().map_or(d.default_volume, |x| x.min(100) as u8),
        }
    }

    pub fn save(&self) {
        let v = json!({
            "close_to_tray": self.close_to_tray,
            "start_with_windows": self.start_with_windows,
            "start_minimized": self.start_minimized,
            "cache_mb": self.cache_mb,
            "device_name": self.device_name.trim(),
            "default_volume": self.default_volume,
        });
        let _ = std::fs::create_dir_all(data_dir());
        let _ = std::fs::write(file(), serde_json::to_string_pretty(&v).unwrap_or_default());
        set_run_at_startup(self.start_with_windows, self.start_minimized);
    }

    /// Soloist must restart for these to take effect.
    pub fn player_settings_differ(&self, other: &Prefs) -> bool {
        self.cache_mb != other.cache_mb
            || self.device_name.trim() != other.device_name.trim()
            || self.default_volume != other.default_volume
    }
}

/// Where Soloist keeps cached audio. Lives in Local AppData: it's disposable and shouldn't roam.
pub fn cache_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(data_dir);
    base.join("Spoty").join("cache")
}

/// Adds or removes Spoty from HKCU\...\Run, so it starts when the user signs in to Windows.
pub fn set_run_at_startup(enabled: bool, minimized: bool) {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW};

    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
    let name = wide("Spoty");
    unsafe {
        if !enabled {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr());
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let mut cmd = format!("\"{}\"", exe.display());
        if minimized {
            cmd.push_str(" --minimized");
        }
        let data = wide(&cmd);
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        );
    }
}
