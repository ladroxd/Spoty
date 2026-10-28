use std::sync::{Arc, Mutex};

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Margin, RichText, Sense, Stroke, TextureHandle,
    Vec2,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::engine::{self, Audio, AuthState, Cmd, Repeat, State, View};
use crate::prefs::{self, Prefs};
use crate::library::Track;
use crate::soloist;

const ACCENT: Color32 = Color32::from_rgb(88, 204, 140);
const BG: Color32 = Color32::from_rgb(18, 18, 20);
const PANEL: Color32 = Color32::from_rgb(26, 26, 30);
const MUTED: Color32 = Color32::from_rgb(150, 150, 158);
const ROW_H: f32 = 40.0;
const NAV_H: f32 = 28.0;
const NAV2_H: f32 = 40.0;
/// Most album thumbnails kept in memory (about 9 KB each); the least recently seen go first.
const MAX_THUMBS: usize = 150;

#[derive(Clone, Copy, PartialEq)]
enum Owner {
    All,
    Mine,
    Others,
}

#[derive(Clone, Copy, PartialEq)]
enum LibTab {
    Playlists,
    Albums,
    Artists,
}

pub struct App {
    tx: UnboundedSender<Cmd>,
    state: Arc<Mutex<State>>,
    query: String,
    art: Option<(String, TextureHandle)>,
    seeking: Option<u32>,
    /// The playback setup dialog, while open.
    setup: Option<soloist::Config>,
    setup_offered: bool,
    pairing_hidden: bool,
    tray_ready: bool,
    /// Started with --minimized: hide to the tray once it exists.
    start_hidden: bool,
    /// The preferences being edited, while that window is open.
    prefs: Option<Prefs>,
    /// Size of Soloist's audio cache and when it was measured.
    cache_size: Option<(std::time::Instant, u64)>,
    about_open: bool,
    lib_tab: LibTab,
    /// Filters the open library tab by name.
    lib_filter: String,
    /// Which playlists to show by owner.
    owner: Owner,
    /// Album thumbnails, with the frame each was last drawn in.
    thumbs: std::collections::HashMap<String, (TextureHandle, u64)>,
    thumbs_pending: std::collections::HashSet<String>,
    frame: u64,
    /// Pinned playlists/albums/artists, most recently pinned first.
    pins: Vec<String>,
}

impl App {
    pub fn new(ctx: egui::Context, start_hidden: bool) -> Self {
        style(&ctx);
        let (tx, state) = engine::start(ctx);
        App {
            tx,
            state,
            query: String::new(),
            art: None,
            seeking: None,
            setup: None,
            setup_offered: false,
            pairing_hidden: false,
            tray_ready: false,
            start_hidden,
            prefs: None,
            cache_size: None,
            about_open: false,
            lib_tab: LibTab::Playlists,
            lib_filter: String::new(),
            owner: Owner::All,
            thumbs: Default::default(),
            thumbs_pending: Default::default(),
            frame: 0,
            pins: load_pins(),
        }
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

/// Shown by a second copy of Spoty: explains, counts down, then hands over to the open one.
pub struct AlreadyRunning {
    started: std::time::Instant,
    /// The window of the Spoty that's already open (an HWND).
    other: usize,
}

const CLOSE_AFTER_SECS: u64 = 5;

impl AlreadyRunning {
    pub fn new(ctx: egui::Context, other: usize) -> Self {
        style(&ctx);
        // Tick the countdown, and close even if the window is never repainted.
        std::thread::spawn(move || {
            for _ in 0..CLOSE_AFTER_SECS * 4 {
                std::thread::sleep(std::time::Duration::from_millis(250));
                ctx.request_repaint();
            }
            AlreadyRunning::finish(other);
        });
        AlreadyRunning { started: std::time::Instant::now(), other }
    }

    fn finish(other: usize) -> ! {
        if other != 0 {
            crate::tray::bring_to_front(other as _);
        }
        std::process::exit(0);
    }
}

impl egui_software_backend::App for AlreadyRunning {
    fn ui(&mut self, ui: &mut egui::Ui, _backend: &mut egui_software_backend::SoftwareBackend) {
        let left = CLOSE_AFTER_SECS.saturating_sub(self.started.elapsed().as_secs());
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(Margin::same(16))).show_inside(ui, |ui| {
            ui.label(RichText::new("Spoty is already running").size(17.0).strong());
            ui.add_space(4.0);
            ui.label(RichText::new("Only one Spoty can run at a time. This window will close and switch to the open one.").color(MUTED));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let btn = egui::Button::new(RichText::new(format!("Switch now ({left})")).color(BG)).fill(ACCENT);
                if ui.add(btn).clicked() {
                    AlreadyRunning::finish(self.other);
                }
            });
        });
    }
}

fn style(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());
    ctx.all_styles_mut(|s| {
        s.visuals.panel_fill = BG;
        s.visuals.window_fill = PANEL;
        s.visuals.extreme_bg_color = Color32::from_rgb(36, 36, 42);
        s.visuals.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        s.visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
        s.visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
        s.visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
        s.visuals.widgets.active.corner_radius = CornerRadius::same(6);
        s.visuals.slider_trailing_fill = true;
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(10.0, 5.0);
    });
}

