//! Background worker: owns sign-in, the library client, the Soloist player and the play queue.
//! The UI talks to it through `Cmd`s and reads the shared `State`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::auth::Auth;
use crate::library::{LIKED_SONGS, Library, Named, Playlist, Track};
use crate::prefs::{self, Prefs};
use crate::soloist::{self, Soloist};
use crate::songs::{self, SongSort};

/// How many pages Back can return through.
const MAX_HISTORY: usize = 30;
/// Song and album details kept in memory. Past these, whatever isn't on an open list or in the
/// queue is forgotten, and loads again if it's needed.
const MAX_DETAILS: usize = 3000;
const MAX_NAMES: usize = 2000;
/// A track we asked for normally starts well within this. If a different one shows up instead,
/// Spotify has swapped in another release of the song, so stop waiting.
const PLAY_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

pub enum Cmd {
    Login,
    Logout,
    Search(String),
    OpenLiked,
    OpenPlaylist(String),
    OpenAlbum(String),
    OpenArtist(String),
    Back,
    SetTab(Tab),
    LoadAlbums,
    LoadArtists,
    /// Fetch names for these albums/artists (rows that just scrolled into view).
    NeedNames(Vec<String>),
    /// Fetch list thumbnails, (key, image URL) pairs, decoded at this pixel size.
    NeedThumbs(Vec<(String, String)>, u32),
    /// Fetch details for these tracks (rows that just scrolled into view).
    NeedDetails(Vec<String>),
    Play(Vec<String>, usize),
    /// Sort the open list's songs.
    SetSongSort(SongSort),
    /// Move a song of the open list into the gap before a position, in its custom order.
    MoveSong(usize, usize),
    /// Forget the open list's custom order and start over from its playlist order.
    ResetCustomOrder,
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

#[derive(Clone, PartialEq, Hash)]
pub enum View {
    Empty,
    Search(String),
    Liked,
    Playlist(String),
    Album(String),
    Artist(String),
}

impl View {
    /// The URI of a list whose songs can be sorted.
    pub fn sortable_uri(&self) -> Option<&str> {
        match self {
            View::Liked => Some(LIKED_SONGS),
            View::Playlist(uri) => Some(uri),
            _ => None,
        }
    }
}

/// The tabs on search results and artist pages.
#[derive(Clone, Copy, PartialEq, Hash)]
pub enum Tab {
    Songs,
    Albums,
    Singles,
    Compilations,
    AppearsOn,
    Artists,
}

/// What the open view shows besides songs (search results or an artist's releases), as URIs.
#[derive(Default)]
pub struct Page {
    pub albums: Vec<String>,
    pub singles: Vec<String>,
    pub compilations: Vec<String>,
    pub appears_on: Vec<String>,
    pub artists: Vec<String>,
}

impl Page {
    pub fn items(&self, tab: Tab) -> &[String] {
        match tab {
            Tab::Songs => &[],
            Tab::Albums => &self.albums,
            Tab::Singles => &self.singles,
            Tab::Compilations => &self.compilations,
            Tab::AppearsOn => &self.appears_on,
            Tab::Artists => &self.artists,
        }
    }
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
    /// Track URIs of the open list, in the order they're shown.
    pub list: Vec<String>,
    /// How the open list is sorted, if it's a list that can be.
    pub song_sort: SongSort,
    /// Song details loaded so far for a sort by name: done and total.
    pub sort_progress: Option<(usize, usize)>,
    pub page: Page,
    pub tab: Tab,
    pub can_go_back: bool,
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
    /// The queue in its original order, kept only while shuffle is on.
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

    /// Whether a song that ends should be followed by another. On repeat-one Soloist loops
    /// the song by itself, so the queue stays put.
    fn has_next(&self) -> bool {
        match self.index {
            _ if self.repeat == Repeat::One => false,
            Some(i) => i + 1 < self.queue.len() || (self.repeat == Repeat::All && !self.queue.is_empty()),
            None => false,
        }
    }

