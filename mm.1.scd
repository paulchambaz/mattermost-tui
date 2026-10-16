MM(1) "github.com/paulchambaz/mattermost-tui" "General Commands Manual"

# NAME

*mm* - keyboard-driven terminal client for Mattermost

# SYNOPSIS

*mm*

# DESCRIPTION

*mm* is a terminal client for Mattermost. The channels of the current team are
tabs along the top, the chat is a plain log, and a status line closes the
screen. Keys are single letters, with no sequences. Press *?* inside *mm* for the full list.

# FILES

_~/.config/mm/servers.toml_
	The servers and how to log in to each. Added with *:new-server*.

_~/.config/mm/config.toml_
	Optional settings: *editor* (the program run by *ctrl+g*), *copy* (the
	command that receives copied text) and the *[open]* table with the
	programs for *image*, *video*, *browser*, *text* and *other* that *o*
	starts. Each program defaults to *xdg-open*; the editor defaults to
	*$EDITOR*, then *vi*.

# LICENSE

GPL-3.0-only
