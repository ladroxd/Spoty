# Changelog

All notable changes to Spoty are listed here, newest first. Planned work is in [TODO.md](TODO.md).

## [Unreleased]

### Added

- `TODO.md` with the plan for Linux and macOS versions.
- This changelog.

## [0.1.0] - 2026-10-28

First release.

### Added

- **Library:** search, Liked Songs, playlists, saved albums and followed artists, in a sidebar with Playlists, Albums and Artists tabs.
- **Covers:** album art in the player bar, and small covers for albums and playlists in the sidebar. They load only for rows on screen and at most 150 are kept in memory. Playlists using Spotify's 4-album mosaic show a placeholder.
- **Sidebar filter:** narrow the open tab by name. Albums also match on the artist. **By you / By others** chips filter playlists by owner.
- **Pins:** right-click a playlist, album or artist to pin it to the top of its tab. Pins are saved in `%APPDATA%\Spoty\pins.json`.
- **Playback:** play, pause, seek, next and previous, shuffle, repeat (all songs or this song) and volume. Space toggles play and pause. A ▶ button next to each song plays it with one click, and double-clicking a row works too.
- **Menu bar:** File (Preferences, Sign out, Quit), Playback and Help (About, Open logs folder, Re-download Soloist).
- **Preferences:** close button to tray or quit, start with Windows (optionally in the tray), the name shown in Spotify, starting volume, audio cache limit and a Clear cache button.
- **Tray:** closing the window keeps Spoty playing in the tray. Left-click the icon to open it; right-click for Open or Quit.
- **Single instance:** opening Spoty a second time shows a 5-second countdown, then switches to the window that's already open.
- **Audio through Spotify Soloist:** Spoty downloads Spotify's official headless player, runs it hidden, controls it over its local WebSocket API and closes it when Spoty quits. Expired Soloist builds are replaced automatically.
- **Guided setup:** paste a Soloist API key (with a Paste button and Ctrl+V), then a step-by-step card for the one-time link in the Spotify app. A song picked during linking starts once it's done.
- **App icon** on the exe, window, taskbar and tray.
- **About window** with the version and a link to [Ladro's GitHub](https://github.com/ladroxd).
- **Logs:** start Spoty with `SPOTY_LOG=debug` to write `%APPDATA%\Spoty\spoty.log`; Soloist's output goes to `soloist.log`.
- **Releases:** GitHub Actions builds `spoty.exe` and a SHA-256 checksum for each version tag, with notes from `.github/release-notes/`.
- README and MIT license.

### Changed during development

These shaped 0.1.0 but were never released separately:

- **CPU rendering instead of OpenGL.** With OpenGL, the graphics driver alone added over 100 MB of memory. Drawing on the CPU brought Spoty from 117 MB to about 30 MB, with no CPU used while idle.
- **Library from Spotify's internal endpoints.** The public Web API is rate-limited for every request made with this client ID, so search, playlists, albums, artists and track details come from the same endpoints the official apps use.
- **librespot removed.** Spotify refuses librespot's audio keys for many accounts ([librespot#1649](https://github.com/librespot-org/librespot/issues/1649)), so playback moved to Spotify Soloist, then librespot was dropped completely. Sign-in became a small built-in OAuth flow, and the exe shrank from 7.2 MB to about 5 MB.
- **Static C runtime**, so the exe runs without the Visual C++ redistributable.

### Fixed during development

- Pasting into text fields: clipboard support was off in the UI backend.
- Small playlist covers didn't load because Spotify serves them as WebP.
- The setup dialog popped up on every launch even when playback was already set up.
- Two copies of Spoty could start two Soloist players and break playback; now only one can run.
- The Task Manager icon was missing in builds made before the icon was embedded.

[Unreleased]: https://github.com/ladroxd/spoty/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ladroxd/spoty/releases/tag/v0.1.0
