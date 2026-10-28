//! Background worker: owns sign-in, the library client, the Soloist player and the play queue.
//! The UI talks to it through `Cmd`s and reads the shared `State`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::auth::Auth;
use crate::library::{Library, Named, Playlist, Track};
use crate::prefs::{self, Prefs};
use crate::soloist::{self, Soloist};

/// Pixel size list thumbnails are decoded at.
const THUMB_PX: u32 = 48;

pub enum Cmd {
    Login,
    Logout,
    Search(String),
    OpenLiked,
    OpenPlaylist(Playlist),
    OpenAlbum(String),
    OpenArtist(String),
    LoadAlbums,
    LoadArtists,
    /// Fetch names for these albums/artists (rows that just scrolled into view).
    NeedNames(Vec<String>),
    /// Fetch list thumbnails: (key, image URL) pairs.
    NeedThumbs(Vec<(String, String)>),
    /// Fetch details for these tracks (rows that just scrolled into view).
    NeedDetails(Vec<String>),
    Play(Vec<String>, usize),
    Toggle,
    Next,
    Prev,
    Seek(u32),
    Volume(u8),
    SetupAudio(soloist::Config),
    SetShuffle(bool),
    SetRepeat(Repeat),
    SavePrefs(Prefs),
    ClearCache,
    RedownloadPlayer,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Repeat {
    Off,
    /// Start over at the top of the list after the last song.
    All,
    One,
}

#[derive(Clone, PartialEq)]
pub enum AuthState {
    Starting,
    SignedOut,
    Waiting,
    Ready,
}

#[derive(Clone, PartialEq)]
pub enum Audio {
    NotSetUp,
    Downloading,
    Starting,
    /// Soloist runs but no account is linked yet.
    Pairing,
    Ready,
    Failed(String),
}

#[derive(Clone, PartialEq)]
pub enum View {
    Empty,
    Search(String),
    Liked,
    Playlist(String),
    Album(String),
    Artist(String),
}

pub struct State {
    pub auth: AuthState,
    pub audio: Audio,
    pub error: Option<String>,
    pub playlists: Vec<Playlist>,
    /// Saved albums and followed artists, loaded the first time their tab is opened.
    pub albums: Option<Vec<String>>,
    pub artists: Option<Vec<String>>,
    pub names: HashMap<String, Named>,
    names_requested: HashSet<String>,
    /// Freshly decoded thumbnails; the UI takes them (and owns them from then on).
    pub new_thumbs: Vec<(String, egui::ColorImage)>,
    pub view: View,
    pub title: String,
    /// Track URIs of the open list.
    pub list: Vec<String>,
    pub loading: bool,
    /// Track details by URI, filled in lazily.
    pub details: HashMap<String, Track>,
    requested: HashSet<String>,
    pub queue: Vec<String>,
    pub index: Option<usize>,
    pub playing: bool,
    pos_ms: u32,
    pos_at: Instant,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// The queue in its original (unshuffled) order.
    original: Vec<String>,
    /// Cover art for the current track, keyed by its URI.
    pub art: Option<(String, egui::ColorImage)>,
}

impl State {
    pub fn current_uri(&self) -> Option<&str> {
        self.queue.get(self.index?).map(String::as_str)
    }

    pub fn current(&self) -> Option<&Track> {
        self.details.get(self.current_uri()?)
    }

    pub fn position_ms(&self) -> u32 {
        let dur = self.current().map_or(u32::MAX, |t| t.duration_ms);
        let mut p = self.pos_ms as u64;
        if self.playing {
            p += self.pos_at.elapsed().as_millis() as u64;
        }
        (p.min(u32::MAX as u64) as u32).min(dur)
    }

    fn set_pos(&mut self, ms: u32) {
        self.pos_ms = ms;
        self.pos_at = Instant::now();
    }

    fn has_next(&self) -> bool {
        match self.index {
            Some(i) => i + 1 < self.queue.len() || (self.repeat == Repeat::All && !self.queue.is_empty()),
            None => false,
        }
    }

    fn near_end(&self) -> bool {
        self.current().is_some_and(|t| t.duration_ms > 0 && self.position_ms() + 4000 >= t.duration_ms)
    }
}

#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<State>>,
    ctx: egui::Context,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap());
        self.ctx.request_repaint();
    }

    fn fail(&self, msg: impl Into<String>) {
        let msg = msg.into();
        self.update(|s| {
            s.error = Some(msg);
            s.loading = false;
        });
    }
}

