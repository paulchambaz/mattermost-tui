//! Drawing: the tab strip, the log, the status line, the input row and the help page.

use std::time::Instant;

use chrono::Local;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Kind};
use crate::finder;
use crate::model::{self, group_with_previous, Link, Message, Server};
use crate::term;

const MARGIN: usize = 1;
const TIME: usize = 5;
const NICK: usize = 16;
const MIN_TEXT: usize = 10;

const BINDINGS: &[(&str, &str)] = &[
    (
        "j k",
        "next / previous message; k at the top loads older ones",
    ),
    ("g G", "first / last message; g also loads older ones"),
    ("w b", "next / previous author group"),
    ("J K", "next / previous channel"),
    ("H L", "previous / next team, across servers"),
    (":", "command line"),
    ("?", "this help"),
    ("n", "next unread message, then next unread channel"),
    ("N", "next mention, across all teams and servers"),
    ("c", "mark the open channel as read"),
    ("f F", "find a channel: f in this team, F everywhere"),
    (
        "i",
        "write a message: enter sends, esc leaves (the text is kept)",
    ),
    (
        "ctrl-g",
        "write the message in $EDITOR, in the lower half of the screen",
    ),
    ("r", "reply in the thread of the selected message"),
    ("e", "edit your selected message"),
    ("d", "delete your selected message, y confirms"),
    ("a", "reactions under the messages"),
    ("q", "quit"),
];

const FINDER: &[(&str, &str)] = &[
    (
        "words",
        "all must match the channel name (F: or the server:team)",
    ),
    ("@x", "direct and group messages matching x"),
    (
        "a#b",
        "channel b in the team a; #b any team, a# the team only",
    ),
    ("tab, arrows", "choose"),
    ("enter, esc", "go there, close"),
];

const COMMANDS: &[(&str, &str)] = &[
    ("new-server", "log in to another server"),
    ("help", "this help"),
    ("quit", "quit"),
];

fn muted() -> Style {
    Style::new().fg(Color::Indexed(8))
}

/// Cuts `s` to at most `width` cells, ending with an ellipsis when something was cut.
pub fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w >= width {
            break;
        }
        out.push(c);
        used += w;
    }
    if width > 0 {
        out.push('…');
    }
    out
}

/// Breaks text into lines of at most `width` cells, at spaces when it can.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.replace('\r', "").split('\n') {
        let (mut line, mut used) = (String::new(), 0);
        for word in paragraph.split(' ') {
            let w = word.width();
            if used > 0 && used + 1 + w > width {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            if used > 0 {
                line.push(' ');
                used += 1;
            }
            for c in word.chars() {
                let cw = c.width().unwrap_or(0);
                if used + cw > width && used > 0 {
                    out.push(std::mem::take(&mut line));
                    used = 0;
                }
                line.push(c);
                used += cw;
            }
        }
        out.push(line);
    }
    out
}

/// Replaces `:shortcode:` with the emoji it names; unknown names stay as they are.
pub fn emoji(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(':') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')))
            .unwrap_or(after.len());
        let found = (len > 0 && after[len..].starts_with(':'))
            .then(|| emojis::get_by_shortcode(&after[..len]))
            .flatten();
        match found {
            Some(e) => {
                out.push_str(e.as_str());
                rest = &after[len + 1..];
            }
            None => {
                out.push(':');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The first item to show so that `active` fits in `room` cells.
fn first_visible(widths: &[usize], active: usize, room: usize) -> usize {
    (0..active)
        .find(|&first| widths[first..=active].iter().sum::<usize>() <= room)
        .unwrap_or(active)
}

/// The scroll that shows the span of message `index`, moving as little as it can.
fn scrolled(
    scroll: usize,
    span: (usize, usize),
    index: usize,
    count: usize,
    height: usize,
    total: usize,
) -> usize {
    let bottom = total.saturating_sub(height);
    let (start, end) = span;
    if index == 0 {
        0
    } else if index + 1 == count {
        bottom
    } else if start < scroll {
        start
    } else if end >= scroll + height {
        match end - start >= height {
            true => start,
            false => end + 1 - height,
        }
    } else {
        scroll
    }
}

struct Columns {
    time: usize,
    nick: usize,
    text: usize,
}

fn columns(width: usize) -> Columns {
    let text_for = |time: usize, nick: usize| {
        let gap = if time > 0 { 1 } else { 0 };
        width.saturating_sub(2 * MARGIN + time + gap + nick + 2)
    };
    let (time, mut nick) = match text_for(TIME, NICK) >= MIN_TEXT {
        true => (TIME, NICK),
        false => (0, NICK),
    };
    let text = text_for(time, nick);
    if text < MIN_TEXT {
        nick = nick.saturating_sub(MIN_TEXT - text).max(4);
    }
    Columns {
        time,
        nick,
        text: text_for(time, nick).max(MIN_TEXT),
    }
}

fn pad_left(s: &str, width: usize) -> String {
    format!("{}{s}", " ".repeat(width.saturating_sub(s.width())))
}

fn pad_right(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(s.width())))
}

fn mention_spans(line: &str, base: Style, me: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut rest = line;
    while let Some(at) = rest.find('@') {
        let (before, from) = rest.split_at(at);
        let name = from[1..]
            .split(|c: char| !(c.is_alphanumeric() || matches!(c, '.' | '_' | '-')))
            .next()
            .unwrap_or_default()
            .trim_end_matches(['.', '-']);
        let glued = before
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric);
        plain.push_str(before);
        if name.is_empty() || glued {
            plain.push('@');
            rest = &from[1..];
            continue;
        }
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut plain), base));
        }
        let mine = name == me || matches!(name, "all" | "channel" | "here");
        let color = if mine {
            Color::Indexed(11)
        } else {
            Color::Indexed(12)
        };
        spans.push(Span::styled(
            format!("@{name}"),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ));
        rest = &from[1 + name.len()..];
    }
    plain.push_str(rest);
    if !plain.is_empty() {
        spans.push(Span::styled(plain, base));
    }
    spans
}