impl egui_software_backend::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _backend: &mut egui_software_backend::SoftwareBackend) {
        if !self.tray_ready {
            self.tray_ready = crate::tray::install();
            if self.tray_ready && std::mem::take(&mut self.start_hidden) {
                crate::tray::hide_own_window();
            }
        }
        let state = self.state.clone();
        self.frame += 1;
        let fresh = std::mem::take(&mut state.lock().unwrap().new_thumbs);
        self.store_thumbs(ui.ctx(), fresh);
        let s = state.lock().unwrap();

        match s.auth {
            AuthState::Ready => {}
            _ => {
                self.sign_in_screen(ui, &s);
                return;
            }
        }

        self.sync_art(ui.ctx(), &s);
        // Offer the playback setup once, and only if it was never done (a saved setup
        // also reads as "not set up" for a moment while the player starts).
        if s.audio == Audio::NotSetUp && !self.setup_offered {
            self.setup_offered = true;
            if soloist::Config::load().is_none() {
                self.open_setup();
            }
        }

        egui::Panel::top("top")
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(12, 10)))
            .show_inside(ui, |ui| self.top_bar(ui, &s));
        egui::Panel::bottom("player")
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::symmetric(12, 10)))
            .exact_size(84.0)
            .show_inside(ui, |ui| self.player_bar(ui, &s));
        egui::Panel::left("library")
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::symmetric(8, 6)))
            .resizable(true)
            .default_size(210.0)
            .size_range(150.0..=360.0)
            .show_inside(ui, |ui| self.library(ui, &s));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(PANEL).inner_margin(Margin::same(12)).corner_radius(8))
            .show_inside(ui, |ui| self.track_list(ui, &s));
        self.setup_window(ui.ctx());
        self.prefs_window(ui.ctx(), &s);
        self.about_window(ui.ctx());
        if s.audio == Audio::Pairing && !self.pairing_hidden {
            self.pairing_card(ui.ctx());
        }
    }
}

