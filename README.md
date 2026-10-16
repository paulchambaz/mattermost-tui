# mattermost-tui

mattermost-tui (`mm`) is a keyboard-driven terminal client for Mattermost. It keeps several servers and teams open at once, shows new messages as they arrive, tells you what is unread and where you were mentioned, finds a channel as you type its name, and lets you write your messages in your own editor. Links and attachments open in the programs you choose.

That being said, the real ground truth is your Mattermost server. `mm` is for reading and writing messages: administration, calls, plugins, dialogs and inline images are left to the web app.

## Installation

### Nix

```sh
nix profile install github:paulchambaz/mattermost-tui
```

Or add `github:paulchambaz/mattermost-tui` as an input of your flake and install its `packages.default`. The man page is installed with it.

### Cargo

```sh
cargo install mattermost-tui
```

This installs the `mm` command.

### Manual

See the [**Building**](#building) section.

## Usage

Start `mm`, then type `:new-server`. It asks for a name, the address of the server, your login and your password (and a code if your account uses two-factor authentication), and logs you in. Add as many servers as you like.

The channels of the current team are tabs along the top, the conversation is below, and the bottom line shows where you are. Keys are single letters:

| Key | Action |
| --- | --- |
| `j` `k` | next / previous message |
| `J` `K` | next / previous channel |
| `H` `L` | previous / next team, across servers |
| `f` | find a channel by typing part of its name |
| `n` `N` | next unread message / next mention |
| `i` | write a message, `enter` sends |
| `ctrl-g` | write it in your editor instead |
| `r` `e` `d` | reply to, edit, delete the selected message |
| `x` | react to the selected message |
| `o` | open the links and attachments of the selected message |
| `y` | copy the selected message |
| `?` | the full list of keys |
| `q` | quit |

Everything is detailed in the [`man`](./mm.1.scd) page (`man mm` once installed).

Your servers are kept in `~/.config/mm/servers.toml`, readable only by you, password included so the session can be renewed. If you would rather not keep a password in a file, `servers.toml` can instead name a command that prints it, such as `password_cmd = "pass show work/chat"`. See the man page.

## Configuration

Everything is optional. Settings go in `~/.config/mm/config.toml`:

```toml
editor = "nvim"
copy = "xclip -selection clipboard"

[open]
image = "nsxiv"
video = "mpv"
browser = "firefox"
text = "nvim -R"
other = "xdg-open"
```

- `editor` is the program `ctrl-g` starts. Without it `$EDITOR` is used, then `vi`.
- `copy` is the command that receives the text copied with `y`. Without it `mm` uses `wl-copy`, `xclip` or `xsel` if one is installed.
- `[open]` says which program `o` starts: `browser` for links, `image`, `video` for video and audio, `text` for text and source files, `other` for the rest. Each defaults to `xdg-open`. `text` runs inside `mm`, so it should be a terminal program.

Set `NO_COLOR` to use `mm` without colors.

## Building

### Nix

```sh
git clone https://github.com/paulchambaz/mattermost-tui.git
cd mattermost-tui
nix develop # a shell with every dev tool
nix build
just --list # the dev commands
```

### Manual

You need `cargo`, `scdoc` for the man page and `just` for the dev commands.

```sh
just build
just test
just lint
just install # binary and man page in ~/.local
```

## Contribution

Contributions are welcome: feature suggestions, bug reports or code, through issues and pull requests on GitHub.

## License

<a href="https://www.gnu.org/licenses/gpl-3.0.en.html">
  <img src="https://www.gnu.org/graphics/gplv3-127x51.png" alt="GNU GPLv3" />
</a>

mattermost-tui is free software. You can use, study, modify and distribute it under the
terms of the [GNU General Public License](https://www.gnu.org/licenses/gpl-3.0.en.html),
version 3.
