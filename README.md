# Tempo

Better media keys for Windows 11. Tempo runs silently in the background (no window, no tray icon) and takes over your keyboard's media keys, so they control the app that's **actually playing**, not whichever paused tab Windows happens to pick. The Windows media flyout is replaced by a small pop-up that shows briefly on each key press.

## What the keys do

| Key | Tap | Hold | Double-tap |
| --- | --- | --- | --- |
| Play/Pause | play / pause | switch to the next app | bring the playing app to the front |
| Next / Previous | skip track | scrub forward / back (the app follows live) | |
| Stop | stop | | |

## The pop-up

- Artwork, title, artist and a colour taken from the cover
- Drag the seek bar, ±10 s buttons (5/10/15/30 s), per-app volume (or scroll anywhere on the card)
- One icon per app with media: click to switch
- Click the artwork to open the app, the copy button to copy "Artist – Title"
- Right-click: open app, copy song name, prefer this app, hide pop-ups for an hour, settings

## Which app gets the keys

1. An app you picked (by clicking it or holding Play/Pause), until another app starts playing
2. Otherwise the app that most recently started playing
3. With nothing playing: your preferred app (star it in settings), else the app that played last

Players that don't report to Windows (VLC 3, MPC-HC, PotPlayer, Winamp…) get the keys while they're playing.

## Also

- Recently played list with YouTube / Spotify search and copy buttons
- Stats: key presses, how often Tempo picked a different app than Windows would have
- Checks that its keyboard hook is still alive and reconnects it if Windows drops it
- Updates itself from GitHub releases (verified with SHA-256)

## Install

1. Install the [.NET 10 Desktop Runtime](https://dotnet.microsoft.com/download/dotnet/10.0) (x64) if you don't have it.
2. Download `Tempo.exe` from the [latest release](https://github.com/JustSad42/Tempo/releases/latest) and put it in a folder you can write to, e.g. `%LOCALAPPDATA%\Programs\Tempo`.
3. Run it once. It adds itself to Windows startup and the Start menu. Open **Tempo** from the Start menu any time to get to settings or quit.

Settings, history and stats are stored locally in `%APPDATA%\Tempo`. The only network requests are update checks to GitHub.
