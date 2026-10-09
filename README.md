# DayZ Server Browser

A DayZ server browser and launcher for Linux and Windows, written in Rust. It works alongside the
[DayZ map overlay](https://github.com/BrokeAsh/dayz-map-overlay): while the browser is open, the
overlay runs in the background, ready for the game.

- Lists every server DZSA Launcher knows (about 10,000), with search and filters (maps, empty,
  full, locked, first or third person, vanilla or modded, players, server size, ping, game
  version, favorites).
- Pings the servers on screen and shows live players, queue, and in-game time.
- Warns about servers on a different version of DayZ than yours, which the game can't join.
- Asks the selected server for its mod list and shows which mods you have, which need an update,
  and which are missing.
- Downloads missing mods through your Steam client, or opens their Workshop pages.
- Launches DayZ connected to the server with its mods, and stays open afterwards. For a full
  server, it can wait and join as soon as a slot opens.
- Connects by address to servers the list doesn't have.
- Optional: use Steam's own server list with your Steam Web API key instead of DZSA's.

Status: early. Tested on Linux with a real DayZ install, including mod downloads and joining;
Windows is only tested in a VM so far.

## How it works

### Server list

By default the list comes from DZSA Launcher's public API, which most third-party DayZ launchers
use. It's cached for 5 minutes and refreshed when you ask (at most every 30 seconds). Servers DZSA
marks as sponsors get no special place: the browser sorts the list itself.

With your own [Steam Web API key](https://steamcommunity.com/dev/apikey) (Settings), the list comes
from Steam instead, in about ten requests (Steam returns at most about 10,000 servers per request).
The key is kept in your system keyring (KWallet or GNOME Keyring, or Windows Credential Manager),
never in the settings file or logs. Don't share it: it's tied to your Steam account.

### Connecting by address

**Connect by address** takes an IP or host name with the port from the server's ad (its game
port). Servers answer queries on a separate port, most often the game port + 1 or 27016, so the
browser asks the likely ones and keeps the server that says it plays on that port. Servers found
this way stay in the list (until removed) and are looked up again each time the browser starts.

### Game version

The browser reads your DayZ version from the game's program file (`DayZ_x64.exe`) and compares it
with each server's. A server on another version shows its version in orange, the join window
warns, and **Hide other versions** hides them.

### Mods

When you select a server, the browser asks it for its mod list (Steam's A2S_RULES query, where
DayZ servers put their mods). If the server doesn't answer, it uses the mods from DZSA's list.

Whether a mod is installed and current comes from Steam's own records
(`steamapps/workshop/appworkshop_221100.acf`) and the Workshop's last-update times.

Missing mods are downloaded by a small helper program, `dayz-browser-workshop`, that talks to your
running Steam client the way the official launcher does: it subscribes to each mod and asks Steam
to download it, with progress shown in the join window. It needs no key. While it runs, Steam
shows DayZ as running, so the helper exits as soon as the downloads finish. If it can't be used,
**Open Workshop pages** opens each mod's page in Steam to subscribe yourself; the join window
notices when Steam has downloaded them.

### Launching

- **Linux:** `steam -applaunch 221100 -nolauncher -connect=<ip>:<port>:<query port> -mod=...`.
  Steam runs the game under Proton with BattlEye. Steam on Linux hangs when launch arguments pass
  about 1,000 characters, so mods are passed as short links in the DayZ folder (`@w...`), which
  the browser makes and removes itself.
- **Windows:** `DayZ_BE.exe 0 1 1 -exe DayZ_x64.exe -mod=<full paths> -connect=...`, the way the
  official launcher starts the game.

Extra launch arguments (Settings) are added to every launch.

### The map overlay

With the overlay on (the default), the browser starts `dayz-map run` in the background if it isn't
running yet. The overlay keeps working if you close the browser, and it still finds the map by
itself if you join a server some other way. The overlay's in-game hotkey can be changed in the
browser's settings; the browser writes it to the overlay's config and restarts the overlay.

## Build

Needs Rust 1.95 or newer.

```sh
cargo build --release
```

This builds `target/release/dayz-browser` and the helper `target/release/dayz-browser-workshop`.
The helper needs Valve's Steamworks library next to it (`libsteam_api.so` on Linux,
`steam_api64.dll` on Windows). The build puts a copy under `target/release/build/steamworks-sys-*/out/`;
release downloads include it. Valve's library isn't part of this repository.

## Settings and files

- Settings: `~/.config/dayz-browser/config.toml` (Windows: `%APPDATA%\dayz-browser\config`).
- Favorites, recent servers, filters, and the cached list: `~/.local/share/dayz-browser/`
  (Windows: `%APPDATA%\dayz-browser\data`).

## Credits

- The mod list format was documented by [dayzquery](https://github.com/Yepoleb/dayzquery) and
  [WoozyMasta/a2s](https://github.com/WoozyMasta/a2s) (both MIT). This decoder is written from that
  description.
- Server list: [DZSA Launcher](https://dayzsalauncher.com)'s public API.
- Steamworks access: [steamworks-rs](https://github.com/Noxime/steamworks-rs).

## License

MIT OR Apache-2.0, at your option. Valve's Steamworks library, included in release downloads, is
under Valve's own terms.