    /// Shows the open list in a new order. If the queue was playing through this list, it
    /// continues in the new order too.
    fn show_order(&mut self, list: Vec<String>) {
        if self.original == self.list && !self.original.is_empty() {
            self.original = list.clone();
        } else if !self.shuffle && self.queue == self.list && !self.queue.is_empty() {
            let current = self.current_uri().map(str::to_string);
            self.queue = list.clone();
            self.index = current.and_then(|c| self.queue.iter().position(|u| *u == c)).or(self.index.map(|_| 0));
        }
        self.list = list;
    }

    /// Forgets details and names nothing open refers to, once there are too many of them.
    fn trim_caches(&mut self) {
        if self.details.len() > MAX_DETAILS {
            let keep: HashSet<&str> = self.list.iter().chain(&self.queue).chain(&self.original).map(String::as_str).collect();
            self.details.retain(|uri, _| keep.contains(uri.as_str()));
        }
        if self.names.len() > MAX_NAMES {
            let page = &self.page;
            let saved = self.albums.iter().chain(&self.artists).flatten();
            let shown = [&page.albums, &page.singles, &page.compilations, &page.appears_on, &page.artists];
            let keep: HashSet<&str> = saved.chain(shown.into_iter().flatten()).map(String::as_str).collect();
            self.names.retain(|uri, _| keep.contains(uri.as_str()));
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
        // The clock ticker redraws once the window is back, so nothing is missed.
        if crate::tray::window_on_screen() {
            self.ctx.request_repaint();
        }
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
        song_sort: SongSort::Playlist,
        sort_progress: None,
        page: Page::default(),
        tab: Tab::Songs,
        can_go_back: false,
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
    /// A track we asked Soloist to play that it hasn't confirmed yet, and when we asked.
    expected: Option<(String, Instant)>,
    /// The last track we asked Soloist to play. Soloist plays it as a context of its own, and
    /// anything else in that context is Spotify's autoplay moving on from it.
    started: Option<String>,
    /// The last track Soloist said it's on.
    reported: Option<String>,
    /// Whether our song was about to end when Soloist said the track changed.
    near_end_at_change: Option<bool>,
    /// The queue only mirrors what Soloist is playing from elsewhere, like the phone app or the
    /// session it resumed at startup. Soloist knows what comes before and after, not us.
    following_player: bool,
    /// Each list's chosen sort, for lists not in their playlist order.
    song_sorts: HashMap<String, SongSort>,
    /// The user picked a song before the player was linked; play it once it is.
    pending_play: bool,
    prefs: Prefs,
    /// Pages to return to with Back, oldest first.
    history: Vec<(View, Tab)>,
}

impl Engine {
    async fn run(shared: Shared, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        // Idle connections hold on to buffers; don't keep them around for long.
        let http = reqwest::Client::builder().pool_idle_timeout(Duration::from_secs(20)).build().unwrap_or_default();
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
                started: None,
                reported: None,
                near_end_at_change: None,
                following_player: false,
                song_sorts: songs::load_sorts(),
                pending_play: false,
                prefs: Prefs::load(),
                history: Vec::new(),
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
                s.page = Page::default();
                s.can_go_back = false;
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
        self.reported = None;
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
            Cmd::Search(q) => self.navigate(View::Search(q)),
            Cmd::OpenLiked => self.navigate(View::Liked),
            Cmd::OpenPlaylist(uri) => self.navigate(View::Playlist(uri)),
            Cmd::OpenAlbum(uri) => self.navigate(View::Album(uri)),
            Cmd::OpenArtist(uri) => self.navigate(View::Artist(uri)),
            Cmd::Back => {
                if let Some((view, tab)) = self.history.pop() {
                    self.load(view, tab);
                }
            }
            Cmd::SetTab(tab) => self.shared.update(|s| s.tab = tab),
            Cmd::NeedDetails(uris) => self.fetch_details(uris),
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
            Cmd::NeedThumbs(urls, px) => {
                let (lib, sh) = (self.lib.clone(), self.shared.clone());
                tokio::spawn(async move {
                    use futures_util::stream::{self, StreamExt};
                    let lib = &lib;
                    let thumbs: Vec<_> = stream::iter(urls)
                        .map(|(uri, url)| async move {
                            let bytes = lib.cover(&url).await.ok()?;
                            let img = image::load_from_memory(&bytes).ok()?.thumbnail(px, px).to_rgba8();
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
                            s.trim_caches();
                        });
                    });
                }
            }
            Cmd::Play(queue, index) => {
                self.shared.update(|s| {
                    s.queue = queue;
                    s.index = Some(index);
                    s.original.clear();
                    if s.shuffle {
                        s.original = s.queue.clone();
                        shuffle_after_current(s);
                    }
                });
                self.play_current();
            }
            Cmd::SetShuffle(on) => self.shared.update(|s| {
                if on == s.shuffle {
                    return;
                }
                s.shuffle = on;
                if on {
                    s.original = s.queue.clone();
                    shuffle_after_current(s);
                } else if !s.original.is_empty() {
                    // Back to the list's own order, staying on the current song.
                    let current = s.current_uri().map(str::to_string);
                    s.queue = std::mem::take(&mut s.original);
                    s.index = current.and_then(|c| s.queue.iter().position(|u| *u == c)).or(s.index.map(|_| 0));
                }
            }),
            Cmd::SetRepeat(r) => {
                self.send_repeat(r);
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
            Cmd::Prev => self.skip(-1),
            Cmd::SetSongSort(sort) => self.set_song_sort(sort),
            Cmd::MoveSong(from, gap) => self.shared.update(|s| {
                let Some(uri) = s.view.sortable_uri().map(str::to_string) else { return };
                if s.song_sort != SongSort::Custom || s.loading {
                    return;
                }
                let mut list = s.list.clone();
                if songs::move_song(&mut list, from, gap) {
                    s.show_order(list);
                    songs::save_order(&uri, &s.list);
                }
            }),
            Cmd::ResetCustomOrder => {
                let view = self.shared.state.lock().unwrap().view.clone();
                if let Some(uri) = view.sortable_uri() {
                    songs::forget_order(uri);
                    self.shared.update(|s| s.loading = true);
                    self.load_songs(view.clone(), SongSort::Custom, None);
                }
            }
            Cmd::Seek(ms) => self.seek(ms),
            Cmd::Volume(v) => {
                self.send(json!({ "command": "set_volume", "volume": v }));
                self.shared.update(|s| s.volume = v);
            }
        }
    }

    /// Soloist loops a single song itself; repeating the list is handled by our queue.
    fn send_repeat(&self, r: Repeat) {
        self.send(json!({ "command": "set_repeat_track", "enabled": r == Repeat::One }));
        self.send(json!({ "command": "set_repeat_context", "enabled": false }));
    }

    fn seek(&self, ms: u32) {
        self.send(json!({ "command": "seek", "position_ms": ms }));
        self.shared.update(|s| s.set_pos(ms));
    }

    /// Moves through the queue. Past either end of it, Soloist decides: it can go on with
    /// autoplay, or back through whatever it's playing from elsewhere.
    fn skip(&mut self, by: isize) {
        let (has_track, step) = {
            let s = self.shared.state.lock().unwrap();
            let mirroring_one_song = self.following_player && s.queue.len() == 1;
            let step = if mirroring_one_song { None } else { queue_step(s.index, s.queue.len(), by, s.repeat) };
            (s.index.is_some(), step)
        };
        match step {
            Some(i) => {
                self.shared.update(|s| s.index = Some(i));
                self.play_current();
            }
            None if !has_track => {}
            None if by > 0 => self.player_skip("skip_next"),
            None if self.following_player => self.player_skip("skip_prev"),
            // The first song of our own list: there's nothing before it.
            None => self.seek(0),
        }
    }

    fn player_skip(&self, command: &str) {
        if !self.active {
            self.send(json!({ "command": "activate" }));
        }
        self.send(json!({ "command": command }));
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
        self.expected = Some((uri.clone(), Instant::now()));
        self.started = Some(uri.clone());
        self.following_player = false;
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
                if ready {
                    // Repeat may have been chosen before the player was up, or it may have restarted.
                    let repeat = self.shared.state.lock().unwrap().repeat;
                    self.send_repeat(repeat);
                }
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
                    self.track_reported(uri.to_string(), context_uri(&v["context"]));
                }
                self.position(&v["position"]);
                self.status_changed(v["status"].as_str().unwrap_or(""));
            }
            "track_changed" => {
                if let Some(uri) = v["item"]["uri"].as_str() {
                    self.track_changed(uri);
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
                if was_playing && near_end && has_next && !self.waiting_for_track() {
                    self.skip(1);
                }
            }
        }
    }

