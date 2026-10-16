//! Application state, the events that change it, and the requests it starts.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::UnboundedSender;

use crate::api::types::{
    CategoryList, Channel, ChannelId, ChannelMember, PostList, Team as WireTeam, TeamId, User,
    UserId,
};
use crate::api::{Client, Error};
use crate::config::{self, Credentials, ServerConfig};
use crate::input::{self, Command, Line, Step, Wizard};
use crate::model::{self, group_start, group_with_previous, Link, Message, Server, Team};
use crate::ws::{self, Event, Ws};

const POSTS_PER_PAGE: u32 = 60;

/// Everything that can wake the application.
#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Resize,
    Loaded(usize, Loaded),
    LoggedIn(Result<Box<Session>, Error>),
    Ws(usize, Ws),
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
    Users(Vec<User>),
    Failed(String),
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
    pub quit: bool,
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
            quit: false,
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
        }
    }

    /// Name of the current mode, written on the status line.
    pub fn mode(&self) -> &'static str {
        match (&self.input, &self.wizard, &self.help) {
            (_, Some(_), _) => "LOGIN",
            (Some(_), ..) => "COMMAND",
            (_, _, Some(_)) => "HELP",
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
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
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
        let KeyCode::Char(c) = key.code else {
            return;
        };
        match c {
            'q' => self.quit = true,
            ':' => self.input = Some(Line::new(false)),
            '?' => self.help = Some(0),
            'j' => self.step_down(),
            'k' => self.step_up(),
            'g' => self.move_cursor(0),
            'G' => self.move_cursor(usize::MAX),
            'a' => self.reactions = !self.reactions,
            'w' => self.next_group(),
            'b' => self.previous_group(),
            'J' => self.move_channel(1),
            'K' => self.move_channel(-1),
            'H' => self.move_team(-1),
            'L' => self.move_team(1),
            _ => {}
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
                self.start_ws(self.server);
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
        match self.browsing {
            true => self.move_cursor(self.cursor.saturating_sub(1)),
            false => self.browsing = self.count() > 0,
        }
    }

    fn step_down(&mut self) {
        match (self.browsing, self.cursor + 1 >= self.count()) {
            (false, _) => {}
            (true, true) => self.browsing = false,
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
            .get_mut(self.server)
            .and_then(|s| s.teams.get_mut(self.team))
        else {
            return;
        };
        let Some(count) = team.chans.as_ref().map(Vec::len).filter(|&n| n > 0) else {
            return;
        };
        team.cur = team.cur.saturating_add_signed(step).min(count - 1);
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
            }
            Loaded::Channels {
                team,
                channels,
                members,
                categories,
            } => {
                let chans = model::order_channels(channels, members, categories.as_ref());
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
                if current.as_ref() == Some(&channel) && self.browsing {
                    let old = server
                        .messages
                        .get(&channel)
                        .and_then(|m| m.get(self.cursor));
                    let at = old.and_then(|o| messages.iter().position(|m| m.id == o.id));
                    self.cursor = at.unwrap_or(self.cursor);
                }
                server.messages.insert(channel, messages);
                self.fetch_users(i, ids);
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
        self.refresh_channels(i);
        if let Some(id) = current {
            self.fetch_posts(i, id);
        }
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
        let (mut touched, mut removed, mut refresh) = (None, None, false);
        let mut ids = Vec::new();
        match event {
            Event::Posted { post, mentions } => {
                let channel = post.channel_id.clone();
                let mine = me.as_ref() == Some(&post.user_id);
                let mentioned = !mine && me.as_ref().is_some_and(|m| mentions.contains(m));
                let mut known = false;
                let chans = server
                    .teams
                    .iter_mut()
                    .flat_map(|t| t.chans.iter_mut().flatten());
                for chan in chans.filter(|c| c.info.id == channel) {
                    known = true;
                    chan.info.last_post_at = chan.info.last_post_at.max(post.create_at);
                    if let (true, Some(member)) = (mentioned, chan.member.as_mut()) {
                        member.mention_count += 1;
                    }
                }
                if mine {
                    let seen = server.seen.entry(channel.clone()).or_insert(0);
                    *seen = (*seen).max(post.create_at);
                }
                refresh = !known && server.refreshed.insert(channel.clone());
                ids = server.unknown(std::iter::once(&post.user_id));
                if let Some(list) = server.messages.get_mut(&channel) {
                    model::insert_message(list, Message::from_post(&post));
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
            Event::Viewed(channel) => {
                let chans = server
                    .teams
                    .iter_mut()
                    .flat_map(|t| t.chans.iter_mut().flatten());
                for model::Chan { info, member } in chans.filter(|c| c.info.id == channel) {
                    if let Some(member) = member {
                        member.last_viewed_at = member.last_viewed_at.max(info.last_post_at);
                        member.mention_count = 0;
                    }
                }
            }
        }
        if refresh {
            self.refresh_channels(i);
        }
        self.fetch_users(i, ids);
        if touched.is_some() && touched == current {
            let after = self.count();
            match removed {
                Some(at) => {
                    self.cursor = self.cursor.saturating_sub(usize::from(at < self.cursor));
                    self.cursor = self.cursor.min(after.saturating_sub(1));
                }
                None if after > before && at_end => {
                    self.cursor = after - 1;
                    match self.browsing {
                        true => self.follow = true,
                        false => self.scroll = usize::MAX,
                    }
                }
                None => {}
            }
        }
        if i == self.server {
            self.ensure();
        }
    }

    fn mark_seen(&mut self) {
        let Some(server) = self.servers.get_mut(self.server) else {
            return;
        };
        let chan = server
            .teams
            .get(self.team)
            .and_then(|t| t.chans.as_ref()?.get(t.cur));
        let Some((id, last_post)) = chan.map(|c| (c.info.id.clone(), c.info.last_post_at)) else {
            return;
        };
        let Some(newest) = server
            .messages
            .get(&id)
            .map(|m| m.last().map_or(0, |m| m.ms))
        else {
            return;
        };
        let seen = server.seen.entry(id).or_insert(0);
        *seen = (*seen).max(last_post.max(newest));
    }

    fn ensure(&mut self) {
        self.mark_seen();
        let (i, t) = (self.server, self.team);
        let Some(server) = self.servers.get_mut(i) else {
            return;
        };
        if server.client.is_none() {
            return;
        }
        let Some(team) = server.teams.get_mut(t) else {
            return;
        };
        if team.chans.is_none() {
            if !team.loading {
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
        if !server.messages.contains_key(&id) {
            self.fetch_posts(i, id);
        }
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
            match client.posts(&id, None, POSTS_PER_PAGE).await {
                Ok(list) => Loaded::Posts { channel: id, list },
                Err(e) => Loaded::Failed(e.to_string()),
            }
        });
    }

    fn start_ws(&self, i: usize) {
        let Some(client) = self.servers.get(i).and_then(|s| s.client.clone()) else {
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(ws::run(client, ws::BACKOFF, move |event| {
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
    use tokio::sync::mpsc::unbounded_channel;

    use super::*;
    use crate::api::types::{Post, PostList, Reaction};
    use crate::ui::tests::screen;

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
        let (tx, rx) = unbounded_channel();
        std::mem::forget(rx);
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
        app
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

    fn post(id: &str, channel: &str, user: &str, text: &str, ms: i64) -> Post {
        serde_json::from_value(serde_json::json!({
            "id": id, "create_at": ms, "user_id": user, "channel_id": channel, "message": text
        }))
        .unwrap()
    }

    fn posted(app: &mut App, post: Post, mentions: &[&str]) {
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

    #[test]
    fn what_was_read_here_stays_read_after_a_refresh() {
        let mut app = fixture();
        press(&mut app, "J");
        let list: PostList = json("posts");
        app.loaded(
            0,
            Loaded::Posts {
                channel: ChannelId("c1".into()),
                list,
            },
        );
        assert!(tabs(&mut app).contains(" #Town Square "));
        press(&mut app, "K");
        app.loaded(
            0,
            Loaded::Channels {
                team: TeamId("t1".into()),
                channels: json("channels"),
                members: json("members"),
                categories: Some(json("categories")),
            },
        );
        assert!(tabs(&mut app).contains(" #Town Square "));
        assert_eq!(app.servers[0].teams[0].cur, 0);
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
}
