//! Application state, the events that change it, and the requests it starts.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::api::types::{
    CategoryList, Channel, ChannelId, ChannelMember, Post, PostId, PostList, Reaction, Status,
    Team as WireTeam, TeamId, User, UserId, UserStatus,
};
use crate::api::{Client, Error};
use crate::clipboard;
use crate::config::{self, Credentials, ServerConfig, Settings};
use crate::debug;
use crate::finder;
use crate::input::{self, Command, Line, Step, Wizard};
use crate::model::{
    self, group_start, group_with_previous, Attachment, Link, Message, Server, Team,
};
use crate::open::{self, Files, Kind as FileKind};
use crate::term::{self, Pane};
use crate::unread;
use crate::ws::{self, Action, Event, Ws};

/// Everything that can wake the application.
#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Resize,
    Loaded(usize, Loaded),
    LoggedIn(Result<Box<Session>, Error>),
    Ws(usize, Ws),
    Paste(String),
    Notice(String),
    Tick,
    Pty,
    EditorDone,
}

/// The answer to one request, tagged by the server it came from.
#[derive(Debug)]
pub enum Loaded {
    Connected(Result<Box<Session>, String>),
    Teams(Vec<WireTeam>),
    Channels {
        team: TeamId,
        channels: Vec<Channel>,
        members: Vec<ChannelMember>,
        categories: Option<CategoryList>,
    },
    Posts {
        channel: ChannelId,
        list: PostList,
    },
    Older {
        channel: ChannelId,
        before: PostId,
        list: Result<PostList, String>,
        asked: usize,
    },
    Users(Vec<User>),
    Downloaded {
        path: std::path::PathBuf,
        name: String,
        kind: FileKind,
    },
    Members {
        channel: ChannelId,
        users: Vec<User>,
        statuses: Vec<UserStatus>,
    },
    Reacted(Reaction),
    Unreacted(Reaction),
    Created(Post),
    Edited(Post),
    Deleted {
        channel: ChannelId,
        id: PostId,
    },
    NotSent {
        channel: ChannelId,
        text: String,
        error: String,
    },
    Viewed,
    Failed(String),
}

/// Something `o` can open: a link or an attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Link(String),
    File(Attachment),
}

/// The reactions of the picker, chosen with the keys 1 to 9.
pub const REACTIONS: [&str; 9] = [
    "+1",
    "heart",
    "joy",
    "tada",
    "eyes",
    "fire",
    "skull",
    "ok_hand",
    "open_mouth",
];

/// The narrowest terminal that has room for the users column.
const MIN_WIDTH_FOR_USERS: u16 = 70;

/// The users column: whether it is shown, has the focus, and how far it is scrolled.
#[derive(Debug, Default)]
pub struct Users {
    pub shown: bool,
    pub focus: bool,
    pub top: usize,
}

/// The channel finder: what is being typed and which match is chosen.
#[derive(Debug)]
pub struct Finder {
    pub all: bool,
    pub line: Line,
    pub selected: usize,
}

/// What the input line is writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Message,
    Reply { root: PostId, to: String },
    Edit(PostId),
}

/// The message being written in INSERT mode.
#[derive(Debug)]
pub struct Compose {
    pub line: Line,
    pub kind: Kind,
}

/// A question the next key answers.
#[derive(Debug)]
enum Awaiting {
    Delete { channel: ChannelId, id: PostId },
}

/// The selected message, as the writing keys need it.
struct Target {
    id: PostId,
    root: Option<PostId>,
    text: String,
    author: String,
    own: bool,
    system: bool,
    mine: Vec<String>,
}

/// What entering a channel should put under the cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Goal {
    #[default]
    Unread,
    Mention,
}

/// The channel being read, and how much of it was already read when it was entered.
#[derive(Debug)]
struct Entry {
    server: usize,
    channel: ChannelId,
    unread: usize,
    boundary: Option<PostId>,
    goal: Goal,
    placed: bool,
}

/// A logged-in connection and the account behind it.
#[derive(Debug)]
pub struct Session {
    pub client: Arc<Client>,
    pub me: User,
}

/// The whole state of the program.
pub struct App {
    pub servers: Vec<Server>,
    configs: Vec<ServerConfig>,
    path: PathBuf,
    tx: UnboundedSender<Msg>,
    pub server: usize,
    pub team: usize,
    pub cursor: usize,
    pub browsing: bool,
    pub follow: bool,
    pub reactions: bool,
    pub scroll: usize,
    pub notice: String,
    pub input: Option<Line>,
    pub wizard: Option<Wizard>,
    pub help: Option<usize>,
    pub finder: Option<Finder>,
    pub picker: bool,
    pub selecting: bool,
    anchor: usize,
    register: String,
    last_reaction: Option<&'static str>,
    pub clipboard: Vec<String>,
    pub users: Users,
    pub compose: Option<Compose>,
    pub pane: Option<Pane>,
    pub size: (u16, u16),
    pub settings: Settings,
    /// Whether colors are dropped, as `NO_COLOR` asks.
    pub plain: bool,
    files: Files,
    pub opener: Option<Vec<Item>>,
    editor_fresh: bool,
    awaiting: Option<Awaiting>,
    drafts: HashMap<(usize, ChannelId), String>,
    typed: Option<Instant>,
    pub quit: bool,
    entry: Option<Entry>,
    goal: Goal,
}

impl App {
    /// Starts with the configured servers, connecting to each; asks for one when there is none.
    pub fn new(
        boot: Vec<(ServerConfig, Result<Credentials, String>)>,
        path: PathBuf,
        tx: UnboundedSender<Msg>,
    ) -> Self {
        let mut app = Self {
            servers: Vec::new(),
            configs: Vec::new(),
            path,
            tx,
            server: 0,
            team: 0,
            cursor: 0,
            browsing: false,
            follow: false,
            reactions: false,
            scroll: usize::MAX,
            notice: String::new(),
            input: None,
            wizard: None,
            help: None,
            finder: None,
            picker: false,
            selecting: false,
            anchor: 0,
            register: String::new(),
            last_reaction: None,
            clipboard: Vec::new(),
            users: Users::default(),
            compose: None,
            pane: None,
            size: (80, 24),
            settings: Settings::default(),
            files: Files::new(),
            opener: None,
            plain: false,
            editor_fresh: false,
            awaiting: None,
            drafts: HashMap::new(),
            typed: None,
            quit: false,
            entry: None,
            goal: Goal::Unread,
        };
        for (i, (config, credentials)) in boot.into_iter().enumerate() {
            app.servers.push(Server::new(config.name.clone()));
            match credentials {
                Ok(credentials) => app.connect(i, &config.url, credentials),
                Err(e) => app.notice = format!("{}: {e}", config.name),
            }
            app.configs.push(config);
        }
        if app.servers.is_empty() {
            app.new_server();
        }
        app
    }

