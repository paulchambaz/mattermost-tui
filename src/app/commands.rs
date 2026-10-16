//! The commands typed after `:`, each a typed route to something a key does or to a server action.

use std::cmp::Reverse;
use std::sync::Arc;

use super::{App, Loaded};
use crate::api::types::{ChannelId, ChannelKind, Status, UserId};
use crate::api::{Client, Error};
use crate::config;
use crate::finder;
use crate::input::{self, Command, Line};
use crate::markdown;
use crate::model::Server;

const SETTINGS: &str = "editor copy open.image open.video open.browser open.text open.other";

/// The names that fit what is typed after a command, best first, with the letters that matched.
pub struct Completion {
    pub command: &'static str,
    pub names: Vec<(String, Vec<usize>)>,
}

impl App {
    /// What the command line offers to complete, for the commands that take a name.
    pub fn completion(&self) -> Option<Completion> {
        if self.wizard.is_some() {
            return None;
        }
        let text = self.input.as_ref()?.text();
        let Some((word, rest)) = text.split_once(' ') else {
            let base = self.cycle.as_deref().unwrap_or(&text);
            let hits: Vec<usize> = (0..base.chars().count()).collect();
            let names = input::starting(base)
                .into_iter()
                .map(|name| (name.to_owned(), hits.clone()))
                .collect();
            return Some(Completion { command: "", names });
        };
        let word = match input::matching(word).as_slice() {
            [only] => *only,
            _ => return None,
        };
        let rest = rest.trim().trim_start_matches('@');
        let server = self.servers.get(self.server);
        let (command, pool): (&'static str, Vec<String>) = match word {
            "server" | "remove-server" => (
                if word == "server" {
                    "server"
                } else {
                    "remove-server"
                },
                self.server_names().into_iter().map(|(_, n)| n).collect(),
            ),
            "team" => (
                "team",
                server
                    .iter()
                    .flat_map(|s| s.teams.iter().map(|t| t.info.display_name.clone()))
                    .collect(),
            ),
            "dm" => {
                let me = server.and_then(|s| s.me.as_ref()).map(|m| &m.id);
                let mut names: Vec<String> = server
                    .iter()
                    .flat_map(|s| s.users.values())
                    .filter(|u| Some(&u.id) != me)
                    .map(|u| u.username.clone())
                    .collect();
                names.sort();
                ("dm", names)
            }
            _ => return None,
        };
        let mut found: Vec<(i32, String, Vec<usize>)> = pool
            .into_iter()
            .filter_map(|name| {
                let (score, hits) = finder::fuzzy(rest, &name)?;
                Some((score, name, hits))
            })
            .collect();
        found.sort_by_key(|(score, ..)| Reverse(*score));
        let names = found.into_iter().map(|(_, n, h)| (n, h)).collect();
        Some(Completion { command, names })
    }

    /// Moves through the completion: a name is chosen, or a command is written in full.
    pub(super) fn tab(&mut self, back: bool) {
        let Some(offered) = self.completion() else {
            return;
        };
        let count = offered.names.len();
        if count == 0 {
            return;
        }
        let step = |from: usize| match back {
            true => (from + count - 1) % count,
            false => (from + 1) % count,
        };
        let from = self.choice.min(count - 1);
        if !offered.command.is_empty() {
            self.choice = step(from);
            return;
        }
        let next = match (&self.cycle, back) {
            (Some(_), _) => step(from),
            (None, false) => 0,
            (None, true) => count - 1,
        };
        let base = self
            .cycle
            .take()
            .or_else(|| self.input.as_ref().map(Line::text));
        self.input = Some(Line::with_text(&offered.names[next].0, false));
        (self.choice, self.cycle) = (next, base);
    }

    /// The line to run: as typed, or with the name chosen in the completion.
    pub(super) fn completed(&self, typed: &str) -> String {
        match self.completion() {
            Some(c) if !c.command.is_empty() && !c.names.is_empty() => {
                let chosen = &c.names[self.choice.min(c.names.len() - 1)].0;
                format!("{} {chosen}", c.command)
            }
            _ => typed.to_owned(),
        }
    }

