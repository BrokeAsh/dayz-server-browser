# AUR package

`dayz-server-browser-bin` installs the Linux release as it is: the browser, the Workshop helper,
the map overlay, and Valve's Steamworks library in `/opt/dayz-server-browser`, with
`dayz-browser` on the PATH and a menu entry. Installed this way, the browser can't change its
own folder, so instead of an Update button it says when a new version is out, to update with
the package manager.

For each release, once it's published on GitHub:

```sh
packaging/aur/update.sh 0.3.1     # version, checksums, .SRCINFO
```

Commit the result here (the README's Arch Linux steps build from it). Then copy
`dayz-server-browser-bin/PKGBUILD`, `.SRCINFO` and `dayz-browser.desktop` into a
clone of `ssh://aur@aur.archlinux.org/dayz-server-browser-bin.git`, commit, and push.