pub fn start(ctx: egui::Context) -> (mpsc::UnboundedSender<Cmd>, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State {
        auth: AuthState::Starting,
        audio: Audio::NotSetUp,
        error: None,
        playlists: Vec::new(),
        albums: None,
        artists: None,
        names: HashMap::new(),
        names_requested: HashSet::new(),
        new_thumbs: Vec::new(),
        view: View::Empty,
        title: String::new(),
        list: Vec::new(),
        loading: false,
        details: HashMap::new(),
        requested: HashSet::new(),
        queue: Vec::new(),
        index: None,
        playing: false,
        pos_ms: 0,
        pos_at: Instant::now(),
        volume: Prefs::load().default_volume,
        shuffle: false,
        repeat: Repeat::Off,
        original: Vec::new(),
        art: None,
    }));
    let (tx, rx) = mpsc::unbounded_channel();
    let shared = Shared { state: state.clone(), ctx };
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.spawn(clock_ticker(shared.clone()));
        rt.block_on(Engine::run(shared, rx));
    });
    (tx, state)
}

struct Engine {
    shared: Shared,
    http: reqwest::Client,
    config: Option<soloist::Config>,
    lib: Arc<Library>,
    player: Option<Soloist>,
    player_events: mpsc::UnboundedSender<soloist::Event>,
    /// Soloist is the active Spotify Connect device.
    active: bool,
    /// Last status Soloist reported ("idle", "playing", "paused", "buffering").
    status: String,
    /// A track we asked Soloist to play that it hasn't confirmed yet.
    expected: Option<String>,
    /// The user picked a song before the player was linked; play it once it is.
    pending_play: bool,
    prefs: Prefs,
}

impl Engine {
    async fn run(shared: Shared, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        let http = reqwest::Client::new();
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        loop {
            let Some(auth) = sign_in(&shared, &http, &mut rx).await else { return };
            let lib = Arc::new(Library::new(http.clone(), Arc::new(auth)));
            let mut engine = Engine {
                shared: shared.clone(),
                http: http.clone(),
                config: None,
                lib,
                player: None,
                player_events: ev_tx.clone(),
                active: false,
                status: String::new(),
                expected: None,
                pending_play: false,
                prefs: Prefs::load(),
            };
            shared.update(|s| s.auth = AuthState::Ready);
            engine.load_playlists();
            if let Some(cfg) = soloist::Config::load() {
                engine.start_player(cfg, false).await;
            }
            if !engine.serve(&mut rx, &mut ev_rx).await {
                return;
            }
            // Signed out: stop audio and go back to the sign-in screen.
            drop(engine);
            Auth::forget();
            shared.update(|s| {
                s.auth = AuthState::SignedOut;
                s.audio = Audio::NotSetUp;
                s.playlists.clear();
                s.albums = None;
                s.artists = None;
                s.list.clear();
                s.view = View::Empty;
                s.queue.clear();
                s.index = None;
                s.playing = false;
                s.art = None;
            });
        }
    }