    fn waiting_for_track(&self) -> bool {
        self.expected.as_ref().is_some_and(|(_, asked)| asked.elapsed() < PLAY_CONFIRM_TIMEOUT)
    }

    /// Clears the track we're waiting for if this is it. Returns whether to go on and handle
    /// this report: not while still waiting, since it's probably the song we just left.
    fn settle_expected(&mut self, uri: &str) -> bool {
        let confirmed = self.expected.as_ref().is_some_and(|(want, _)| want == uri);
        let waiting = self.waiting_for_track();
        if confirmed || !waiting {
            self.expected = None;
        }
        if confirmed || waiting {
            self.reported = Some(uri.to_string());
            return false;
        }
        true
    }

    /// Soloist's track-change event doesn't say what the track is playing from, and the event
    /// that does may come later. Ask for both together before deciding what the change means.
    fn track_changed(&mut self, uri: &str) {
        if !self.settle_expected(uri) || self.reported.as_deref() == Some(uri) {
            return;
        }
        // By the time the answer is here the clock will have moved on to the new track.
        self.near_end_at_change = Some(self.shared.state.lock().unwrap().near_end());
        self.send(json!({ "command": "get_state" }));
    }

    fn track_reported(&mut self, uri: String, context: Option<&str>) {
        if !self.settle_expected(&uri) || self.reported.as_deref() == Some(uri.as_str()) {
            return;
        }
        self.reported = Some(uri.clone());
        let in_our_context = context.is_some() && context == self.started.as_deref();
        let change = {
            let s = self.shared.state.lock().unwrap();
            let near_end = self.near_end_at_change.take().unwrap_or_else(|| s.near_end());
            what_changed(&s.queue, s.index, &uri, in_our_context, near_end, s.has_next())
        };
        match change {
            Change::Nothing => {}
            Change::Advance => self.skip(1),
            Change::Append => {
                self.shared.update(|s| {
                    if s.shuffle {
                        s.original.push(uri.clone());
                    }
                    s.queue.push(uri.clone());
                    s.index = Some(s.queue.len() - 1);
                });
                self.fetch_art(uri);
            }
            Change::Jump(i) => {
                self.shared.update(|s| s.index = Some(i));
                self.fetch_art(uri);
            }
            Change::External => {
                self.following_player = true;
                self.shared.update(|s| {
                    s.queue = vec![uri.clone()];
                    s.index = Some(0);
                    s.original.clear();
                });
                self.fetch_art(uri);
            }
        }
    }