struct Log {
    lines: Vec<Line<'static>>,
    spans: Vec<(usize, usize)>,
}

fn separator(text: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(MARGIN)),
        Span::styled(format!("─── {text}"), style),
    ])
}

fn notice_log(text: &str) -> Log {
    Log {
        lines: vec![Line::styled(format!(" {text}"), muted())],
        spans: Vec::new(),
    }
}

fn render_log(app: &App, width: usize) -> Log {
    let Some((server, chan)) = app.channel() else {
        return notice_log(match app.servers.is_empty() {
            true => "No server yet. Type :new-server to log in.",
            false => "Loading…",
        });
    };
    let Some(messages) = app.messages() else {
        return notice_log("Loading…");
    };
    if messages.is_empty() {
        return notice_log(&format!("No messages in {}", server.label(chan)));
    }
    let start = server
        .complete
        .contains(&chan.info.id)
        .then(|| format!("Start of {}", server.label(chan)));
    messages_log(app, server, messages, start, width)
}

fn messages_log(
    app: &App,
    server: &Server,
    messages: &[Message],
    start: Option<String>,
    width: usize,
) -> Log {
    let cols = columns(width);
    let now = Local::now();
    let me = server.me.as_ref().map_or("", |u| u.username.as_str());
    let unread_at = app
        .unread_boundary()
        .and_then(|id| messages.iter().position(|m| &m.id == id));
    let indent = " ".repeat(MARGIN + cols.time + usize::from(cols.time > 0) + cols.nick + 2);
    let mut log = Log {
        lines: Vec::new(),
        spans: Vec::new(),
    };

    if let Some(start) = start {
        log.lines.push(separator(&start, muted()));
    }
    for (i, message) in messages.iter().enumerate() {
        let previous = i.checked_sub(1).map(|p| &messages[p]);
        let start = log.lines.len();
        if previous.is_none_or(|p| p.at.date_naive() != message.at.date_naive()) {
            log.lines
                .push(separator(&model::day_label(message.at, now), muted()));
        }
        if unread_at == Some(i) {
            log.lines.push(separator(
                "new messages",
                Style::new().fg(Color::Indexed(1)),
            ));
        }
        let continuing = previous.is_some_and(|p| group_with_previous(p, message));

        let color = Color::Indexed(model::user_color(&message.user));
        let name = server.username(&message.user);
        let selected = app.browsing && app.cursor == i;
        let nick = match (message.system, continuing, selected) {
            (true, _, _) => Span::raw(" ".repeat(cols.nick)),
            (_, _, true) => {
                let block = format!(" {} ", truncate(name, cols.nick.saturating_sub(2).max(1)));
                Span::styled(
                    pad_left(&block, cols.nick),
                    Style::new()
                        .bg(color)
                        .fg(Color::Indexed(0))
                        .add_modifier(Modifier::BOLD),
                )
            }
            (_, true, _) => Span::raw(" ".repeat(cols.nick)),
            _ => Span::styled(
                pad_left(&truncate(name, cols.nick), cols.nick),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            ),
        };
        let clock = match (cols.time, continuing || message.system) {
            (0, _) | (_, true) => " ".repeat(cols.time),
            _ => message.at.format("%H:%M").to_string(),
        };

        let base = if message.system {
            muted()
        } else {
            Style::new()
        };
        let marker = message.root.as_ref().map(|root| {
            let author = messages
                .iter()
                .find(|m| &m.id == root)
                .map_or("?", |m| server.username(&m.user));
            format!("↪ {}", truncate(author, 12))
        });
        let text = emoji(&message.text);
        let body = match &marker {
            Some(marker) => format!("{marker} {text}"),
            None => text,
        };
        let wrapped = wrap(&body, cols.text);
        let last = wrapped.len() - 1;
        for (n, part) in wrapped.iter().enumerate() {
            let mut spans = Vec::new();
            if n == 0 {
                spans.push(Span::raw(" ".repeat(MARGIN)));
                if cols.time > 0 {
                    spans.push(Span::styled(pad_right(&clock, cols.time), muted()));
                    spans.push(Span::raw(" "));
                }
                spans.push(nick.clone());
                spans.push(Span::raw("  "));
            } else {
                spans.push(Span::raw(indent.clone()));
            }
            let rest = match (&marker, n) {
                (Some(marker), 0) => match part.strip_prefix(marker.as_str()) {
                    Some(rest) => {
                        spans.push(Span::styled(marker.clone(), muted()));
                        rest
                    }
                    None => part,
                },
                _ => part,
            };
            spans.extend(mention_spans(rest, base, me));
            if message.edited && n == last {
                spans.push(Span::styled(" *", muted()));
            }
            log.lines.push(Line::from(spans));
        }
        if app.reactions && !message.reactions.is_empty() {
            let mut spans = vec![Span::raw(indent.clone())];
            for (n, group) in message.reactions.iter().enumerate() {
                let mine = server
                    .me
                    .as_ref()
                    .is_some_and(|m| group.users.contains(&m.id));
                let style = if mine {
                    Style::new().add_modifier(Modifier::BOLD)
                } else {
                    muted()
                };
                let sep = if n > 0 { "  " } else { "" };
                spans.push(Span::styled(
                    format!(
                        "{sep}{} {}",
                        emoji(&format!(":{}:", group.name)),
                        group.users.len()
                    ),
                    style,
                ));
            }
            log.lines.push(Line::from(spans));
        }
        for file in &message.files {
            log.lines.push(Line::from(vec![
                Span::raw(indent.clone()),
                Span::styled(format!("[file: {file}]"), muted()),
            ]));
        }
        log.spans.push((start, log.lines.len() - 1));
    }
    log
}