    /// Runs one parsed command.
    pub(super) fn run_command(&mut self, command: Command) {
        match command {
            Command::NewServer => self.new_server(),
            Command::Help => self.help = Some(0),
            Command::Quit => self.quit = true,
            Command::Server(name) => self.go_server(&name),
            Command::Team(name) => self.go_team(&name),
            Command::Go(words) => self.go(&words),
            Command::RemoveServer(name) => self.remove_server(&name),
            Command::Reload => self.reload(),
            Command::MarkRead => self.mark_read(),
            Command::MarkAllRead => self.mark_all_read(),
            Command::Users => self.toggle_users(),
            Command::Reactions => self.reactions = !self.reactions,
            Command::Open => self.open_selected(),
            Command::Copy(what) => match what.as_str() {
                "" => self.yank(false),
                "full" => self.yank(true),
                _ => self.notice = "Usage: :copy [full]".into(),
            },
            Command::React(name) => self.react_named(&name),
            Command::Set(key, value) => self.set(&key, &value),
            Command::Topic => self.topic(),
            Command::Dm(name) => self.dm(&name),
            Command::Join(name) => self.join(&name),
            Command::Leave => self.leave(),
            Command::Status(word) => self.set_status(&word),
            Command::Pin => self.pin(true),
            Command::Unpin => self.pin(false),
        }
    }

    fn pick(
        &mut self,
        (command, kind): (&str, &str),
        names: &[(usize, String)],
        wanted: &str,
    ) -> Option<usize> {
        if wanted.is_empty() {
            self.notice = format!("Usage: :{command} <name>");
            return None;
        }
        let wanted = wanted.to_lowercase();
        if let Some((i, _)) = names.iter().find(|(_, n)| n.to_lowercase() == wanted) {
            return Some(*i);
        }
        let hits: Vec<&(usize, String)> = names
            .iter()
            .filter(|(_, n)| n.to_lowercase().contains(&wanted))
            .collect();
        match hits.as_slice() {
            [(i, _)] => Some(*i),
            [] => {
                self.notice = format!("No {kind} matches {wanted}");
                None
            }
            many => {
                let all: Vec<&str> = many.iter().map(|(_, n)| n.as_str()).collect();
                self.notice = format!("Several match: {}", all.join(", "));
                None
            }
        }
    }