    /// Applies one event.
    pub fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Key(key) => self.key(key),
            Msg::Resize => {}
            Msg::Loaded(i, loaded) => self.loaded(i, loaded),
            Msg::LoggedIn(result) => self.logged_in(result),
            Msg::Ws(i, ws) => self.ws(i, ws),
            Msg::Paste(text) => self.paste(&text),
            Msg::Notice(text) => self.notice = text,
            Msg::Tick | Msg::Pty => {}
            Msg::EditorDone => self.editor_done(),
        }
    }

    /// Name of the current mode, written on the status line.
    pub fn mode(&self) -> &'static str {
        if let Some(pane) = &self.pane {
            return if pane.is_viewer() { "VIEWER" } else { "EDITOR" };
        }
        if self.compose.is_some() {
            return "INSERT";
        }
        match (&self.input, &self.wizard, &self.help, &self.finder) {
            (_, Some(_), ..) => "LOGIN",
            (Some(_), ..) => "COMMAND",
            (.., Some(_)) => "SEARCH",
            (_, _, Some(_), _) => "HELP",
            _ if self.picker => "REACT",
            _ if self.opener.is_some() => "OPEN",
            _ if self.users.focus => "USERS",
            _ if self.selecting => "VISUAL",
            _ => "NORMAL",
        }
    }

    /// What the input row asks for.
    pub fn prompt(&self) -> &'static str {
        self.wizard.as_ref().map_or(":", |w| w.step.prompt())
    }

    /// The channel the reader is looking at, with its server.
    pub fn channel(&self) -> Option<(&Server, &model::Chan)> {
        let server = self.servers.get(self.server)?;
        let team = server.teams.get(self.team)?;
        Some((server, team.chans.as_ref()?.get(team.cur)?))
    }

    /// The messages of the current channel, none until they are loaded.
    pub fn messages(&self) -> Option<&[Message]> {
        let (server, chan) = self.channel()?;
        server.messages.get(&chan.info.id).map(Vec::as_slice)
    }

    fn count(&self) -> usize {
        self.messages().map_or(0, <[Message]>::len)
    }

    fn places(&self) -> Vec<(usize, usize)> {
        let teams = |(s, server): (usize, &Server)| (0..server.teams.len()).map(move |t| (s, t));
        self.servers.iter().enumerate().flat_map(teams).collect()
    }

    fn key(&mut self, key: KeyEvent) {
        self.notice.clear();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let plain = self.input.is_none()
            && self.finder.is_none()
            && self.help.is_none()
            && self.wizard.is_none();
        if let Some(pane) = &mut self.pane {
            pane.send_key(key);
        } else if key.code == KeyCode::Char('c') && ctrl {
            self.quit = true;
        } else if let Some(Awaiting::Delete { channel, id }) = self.awaiting.take() {
            if key.code == KeyCode::Char('y') {
                self.delete(channel, id);
            }
        } else if key.code == KeyCode::Char('g') && ctrl && (plain || self.compose.is_some()) {
            self.open_editor();
        } else if self.picker {
            self.picker_key(key);
        } else if self.opener.is_some() {
            self.opener_key(key);
        } else if self.compose.is_some() {
            self.compose_key(key);
        } else if self.finder.is_some() {
            self.finder_key(key);
        } else if self.input.is_some() {
            self.input_key(key);
        } else if self.wizard.is_none() {
            match self.help {
                Some(_) => self.help_key(key),
                None => self.normal_key(key),
            }
        }
    }

    fn normal_key(&mut self, key: KeyEvent) {
        if self.users.focus {
            return self.users_key(key);
        }
        if key.code == KeyCode::Esc {
            self.selecting = false;
            return;
        }
        let KeyCode::Char(c) = key.code else {
            return;
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return;
        }
        match c {
            'q' => self.quit = true,
            ':' => self.input = Some(Line::new(false)),
            '?' => self.help = Some(0),
            'j' => self.step_down(),
            'k' => self.step_up(),
            'g' => {
                self.move_cursor(0);
                self.load_older();
            }
            'G' => self.move_cursor(usize::MAX),
            'a' => self.reactions = !self.reactions,
            'n' => self.next_unread(),
            'N' => self.next_mention(),
            'c' => self.mark_read(),
            'o' => self.open_selected(),
            'x' => self.open_picker(),
            'X' => self.repeat_reaction(),
            'y' => self.yank(false),
            'Y' => self.yank(true),
            'p' => self.paste_register(),
            'V' => self.toggle_selection(),
            'm' => self.toggle_users(),
            'i' => self.start_compose(Kind::Message, None),
            'r' => self.reply(),
            'e' => self.edit(),
            'd' => self.ask_delete(),
            'f' | 'F' => {
                self.finder = Some(Finder {
                    all: c == 'F',
                    line: Line::new(false),
                    selected: 0,
                });
            }
            'w' => self.next_group(),
            'b' => self.previous_group(),
            'J' => self.move_channel(1),
            'K' => self.move_channel(-1),
            'H' => self.move_team(-1),
            'L' => self.move_team(1),
            _ => {}
        }
    }

    fn target(&self) -> Option<Target> {
        let (server, _) = self.channel()?;
        let message = self.messages()?.get(self.cursor)?;
        Some(Target {
            id: message.id.clone(),
            root: message.root.clone(),
            text: message.text.clone(),
            author: server.username(&message.user).to_owned(),
            own: server.me.as_ref().is_some_and(|m| m.id == message.user),
            system: message.system,
            mine: message
                .reactions
                .iter()
                .filter(|g| server.me.as_ref().is_some_and(|m| g.users.contains(&m.id)))
                .map(|g| g.name.clone())
                .collect(),
        })
    }

    fn draft_key(&self) -> Option<(usize, ChannelId)> {
        self.channel()
            .map(|(_, chan)| (self.server, chan.info.id.clone()))
    }

    fn start_compose(&mut self, kind: Kind, text: Option<String>) {
        let Some(key) = self.draft_key() else {
            return;
        };
        let draft = match kind {
            Kind::Edit(_) => None,
            _ => self.drafts.remove(&key),
        };
        let text = text.or(draft).unwrap_or_default();
        self.compose = Some(Compose {
            line: Line::with_text(&text, false),
            kind,
        });
        self.typed = None;
    }

    fn reply(&mut self) {
        let Some(target) = self.target().filter(|t| !t.system) else {
            return;
        };
        self.browsing = true;
        let root = target.root.unwrap_or(target.id);
        self.start_compose(
            Kind::Reply {
                root,
                to: target.author,
            },
            None,
        );
    }

    fn edit(&mut self) {
        let Some(target) = self.target() else {
            return;
        };
        self.browsing = true;
        if !target.own || target.system {
            self.notice = "That is not your message".into();
            return;
        }
        self.start_compose(Kind::Edit(target.id), Some(target.text));
    }

    fn ask_delete(&mut self) {
        let (Some(target), Some((_, chan))) = (self.target(), self.channel()) else {
            return;
        };
        let channel = chan.info.id.clone();
        self.browsing = true;
        if !target.own || target.system {
            self.notice = "That is not your message".into();
            return;
        }
        self.notice = "Delete this message?  y = yes, any other key = no".into();
        self.awaiting = Some(Awaiting::Delete {
            channel,
            id: target.id,
        });
    }

    fn delete(&mut self, channel: ChannelId, id: PostId) {
        let Some(client) = self.servers.get(self.server).and_then(|s| s.client.clone()) else {
            return;
        };
        self.spawn(self.server, async move {
            match client.delete_post(&id).await {
                Ok(()) => Loaded::Deleted { channel, id },
                Err(e) => Loaded::Failed(e.to_string()),
            }
        });
    }

    fn compose_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.leave_compose(),
            KeyCode::Enter => self.send(),
            _ => {
                let Some(compose) = &mut self.compose else {
                    return;
                };
                let before = compose.line.text();
                if compose.line.edit(key) && compose.line.text() != before {
                    self.ping_typing();
                }
            }
        }
    }

    fn open_editor(&mut self) {
        self.editor_fresh = self.compose.is_none();
        if self.editor_fresh {
            self.start_compose(Kind::Message, None);
        }
        let Some(compose) = &self.compose else {
            return;
        };
        let (_, rows) = term::split(self.size.1);
        let tx = self.tx.clone();
        let wake = move |done| {
            let _ = tx.send(if done { Msg::EditorDone } else { Msg::Pty });
        };
        let editor = self
            .settings
            .editor(std::env::var("EDITOR").ok().as_deref());
        match Pane::spawn(&editor, &compose.line.text(), rows, self.size.0, wake) {
            Ok(pane) => self.pane = Some(pane),
            Err(e) => {
                self.notice = format!("Editor: {e}");
                if self.editor_fresh {
                    self.leave_compose();
                }
            }
        }
    }

    fn editor_done(&mut self) {
        let Some(pane) = self.pane.take() else {
            return;
        };
        if !pane.is_viewer() {
            self.finished(pane.finish().map_err(|e| e.to_string()));
        }
    }

    /// Takes back what the editor saved, or says why it failed.
    fn finished(&mut self, result: Result<String, String>) {
        let fresh = std::mem::take(&mut self.editor_fresh);
        match result {
            Ok(text) if text.is_empty() && fresh => self.leave_compose(),
            Ok(text) => {
                if let Some(compose) = &mut self.compose {
                    compose.line = Line::with_text(&text, false);
                    self.ping_typing();
                }
            }
            Err(e) => {
                self.notice = format!("Editor: {e}");
                if fresh {
                    self.leave_compose();
                }
            }
        }
    }

    fn leave_compose(&mut self) {
        let (Some(compose), Some(key)) = (self.compose.take(), self.draft_key()) else {
            return;
        };
        self.typed = None;
        let text = compose.line.text();
        if compose.kind != Kind::Edit(PostId::default())
            && !matches!(compose.kind, Kind::Edit(_))
            && !text.is_empty()
        {
            self.drafts.insert(key, text);
        }
    }

    fn send(&mut self) {
        let Some(compose) = &self.compose else {
            return;
        };
        let text = compose.line.text();
        let message = text.trim().to_owned();
        if message.is_empty() {
            return;
        }
        if text.starts_with('/') {
            self.notice = "Slash commands are not supported".into();
            return;
        }
        let kind = compose.kind.clone();
        let (Some((server, chan)), i) = (self.channel(), self.server) else {
            return;
        };
        let Some(client) = server.client.clone() else {
            self.notice = format!("{}: not connected", server.name);
            return;
        };
        let channel = chan.info.id.clone();
        let edited = matches!(kind, Kind::Edit(_));
        if !edited {
            if let Some(server) = self.servers.get_mut(i) {
                server.mark_viewed(&channel);
            }
        }
        self.spawn(i, async move {
            let sent = match &kind {
                Kind::Edit(id) => client.edit_post(id, &message).await.map(Loaded::Edited),
                Kind::Message => client
                    .create_post(&channel, &message, None)
                    .await
                    .map(Loaded::Created),
                Kind::Reply { root, .. } => client
                    .create_post(&channel, &message, Some(root))
                    .await
                    .map(Loaded::Created),
            };
            if sent.is_ok() && !matches!(kind, Kind::Edit(_)) {
                let _ = client.view(&channel, None).await;
            }
            sent.unwrap_or_else(|e| match kind {
                Kind::Edit(_) => Loaded::Failed(e.to_string()),
                _ => Loaded::NotSent {
                    channel,
                    text: message,
                    error: e.to_string(),
                },
            })
        });
        self.compose = match edited {
            true => None,
            false => Some(Compose {
                line: Line::new(false),
                kind: Kind::Message,
            }),
        };
    }

    fn ping_typing(&mut self) {
        if self
            .typed
            .is_some_and(|at| at.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        let parent = match self.compose.as_ref().map(|c| &c.kind) {
            Some(Kind::Message) => None,
            Some(Kind::Reply { root, .. }) => Some(root.clone()),
            _ => return,
        };
        let Some((server, chan)) = self.channel() else {
            return;
        };
        let Some(actions) = &server.actions else {
            return;
        };
        let channel = chan.info.id.clone();
        if actions.send(Action::Typing { channel, parent }).is_ok() {
            self.typed = Some(Instant::now());
        }
    }

    fn paste(&mut self, text: &str) {
        if let Some(pane) = &mut self.pane {
            return pane.send_paste(text);
        }
        let flat = text.replace(['\r', '\n'], " ");
        if let Some(compose) = &mut self.compose {
            compose.line.insert_str(&flat);
            self.ping_typing();
        } else if let Some(finder) = &mut self.finder {
            finder.line.insert_str(&flat);
            finder.selected = 0;
        } else if let Some(line) = &mut self.input {
            line.insert_str(&flat);
        }
    }

    fn tick_later(&self) {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let tx = self.tx.clone();
            runtime.spawn(async move {
                tokio::time::sleep(Duration::from_secs(6)).await;
                let _ = tx.send(Msg::Tick);
            });
        }
    }

    /// Whether message `i` is shown as selected: the cursor, or the range being selected.
    pub fn is_selected(&self, i: usize) -> bool {
        let (low, high) = match self.selecting {
            true => (self.anchor.min(self.cursor), self.anchor.max(self.cursor)),
            false => (self.cursor, self.cursor),
        };
        self.browsing && (low..=high).contains(&i)
    }

    fn toggle_selection(&mut self) {
        if self.count() == 0 {
            return;
        }
        self.selecting = !self.selecting;
        self.anchor = self.cursor;
        self.browsing = true;
    }

    fn yank(&mut self, quote: bool) {
        let (Some((server, _)), Some(messages)) = (self.channel(), self.messages()) else {
            return;
        };
        if messages.is_empty() {
            return;
        }
        let last = messages.len() - 1;
        let (low, high) = match self.selecting {
            true => (self.anchor.min(self.cursor), self.anchor.max(self.cursor)),
            false => (self.cursor, self.cursor),
        };
        let chosen = &messages[low.min(last)..=high.min(last)];
        let lines: Vec<String> = chosen
            .iter()
            .map(|m| match quote {
                true => format!(
                    "[{}] {}: {}",
                    m.at.format("%H:%M"),
                    server.username(&m.user),
                    m.text
                ),
                false => m.text.clone(),
            })
            .collect();
        let count = lines.len();
        self.register = lines.join("\n");
        self.clipboard.push(clipboard::osc52(&self.register));
        self.copy_to_system();
        self.selecting = false;
        self.browsing = true;
        self.notice = match count {
            1 => "Copied 1 message".into(),
            n => format!("Copied {n} messages"),
        };
    }

    fn copy_to_system(&mut self) {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let x11 = std::env::var_os("DISPLAY").is_some();
        let setting = self.settings.copy.as_deref();
        let Some(command) = clipboard::tool(setting, wayland, x11, clipboard::on_path) else {
            return;
        };
        let tx = self.tx.clone();
        let done = move |outcome: Option<String>| {
            if let Some(why) = outcome {
                let _ = tx.send(Msg::Notice(format!("Copy failed: {why}")));
            }
        };
        if let Err(e) = clipboard::copy(&command, &self.register, done) {
            self.notice = format!("Copy failed: {e}");
        }
    }

    fn paste_register(&mut self) {
        if self.register.is_empty() {
            self.notice = "Nothing to paste: copy a message with y first".into();
            return;
        }
        if self.compose.is_none() {
            self.start_compose(Kind::Message, None);
        }
        if let Some(compose) = &mut self.compose {
            compose.line.insert_str(&self.register);
            self.ping_typing();
        }
    }

    fn open_selected(&mut self) {
        let Some(message) = self.messages().and_then(|m| m.get(self.cursor)) else {
            return;
        };
        let mut items: Vec<Item> = open::links(&message.text)
            .into_iter()
            .map(Item::Link)
            .collect();
        items.extend(message.files.iter().cloned().map(Item::File));
        self.browsing = true;
        match items.len() {
            0 => self.notice = "Nothing to open".into(),
            1 => self.open_item(items.remove(0)),
            _ => self.opener = Some(items),
        }
    }

    fn opener_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c @ '1'..='9') => {
                let index = usize::from(c as u8 - b'1');
                if let Some(item) = self
                    .opener
                    .as_ref()
                    .and_then(|items| items.get(index))
                    .cloned()
                {
                    self.opener = None;
                    self.open_item(item);
                }
            }
            KeyCode::Esc | KeyCode::Char('o' | 'q') => self.opener = None,
            _ => {}
        }
    }

    fn open_item(&mut self, item: Item) {
        match item {
            Item::Link(url) => self.start_program(&self.settings.open.browser.clone(), &url, &url),
            Item::File(file) => {
                let Some(client) = self.servers.get(self.server).and_then(|s| s.client.clone())
                else {
                    self.notice = "Not connected".into();
                    return;
                };
                self.notice = format!(
                    "Downloading {} ({})…",
                    file.name,
                    open::human_size(file.size)
                );
                let base = self.files.base().to_owned();
                let kind = open::kind(&file.mime, &file.name);
                self.spawn(self.server, async move {
                    let bytes = client.download(&file.id).await.map_err(|e| e.to_string());
                    let saved = bytes.and_then(|b| {
                        open::save(&base, &file.id, &file.name, &b).map_err(|e| e.to_string())
                    });
                    match saved {
                        Ok(path) => Loaded::Downloaded {
                            path,
                            name: file.name,
                            kind,
                        },
                        Err(e) => Loaded::Failed(e),
                    }
                });
            }
        }
    }

    fn start_program(&mut self, command: &str, target: &str, name: &str) {
        let tx = self.tx.clone();
        let done = move |outcome: Option<String>| {
            if let Some(why) = outcome {
                let _ = tx.send(Msg::Notice(why));
            }
        };
        match open::start(command, target, done) {
            Ok(()) => self.notice = format!("Opened {name}"),
            Err(e) => self.notice = format!("{command}: {e}"),
        }
    }

    fn downloaded(&mut self, path: &std::path::Path, name: &str, kind: FileKind) {
        let open = &self.settings.open;
        let command = match kind {
            FileKind::Image => open.image.clone(),
            FileKind::Video => open.video.clone(),
            FileKind::Text => open.text.clone(),
            FileKind::Other => open.other.clone(),
        };
        let target = path.to_string_lossy().into_owned();
        if kind != FileKind::Text {
            return self.start_program(&command, &target, name);
        }
        let (_, rows) = term::split(self.size.1);
        let tx = self.tx.clone();
        let wake = move |done| {
            let _ = tx.send(if done { Msg::EditorDone } else { Msg::Pty });
        };
        match Pane::view(&command, path, name, rows, self.size.0, wake) {
            Ok(pane) => self.pane = Some(pane),
            Err(e) => self.notice = format!("{command}: {e}"),
        }
    }

    fn open_picker(&mut self) {
        if self.target().is_some() {
            self.browsing = true;
            self.picker = true;
        }
    }

    fn picker_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c @ '1'..='9') => {
                self.picker = false;
                self.react(REACTIONS[usize::from(c as u8 - b'1')]);
            }
            KeyCode::Esc | KeyCode::Char('x' | 'q') => self.picker = false,
            _ => {}
        }
    }

    fn repeat_reaction(&mut self) {
        match self.last_reaction {
            Some(name) => self.react(name),
            None => self.notice = "No recent reaction: pick one with x".into(),
        }
    }

    fn react(&mut self, name: &'static str) {
        let Some(target) = self.target() else {
            return;
        };
        let Some((server, _)) = self.channel() else {
            return;
        };
        let (Some(client), Some(me)) = (
            server.client.clone(),
            server.me.as_ref().map(|m| m.id.clone()),
        ) else {
            self.notice = "Not connected".into();
            return;
        };
        self.browsing = true;
        self.last_reaction = Some(name);
        let post = target.id;
        let had = target.mine.iter().any(|n| n == name);
        self.spawn(self.server, async move {
            let done = match had {
                true => client.unreact(&me, &post, name).await.map(|()| {
                    Loaded::Unreacted(Reaction {
                        user_id: me.clone(),
                        post_id: post.clone(),
                        emoji_name: name.into(),
                    })
                }),
                false => client.react(&me, &post, name).await.map(Loaded::Reacted),
            };
            done.unwrap_or_else(|e| Loaded::Failed(e.to_string()))
        });
    }

    fn toggle_users(&mut self) {
        match (self.users.shown, self.users.focus) {
            (false, _) if self.size.0 < MIN_WIDTH_FOR_USERS => {
                self.notice = "Screen too narrow for the users list".into();
            }
            (false, _) => {
                self.users = Users {
                    shown: true,
                    focus: true,
                    top: 0,
                };
                if let Some(id) = self.channel().map(|(_, c)| c.info.id.clone()) {
                    if let Some(server) = self.servers.get_mut(self.server) {
                        server.members.remove(&id);
                        server.members_loading.remove(&id);
                    }
                }
                self.ensure_members();
            }
            (true, false) => self.users.focus = true,
            (true, true) => self.users = Users::default(),
        }
    }

    fn users_key(&mut self, key: KeyEvent) {
        let users = &mut self.users;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => users.focus = false,
            KeyCode::Char('m') => *users = Users::default(),
            KeyCode::Char('j') | KeyCode::Down => users.top += 1,
            KeyCode::Char('k') | KeyCode::Up => users.top = users.top.saturating_sub(1),
            KeyCode::PageDown => users.top += 10,
            KeyCode::PageUp => users.top = users.top.saturating_sub(10),
            KeyCode::Char('g') | KeyCode::Home => users.top = 0,
            KeyCode::Char('G') | KeyCode::End => users.top = usize::MAX / 2,
            _ => {}
        }
    }

    fn ensure_members(&mut self) {
        if !self.users.shown {
            return;
        }
        let Some((server, chan)) = self.channel() else {
            return;
        };
        let id = chan.info.id.clone();
        let Some(client) = server
            .client
            .clone()
            .filter(|_| !server.members.contains_key(&id))
        else {
            return;
        };
        let i = self.server;
        let Some(server) = self.servers.get_mut(i) else {
            return;
        };
        if !server.members_loading.insert(id.clone()) {
            return;
        }
        self.spawn(i, async move {
            match client.channel_users(&id).await {
                Ok(users) => {
                    let ids: Vec<UserId> = users.iter().map(|u| u.id.clone()).collect();
                    let statuses = client.statuses(&ids).await.unwrap_or_default();
                    Loaded::Members {
                        channel: id,
                        users,
                        statuses,
                    }
                }
                Err(e) => Loaded::Failed(e.to_string()),
            }
        });
    }

    /// The (server, team, channel) of every match of the finder, best first.
    fn finder_targets(&self) -> Vec<(usize, usize, usize)> {
        let Some(finder) = &self.finder else {
            return Vec::new();
        };
        let text = finder.line.text();
        let words: Vec<&str> = text.split_whitespace().collect();
        let here = (self.server, self.team);
        let candidates = finder::candidates(&self.servers, here, finder.all);
        let found = finder::entries(&words, &candidates, finder.all);
        found
            .iter()
            .map(|e| (e.candidate.server, e.candidate.team, e.candidate.chan))
            .collect()
    }

    fn finder_key(&mut self, key: KeyEvent) {
        let targets = self.finder_targets();
        let count = targets.len();
        let Some(finder) = &mut self.finder else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.finder = None,
            KeyCode::Enter => {
                let target = targets.get(finder.selected.min(count.saturating_sub(1)));
                let target = target.copied();
                self.finder = None;
                if let Some((server, team, chan)) = target {
                    (self.server, self.team) = (server, team);
                    self.go_channel(chan);
                }
            }
            KeyCode::Up | KeyCode::BackTab if count > 0 => {
                finder.selected = (finder.selected + count - 1) % count;
            }
            KeyCode::Down | KeyCode::Tab if count > 0 => {
                finder.selected = (finder.selected + 1) % count;
            }
            _ => {
                if finder.line.edit(key) {
                    finder.selected = 0;
                }
            }
        }
    }

    fn help_key(&mut self, key: KeyEvent) {
        let Some(top) = self.help else {
            return;
        };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.help = Some(top + 1),
            KeyCode::Char('k') | KeyCode::Up => self.help = Some(top.saturating_sub(1)),
            KeyCode::Esc | KeyCode::Char('q' | '?') => self.help = None,
            _ => {}
        }
    }

    fn input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.input = None;
                self.wizard = None;
            }
            KeyCode::Enter => {
                if let Some(line) = self.input.take() {
                    self.submit(&line.text());
                }
            }
            _ => {
                if let Some(line) = &mut self.input {
                    line.edit(key);
                }
            }
        }
    }

    fn submit(&mut self, text: &str) {
        if self.wizard.is_some() {
            return self.answer(text);
        }
        match (input::parse(text), text.trim()) {
            (_, "") => {}
            (Some(Command::NewServer), _) => self.new_server(),
            (Some(Command::Help), _) => self.help = Some(0),
            (Some(Command::Quit), _) => self.quit = true,
            (None, other) => self.notice = format!("Unknown command: {other}"),
        }
    }

    fn new_server(&mut self) {
        self.wizard = Some(Wizard::new());
        self.input = Some(Line::new(false));
    }

    fn answer(&mut self, text: &str) {
        let Some(wizard) = &mut self.wizard else {
            return;
        };
        let taken = wizard.step == Step::Name && self.servers.iter().any(|s| s.name == text.trim());
        if taken {
            self.notice = format!("A server named {} already exists", text.trim());
        } else if wizard.answer(text) {
            return self.login();
        }
        self.input = Some(Line::new(wizard.step.masked()));
    }

    fn login(&mut self) {
        let Some(wizard) = &self.wizard else {
            return;
        };
        let credentials = Credentials::Password {
            login: wizard.login().to_owned(),
            password: wizard.password().to_owned(),
        };
        let (url, mfa, tx) = (wizard.url().to_owned(), wizard.mfa.clone(), self.tx.clone());
        self.notice = "Logging in…".into();
        tokio::spawn(async move {
            let result = open(&url, credentials, mfa.as_deref()).await;
            let _ = tx.send(Msg::LoggedIn(result.map(Box::new)));
        });
    }

    fn logged_in(&mut self, result: Result<Box<Session>, Error>) {
        let Some(wizard) = &mut self.wizard else {
            return;
        };
        match result {
            Ok(session) => {
                let config = wizard.config();
                self.wizard = None;
                self.notice = format!("Logged in to {}", config.name);
                self.configs.push(config.clone());
                if let Err(e) = config::save(&self.path, &self.configs) {
                    self.notice = format!("Logged in, but the server list was not saved: {e}");
                }
                let mut server = Server::new(config.name);
                server.me = Some(session.me.clone());
                server.users.insert(session.me.id.clone(), session.me);
                server.client = Some(session.client);
                self.servers.push(server);
                self.server = self.servers.len() - 1;
                self.team = 0;
                self.reset_view();
                self.load_teams(self.server);
                let added = self.server;
                self.start_ws(added);
            }
            Err(Error::MfaRequired) => {
                wizard.step = Step::Mfa;
                self.notice = "A multi-factor code is required".into();
                self.input = Some(Line::new(true));
            }
            Err(e) => {
                self.notice = e.to_string();
                self.input = Some(Line::new(wizard.step.masked()));
            }
        }
    }

    fn reset_view(&mut self) {
        self.selecting = false;
        self.cursor = self.count().saturating_sub(1);
        self.browsing = false;
        self.follow = false;
        self.scroll = usize::MAX;
    }

    fn move_cursor(&mut self, to: usize) {
        let count = self.count();
        if count > 0 {
            self.cursor = to.min(count - 1);
            self.browsing = true;
            self.follow = true;
        }
    }

    fn step_up(&mut self) {
        match (self.browsing, self.cursor) {
            (true, 0) => self.load_older(),
            (true, c) => self.move_cursor(c - 1),
            (false, _) => self.browsing = self.count() > 0,
        }
    }

    fn load_older(&mut self) {
        if self.cursor != 0 {
            return;
        }
        let Some((server, chan)) = self.channel() else {
            return;
        };
        let id = chan.info.id.clone();
        let Some(messages) = server.messages.get(&id) else {
            return;
        };
        let (Some(before), len) = (messages.first().map(|m| m.id.clone()), messages.len()) else {
            return;
        };
        if server.complete.contains(&id) {
            self.notice = "Start of the channel".into();
            return;
        }
        if len >= model::MAX_MESSAGES {
            self.notice = format!("Showing the last {} messages", model::MAX_MESSAGES);
            return;
        }
        let asked = model::PAGE.min(model::MAX_MESSAGES - len);
        let Some(server) = self.servers.get_mut(self.server) else {
            return;
        };
        let Some(client) = server.client.clone() else {
            return;
        };
        if !server.paging.insert(id.clone()) {
            return;
        }
        self.notice = "Loading older messages…".into();
        self.spawn(self.server, async move {
            let list = client.posts(&id, Some(&before), asked as u32).await;
            Loaded::Older {
                channel: id,
                before,
                list: list.map_err(|e| e.to_string()),
                asked,
            }
        });
    }

    fn step_down(&mut self) {
        match (self.browsing, self.cursor + 1 >= self.count()) {
            (false, _) => {}
            (true, true) => self.browsing = self.selecting,
            (true, false) => self.move_cursor(self.cursor + 1),
        }
    }

    fn next_group(&mut self) {
        let Some(messages) = self.messages() else {
            return;
        };
        let mut i = self.cursor + 1;
        while i < messages.len() && group_with_previous(&messages[i - 1], &messages[i]) {
            i += 1;
        }
        self.move_cursor(i);
    }

    fn previous_group(&mut self) {
        let Some(messages) = self.messages().filter(|m| !m.is_empty()) else {
            return;
        };
        let here = self.cursor.min(messages.len() - 1);
        let start = group_start(messages, here);
        let target = match (start < self.cursor, self.cursor) {
            (true, _) => start,
            (false, 0) => return,
            (false, c) => group_start(messages, c - 1),
        };
        self.move_cursor(target);
    }

    fn move_channel(&mut self, step: isize) {
        let Some(team) = self
            .servers
            .get(self.server)
            .and_then(|s| s.teams.get(self.team))
        else {
            return;
        };
        let Some(count) = team.chans.as_ref().map(Vec::len).filter(|&n| n > 0) else {
            return;
        };
        self.go_channel(team.cur.saturating_add_signed(step).min(count - 1));
    }

    fn go_channel(&mut self, index: usize) {
        if let Some(team) = self
            .servers
            .get_mut(self.server)
            .and_then(|s| s.teams.get_mut(self.team))
        {
            team.cur = index;
        }
        self.reset_view();
        self.ensure();
    }

    fn move_team(&mut self, step: isize) {
        let places = self.places();
        let Some(here) = places.iter().position(|&p| p == (self.server, self.team)) else {
            return;
        };
        let next = here.saturating_add_signed(step).min(places.len() - 1);
        (self.server, self.team) = places[next];
        self.reset_view();
        self.ensure();
    }

    fn current_id(&self, i: usize) -> Option<ChannelId> {
        if i != self.server {
            return None;
        }
        self.channel().map(|(_, chan)| chan.info.id.clone())
    }

    fn loaded(&mut self, i: usize, loaded: Loaded) {
        let loaded = match loaded {
            Loaded::Created(post) => {
                if self.current_id(i).as_ref() == Some(&post.channel_id) {
                    self.reset_view();
                }
                let mentions = Vec::new();
                return self.apply_event(i, Event::Posted { post, mentions });
            }
            Loaded::Edited(post) => return self.apply_event(i, Event::Edited(post)),
            Loaded::Reacted(reaction) => {
                let glyph = crate::markdown::emoji(&format!(":{}:", reaction.emoji_name));
                self.notice = format!("Reacted {glyph}");
                self.reactions = true;
                return self.apply_event(i, Event::ReactionAdded(reaction));
            }
            Loaded::Unreacted(reaction) => {
                let glyph = crate::markdown::emoji(&format!(":{}:", reaction.emoji_name));
                self.notice = format!("Removed {glyph}");
                self.reactions = true;
                return self.apply_event(i, Event::ReactionRemoved(reaction));
            }
            Loaded::Deleted { channel, id } => {
                let post = Post {
                    id,
                    channel_id: channel,
                    ..Post::default()
                };
                return self.apply_event(i, Event::Deleted(post));
            }
            Loaded::NotSent {
                channel,
                text,
                error,
            } => {
                let kept = self
                    .drafts
                    .remove(&(i, channel.clone()))
                    .unwrap_or_default();
                let draft = format!("{text} {kept}").trim_end().to_owned();
                self.drafts.insert((i, channel), draft);
                let name = self.servers.get(i).map_or("", |s| s.name.as_str());
                self.notice = format!("{name}: {error} (kept as a draft)");
                return;
            }
            other => other,
        };
        let current = self.current_id(i);
        let Some(server) = self.servers.get_mut(i) else {
            return;
        };
        match loaded {
            Loaded::Connected(Ok(session)) => {
                server.me = Some(session.me.clone());
                server.users.insert(session.me.id.clone(), session.me);
                server.client = Some(session.client);
                self.load_teams(i);
                self.start_ws(i);
            }
            Loaded::Connected(Err(e)) | Loaded::Failed(e) => {
                self.notice = format!("{}: {e}", server.name);
            }
            Loaded::Teams(teams) => {
                server.teams = teams
                    .into_iter()
                    .map(|info| Team {
                        info,
                        chans: None,
                        loading: false,
                        cur: 0,
                    })
                    .collect();
                let ids: Vec<TeamId> = server
                    .teams
                    .iter_mut()
                    .map(|t| {
                        t.loading = true;
                        t.info.id.clone()
                    })
                    .collect();
                for id in ids {
                    self.fetch_channels(i, id);
                }
            }
            Loaded::Channels {
                team,
                channels,
                members,
                categories,
            } => {
                let chans = model::order_channels(channels, members, categories.as_ref());
                if debug::on() {
                    for chan in &chans {
                        let (total, read, mentions) = (
                            chan.info.total_msg_count,
                            chan.member.as_ref().map(|m| m.msg_count),
                            chan.member.as_ref().map(|m| m.mention_count),
                        );
                        debug::log(&format!(
                            "loaded {} total={total} read={read:?} mentions={mentions:?} unread={}",
                            chan.info.id.0,
                            chan.unread()
                        ));
                    }
                }
                let others: Vec<UserId> = chans
                    .iter()
                    .filter_map(|c| server.other_of(&c.info))
                    .collect();
                let ids = server.unknown(others.iter());
                if let Some(t) = server.teams.iter_mut().find(|t| t.info.id == team) {
                    let kept = t.chans.as_ref().and_then(|c| c.get(t.cur));
                    let kept = kept.map(|c| c.info.id.clone());
                    t.cur = kept
                        .and_then(|id| chans.iter().position(|c| c.info.id == id))
                        .unwrap_or(0);
                    t.chans = Some(chans);
                    t.loading = false;
                }
                self.fetch_users(i, ids);
            }
            Loaded::Posts { channel, list } => {
                let messages = model::messages_of(&list);
                let ids = server.unknown(messages.iter().map(|m| &m.user));
                server.loading.remove(&channel);
                let selected = (current.as_ref() == Some(&channel) && self.browsing)
                    .then(|| server.messages.get(&channel)?.get(self.cursor))
                    .flatten()
                    .map(|m| m.id.clone());
                let cached = server.messages.entry(channel.clone()).or_default();
                let kept = model::replace_window(cached, messages);
                if let Some(at) = selected.and_then(|id| cached.iter().position(|m| m.id == id)) {
                    self.cursor = at;
                }
                if !kept {
                    server.complete.remove(&channel);
                }
                if list.order.len() < model::PAGE {
                    server.complete.insert(channel);
                }
                self.fetch_users(i, ids);
            }
            Loaded::Older {
                channel,
                before,
                list,
                asked,
            } => {
                server.paging.remove(&channel);
                match list {
                    Err(e) => self.notice = format!("{}: {e}", server.name),
                    Ok(list) => {
                        let page = model::messages_of(&list);
                        let ids = server.unknown(page.iter().map(|m| &m.user));
                        let intact = server
                            .messages
                            .get(&channel)
                            .and_then(|m| m.first())
                            .is_some_and(|m| m.id == before);
                        if intact {
                            if list.order.len() < asked {
                                server.complete.insert(channel.clone());
                            }
                            let here = current.as_ref() == Some(&channel);
                            let cached = server.messages.entry(channel).or_default();
                            let selected = here
                                .then(|| cached.get(self.cursor))
                                .flatten()
                                .map(|m| m.id.clone());
                            model::merge_page(cached, page);
                            if let Some(id) = selected {
                                let at = cached.iter().position(|m| m.id == id).unwrap_or(0);
                                self.cursor = if id == before {
                                    at.saturating_sub(1)
                                } else {
                                    at
                                };
                                self.follow = true;
                            }
                        }
                        self.fetch_users(i, ids);
                    }
                }
            }
            Loaded::Created(_)
            | Loaded::Edited(_)
            | Loaded::Reacted(_)
            | Loaded::Unreacted(_)
            | Loaded::Deleted { .. }
            | Loaded::NotSent { .. }
            | Loaded::Viewed => {}
            Loaded::Downloaded { path, name, kind } => {
                self.downloaded(&path, &name, kind);
            }
            Loaded::Members {
                channel,
                users,
                statuses,
            } => {
                server.members_loading.remove(&channel);
                let ids = users.iter().map(|u| u.id.clone()).collect();
                server
                    .users
                    .extend(users.into_iter().map(|u| (u.id.clone(), u)));
                for entry in statuses {
                    server
                        .status
                        .insert(entry.user_id, Status::parse(&entry.status));
                }
                server.members.insert(channel, ids);
            }
            Loaded::Users(users) => {
                server
                    .users
                    .extend(users.into_iter().map(|u| (u.id.clone(), u)));
            }
        }
        if i == self.server {
            self.cursor = self.cursor.min(self.count().saturating_sub(1));
            if self.scroll == usize::MAX && !self.browsing {
                self.reset_view();
            }
            self.ensure();
        }
    }

    fn ws(&mut self, i: usize, ws: Ws) {
        let Some(server) = self.servers.get_mut(i) else {
            return;
        };
        match ws {
            Ws::Up { resync } => {
                server.link = Link::Up;
                if resync {
                    self.resync(i);
                }
            }
            Ws::Down(reason) => {
                server.link = Link::Down;
                if self.notice.is_empty() {
                    self.notice = format!("{}: {reason}", server.name);
                }
            }
            Ws::Event(event) => self.apply_event(i, *event),
        }
    }

    fn resync(&mut self, i: usize) {
        let current = self.current_id(i);
        let Some(server) = self.servers.get_mut(i) else {
            return;
        };
        server.messages.retain(|id, _| Some(id) == current.as_ref());
        server.complete.retain(|id| Some(id) == current.as_ref());
        server.members.clear();
        server.members_loading.clear();
        self.refresh_channels(i);
        if let Some(id) = current {
            self.fetch_posts(i, id);
        }
        self.ensure_members();
    }

    fn refresh_channels(&self, i: usize) {
        let Some(server) = self.servers.get(i) else {
            return;
        };
        let loaded = server.teams.iter().filter(|t| t.chans.is_some());
        for team in loaded {
            self.fetch_channels(i, team.info.id.clone());
        }
    }

    fn apply_event(&mut self, i: usize, event: Event) {
        let current = self.current_id(i);
        let before = self.count();
        let at_end = !self.browsing || self.cursor + 1 >= before;
        let Some(server) = self.servers.get_mut(i) else {
            return;
        };
        let me = server.me.as_ref().map(|m| m.id.clone());
        let (mut touched, mut removed, mut refresh, mut view) = (None, None, false, None);
        let mut typed = false;
        let (mut added, mut dropped) = (false, 0);
        let mut ids = Vec::new();
        match event {
            Event::Posted { post, mentions } => {
                let channel = post.channel_id.clone();
                let mine = me.as_ref() == Some(&post.user_id);
                let mentioned = !mine && me.as_ref().is_some_and(|m| mentions.contains(m));
                let mut known = false;
                let fresh = server
                    .messages
                    .get(&channel)
                    .is_none_or(|l| l.iter().all(|m| m.id != post.id));
                let chans = server
                    .teams
                    .iter_mut()
                    .flat_map(|t| t.chans.iter_mut().flatten());
                for chan in chans.filter(|c| c.info.id == channel) {
                    known = true;
                    chan.info.last_post_at = chan.info.last_post_at.max(post.create_at);
                    if fresh {
                        chan.info.total_msg_count += 1;
                    }
                    if let (true, Some(member)) = (mentioned, chan.member.as_mut()) {
                        member.mention_count += 1;
                    }
                }
                if fresh && (mine || current.as_ref() == Some(&channel)) {
                    server.mark_viewed(&channel);
                }
                if fresh && !mine && current.as_ref() == Some(&channel) {
                    view = Some(channel.clone());
                }
                server.stopped(&channel, &post.user_id);
                refresh = !known && server.refreshed.insert(channel.clone());
                ids = server.unknown(std::iter::once(&post.user_id));
                if let Some(list) = server.messages.get_mut(&channel) {
                    added = list.iter().all(|m| m.id != post.id);
                    model::insert_message(list, Message::from_post(&post));
                    dropped = model::cap(list, model::MAX_MESSAGES);
                }
                if dropped > 0 {
                    server.complete.remove(&channel);
                }
                touched = Some(channel);
            }
            Event::Edited(post) => {
                if let Some(list) = server.messages.get_mut(&post.channel_id) {
                    model::edit_message(list, &post);
                }
            }
            Event::Deleted(post) => {
                if let Some(list) = server.messages.get_mut(&post.channel_id) {
                    removed = model::remove_message(list, &post.id);
                }
                touched = Some(post.channel_id);
            }
            Event::ReactionAdded(reaction) => {
                server
                    .messages
                    .values_mut()
                    .for_each(|l| model::react(l, &reaction));
            }
            Event::ReactionRemoved(reaction) => {
                server
                    .messages
                    .values_mut()
                    .for_each(|l| model::unreact(l, &reaction));
            }
            Event::Viewed(channel) => server.mark_viewed(&channel),
            Event::Status { user, status } => {
                server.status.insert(user, status);
            }
            Event::Typing { channel, user } => {
                if me.as_ref() != Some(&user) {
                    server.typed(&channel, &user, Instant::now());
                    typed = true;
                }
            }
        }
        if refresh {
            self.refresh_channels(i);
        }
        if typed {
            self.tick_later();
        }
        if let Some(channel) = view.filter(|c| current.as_ref() == Some(c)) {
            self.send_view(i, channel, None);
        }
        self.fetch_users(i, ids);
        if touched.is_some() && touched == current {
            let after = self.count();
            match removed {
                Some(at) => {
                    self.cursor = self.cursor.saturating_sub(usize::from(at < self.cursor));
                    self.cursor = self.cursor.min(after.saturating_sub(1));
                }
                None if added && at_end => {
                    self.cursor = after - 1;
                    match self.browsing {
                        true => self.follow = true,
                        false => self.scroll = usize::MAX,
                    }
                }
                None => self.cursor = self.cursor.saturating_sub(dropped),
            }
        }
        if i == self.server {
            self.ensure();
        }
    }

    fn ensure(&mut self) {
        let (i, t) = (self.server, self.team);
        let Some(server) = self.servers.get_mut(i) else {
            return;
        };
        let has_client = server.client.is_some();
        let Some(team) = server.teams.get_mut(t) else {
            return;
        };
        if team.chans.is_none() {
            if has_client && !team.loading {
                team.loading = true;
                let id = team.info.id.clone();
                self.fetch_channels(i, id);
            }
            return;
        }
        let chan = team.chans.as_ref().and_then(|c| c.get(team.cur));
        let Some(id) = chan.map(|c| c.info.id.clone()) else {
            return;
        };
        let missing = !server.messages.contains_key(&id);
        self.enter();
        if missing {
            self.fetch_posts(i, id);
        }
        self.place();
    }

    fn enter(&mut self) {
        let Some((_, chan)) = self.channel() else {
            return;
        };
        let (id, unread) = (chan.info.id.clone(), chan.unread_count());
        let same = |e: &Entry| e.server == self.server && e.channel == id;
        if self.entry.as_ref().is_some_and(same) {
            return;
        }
        if debug::on() {
            debug::log(&format!("enter {} unread={unread}", id.0));
        }
        let old = self.entry.take();
        if let Some(e) = &old {
            if let Some(server) = self.servers.get_mut(e.server) {
                server.trim(&e.channel);
            }
        }
        let prev = old.filter(|e| e.server == self.server).map(|e| e.channel);
        let goal = std::mem::take(&mut self.goal);
        self.entry = Some(Entry {
            server: self.server,
            channel: id.clone(),
            unread,
            boundary: None,
            goal,
            placed: false,
        });
        if let Some(server) = self.servers.get_mut(self.server) {
            server.mark_viewed(&id);
        }
        self.send_view(self.server, id, prev);
    }

    fn place(&mut self) {
        let Some(entry) = self.entry.as_ref().filter(|e| !e.placed) else {
            return;
        };
        let (unread, goal) = (entry.unread, entry.goal);
        let Some((server, chan)) = self.channel() else {
            return;
        };
        let (Some(messages), Some(me)) = (server.messages.get(&chan.info.id), server.me.as_ref())
        else {
            return;
        };
        let boundary = unread::first_unread(messages, unread);
        let start = messages.len().saturating_sub(unread);
        let mention = || {
            let found = unread::first_mention(messages, start, None, chan.info.kind, me);
            found.filter(|_| unread > 0)
        };
        let target = match goal {
            Goal::Mention => mention().or(boundary),
            Goal::Unread => boundary,
        };
        let boundary = boundary.map(|i| messages[i].id.clone());
        if let Some(entry) = &mut self.entry {
            entry.placed = true;
            entry.boundary = boundary;
        }
        match target {
            Some(index) => self.move_cursor(index),
            None => self.reset_view(),
        }
    }

    fn send_view(&self, i: usize, channel: ChannelId, prev: Option<ChannelId>) {
        let Some(client) = self.servers.get(i).and_then(|s| s.client.clone()) else {
            return;
        };
        self.spawn(i, async move {
            let sent = client.view(&channel, prev.as_ref()).await;
            if debug::on() {
                let outcome = sent
                    .as_ref()
                    .map_or_else(ToString::to_string, |()| "ok".into());
                let before = prev.as_ref().map(|p| p.0.as_str());
                debug::log(&format!("view {} prev={before:?}: {outcome}", channel.0));
                let info = client.inspect(&format!("/channels/{}", channel.0)).await;
                let member = client
                    .inspect(&format!("/channels/{}/members/me", channel.0))
                    .await;
                let (channel_keys, member_keys) = (
                    ["last_post_at", "total_msg_count", "total_msg_count_root"],
                    [
                        "last_viewed_at",
                        "msg_count",
                        "msg_count_root",
                        "mention_count",
                    ],
                );
                debug::log(&format!(
                    "  server now: {} | {}",
                    debug::pick(&info, &channel_keys),
                    debug::pick(&member, &member_keys)
                ));
            }
            match sent {
                Ok(()) => Loaded::Viewed,
                Err(e) => Loaded::Failed(e.to_string()),
            }
        });
    }

    /// The first post that was unread when the open channel was entered, unless it is the first shown.
    pub fn unread_boundary(&self) -> Option<&PostId> {
        self.entry.as_ref()?.boundary.as_ref()
    }

    fn unread_start(&self) -> Option<usize> {
        let entry = self.entry.as_ref().filter(|e| e.unread > 0)?;
        let messages = self.messages()?;
        let boundary = entry.boundary.as_ref();
        Some(
            boundary
                .and_then(|id| messages.iter().position(|m| &m.id == id))
                .unwrap_or(0),
        )
    }

    fn next_unread(&mut self) {
        let first = self.unread_start().filter(|&f| f > 0);
        if let Some(first) = first.filter(|&f| !self.browsing || self.cursor < f) {
            return self.move_cursor(first);
        }
        let Some(team) = self
            .servers
            .get(self.server)
            .and_then(|s| s.teams.get(self.team))
        else {
            return;
        };
        let Some(chans) = &team.chans else {
            return;
        };
        match unread::next_marked(chans.len(), team.cur, |i| {
            i != team.cur && chans[i].unread()
        }) {
            Some(index) => self.go_channel(index),
            None => self.notice = "No other unread channel in this team".into(),
        }
    }

    fn next_mention(&mut self) {
        let here = self.channel().and_then(|(server, chan)| {
            let (messages, me) = (server.messages.get(&chan.info.id)?, server.me.as_ref()?);
            let after = self.browsing.then_some(self.cursor);
            let start = self.unread_start()?;
            unread::first_mention(messages, start, after, chan.info.kind, me)
        });
        if let Some(index) = here {
            return self.move_cursor(index);
        }
        let current = self.channel().map(|(_, chan)| chan.info.id.clone());
        let mut places = Vec::new();
        let mut here = 0;
        for (s, server) in self.servers.iter().enumerate() {
            for (t, team) in server.teams.iter().enumerate() {
                for (c, chan) in team.chans.iter().flatten().enumerate() {
                    let open = s == self.server && Some(&chan.info.id) == current.as_ref();
                    if (s, t, c) == (self.server, self.team, team.cur) {
                        here = places.len();
                    }
                    places.push((s, t, c, chan.mentions() && !open));
                }
            }
        }
        match unread::next_marked(places.len(), here, |i| places[i].3) {
            Some(i) => {
                let (s, t, c, _) = places[i];
                (self.server, self.team) = (s, t);
                self.goal = Goal::Mention;
                self.go_channel(c);
            }
            None => self.notice = "No unread mention".into(),
        }
    }

    fn mark_read(&mut self) {
        let Some((_, chan)) = self.channel() else {
            return;
        };
        let id = chan.info.id.clone();
        if let Some(entry) = &mut self.entry {
            entry.unread = 0;
            entry.boundary = None;
        }
        if let Some(server) = self.servers.get_mut(self.server) {
            server.mark_viewed(&id);
        }
        self.send_view(self.server, id, None);
        self.notice = "Marked as read".into();
    }

    fn fetch_channels(&self, i: usize, team: TeamId) {
        let Some(client) = self.servers.get(i).and_then(|s| s.client.clone()) else {
            return;
        };
        self.spawn(i, async move {
            let channels = client.channels(&team).await;
            let members = client.members(&team).await;
            let categories = client.categories(&team).await.ok();
            match (channels, members) {
                (Ok(channels), Ok(members)) => Loaded::Channels {
                    team,
                    channels,
                    members,
                    categories,
                },
                (Err(e), _) | (_, Err(e)) => Loaded::Failed(e.to_string()),
            }
        });
    }

    fn fetch_posts(&mut self, i: usize, id: ChannelId) {
        let Some(server) = self.servers.get_mut(i) else {
            return;
        };
        let Some(client) = server.client.clone() else {
            return;
        };
        if !server.loading.insert(id.clone()) {
            return;
        }
        self.spawn(i, async move {
            match client.posts(&id, None, model::PAGE as u32).await {
                Ok(list) => Loaded::Posts { channel: id, list },
                Err(e) => Loaded::Failed(e.to_string()),
            }
        });
    }

    fn start_ws(&mut self, i: usize) {
        let tx = self.tx.clone();
        let Some(server) = self.servers.get_mut(i) else {
            return;
        };
        let Some(client) = server.client.clone() else {
            return;
        };
        let (actions, receiver) = unbounded_channel();
        server.actions = Some(actions);
        tokio::spawn(ws::run(client, ws::BACKOFF, receiver, move |event| {
            tx.send(Msg::Ws(i, event)).is_ok()
        }));
    }

    fn connect(&self, i: usize, url: &str, credentials: Credentials) {
        let url = url.to_owned();
        self.spawn(i, async move {
            match open(&url, credentials, None).await {
                Ok(session) => Loaded::Connected(Ok(Box::new(session))),
                Err(e) => Loaded::Connected(Err(e.to_string())),
            }
        });
    }

    fn load_teams(&self, i: usize) {
        let Some(client) = self.servers.get(i).and_then(|s| s.client.clone()) else {
            return;
        };
        self.spawn(i, async move {
            match client.teams().await {
                Ok(teams) => Loaded::Teams(teams),
                Err(e) => Loaded::Failed(e.to_string()),
            }
        });
    }

    fn fetch_users(&self, i: usize, ids: Vec<UserId>) {
        let Some(client) = self.servers.get(i).and_then(|s| s.client.clone()) else {
            return;
        };
        for chunk in ids.chunks(100) {
            let (client, chunk) = (client.clone(), chunk.to_vec());
            self.spawn(i, async move {
                match client.users(&chunk).await {
                    Ok(users) => Loaded::Users(users),
                    Err(e) => Loaded::Failed(e.to_string()),
                }
            });
        }
    }

    fn spawn(&self, server: usize, work: impl Future<Output = Loaded> + Send + 'static) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Msg::Loaded(server, work.await));
        });
    }
}