    /// Handles commands and player events. Returns false when the UI is gone, true on sign-out.
    async fn serve(
        &mut self,
        rx: &mut mpsc::UnboundedReceiver<Cmd>,
        events: &mut mpsc::UnboundedReceiver<soloist::Event>,
    ) -> bool {
        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    None => return false,
                    Some(Cmd::Logout) => return true,
                    Some(cmd) => self.command(cmd).await,
                },
                Some(ev) = events.recv() => match ev {
                    soloist::Event::Message(v) => self.player_message(&v),
                    soloist::Event::Stopped { expired, message } => {
                        self.player = None;
                        self.shared.update(|s| s.playing = false);
                        match self.config.clone() {
                            // Spotify retires builds every few months; fetch the new one and carry on.
                            Some(cfg) if expired && cfg.is_managed() => self.start_player(cfg, true).await,
                            _ => self.shared.update(|s| s.audio = Audio::Failed(message)),
                        }
                    }
                },
            }
        }
    }

    /// Starts Soloist, downloading it first if Spoty manages it and it's missing (or `refresh`).
    async fn start_player(&mut self, cfg: soloist::Config, mut refresh: bool) {
        self.player = None; // stops any previous instance
        self.active = false;
        self.config = Some(cfg.clone());
        for attempt in 0..2 {
            if cfg.is_managed() && (refresh || !cfg.exe_path().is_file()) {
                self.shared.update(|s| s.audio = Audio::Downloading);
                if let Err(e) = soloist::download(&self.http).await {
                    self.shared.update(|s| s.audio = Audio::Failed(e));
                    return;
                }
            }
            self.shared.update(|s| s.audio = Audio::Starting);
            match Soloist::start(&cfg, &self.prefs, self.player_events.clone()).await {
                Ok(p) => {
                    self.player = Some(p);
                    return;
                }
                Err(soloist::StartError::Expired) if cfg.is_managed() && attempt == 0 => refresh = true,
                Err(soloist::StartError::Expired) => {
                    let msg = "This Soloist build has expired. Download the latest one from Spotify.";
                    self.shared.update(|s| s.audio = Audio::Failed(msg.into()));
                    return;
                }
                Err(soloist::StartError::Other(e)) => {
                    self.shared.update(|s| s.audio = Audio::Failed(e));
                    return;
                }
            }
        }
    }

    fn send(&self, cmd: Value) {
        if let Some(p) = &self.player {
            p.send(cmd);
        }
    }

    async fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Login | Cmd::Logout => {}
            Cmd::SetupAudio(cfg) => {
                cfg.save();
                self.start_player(cfg, false).await;
            }
            Cmd::Search(q) => {
                let view = View::Search(q.clone());
                self.open(view.clone(), format!("Results for “{q}”"));
                let lib = self.lib.clone();
                self.fill_list(view, async move { lib.search(&q).await });
            }
            Cmd::OpenLiked => {
                self.open(View::Liked, "Liked Songs".into());
                let lib = self.lib.clone();
                self.fill_list(View::Liked, async move { lib.liked().await });
            }
            Cmd::OpenPlaylist(p) => {
                let view = View::Playlist(p.uri.clone());
                self.open(view.clone(), p.name.clone());
                let lib = self.lib.clone();
                self.fill_list(view, async move { lib.context_tracks(&p.uri).await });
            }
            Cmd::NeedDetails(uris) => self.fetch_details(uris),
            Cmd::OpenAlbum(uri) => {
                let title = self.name_of(&uri, "Album");
                self.open(View::Album(uri.clone()), title);
                let lib = self.lib.clone();
                self.fill_list(View::Album(uri.clone()), async move { lib.context_tracks(&uri).await });
            }
            Cmd::OpenArtist(uri) => {
                let title = self.name_of(&uri, "Artist");
                self.open(View::Artist(uri.clone()), title);
                let lib = self.lib.clone();
                self.fill_list(View::Artist(uri.clone()), async move { lib.artist_tracks(&uri).await });
            }
            Cmd::LoadAlbums => {
                let (lib, sh) = (self.lib.clone(), self.shared.clone());
                sh.update(|s| s.albums = Some(Vec::new()));
                tokio::spawn(async move {
                    match lib.saved_albums().await {
                        Ok(a) => sh.update(|s| s.albums = Some(a)),
                        Err(e) => sh.fail(format!("Couldn't load albums: {e}")),
                    }
                });
            }
            Cmd::LoadArtists => {
                let (lib, sh) = (self.lib.clone(), self.shared.clone());
                sh.update(|s| s.artists = Some(Vec::new()));
                tokio::spawn(async move {
                    match lib.followed_artists().await {
                        Ok(a) => sh.update(|s| s.artists = Some(a)),
                        Err(e) => sh.fail(format!("Couldn't load artists: {e}")),
                    }
                });
            }
            Cmd::NeedThumbs(urls) => {
                let (lib, sh) = (self.lib.clone(), self.shared.clone());
                tokio::spawn(async move {
                    use futures_util::stream::{self, StreamExt};
                    let lib = &lib;
                    let thumbs: Vec<_> = stream::iter(urls)
                        .map(|(uri, url)| async move {
                            let bytes = lib.cover(&url).await.ok()?;
                            let img = image::load_from_memory(&bytes).ok()?.thumbnail(THUMB_PX, THUMB_PX).to_rgba8();
                            let size = [img.width() as usize, img.height() as usize];
                            Some((uri, egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw())))
                        })
                        .buffer_unordered(6)
                        .filter_map(|x| async move { x })
                        .collect()
                        .await;
                    sh.update(|s| s.new_thumbs.extend(thumbs));
                });
            }
            Cmd::NeedNames(uris) => {
                let wanted: Vec<String> = {
                    let mut s = self.shared.state.lock().unwrap();
                    let wanted: Vec<String> =
                        uris.into_iter().filter(|u| !s.names.contains_key(u) && !s.names_requested.contains(u)).collect();
                    s.names_requested.extend(wanted.iter().cloned());
                    wanted
                };
                if !wanted.is_empty() {
                    let (lib, sh) = (self.lib.clone(), self.shared.clone());
                    tokio::spawn(async move {
                        let found = lib.names(&wanted).await;
                        sh.update(|s| {
                            for u in &wanted {
                                s.names_requested.remove(u);
                            }
                            s.names.extend(found);
                        });
                    });
                }
            }
            Cmd::Play(queue, index) => {
                self.shared.update(|s| {
                    s.original = queue.clone();
                    s.queue = queue;
                    s.index = Some(index);
                    if s.shuffle {
                        shuffle_after_current(s);
                    }
                });
                self.play_current();
            }
            Cmd::SetShuffle(on) => self.shared.update(|s| {
                s.shuffle = on;
                if on {
                    shuffle_after_current(s);
                } else if !s.original.is_empty() {
                    // Back to the list's own order, staying on the current song.
                    let current = s.current_uri().map(str::to_string);
                    s.queue = s.original.clone();
                    s.index = current.and_then(|c| s.queue.iter().position(|u| *u == c)).or(s.index.map(|_| 0));
                }
            }),
            Cmd::SetRepeat(r) => {
                // Soloist loops a single song itself; repeating the list is handled by our queue.
                self.send(json!({ "command": "set_repeat_track", "enabled": r == Repeat::One }));
                self.send(json!({ "command": "set_repeat_context", "enabled": false }));
                self.shared.update(|s| s.repeat = r);
            }
            Cmd::SavePrefs(p) => {
                let restart = p.player_settings_differ(&self.prefs);
                p.save();
                self.prefs = p;
                if restart && let Some(cfg) = self.config.clone() {
                    self.start_player(cfg, false).await;
                }
            }
            Cmd::ClearCache => {
                // Soloist holds the cache open, so stop it while the files go.
                let cfg = self.config.clone();
                self.player = None;
                tokio::time::sleep(Duration::from_millis(500)).await;
                let _ = std::fs::remove_dir_all(prefs::cache_dir());
                if let Some(cfg) = cfg {
                    self.start_player(cfg, false).await;
                }
            }
            Cmd::RedownloadPlayer => {
                if let Some(cfg) = self.config.clone().filter(soloist::Config::is_managed) {
                    self.start_player(cfg, true).await;
                }
            }
            Cmd::Toggle => {
                let (has_track, playing) = {
                    let s = self.shared.state.lock().unwrap();
                    (s.index.is_some(), s.playing)
                };
                if playing {
                    self.send(json!({ "command": "pause" }));
                } else if has_track && self.status == "paused" && self.active {
                    self.send(json!({ "command": "play" }));
                } else if has_track {
                    self.play_current();
                }
            }
            Cmd::Next => self.skip(1),
            Cmd::Prev => {
                if self.shared.state.lock().unwrap().position_ms() > 3000 {
                    self.seek(0);
                } else {
                    self.skip(-1);
                }
            }
            Cmd::Seek(ms) => self.seek(ms),
            Cmd::Volume(v) => {
                self.send(json!({ "command": "set_volume", "volume": v }));
                self.shared.update(|s| s.volume = v);
            }
        }
    }

    fn seek(&self, ms: u32) {
        self.send(json!({ "command": "seek", "position_ms": ms }));
        self.shared.update(|s| s.set_pos(ms));
    }

    fn skip(&mut self, by: isize) {
        let moved = {
            let mut s = self.shared.state.lock().unwrap();
            let len = s.queue.len() as isize;
            match s.index.map(|i| i as isize + by) {
                Some(i) if i >= 0 && i < len => {
                    s.index = Some(i as usize);
                    true
                }
                // Repeat all: wrap around the ends of the list.
                Some(i) if s.repeat == Repeat::All && len > 0 => {
                    s.index = Some(i.rem_euclid(len) as usize);
                    true
                }
                _ => false,
            }
        };
        if moved {
            self.play_current();
        } else if by < 0 {
            self.seek(0);
        }
    }

    fn play_current(&mut self) {
        let Some(uri) = self.shared.state.lock().unwrap().current_uri().map(str::to_string) else { return };
        if self.player.is_none() {
            self.shared.update(|s| s.error = Some("Set up playback first (button at the bottom).".into()));
            return;
        }
        if self.shared.state.lock().unwrap().audio != Audio::Ready {
            // Still pairing or starting: remember the choice and play it when the player is ready.
            self.pending_play = true;
            self.fetch_art(uri);
            return;
        }
        if !self.active {
            self.send(json!({ "command": "activate" }));
        }
        self.send(json!({ "command": "play", "uri": uri }));
        self.expected = Some(uri.clone());
        self.shared.update(|s| {
            s.set_pos(0);
            s.error = None;
        });
        self.fetch_art(uri);
    }

    fn player_message(&mut self, v: &Value) {
        match v["type"].as_str().unwrap_or("") {
            "auth_state" => {
                self.active = v["is_active"].as_bool().unwrap_or(false);
                let ready = v["logged_in"].as_bool().unwrap_or(false);
                self.shared.update(|s| s.audio = if ready { Audio::Ready } else { Audio::Pairing });
                if ready && std::mem::take(&mut self.pending_play) {
                    self.play_current();
                } else if ready {
                    self.send(json!({ "command": "get_state" }));
                }
            }
            "device_changed" => {
                if let Some(a) = v["is_active"].as_bool() {
                    self.active = a;
                }
            }
            "playback_state" => {
                if let Some(a) = v["is_active"].as_bool() {
                    self.active = a;
                }
                if let Some(vol) = v["volume"].as_u64() {
                    self.shared.update(|s| s.volume = vol.min(100) as u8);
                }
                if let Some(uri) = v["item"]["uri"].as_str() {
                    self.track_changed(uri.to_string());
                }
                self.position(&v["position"]);
                self.status_changed(v["status"].as_str().unwrap_or(""));
            }
            "track_changed" => {
                if let Some(uri) = v["item"]["uri"].as_str() {
                    self.track_changed(uri.to_string());
                }
            }
            "playback_changed" => self.status_changed(v["status"].as_str().unwrap_or("")),
            "position_sync" => self.position(&v["position"]),
            "volume_changed" => {
                if let Some(vol) = v["volume"].as_u64() {
                    self.shared.update(|s| s.volume = vol.min(100) as u8);
                }
            }
            "error" => {
                let msg = v["message"].as_str().unwrap_or("Soloist reported an error").to_string();
                self.shared.update(|s| s.error = Some(format!("Player: {msg}")));
            }
            _ => {}
        }
    }

    fn position(&self, p: &Value) {
        let Some(ms) = p["position_ms"].as_u64() else { return };
        // The position was measured at `timestamp_ms`; carry it forward to now.
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let ts = p["timestamp_ms"].as_u64().unwrap_or(now);
        let speed = p["speed"].as_f64().unwrap_or(1.0);
        let elapsed = if speed > 0.0 { (now.saturating_sub(ts) as f64 * speed) as u64 } else { 0 };
        self.shared.update(|s| s.set_pos((ms + elapsed) as u32));
    }

    fn status_changed(&mut self, status: &str) {
        if status.is_empty() {
            return;
        }
        let was_playing = self.status == "playing";
        self.status = status.to_string();
        let (near_end, has_next) = {
            let s = self.shared.state.lock().unwrap();
            (s.near_end(), s.has_next())
        };
        match status {
            "playing" => self.shared.update(|s| {
                let pos = s.position_ms();
                s.playing = true;
                s.set_pos(pos);
            }),
            "buffering" => {}
            _ => {
                self.shared.update(|s| {
                    let pos = s.position_ms();
                    s.playing = false;
                    s.set_pos(pos);
                });
                // The track ran out and Soloist stopped: continue with our queue.
                if was_playing && near_end && has_next && self.expected.is_none() {
                    self.skip(1);
                }
            }
        }
    }

    fn track_changed(&mut self, uri: String) {
        if self.expected.as_deref() == Some(uri.as_str()) {
            self.expected = None;
            return;
        }
        if self.expected.is_some() {
            return; // still waiting for the track we asked for
        }
        let (current, near_end, has_next) = {
            let s = self.shared.state.lock().unwrap();
            (s.current_uri().map(str::to_string), s.near_end(), s.has_next())
        };
        if current.as_deref() == Some(uri.as_str()) {
            return;
        }
        if near_end && has_next {
            // Our track ended and Spotify moved on by itself (autoplay); play our next one instead.
            self.skip(1);
            return;
        }
        // Something else started playback (e.g. from the phone app): show it.
        self.shared.update(|s| {
            s.queue = vec![uri.clone()];
            s.index = Some(0);
        });
        self.fetch_art(uri);
    }

    fn name_of(&self, uri: &str, fallback: &str) -> String {
        self.shared.state.lock().unwrap().names.get(uri).map_or(fallback.to_string(), |n| n.name.clone())
    }

    fn open(&self, view: View, title: String) {
        self.shared.update(|s| {
            s.view = view;
            s.title = title;
            s.list.clear();
            s.loading = true;
            s.error = None;
        });
    }

    /// Runs a list query and shows the result if the user is still on that view.
    fn fill_list(&self, view: View, query: impl Future<Output = Result<Vec<String>, String>> + Send + 'static) {
        let sh = self.shared.clone();
        tokio::spawn(async move {
            match query.await {
                Ok(uris) => sh.update(|s| {
                    if s.view == view {
                        s.list = uris;
                        s.loading = false;
                    }
                }),
                Err(e) => sh.fail(format!("Couldn't load the list: {e}")),
            }
        });
    }

    fn fetch_details(&self, uris: Vec<String>) {
        let wanted: Vec<String> = {
            let mut s = self.shared.state.lock().unwrap();
            let wanted: Vec<String> =
                uris.into_iter().filter(|u| !s.details.contains_key(u) && !s.requested.contains(u)).collect();
            s.requested.extend(wanted.iter().cloned());
            wanted
        };
        if wanted.is_empty() {
            return;
        }
        let (sh, lib) = (self.shared.clone(), self.lib.clone());
        tokio::spawn(async move {
            let found = lib.tracks(&wanted).await;
            sh.update(|s| {
                for u in &wanted {
                    s.requested.remove(u);
                }
                s.details.extend(found);
            });
        });
    }

    fn load_playlists(&self) {
        let (lib, sh) = (self.lib.clone(), self.shared.clone());
        tokio::spawn(async move {
            match lib.playlists().await {
                Ok(p) => sh.update(|s| s.playlists = p),
                Err(e) => sh.fail(format!("Couldn't load playlists: {e}")),
            }
        });
    }

    /// Loads the current track's details (if needed) and its cover.
    fn fetch_art(&self, uri: String) {
        if self.shared.state.lock().unwrap().art.as_ref().is_some_and(|(u, _)| *u == uri) {
            return;
        }
        let (sh, lib) = (self.shared.clone(), self.lib.clone());
        tokio::spawn(async move {
            let known = sh.state.lock().unwrap().details.get(&uri).cloned();
            let track = match known {
                Some(t) => t,
                None => {
                    let Some((_, t)) = lib.tracks(std::slice::from_ref(&uri)).await.pop() else { return };
                    sh.update(|s| {
                        s.details.insert(uri.clone(), t.clone());
                    });
                    t
                }
            };
            let Some(url) = track.cover_url else { return };
            let Ok(bytes) = lib.cover(&url).await else { return };
            let Ok(img) = image::load_from_memory(&bytes) else { return };
            let img = img.thumbnail(112, 112).to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            sh.update(|s| {
                if s.current_uri() == Some(uri.as_str()) {
                    s.art = Some((uri, color));
                }
            });
        });
    }
}

