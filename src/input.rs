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

    /// What was typed.
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// The text as it appears on screen: bullets for a secret.
    pub fn shown(&self) -> String {
        match self.masked {
            true => self.chars.iter().map(|_| '•').collect(),
            false => self.text(),
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

/// What a `:` command asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    NewServer,
    Help,
    Quit,
}

/// Reads a command name; none when it is not one.
pub fn parse(input: &str) -> Option<Command> {
    match input.trim() {
        "new-server" => Some(Command::NewServer),
        "help" => Some(Command::Help),
        "quit" | "q" => Some(Command::Quit),
        _ => None,
    }
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
