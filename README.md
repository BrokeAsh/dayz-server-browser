# DayZ Server Browser

A DayZ server browser and launcher for Linux and Windows, written in Rust. It comes with the
[DayZ map overlay](https://github.com/BrokeAsh/dayz-map-overlay) built in: while the browser is
open, the overlay runs in the background, ready for the game.

- Lists every server DZSA Launcher knows (about 10,000), with search and filters (maps, empty,
  full, locked, first or third person, vanilla or modded, players, server size, ping, game
  version, favorites). **More filters** adds country, official or community, in-game day or
  night, time speed, BattlEye, and low population (under 30% full).
- Shows each server's country, and hides likely fakes and servers listed twice (see below).
- Pings the servers on screen and shows live players, queue, and in-game time.
- Warns about servers on a different version of DayZ than yours, which the game can't join.
- Asks the selected server for its mod list and shows which mods you have, which need an update,
  and which are missing.
- Downloads missing mods through your Steam client, or opens their Workshop pages.
- Launches DayZ connected to the server with its mods, and stays open afterwards. For a full or
  queued server, it can wait and join as soon as there's a free slot and no queue, such as right
  after a restart.
- Connects by address to servers the list doesn't have, and finds servers on your local network.
- Notifies you when servers you pick have room: right-click any number of servers and choose
  **Notify me when there's room**.
- Plays DayZ offline, on your own copy of Chernarus, Livonia, or Namalsk, with the Community
  Offline Mode missions.
- Notes on servers, which the search looks in too.
- Records how many play on your favorite, recent, and watched servers while it's open, charts the last day
  or week, and tells when a server usually restarts.
- Optional: use Steam's own server list with your Steam Web API key instead of DZSA's.
- Shows the DayZ developers' latest news from Steam, with a count of new posts on the News
  button.

Status: early. Tested on Linux with a real DayZ install, including mod downloads and joining;
Windows is only tested in a VM so far.

## Install and update

Download the latest release from the
[releases page](https://github.com/BrokeAsh/dayz-server-browser/releases) and unpack it in a folder
you can write to:

- **Linux:** for example `~/.local/opt/dayz-browser`, then run `dayz-browser` from there (or link
  it into `~/.local/bin`).
- **Windows:** any folder, such as one in your Documents, then run `dayz-browser.exe`.

Everything comes in that one folder: the browser, the Workshop helper, the map overlay, and
Valve's Steamworks library.

To update, click **Update** in the top bar when it appears. The browser asks GitHub for the
latest release when it starts (and twice a day while it stays open). The update downloads the
new release, checks it against the release's checksums, replaces the files in the folder, and
restarts the browser and the overlay; your settings, favorites, and filters stay. Nothing is
changed if the download fails or doesn't match. It waits while DayZ is running, since it restarts
the overlay. Settings can turn the check off. If the browser can't write to its own folder (for
example when a package manager installed it), update it the way you installed it.

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

**Find servers on my network** (in the same window) asks every address in your computer's local
network (its /24, such as `192.168.1.1` to `192.168.1.254`) on the usual query ports, 27016 and
2303, plus one more port if you type it, and lists the DayZ servers that answer within two
seconds. Joining one remembers it like a server typed in.

### Playing offline

**Play offline** uses [Community Offline Mode](https://github.com/Arkensor/DayZCommunityOfflineMode),
a set of missions that run a map in the game itself, with no server: loot and infected spawn as
usual, and in the game, Y (Z on German keyboards) opens its menu (items, teleporting, the free
camera). It isn't a Workshop mod: the browser downloads its latest GitHub release (about 13 MB)
into its data folder, and offers an update when a new one comes out. Each map's progress is saved
in its mission folder (`storage_-1`); updates keep it, and **Reset save** deletes it to start that
map over. Namalsk needs the Namalsk Island and Namalsk Survival mods, which the window downloads
like a server's mods.

- **Linux:** `steam -applaunch 221100 -nolauncher -mission=Missions/<map's mission> -nosplash
  -noPause -noBenchmark -filePatching -doLogs -scriptDebug=true`, with `-mod=` for Namalsk. The
  mission path is relative to the game folder, so the browser links `Missions` there to its copy
  (only if nothing else has that name) and removes the link at its next start. Flatpak Steam can't
  see the browser's data folder until it's allowed:
  `flatpak override --user --filesystem=~/.local/share/dayz-browser/offline com.valvesoftware.Steam`.
- **Windows:** `DayZ_x64.exe -mission=<full path of the mission>` with the same arguments, the way
  the release's own `.bat` starts it (without BattlEye). Nothing is written to the game folder.

### Notes and player history

Right-click a server and pick **Add note…** (or **Add note** in the details panel) to write
something down about it. A server with a note has a small note mark after its name in the list;
hover it to read the note. Notes are saved with your favorites. Empty a note to remove it.

Sites that keep player history want a paid key or have no API, so the browser keeps its own: while
it's open, it asks your favorite, recent, and watched servers for their player count every 5 minutes (and
keeps answers it gets anyway), for two weeks. The details panel charts the last 24 hours or 7 days.
When a server stops answering for a few minutes, or its in-game clock is set back, that counts as
a restart; once those fall at the same times of day on several days, the panel says when it usually
restarts (in your local time). Nothing is recorded while the browser is closed.
### Countries, official servers, and likely fakes

- **Country** comes from the server's IP address, looked up in [DB-IP](https://db-ip.com)'s free
  country database. The browser downloads it (about 4 MB) into its data folder the first time and
  again once it's a month old, in the background; until it's there, servers just have no country.
  Search matches countries too (`de`, `germany`).
- **Official** servers are the game's own: DZSA's "public" shard, or, on Steam's list, servers
  without the community tags. A name with "Official" in it doesn't count.
- **Time speed** is how many times faster than real time the server's day runs (and its night,
  once the server has answered). Day is 06:00 to 17:59 in-game.