impl App {
    fn sign_in_screen(&mut self, ui: &mut egui::Ui, s: &State) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG)).show_inside(ui, |ui| {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.label(RichText::new("Spoty").size(34.0).strong().color(ACCENT));
                ui.label(RichText::new("A tiny Spotify player").color(MUTED));
                ui.add_space(24.0);
                match s.auth {
                    AuthState::Starting => {
                        ui.spinner();
                        ui.label(RichText::new("Connecting…").color(MUTED));
                    }
                    AuthState::Waiting => {
                        ui.spinner();
                        ui.label("Finish signing in in your browser…");
                    }
                    _ => {
                        let btn = egui::Button::new(RichText::new("Sign in with Spotify").size(16.0).color(BG))
                            .fill(ACCENT)
                            .corner_radius(20)
                            .min_size(Vec2::new(200.0, 40.0));
                        if ui.add(btn).clicked() {
                            self.send(Cmd::Login);
                        }
                        ui.add_space(8.0);
                        ui.label(RichText::new("Requires Spotify Premium").small().color(MUTED));
                    }
                }
                if let Some(e) = &s.error {
                    ui.add_space(16.0);
                    ui.label(RichText::new(e).color(Color32::from_rgb(240, 110, 110)));
                }
            });
        });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, s: &State) {
        ui.horizontal(|ui| {
            ui.scope(|ui| self.menus(ui, s));
            ui.add_space(8.0);
            let edit = egui::TextEdit::singleline(&mut self.query)
                .hint_text("Search songs")
                .desired_width(320.0)
                .margin(Margin::symmetric(10, 6));
            let r = ui.add(edit);
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !self.query.trim().is_empty() {
                self.send(Cmd::Search(self.query.trim().to_string()));
            }
            if ui.input(|i| i.modifiers.command && i.key_pressed(Key::F)) {
                r.request_focus();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Sign out").clicked() {
                    self.send(Cmd::Logout);
                }
                if let Some(e) = &s.error {
                    ui.label(RichText::new(e).color(Color32::from_rgb(240, 110, 110)).small())
                        .on_hover_text(e);
                }
            });
        });
    }

    fn library(&mut self, ui: &mut egui::Ui, s: &State) {
        ui.add_space(4.0);
        if nav_item(ui, "♥  Liked Songs", false, s.view == View::Liked).clicked() {
            self.send(Cmd::OpenLiked);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (tab, label) in [(LibTab::Playlists, "Playlists"), (LibTab::Albums, "Albums"), (LibTab::Artists, "Artists")] {
                if ui.add(egui::Button::selectable(self.lib_tab == tab, label).corner_radius(12)).clicked() && self.lib_tab != tab {
                    self.lib_tab = tab;
                    self.lib_filter.clear();
                }
            }
        });
        ui.add_space(4.0);

        // Albums and artists load the first time their tab is opened.
        match self.lib_tab {
            LibTab::Albums if s.albums.is_none() => self.send(Cmd::LoadAlbums),
            LibTab::Artists if s.artists.is_none() => self.send(Cmd::LoadArtists),
            _ => {}
        }
        ui.horizontal(|ui| {
            let hint = match self.lib_tab {
                LibTab::Playlists => "Filter playlists",
                LibTab::Albums => "Filter albums or artists",
                LibTab::Artists => "Filter artists",
            };
            let clear_w = if self.lib_filter.is_empty() { 0.0 } else { 26.0 };
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.lib_filter)
                    .hint_text(hint)
                    .desired_width(ui.available_width() - clear_w)
                    .margin(Margin::symmetric(8, 4)),
            );
            if r.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                self.lib_filter.clear();
            }
            if !self.lib_filter.is_empty() && ui.small_button("✕").on_hover_text("Clear filter").clicked() {
                self.lib_filter.clear();
            }
        });
        if self.lib_tab == LibTab::Playlists {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (owner, label) in [(Owner::All, "All"), (Owner::Mine, "By you"), (Owner::Others, "By others")] {
                    let chip = egui::Button::selectable(self.owner == owner, RichText::new(label).small()).corner_radius(10);
                    if ui.add(chip).clicked() {
                        self.owner = owner;
                    }
                }
            });
        }
        ui.add_space(4.0);
        let query = self.lib_filter.trim().to_lowercase();
        let matches = |text: &str| text.to_lowercase().contains(&query);

        let mut missing = Vec::new();
        let mut toggle_pin: Option<String> = None;
        match self.lib_tab {
            LibTab::Playlists => {
                let mut want_thumbs = Vec::new();
                let mut order = pinned_first(s.playlists.iter().map(|p| p.uri.as_str()), &self.pins);
                match self.owner {
                    Owner::All => {}
                    Owner::Mine => order.retain(|&i| s.playlists[i].mine),
                    Owner::Others => order.retain(|&i| !s.playlists[i].mine),
                }
                if !query.is_empty() {
                    order.retain(|&i| matches(&s.playlists[i].name));
                }
                no_matches(ui, order.is_empty() && !s.playlists.is_empty());
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, NAV2_H, order.len(), |ui, rows| {
                    for &i in &order[rows] {
                        let p = &s.playlists[i];
                        let pinned = self.pins.contains(&p.uri);
                        let thumb = self.thumb(&p.uri, p.thumb_url.as_deref(), &mut want_thumbs);
                        let songs = match p.songs {
                            1 => "1 song".to_string(),
                            n => format!("{n} songs"),
                        };
                        let sub = if pinned { format!("Pinned · {songs}") } else { songs };
                        let r = nav_item2(ui, thumb, &p.name, &sub, pinned, s.view == View::Playlist(p.uri.clone()));
                        if r.clicked() {
                            self.send(Cmd::OpenPlaylist(p.clone()));
                        }
                        pin_menu(&r, &p.uri, pinned, &mut toggle_pin);
                    }
                });
                if !want_thumbs.is_empty() {
                    self.send(Cmd::NeedThumbs(want_thumbs));
                }
            }
            LibTab::Albums => {
                let albums = s.albums.as_deref().unwrap_or_default();
                if albums.is_empty() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Loading albums…").small().color(MUTED));
                    });
                }
                let mut want_thumbs = Vec::new();
                let mut order = pinned_first(albums.iter().map(String::as_str), &self.pins);
                if !query.is_empty() {
                    // Filtering needs every album's name, not just the ones scrolled past.
                    missing.extend(albums.iter().filter(|u| !s.names.contains_key(*u)).cloned());
                    order.retain(|&i| s.names.get(&albums[i]).is_some_and(|n| matches(&n.name) || matches(&n.subtitle)));
                    no_matches(ui, order.is_empty() && !albums.is_empty());
                }
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, NAV2_H, order.len(), |ui, rows| {
                    for &i in &order[rows] {
                        let uri = &albums[i];
                        let pinned = self.pins.contains(uri);
                        let named = s.names.get(uri);
                        if named.is_none() {
                            missing.push(uri.clone());
                        }
                        let thumb = self.thumb(uri, named.and_then(|n| n.thumb_url.as_deref()), &mut want_thumbs);
                        let (title, artist) = named.map_or(("…", ""), |n| (n.name.as_str(), n.subtitle.as_str()));
                        let sub = if pinned { format!("Pinned · {artist}") } else { artist.to_string() };
                        let r = nav_item2(ui, thumb, title, &sub, pinned, s.view == View::Album(uri.clone()));
                        if r.clicked() {
                            self.send(Cmd::OpenAlbum(uri.clone()));
                        }
                        pin_menu(&r, uri, pinned, &mut toggle_pin);
                    }
                });
                if !want_thumbs.is_empty() {
                    self.send(Cmd::NeedThumbs(want_thumbs));
                }
            }
            LibTab::Artists => {
                let artists = s.artists.as_deref().unwrap_or_default();
                if artists.is_empty() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Loading artists…").small().color(MUTED));
                    });
                }
                let mut order = pinned_first(artists.iter().map(String::as_str), &self.pins);
                if !query.is_empty() {
                    missing.extend(artists.iter().filter(|u| !s.names.contains_key(*u)).cloned());
                    order.retain(|&i| s.names.get(&artists[i]).is_some_and(|n| matches(&n.name)));
                    no_matches(ui, order.is_empty() && !artists.is_empty());
                }
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, NAV_H, order.len(), |ui, rows| {
                    for &i in &order[rows] {
                        let uri = &artists[i];
                        let pinned = self.pins.contains(uri);
                        let named = s.names.get(uri);
                        if named.is_none() {
                            missing.push(uri.clone());
                        }
                        let title = named.map_or("…", |n| n.name.as_str());
                        let r = nav_item(ui, title, pinned, s.view == View::Artist(uri.clone()));
                        if r.clicked() {
                            self.send(Cmd::OpenArtist(uri.clone()));
                        }
                        pin_menu(&r, uri, pinned, &mut toggle_pin);
                    }
                });
            }
        }
        if let Some(uri) = toggle_pin {
            self.toggle_pin(uri);
        }
        if !missing.is_empty() {
            self.send(Cmd::NeedNames(missing));
        }
    }

    fn track_list(&mut self, ui: &mut egui::Ui, s: &State) {
        if s.view == View::Empty {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Search for a song, or pick something from your library on the left.").color(MUTED));
            });
            return;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(&s.title).size(20.0).strong());
            if s.loading {
                ui.spinner();
            } else {
                ui.label(RichText::new(format!("{} songs", s.list.len())).color(MUTED));
            }
        });
        ui.add_space(6.0);

        if !s.loading && s.list.is_empty() {
            ui.label(RichText::new("Nothing here.").color(MUTED));
            return;
        }
        let playing_uri = s.current_uri();
        let mut clicked = None;
        let mut missing = Vec::new();
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, ROW_H, s.list.len(), |ui, rows| {
            // Ask for details a screen ahead so scrolling stays smooth.
            let ahead = rows.end..(rows.end + rows.len()).min(s.list.len());
            for i in rows.clone().chain(ahead) {
                if !s.details.contains_key(&s.list[i]) {
                    missing.push(s.list[i].clone());
                }
            }
            for i in rows {
                let uri = &s.list[i];
                if track_row(ui, i, s.details.get(uri), playing_uri == Some(uri.as_str())) {
                    clicked = Some(i);
                }
            }
        });
        if !missing.is_empty() {
            self.send(Cmd::NeedDetails(missing));
        }
        if let Some(i) = clicked {
            self.send(Cmd::Play(s.list.clone(), i));
        }
    }

    fn player_bar(&mut self, ui: &mut egui::Ui, s: &State) {
        let track = s.current();
        ui.horizontal_centered(|ui| {
            // Cover art and title.
            let info_w = (ui.available_width() * 0.3).clamp(160.0, 320.0);
            ui.allocate_ui_with_layout(Vec2::new(info_w, 64.0), Layout::left_to_right(Align::Center), |ui| {
                let size = Vec2::splat(56.0);
                match &self.art {
                    Some((_, tex)) if track.is_some() => {
                        ui.add(egui::Image::new((tex.id(), size)).corner_radius(4));
                    }
                    _ => {
                        let (r, _) = ui.allocate_exact_size(size, Sense::hover());
                        ui.painter().rect_filled(r, 4, Color32::from_rgb(44, 44, 50));
                    }
                }
                ui.vertical(|ui| {
                    ui.add_space(10.0);
                    if let Some(t) = track {
                        ui.add(egui::Label::new(RichText::new(&t.name).strong()).truncate());
                        ui.add(egui::Label::new(RichText::new(&t.artists).color(MUTED).small()).truncate());
                    }
                });
            });

            // Volume on the far right, transport in the middle.
            let vol_w = 130.0;
            let mid_w = (ui.available_width() - vol_w - 16.0).max(120.0);
            ui.allocate_ui_with_layout(Vec2::new(mid_w, 64.0), Layout::top_down(Align::Center), |ui| {
                ui.horizontal(|ui| {
                    let btns = 3.0 * 36.0 + 2.0 * ui.spacing().item_spacing.x;
                    ui.add_space(((ui.available_width() - btns) / 2.0).max(0.0));
                    if icon_button(ui, "⏮", 36.0, false).clicked() {
                        self.send(Cmd::Prev);
                    }
                    if icon_button(ui, if s.playing { "⏸" } else { "▶" }, 36.0, true).clicked() {
                        self.send(Cmd::Toggle);
                    }
                    if icon_button(ui, "⏭", 36.0, false).clicked() {
                        self.send(Cmd::Next);
                    }
                });
                if s.audio != Audio::Ready {
                    self.audio_status(ui, &s.audio);
                    return;
                }
                let dur = track.map_or(0, |t| t.duration_ms);
                let pos = self.seeking.unwrap_or_else(|| s.position_ms());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(clock(pos)).small().color(MUTED));
                    let w = ui.available_width() - 44.0;
                    ui.spacing_mut().slider_width = w.max(40.0);
                    let mut v = pos;
                    let r = ui.add_enabled(dur > 0, egui::Slider::new(&mut v, 0..=dur.max(1)).show_value(false));
                    if r.dragged() || r.changed() {
                        self.seeking = Some(v);
                    }
                    if (r.drag_stopped() || (r.changed() && !r.dragged())) && self.seeking.is_some() {
                        self.send(Cmd::Seek(v));
                        self.seeking = None;
                    }
                    ui.label(RichText::new(clock(dur)).small().color(MUTED));
                });
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().slider_width = 90.0;
                let mut vol = s.volume;
                if ui.add(egui::Slider::new(&mut vol, 0..=100).show_value(false)).changed() {
                    self.send(Cmd::Volume(vol));
                }
                ui.label(RichText::new("🔊").color(MUTED));
            });
        });

        // Space toggles playback when no text field has focus.
        if ui.input(|i| i.key_pressed(Key::Space)) && !ui.ctx().egui_wants_keyboard_input() {
            self.send(Cmd::Toggle);
        }
    }

    fn menus(&mut self, ui: &mut egui::Ui, s: &State) {
        // Flat, navbar-style buttons: no box until hovered.
        let w = &mut ui.style_mut().visuals.widgets;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        ui.menu_button("File", |ui| {
            if ui.button("Preferences…").clicked() {
                self.prefs = Some(Prefs::load());
                ui.close();
            }
            if ui.button("Sign out").clicked() {
                self.send(Cmd::Logout);
                ui.close();
            }
            ui.separator();
            if ui.button("Quit").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
        ui.menu_button("Playback", |ui| {
            let ready = s.audio == Audio::Ready;
            let label = if s.playing { "Pause" } else { "Play" };
            if ui.add_enabled(ready, egui::Button::new(label).shortcut_text("Space")).clicked() {
                self.send(Cmd::Toggle);
                ui.close();
            }
            if ui.add_enabled(ready, egui::Button::new("Next")).clicked() {
                self.send(Cmd::Next);
                ui.close();
            }
            if ui.add_enabled(ready, egui::Button::new("Previous")).clicked() {
                self.send(Cmd::Prev);
                ui.close();
            }
            ui.separator();
            let mut shuffle = s.shuffle;
            if ui.checkbox(&mut shuffle, "Shuffle").changed() {
                self.send(Cmd::SetShuffle(shuffle));
            }
            ui.label(RichText::new("Repeat").small().color(MUTED));
            let mut repeat = s.repeat;
            ui.radio_value(&mut repeat, Repeat::Off, "Off");
            ui.radio_value(&mut repeat, Repeat::All, "All songs");
            ui.radio_value(&mut repeat, Repeat::One, "This song");
            if repeat != s.repeat {
                self.send(Cmd::SetRepeat(repeat));
            }
        });
        ui.menu_button("Help", |ui| {
            if ui.button("About Spoty").clicked() {
                self.about_open = true;
                ui.close();
            }
            if ui.button("Open logs folder").clicked() {
                let _ = std::process::Command::new("explorer").arg(crate::auth::data_dir()).spawn();
                ui.close();
            }
            let managed = soloist::Config::load().is_some_and(|c| c.is_managed());
            if ui
                .add_enabled(managed, egui::Button::new("Re-download Soloist"))
                .on_hover_text("Fetch a fresh copy of the player from Spotify")
                .clicked()
            {
                self.send(Cmd::RedownloadPlayer);
                ui.close();
            }
        });
    }

    fn prefs_window(&mut self, ctx: &egui::Context, s: &State) {
        if self.prefs.is_none() {
            return;
        }
        // Measure the cache now and then while the window is open.
        if self.cache_size.is_none_or(|(at, _)| at.elapsed().as_secs() >= 2) {
            self.cache_size = Some((std::time::Instant::now(), dir_size(&prefs::cache_dir())));
        }
        let used_mb = self.cache_size.map_or(0, |(_, b)| b / 1_000_000);
        let mut open = true;
        let (mut save, mut clear, mut cancel) = (false, false, false);
        let p = self.prefs.as_mut().unwrap();
        egui::Window::new("Preferences")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                section(ui, "Window");
                ui.checkbox(&mut p.close_to_tray, "Close button keeps Spoty running in the tray");
                ui.checkbox(&mut p.start_with_windows, "Start Spoty when Windows starts");
                ui.add_enabled(p.start_with_windows, egui::Checkbox::new(&mut p.start_minimized, "Start in the tray"));

                section(ui, "Player");
                egui::Grid::new("player-prefs").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Name in Spotify");
                    ui.add(egui::TextEdit::singleline(&mut p.device_name).desired_width(200.0));
                    ui.end_row();
                    ui.label("Starting volume");
                    ui.add(egui::Slider::new(&mut p.default_volume, 0..=100).suffix("%"));
                    ui.end_row();
                    ui.label("Audio cache limit");
                    ui.add(egui::DragValue::new(&mut p.cache_mb).range(100..=20_000).speed(10).suffix(" MB"));
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Cache in use: {used_mb} MB")).small().color(MUTED));
                    let can_clear = matches!(s.audio, Audio::Ready | Audio::Pairing | Audio::Failed(_));
                    if ui.add_enabled(can_clear, egui::Button::new("Clear cache")).clicked() {
                        clear = true;
                    }
                });
                ui.label(RichText::new("Changing player settings restarts the player for a moment.").small().color(MUTED));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new("Save").color(BG)).fill(ACCENT)).clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if clear {
            self.send(Cmd::ClearCache);
            self.cache_size = None;
        }
        if save {
            let p = self.prefs.take().unwrap();
            crate::tray::CLOSE_TO_TRAY.store(p.close_to_tray, std::sync::atomic::Ordering::SeqCst);
            self.send(Cmd::SavePrefs(p));
        } else if !open || cancel {
            self.prefs = None;
        }
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        egui::Window::new("About Spoty")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Spoty").size(24.0).strong().color(ACCENT));
                    ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(MUTED));
                    ui.add_space(8.0);
                    ui.label("A tiny Spotify player for Windows.");
                    ui.add_space(4.0);
                    ui.label("Made by Ladro");
                    if ui.link("github.com/ladroxd").clicked() {
                        crate::auth::open_url("https://github.com/ladroxd");
                    }
                    ui.add_space(4.0);
                    ui.label(RichText::new("Audio by Spotify Soloist. Spotify Premium required.").small().color(MUTED));
                    ui.label(RichText::new("Spoty isn't made or endorsed by Spotify.").small().color(MUTED));
                });
            });
        self.about_open = open;
    }

    fn audio_status(&mut self, ui: &mut egui::Ui, audio: &Audio) {
        ui.horizontal(|ui| {
            match audio {
                Audio::NotSetUp => {
                    ui.label(RichText::new("Playback isn't set up yet.").color(MUTED));
                    if ui.button("Set up playback").clicked() {
                        self.open_setup();
                    }
                }
                Audio::Downloading => {
                    ui.spinner();
                    ui.label(RichText::new("Downloading the player from Spotify…").color(MUTED));
                }
                Audio::Starting => {
                    ui.spinner();
                    ui.label(RichText::new("Starting the player…").color(MUTED));
                }
                Audio::Pairing => {
                    ui.spinner();
                    ui.label(RichText::new("One-time step: in the Spotify app (phone or PC), open the device list and pick “Spoty”.").color(MUTED))
                        .on_hover_text("This links the player to your account. You only do it once.");
                }
                Audio::Failed(msg) => {
                    ui.label(RichText::new(msg).color(Color32::from_rgb(240, 110, 110)));
                    if ui.button("Fix setup").clicked() {
                        self.open_setup();
                    }
                }
                Audio::Ready => {}
            }
        });
    }

    /// Soloist only links to an account when it's picked in a Spotify app's device list.
    fn pairing_card(&mut self, ctx: &egui::Context) {
        egui::Window::new("Link the player to your account")
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Waiting for Spotify…").color(MUTED));
                });
                ui.add_space(8.0);
                let step = |ui: &mut egui::Ui, n: &str, text: &str| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(n).strong().color(ACCENT));
                        ui.label(text);
                    });
                };
                step(ui, "1", "Open Spotify on your phone or another computer on the same Wi-Fi.");
                step(ui, "2", "Tap the devices icon (the speaker next to the play controls).");
                step(ui, "3", "Choose “Spoty”.");
                ui.add_space(8.0);
                ui.label(
                    RichText::new("This happens once per PC. Spotify only lets its own app link a new player, so Spoty can't skip this step.")
                        .small()
                        .color(MUTED),
                );
                ui.add_space(6.0);
                if ui.button("Later").clicked() {
                    self.pairing_hidden = true;
                }
            });
    }

    fn open_setup(&mut self) {
        self.setup = Some(soloist::Config::load().unwrap_or_default());
    }

    fn setup_window(&mut self, ctx: &egui::Context) {
        let Some(cfg) = self.setup.as_mut() else { return };
        let mut open = true;
        let mut save = false;
        egui::Window::new("Set up playback")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Spoty plays audio through Spotify Soloist, Spotify's own lightweight player. Spoty downloads it from Spotify and keeps it up to date; you just need a free API key, once:");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("1. Open the Spotify developer dashboard, accept the Soloist terms and create an API key (needs Premium).");
                });
                if ui.link("Open the developer dashboard").clicked() {
                    crate::auth::open_url(soloist::DASHBOARD);
                }
                ui.add_space(4.0);
                ui.label("2. Paste the key here:");
                ui.horizontal(|ui| {
                    if ui.button("Paste").on_hover_text("Paste the key from the clipboard").clicked()
                        && let Ok(text) = arboard::Clipboard::new().and_then(|mut c| c.get_text())
                    {
                        cfg.api_key = text.trim().to_string();
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut cfg.api_key)
                            .hint_text("Soloist API key (Ctrl+V works too)")
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.add_space(6.0);
                egui::CollapsingHeader::new(RichText::new("Advanced").small().color(MUTED)).show(ui, |ui| {
                    ui.label(RichText::new("Use your own soloist.exe instead of the one Spoty downloads:").small().color(MUTED));
                    ui.add(egui::TextEdit::singleline(&mut cfg.exe).hint_text(r"C:\Tools\soloist\soloist.exe (or its folder)").desired_width(f32::INFINITY));
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let ready = !cfg.api_key.trim().is_empty();
                    if ui.add_enabled(ready, egui::Button::new(RichText::new("Save and start").color(BG)).fill(ACCENT)).clicked() {
                        save = true;
                    }
                    ui.label(RichText::new(r"Saved in %APPDATA%\Spoty").small().color(MUTED));
                });
            });
        if save {
            let cfg = self.setup.take().unwrap();
            self.send(Cmd::SetupAudio(cfg));
        } else if !open {
            self.setup = None;
        }
    }

    /// The thumbnail for `key` if it's loaded; otherwise queues it (once) for fetching.
    fn thumb(&mut self, key: &str, url: Option<&str>, want: &mut Vec<(String, String)>) -> Option<egui::TextureId> {
        if let Some((tex, used)) = self.thumbs.get_mut(key) {
            *used = self.frame;
            return Some(tex.id());
        }
        if let Some(url) = url
            && self.thumbs_pending.insert(key.to_string())
        {
            want.push((key.to_string(), url.to_string()));
        }
        None
    }

    fn toggle_pin(&mut self, uri: String) {
        match self.pins.iter().position(|p| *p == uri) {
            Some(i) => {
                self.pins.remove(i);
            }
            None => self.pins.insert(0, uri),
        }
        let _ = std::fs::create_dir_all(crate::auth::data_dir());
        let _ = std::fs::write(pins_file(), serde_json::to_string(&self.pins).unwrap_or_default());
    }

    fn store_thumbs(&mut self, ctx: &egui::Context, fresh: Vec<(String, egui::ColorImage)>) {
        for (uri, img) in fresh {
            self.thumbs_pending.remove(&uri);
            let tex = ctx.load_texture(format!("thumb:{uri}"), img, egui::TextureOptions::LINEAR);
            self.thumbs.insert(uri, (tex, self.frame));
        }
        if self.thumbs.len() > MAX_THUMBS {
            let mut by_age: Vec<(u64, String)> = self.thumbs.iter().map(|(u, (_, used))| (*used, u.clone())).collect();
            by_age.sort_unstable();
            for (_, uri) in by_age.into_iter().take(self.thumbs.len() - MAX_THUMBS) {
                self.thumbs.remove(&uri); // dropping the handle frees the texture
            }
        }
    }

    fn sync_art(&mut self, ctx: &egui::Context, s: &State) {
        match &s.art {
            Some((url, img)) if self.art.as_ref().is_none_or(|(u, _)| u != url) => {
                let tex = ctx.load_texture("art", img.clone(), egui::TextureOptions::LINEAR);
                self.art = Some((url.clone(), tex));
            }
            None => self.art = None,
            _ => {}
        }
    }
}