fn tabs_line(app: &App, width: usize) -> Line<'static> {
    let Some(server) = app.servers.get(app.server) else {
        return Line::default();
    };
    let places: usize = app.servers.iter().map(|s| s.teams.len()).sum();
    let place = app.servers[..app.server]
        .iter()
        .map(|s| s.teams.len())
        .sum::<usize>()
        + app.team
        + 1;
    let Some(team) = server.teams.get(app.team) else {
        let state = if server.client.is_some() {
            "loading…"
        } else {
            "connecting…"
        };
        return Line::from(vec![
            chip(&format!(" {} ", server.name)),
            Span::styled(format!(" {state}"), muted()),
        ]);
    };
    let chip = chip(&truncate(
        &format!(
            " {}:{} {place}/{places} ",
            server.name, team.info.display_name
        ),
        (width / 2).max(8),
    ));
    let Some(chans) = team.chans.as_ref().filter(|c| !c.is_empty()) else {
        return Line::from(vec![chip, Span::styled(" loading…", muted())]);
    };

    let active = team.cur.min(chans.len() - 1);
    let flagged = |i: usize| i != active && chans[i].unread();
    let labels: Vec<String> = chans
        .iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                " {}{}{} ",
                if flagged(i) { "•" } else { "" },
                server.label(c),
                if i != active && c.mentions() { "!" } else { "" }
            )
        })
        .collect();
    let widths: Vec<usize> = labels.iter().map(|l| l.width()).collect();
    let room = width.saturating_sub(chip.width() + 2).max(1);
    let first = first_visible(&widths, active, room);

    let mut spans = vec![chip, Span::raw(if first > 0 { "‹" } else { " " })];
    let mut used = 0;
    let mut last = first;
    for i in first..chans.len() {
        if used + widths[i] > room {
            break;
        }
        let style = match (i == active, flagged(i)) {
            (true, _) => Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            (_, true) => Style::new().add_modifier(Modifier::BOLD),
            _ => Style::new(),
        };
        spans.push(Span::styled(labels[i].clone(), style));
        used += widths[i];
        last = i;
    }
    spans.push(Span::raw(if last + 1 < chans.len() { "›" } else { " " }));
    Line::from(spans)
}