- **Hide likely fakes** (on by default) hides a listing when another one has the same IP and
  game port; when several listings with players share a name, map, and mods (the busiest one
  stays; default host names like "nitrado.net gameserver" don't count); when its numbers can't
  be right (more players than slots, over 127 slots, a queue over 255); and when the server's own
  answers keep saying far fewer players than the list. The details panel says why. Favorites,
  recent servers, watched servers, and servers added by address always show.

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

The join window adds up the size of the mods to download or update (from the Workshop's details;
an update counts in full, since Steam downloads the whole new version before replacing the old
one) and checks the free space on the drive with the Steam library. If the downloads wouldn't
leave a tenth of their size spare (at least 1 GB), it says how much is needed and how much is
free, and won't download them or wait to join until there's room. When sizes aren't known, it
says so and doesn't stop you.

**Mods** in the top bar lists the Workshop mods Steam installed for your account, with their
size, when each was last updated (and whether there's an update), and which of your favorite and
recent servers use them. Search and sort them, open a mod's Workshop page or folder, or
unsubscribe from mods, one at a time or all those no favorite or recent server uses. The helper
does the unsubscribing; Steam deletes the files once it no longer counts DayZ as running, a
moment after the helper quits. Folders Steam didn't install for your account (left behind, or
another Steam account's) are listed apart, and can be deleted. Nothing is unsubscribed or deleted
while DayZ runs.

### Launching

- **Linux:** `steam -applaunch 221100 -nolauncher -connect=<ip>:<port>:<query port> -mod=...`.
  Steam runs the game under Proton with BattlEye. Steam on Linux hangs when launch arguments pass
  about 1,000 characters, so mods are passed as short links in the DayZ folder (`@w...`), which
  the browser makes and removes itself.
- **Windows:** `DayZ_BE.exe 0 1 1 -exe DayZ_x64.exe -mod=<full paths> -connect=...`, the way the
  official launcher starts the game.

Settings has the common launch options, added to every launch:

- **Name in game** (`-name=`), if set. Quotes, backslashes, backticks, and `$ ; & | < >` are left
  out, since they could break the command line on the way to the game.
- **Skip the intro and splash screens** (`-nosplash -skipIntro`), **Keep running in the
  background** (`-noPause`), **Windowed** (`-window`), **Empty main menu** (`-world=empty`, loads
  faster), and **CPU cores** (`-cpuCount=`, off by default).

Anything else goes in **Extra launch arguments**, written as on a command line. Settings shows
the arguments a launch adds, and flags extra arguments that repeat the options or the ones the
browser sets itself (`-connect`, `-mod`, `-password`, `-port`). On Linux, the options count
towards Steam's limit of about 1,000 characters.

### News

**News** in the top bar lists the developers' latest announcements on Steam (Steam's public news
API; no key needed), and shows how many are new since you last looked (only posts from the last
30 days count). Clicking a title opens the post in your web browser. The news is fetched when the
browser starts and every 6 hours; if Steam can't be reached, the News window says so and nothing
else is affected.

### Discord

The browser can show the server you're playing on in your Discord status (its name, map, and
first or third person, with the time played), from the moment it launches DayZ until the game
closes. It talks to the Discord app running on the same computer, and does nothing if Discord
isn't running; it picks up if Discord starts later or restarts. Settings has a switch for it. The
feature needs the browser's own Discord application id (`DISCORD_APP_ID` in `src/discord.rs`,
from the [Discord developer portal](https://discord.com/developers/applications)); builds without
one leave it out entirely.

### Notifications

Right-click a server and pick **Notify me when there's room** (or click **Notify** in its details)
to put it on your watch list; the status bar's bell opens the list, with how each server stands.
While the browser is open it asks each one about every 30 seconds, and sends a desktop
notification when one goes from full, queued, or not answering to a free slot with no queue.
A server that keeps getting and losing a slot isn't announced again until it has been without room
for two minutes. This is separate from the join window's **Join when there's room**, which joins by
itself.

- **Linux:** through your desktop's notification service. Clicking the notification or its
  **Join** button brings the browser forward with the join window open.
- **Windows:** a toast, for information only. Windows names the sender of a toast after the
  program's Start menu entry, which the unpacked browser doesn't have, so it shows as Windows
  PowerShell, and clicking it doesn't bring up the browser.

If no notification can be shown, the browser says it in its own window instead. Settings has
**Play a sound** and **Send a test notification**.

### The map overlay

The overlay ships with the browser: its repository is included here as a git submodule
(`overlay/`), and it's built and packaged with the browser, so there's nothing else to install.
It's still its own program (`dayz-map`, next to `dayz-browser`), and its repository still has its
own releases for anyone who wants only the overlay.

With the overlay on (the default), the browser starts `dayz-map run` in the background if it isn't
running yet. The overlay keeps working if you close the browser, and it still finds the map by
itself if you join a server some other way. The overlay's in-game hotkey can be changed in the
browser's settings; the browser writes it to the overlay's config and restarts the overlay.

## Build

Needs Rust 1.95 or newer. The overlay comes in as a submodule, so clone with it:

```sh
git clone --recurse-submodules <this repository>
# or, in an existing clone:
git submodule update --init

cargo build --release
```

This builds `target/release/dayz-browser`, the helper `target/release/dayz-browser-workshop`, and
the overlay `target/release/dayz-map`.

The overlay follows the overlay repository's `main` by itself: every hour, the "Update the
overlay" workflow looks for new overlay commits, tests the browser with them on Linux and Windows,
and commits the submodule update here (run it from the Actions tab to update right away). Users
get the new overlay with the next browser release.
The helper needs Valve's Steamworks library next to it (`libsteam_api.so` on Linux,
`steam_api64.dll` on Windows). The build puts a copy under `target/release/build/steamworks-sys-*/out/`;
release downloads include it. Valve's library isn't part of this repository.

