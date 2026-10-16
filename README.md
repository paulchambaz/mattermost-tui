# mattermost-tui

A keyboard-driven terminal client for Mattermost. The binary is called `mm`.

Channels are tabs along the top, the chat is a plain log with the nicks aligned on the right, and a reversed status line closes the screen. There are no frames. Everything the screen hides has a key that reveals it.

It is a Rust port of the Quiet layout of tecord, adapted to Mattermost.

Status: scaffolding, nothing works yet.

## Build

```
just build
just run
```

## License

GPL-3.0-only
