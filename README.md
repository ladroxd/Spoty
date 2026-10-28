<p align="center">
  <img src="assets/icon.png" width="96" alt="Spoty icon">
</p>

<h1 align="center">Spoty</h1>

<p align="center">A tiny native Spotify player for Windows: one 5&nbsp;MB exe, about 30&nbsp;MB of RAM.</p>

---

Spoty is a lightweight alternative to the Spotify desktop app. It's written in Rust and draws its own UI on the CPU, so there's no browser engine and no GPU driver loaded into the process. It sits at about 30 MB of memory and uses no CPU when idle.

## Features

- Search, Liked Songs, playlists, saved albums and followed artists
- Album and playlist covers, loaded only for what's on screen (capped at about 1.5 MB)
- Play, pause, seek, next and previous, shuffle, repeat, volume
- Closes to the system tray; optional start with Windows
- One window only: launching it twice switches to the open one
- Preferences for the device name, starting volume and audio cache limit

## Requirements

- Windows 10 or 11 (64-bit)
- Spotify **Premium**
- A free **Spotify Soloist API key** from the [Spotify for Developers dashboard](https://developer.spotify.com/dashboard/soloist)

## Getting started

1. Download `spoty.exe` and run it. Nothing to install.
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