fn chip(text: &str) -> Span<'static> {
    Span::styled(
        text.to_owned(),
        Style::new()
            .bg(Color::Indexed(4))
            .fg(Color::Indexed(15))
            .add_modifier(Modifier::BOLD),
    )
}

fn status_line(app: &App, width: usize, percent: usize) -> Line<'static> {
    let mode = app.mode();
    let (left, right) = if app.notice.is_empty() {
        let place = match (app.servers.get(app.server), app.channel()) {
            (Some(server), Some((_, chan))) => {
                let team = server
                    .teams
                    .get(app.team)
                    .map_or("", |t| t.info.display_name.as_str());
                format!("{}:{team} > {}", server.name, server.label(chan))
            }
            (Some(server), None) => server.name.clone(),
            _ => String::new(),
        };
        let count = app.messages().map_or(0, <[Message]>::len);
        let offline = if server_down(app) { "offline  " } else { "" };
        let offline = format!("{}{offline}", typing_names(app));
        let right = match count {
            0 => format!("{offline}? help "),
            _ => format!(
                "{offline}{}/{count}  {percent:>3}%  ? help ",
                app.cursor + 1
            ),
        };
        (format!(" {mode}  {place}"), right)
    } else {
        (format!(" {mode}  {}", app.notice), String::new())
    };
    let left = truncate(&left, width.saturating_sub(right.width()));
    let gap = width.saturating_sub(left.width() + right.width());
    Line::styled(
        format!("{left}{}{right}", " ".repeat(gap)),
        Style::new().add_modifier(Modifier::REVERSED),
    )
}

fn server_down(app: &App) -> bool {
    app.servers
        .get(app.server)
        .is_some_and(|s| s.link == Link::Down)
}

/// Text in `base`, with the characters at `hits` in `hl`.
fn highlighted(text: &str, hits: &[usize], base: Style, hl: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut marked = false;
    for (i, c) in text.chars().enumerate() {
        if hits.contains(&i) != marked && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if marked { hl } else { base },
            ));
        }
        marked = hits.contains(&i);
        run.push(c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if marked { hl } else { base }));
    }
    spans
}

/// The matches of the finder on one line: the count, the channels side by side, and the scope.
fn wildmenu(app: &App, width: usize) -> Line<'static> {
    let Some(finder) = &app.finder else {
        return Line::default();
    };
    let line = Style::new().add_modifier(Modifier::REVERSED);
    let mode = Span::styled(" SEARCH ", line);
    let text = finder.line.text();
    let words: Vec<&str> = text.split_whitespace().collect();
    let candidates = finder::candidates(&app.servers, (app.server, app.team), finder.all);
    let found = finder::entries(&words, &candidates, finder.all);
    if found.is_empty() {
        let rest = width.saturating_sub(mode.width());
        return Line::from(vec![
            mode,
            Span::styled(pad_right(" no result", rest), line),
        ]);
    }

    let selected = finder.selected.min(found.len() - 1);
    let chip = Span::styled(format!(" {}/{} ", selected + 1, found.len()), line);
    let chosen = found[selected].candidate;
    let scope = match (finder.all, chosen.direct()) {
        (false, _) => " this team ".to_owned(),
        (true, true) => " ".to_owned(),
        (true, false) => format!(" {} ", truncate(&chosen.scope, (width / 3).max(8))),
    };
    let scope = Span::styled(scope, line);

    let items: Vec<Vec<Span<'static>>> = found
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let candidate = entry.candidate;
            let mut base = line;
            if candidate.unread {
                base = base.add_modifier(Modifier::BOLD);
            }
            let mut hl = base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            if i == selected {
                base = Style::new()
                    .bg(Color::Indexed(3))
                    .fg(Color::Indexed(0))
                    .add_modifier(Modifier::BOLD);
                hl = base.add_modifier(Modifier::UNDERLINED);
            }
            let mut spans = vec![Span::styled(" ", base)];
            if finder.all && !candidate.direct() {
                let scope = truncate(&candidate.scope, 14);
                spans.extend(highlighted(&scope, &entry.scope_hits, base, hl));
            }
            spans.push(Span::styled(model::glyph(candidate.kind).to_string(), base));
            spans.extend(highlighted(&candidate.name, &entry.name_hits, base, hl));
            spans.push(Span::styled(" ", base));
            spans
        })
        .collect();
    let widths: Vec<usize> = items
        .iter()
        .map(|spans| spans.iter().map(Span::width).sum())
        .collect();

    let room = width
        .saturating_sub(chip.width() + scope.width() + 2)
        .max(1);
    let first = first_visible(&widths, selected, room);
    let (mut used, mut last) = (0, first);
    let mut menu = Vec::new();
    for (i, spans) in items.into_iter().enumerate().skip(first) {
        if used + widths[i] > room {
            break;
        }
        used += widths[i];
        last = i;
        menu.extend(spans);
    }

    let left = Span::styled(if first > 0 { "‹" } else { " " }, line);
    let right = Span::styled(if last + 1 < found.len() { "›" } else { " " }, line);
    let gap = width.saturating_sub(mode.width() + chip.width() + 2 + used + scope.width());
    let mut spans = vec![mode, chip, left];
    spans.extend(menu);
    spans.push(right);
    spans.push(Span::styled(" ".repeat(gap), line));
    spans.push(scope);
    Line::from(spans)
}

