# mattermost-tui

A keyboard-driven terminal client for Mattermost.

## Build

```
just build
just run
```

## Configuration

Servers are added from inside `mm` with `:new-server` and kept in `~/.config/mm/servers.toml`. Everything else is optional and goes in `~/.config/mm/config.toml`:

```toml
editor = "vi"
copy = "xclip -selection clipboard"

[open]
image = "nsxiv"
video = "mpv"
browser = "firefox"
text = "nvim"
other = "xdg-open"
```

- `editor` is the program `ctrl+g` runs in the lower half of the screen; without it `$EDITOR` is used, then `vi`.
- `copy` is the command that receives the text copied with `y` or `Y`. Without it `mm` uses `wl-copy`, `xclip` or `xsel` when it finds one for your session, and it always also asks the terminal to copy (OSC 52).
- `[open]` says what `o` starts: `browser` for links, `image` for images, `video` for video and audio, `text` for text and source files, `other` for the rest. Each defaults to `xdg-open`. `text` is run inside `mm`, in the lower half of the screen, so it should be a terminal program; the others are started on their own and keep running if `mm` is closed.

## Contribution

Contributions are welcome: feature suggestions, bug reports or code, through issues and pull requests on GitHub.

## License

<a href="https://www.gnu.org/licenses/gpl-3.0.en.html">
  <img src="https://www.gnu.org/graphics/gplv3-127x51.png" alt="GNU GPLv3" />
</a>

taiga-cli is free software. You can use, study, modify and distribute it under the
terms of the [GNU General Public License](https://www.gnu.org/licenses/gpl-3.0.en.html),
version 3.
