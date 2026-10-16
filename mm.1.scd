MM(1) "github.com/paulchambaz/mattermost-tui" "General Commands Manual"

# NAME

*mm* - keyboard-driven terminal client for Mattermost

# SYNOPSIS

*mm* [*--help*] [*--version*]

# DESCRIPTION

*mm* is a terminal client for Mattermost. It keeps several servers and teams
open at once, shows new messages as they arrive, tells you what is unread and
where you were mentioned, and lets you write messages in your own editor.

The screen has four parts. The channels of the current team are tabs along the
top: the current one is inverted, unread ones are bold with a dot, and a
channel where you were mentioned ends with *!*. Below is the conversation, a
plain log with the time, the author and the text of each message. At the
bottom, the input row appears when you type, and a status line shows the mode,
where you are, who is typing, your position in the channel and how to get help.

Keys are single letters, with no sequences. They act on the selected message,
the one highlighted in the log. Channels are written *#name* when public,
*+name* when private and *@name* for direct and group conversations.

# OPTIONS

*--help*
	Print a short help and exit.

*--version*
	Print the version and exit.

# GETTING STARTED

Start *mm* and type *:new-server*. It asks, one question at a time, for a
name, the address of the server, your login and your password, and for the
code of your authenticator when your account asks for one. Once the login
works, the server is saved and opened. Repeat it to add more servers.

The label at the left of the tabs shows the server and the team you are in.
*H* and *L* move
between the teams of all your servers, *J* and *K* between the channels of the
current team, and *f* finds a channel by name.

Press *?* at any time for the list of keys.

# KEYS

*j k*
	Next and previous message. At the top of the channel, *k* loads older
	messages.

*g G*
	First and last message. *g* also loads older messages.

*w b*
	Next and previous group of messages from the same author.

*J K*
	Next and previous channel.

*H L*
	Previous and next team, across servers.

*:*
	Open the command line. See *COMMANDS*.

*?*
	Show the help. *j* and *k* scroll it, *esc* closes it.

*n*
	Go to the next unread message, then to the next channel with unread
	messages.

*N*
	Go to the next message that mentions you, in any team and any server.

*c*
	Mark the open channel as read.

*f F*
	Find a channel. *f* looks in the current team, *F* in every team of
	every server. See *FINDER*.

*i*
	Write a message. *enter* sends it, *esc* leaves and keeps the text for
	next time. See *WRITING*.

*ctrl-g*
	Write the message in your editor, in the lower half of the screen.

*r*
	Reply in the thread of the selected message.

*e*
	Edit the selected message, if it is yours.

*d*
	Delete the selected message, if it is yours. *y* confirms.

*a*
	Show or hide the reactions under the messages.

*x X*
	React to the selected message. *x* shows the usual reactions on the
	status line and *1* to *9* picks one. *X* repeats the last one. Reacting
	again with the same emoji removes your reaction. Showing the reactions
	is turned on for you.

*V*
	Select a range of messages, then *j* and *k* extend it.

*y Y*
	Copy the selected message or range. *Y* adds the author and the time of
	each message.

*p*
	Paste what was copied into the input.

*m*
	Show the users of the channel with their presence. *j* and *k* scroll
	the column, *m* hides it.

*o*
	Open the links and attachments of the selected message. See *LINKS AND
	ATTACHMENTS*.

*q*
	Quit.

# FINDER

*f* and *F* put a search line on the input row and the matching channels on
the status line, like a wildmenu.

_words_
	Channels whose name contains every word. With *F*, the name of the
	server and the team count too.

_@x_
	Direct and group conversations matching _x_.

_a#b_
	The channel _b_ of the team _a_. *#b* is the channel _b_ in any team,
	*a#* is the team _a_ alone.

*tab* and the arrow keys choose, *enter* goes there, *esc* closes.

# COMMANDS

A command is typed after *:* and run with *enter*. As in vi, a command can be
shortened to the fewest letters that tell it from the others: *:ser* is *server*,
*:rel* is *reload*, *:q* is *quit*. When the letters fit several commands, they
are listed instead. The status line shows the commands that begin with what you
typed, and *tab* writes the first one in full, then the next ones, *shift-tab*
going back. Names of servers, teams and
users are matched without regard to case: a whole name wins, otherwise a part of
a name works when only one name has it.

*server*, *team*, *remove-server* and *dm* show the names that fit what you type on
the status line, like the finder does. *tab* and the arrow keys choose and
*enter* takes the chosen name, the first one if you did not choose. For *dm*
the names are the users *mm* already knows, such as the authors of messages
you have seen and the people you have talked to.

*new-server*
	Log in to another server.

*server* _name_
	Go to a server.

*team* _name_
	Go to a team of the current server.

*go* _words_
	Open the best channel matching the words, in the current team, as *f* does.
	See *FINDER*.

*remove-server* _name_
	Forget a server: it is taken out of _servers.toml_ and closed. Nothing is
	changed on the server itself.

*dm* _user_
	Open the direct conversation with a user, creating it if needed.

*join* _channel_
	Join a public channel of the current team, given by the name it has in its
	address, such as _town-square_.

*leave*
	Leave the open channel. Direct and group conversations cannot be left.

*topic*
	Show the header and the purpose of the open channel.

*reload*
	Fetch the open channel, its messages and the channel list again, for
	example after the connection dropped.

