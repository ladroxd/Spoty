# Changelog

All notable changes to Spoty are listed here, newest first. Planned work is in [TODO.md](TODO.md).

## [Unreleased]

### Added

- **Artist pages:** popular songs, albums, singles and EPs, compilations and appearances, each on its own tab.
- **Search for albums and artists:** results now have Songs, Albums and Artists tabs, and any album can be opened without saving it first. Song search returns up to 50 results instead of 20.
- **Clickable names:** click an artist or album on a song row, or an artist or song title in the player bar, to open its page.
- **Back:** a back button returns through the pages you've opened. Alt+Left and the mouse's back button work too.
- **Shuffle and repeat buttons** beside the player controls. Repeat cycles through off, all songs and this song.
- **Sort the songs in a playlist:** each playlist and Liked Songs has a sort button with playlist order, recently or oldest added, title, artist and album. Each list remembers its choice, and playback follows the order on screen. Title, artist and album need one request per song, so they're for lists of up to 1,000 songs.
- **Custom song order:** drag songs into your own order, or right-click one to move it to the top or bottom. It's saved on this PC, not on Spotify. Songs added to the playlist later appear at the end.
- `TODO.md` with the plan for Linux and macOS versions.
- This changelog.

### Changed

- **Much less memory when the window is large:** about 60 MB down to 32 MB maximized on a 1080p screen, and 30 MB down to 25 MB in the default window, by turning off the renderer's cache of window-sized images.
- **Almost no memory in use while in the tray:** Spoty stops drawing while hidden or minimized and hands back the memory only drawing needed.
- Thumbnails are decoded at the size they're drawn, and at most 100 are kept (about 400 KB, down from 1.4 MB).
- The unshuffled queue is only kept while shuffle is on, and idle network connections close sooner.
- Song and album details kept in memory are now limited; whatever isn't on an open list or in the queue is dropped once there are too many.
- Spoty no longer downloads your whole Liked Songs list at startup just to find out who is signed in.

### Fixed

- **Next and Previous:** after Spotify's autoplay moved on from a song, or when resuming the song Spoty opened with, the queue shrank to one song, so Next did nothing and Previous only restarted the song. They now always move between songs, handing over to Spotify when the queue runs out.
- Previous now always goes to the previous song instead of restarting the current one after the first 3 seconds. On the first song of a list it restarts.
- A song Spotify swapped for another release of itself no longer stops the queue from moving on.
- Repeat is sent to the player again when it starts, so a mode chosen before it was ready still applies.
- With repeat on "this song", a song that ends no longer moves on to the next one in the queue.
- Each list now keeps its own scroll position instead of sharing one.

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