    fn name_of(&self, uri: &str, fallback: &str) -> String {
        self.shared.state.lock().unwrap().names.get(uri).map_or(fallback.to_string(), |n| n.name.clone())
    }

    /// Opens a view, remembering the current one for Back.
    fn navigate(&mut self, view: View) {
        let (current, tab) = {
            let s = self.shared.state.lock().unwrap();
            (s.view.clone(), s.tab)
        };
        if current != View::Empty && current != view {
            self.history.push((current, tab));
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }
        self.load(view, Tab::Songs);
    }

    /// Shows a view and starts fetching what's on it.
    fn load(&self, view: View, tab: Tab) {
        let title = match &view {
            View::Empty => String::new(),
            View::Search(q) => format!("Results for “{q}”"),
            View::Liked => "Liked Songs".into(),
            View::Playlist(uri) => {
                let s = self.shared.state.lock().unwrap();
                s.playlists.iter().find(|p| p.uri == *uri).map_or("Playlist".into(), |p| p.name.clone())
            }
            View::Album(uri) => self.name_of(uri, "Album"),
            View::Artist(uri) => self.name_of(uri, "Artist"),
        };
        let can_go_back = !self.history.is_empty();
        self.shared.update(|s| {
            s.view = view.clone();
            s.title = title;
            s.tab = tab;
            s.can_go_back = can_go_back;
            s.list.clear();
            s.sort_progress = None;
            s.page = Page::default();
            s.loading = true;
            s.error = None;
        });
        let lib = self.lib.clone();
        match view.clone() {
            View::Empty => {}
            View::Search(q) => self.load_search(view, q),
            View::Liked | View::Playlist(_) => {
                let sort = view.sortable_uri().and_then(|uri| self.song_sorts.get(uri)).copied();
                let sort = sort.unwrap_or(SongSort::Playlist);
                self.shared.update(|s| s.song_sort = sort);
                self.load_songs(view, sort, None);
            }
            View::Album(uri) => {
                self.load_album_name(view.clone(), uri.clone());
                self.fill_list(view, async move { lib.context_tracks(&uri).await });
            }
            View::Artist(uri) => self.load_artist(view, uri),
        }
    }

