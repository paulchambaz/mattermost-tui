//! Keyboard-driven terminal client for Mattermost.

mod api;
mod app;
mod config;
mod finder;
mod input;
mod model;
mod term;
mod ui;
mod unread;
mod ws;

use anyhow::Result;
use app::{App, Msg};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let path = config::default_path()?;
    let boot = config::load(&path)?
        .into_iter()
        .map(|server| {
            let credentials = server.credentials().map_err(|e| e.to_string());
            (server, credentials)
        })
        .collect();
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
    let result = run(&mut terminal, &mut app, rx).await;
    drop(app);
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
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
    }
    Ok(())
}