*mark-read*
	Mark the open channel as read, like *c*.

*mark-all-read*
	Mark every unread channel of the current team as read.

*users*, *reactions*, *open*
	The same as *m*, *a* and *o*.

*copy* [*full*]
	The same as *y*, or as *Y* with *full*.

*react* _name_
	React to the selected message with any emoji known by name, such as
	_tada_. Like *x*, reacting again with the same emoji removes it.

*pin*, *unpin*
	Pin the selected message to its channel, or unpin it.

*status* _online|away|dnd|offline_
	Set your presence.

*set* _key_ _value_
	Change a setting until you quit: *editor*, *copy*, *open.image*,
	*open.video*, *open.browser*, *open.text* or *open.other*. Without a value,
	*editor* and *copy* go back to their automatic choice. Nothing is written to
	_config.toml_.

*help*
	Show the help.

*quit*
	Quit.

# WRITING

*i* opens a line at the bottom of the screen. *enter* sends the message, *esc*
leaves without sending: the text is kept, per channel, until you come back to
it. Pasting keeps line breaks.

*ctrl-g* works in the same way, from the input row or from the log. It starts
your editor on the text, in the lower half of the screen, while the
conversation stays alive above it. When you quit the editor, the text comes
back to the input row, where *enter* sends it. Leaving the editor with an empty
file cancels. The editor is the *editor* setting, else *$EDITOR*, else *vi*.

*r* replies to the selected message, *e* edits it and *d* deletes it. Editing
and deleting only work on your own messages.

Other people see when you are typing, as with the web app. Sending a message
marks the channel as read.

# MARKDOWN AND EMOJI

Messages are shown with a minimal version of Markdown: the markers are hidden
and the text is styled. *\*\*bold\*\**, *\*italic\** and *\_italic\_* (only at
the edge of a word, so snake_case_names are left alone), *~~strikethrough~~*,
*`code`* and fenced blocks, *# headings*, *> quotes* and *[text](url)* links.
A backslash keeps a marker as typed. Lists, tables and images are shown as
they were written.

Mentions are colored: yellow when they are you, *@all*, *@channel* or
*@here*, blue otherwise. System messages such as joins and leaves are muted.

*:smile:* style names are shown as the emoji. Names the terminal font cannot
know, such as custom emoji of the server, stay as they are written.

# LINKS AND ATTACHMENTS

*o* opens what the selected message holds: its links first, then its
attachments. With nothing to open it says so, with one thing it opens it, and
with several it shows them on the status line and *1* to *9* picks one.

A link goes to the *browser* program. An attachment is downloaded first, to a
private folder that is removed when *mm* quits, then opened by the program
for its type: *image* for images, *video* for video and audio, *text* for
text and source files and *other* for the rest. See *CONFIGURATION*.

The *text* program runs inside *mm*, in the lower half of the screen, and you
come back when you quit it. The other programs are started on their own and
keep running if you close *mm*. Large files are downloaded completely before
they are opened.

# CONFIGURATION

Servers are in _servers.toml_, one *[[server]]* table each:

*name*
	How the server is called in the status line.

*url*
	The address of the server.

*login*
	Your login, for the password forms below.

*password*, *password_cmd*, *token*, *token_cmd*
	Exactly one of them. A password or a personal access token written in
	the file, or a command whose output is that password or token. The
	commands are run by the shell when *mm* starts.

_config.toml_ holds the settings. Every key is optional and a mistake in a key
name is reported when *mm* starts.

*editor*
	The program *ctrl-g* starts. It may have arguments.

*copy*
	The command that receives the text copied with *y*. Without it, *mm*
	uses *wl-copy*, *xclip* or *xsel*, whichever is installed for your
	session. The terminal is also asked to copy, which is enough in many
	terminals, including over SSH.

*[open]*
	A table with the programs for *browser*, *image*, *video*, *text* and
	*other*, as described in *LINKS AND ATTACHMENTS*. Each defaults to
	*xdg-open*. The file or address is added as the last argument.

# ENVIRONMENT

*EDITOR*
	The editor, when *editor* is not set.

*NO_COLOR*
	When set to anything, *mm* uses no colors; emphasis is kept.

*TMPDIR*
	Where attachments are downloaded and messages are written for the
	editor.

# FILES

_~/.config/mm/servers.toml_
	The servers and how to log in to each. Added with *:new-server*, and
	readable only by you.

_~/.config/mm/config.toml_
	The settings above.

# EXAMPLES

A _servers.toml_ with a password kept in a password manager, and a token:

```
[[server]]
name = "work"
url = "https://chat.example.org"
login = "paul"
password_cmd = "pass show work/chat"

[[server]]
name = "club"
url = "https://club.example.net"
token_cmd = "secret-tool lookup service club-chat"
```

A _config.toml_ for a Wayland desktop:

```
editor = "nvim"

[open]
image = "imv"
video = "mpv"
browser = "firefox"
text = "nvim -R"
```

Jump to the channel _ops_ of the team _infra_: press *f*, type _infra#ops_ and
press *enter*.

# EXIT STATUS

*0* on success. *1* when the arguments are wrong or an error stops *mm*, which
is then printed on the standard error with its cause.

# AUTHORS

This program was written by Paul Chambaz. It is licensed under the GPLv3, for
more information, please read the *LICENSE* file.
