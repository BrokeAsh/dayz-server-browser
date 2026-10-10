# Changelog

## Unreleased

- First version: server list from DZSA (or Steam with your own Web API key), filters, live pings,
  mod lists read from each server, mod downloads through the Steam client, launching on Linux and
  Windows, and the DayZ map overlay started alongside.
- Filters for several maps (with type-to-search), players online, and server size.
- A Queue column, and resizable columns.
- Game version check: a warning for servers on another version, and a filter to hide them.
- Connect by address, for servers the list doesn't have.
- Join when there's room: for a full, queued, or restarting server, asks it every second and
  launches once it has a free slot and nobody queued, checking its mods again after a restart.
- Calmer list: servers are pinged again every 5 minutes, the list re-sorts at most every few
  seconds, and a ping sort ignores small ping changes.
- The map overlay is built in: it comes from the overlay's repository as a git submodule and ships
  next to the browser, so it no longer has to be installed or found separately.
- Updates from the browser: it looks for a new release at start, and an Update button in the top
  bar downloads it, checks it, replaces the files, and restarts the browser and the overlay.
