//! Runs Spotify Soloist (Spotify's official headless player) in the background and talks to it
//! over its local WebSocket API. Soloist does the actual audio; Spoty is the remote.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::auth::data_dir;
use crate::prefs::{self, Prefs};

/// Spotify's official Windows build. Spoty fetches it on the user's machine (Spotify's terms
/// forbid bundling it), and fetches it again when a build expires.
const BUILD_URL: &str = "https://soloist-builds.spotifycdn.com/soloist_release_windows_x86_64.zip";
pub const DASHBOARD: &str = "https://developer.spotify.com/dashboard/soloist";

#[derive(Clone, Default)]
pub struct Config {
    /// Empty means "the copy Spoty downloads and keeps updated".
    pub exe: String,
    pub api_key: String,
}

impl Config {
    fn file() -> PathBuf {
        data_dir().join("soloist.json")
    }

    pub fn load() -> Option<Config> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(Config::file()).ok()?).ok()?;
        Some(Config { exe: v["exe"].as_str()?.to_string(), api_key: v["api_key"].as_str()?.to_string() })
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(data_dir());
        let _ = std::fs::write(Config::file(), json!({ "exe": self.exe, "api_key": self.api_key }).to_string());
    }

    pub fn is_managed(&self) -> bool {
        self.exe.trim().is_empty()
    }

    /// Accepts either the exe itself or the folder it was extracted to.
    pub fn exe_path(&self) -> PathBuf {
        if self.is_managed() {
            return managed_dir().join("soloist.exe");
        }
        let p = PathBuf::from(self.exe.trim().trim_matches('"'));
        if p.is_dir() { p.join("soloist.exe") } else { p }
    }
}

pub enum Event {
    Message(Value),
    /// Soloist quit. `expired` means Spotify retired this build and a newer one is needed.
    Stopped { expired: bool, message: String },
}

pub enum StartError {
    Expired,
    Other(String),
}

impl From<String> for StartError {
    fn from(e: String) -> Self {
        StartError::Other(e)
    }
}

const EXPIRED: i32 = 10;

fn managed_dir() -> PathBuf {
    data_dir().join("soloist")
}

/// Downloads the current Soloist build from Spotify into Spoty's data folder.
pub async fn download(http: &reqwest::Client) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("Couldn't download Soloist: {e}");
    let bytes = http
        .get(BUILD_URL)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| fail(&e))?
        .bytes()
        .await
        .map_err(|e| fail(&e))?;
    let zip = data_dir().join("soloist.zip");
    let fresh = data_dir().join("soloist.new");
    let _ = std::fs::remove_dir_all(&fresh);
    std::fs::create_dir_all(&fresh).map_err(|e| fail(&e))?;
    std::fs::write(&zip, &bytes).map_err(|e| fail(&e))?;
    // Windows 10 and later ship tar.exe, which reads .zip files.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = tokio::process::Command::new("tar")
        .arg("-xf")
        .arg(&zip)
        .arg("-C")
        .arg(&fresh)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .await
        .map_err(|e| fail(&e))?;
    let _ = std::fs::remove_file(&zip);
    if !status.success() {
        return Err(fail(&"couldn't unpack the archive"));
    }
    // The archive may wrap everything in a folder; keep whichever folder holds soloist.exe.
    let root = find_exe_dir(&fresh).ok_or_else(|| fail(&"soloist.exe wasn't in the archive"))?;
    let _ = std::fs::remove_dir_all(managed_dir());
    std::fs::rename(&root, managed_dir()).map_err(|e| fail(&e))?;
    let _ = std::fs::remove_dir_all(&fresh);
    Ok(())
}

fn find_exe_dir(dir: &Path) -> Option<PathBuf> {
    if dir.join("soloist.exe").is_file() {
        return Some(dir.to_path_buf());
    }
    std::fs::read_dir(dir).ok()?.flatten().filter(|e| e.path().is_dir()).find_map(|e| find_exe_dir(&e.path()))
}

pub struct Soloist {
    commands: mpsc::UnboundedSender<String>,
    // Closing the job kills Soloist, so it never outlives Spoty (even after a crash).
    _job: Job,
}

impl Soloist {
    pub async fn start(config: &Config, prefs: &Prefs, events: mpsc::UnboundedSender<Event>) -> Result<Soloist, StartError> {
        let exe = config.exe_path();
        if !exe.is_file() {
            return Err(format!("Couldn't find soloist.exe at {}", exe.display()).into());
        }
        let dir = data_dir().join("soloist-data");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(dir.join("ws.port"));
        let log = std::fs::File::create(data_dir().join("soloist.log")).map_err(|e| e.to_string())?;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut child = tokio::process::Command::new(&exe)
            .args(["--device-name", prefs.device_name.trim(), "--api-key", config.api_key.trim(), "--ws", "127.0.0.1:0"])
            .args(["--cache-size", &prefs.cache_mb.to_string(), "--initial-volume", &prefs.default_volume.to_string()])
            .arg("--data-dir")
            .arg(&dir)
            .arg("--cache-dir")
            .arg(prefs::cache_dir())
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .creation_flags(CREATE_NO_WINDOW)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Couldn't start Soloist: {e}"))?;
        let job = Job::for_process(child.raw_handle().unwrap_or(std::ptr::null_mut()));

        // Soloist writes the port it picked once the WebSocket server is up.
        let mut port = None;
        for _ in 0..100 {
            if let Some(code) = child.try_wait().ok().flatten() {
                return Err(match code.code() {
                    Some(EXPIRED) => StartError::Expired,
                    c => StartError::Other(exit_message(c)),
                });
            }
            if let Some(p) = read_port(&dir) {
                port = Some(p);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let port = port.ok_or_else(|| "Soloist started but its control socket never came up".to_string())?;
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
            .await
            .map_err(|e| format!("Couldn't connect to Soloist: {e}"))?;
        let (mut sink, mut stream) = ws.split();

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                if sink.send(Message::text(cmd)).await.is_err() {
                    break;
                }
            }
        });
        let ev = events.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                if let Message::Text(text) = msg
                    && let Ok(v) = serde_json::from_str::<Value>(text.as_str())
                {
                    let _ = ev.send(Event::Message(v));
                }
            }
        });
        tokio::spawn(async move {
            let status = child.wait().await.ok();
            let code = status.and_then(|s| s.code());
            let _ = events.send(Event::Stopped { expired: code == Some(EXPIRED), message: exit_message(code) });
        });

        let soloist = Soloist { commands: tx, _job: job };
        soloist.send(json!({ "command": "get_auth_state" }));
        Ok(soloist)
    }

    pub fn send(&self, mut cmd: Value) {
        cmd["type"] = json!("command");
        let _ = self.commands.send(cmd.to_string());
    }
}

fn read_port(dir: &Path) -> Option<u16> {
    std::fs::read_to_string(dir.join("ws.port")).ok()?.trim().parse().ok()
}

fn exit_message(code: Option<i32>) -> String {
    match code {
        Some(EXPIRED) => "This Soloist build has expired. Download the latest one from Spotify.".into(),
        Some(c) => format!("Soloist stopped (exit code {c}). Check your API key, or see soloist.log in %APPDATA%\\Spoty."),
        None => "Soloist stopped.".into(),
    }
}

/// A Windows job object that kills its processes when Spoty closes it or exits.
struct Job(windows_sys::Win32::Foundation::HANDLE);

// The handle is only used to close the job.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    fn for_process(process: windows_sys::Win32::Foundation::HANDLE) -> Job {
        use windows_sys::Win32::System::JobObjects::*;
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            );
            AssignProcessToJobObject(job, process);
            Job(job)
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}