fn pins_file() -> std::path::PathBuf {
    crate::auth::data_dir().join("pins.json")
}

fn load_pins() -> Vec<String> {
    std::fs::read_to_string(pins_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Row order for a sidebar list: pinned items (in pin order) first, then the rest as they come.
fn pinned_first<'a>(uris: impl Iterator<Item = &'a str>, pins: &[String]) -> Vec<usize> {
    let uris: Vec<&str> = uris.collect();
    let mut order: Vec<usize> = pins.iter().filter_map(|p| uris.iter().position(|u| u == p)).collect();
    order.extend((0..uris.len()).filter(|&i| !pins.iter().any(|p| p == uris[i])));
    order
}

fn no_matches(ui: &mut egui::Ui, empty: bool) {
    if empty {
        ui.label(RichText::new("No matches").small().color(MUTED));
    }
}

/// Right-click menu on a sidebar row.
fn pin_menu(r: &egui::Response, uri: &str, pinned: bool, toggle: &mut Option<String>) {
    r.context_menu(|ui| {
        if ui.button(if pinned { "Unpin" } else { "Pin to top" }).clicked() {
            *toggle = Some(uri.to_string());
            ui.close();
        }
    });
}

/// Small accent dot marking a pinned row.
fn pin_dot(ui: &egui::Ui, rect: egui::Rect) {
    ui.painter().circle_filled(egui::pos2(rect.right() - 9.0, rect.center().y), 3.0, ACCENT);
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong());
    ui.add_space(2.0);
}