fn compose_title(kind: &Kind) -> String {
    match kind {
        Kind::Message => "New message".into(),
        Kind::Reply { to, .. } => format!("Reply to {}", truncate(to, 20)),
        Kind::Edit(_) => "Editing your message".into(),
    }
}

fn compose_label(kind: &Kind) -> String {
    match kind {
        Kind::Message => String::new(),
        Kind::Reply { to, .. } => format!("↪ {} ", truncate(to, 12)),
        Kind::Edit(_) => "✎ edit ".into(),
    }
}

/// The part of an input that fits in `room` cells with the cursor inside, and the cursor's column in it.
fn visible(shown: &str, cursor: usize, room: usize) -> (String, usize) {
    let chars: Vec<char> = shown.chars().collect();
    let cells = |c: &char| c.width().unwrap_or(0);
    let room = room.max(2);
    let before: usize = chars[..cursor.min(chars.len())].iter().map(cells).sum();
    let (mut start, mut skipped) = (0, 0);
    while before - skipped >= room {
        skipped += cells(&chars[start]);
        start += 1;
    }
    let (mut text, mut used) = (String::new(), 0);
    for c in &chars[start..] {
        if used + cells(c) > room {
            break;
        }
        text.push(*c);
        used += cells(c);
    }
    (text, before - skipped)
}

fn typing_names(app: &App) -> String {
    let Some((server, chan)) = app.channel() else {
        return String::new();
    };
    let names = server.typing_names(&chan.info.id, Instant::now());
    if names.is_empty() {
        return String::new();
    }
    let shown = names.iter().take(3).copied().collect::<Vec<_>>().join(", ");
    let more = names.len().saturating_sub(3);
    let more = if more > 0 {
        format!(" +{more}")
    } else {
        String::new()
    };
    format!("✎ {shown}{more}  ")
}

fn help_lines() -> Vec<Line<'static>> {
    let section = |title: &str, rows: &[(&str, &str)]| {
        let heading = Line::styled(
            format!("  {title}"),
            Style::new().add_modifier(Modifier::BOLD),
        );
        let rows = rows
            .iter()
            .map(|(key, what)| Line::from(format!("    {key:<12}{what}")));
        std::iter::once(heading)
            .chain(rows)
            .chain(std::iter::once(Line::default()))
            .collect::<Vec<_>>()
    };
    [
        section("Keys", BINDINGS),
        section("Finder", FINDER),
        section("Commands (type : first)", COMMANDS),
    ]
    .concat()
}

