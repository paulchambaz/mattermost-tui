//! The one-line editor, the command parser and the new-server wizard.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::ServerConfig;

/// A line of text being typed, which can show itself masked.
#[derive(Debug, Default)]
pub struct Line {
    chars: Vec<char>,
    cursor: usize,
    masked: bool,
}

impl Line {
    /// An empty line, masked when it holds a secret.
    pub fn new(masked: bool) -> Self {
        Self {
            masked,
            ..Self::default()
        }
    }

    /// A line that already holds `text`, with the cursor after it.
    pub fn with_text(text: &str, masked: bool) -> Self {
        let mut line = Self::new(masked);
        line.insert_str(text);
        line
    }

    /// Inserts text at the cursor, as a paste does.
    pub fn insert_str(&mut self, text: &str) {
        let added: Vec<char> = text
            .chars()
            .filter(|c| *c == '\n' || !c.is_control())
            .collect();
        let count = added.len();
        self.chars.splice(self.cursor..self.cursor, added);
        self.cursor += count;
    }

    /// What was typed.
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// The text as it appears on screen: bullets for a secret.
    pub fn shown(&self) -> String {
        match self.masked {
            true => self.chars.iter().map(|_| '•').collect(),
            false => self
                .chars
                .iter()
                .map(|&c| if c == '\n' { '⏎' } else { c })
                .collect(),
        }
    }

    /// Cursor position in characters.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Applies an editing key; false when the key edits nothing.
    pub fn edit(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.chars.len(),
            KeyCode::Char('u') if ctrl => {
                self.chars.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let start = self.chars[..self.cursor]
                    .iter()
                    .rposition(|c| !c.is_whitespace())
                    .map_or(0, |i| {
                        self.chars[..=i]
                            .iter()
                            .rposition(|c| c.is_whitespace())
                            .map_or(0, |j| j + 1)
                    });
                self.chars.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char(c) if !ctrl => {
                self.chars.insert(self.cursor, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.chars.len(),
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        true
    }
}

/// What a `:` command asks for; the text is the argument, empty when none was given.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    NewServer,
    Help,
    Quit,
    Server(String),
    Team(String),
    Go(String),
    RemoveServer(String),
    Reload,
    MarkRead,
    MarkAllRead,
    Users,
    Reactions,
    Open,
    Copy(String),
    React(String),
    Set(String, String),
    Topic,
    Dm(String),
    Join(String),
    Leave,
    Status(String),
    Pin,
    Unpin,
}

/// Every command with whether it takes an argument, in the order the help lists them.
pub const COMMANDS: &[(&str, bool)] = &[
    ("new-server", false),
    ("remove-server", true),
    ("server", true),
    ("team", true),
    ("go", true),
    ("dm", true),
    ("join", true),
    ("leave", false),
    ("topic", false),
    ("reload", false),
    ("mark-read", false),
    ("mark-all-read", false),
    ("users", false),
    ("reactions", false),
    ("react", true),
    ("pin", false),
    ("unpin", false),
    ("open", false),
    ("copy", true),
    ("status", true),
    ("set", true),
    ("help", false),
    ("quit", false),
];

/// The commands whose name starts with `word`.
pub fn starting(word: &str) -> Vec<&'static str> {
    COMMANDS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| name.starts_with(word))
        .collect()
}

/// The commands `word` can mean: the one it spells out, else those it is the start of.
pub fn matching(word: &str) -> Vec<&'static str> {
    match COMMANDS.iter().find(|(name, _)| *name == word) {
        Some((name, _)) => vec![name],
        None => starting(word),
    }
}

/// Reads a command and its argument; none when the name is not one or is the start of several.
pub fn parse(input: &str) -> Option<Command> {
    let input = input.trim();
    let (name, rest) = input.split_once(char::is_whitespace).unwrap_or((input, ""));
    let name = match matching(name).as_slice() {
        [only] => *only,
        _ => return None,
    };
    let rest = rest.trim().to_owned();
    Some(match name {
        "new-server" => Command::NewServer,
        "help" => Command::Help,
        "quit" => Command::Quit,
        "server" => Command::Server(rest),
        "team" => Command::Team(rest),
        "go" => Command::Go(rest),
        "remove-server" => Command::RemoveServer(rest),
        "reload" => Command::Reload,
        "mark-read" => Command::MarkRead,
        "mark-all-read" => Command::MarkAllRead,
        "users" => Command::Users,
        "reactions" => Command::Reactions,
        "open" => Command::Open,
        "copy" => Command::Copy(rest),
        "react" => Command::React(rest),
        "set" => {
            let (key, value) = rest.split_once(char::is_whitespace).unwrap_or((&rest, ""));
            Command::Set(key.to_owned(), value.trim().to_owned())
        }
        "topic" => Command::Topic,
        "dm" => Command::Dm(rest),
        "join" => Command::Join(rest),
        "leave" => Command::Leave,
        "status" => Command::Status(rest),
        "pin" => Command::Pin,
        "unpin" => Command::Unpin,
        _ => return None,
    })
}

/// The questions of the new-server wizard, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Name,
    Url,
    Login,
    Password,
    Mfa,
}

impl Step {
    /// The question written before the input.
    pub fn prompt(self) -> &'static str {
        match self {
            Self::Name => "Name: ",
            Self::Url => "URL: ",
            Self::Login => "Login: ",
            Self::Password => "Password: ",
            Self::Mfa => "MFA code: ",
        }
    }

    /// Whether the answer is a secret.
    pub fn masked(self) -> bool {
        matches!(self, Self::Password | Self::Mfa)
    }
}