fn dir_size(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// A two-line sidebar row with a small cover: title and a muted subtitle (an album and its artist).
fn nav_item2(
    ui: &mut egui::Ui,
    thumb: Option<egui::TextureId>,
    title: &str,
    subtitle: &str,
    pinned: bool,
    selected: bool,
) -> egui::Response {
    let w = ui.available_width();
    let (rect, r) = ui.allocate_exact_size(Vec2::new(w, NAV2_H), Sense::click());
    if selected || r.hovered() {
        let fill = if selected { Color32::from_rgb(40, 40, 46) } else { Color32::from_rgb(32, 32, 37) };
        ui.painter().rect_filled(rect, 6, fill);
    }
    let cover = egui::Rect::from_min_size(rect.left_top() + Vec2::new(4.0, 4.0), Vec2::splat(32.0));
    match thumb {
        Some(id) => {
            egui::Image::new((id, cover.size())).corner_radius(4).paint_at(ui, cover);
        }
        None => {
            ui.painter().rect_filled(cover, 4, Color32::from_rgb(44, 44, 50));
            ui.painter().text(cover.center(), Align2::CENTER_CENTER, "♪", FontId::proportional(15.0), MUTED);
        }
    }
    let x = 44.0;
    let color = if selected { ACCENT } else { ui.visuals().text_color() };
    let text_w = w - x - if pinned { 20.0 } else { 6.0 };
    if pinned {
        pin_dot(ui, rect);
    }
    let top = elide(ui, title, FontId::proportional(14.0), color, text_w);
    ui.painter().galley(rect.left_top() + Vec2::new(x, 4.0), top, color);
    let sub = elide(ui, subtitle, FontId::proportional(12.0), MUTED, text_w);
    ui.painter().galley(rect.left_top() + Vec2::new(x, 22.0), sub, MUTED);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn nav_item(ui: &mut egui::Ui, text: &str, pinned: bool, selected: bool) -> egui::Response {
    let w = ui.available_width();
    let (rect, r) = ui.allocate_exact_size(Vec2::new(w, 28.0), Sense::click());
    if selected || r.hovered() {
        let fill = if selected { Color32::from_rgb(40, 40, 46) } else { Color32::from_rgb(32, 32, 37) };
        ui.painter().rect_filled(rect, 6, fill);
    }
    let color = if selected { ACCENT } else { ui.visuals().text_color() };
    if pinned {
        pin_dot(ui, rect);
    }
    let galley = elide(ui, text, FontId::proportional(14.0), color, w - if pinned { 28.0 } else { 16.0 });
    ui.painter().galley(rect.left_center() + Vec2::new(8.0, -galley.size().y / 2.0), galley, color);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Draws one track row. Returns true when the user asked to play it
/// (clicking the play button, or double-clicking the row).
fn track_row(ui: &mut egui::Ui, i: usize, t: Option<&Track>, is_current: bool) -> bool {
    let w = ui.available_width();
    let (rect, r) = ui.allocate_exact_size(Vec2::new(w, ROW_H), Sense::click());
    // The play button sits just left of the title; added after the row so it takes the click.
    let btn_rect = egui::Rect::from_center_size(egui::pos2(rect.left() + 48.0, rect.center().y), Vec2::splat(26.0));
    let btn = ui.interact(btn_rect, r.id.with("play"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    let play = btn.clicked() || r.double_clicked();
    if r.hovered() || btn.hovered() {
        ui.painter().rect_filled(rect, 6, Color32::from_rgb(38, 38, 44));
    }
    let p = ui.painter();
    let (fill, fg) = if btn.hovered() {
        (ACCENT, BG)
    } else if r.hovered() || is_current {
        (Color32::from_rgb(56, 56, 64), if is_current { ACCENT } else { Color32::WHITE })
    } else {
        (Color32::TRANSPARENT, MUTED)
    };
    p.circle_filled(btn_rect.center(), 12.0, fill);
    p.text(btn_rect.center() + Vec2::new(1.0, 0.0), Align2::CENTER_CENTER, "▶", FontId::proportional(11.0), fg);
    let text = if is_current { ACCENT } else { ui.visuals().strong_text_color() };
    let x = rect.left();
    p.text(
        egui::pos2(x + 24.0, rect.center().y),
        Align2::RIGHT_CENTER,
        (i + 1).to_string(),
        FontId::proportional(12.0),
        MUTED,
    );
    let Some(t) = t else {
        // Details still loading: draw placeholder bars.
        let bar = |y: f32, w: f32| egui::Rect::from_min_size(egui::pos2(x + 70.0, y), Vec2::new(w, 9.0));
        p.rect_filled(bar(rect.top() + 8.0, (w * 0.3).min(220.0)), 3, Color32::from_rgb(40, 40, 46));
        p.rect_filled(bar(rect.top() + 24.0, (w * 0.18).min(140.0)), 3, Color32::from_rgb(36, 36, 41));
        return play;
    };
    p.text(egui::pos2(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, clock(t.duration_ms), FontId::proportional(12.0), MUTED);

    let album_w = if w > 560.0 { (w * 0.3).min(260.0) } else { 0.0 };
    let title_w = w - 70.0 - album_w - 70.0;
    let top = rect.top() + 4.0;
    let name = elide(ui, &t.name, FontId::proportional(14.0), text, title_w);
    p.galley(egui::pos2(x + 70.0, top), name, text);
    let artists = elide(ui, &t.artists, FontId::proportional(12.0), MUTED, title_w);
    p.galley(egui::pos2(x + 70.0, top + 18.0), artists, MUTED);
    if album_w > 0.0 {
        let album = elide(ui, &t.album, FontId::proportional(12.0), MUTED, album_w - 12.0);
        p.galley(egui::pos2(rect.right() - 70.0 - album_w, rect.center().y - 7.0), album, MUTED);
    }
    play
}

fn elide(ui: &egui::Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(10.0));
    ui.fonts_mut(|f| f.layout_job(job))
}

fn icon_button(ui: &mut egui::Ui, icon: &str, size: f32, primary: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let (fill, fg) = if primary {
        (if r.hovered() { Color32::WHITE } else { Color32::from_rgb(230, 230, 230) }, BG)
    } else {
        (if r.hovered() { Color32::from_rgb(44, 44, 50) } else { Color32::TRANSPARENT }, Color32::from_rgb(220, 220, 225))
    };
    ui.painter().circle_filled(rect.center(), size / 2.0, fill);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, FontId::proportional(size * 0.45), fg);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn clock(ms: u32) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}