/// Draws the whole screen, following the cursor when a key asked for it.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let width = usize::from(area.width);
    app.size = (area.width, area.height);
    let editor_rows = match app.pane {
        Some(_) => term::split(area.height).1 + 1,
        None => 1,
    };
    let [tabs, body, input, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(editor_rows),
        Constraint::Length(1),
    ])
    .areas(area);
    let height = usize::from(body.height);

    frame.render_widget(Paragraph::new(tabs_line(app, width)), tabs);

    let mut percent = 100;
    if let Some(top) = app.help {
        let lines = help_lines();
        let title = Line::from(vec![
            Span::styled(" Help", Style::new().add_modifier(Modifier::BOLD)),
            Span::styled("   j k scroll   esc ? q close", muted()),
        ]);
        let rows = height.saturating_sub(1);
        let top = top.min(lines.len().saturating_sub(rows));
        app.help = Some(top);
        let page: Vec<Line> = std::iter::once(title)
            .chain(lines.into_iter().skip(top).take(rows))
            .collect();
        frame.render_widget(Paragraph::new(page), body);
    } else {
        let log = render_log(app, width);
        let bottom = log.lines.len().saturating_sub(height);
        let mut scroll = app.scroll.min(bottom);
        if app.follow {
            if let Some(&span) = log.spans.get(app.cursor) {
                scroll = scrolled(
                    scroll,
                    span,
                    app.cursor,
                    log.spans.len(),
                    height,
                    log.lines.len(),
                )
                .min(bottom);
                app.scroll = scroll;
            }
            app.follow = false;
        }
        percent = (scroll * 100).checked_div(bottom).unwrap_or(100);
        let page: Vec<Line> = log.lines.into_iter().skip(scroll).take(height).collect();
        frame.render_widget(Paragraph::new(page), body);
    }

    if let Some(pane) = &mut app.pane {
        let label = app
            .compose
            .as_ref()
            .map_or(String::new(), |c| compose_title(&c.kind));
        let rule = format!("─── {label} ");
        let rest = width.saturating_sub(rule.width());
        let line = Line::styled(format!("{rule}{}", "─".repeat(rest)), muted());
        let editor = Rect::new(
            input.x,
            input.y + 1,
            input.width,
            input.height.saturating_sub(1),
        );
        frame.render_widget(Paragraph::new(line), Rect { height: 1, ..input });
        pane.resize(editor.height, editor.width);
        frame.render_widget(&*pane, editor);
        if let Some((column, row)) = pane.cursor() {
            frame.set_cursor_position(Position::new(editor.x + column, editor.y + row));
        }
    }
    let typing = match (&app.compose, &app.finder, &app.input) {
        _ if app.pane.is_some() => None,
        (Some(compose), ..) => Some((compose_label(&compose.kind), "> ", &compose.line)),
        (_, Some(finder), _) => Some((String::new(), "find> ", &finder.line)),
        (.., Some(line)) => Some((String::new(), app.prompt(), line)),
        _ => None,
    };
    if let Some((label, prompt, line)) = typing {
        let lead = 1 + label.width() + prompt.width();
        let (text, column) = visible(&line.shown(), line.cursor(), width.saturating_sub(lead));
        let row = Line::from(vec![
            Span::raw(" "),
            Span::styled(label, muted()),
            Span::raw(prompt),
            Span::raw(text),
        ]);
        frame.render_widget(Paragraph::new(row), input);
        let x = input.x + u16::try_from(lead + column).unwrap_or(0);
        frame.set_cursor_position(Position::new(
            x.min(input.right().saturating_sub(1)),
            input.y,
        ));
    }
    let bottom = match app.finder {
        Some(_) => wildmenu(app, width),
        None => status_line(app, width, percent),
    };
    frame.render_widget(Paragraph::new(bottom), status);
}