/// The answers collected so far.
#[derive(Debug)]
pub struct Wizard {
    pub step: Step,
    name: String,
    url: String,
    login: String,
    password: String,
    pub mfa: Option<String>,
}

impl Wizard {
    /// A wizard at its first question.
    pub fn new() -> Self {
        Self {
            step: Step::Name,
            name: String::new(),
            url: String::new(),
            login: String::new(),
            password: String::new(),
            mfa: None,
        }
    }

    /// Stores the answer to the current question; true once everything needed to log in is known.
    /// An empty answer is ignored.
    pub fn answer(&mut self, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        match self.step {
            Step::Name => (self.name, self.step) = (text.to_owned(), Step::Url),
            Step::Url => {
                self.url = match text.contains("://") {
                    true => text.to_owned(),
                    false => format!("https://{text}"),
                };
                self.step = Step::Login;
            }
            Step::Login => (self.login, self.step) = (text.to_owned(), Step::Password),
            Step::Password => {
                self.password = text.to_owned();
                return true;
            }
            Step::Mfa => {
                self.mfa = Some(text.to_owned());
                return true;
            }
        }
        false
    }

    /// The server address, with a scheme.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The login that was given.
    pub fn login(&self) -> &str {
        &self.login
    }

    /// The password that was given.
    pub fn password(&self) -> &str {
        &self.password
    }

    /// The server as it is saved once the login worked.
    pub fn config(&self) -> ServerConfig {
        ServerConfig {
            name: self.name.clone(),
            url: self.url.clone(),
            login: self.login.clone(),
            password: Some(self.password.clone()),
            password_cmd: None,
            token: None,
            token_cmd: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(line: &mut Line, text: &str) {
        for c in text.chars() {
            line.edit(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn typing_and_editing_in_the_middle() {
        let mut line = Line::new(false);
        typed(&mut line, "helo");
        line.edit(key(KeyCode::Left));
        line.edit(key(KeyCode::Char('l')));
        assert_eq!(line.text(), "hello");
        line.edit(key(KeyCode::Home));
        line.edit(key(KeyCode::Delete));
        assert_eq!(line.text(), "ello");
        line.edit(key(KeyCode::Backspace));
        assert_eq!(line.cursor(), 0);
    }

    #[test]
    fn inserted_text_keeps_line_breaks_and_drops_other_control_characters() {
        let mut line = Line::with_text("hello world", false);
        for _ in 0..6 {
            line.edit(key(KeyCode::Left));
        }
        line.insert_str("big\n\tone ");
        assert_eq!(line.text(), "hellobig\none  world");
        assert_eq!(line.shown(), "hellobig⏎one  world");
        assert_eq!(line.cursor(), 13);
    }

    #[test]
    fn control_keys_kill_text() {
        let mut line = Line::new(false);
        typed(&mut line, "one two three");
        line.edit(ctrl('w'));
        assert_eq!(line.text(), "one two ");
        line.edit(ctrl('u'));
        assert_eq!(line.text(), "");
    }

    #[test]
    fn secrets_are_masked_with_wide_characters_counted_once() {
        let mut line = Line::new(true);
        typed(&mut line, "pä🙂");
        assert_eq!(line.shown(), "•••");
        assert_eq!(line.text(), "pä🙂");
    }

    #[test]
    fn commands_are_parsed() {
        assert_eq!(parse(" new-server "), Some(Command::NewServer));
        assert_eq!(parse("q"), Some(Command::Quit));
        assert_eq!(parse("nope"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn a_command_is_known_by_its_shortest_unambiguous_start() {
        assert_eq!(parse("ser x"), Some(Command::Server("x".into())));
        assert_eq!(parse("rel"), Some(Command::Reload));
        assert_eq!(parse("react tada"), Some(Command::React("tada".into())));
        assert_eq!(parse("reacti"), Some(Command::Reactions));
        assert_eq!(parse("s"), None);
        assert_eq!(parse("se"), None);
        assert_eq!(parse("set a b"), Some(Command::Set("a".into(), "b".into())));
        assert_eq!(matching("mark"), ["mark-read", "mark-all-read"]);
        assert_eq!(starting("react"), ["reactions", "react"]);
        assert_eq!(matching("react"), ["react"]);
    }

    #[test]
    fn arguments_are_the_rest_of_the_line() {
        assert_eq!(parse("go  town sq "), Some(Command::Go("town sq".into())));
        assert_eq!(parse("server"), Some(Command::Server(String::new())));
        assert_eq!(
            parse("set open.image nsxiv -f"),
            Some(Command::Set("open.image".into(), "nsxiv -f".into()))
        );
        assert_eq!(
            parse("set copy"),
            Some(Command::Set("copy".into(), String::new()))
        );
        assert_eq!(parse("status dnd"), Some(Command::Status("dnd".into())));
        assert_eq!(parse("copy full"), Some(Command::Copy("full".into())));
        assert_eq!(parse("leave"), Some(Command::Leave));
    }

    #[test]
    fn wizard_collects_answers_then_asks_for_mfa() {
        let mut wizard = Wizard::new();
        assert!(!wizard.answer(""));
        assert!(!wizard.answer("work"));
        assert!(!wizard.answer("chat.example.org"));
        assert!(!wizard.answer("paul"));
        assert_eq!(wizard.step, Step::Password);
        assert!(wizard.answer("hunter2"));
        assert_eq!(wizard.url(), "https://chat.example.org");
        wizard.step = Step::Mfa;
        assert!(wizard.answer("123456"));
        assert_eq!(wizard.mfa.as_deref(), Some("123456"));
        assert_eq!(wizard.config().password.as_deref(), Some("hunter2"));
    }
}