async fn open(url: &str, credentials: Credentials, mfa: Option<&str>) -> Result<Session, Error> {
    let client = Client::connect(url, credentials, mfa).await?;
    let me = client.me().await?;
    Ok(Session {
        client: Arc::new(client),
        me,
    })
}

#[cfg(test)]
pub mod tests {
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState};
    use serde::de::DeserializeOwned;
    use std::sync::Arc;
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;
    use crate::api::types::{Post, PostList, Reaction, Status};
    use crate::api::Client;
    use crate::ui::tests::screen;
    use unicode_width::UnicodeWidthStr;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    pub fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            app.update(Msg::Key(key(KeyCode::Char(c))));
        }
    }

    pub fn press_code(app: &mut App, code: KeyCode) {
        app.update(Msg::Key(key(code)));
    }

    pub fn fixture() -> App {
        let (app, rx) = fixture_rx();
        std::mem::forget(rx);
        app
    }

    fn fixture_rx() -> (App, tokio::sync::mpsc::UnboundedReceiver<Msg>) {
        let (tx, rx) = unbounded_channel();
        let config = ServerConfig {
            name: "work".into(),
            url: "https://chat.example.org".into(),
            login: "paul".into(),
            password: None,
            password_cmd: None,
            token: Some("t".into()),
            token_cmd: None,
        };
        let mut app = App::new(
            vec![(config, Err("offline".into()))],
            PathBuf::from("/nonexistent/servers.toml"),
            tx,
        );
        app.notice.clear();
        app.settings.copy = Some("true".into());
        app.settings.open = crate::config::Open {
            image: "true".into(),
            video: "true".into(),
            browser: "true".into(),
            text: "true".into(),
            other: "true".into(),
        };
        app.servers[0].me = Some(json_user("u1", "paul"));
        app.loaded(
            0,
            Loaded::Users(vec![json_user("u1", "paul"), json_user("u2", "ana")]),
        );
        app.loaded(
            0,
            Loaded::Teams(vec![serde_json::from_value(serde_json::json!(
                {"id": "t1", "name": "eng", "display_name": "Engineering"}
            ))
            .unwrap()]),
        );
        app.loaded(
            0,
            Loaded::Channels {
                team: TeamId("t1".into()),
                channels: json("channels"),
                members: json("members"),
                categories: Some(json("categories")),
            },
        );
        let list: PostList = json("posts");
        app.loaded(
            0,
            Loaded::Posts {
                channel: ChannelId("c3".into()),
                list,
            },
        );
        (app, rx)
    }

    fn json<T: DeserializeOwned>(name: &str) -> T {
        let text = std::fs::read_to_string(format!("tests/fixtures/{name}.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn json_user(id: &str, name: &str) -> User {
        serde_json::from_value(serde_json::json!({"id": id, "username": name})).unwrap()
    }

    #[test]
    fn first_k_reveals_and_j_from_the_latest_hides() {
        let mut app = fixture();
        assert!(!app.browsing);
        press(&mut app, "k");
        assert!(app.browsing);
        assert_eq!(app.cursor, 1);
        press(&mut app, "k");
        assert_eq!(app.cursor, 0);
        press(&mut app, "jj");
        assert!(!app.browsing);
    }

    #[test]
    fn g_and_capital_g_reach_the_ends() {
        let mut app = fixture();
        press(&mut app, "g");
        assert_eq!(app.cursor, 0);
        press(&mut app, "G");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn q_quits_but_closes_help_first() {
        let mut app = fixture();
        press(&mut app, "?q");
        assert!(!app.quit);
        press(&mut app, "q");
        assert!(app.quit);
    }

    #[test]
    fn unknown_commands_are_reported_and_quit_works() {
        let mut app = fixture();
        press(&mut app, ":nope");
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(app.notice, "Unknown command: nope");
        press(&mut app, ":quit");
        press_code(&mut app, KeyCode::Enter);
        assert!(app.quit);
    }

    #[test]
    fn channel_keys_stop_at_the_ends() {
        let mut app = fixture();
        press(&mut app, "K");
        assert_eq!(app.servers[0].teams[0].cur, 0);
        press(&mut app, "JJJJJJ");
        assert_eq!(app.servers[0].teams[0].cur, 2);
    }

    async fn walk(app: &mut App, answers: &[&str]) {
        for answer in answers {
            press(app, answer);
            press_code(app, KeyCode::Enter);
        }
    }

    async fn mock() -> wiremock::MockServer {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let user = serde_json::json!({"id": "u1", "username": "paul"});
        Mock::given(path("/api/v4/users/login"))
            .and(body_partial_json(serde_json::json!({"token": "123456"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Token", "tok")
                    .set_body_json(&user),
            )
            .mount(&server)
            .await;
        Mock::given(path("/api/v4/users/login"))
            .and(body_partial_json(serde_json::json!({"login_id": "paul", "password": "hunter2"})))
            .and(wiremock::matchers::header_exists("content-type"))
            .respond_with(ResponseTemplate::new(401).set_body_json(
                serde_json::json!({"id": "mfa.validate_token.authenticate.app_error", "message": "mfa"}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/users/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&user))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn the_wizard_asks_for_the_mfa_code_then_saves_the_server() {
        let server = mock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.toml");
        let (tx, mut rx) = unbounded_channel();
        let mut app = App::new(Vec::new(), path.clone(), tx);
        assert_eq!(app.mode(), "LOGIN");
        walk(&mut app, &["home", &server.uri(), "paul", "hunter2"]).await;

        let msg = rx.recv().await.unwrap();
        app.update(msg);
        assert_eq!(app.prompt(), "MFA code: ");
        assert!(app.servers.is_empty());

        walk(&mut app, &["123456"]).await;
        let msg = rx.recv().await.unwrap();
        app.update(msg);
        assert_eq!(app.notice, "Logged in to home");
        assert_eq!(app.mode(), "NORMAL");
        assert_eq!(app.servers[0].me.as_ref().unwrap().username, "paul");
        let saved = config::load(&path).unwrap();
        assert_eq!(saved[0].name, "home");
        assert_eq!(saved[0].password.as_deref(), Some("hunter2"));
    }

    #[tokio::test]
    async fn a_refused_login_asks_for_the_password_again() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/api/v4/users/login"))
            .respond_with(wiremock::ResponseTemplate::new(401).set_body_json(
                serde_json::json!({"id": "api.user.login.invalid", "message": "Wrong password."}),
            ))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = unbounded_channel();
        let mut app = App::new(Vec::new(), dir.path().join("servers.toml"), tx);
        walk(&mut app, &["home", &server.uri(), "paul", "wrong"]).await;
        let msg = rx.recv().await.unwrap();
        app.update(msg);
        assert!(app.notice.contains("Wrong password."));
        assert_eq!(app.prompt(), "Password: ");
        assert!(app.servers.is_empty());
    }

    pub fn post(id: &str, channel: &str, user: &str, text: &str, ms: i64) -> Post {
        serde_json::from_value(serde_json::json!({
            "id": id, "create_at": ms, "user_id": user, "channel_id": channel, "message": text
        }))
        .unwrap()
    }

    pub fn posted(app: &mut App, post: Post, mentions: &[&str]) {
        let mentions = mentions.iter().map(|m| UserId((*m).into())).collect();
        app.apply_event(0, Event::Posted { post, mentions });
    }

    fn reaction(user: &str, post: &str) -> Reaction {
        serde_json::from_value(serde_json::json!({
            "user_id": user, "post_id": post, "emoji_name": "tada"
        }))
        .unwrap()
    }

    fn text(app: &mut App) -> String {
        screen(app, 80, 14).join("\n")
    }

    fn tabs(app: &mut App) -> String {
        screen(app, 80, 14).remove(0)
    }

    #[test]
    fn a_new_post_in_the_current_channel_appears_and_the_view_follows() {
        let mut app = fixture();
        posted(
            &mut app,
            post("p10", "c3", "u2", "late news", 1_700_000_300_000),
            &[],
        );
        assert!(text(&mut app).contains("late news"));
        assert_eq!((app.cursor, app.browsing), (2, false));
        press(&mut app, "kk");
        assert_eq!(app.cursor, 1);
        posted(
            &mut app,
            post("p11", "c3", "u2", "later news", 1_700_000_400_000),
            &[],
        );
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn a_post_elsewhere_counts_a_mention_and_viewing_clears_the_tab() {
        let mut app = fixture();
        assert!(tabs(&mut app).contains("•#Town Square!"));
        posted(
            &mut app,
            post("p12", "c1", "u2", "@paul ping", 1_700_000_500_000),
            &["u1"],
        );
        let member = app.servers[0].teams[0].chans.as_ref().unwrap()[1]
            .member
            .clone()
            .unwrap();
        assert_eq!(member.mention_count, 2);
        app.apply_event(0, Event::Viewed(ChannelId("c1".into())));
        assert!(tabs(&mut app).contains(" #Town Square "));
    }

    #[test]
    fn my_own_post_from_another_device_is_not_unread() {
        let mut app = fixture();
        app.servers[0].teams[0].chans.as_mut().unwrap()[1].member = None;
        posted(
            &mut app,
            post("p13", "c1", "u1", "from my phone", 1_700_000_600_000),
            &[],
        );
        assert!(!tabs(&mut app).contains("•"));
    }

    fn members_viewed_up_to(ms: i64) -> Vec<ChannelMember> {
        let mut members: Vec<ChannelMember> = json("members");
        members[0].msg_count = ms;
        members[0].mention_count = 0;
        members
    }

    #[test]
    fn entering_a_channel_clears_its_tab_and_a_refresh_with_the_server_state_agrees() {
        let mut app = fixture();
        press(&mut app, "J");
        let member = app.servers[0].teams[0].chans.as_ref().unwrap()[1]
            .member
            .clone()
            .unwrap();
        assert_eq!((member.msg_count, member.mention_count), (2, 0));
        press(&mut app, "K");
        assert!(tabs(&mut app).contains(" #Town Square "));
        app.loaded(
            0,
            Loaded::Channels {
                team: TeamId("t1".into()),
                channels: json("channels"),
                members: members_viewed_up_to(2),
                categories: Some(json("categories")),
            },
        );
        assert!(tabs(&mut app).contains(" #Town Square "));
        assert_eq!(app.servers[0].teams[0].cur, 0);
    }

    fn enter_town_square(app: &mut App) {
        press(app, "J");
        let list: PostList = json("posts");
        app.loaded(
            0,
            Loaded::Posts {
                channel: ChannelId("c1".into()),
                list,
            },
        );
    }

    #[test]
    fn the_separator_and_the_cursor_mark_the_first_unread_until_the_next_visit() {
        let mut app = fixture();
        enter_town_square(&mut app);
        assert!(text(&mut app).contains("new messages"));
        assert_eq!((app.cursor, app.browsing), (1, true));
        press(&mut app, "KJ");
        assert!(!text(&mut app).contains("new messages"));
    }

    #[test]
    fn c_drops_the_separator_for_this_visit() {
        let mut app = fixture();
        enter_town_square(&mut app);
        press(&mut app, "c");
        assert_eq!(app.notice, "Marked as read");
        assert!(!text(&mut app).contains("new messages"));
    }

    #[test]
    fn n_goes_to_the_next_unread_channel_then_says_when_there_is_none() {
        let mut app = fixture();
        press(&mut app, "n");
        assert_eq!(app.servers[0].teams[0].cur, 1);
        let list: PostList = json("posts");
        app.loaded(
            0,
            Loaded::Posts {
                channel: ChannelId("c1".into()),
                list,
            },
        );
        press(&mut app, "n");
        assert_eq!(app.notice, "No other unread channel in this team");
        assert_eq!(app.servers[0].teams[0].cur, 1);
    }

    #[test]
    fn capital_n_goes_to_the_next_mention_then_says_when_there_is_none() {
        let mut app = fixture();
        press(&mut app, "N");
        assert_eq!(app.servers[0].teams[0].cur, 1);
        let list: PostList = json("posts");
        app.loaded(
            0,
            Loaded::Posts {
                channel: ChannelId("c1".into()),
                list,
            },
        );
        assert_eq!(app.cursor, 1);
        press(&mut app, "N");
        assert_eq!(app.notice, "No unread mention");
    }

    #[test]
    fn a_post_elsewhere_flags_its_tab_and_one_in_the_open_channel_does_not() {
        let mut app = fixture();
        app.servers[0].teams[0].chans.as_mut().unwrap()[0].member =
            serde_json::from_value(serde_json::json!({"channel_id": "c3", "msg_count": 0}))
                .unwrap();
        press(&mut app, "J");
        posted(
            &mut app,
            post("p20", "c3", "u2", "elsewhere", 1_700_000_900_000),
            &[],
        );
        assert!(tabs(&mut app).contains("•+Secret"));
        posted(
            &mut app,
            post("p21", "c1", "u2", "right here", 1_700_000_950_000),
            &[],
        );
        assert!(!tabs(&mut app).contains("•#Town Square"));
    }

    async fn live() -> (App, wiremock::MockServer) {
        use wiremock::matchers::any;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "OK"})),
            )
            .mount(&server)
            .await;
        let mut app = fixture();
        let client = Client::connect(&server.uri(), Credentials::Token("t".into()), None)
            .await
            .unwrap();
        app.servers[0].client = Some(Arc::new(client));
        (app, server)
    }

    async fn sent(server: &wiremock::MockServer) -> Vec<(String, String, String)> {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let requests = server.received_requests().await.unwrap();
        requests
            .iter()
            .map(|r| {
                let query = r.url.query().map_or(String::new(), |q| format!("?{q}"));
                (
                    r.method.to_string(),
                    format!("{}{query}", r.url.path()),
                    String::from_utf8_lossy(&r.body).into_owned(),
                )
            })
            .collect()
    }

    fn views(requests: &[(String, String, String)]) -> Vec<serde_json::Value> {
        requests
            .iter()
            .filter(|(_, path, _)| path == "/api/v4/channels/members/me/view")
            .map(|(_, _, body)| serde_json::from_str(body).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn entering_a_channel_sends_one_view_naming_the_channel_left() {
        let (mut app, server) = live().await;
        press(&mut app, "J");
        let sent = sent(&server).await;
        assert_eq!(
            views(&sent),
            [serde_json::json!({"channel_id": "c1", "prev_channel_id": "c3"})]
        );
    }

    #[tokio::test]
    async fn a_live_post_is_viewed_in_the_open_channel_only() {
        let (mut app, server) = live().await;
        posted(
            &mut app,
            post("p30", "c1", "u2", "elsewhere", 1_700_000_900_000),
            &[],
        );
        assert!(views(&sent(&server).await).is_empty());
        posted(
            &mut app,
            post("p31", "c3", "u2", "here", 1_700_000_950_000),
            &[],
        );
        let sent = sent(&server).await;
        assert_eq!(
            views(&sent),
            [serde_json::json!({"channel_id": "c3", "prev_channel_id": ""})]
        );
    }

    #[tokio::test]
    async fn a_whole_session_only_reads_and_views() {
        let (mut app, server) = live().await;
        press(&mut app, "JnNcKjkgGwb?qkkggmjjmo");
        posted(
            &mut app,
            post("p40", "c1", "u2", "@paul hi", 1_700_000_900_000),
            &["u1"],
        );
        let allowed = [
            "/api/v4/users/ids",
            "/api/v4/users/status/ids",
            "/api/v4/channels/members/me/view",
        ];
        for (method, path, _) in sent(&server).await {
            assert!(
                method == "GET" || (method == "POST" && allowed.contains(&path.as_str())),
                "{method} {path}"
            );
        }
    }
    #[test]
    fn edits_deletes_and_reactions_change_the_screen() {
        let mut app = fixture();
        press(&mut app, "a");
        let mut edited = post("p2", "c3", "u2", "hi there", 1_700_000_060_000);
        edited.edit_at = 1_700_000_070_000;
        app.apply_event(0, Event::Edited(edited));
        let shown = text(&mut app);
        assert!(shown.contains("hi there *"), "{shown}");
        app.apply_event(0, Event::ReactionAdded(reaction("u1", "p2")));
        assert!(text(&mut app).contains("🎉 1"));
        app.apply_event(0, Event::ReactionRemoved(reaction("u1", "p2")));
        assert!(!text(&mut app).contains("🎉 1"));
        press(&mut app, "kk");
        app.apply_event(
            0,
            Event::Deleted(post("p1", "c3", "u1", "", 1_700_000_000_000)),
        );
        assert!(!text(&mut app).contains("hello"));
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn a_dropped_stream_shows_offline_until_it_is_back() {
        let mut app = fixture();
        app.ws(0, Ws::Down("connection closed".into()));
        assert_eq!(app.notice, "work: connection closed");
        app.notice.clear();
        assert!(screen(&mut app, 80, 14)
            .last()
            .unwrap()
            .ends_with("offline  2/2  100%  ? help"));
        app.ws(0, Ws::Up { resync: false });
        assert!(!screen(&mut app, 80, 14).last().unwrap().contains("offline"));
    }

    fn page(posts: &[(&str, i64)]) -> PostList {
        let order: Vec<&str> = posts.iter().rev().map(|(id, _)| *id).collect();
        let posts: serde_json::Map<String, serde_json::Value> = posts
            .iter()
            .map(|(id, ms)| {
                let post = serde_json::json!({
                    "id": id, "create_at": ms, "user_id": "u2", "channel_id": "c3", "message": id
                });
                ((*id).to_owned(), post)
            })
            .collect();
        serde_json::from_value(serde_json::json!({"order": order, "posts": posts})).unwrap()
    }

    fn older(app: &mut App, before: &str, posts: &[(&str, i64)], asked: usize) {
        app.loaded(
            0,
            Loaded::Older {
                channel: ChannelId("c3".into()),
                before: PostId(before.into()),
                list: Ok(page(posts)),
                asked,
            },
        );
    }

    fn ids(app: &App) -> Vec<String> {
        app.messages()
            .unwrap()
            .iter()
            .map(|m| m.id.0.clone())
            .collect()
    }

    const OLD: [(&str, i64); 2] = [("o1", 1_699_999_000_000), ("o2", 1_699_999_100_000)];

    #[test]
    fn an_older_page_lands_the_cursor_above_the_old_top_or_keeps_it_on_its_message() {
        let mut app = fixture();
        press(&mut app, "kk");
        assert_eq!(app.cursor, 0);
        older(&mut app, "p1", &OLD, 60);
        assert_eq!(ids(&app), ["o1", "o2", "p1", "p2"]);
        assert_eq!(app.cursor, 1);

        let mut app = fixture();
        press(&mut app, "k");
        older(&mut app, "p1", &OLD, 60);
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn overlapping_and_stale_pages_change_nothing_wrong() {
        let mut app = fixture();
        older(&mut app, "p1", &[OLD[0], ("p1", 1_700_000_000_000)], 60);
        assert_eq!(ids(&app), ["o1", "p1", "p2"]);
        older(&mut app, "p1", &[OLD[1]], 60);
        assert_eq!(ids(&app), ["o1", "p1", "p2"]);
    }

    #[test]
    fn a_short_page_ends_the_history() {
        let mut app = fixture();
        app.servers[0].complete.clear();
        older(&mut app, "p1", &OLD, 60);
        assert!(text(&mut app).contains("Start of +Secret"));
        press(&mut app, "kkkkk");
        assert_eq!(app.notice, "Start of the channel");
    }

    #[test]
    fn a_full_page_leaves_the_history_open_and_a_failed_one_can_be_retried() {
        let mut app = fixture();
        app.servers[0].complete.clear();
        app.servers[0].paging.insert(ChannelId("c3".into()));
        older(&mut app, "p1", &OLD, 2);
        assert!(!app.servers[0].complete.contains(&ChannelId("c3".into())));
        app.servers[0].paging.insert(ChannelId("c3".into()));
        app.loaded(
            0,
            Loaded::Older {
                channel: ChannelId("c3".into()),
                before: PostId("o1".into()),
                list: Err("timeout".into()),
                asked: 60,
            },
        );
        assert!(app.servers[0].paging.is_empty());
        assert_eq!(app.notice, "work: timeout");
    }

    #[test]
    fn a_refresh_of_the_newest_page_keeps_the_older_history() {
        let mut app = fixture();
        older(&mut app, "p1", &OLD, 60);
        let list: PostList = json("posts");
        app.loaded(
            0,
            Loaded::Posts {
                channel: ChannelId("c3".into()),
                list,
            },
        );
        assert_eq!(ids(&app), ["o1", "o2", "p1", "p2"]);
    }

    fn crowd(app: &mut App, count: usize) {
        let base = 1_600_000_000_000;
        let list = (0..count)
            .map(|n| {
                Message::from_post(&post(
                    &format!("m{n}"),
                    "c3",
                    "u2",
                    "x",
                    base + n as i64 * 1000,
                ))
            })
            .collect();
        app.servers[0].messages.insert(ChannelId("c3".into()), list);
        app.servers[0].complete.clear();
    }

    #[test]
    fn a_live_post_over_the_cap_drops_the_oldest_and_keeps_the_cursor_on_its_message() {
        let mut app = fixture();
        crowd(&mut app, 500);
        (app.browsing, app.cursor) = (true, 10);
        posted(
            &mut app,
            post("new", "c3", "u2", "x", 1_700_000_000_000),
            &[],
        );
        assert_eq!((app.count(), app.cursor), (500, 9));
        assert_eq!(app.messages().unwrap()[0].id.0, "m1");
    }

    #[test]
    fn leaving_a_long_channel_trims_it_to_the_newest_page() {
        let mut app = fixture();
        crowd(&mut app, 100);
        app.servers[0].complete.insert(ChannelId("c3".into()));
        press(&mut app, "J");
        let id = ChannelId("c3".into());
        assert_eq!(app.servers[0].messages[&id].len(), model::PAGE);
        assert!(!app.servers[0].complete.contains(&id));
    }

    fn older_requests(sent: &[(String, String, String)]) -> Vec<String> {
        sent.iter()
            .filter(|(_, path, _)| path.contains("before="))
            .map(|(_, path, _)| path.clone())
            .collect()
    }

    #[tokio::test]
    async fn k_at_the_top_asks_once_for_the_page_before_the_oldest() {
        let (mut app, server) = live().await;
        app.servers[0].complete.clear();
        press(&mut app, "kkk");
        assert_eq!(app.notice, "Loading older messages…");
        press(&mut app, "k");
        let sent = sent(&server).await;
        let asked = older_requests(&sent);
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(asked[0].starts_with("/api/v4/channels/c3/posts?"));
        assert!(asked[0].contains("before=p1") && asked[0].contains("per_page=60"));
    }

    #[tokio::test]
    async fn the_cap_stops_paging_and_the_last_page_is_only_as_big_as_what_fits() {
        let (mut app, server) = live().await;
        crowd(&mut app, 500);
        (app.browsing, app.cursor) = (true, 0);
        press(&mut app, "k");
        assert_eq!(app.notice, "Showing the last 500 messages");
        assert!(older_requests(&sent(&server).await).is_empty());
        crowd(&mut app, 480);
        press(&mut app, "k");
        let asked = older_requests(&sent(&server).await);
        assert!(asked[0].contains("per_page=20"), "{asked:?}");
    }

    fn add_home(app: &mut App) {
        let mut server = Server::new("home".into());
        server.me = Some(json_user("u1", "paul"));
        app.servers.push(server);
        let team = serde_json::from_value(serde_json::json!(
            {"id": "t9", "display_name": "Family"}
        ))
        .unwrap();
        app.loaded(1, Loaded::Teams(vec![team]));
        let channels = serde_json::from_value(serde_json::json!([
            {"id": "h1", "type": "O", "display_name": "Garden", "name": "garden"},
            {"id": "h2", "type": "O", "display_name": "Kitchen", "name": "kitchen"}
        ]))
        .unwrap();
        app.loaded(
            1,
            Loaded::Channels {
                team: TeamId("t9".into()),
                channels,
                members: Vec::new(),
                categories: None,
            },
        );
    }

    #[test]
    fn f_filters_the_team_and_enter_goes_to_the_channel() {
        let mut app = fixture();
        press(&mut app, "f");
        assert_eq!(app.mode(), "SEARCH");
        press(&mut app, "town");
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(app.mode(), "NORMAL");
        assert_eq!(app.servers[0].teams[0].cur, 1);
    }

    #[test]
    fn esc_closes_the_finder_without_moving_and_keys_are_text_in_it() {
        let mut app = fixture();
        press(&mut app, "fjk");
        assert_eq!(app.finder.as_ref().unwrap().line.text(), "jk");
        press_code(&mut app, KeyCode::Esc);
        assert!(app.finder.is_none());
        assert_eq!(app.servers[0].teams[0].cur, 0);
    }

    #[test]
    fn tab_wraps_around_the_matches_and_typing_goes_back_to_the_best() {
        let mut app = fixture();
        press(&mut app, "f");
        for _ in 0..3 {
            press_code(&mut app, KeyCode::Tab);
        }
        assert_eq!(app.finder.as_ref().unwrap().selected, 0);
        press_code(&mut app, KeyCode::BackTab);
        assert_eq!(app.finder.as_ref().unwrap().selected, 2);
        press(&mut app, "t");
        assert_eq!(app.finder.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn capital_f_reaches_other_servers_and_f_does_not() {
        let mut app = fixture();
        add_home(&mut app);
        press(&mut app, "fgard");
        assert!(app.finder_targets().is_empty());
        press_code(&mut app, KeyCode::Esc);
        press(&mut app, "Fgard");
        assert_eq!(app.finder_targets().len(), 1);
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(
            (app.server, app.team, app.servers[1].teams[0].cur),
            (1, 0, 0)
        );
        press(&mut app, "FKitchen");
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(app.servers[1].teams[0].cur, 1);
    }

    #[test]
    fn at_lists_the_conversations_and_a_hash_names_the_team() {
        let mut app = fixture();
        add_home(&mut app);
        press(&mut app, "F@");
        assert_eq!(app.finder_targets(), [(0, 0, 2)]);
        press_code(&mut app, KeyCode::Esc);
        press(&mut app, "Fhome#k");
        assert_eq!(app.finder_targets(), [(1, 0, 1)]);
    }

    fn new_post(id: &str, user: &str, text: &str, root: &str, ms: i64) -> serde_json::Value {
        serde_json::json!({
            "id": id, "create_at": ms, "user_id": user, "channel_id": "c3",
            "message": text, "root_id": root
        })
    }

    async fn writing() -> (
        App,
        tokio::sync::mpsc::UnboundedReceiver<Msg>,
        wiremock::MockServer,
    ) {
        use wiremock::matchers::{any, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let reply = |post| ResponseTemplate::new(201).set_body_json(post);
        Mock::given(method("POST"))
            .and(path("/api/v4/posts"))
            .respond_with(reply(new_post(
                "new1",
                "u1",
                "hello there",
                "",
                1_700_000_500_000,
            )))
            .mount(&server)
            .await;
        let reaction = serde_json::json!({"user_id": "u1", "post_id": "p2", "emoji_name": "heart"});
        Mock::given(method("POST"))
            .and(path("/api/v4/reactions"))
            .respond_with(reply(reaction))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/users"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "u1", "username": "paul"}, {"id": "u2", "username": "ana"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v4/users/status/ids"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{"user_id": "u2", "status": "online"}])),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v4/posts/p1/patch"))
            .respond_with(reply(new_post(
                "p1",
                "u1",
                "new text",
                "",
                1_700_000_000_000,
            )))
            .mount(&server)
            .await;
        Mock::given(any())
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "OK"})),
            )
            .mount(&server)
            .await;
        let (mut app, rx) = fixture_rx();
        let client = Client::connect(&server.uri(), Credentials::Token("t".into()), None)
            .await
            .unwrap();
        app.servers[0].client = Some(Arc::new(client));
        (app, rx, server)
    }

    async fn settle(
        app: &mut App,
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
        done: impl Fn(&App) -> bool,
    ) {
        while !done(app) {
            let waited = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;
            match waited {
                Ok(Some(msg)) => app.update(msg),
                _ => panic!("the expected change never came"),
            }
        }
    }

    fn writes(sent: &[(String, String, String)]) -> Vec<(String, String)> {
        sent.iter()
            .filter(|(m, p, _)| {
                m != "GET"
                    && !p.ends_with("/view")
                    && !p.ends_with("/users/ids")
                    && !p.ends_with("/status/ids")
            })
            .map(|(m, p, b)| (format!("{m} {p}"), b.clone()))
            .collect()
    }

    #[tokio::test]
    async fn a_message_is_sent_once_shown_once_and_the_input_stays_open() {
        let (mut app, mut rx, server) = writing().await;
        press(&mut app, "ihello there");
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(
            (app.mode(), app.compose.as_ref().unwrap().line.text()),
            ("INSERT", String::new())
        );
        settle(&mut app, &mut rx, |a| {
            a.messages()
                .is_some_and(|m| m.iter().any(|m| m.id.0 == "new1"))
        })
        .await;
        assert!(text(&mut app).contains("hello there"));
        posted(
            &mut app,
            post("new1", "c3", "u1", "hello there", 1_700_000_500_000),
            &[],
        );
        assert_eq!(ids(&app).iter().filter(|id| *id == "new1").count(), 1);
        let sent = writes(&sent(&server).await);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "POST /api/v4/posts");
        let body: serde_json::Value = serde_json::from_str(&sent[0].1).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"channel_id": "c3", "message": "hello there", "root_id": ""})
        );
    }

    #[tokio::test]
    async fn empty_and_slash_messages_are_not_sent() {
        let (mut app, _rx, server) = writing().await;
        press(&mut app, "i   ");
        press_code(&mut app, KeyCode::Enter);
        assert!(writes(&sent(&server).await).is_empty());
        app.compose.as_mut().unwrap().line = Line::with_text("/join town", false);
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(app.notice, "Slash commands are not supported");
        assert!(writes(&sent(&server).await).is_empty());
    }

    #[tokio::test]
    async fn a_reply_goes_to_the_thread_root() {
        let (mut app, _rx, server) = writing().await;
        posted(
            &mut app,
            post("p5", "c3", "u2", "root", 1_700_000_100_000),
            &[],
        );
        let mut reply = post("p6", "c3", "u2", "answer", 1_700_000_200_000);
        reply.root_id = "p5".into();
        posted(&mut app, reply, &[]);
        press(&mut app, "r");
        assert_eq!(
            screen(&mut app, 80, 14).iter().nth_back(1).unwrap(),
            " ↪ ana >"
        );
        press(&mut app, "one");
        press_code(&mut app, KeyCode::Enter);
        press_code(&mut app, KeyCode::Esc);
        app.cursor = app.count() - 2;
        press(&mut app, "rtwo");
        press_code(&mut app, KeyCode::Enter);
        let sent = writes(&sent(&server).await);
        let roots: Vec<String> = sent
            .iter()
            .map(|(_, b)| {
                serde_json::from_str::<serde_json::Value>(b).unwrap()["root_id"].to_string()
            })
            .collect();
        assert_eq!(roots, ["\"p5\"", "\"p5\""]);
    }

    #[tokio::test]
    async fn only_your_own_messages_can_be_edited_and_the_edit_is_a_patch() {
        let (mut app, mut rx, server) = writing().await;
        press(&mut app, "e");
        assert_eq!(app.notice, "That is not your message");
        assert!(app.compose.is_none());
        app.cursor = 0;
        press(&mut app, "e");
        assert_eq!(app.compose.as_ref().unwrap().line.text(), "hello :tada:");
        assert_eq!(
            screen(&mut app, 80, 14).iter().nth_back(1).unwrap(),
            " ✎ edit > hello :tada:"
        );
        app.compose.as_mut().unwrap().line = Line::with_text("new text", false);
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(app.mode(), "NORMAL");
        settle(&mut app, &mut rx, |a| {
            a.messages().is_some_and(|m| m[0].text == "new text")
        })
        .await;
        let sent = writes(&sent(&server).await);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "PUT /api/v4/posts/p1/patch");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&sent[0].1).unwrap(),
            serde_json::json!({"message": "new text"})
        );
    }

    #[tokio::test]
    async fn deleting_asks_first_and_only_y_deletes() {
        let (mut app, mut rx, server) = writing().await;
        app.cursor = 0;
        press(&mut app, "dn");
        assert!(writes(&sent(&server).await).is_empty());
        assert_eq!(ids(&app), ["p1", "p2"]);
        press(&mut app, "d");
        assert_eq!(
            app.notice,
            "Delete this message?  y = yes, any other key = no"
        );
        press(&mut app, "y");
        settle(&mut app, &mut rx, |a| a.count() == 1).await;
        assert_eq!(ids(&app), ["p2"]);
        assert_eq!(writes(&sent(&server).await)[0].0, "DELETE /api/v4/posts/p1");
        press(&mut app, "d");
        assert_eq!(app.notice, "That is not your message");
        press(&mut app, "y");
        assert_eq!(writes(&sent(&server).await).len(), 1);
    }

    #[test]
    fn a_draft_is_kept_per_channel_and_a_failed_send_gives_the_text_back() {
        let mut app = fixture();
        press(&mut app, "idraft one");
        press_code(&mut app, KeyCode::Esc);
        assert_eq!(app.mode(), "NORMAL");
        press(&mut app, "i");
        assert_eq!(app.compose.as_ref().unwrap().line.text(), "draft one");
        press_code(&mut app, KeyCode::Esc);
        press(&mut app, "Ji");
        assert_eq!(app.compose.as_ref().unwrap().line.text(), "");
        press_code(&mut app, KeyCode::Esc);
        app.loaded(
            0,
            Loaded::NotSent {
                channel: ChannelId("c1".into()),
                text: "lost words".into(),
                error: "timeout".into(),
            },
        );
        assert_eq!(app.notice, "work: timeout (kept as a draft)");
        press(&mut app, "i");
        assert_eq!(app.compose.as_ref().unwrap().line.text(), "lost words");
    }

    #[test]
    fn a_paste_never_sends_and_loses_its_line_breaks() {
        let mut app = fixture();
        press(&mut app, "ix ");
        app.update(Msg::Paste("a\nb\r\nc".into()));
        assert_eq!(app.compose.as_ref().unwrap().line.text(), "x a b  c");
        app.update(Msg::Key(key(KeyCode::Esc)));
        app.update(Msg::Paste("ignored".into()));
        assert!(app.compose.is_none() && app.input.is_none());
    }

    #[test]
    fn typing_is_announced_once_per_input_session_and_never_while_editing() {
        let mut app = fixture();
        let (actions, mut seen) = unbounded_channel();
        app.servers[0].actions = Some(actions);
        press(&mut app, "iabc");
        assert!(matches!(
            seen.try_recv(),
            Ok(Action::Typing { parent: None, .. })
        ));
        assert!(seen.try_recv().is_err());
        press_code(&mut app, KeyCode::Esc);
        press(&mut app, "id");
        assert!(seen.try_recv().is_ok());
        press_code(&mut app, KeyCode::Esc);
        app.cursor = 0;
        press(&mut app, "ex");
        assert!(seen.try_recv().is_err());
    }

    #[test]
    fn who_is_typing_shows_on_the_status_line_until_they_stop_or_expire() {
        let mut app = fixture();
        let (channel, ana) = (ChannelId("c3".into()), UserId("u2".into()));
        app.apply_event(
            0,
            Event::Typing {
                channel: channel.clone(),
                user: ana.clone(),
            },
        );
        let status = screen(&mut app, 80, 14).pop().unwrap();
        assert!(status.contains("✎ ana"), "{status:?}");
        posted(
            &mut app,
            post("p9", "c3", "u2", "done", 1_700_000_900_000),
            &[],
        );
        assert!(!screen(&mut app, 80, 14).pop().unwrap().contains("✎"));
        let old = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        app.servers[0].typing.insert(channel, vec![(ana, old)]);
        assert!(!screen(&mut app, 80, 14).pop().unwrap().contains("✎"));
        let me = UserId("u1".into());
        app.apply_event(
            0,
            Event::Typing {
                channel: ChannelId("c3".into()),
                user: me,
            },
        );
        assert!(!screen(&mut app, 80, 14).pop().unwrap().contains("✎"));
    }

    #[tokio::test]
    async fn a_writing_session_only_makes_the_permitted_writes() {
        let (mut app, mut rx, server) = writing().await;
        press(&mut app, "ione");
        press_code(&mut app, KeyCode::Enter);
        press_code(&mut app, KeyCode::Esc);
        app.cursor = 0;
        press(&mut app, "e");
        press_code(&mut app, KeyCode::Enter);
        press(&mut app, "dy");
        settle(&mut app, &mut rx, |a| a.count() < 3).await;
        app.cursor = app.count() - 1;
        press(&mut app, "x2");
        press(&mut app, "X");
        let allowed = [
            "POST /api/v4/posts",
            "PUT /api/v4/posts/p1/patch",
            "DELETE /api/v4/posts/p1",
            "POST /api/v4/reactions",
            "DELETE /api/v4/users/u1/posts/p2/reactions/heart",
        ];
        for (request, _) in writes(&sent(&server).await) {
            assert!(allowed.contains(&request.as_str()), "{request}");
        }
    }

    #[tokio::test]
    async fn sending_marks_the_channel_viewed_right_after_the_post_and_only_once() {
        let (mut app, mut rx, server) = writing().await;
        press(&mut app, "ihello there");
        press_code(&mut app, KeyCode::Enter);
        settle(&mut app, &mut rx, |a| {
            a.messages()
                .is_some_and(|m| m.iter().any(|m| m.id.0 == "new1"))
        })
        .await;
        posted(
            &mut app,
            post("new1", "c3", "u1", "hello there", 1_700_000_500_000),
            &[],
        );
        let sent = sent(&server).await;
        let order: Vec<&str> = sent
            .iter()
            .filter(|(m, p, _)| m == "POST" && (p == "/api/v4/posts" || p.ends_with("/view")))
            .map(|(_, p, _)| p.as_str())
            .collect();
        assert_eq!(order, ["/api/v4/posts", "/api/v4/channels/members/me/view"]);
        assert_eq!(
            views(&sent),
            [serde_json::json!({"channel_id": "c3", "prev_channel_id": ""})]
        );
    }

    pub fn ctrl(c: char) -> Msg {
        Msg::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    pub fn editor(app: &mut App, body: &str) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("editor.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        app.settings.editor = Some(format!("sh {}", path.display()));
        dir
    }

    fn until_screen(app: &mut App, wanted: &str) -> Vec<String> {
        for _ in 0..100 {
            let lines = screen(app, 60, 16);
            if lines.iter().any(|l| l.contains(wanted)) {
                return lines;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        panic!("{wanted:?} never appeared: {:?}", screen(app, 60, 16));
    }

    #[test]
    fn ctrl_g_from_normal_mode_opens_the_editor_in_the_lower_half() {
        let mut app = fixture();
        let _dir = editor(&mut app, "printf 'IN-THE-EDITOR'; sleep 30");
        app.update(ctrl('g'));
        assert_eq!(app.mode(), "EDITOR");
        let lines = until_screen(&mut app, "IN-THE-EDITOR");
        let row = lines
            .iter()
            .position(|l| l.contains("IN-THE-EDITOR"))
            .unwrap();
        assert!(row >= lines.len() / 2, "{row}");
        assert!(
            lines.iter().take(row).any(|l| l.contains("hello")),
            "{lines:?}"
        );
        assert!(lines.last().unwrap().starts_with(" EDITOR"));
        let (pane_rows, _) = app.pane.as_ref().unwrap().size();
        assert_eq!(pane_rows, term::split(16).1);
    }

    #[test]
    fn the_conversation_stays_alive_above_the_editor() {
        let mut app = fixture();
        let _dir = editor(&mut app, "printf 'EDITING'; sleep 30");
        app.update(ctrl('g'));
        until_screen(&mut app, "EDITING");
        posted(
            &mut app,
            post(
                "p50",
                "c3",
                "u2",
                "arrived while editing",
                1_700_000_900_000,
            ),
            &[],
        );
        until_screen(&mut app, "arrived while editing");
    }

    #[test]
    fn keys_go_to_the_editor_and_its_result_lands_in_the_input_line() {
        let (mut app, mut rx) = fixture_rx();
        let _dir = editor(
            &mut app,
            "IFS= read -r a\nprintf '%s\\n%s\\n' \"$a\" second > \"$1\"",
        );
        app.update(ctrl('g'));
        press(&mut app, "q");
        assert!(!app.quit);
        press(&mut app, "ix");
        press_code(&mut app, KeyCode::Enter);
        while app.pane.is_some() {
            let msg = rx.blocking_recv().unwrap();
            app.update(msg);
        }
        assert_eq!(app.mode(), "INSERT");
        assert_eq!(app.compose.as_ref().unwrap().line.text(), "qix\nsecond");
        let row = screen(&mut app, 60, 12).iter().nth_back(1).unwrap().clone();
        assert_eq!(row, " > qix⏎second");
    }

    #[test]
    fn an_empty_result_from_normal_mode_cancels_and_keeps_the_draft() {
        let (mut app, mut rx) = fixture_rx();
        press(&mut app, "idraft");
        press_code(&mut app, KeyCode::Esc);
        let _dir = editor(&mut app, ": > \"$1\"");
        app.update(ctrl('g'));
        while app.pane.is_some() {
            let msg = rx.blocking_recv().unwrap();
            app.update(msg);
        }
        assert!(app.compose.is_none());
        press(&mut app, "i");
        assert_eq!(app.compose.as_ref().unwrap().line.text(), "draft");
        let (mut app, mut rx) = fixture_rx();
        let _dir = editor(&mut app, ": > \"$1\"");
        app.update(ctrl('g'));
        while app.pane.is_some() {
            let msg = rx.blocking_recv().unwrap();
            app.update(msg);
        }
        assert_eq!(app.mode(), "NORMAL");
    }

    #[test]
    fn a_failing_editor_says_so_and_a_normal_mode_start_is_undone() {
        let (mut app, mut rx) = fixture_rx();
        let _dir = editor(&mut app, "exit 1");
        app.update(ctrl('g'));
        while app.pane.is_some() {
            let msg = rx.blocking_recv().unwrap();
            app.update(msg);
        }
        assert!(
            app.notice.starts_with("Editor: the editor exited"),
            "{}",
            app.notice
        );
        assert_eq!(app.mode(), "NORMAL");
    }

    #[test]
    fn ctrl_keys_do_nothing_in_normal_mode_and_ctrl_g_needs_a_plain_screen() {
        let mut app = fixture();
        app.update(ctrl('d'));
        assert!(app.notice.is_empty() && app.awaiting.is_none());
        press(&mut app, "f");
        app.update(ctrl('g'));
        assert!(app.pane.is_none());
        assert_eq!(app.mode(), "SEARCH");
    }

    #[test]
    fn a_pasted_text_goes_to_the_editor_not_to_the_line() {
        let mut app = fixture();
        let _dir = editor(&mut app, "sleep 30");
        app.update(ctrl('g'));
        app.update(Msg::Paste("pasted".into()));
        assert_eq!(app.compose.as_ref().unwrap().line.text(), "");
    }

    #[tokio::test]
    async fn a_message_written_in_the_editor_is_sent_with_its_line_breaks() {
        let (mut app, _rx, server) = writing().await;
        press(&mut app, "i");
        app.finished(Ok("first\n\nsecond".into()));
        press_code(&mut app, KeyCode::Enter);
        let sent = writes(&sent(&server).await);
        let body: serde_json::Value = serde_json::from_str(&sent[0].1).unwrap();
        assert_eq!(body["message"], "first\n\nsecond");
    }

    #[test]
    fn a_channel_the_server_counts_as_read_shows_no_separator_whatever_its_old_timestamps() {
        let mut app = fixture();
        app.servers[0].teams[0].chans.as_mut().unwrap()[1]
            .member
            .as_mut()
            .unwrap()
            .msg_count = 2;
        enter_town_square(&mut app);
        assert!(!text(&mut app).contains("new messages"));
        assert_eq!((app.cursor, app.browsing), (1, false));
    }

    #[test]
    fn the_separator_stays_on_its_post_while_new_posts_arrive() {
        let mut app = fixture();
        enter_town_square(&mut app);
        for (n, id) in ["n1", "n2"].into_iter().enumerate() {
            let ms = 1_700_000_100_000 + n as i64 * 1000;
            posted(&mut app, post(id, "c1", "u2", "more", ms), &[]);
        }
        let shown = text(&mut app);
        assert_eq!(shown.matches("new messages").count(), 1, "{shown}");
        let lines: Vec<&str> = shown.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains("new messages"))
            .unwrap();
        assert!(lines[at + 1].contains("hi"), "{shown}");
    }

    fn last_reactions(app: &App) -> Vec<(String, usize)> {
        let list = app.messages().unwrap();
        list[list.len() - 1]
            .reactions
            .iter()
            .map(|g| (g.name.clone(), g.users.len()))
            .collect()
    }

    #[tokio::test]
    async fn the_picker_adds_a_reaction_and_the_same_key_again_removes_it() {
        let (mut app, mut rx, server) = writing().await;
        press(&mut app, "x");
        assert_eq!(app.mode(), "REACT");
        press(&mut app, "z");
        assert_eq!(app.mode(), "REACT");
        press(&mut app, "2");
        assert_eq!(app.mode(), "NORMAL");
        settle(&mut app, &mut rx, |a| {
            last_reactions(a).iter().any(|r| r.0 == "heart")
        })
        .await;
        assert_eq!(app.notice, "Reacted ❤️");
        press(&mut app, "x2");
        settle(&mut app, &mut rx, |a| {
            !last_reactions(a).iter().any(|r| r.0 == "heart")
        })
        .await;
        assert_eq!(app.notice, "Removed ❤️");
        press(&mut app, "X");
        settle(&mut app, &mut rx, |a| {
            last_reactions(a).iter().any(|r| r.0 == "heart")
        })
        .await;
        let sent = writes(&sent(&server).await);
        let requests: Vec<&str> = sent.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(
            requests,
            [
                "POST /api/v4/reactions",
                "DELETE /api/v4/users/u1/posts/p2/reactions/heart",
                "POST /api/v4/reactions"
            ]
        );
        let body: serde_json::Value = serde_json::from_str(&sent[0].1).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"user_id": "u1", "post_id": "p2", "emoji_name": "heart"})
        );
    }

    #[test]
    fn the_picker_closes_on_esc_and_x_does_nothing_without_a_message_and_x_repeats_nothing_at_first(
    ) {
        let mut app = fixture();
        press(&mut app, "x");
        press_code(&mut app, KeyCode::Esc);
        assert_eq!(app.mode(), "NORMAL");
        press(&mut app, "X");
        assert_eq!(app.notice, "No recent reaction: pick one with x");
        press(&mut app, "J");
        press(&mut app, "J");
        press(&mut app, "x");
        assert_eq!(app.mode(), "NORMAL");
        let bottom = screen(&mut app, 80, 12).pop().unwrap();
        assert!(!bottom.starts_with(" REACT"));
    }

    #[test]
    fn y_and_capital_y_copy_a_message_or_a_range_through_the_register_and_the_clipboard() {
        let mut app = fixture();
        press(&mut app, "y");
        assert_eq!(app.register, "hi");
        assert_eq!(app.clipboard, [clipboard::osc52("hi")]);
        assert_eq!(app.notice, "Copied 1 message");
        let time = app.messages().unwrap()[1].at.format("%H:%M").to_string();
        press(&mut app, "Y");
        assert_eq!(app.register, format!("[{time}] ana: hi"));
        (app.cursor, app.browsing) = (0, true);
        press(&mut app, "Vj");
        assert_eq!(app.mode(), "VISUAL");
        press(&mut app, "y");
        assert_eq!(app.register, "hello :tada:\nhi");
        assert_eq!(app.notice, "Copied 2 messages");
        assert_eq!(app.mode(), "NORMAL");
        press(&mut app, "Vj");
        press_code(&mut app, KeyCode::Esc);
        assert_eq!(app.mode(), "NORMAL");
    }

    #[test]
    fn p_puts_the_register_in_the_input_and_complains_when_it_is_empty() {
        let mut app = fixture();
        press(&mut app, "p");
        assert_eq!(app.notice, "Nothing to paste: copy a message with y first");
        assert!(app.compose.is_none());
        (app.cursor, app.browsing) = (0, true);
        press(&mut app, "Vjy");
        press(&mut app, "p");
        assert_eq!(app.mode(), "INSERT");
        assert_eq!(
            app.compose.as_ref().unwrap().line.text(),
            "hello :tada:\nhi"
        );
    }

    #[test]
    fn m_shows_scrolls_unfocuses_and_hides_the_column_and_refuses_a_narrow_screen() {
        let mut app = fixture();
        screen(&mut app, 60, 14);
        press(&mut app, "m");
        assert_eq!(app.notice, "Screen too narrow for the users list");
        assert!(!app.users.shown);
        screen(&mut app, 100, 14);
        press(&mut app, "m");
        assert!(app.users.shown && app.users.focus);
        assert_eq!(app.mode(), "USERS");
        press(&mut app, "jj");
        assert_eq!(app.users.top, 2);
        press(&mut app, "q");
        assert_eq!(app.mode(), "NORMAL");
        assert!(app.users.shown && !app.quit);
        press(&mut app, "mm");
        assert!(!app.users.shown);
    }

    #[test]
    fn the_column_lists_who_is_around_first_and_follows_status_events() {
        let mut app = fixture();
        let channel = ChannelId("c3".into());
        screen(&mut app, 100, 14);
        press(&mut app, "m");
        app.servers[0].members.insert(
            channel.clone(),
            vec![UserId("u1".into()), UserId("u2".into())],
        );
        app.servers[0]
            .status
            .insert(UserId("u2".into()), Status::Online);
        let shown = screen(&mut app, 100, 14).join("\n");
        assert!(
            shown.contains("present — 1") && shown.contains("● ana"),
            "{shown}"
        );
        assert!(
            shown.contains("absent — 1") && shown.contains("○ paul"),
            "{shown}"
        );
        assert!(shown.contains("hello"), "{shown}");
        app.apply_event(
            0,
            Event::Status {
                user: UserId("u1".into()),
                status: Status::Away,
            },
        );
        let shown = screen(&mut app, 100, 14).join("\n");
        assert!(
            shown.contains("present — 2") && shown.contains("● paul"),
            "{shown}"
        );
        let row = shown.lines().find(|l| l.contains("hello")).unwrap();
        assert!(row.contains("│") || row.width() <= 100, "{row}");
    }

    #[tokio::test]
    async fn m_fetches_the_members_and_their_statuses() {
        let (mut app, mut rx, server) = writing().await;
        screen(&mut app, 100, 14);
        press(&mut app, "m");
        settle(&mut app, &mut rx, |a| {
            a.servers[0].members.contains_key(&ChannelId("c3".into()))
        })
        .await;
        let shown = screen(&mut app, 100, 14).join("\n");
        assert!(
            shown.contains("● ana") && shown.contains("○ paul"),
            "{shown}"
        );
        let requests = sent(&server).await;
        assert!(requests.iter().any(|(m, p, _)| m == "GET"
            && p.starts_with("/api/v4/users?")
            && p.contains("in_channel=c3")));
        assert!(requests
            .iter()
            .any(|(m, p, b)| m == "POST" && p == "/api/v4/users/status/ids" && b.contains("u2")));
    }

    fn program(dir: &tempfile::TempDir, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        format!("sh {}", path.display())
    }

    fn wait_for_file(path: &std::path::Path) -> String {
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(path) {
                if !text.is_empty() {
                    return text;
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("{} never appeared", path.display());
    }

    fn message_with(app: &mut App, id: &str, text: &str, files: &[(&str, &str, &str)]) {
        let mut post = post(id, "c3", "u2", text, 1_700_000_800_000);
        let files: Vec<serde_json::Value> = files
            .iter()
            .map(|(fid, name, mime)| serde_json::json!({"id": fid, "name": name, "mime_type": mime, "size": 4}))
            .collect();
        post.metadata = serde_json::from_value(serde_json::json!({"files": files})).unwrap();
        posted(app, post, &[]);
    }

    #[test]
    fn o_opens_a_single_link_in_the_browser_and_says_when_there_is_nothing() {
        let mut app = fixture();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        app.settings.open.browser = program(
            &dir,
            "browser.sh",
            &format!("printf '%s' \"$1\" > {}", out.display()),
        );
        (app.cursor, app.browsing) = (0, true);
        press(&mut app, "o");
        assert_eq!(app.notice, "Nothing to open");
        message_with(&mut app, "p80", "read https://x.org/page, then reply", &[]);
        app.cursor = app.count() - 1;
        press(&mut app, "o");
        assert_eq!(wait_for_file(&out), "https://x.org/page");
        assert_eq!(app.notice, "Opened https://x.org/page");
    }

    #[test]
    fn several_things_make_a_picker_and_a_digit_chooses() {
        let mut app = fixture();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        app.settings.open.browser = program(
            &dir,
            "browser.sh",
            &format!("printf '%s' \"$1\" > {}", out.display()),
        );
        message_with(&mut app, "p81", "one https://a.org and https://b.org", &[]);
        press(&mut app, "o");
        assert_eq!(app.mode(), "OPEN");
        let bottom = screen(&mut app, 100, 12).pop().unwrap();
        assert!(
            bottom.starts_with(" OPEN  1 https://a.org") && bottom.contains("2 https://b.org"),
            "{bottom:?}"
        );
        press(&mut app, "z");
        assert_eq!(app.mode(), "OPEN");
        press(&mut app, "2");
        assert_eq!(app.mode(), "NORMAL");
        assert_eq!(wait_for_file(&out), "https://b.org");
        press(&mut app, "o");
        press_code(&mut app, KeyCode::Esc);
        assert_eq!(app.mode(), "NORMAL");
    }

    #[tokio::test]
    async fn an_attachment_is_downloaded_privately_and_opened_by_its_kind() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/files/f9"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PIXELS".to_vec()))
            .mount(&server)
            .await;
        let (mut app, mut rx) = fixture_rx();
        app.servers[0].client = Some(Arc::new(
            Client::connect(&server.uri(), Credentials::Token("t".into()), None)
                .await
                .unwrap(),
        ));
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        app.settings.open.image = program(
            &dir,
            "image.sh",
            &format!(
                "printf '%s|' \"$1\" > {}; cat \"$1\" >> {}",
                out.display(),
                out.display()
            ),
        );
        message_with(
            &mut app,
            "p82",
            "look",
            &[("f9", "../cat.png", "image/png")],
        );
        press(&mut app, "o");
        assert_eq!(app.notice, "Downloading ../cat.png (4 B)…");
        settle(&mut app, &mut rx, |a| a.notice.starts_with("Opened")).await;
        let result = wait_for_file(&out);
        let (path, content) = result.split_once('|').unwrap();
        assert!(path.ends_with("/f9/cat.png"), "{path}");
        assert_eq!(content, "PIXELS");
        let folder = app.files.base().to_owned();
        drop(app);
        assert!(!folder.exists());
    }

    #[tokio::test]
    async fn a_text_attachment_opens_in_the_pane_and_the_pane_closes_when_it_ends() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/files/f7"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"line one".to_vec()))
            .mount(&server)
            .await;
        let (mut app, mut rx) = fixture_rx();
        app.servers[0].client = Some(Arc::new(
            Client::connect(&server.uri(), Credentials::Token("t".into()), None)
                .await
                .unwrap(),
        ));
        let dir = tempfile::tempdir().unwrap();
        app.settings.open.text = program(&dir, "viewer.sh", "cat \"$1\"; sleep 0.3");
        message_with(
            &mut app,
            "p83",
            "notes",
            &[("f7", "notes.txt", "text/plain")],
        );
        press(&mut app, "o");
        settle(&mut app, &mut rx, |a| a.pane.is_some()).await;
        assert_eq!(app.mode(), "VIEWER");
        let lines = until_screen(&mut app, "line one");
        assert!(
            lines.iter().any(|l| l.starts_with("─── Viewing notes.txt")),
            "{lines:?}"
        );
        settle(&mut app, &mut rx, |a| a.pane.is_none()).await;
        assert_eq!(app.mode(), "NORMAL");
        assert!(app.compose.is_none());
    }

    #[test]
    fn reacting_turns_the_reaction_display_on() {
        let mut app = fixture();
        assert!(!app.reactions);
        app.loaded(
            0,
            Loaded::Reacted(Reaction {
                user_id: UserId("u1".into()),
                post_id: PostId("p2".into()),
                emoji_name: "heart".into(),
            }),
        );
        assert!(app.reactions);
        press(&mut app, "a");
        assert!(!app.reactions);
        app.loaded(
            0,
            Loaded::Unreacted(Reaction {
                user_id: UserId("u1".into()),
                post_id: PostId("p2".into()),
                emoji_name: "heart".into(),
            }),
        );
        assert!(app.reactions);
    }

    #[test]
    fn yank_hands_the_text_to_the_clipboard_command_and_reports_its_failure() {
        let (mut app, mut rx) = fixture_rx();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("copied");
        app.settings.copy = Some(format!("cat > {}", out.display()));
        press(&mut app, "y");
        assert_eq!(wait_for_file(&out), "hi");
        app.settings.copy = Some("exit 2".into());
        press(&mut app, "y");
        let msg = rx.blocking_recv().unwrap();
        app.update(msg);
        assert!(
            app.notice.starts_with("Copy failed: exit 2 exited"),
            "{}",
            app.notice
        );
    }
}