    fn set_song_sort(&mut self, sort: SongSort) {
        let (view, shown) = {
            let s = self.shared.state.lock().unwrap();
            (s.view.clone(), s.list.clone())
        };
        let Some(uri) = view.sortable_uri() else { return };
        if sort == SongSort::Playlist {
            self.song_sorts.remove(uri);
        } else {
            self.song_sorts.insert(uri.to_string(), sort);
        }
        songs::save_sorts(&self.song_sorts);
        self.shared.update(|s| {
            s.song_sort = sort;
            s.sort_progress = None;
            s.loading = true;
        });
        // A first custom order starts from whatever order is on screen.
        self.load_songs(view.clone(), sort, (sort == SongSort::Custom).then_some(shown));
    }

    /// Fetches a list's songs and shows them in the given order. Nothing but the final order
    /// stays in memory, so the dates and such are fetched again the next time.
    fn load_songs(&self, view: View, sort: SongSort, shown: Option<Vec<String>>) {
        let Some(uri) = view.sortable_uri().map(str::to_string) else { return };
        let (lib, sh) = (self.lib.clone(), self.shared.clone());
        tokio::spawn(async move {
            let found = if view == View::Liked { lib.liked().await } else { lib.songs(&uri).await };
            let found = match found {
                Ok(found) => found,
                Err(e) => return sh.fail(format!("Couldn't load the list: {e}")),
            };
            let still_wanted = |s: &State| s.view == view && s.song_sort == sort;
            let list = match sort {
                SongSort::Newest | SongSort::Oldest => songs::by_date(found, sort == SongSort::Newest),
                SongSort::Playlist => found.into_iter().map(|s| s.uri).collect(),
                SongSort::Custom => {
                    let plain = found.into_iter().map(|s| s.uri).collect();
                    match songs::load_order(&uri) {
                        Some(saved) => songs::in_saved_order(&saved, plain),
                        None => {
                            let list = songs::in_saved_order(&shown.unwrap_or_default(), plain);
                            songs::save_order(&uri, &list);
                            list
                        }
                    }
                }
                SongSort::Title | SongSort::Artist | SongSort::Album => {
                    let mut list: Vec<String> = found.into_iter().map(|s| s.uri).collect();
                    // The list may have grown past the limit since this sort was chosen.
                    if list.len() > songs::MAX_SONGS_FOR_NAME_SORT {
                        sh.update(|s| {
                            if still_wanted(s) {
                                s.song_sort = SongSort::Playlist;
                                s.show_order(list);
                                s.loading = false;
                            }
                        });
                        return;
                    }
                    sh.update(|s| {
                        if still_wanted(s) {
                            s.show_order(list.clone());
                        }
                    });
                    if !load_all_details(&lib, &sh, &list, still_wanted).await {
                        return;
                    }
                    let s = sh.state.lock().unwrap();
                    songs::by_name(&mut list, &s.details, sort);
                    list
                }
            };
            sh.update(|s| {
                if still_wanted(s) {
                    s.show_order(list);
                    s.loading = false;
                    s.sort_progress = None;
                }
            });
        });
    }

