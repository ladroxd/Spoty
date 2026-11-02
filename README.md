<p align="center">
  <img src="assets/icon.png" width="96" alt="Spoty icon">
</p>

<h1 align="center">Spoty</h1>

<p align="center">A tiny native Spotify player for Windows: one 6&nbsp;MB exe, about 30&nbsp;MB of RAM.</p>

<p align="center">
  <a href="https://github.com/ladroxd/spoty/releases/latest/download/spoty.exe"><img src="https://img.shields.io/badge/Download_for_Windows-1DB954?style=for-the-badge" alt="Download for Windows"></a>
</p>

---

Spoty is a lightweight alternative to the Spotify desktop app. It's written in Rust and draws its own UI on the CPU, so there's no browser engine and no GPU driver loaded into the process. It sits at about 25 MB of memory in its default window, about 32 MB maximized on a 1080p screen, and uses no CPU when idle. The Soloist player runs as its own process and uses another 45 to 60 MB.

## Download

**[Download spoty.exe](https://github.com/ladroxd/spoty/releases/latest/download/spoty.exe)** for Windows 10 or 11 (64-bit). You'll also need Spotify Premium; see [Requirements](#requirements).

There's nothing to install: run the file and Spoty opens. Windows may show a SmartScreen warning because the app isn't code-signed. If it does, click **More info → Run anyway**.

To update, quit Spoty from the tray and replace the old file with the new one. Your sign-in and settings are kept.

What's new in each version, and checksums, are on the [Releases](https://github.com/ladroxd/spoty/releases) page.

## Features

### Find your music

- Search for songs, albums and artists, each with its own tab. Any album opens without saving it first
- Artist pages with popular songs, albums, singles and EPs, compilations and appearances
- Liked Songs, playlists, saved albums and followed artists in the sidebar
- Click an artist or album on a song row, or the artist or song title in the player bar, to open its page
- A back button returns through the pages you've opened; Alt+Left and the mouse's back button work too
- Filter the sidebar by name, and show only playlists **by you** or **by others**
- Right-click a playlist, album or artist to pin it to the top of its tab

### Sort your songs

- Every playlist and Liked Songs has its own sort button: playlist order, recently added, oldest added, title, artist or album
- **Custom order:** drag songs into your own order, or right-click one to move it to the top or bottom. It's saved on your PC, not on Spotify
- Each list remembers its sort, and playback follows the order on screen
- Title, artist and album sorts are for lists of up to 1,000 songs

### Playback

- Play, pause, seek, next, previous and volume, with Space to play or pause
- Shuffle and repeat (all songs or this song) beside the player controls
- Play a song with the ▶ button on its row, or by double-clicking it

### Light on your PC

- Album and playlist covers load only for what's on screen (capped at about 400 KB)
- Almost no memory in use while in the tray, because Spoty stops drawing when it's hidden
- Closes to the system tray and keeps playing, or set the close button to quit
- Optional start with Windows, in the tray if you like
- One window only: launching it twice switches to the open one
- Preferences for the device name, starting volume and audio cache limit, plus a button to clear the cache
- Guided first-time setup; Spoty downloads Soloist for you and keeps it up to date

## Requirements

- Windows 10 or 11 (64-bit)
- Spotify **Premium**
- A free **Spotify Soloist API key** from the [Spotify for Developers dashboard](https://developer.spotify.com/dashboard/soloist)

## Getting started

1. [Download `spoty.exe`](#download) and run it.
2. Sign in with Spotify in the browser window that opens.
3. Paste your Soloist API key when Spoty asks. Spoty downloads Soloist from Spotify and keeps it up to date.
4. Once: in the Spotify app on your phone or another computer, open the device list and pick **Spoty**.

## How it works

- **Audio** is played by [Spotify Soloist](https://developer.spotify.com/documentation/soloist), Spotify's official headless player. Spoty runs it in the background, controls it over its local WebSocket API, and closes it when Spoty quits.
- **Your library and search** come from the same endpoints the official Spotify apps use, over plain HTTPS.
- **The UI** is [egui](https://github.com/emilk/egui) rendered in software, which keeps the GPU driver (often 100+ MB) out of the process.

## What Spoty stores

Everything stays on your PC, in `%APPDATA%\Spoty`:

| File | Contents |
| --- | --- |
| `refresh_token` | Keeps you signed in |
| `soloist.json` | Your Soloist API key |
| `settings.json` | Your preferences |
| `pins.json` | Your pinned items |
| `song-sorts.json`, `orders\` | How each playlist's songs are sorted, and your custom orders |
| `soloist\`, `soloist-data\` | The Soloist player and its sign-in |

Soloist's audio cache is in `%LOCALAPPDATA%\Spoty\cache`, limited to 500 MB by default.

## Building from source

Requires [Rust](https://rustup.rs) and the Visual Studio C++ build tools.

```bash
cargo build --release
```

The exe is written to `target/release/spoty.exe`.

## Disclaimer

Spoty is an independent project. It isn't made, endorsed or supported by Spotify. Spotify and Spotify Soloist are trademarks of Spotify AB.

## Author

Made by [Ladro](https://github.com/ladroxd).

## License

[MIT](LICENSE)
