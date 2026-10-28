# To do

## Linux version

Soloist runs on Linux x86_64, ARM64 and ARM32, and the UI, library and player control are already cross-platform. What's left is the Windows-only code, moved into a `platform` module:

- [ ] Tray icon via StatusNotifier (KDE and most desktops; GNOME needs the AppIndicator extension)
- [ ] Single instance with a lock file and a local socket to bring the open window to the front
- [ ] Start at login with a `~/.config/autostart` entry
- [ ] Stop Soloist with Spoty (parent-death signal instead of a job object)
- [ ] Download and unpack Soloist's `.tar.gz` builds and pick the right one for the CPU
- [ ] Open links and folders with `xdg-open`
- [ ] Store data in `~/.config/spoty` and the cache in `~/.cache/spoty`
- [ ] `.desktop` file and icon
- [ ] Release build in GitHub Actions: `.tar.gz`, plus an AppImage
- [ ] Test on X11 and Wayland (WSL can cover the basics)

## macOS version

Soloist only runs on Apple Silicon Macs with macOS 13 or later, so there will be no Intel build.

- [ ] Menu bar icon instead of a tray icon
- [ ] Single instance and start at login (LaunchAgent)
- [ ] Stop Soloist with Spoty
- [ ] Download and unpack Soloist's macOS build
- [ ] `.app` bundle with the icon
- [ ] Release build in GitHub Actions on an Apple Silicon runner
- [ ] Code signing and notarization (needs an Apple Developer account); until then users right-click → Open on first launch
- [ ] Find a tester with an Apple Silicon Mac

## Releases

- [ ] List Windows, Linux and macOS downloads in the release notes template
- [ ] Code-sign the Windows exe so SmartScreen stops warning

## Smaller things

- [ ] Albums and Artists tabs keep spinning when the list is empty or fails to load; show "Nothing here" or the error instead
- [ ] "Show more" for search results (currently the top 20 songs)
- [ ] Artist pages: albums and singles, not just top tracks
- [ ] Podcasts and episodes
