# Changelog

## 0.2.0

New:

- **Notify when there's room:** right-click any number of servers in the list and add them to a
  watch list; you get a desktop notification when one has a free slot and no queue. This is
  separate from waiting to join, which still launches the game by itself.
- A right-click menu on server rows: join, favorite, notify, add a note, copy the address, and
  more.
- **Notes** on servers, shown as a mark after the name and searched along with it.
- **Player history:** the browser records how many play on your favorite, recent, and watched
  servers while it's open, charts the last day or week, and says when a server usually restarts.
- **Country** column and filter, from DB-IP's free country database; search matches countries
  too.
- **More filters:** official or community, in-game day or night, time speed, BattlEye, and low
  population.
- **Hide likely fakes** (on by default): listings that share an address, copies of a busy server,
  numbers that can't be right, and servers whose own answers keep disagreeing with the list.
  The status bar says how many are hidden.
- **Mods** page: the Workshop mods you have, their size and which of your servers use them;
  unsubscribe from several at once, or from all that none of your servers uses, and clean up
  leftover folders.
- **Launch options** in Settings (name in game, skip the intro, windowed, empty main menu, CPU
  cores, and more), with a preview of what a launch adds.
- Disk space checks before mods download, and before waiting to join.
- **Play offline** with Community Offline Mode on Chernarus, Livonia, or Namalsk, with saves kept
  across updates.
- **Find servers on my network** in Connect by address.
- **News** from the DayZ developers on Steam, with a count of new posts.
- Escape closes the window in front, and windows open beside each other instead of on top.

Changed:

- The map overlay comes inside the browser's release and is kept up to date with it; the
  setting for where to find it is gone.
- Pings and re-sorts refresh less often.
- In a narrow window, less important columns make way so the name, map, players, ping, and
  padlock always show, and the details panel scrolls.
- Updates check the new version runs before replacing anything, and leave other files in the
  folder alone. "Up to date" now says so.
- Search matches an address with the game port too.

Fixed:

- Extra launch arguments on Windows keep their backslashes.
- Waiting to join keeps a busy window, keeps asking for missing mods, and minds the game version.
- Mods in Addons folders with any letter case are found, and Steam's manifests are trusted over
  file times when deciding what needs an update.
- An empty answer from DZSA counts as an error and isn't cached.
- The overlay quits when it's turned off in Settings.
- Settings, favorites, and filters saved by another version load instead of being reset.
- Many smaller fixes from two rounds of review.

## 0.1.0

First release.

- Server list from DZSA Launcher (about 10,000 servers), or Steam's list with your own Web API
  key.
- Search and filters: several maps (type to search), empty, full, locked, first or third person,
  vanilla or modded, players online, server size, ping, and game version.
- Live pings, player counts, queues, and in-game time, with resizable columns.
- Each server's mod list read from the server itself; missing mods downloaded through the Steam
  client.
- Launching on Linux (through Steam and Proton) and Windows, with optional extra launch arguments.
- Connect by address, for servers the list doesn't have.
- Join when there's room: waits for a free slot with nobody queued, and joins right after a
  restart.
- The DayZ map overlay built in, started in the background, with its hotkey set in Settings.
- Updates from the browser: an Update button appears when a new release is out.