#[cfg(test)]
pub mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::app::tests::{fixture, press, press_code};

    pub fn screen(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(usize::from(width))
            .map(|row| {
                let mut line = String::new();
                let mut skip = 0;
                for cell in row {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    skip = cell.symbol().width().saturating_sub(1);
                    line.push_str(cell.symbol());
                }
                line.trim_end().to_owned()
            })
            .collect()
    }

    fn from_end(lines: &[String], n: usize) -> &str {
        &lines[lines.len() - n]
    }

    #[test]
    fn truncate_counts_cells() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("日本語です", 5), "日本…");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn wrap_breaks_at_spaces_and_cuts_long_words() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\n\nb", 5), ["a", "", "b"]);
        assert_eq!(wrap("日本語 日本", 6), ["日本語", "日本"]);
        assert_eq!(wrap("", 5), [""]);
    }

    #[test]
    fn shortcodes_become_emoji() {
        assert_eq!(emoji("well done :tada:!"), "well done 🎉!");
        assert_eq!(
            emoji(":+1: :nope_nope: 12:30:45"),
            "👍 :nope_nope: 12:30:45"
        );
        assert_eq!(emoji("a::b"), "a::b");
    }

    #[test]
    fn mentions_are_split_out() {
        let text = |spans: Vec<Span>| {
            spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            text(mention_spans("hi @ana. mail a@b.c", Style::new(), "paul")),
            ["hi ", "@ana", ". mail a@b.c"]
        );
        assert_eq!(
            mention_spans("@paul", Style::new(), "paul")[0].style.fg,
            Some(Color::Indexed(11))
        );
    }

    #[test]
    fn narrow_screens_drop_the_time_column_first() {
        let wide = columns(80);
        assert_eq!((wide.time, wide.nick), (TIME, NICK));
        let narrow = columns(30);
        assert_eq!(narrow.time, 0);
        assert!(narrow.text >= MIN_TEXT);
        assert!(columns(12).nick >= 4);
    }

    #[test]
    fn the_strip_scrolls_to_the_active_tab() {
        assert_eq!(first_visible(&[5, 5, 5, 5], 3, 10), 2);
        assert_eq!(first_visible(&[5, 5, 5, 5], 0, 10), 0);
        assert_eq!(first_visible(&[20, 5], 1, 10), 1);
    }

    #[test]
    fn scrolling_moves_as_little_as_needed() {
        assert_eq!(scrolled(5, (6, 7), 2, 9, 10, 40), 5);
        assert_eq!(scrolled(5, (2, 3), 2, 9, 10, 40), 2);
        assert_eq!(scrolled(0, (14, 15), 2, 9, 10, 40), 6);
        assert_eq!(scrolled(9, (0, 1), 0, 9, 10, 40), 0);
        assert_eq!(scrolled(0, (38, 39), 8, 9, 10, 40), 30);
    }

    #[test]
    fn the_screen_shows_tabs_log_and_status() {
        let mut app = fixture();
        let lines = screen(&mut app, 80, 12);
        assert!(
            lines[0].starts_with(" work:Engineering 1/1 "),
            "{:?}",
            lines[0]
        );
        assert!(lines[0].contains("+Secret"));
        assert!(lines[0].contains("#Town Square"));
        assert!(lines[0].contains("@ana"));
        assert!(!lines[0].contains("ana, bob"));
        assert!(from_end(&lines, 1).starts_with(" NORMAL  work:Engineering > +Secret"));
        assert!(from_end(&lines, 1).ends_with("2/2  100%  ? help"));
    }

    #[test]
    fn replies_emoji_and_files_are_written() {
        let mut app = fixture();
        let lines = screen(&mut app, 80, 12).join("\n");
        assert!(lines.contains("hello 🎉"), "{lines}");
        assert!(
            lines.contains("↪ ana") || lines.contains("↪ paul"),
            "{lines}"
        );
        assert!(lines.contains("[file: a.png]"));
        assert!(lines.contains("ana"));
        assert!(lines.contains(" *"));
    }

    fn has_background(app: &mut App, row: usize) -> bool {
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content()
            .chunks(80)
            .nth(row)
            .is_some_and(|cells| cells.iter().any(|c| c.bg != Color::Reset))
    }

    #[test]
    fn nothing_is_selected_until_a_key_moves() {
        let mut app = fixture();
        assert!(!has_background(&mut app, 4));
        press(&mut app, "k");
        assert!(has_background(&mut app, 4));
        assert!(!has_background(&mut app, 3));
        press(&mut app, "jj");
        assert!(!has_background(&mut app, 4));
    }

    #[test]
    fn dms_are_tabs_with_an_at_sign_and_follow_the_channel() {
        let mut app = fixture();
        press(&mut app, "JJ");
        let lines = screen(&mut app, 80, 12);
        assert!(from_end(&lines, 1).contains("> @ana"));
        let narrow = screen(&mut app, 34, 12);
        assert!(narrow[0].contains("@ana"), "{:?}", narrow[0]);
    }

    #[test]
    fn help_opens_scrolls_and_closes() {
        let mut app = fixture();
        press(&mut app, "?");
        let lines = screen(&mut app, 60, 12);
        assert!(lines[1].starts_with(" Help"));
        assert!(lines.iter().any(|l| l.contains("next / previous channel")));
        assert!(from_end(&lines, 1).starts_with(" HELP"));
        press(&mut app, "jjjjjjjjjjjjjjjjjjjjjj");
        let lines = screen(&mut app, 60, 12);
        assert!(lines.iter().any(|l| l.contains("log in to another server")));
        press(&mut app, "?");
        assert!(from_end(&screen(&mut app, 60, 12), 1).starts_with(" NORMAL"));
    }

    #[test]
    fn the_command_line_takes_the_input_row() {
        let mut app = fixture();
        press(&mut app, ":qu");
        let lines = screen(&mut app, 60, 12);
        assert_eq!(from_end(&lines, 2), " :qu");
        assert!(from_end(&lines, 1).starts_with(" COMMAND"));
        press_code(&mut app, ratatui::crossterm::event::KeyCode::Esc);
        assert!(from_end(&screen(&mut app, 60, 12), 2).is_empty());
    }

    #[test]
    fn the_wizard_asks_in_order_and_hides_the_password() {
        let mut app = fixture();
        press(&mut app, ":new-server");
        press_code(&mut app, ratatui::crossterm::event::KeyCode::Enter);
        assert!(from_end(&screen(&mut app, 60, 12), 2).starts_with(" Name:"));
        press(&mut app, "home");
        press_code(&mut app, ratatui::crossterm::event::KeyCode::Enter);
        press(&mut app, "chat.example.org");
        press_code(&mut app, ratatui::crossterm::event::KeyCode::Enter);
        press(&mut app, "paul");
        press_code(&mut app, ratatui::crossterm::event::KeyCode::Enter);
        press(&mut app, "secret");
        let lines = screen(&mut app, 60, 12);
        assert_eq!(from_end(&lines, 2), " Password: ••••••");
        assert!(from_end(&lines, 1).starts_with(" LOGIN"));
    }

    #[test]
    fn the_wildmenu_fills_the_last_row_and_the_query_the_one_above() {
        let mut app = fixture();
        press(&mut app, "f");
        let lines = screen(&mut app, 80, 12);
        assert_eq!(from_end(&lines, 2), " find>");
        let menu = from_end(&lines, 1);
        assert!(menu.starts_with(" SEARCH  1/3 "), "{menu:?}");
        assert!(menu.contains("+Secret") && menu.contains("#Town Square") && menu.contains("@ana"));
        assert!(menu.ends_with("this team"));
        assert!(has_background(&mut app, 11));
    }

    #[test]
    fn typing_narrows_the_menu_and_nothing_matching_says_so() {
        let mut app = fixture();
        press(&mut app, "ftown");
        let lines = screen(&mut app, 80, 12);
        assert_eq!(from_end(&lines, 2), " find> town");
        assert!(from_end(&lines, 1).starts_with(" SEARCH  1/1 "));
        assert!(!from_end(&lines, 1).contains("+Secret"));
        press(&mut app, "zzz");
        assert_eq!(from_end(&screen(&mut app, 80, 12), 1), " SEARCH  no result");
    }

    #[test]
    fn a_narrow_menu_scrolls_to_keep_the_choice_visible() {
        let mut app = fixture();
        press(&mut app, "f");
        press_code(&mut app, ratatui::crossterm::event::KeyCode::Tab);
        press_code(&mut app, ratatui::crossterm::event::KeyCode::Tab);
        let menu = from_end(&screen(&mut app, 36, 12), 1).to_owned();
        assert!(menu.contains("@ana") && menu.contains('‹'), "{menu:?}");
        assert!(!menu.contains("+Secret"));
    }

    #[test]
    fn everywhere_the_scope_prefixes_the_items_and_ends_the_row() {
        let mut app = fixture();
        press(&mut app, "Ftown");
        let menu = from_end(&screen(&mut app, 80, 12), 1).to_owned();
        assert!(menu.contains("work:Engineer…#Town Square"), "{menu:?}");
        assert!(menu.ends_with("work:Engineering"), "{menu:?}");
    }

    #[test]
    fn a_long_input_scrolls_so_the_cursor_stays_in_view() {
        assert_eq!(visible("hello", 5, 20), ("hello".to_owned(), 5));
        assert_eq!(visible("0123456789", 10, 5), ("6789".to_owned(), 4));
        assert_eq!(visible("0123456789", 3, 5), ("01234".to_owned(), 3));
        assert_eq!(visible("日本語日本語", 6, 5), ("本語".to_owned(), 4));
    }

    #[test]
    fn insert_mode_shows_in_the_status_line_and_the_text_in_the_input_row() {
        let mut app = fixture();
        press(&mut app, "ihello");
        let lines = screen(&mut app, 40, 12);
        assert_eq!(from_end(&lines, 2), " > hello");
        assert!(
            from_end(&lines, 1).starts_with(" INSERT  work:"),
            "{lines:?}"
        );
        press(&mut app, " and a much longer sentence that cannot fit");
        let row = from_end(&screen(&mut app, 40, 12), 2).to_owned();
        assert!(row.ends_with("cannot fit"), "{row:?}");
    }
}