    fn server_names(&self) -> Vec<(usize, String)> {
        let live = self
            .servers
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_gone());
        live.map(|(i, s)| (i, s.name.clone())).collect()
    }

    fn go_server(&mut self, name: &str) {
        let names = self.server_names();
        if let Some(i) = self.pick(("server", "server"), &names, name) {
            (self.server, self.team) = (i, 0);
            self.reset_view();
            self.ensure();
        }
    }

    fn go_team(&mut self, name: &str) {
        let Some(server) = self.servers.get(self.server) else {
            return;
        };
        let names: Vec<(usize, String)> = server
            .teams
            .iter()
            .enumerate()
            .map(|(t, team)| (t, team.info.display_name.clone()))
            .collect();
        if let Some(t) = self.pick(("team", "team"), &names, name) {
            self.team = t;
            self.reset_view();
            self.ensure();
        }
    }

    fn go(&mut self, text: &str) {
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.is_empty() {
            self.notice = "Usage: :go <words>".into();
            return;
        }
        let here = (self.server, self.team);
        let candidates = finder::candidates(&self.servers, here, false);
        let found = finder::entries(&words, &candidates, false);
        let target = found
            .first()
            .map(|e| (e.candidate.server, e.candidate.team, e.candidate.chan));
        match target {
            Some((server, team, chan)) => {
                (self.server, self.team) = (server, team);
                self.go_channel(chan);
            }
            None => self.notice = format!("No channel matches {text}"),
        }
    }

    fn remove_server(&mut self, name: &str) {
        let names = self.server_names();
        let Some(i) = self.pick(("remove-server", "server"), &names, name) else {
            return;
        };
        let old = std::mem::replace(&mut self.servers[i], Server::new(String::new()));
        if let Some(task) = &old.ws {
            task.abort();
        }
        self.configs.retain(|c| c.name != old.name);
        self.notice = match config::save(&self.path, &self.configs) {
            Ok(()) => format!("Removed {}", old.name),
            Err(e) => format!(
                "Removed {}, but the server list was not saved: {e}",
                old.name
            ),
        };
        if self.entry.as_ref().is_some_and(|e| e.server == i) {
            self.entry = None;
        }
        self.drafts.retain(|(server, _), _| *server != i);
        if self.server == i {
            if let Some(&(server, team)) = self.places().first() {
                (self.server, self.team) = (server, team);
                self.reset_view();
                self.ensure();
            }
        }
    }

    fn reload(&mut self) {
        let Some((server, chan)) = self.channel() else {
            return;
        };
        let id = chan.info.id.clone();
        let Some(team) = server.teams.get(self.team).map(|t| t.info.id.clone()) else {
            return;
        };
        let i = self.server;
        if let Some(server) = self.servers.get_mut(i) {
            server.messages.remove(&id);
            server.complete.remove(&id);
            server.refreshed.remove(&id);
        }
        self.fetch_channels(i, team);
        self.fetch_posts(i, id);
        self.reset_view();
        self.notice = "Reloading…".into();
    }

    fn mark_all_read(&mut self) {
        let i = self.server;
        let Some(team) = self.servers.get(i).and_then(|s| s.teams.get(self.team)) else {
            return;
        };
        let unread: Vec<ChannelId> = team
            .chans
            .iter()
            .flatten()
            .filter(|c| c.unread())
            .map(|c| c.info.id.clone())
            .collect();
        for id in &unread {
            self.servers[i].mark_viewed(id);
            self.send_view(i, id.clone(), None);
        }
        if let Some(entry) = &mut self.entry {
            if entry.server == i && unread.contains(&entry.channel) {
                entry.unread = 0;
                entry.boundary = None;
            }
        }
        self.notice = match unread.len() {
            0 => "Nothing unread".into(),
            1 => "Marked 1 channel as read".into(),
            n => format!("Marked {n} channels as read"),
        };
    }

    fn react_named(&mut self, name: &str) {
        let name = name.trim_matches(':');
        if name.is_empty() {
            self.notice = "Usage: :react <emoji>".into();
            return;
        }
        let shortcode = format!(":{name}:");
        if markdown::emoji(&shortcode) == shortcode {
            self.notice = format!("Unknown emoji {name}");
            return;
        }
        self.react(name);
    }

    fn set(&mut self, key: &str, value: &str) {
        let optional = (!value.is_empty()).then(|| value.to_owned());
        let open = &mut self.settings.open;
        match key {
            "editor" => self.settings.editor = optional,
            "copy" => self.settings.copy = optional,
            "open.image" | "open.video" | "open.browser" | "open.text" | "open.other"
                if value.is_empty() =>
            {
                self.notice = format!("Usage: :set {key} <program>");
                return;
            }
            "open.image" => open.image = value.to_owned(),
            "open.video" => open.video = value.to_owned(),
            "open.browser" => open.browser = value.to_owned(),
            "open.text" => open.text = value.to_owned(),
            "open.other" => open.other = value.to_owned(),
            _ => {
                self.notice = format!("Settings: {SETTINGS}");
                return;
            }
        }
        self.notice = match value.is_empty() {
            true => format!("{key} reset"),
            false => format!("{key} = {value}"),
        };
    }

    fn topic(&mut self) {
        let Some((_, chan)) = self.channel() else {
            return;
        };
        let text: Vec<&str> = [&chan.info.header, &chan.info.purpose]
            .into_iter()
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .collect();
        self.notice = match text.is_empty() {
            true => "No topic".into(),
            false => text.join(" · ").replace('\n', " "),
        };
    }

    fn session(&mut self) -> Option<(Arc<Client>, UserId)> {
        let server = self.servers.get(self.server)?;
        let found = server
            .client
            .clone()
            .zip(server.me.as_ref().map(|m| m.id.clone()));
        if found.is_none() {
            self.notice = "Not connected".into();
        }
        found
    }

    fn dm(&mut self, name: &str) {
        let name = name.trim_start_matches('@').to_owned();
        if name.is_empty() {
            self.notice = "Usage: :dm <user>".into();
            return;
        }
        let Some((client, me)) = self.session() else {
            return;
        };
        self.spawn(self.server, async move {
            let opened = async {
                let user = client.user_named(&name).await?;
                client.create_direct(&me, &user.id).await
            };
            opened
                .await
                .map_or_else(|e| Loaded::Failed(e.to_string()), |c| Loaded::Opened(c.id))
        });
    }

    fn join(&mut self, name: &str) {
        let name = name.trim_start_matches('#').to_owned();
        if name.is_empty() {
            self.notice = "Usage: :join <channel>".into();
            return;
        }
        let team = self
            .servers
            .get(self.server)
            .and_then(|s| s.teams.get(self.team))
            .map(|t| t.info.id.clone());
        let (Some(team), Some((client, me))) = (team, self.session()) else {
            return;
        };
        self.spawn(self.server, async move {
            let joined = async {
                let channel = client.channel_named(&team, &name).await?;
                client.join(&channel.id, &me).await?;
                Ok::<_, Error>(channel.id)
            };
            joined
                .await
                .map_or_else(|e| Loaded::Failed(e.to_string()), Loaded::Opened)
        });
    }

    fn leave(&mut self) {
        let Some((server, chan)) = self.channel() else {
            return;
        };
        if matches!(chan.info.kind, ChannelKind::Direct | ChannelKind::Group) {
            self.notice = "Direct and group conversations cannot be left".into();
            return;
        }
        let label = server.label(chan);
        let id = chan.info.id.clone();
        let Some((client, me)) = self.session() else {
            return;
        };
        self.spawn(self.server, async move {
            match client.leave(&id, &me).await {
                Ok(()) => Loaded::Left(label),
                Err(e) => Loaded::Failed(e.to_string()),
            }
        });
    }

    fn set_status(&mut self, word: &str) {
        let status = match word {
            "online" | "away" | "dnd" | "offline" => Status::parse(word),
            _ => {
                self.notice = "Usage: :status online|away|dnd|offline".into();
                return;
            }
        };
        let Some((client, me)) = self.session() else {
            return;
        };
        self.spawn(self.server, async move {
            match client.set_status(&me, status).await {
                Ok(()) => Loaded::StatusSet(status),
                Err(e) => Loaded::Failed(e.to_string()),
            }
        });
    }

    fn pin(&mut self, pinned: bool) {
        let Some(target) = self.target() else {
            return;
        };
        let Some((client, _)) = self.session() else {
            return;
        };
        self.spawn(self.server, async move {
            match client.pin(&target.id, pinned).await {
                Ok(()) => Loaded::Said(if pinned { "Pinned" } else { "Unpinned" }.into()),
                Err(e) => Loaded::Failed(e.to_string()),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;
    use wiremock::matchers::{any, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::types::TeamId;
    use crate::app::tests::{
        fixture, fixture_rx, json, live, press, press_code, sent, settle, views, writes,
    };
    use crate::app::Msg;
    use crate::config::{Credentials, ServerConfig};
    use crate::model::Team;

    fn run(app: &mut App, text: &str) {
        press(app, &format!(":{text}"));
        press_code(app, KeyCode::Enter);
    }

    fn here(app: &App) -> String {
        app.channel().unwrap().1.info.id.0.clone()
    }

    fn team(name: &str) -> Team {
        Team {
            info: serde_json::from_value(
                serde_json::json!({"id": name, "name": name, "display_name": name}),
            )
            .unwrap(),
            chans: None,
            loading: false,
            cur: 0,
        }
    }

    #[test]
    fn servers_and_teams_are_picked_by_name_and_ambiguity_is_reported() {
        let mut app = fixture();
        app.servers.push(Server::new("club".into()));
        app.servers.push(Server::new("clubby".into()));
        app.run_command(Command::Server("cl".into()));
        assert_eq!(app.notice, "Several match: club, clubby");
        run(&mut app, "server CLUB");
        assert_eq!(app.server, 1);
        run(&mut app, "server wor");
        assert_eq!((app.server, app.team), (0, 0));
        run(&mut app, "server zzz");
        assert_eq!(app.notice, "No server matches zzz");
        run(&mut app, "server");
        assert_eq!(app.notice, "Usage: :server <name>");
        app.servers[0].teams.push(team("Design"));
        app.run_command(Command::Team("e".into()));
        assert_eq!(app.notice, "Several match: Engineering, Design");
        run(&mut app, "team desi");
        assert_eq!(app.team, 1);
    }

    #[test]
    fn typing_after_a_command_offers_names_and_enter_takes_the_chosen_one() {
        let mut app = fixture();
        app.servers.push(Server::new("club".into()));
        app.servers.push(Server::new("clubby".into()));
        press(&mut app, ":server cl");
        let offered = app.completion().unwrap();
        let names: Vec<&str> = offered.names.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["club", "clubby"]);
        press_code(&mut app, KeyCode::Tab);
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(app.server, 2);
        assert_eq!(app.choice, 0);
        press(&mut app, ":server ");
        press_code(&mut app, KeyCode::BackTab);
        assert_eq!(app.choice, 2);
        press(&mut app, "x");
        assert_eq!(app.choice, 0);
        assert!(app.completion().unwrap().names.is_empty());
        press_code(&mut app, KeyCode::Esc);
        assert!(app.completion().is_none());
    }

    #[test]
    fn teams_and_users_are_offered_too() {
        let mut app = fixture();
        app.servers[0].teams.push(team("Design"));
        press(&mut app, ":team ");
        assert_eq!(app.completion().unwrap().names[0].0, "Design");
        press_code(&mut app, KeyCode::Enter);
        assert_eq!(app.team, 1);
        app.servers[0].users.insert(
            UserId("u9".into()),
            crate::app::tests::json_user("u9", "zoe"),
        );
        press(&mut app, ":dm @z");
        let offered = app.completion().unwrap();
        assert_eq!(offered.command, "dm");
        assert_eq!(offered.names[0].0, "zoe");
        assert!(offered.names.iter().all(|(n, _)| n != "paul"));
        press_code(&mut app, KeyCode::Esc);
        press(&mut app, ":copy f");
        assert!(app.completion().is_none());
    }

    #[test]
    fn a_command_works_from_its_shortest_start_and_ambiguity_is_said() {
        let mut app = fixture();
        run(&mut app, "to");
        assert_eq!(app.notice, "No topic");
        run(&mut app, "mark");
        assert_eq!(app.notice, "Ambiguous: mark-read, mark-all-read");
        run(&mut app, "zz");
        assert_eq!(app.notice, "Unknown command: zz");
        app.servers.push(Server::new("club".into()));
        run(&mut app, "ser clu");
        assert_eq!(app.server, 1);
        run(&mut app, "q");
        assert!(app.quit);
    }

    #[test]
    fn tab_writes_the_command_in_full_then_cycles_and_names_follow() {
        let mut app = fixture();
        press(&mut app, ":re");
        let offered = app.completion().unwrap();
        assert_eq!(offered.command, "");
        let names: Vec<&str> = offered.names.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["remove-server", "reload", "reactions", "react"]);
        press_code(&mut app, KeyCode::Tab);
        assert_eq!(app.input.as_ref().unwrap().text(), "remove-server");
        press_code(&mut app, KeyCode::Tab);
        assert_eq!(app.input.as_ref().unwrap().text(), "reload");
        press(&mut app, " x");
        assert_eq!(app.input.as_ref().unwrap().text(), "reload x");
        press_code(&mut app, KeyCode::Esc);
        press(&mut app, ":server ");
        assert_eq!(app.completion().unwrap().command, "server");
        press_code(&mut app, KeyCode::Esc);
        press_code(&mut app, KeyCode::Esc);
        press(&mut app, ":r");
        press_code(&mut app, KeyCode::Tab);
        press_code(&mut app, KeyCode::Tab);
        assert_eq!(app.input.as_ref().unwrap().text(), "reload");
        press_code(&mut app, KeyCode::Tab);
        assert_eq!(app.input.as_ref().unwrap().text(), "reactions");
        press_code(&mut app, KeyCode::BackTab);
        assert_eq!(app.input.as_ref().unwrap().text(), "reload");
        press(&mut app, "x");
        assert!(app.cycle.is_none());
        press_code(&mut app, KeyCode::Esc);
        press(&mut app, ":");
        assert_eq!(app.completion().unwrap().names.len(), input::COMMANDS.len());
    }

    #[test]
    fn go_opens_the_best_channel_of_the_team() {
        let mut app = fixture();
        run(&mut app, "go town");
        assert_eq!(here(&app), "c1");
        run(&mut app, "go zzz");
        assert_eq!(app.notice, "No channel matches zzz");
        run(&mut app, "go");
        assert_eq!(app.notice, "Usage: :go <words>");
    }

    #[test]
    fn set_changes_a_setting_for_the_session_only() {
        let mut app = fixture();
        run(&mut app, "set editor nvim -R");
        assert_eq!(app.settings.editor.as_deref(), Some("nvim -R"));
        assert_eq!(app.notice, "editor = nvim -R");
        run(&mut app, "set editor");
        assert_eq!(app.settings.editor, None);
        assert_eq!(app.notice, "editor reset");
        run(&mut app, "set open.image imv");
        assert_eq!(app.settings.open.image, "imv");
        run(&mut app, "set open.video");
        assert_eq!(app.notice, "Usage: :set open.video <program>");
        assert_eq!(app.settings.open.video, "true");
        run(&mut app, "set bogus 1");
        assert!(app.notice.starts_with("Settings: editor copy"));
    }

    #[test]
    fn small_commands_do_what_their_keys_do() {
        let mut app = fixture();
        run(&mut app, "topic");
        assert_eq!(app.notice, "No topic");
        let info = &mut app.servers[0].teams[0].chans.as_mut().unwrap()[0].info;
        info.header = "Be\nnice".into();
        info.purpose = "Chat".into();
        run(&mut app, "topic");
        assert_eq!(app.notice, "Be nice · Chat");
        let before = app.reactions;
        run(&mut app, "reactions");
        assert_eq!(app.reactions, !before);
        run(&mut app, "react not-an-emoji");
        assert_eq!(app.notice, "Unknown emoji not-an-emoji");
        run(&mut app, "copy what");
        assert_eq!(app.notice, "Usage: :copy [full]");
        run(&mut app, "copy full");
        assert!(app.notice.starts_with("Copied"), "{}", app.notice);
        run(&mut app, "nope");
        assert_eq!(app.notice, "Unknown command: nope");
    }

    #[test]
    fn removing_a_server_forgets_it_and_keeps_every_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = fixture();
        app.path = dir.path().join("servers.toml");
        app.servers.push(Server::new("club".into()));
        app.configs.push(ServerConfig {
            name: "club".into(),
            url: "https://club.example.net".into(),
            login: String::new(),
            password: None,
            password_cmd: None,
            token: Some("t".into()),
            token_cmd: None,
        });
        run(&mut app, "remove-server club");
        let saved = config::load(&app.path).unwrap();
        assert_eq!(
            saved.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["work"]
        );
        assert_eq!(app.servers.len(), 2);
        assert!(app.servers[1].is_gone());
        app.loaded(1, Loaded::Failed("late".into()));
        assert_ne!(app.notice, ": late");
        run(&mut app, "remove-server work");
        assert!(config::load(&app.path).unwrap().is_empty());
        assert!(app.places().is_empty());
        run(&mut app, "server wor");
        assert_eq!(app.notice, "No server matches wor");
        run(&mut app, "remove-server nobody");
        assert_eq!(app.notice, "No server matches nobody");
    }

    #[tokio::test]
    async fn mark_all_read_views_each_unread_channel_once_and_writes_nothing_else() {
        let (mut app, server) = live().await;
        run(&mut app, "mark-all-read");
        assert_eq!(app.notice, "Marked 1 channel as read");
        let sent = sent(&server).await;
        let wanted = [serde_json::json!({"channel_id": "c1", "prev_channel_id": ""})];
        assert_eq!(views(&sent), wanted);
        assert!(writes(&sent).is_empty());
        run(&mut app, "mark-all-read");
        assert_eq!(app.notice, "Nothing unread");
        assert_eq!(views(&crate::app::tests::sent(&server).await).len(), 1);
    }

    async fn commanding() -> (App, tokio::sync::mpsc::UnboundedReceiver<Msg>, MockServer) {
        let server = MockServer::start().await;
        let one = |status: u16, body: serde_json::Value| {
            ResponseTemplate::new(status).set_body_json(body)
        };
        let raw = |name: &str| {
            let text = std::fs::read_to_string(format!("tests/fixtures/{name}.json")).unwrap();
            serde_json::from_str::<serde_json::Value>(&text).unwrap()
        };
        let all = raw("channels");
        for (method_name, route, response) in [
            (
                "GET",
                "/api/v4/users/username/ana",
                one(200, serde_json::json!({"id": "u2", "username": "ana"})),
            ),
            ("POST", "/api/v4/channels/direct", one(201, all[1].clone())),
            (
                "GET",
                "/api/v4/teams/t1/channels/name/town-square",
                one(200, all[0].clone()),
            ),
            (
                "GET",
                "/api/v4/teams/t1/channels/name/nowhere",
                one(404, serde_json::json!({"message": "nope"})),
            ),
            (
                "POST",
                "/api/v4/channels/c1/members",
                one(201, serde_json::json!({})),
            ),
            (
                "GET",
                "/api/v4/users/me/teams/t1/channels",
                one(200, all.clone()),
            ),
            (
                "GET",
                "/api/v4/users/me/teams/t1/channels/members",
                one(200, raw("members")),
            ),
            (
                "GET",
                "/api/v4/users/me/teams/t1/channels/categories",
                one(200, raw("categories")),
            ),
        ] {
            Mock::given(method(method_name))
                .and(path(route))
                .respond_with(response)
                .mount(&server)
                .await;
        }
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

    #[tokio::test]
    async fn server_commands_send_exactly_their_requests() {
        let (mut app, mut rx, server) = commanding().await;

        run(&mut app, "dm @ana");
        settle(&mut app, &mut rx, |a| here(a) == "c2").await;
        run(&mut app, "leave");
        assert_eq!(app.notice, "Direct and group conversations cannot be left");

        run(&mut app, "join #town-square");
        settle(&mut app, &mut rx, |a| here(a) == "c1").await;

        run(&mut app, "go secret");
        run(&mut app, "leave");
        settle(&mut app, &mut rx, |a| a.notice.starts_with("Left")).await;
        assert_eq!(app.notice, "Left +Secret");

        run(&mut app, "status away");
        settle(&mut app, &mut rx, |a| a.notice == "Status: away").await;
        assert_eq!(
            app.servers[0]
                .status
                .get(&app.servers[0].me.as_ref().unwrap().id),
            Some(&Status::Away)
        );
        run(&mut app, "status busy");
        assert_eq!(app.notice, "Usage: :status online|away|dnd|offline");

        let post = app.target().map(|t| t.id.0);
        run(&mut app, "pin");
        settle(&mut app, &mut rx, |a| a.notice == "Pinned").await;
        run(&mut app, "unpin");
        settle(&mut app, &mut rx, |a| a.notice == "Unpinned").await;

        run(&mut app, "join nowhere");
        settle(&mut app, &mut rx, |a| a.notice.contains("nope")).await;
        assert_eq!(app.notice, "work: server answered 404: nope");

        let post = post.unwrap();
        let requests = writes(&sent(&server).await);
        let wanted = [
            (
                "POST /api/v4/channels/direct".to_owned(),
                r#"["u1","u2"]"#.to_owned(),
            ),
            (
                "POST /api/v4/channels/c1/members".to_owned(),
                r#"{"user_id":"u1"}"#.to_owned(),
            ),
            (
                "DELETE /api/v4/channels/c3/members/u1".to_owned(),
                String::new(),
            ),
            (
                "PUT /api/v4/users/u1/status".to_owned(),
                r#"{"user_id":"u1","status":"away"}"#.to_owned(),
            ),
            (format!("POST /api/v4/posts/{post}/pin"), String::new()),
            (format!("POST /api/v4/posts/{post}/unpin"), String::new()),
        ];
        assert_eq!(requests, wanted);
    }

    #[test]
    fn a_channel_missing_from_the_sidebar_is_said_so() {
        let mut app = fixture();
        app.go = Some(crate::api::types::ChannelId("c4".into()));
        app.loaded(
            0,
            Loaded::Channels {
                team: TeamId("t1".into()),
                channels: json("channels"),
                members: json("members"),
                categories: Some(json("categories")),
            },
        );
        assert_eq!(app.notice, "That channel is not in your sidebar");
        assert!(app.go.is_none());
    }
}