/// Keeps the current song first and shuffles everything else.
fn shuffle_after_current(s: &mut State) {
    let Some(i) = s.index else { return };
    s.queue.swap(0, i);
    s.index = Some(0);
    let rest = &mut s.queue[1..];
    for k in (1..rest.len()).rev() {
        let j = getrandom::u32().unwrap_or(0) as usize % (k + 1);
        rest.swap(k, j);
    }
}

/// Waits until the user is signed in. Returns `None` if the UI went away.
async fn sign_in(shared: &Shared, http: &reqwest::Client, rx: &mut mpsc::UnboundedReceiver<Cmd>) -> Option<Auth> {
    if let Some(auth) = Auth::restore(http).await {
        return Some(auth);
    }
    loop {
        shared.update(|s| s.auth = AuthState::SignedOut);
        while !matches!(rx.recv().await?, Cmd::Login) {}
        shared.update(|s| {
            s.auth = AuthState::Waiting;
            s.error = None;
        });
        match tokio::time::timeout(Duration::from_secs(300), Auth::login(http)).await {
            Ok(Ok(a)) => return Some(a),
            Ok(Err(e)) => shared.fail(format!("Sign-in failed: {e}")),
            Err(_) => shared.fail("Sign-in timed out. Try again."),
        }
    }
}

/// Redraws the UI twice a second while music plays so the clock moves; otherwise the UI stays idle.
async fn clock_ticker(shared: Shared) {
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tick.tick().await;
        if shared.state.lock().unwrap().playing {
            shared.ctx.request_repaint();
        }
    }
}