    /// Asks for songs, albums and artists together and shows them all at once.
    fn load_search(&self, view: View, q: String) {
        let (lib, sh) = (self.lib.clone(), self.shared.clone());
        tokio::spawn(async move {
            let (songs, albums, artists) = tokio::join!(lib.search_tracks(&q), lib.search_albums(&q), lib.search_artists(&q));
            sh.update(|s| {
                if s.view != view {
                    return;
                }
                s.loading = false;
                let mut failed = Vec::new();
                match songs {
                    Ok(uris) => s.list = uris,
                    Err(e) => failed.push(format!("songs ({e})")),
                }
                let mut take = |found: Result<Vec<(String, Named)>, String>, what: &str| match found {
                    Ok(found) => {
                        let uris = found.iter().map(|(uri, _)| uri.clone()).collect();
                        s.names.extend(found);
                        uris
                    }
                    Err(e) => {
                        failed.push(format!("{what} ({e})"));
                        Vec::new()
                    }
                };
                s.page.albums = take(albums, "albums");
                s.page.artists = take(artists, "artists");
                if !failed.is_empty() {
                    s.error = Some(format!("Couldn't search {}", failed.join(", ")));
                }
                s.trim_caches();
            });
        });
    }

    fn load_artist(&self, view: View, uri: String) {
        let (lib, sh) = (self.lib.clone(), self.shared.clone());
        tokio::spawn(async move {
            match lib.artist(&uri).await {
                Ok(a) => sh.update(|s| {
                    if s.view != view {
                        return;
                    }
                    s.loading = false;
                    s.title = a.name.clone();
                    s.names.insert(uri, Named { name: a.name, subtitle: String::new(), year: 0, thumb_url: a.thumb_url });
                    s.list = a.top_tracks;
                    s.page.albums = a.albums;
                    s.page.singles = a.singles;
                    s.page.compilations = a.compilations;
                    s.page.appears_on = a.appears_on;
                    // Some artists have no top songs; open on the first tab that has something.
                    if s.tab == Tab::Songs && s.list.is_empty() {
                        let tabs = [Tab::Albums, Tab::Singles, Tab::Compilations, Tab::AppearsOn];
                        s.tab = tabs.into_iter().find(|t| !s.page.items(*t).is_empty()).unwrap_or(Tab::Songs);
                    }
                }),
                Err(e) => sh.fail(format!("Couldn't load this artist: {e}")),
            }
        });
    }