## Settings and files

- Settings: `~/.config/dayz-browser/config.toml` (Windows: `%APPDATA%\dayz-browser\config`).
- Favorites, recent servers, the watch list, filters, the cached list, and the country database:
  `~/.local/share/dayz-browser/` (Windows: `%APPDATA%\dayz-browser\data`). Offline Mode and its
  saves are in `offline/` there.
- Notes are in `state.json` there, and player history in its `history` folder (a small file per
  server).

## Credits

- The mod list format was documented by [dayzquery](https://github.com/Yepoleb/dayzquery) and
  [WoozyMasta/a2s](https://github.com/WoozyMasta/a2s) (both MIT). This decoder is written from that
  description.
- Server list: [DZSA Launcher](https://dayzsalauncher.com)'s public API.
- Server countries: [IP geolocation by DB-IP](https://db-ip.com) (IP to Country Lite, CC BY 4.0),
  read with [maxminddb](https://github.com/oschwald/maxminddb-rust) (ISC).
- Steamworks access: [steamworks-rs](https://github.com/Noxime/steamworks-rs).
- Offline mode: [DayZ Community Offline Mode](https://github.com/Arkensor/DayZCommunityOfflineMode),
  downloaded from its releases. The Linux launch follows [DZGUI](https://github.com/aclist/dztui).
- Discord status: [discord-rich-presence](https://github.com/vionya/discord-rich-presence) (MIT).

## License

MIT OR Apache-2.0, at your option. Valve's Steamworks library, included in release downloads, is
under Valve's own terms.
