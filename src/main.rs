//! Keyboard-driven terminal client for Mattermost.

mod api;
mod app;
mod clipboard;
mod config;
mod debug;
mod finder;
mod input;
mod markdown;
mod model;
mod open;
mod term;
mod ui;
mod unread;
mod ws;

use std::io::Write;
use std::process::ExitCode;

use anyhow::Result;
use app::{App, Msg};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

const USAGE: &str = "Usage: mm [--help] [--version]

A keyboard-driven terminal client for Mattermost.

  --help       print this help and exit
  --version    print the version and exit

Press ? inside mm for the keys, and see man mm for the rest.";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Run,
    Help,
    Version,
    Unknown(String),
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Cli {
    match args.into_iter().next().as_deref() {
        None => Cli::Run,
        Some("--help" | "-h") => Cli::Help,
        Some("--version" | "-V") => Cli::Version,
        Some(other) => Cli::Unknown(other.to_owned()),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<ExitCode> {
    match parse_args(std::env::args().skip(1)) {
        Cli::Run => {}
        Cli::Help => {
            println!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Version => {
            println!("mm {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        Cli::Unknown(argument) => {
            eprintln!("mm: unknown argument {argument}\n{USAGE}");
            return Ok(ExitCode::FAILURE);
        }
    }
    let path = config::default_path()?;
    let boot = config::load(&path)?
        .into_iter()
        .map(|server| {
            let credentials = server.credentials().map_err(|e| e.to_string());
            (server, credentials)
        })
        .collect();
    let settings = config::load_settings(&config::settings_path()?)?;
    let (tx, rx) = unbounded_channel();

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    let keys = tx.clone();
    std::thread::spawn(move || {
        while let Ok(event) = event::read() {
            let msg = match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => Msg::Key(key),
                Event::Resize(..) => Msg::Resize,
                Event::Paste(text) => Msg::Paste(text),
                _ => continue,
            };
            if keys.send(msg).is_err() {
                break;
            }
        }
    });
    let mut app = App::new(boot, path, tx);
    app.settings = settings;
    app.plain = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
    let result = run(&mut terminal, &mut app, rx).await;
    drop(app);
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result.map(|()| ExitCode::SUCCESS)
}

async fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut rx: UnboundedReceiver<Msg>,
) -> Result<()> {
    while !app.quit {
        terminal.draw(|frame| ui::draw(frame, app))?;
        let Some(msg) = rx.recv().await else {
            break;
        };
        app.update(msg);
        while let Ok(msg) = rx.try_recv() {
            app.update(msg);
        }
        for sequence in app.clipboard.drain(..) {
            let mut out = std::io::stdout();
            let _ = out
                .write_all(sequence.as_bytes())
                .and_then(|()| out.flush());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        parse_args(args.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn arguments_are_read_before_the_terminal_is_touched() {
        assert_eq!(parse(&[]), Cli::Run);
        assert_eq!(parse(&["--help"]), Cli::Help);
        assert_eq!(parse(&["-V"]), Cli::Version);
        assert_eq!(parse(&["nope"]), Cli::Unknown("nope".into()));
    }
}
