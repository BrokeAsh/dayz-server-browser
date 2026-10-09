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