    /// An album opened from a song row may not have its name loaded yet.
    fn load_album_name(&self, view: View, uri: String) {
        if self.shared.state.lock().unwrap().names.contains_key(&uri) {
            return;
        }
        let (lib, sh) = (self.lib.clone(), self.shared.clone());
        tokio::spawn(async move {
            let found = lib.names(std::slice::from_ref(&uri)).await;
            sh.update(|s| {
                for (u, named) in found {
                    if s.view == view {
                        s.title = named.name.clone();
                    }
                    s.names.insert(u, named);
                }
            });
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
                s.trim_caches();
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

/// Loads the details of every song on a list, reporting progress. Returns false if the list
/// stopped being wanted along the way.
async fn load_all_details(lib: &Library, sh: &Shared, list: &[String], still_wanted: impl Fn(&State) -> bool) -> bool {
    let wanted: Vec<String> = {
        let s = sh.state.lock().unwrap();
        let unique: HashSet<&String> = list.iter().filter(|u| !s.details.contains_key(*u)).collect();
        unique.into_iter().cloned().collect()
    };
    let total = wanted.len();
    for (i, chunk) in wanted.chunks(40).enumerate() {
        sh.update(|s| s.sort_progress = Some((i * 40, total)));
        if !still_wanted(&sh.state.lock().unwrap()) {
            return false;
        }
        let found = lib.tracks(chunk).await;
        sh.update(|s| s.details.extend(found));
    }
    let mut s = sh.state.lock().unwrap();
    s.trim_caches();
    still_wanted(&s)
}

/// Where a step through the queue lands. `None` means off either end of it.
fn queue_step(index: Option<usize>, len: usize, by: isize, repeat: Repeat) -> Option<usize> {
    let target = index? as isize + by;
    if target >= 0 && target < len as isize {
        Some(target as usize)
    } else if repeat == Repeat::All && len > 0 {
        Some(target.rem_euclid(len as isize) as usize)
    } else {
        None
    }
}

/// What it means that Soloist moved to a track we didn't ask for.
#[derive(Debug, PartialEq)]
enum Change {
    /// It's the song we thought was playing.
    Nothing,
    /// Our song finished and Spotify's autoplay moved on: play the next one of ours instead.
    Advance,
    /// Nothing of ours is left, so let autoplay carry on, and keep the way back to our songs.
    Append,
    /// It went to another song on our queue, say from the phone app.
    Jump(usize),
    /// Something else started playback, say from the phone app. Just show it.
    External,
}

fn what_changed(queue: &[String], index: Option<usize>, uri: &str, in_our_context: bool, near_end: bool, has_next: bool) -> Change {
    let Some(i) = index else { return Change::External };
    if queue.get(i).is_some_and(|current| current == uri) {
        return Change::Nothing;
    }
    if in_our_context || near_end {
        return if has_next { Change::Advance } else { Change::Append };
    }
    match queue.iter().position(|u| u == uri) {
        Some(i) => Change::Jump(i),
        None => Change::External,
    }
}

/// The URI of what Soloist says it's playing from.
fn context_uri(context: &Value) -> Option<&str> {
    context["uri"].as_str().or(context.as_str()).filter(|u| !u.is_empty())
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

/// Redraws the UI twice a second while music plays so the clock moves, and once when the window
/// comes back from the tray. Otherwise the UI stays idle.
async fn clock_ticker(shared: Shared) {
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut was_on_screen = true;
    loop {
        tick.tick().await;
        let on_screen = crate::tray::window_on_screen();
        if on_screen && (!was_on_screen || shared.state.lock().unwrap().playing) {
            shared.ctx.request_repaint();
        }
        was_on_screen = on_screen;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> Vec<String> {
        ["a", "b", "c"].map(String::from).to_vec()
    }

    #[test]
    fn next_and_previous_step_through_the_queue() {
        assert_eq!(queue_step(Some(0), 3, 1, Repeat::Off), Some(1));
        assert_eq!(queue_step(Some(2), 3, -1, Repeat::Off), Some(1));
        // Repeat-one only loops a song that ends; the buttons still move.
        assert_eq!(queue_step(Some(1), 3, 1, Repeat::One), Some(2));
        assert_eq!(queue_step(Some(1), 3, -1, Repeat::One), Some(0));
    }

    #[test]
    fn stepping_past_the_ends_wraps_only_on_repeat_all() {
        assert_eq!(queue_step(Some(2), 3, 1, Repeat::Off), None);
        assert_eq!(queue_step(Some(0), 3, -1, Repeat::Off), None);
        assert_eq!(queue_step(Some(2), 3, 1, Repeat::All), Some(0));
        assert_eq!(queue_step(Some(0), 3, -1, Repeat::All), Some(2));
        assert_eq!(queue_step(None, 3, 1, Repeat::All), None);
    }

    #[test]
    fn autoplay_moving_on_from_our_song_keeps_the_queue() {
        // This used to replace the queue with the new song, which left Next and Previous with
        // nowhere to go. Spotify can move on well before the last few seconds of our song.
        let q = queue();
        assert_eq!(what_changed(&q, Some(0), "other", true, false, true), Change::Advance);
        assert_eq!(what_changed(&q, Some(0), "other", false, true, true), Change::Advance);
        assert_eq!(what_changed(&q, Some(2), "other", true, false, false), Change::Append);
        assert_eq!(what_changed(&q, Some(2), "other", false, true, false), Change::Append);
    }

    #[test]
    fn playback_from_elsewhere_is_shown() {
        let q = queue();
        assert_eq!(what_changed(&q, Some(0), "other", false, false, true), Change::External);
        assert_eq!(what_changed(&q, Some(0), "c", false, false, true), Change::Jump(2));
        assert_eq!(what_changed(&[], None, "other", false, false, false), Change::External);
    }

    #[test]
    fn the_current_song_is_not_a_change() {
        assert_eq!(what_changed(&queue(), Some(1), "b", true, true, true), Change::Nothing);
    }

    #[test]
    fn context_can_be_read_either_way() {
        assert_eq!(context_uri(&json!({ "uri": "spotify:track:x" })), Some("spotify:track:x"));
        assert_eq!(context_uri(&json!("spotify:track:x")), Some("spotify:track:x"));
        assert_eq!(context_uri(&json!({ "uri": "" })), None);
        assert_eq!(context_uri(&Value::Null), None);
    }
}
