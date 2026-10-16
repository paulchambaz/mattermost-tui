//! A fake Mattermost on `127.0.0.1` with invented people and messages, and the real `mm` run against it.
//!
//! It records the demo gif of the README and lets anyone try `mm` without a server. Nothing leaves the
//! machine and the real configuration is never read: `mm` gets a throwaway home directory.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

type Shared = Arc<Mutex<World>>;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

fn lock(shared: &Shared) -> MutexGuard<'_, World> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

struct World {
    me: String,
    users: Vec<String>,
    status: HashMap<String, String>,
    teams: Vec<(String, String)>,
    channels: Vec<Value>,
    posts: Vec<Value>,
    read: HashMap<String, (i64, i64)>,
    events: broadcast::Sender<String>,
    next: u64,
    scripted: Option<(String, String)>,
}

impl World {
    fn new(me: &str, people: &[(&str, &str)], teams: &[(&str, &str)]) -> Self {
        let mut users = vec![me.to_owned()];
        let mut status = HashMap::from([(me.to_owned(), "online".to_owned())]);
        for (name, state) in people {
            users.push((*name).to_owned());
            status.insert((*name).to_owned(), (*state).to_owned());
        }
        Self {
            me: me.to_owned(),
            users,
            status,
            teams: teams
                .iter()
                .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
                .collect(),
            channels: Vec::new(),
            posts: Vec::new(),
            read: HashMap::new(),
            events: broadcast::channel(64).0,
            next: 0,
            scripted: None,
        }
    }

    fn id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{prefix}{}", self.next)
    }

    fn channel(&mut self, team: &str, kind: &str, name: &str, display: &str, header: &str) {
        self.channels.push(json!({
            "id": format!("c-{name}"), "team_id": team, "type": kind, "name": name,
            "display_name": display, "header": header, "purpose": "", "delete_at": 0,
        }));
    }

    fn direct(&mut self, other: &str) -> Value {
        let mut ids = [format!("u-{}", self.me), format!("u-{other}")];
        ids.sort();
        let name = ids.join("__");
        if let Some(found) = self.channels.iter().find(|c| c["name"] == name) {
            return found.clone();
        }
        let channel = json!({
            "id": format!("c-{name}"), "team_id": "", "type": "D", "name": name,
            "display_name": "", "header": "", "purpose": "", "delete_at": 0,
        });
        self.channels.push(channel.clone());
        channel
    }

    fn dm_id(&self, other: &str) -> String {
        let mut ids = [format!("u-{}", self.me), format!("u-{other}")];
        ids.sort();
        format!("c-{}", ids.join("__"))
    }

    fn total(&self, channel: &str) -> i64 {
        let count = self
            .posts
            .iter()
            .filter(|p| p["channel_id"] == channel)
            .count();
        i64::try_from(count).unwrap_or(0)
    }

    fn last(&self, channel: &str) -> i64 {
        self.posts
            .iter()
            .filter(|p| p["channel_id"] == channel)
            .filter_map(|p| p["create_at"].as_i64())
            .max()
            .unwrap_or(0)
    }

    fn channel_json(&self, channel: &Value) -> Value {
        let id = channel["id"].as_str().unwrap_or_default();
        let mut channel = channel.clone();
        channel["total_msg_count"] = json!(self.total(id));
        channel["last_post_at"] = json!(self.last(id));
        channel
    }

    fn post(&mut self, channel: &str, user: &str, root: &str, message: &str, ago: i64) -> String {
        let id = self.id("p");
        let at = now() - ago * 60_000;
        self.posts.push(json!({
            "id": id, "create_at": at, "update_at": at, "edit_at": 0, "delete_at": 0,
            "user_id": format!("u-{user}"), "channel_id": channel, "root_id": root,
            "message": message, "type": "", "props": {}, "hashtags": "", "file_ids": [],
            "metadata": {},
        }));
        id
    }

    fn find(&mut self, post: &str) -> Option<&mut Value> {
        self.posts.iter_mut().find(|p| p["id"] == post)
    }

    fn react(&mut self, post: &str, user: &str, emoji: &str) -> Value {
        let reaction = json!({
            "user_id": format!("u-{user}"), "post_id": post, "emoji_name": emoji, "create_at": now(),
        });
        if let Some(found) = self.find(post) {
            let list = &mut found["metadata"]["reactions"];
            if let Some(items) = list.as_array_mut() {
                items.push(reaction.clone());
            } else {
                *list = json!([reaction.clone()]);
            }
        }
        reaction
    }

    fn attach(&mut self, post: &str, name: &str, mime: &str, size: i64) {
        let file =
            json!({"id": format!("f-{post}"), "name": name, "mime_type": mime, "size": size});
        if let Some(found) = self.find(post) {
            found["metadata"]["files"] = json!([file]);
            found["file_ids"] = json!([format!("f-{post}")]);
        }
    }

    fn edited(&mut self, post: &str, message: &str) {
        if let Some(found) = self.find(post) {
            found["message"] = json!(message);
            found["edit_at"] = json!(now() - 60_000);
        }
    }

    fn unread(&mut self, channel: &str, unread: i64, mentions: i64) {
        let read = self.total(channel) - unread;
        self.read.insert(channel.to_owned(), (read, mentions));
    }

    fn user(&self, name: &str) -> Value {
        json!({"id": format!("u-{name}"), "username": name})
    }

    fn name_of<'a>(&self, id: &'a str) -> &'a str {
        id.strip_prefix("u-").unwrap_or(id)
    }

    fn announce(&self, event: &str, data: &Value, channel: &str) {
        let frame = json!({
            "event": event, "data": data, "broadcast": {"channel_id": channel}, "seq": self.next,
        });
        let _ = self.events.send(frame.to_string());
    }

    fn announce_post(&self, post: &Value) {
        let channel = post["channel_id"].as_str().unwrap_or_default();
        let data = json!({
            "channel_id": channel, "channel_type": "O", "post": post.to_string(),
            "mentions": "[]",
            "sender_name": self.name_of(post["user_id"].as_str().unwrap_or_default()),
        });
        self.announce("posted", &data, channel);
    }
}

fn work() -> World {
    let mut w = World::new(
        "alex",
        &[
            ("maya", "online"),
            ("tomas", "away"),
            ("priya", "dnd"),
            ("jonas", "offline"),
            ("lena", "online"),
            ("sam", "online"),
        ],
        &[("t-eng", "Engineering"), ("t-design", "Design")],
    );
    w.channel(
        "t-eng",
        "O",
        "general",
        "General",
        "Announcements and good mornings",
    );
    w.channel("t-eng", "O", "releases", "Releases", "What ships, and when");
    w.channel(
        "t-eng",
        "O",
        "backend",
        "Backend",
        "API, database and workers",
    );
    w.channel(
        "t-eng",
        "O",
        "incidents",
        "Incidents",
        "Something is on fire",
    );
    w.channel(
        "t-eng",
        "P",
        "roadmap",
        "Roadmap",
        "Not for everyone's eyes yet",
    );
    w.channel(
        "t-design",
        "O",
        "design-system",
        "Design System",
        "Tokens, components, docs",
    );
    w.channel(
        "t-design",
        "O",
        "research",
        "Research",
        "Interviews and findings",
    );
    w.channel("t-design", "O", "brand", "Brand", "Logos, colors, voice");
    for other in ["maya", "tomas", "priya"] {
        w.direct(other);
    }

    let hello = w.post(
        "c-general",
        "lena",
        "",
        "Good morning everyone! :wave:",
        213,
    );
    w.react(&hello, "sam", "wave");
    w.react(&hello, "maya", "wave");
    w.post(
        "c-general",
        "sam",
        "",
        "Reminder: the all-hands is **Thursday at 15:00**. The agenda is in the wiki.",
        200,
    );
    w.post(
        "c-general",
        "jonas",
        "",
        "Anyone knows a good coffee place near the new office?",
        140,
    );
    let coffee = w.post(
        "c-general",
        "lena",
        "",
        "The one on Mill Street. Ask for the *flat white*.",
        138,
    );
    w.react(&coffee, "jonas", "coffee");
    w.react(&coffee, "sam", "+1");

    w.post(
        "c-releases",
        "sam",
        "",
        "Release **v2.14** is tagged. Changelog: https://git.example.org/acme/api/releases/v2.14",
        100,
    );
    w.post(
        "c-releases",
        "tomas",
        "",
        "Staging looks good, the smoke tests are green.",
        93,
    );
    w.post(
        "c-releases",
        "sam",
        "",
        "Deploying v2.14 to production in ten minutes :rocket:",
        13,
    );
    w.post(
        "c-releases",
        "priya",
        "",
        "Canary is clean so far, rolling out to 100%.",
        8,
    );
    w.unread("c-releases", 2, 0);

    w.post(
        "c-incidents",
        "jonas",
        "",
        "Latency on `/orders` is back to normal after the cache flush.",
        66,
    );
    w.post(
        "c-incidents",
        "maya",
        "",
        "Postmortem draft: https://wiki.example.org/pm/2431",
        30,
    );
    w.post(
        "c-incidents",
        "priya",
        "",
        "Elevated 5xx on the payments worker, looking into it.",
        10,
    );
    w.post(
        "c-incidents",
        "jonas",
        "",
        "Seeing it too, the queue is backing up.",
        7,
    );
    w.post(
        "c-incidents",
        "priya",
        "",
        "@alex can you check the worker logs when you get a minute?",
        4,
    );
    w.unread("c-incidents", 3, 1);

    let migration = w.post(
        "c-backend",
        "maya",
        "",
        "Morning! The **orders** migration is up for review: https://git.example.org/acme/api/pull/482",
        31,
    );
    let worry = w.post(
        "c-backend",
        "tomas",
        "",
        "Looking at it now. Does it lock the table? Adding a column to `orders` worries me.",
        29,
    );
    w.post("c-backend", "jonas", &migration, "Seen, thanks Maya!", 28);
    w.post(
        "c-backend",
        "maya",
        "",
        "No lock, the default is applied by the database without a rewrite. The core of it:\n```sql\nALTER TABLE orders\n  ADD COLUMN currency text DEFAULT 'EUR';\n```",
        26,
    );
    let notes = w.post(
        "c-backend",
        "tomas",
        "",
        "> the default is applied without a rewrite\nGood, that answers it. Two small things:\n- rename `currency` to `currency_code`\n- add a test for the _empty table_ case",
        23,
    );
    w.react(&notes, "maya", "+1");
    w.edited(
        &worry,
        "Looking at it now. Does it lock the table? Adding a column to `orders` worries me a bit.",
    );
    let bench = w.post(
        "c-backend",
        "maya",
        "",
        "Both done. Benchmarks attached.",
        16,
    );
    w.attach(&bench, "bench-results.csv", "text/csv", 14_336);
    w.react(&bench, "tomas", "tada");
    w.react(&bench, "priya", "heart");
    w.react(&bench, "sam", "tada");
    w.post(
        "c-backend",
        "priya",
        "",
        "Great work both of you! @alex can you review it before we deploy?",
        11,
    );

    w.post(
        "c-roadmap",
        "priya",
        "",
        "Draft for next quarter is in the doc. Comments welcome until Friday.",
        500,
    );
    w.post(
        "c-roadmap",
        "sam",
        "",
        "I would move the search work before the billing rewrite.",
        466,
    );

    let dm = w.dm_id("maya");
    w.post(
        &dm,
        "maya",
        "",
        "Do you have five minutes later to go over the Q3 plan?",
        23,
    );
    w.unread(&dm, 1, 1);
    let dm = w.dm_id("tomas");
    w.post(&dm, "alex", "", "Thanks for the review yesterday.", 300);
    w.post(&dm, "tomas", "", "Anytime :+1:", 296);
    let dm = w.dm_id("priya");
    w.post(&dm, "priya", "", "Lunch at 12:30?", 166);
    w.post(&dm, "alex", "", "Sure, see you there.", 165);

    w.post(
        "c-design-system",
        "lena",
        "",
        "New **button** tokens are published to the registry.",
        266,
    );
    w.post(
        "c-design-system",
        "sam",
        "",
        "Nice! Does it cover the `danger` variant?",
        260,
    );
    w.post(
        "c-research",
        "lena",
        "",
        "Interview notes from this week are in the shared folder.",
        400,
    );
    w.post(
        "c-brand",
        "sam",
        "",
        "The new wordmark is final, exports coming.",
        666,
    );
    w.scripted = Some(("c-backend".to_owned(), "priya".to_owned()));
    w
}

fn community() -> World {
    let mut w = World::new(
        "alex",
        &[
            ("nora", "online"),
            ("kai", "away"),
            ("elena", "online"),
            ("bram", "offline"),
        ],
        &[("t-club", "Open Source Club")],
    );
    w.channel(
        "t-club",
        "O",
        "announcements",
        "Announcements",
        "Meetups and releases",
    );
    w.channel("t-club", "O", "help", "Help", "Ask anything, be kind");
    w.channel(
        "t-club",
        "O",
        "show-and-tell",
        "Show and Tell",
        "What did you build?",
    );
    w.direct("nora");
    w.post(
        "c-announcements",
        "nora",
        "",
        "The next meetup is on the **12th**, at the library. Pizza included.",
        1000,
    );
    w.post(
        "c-help",
        "kai",
        "",
        "How do you test code that talks to a socket?",
        60,
    );
    w.post(
        "c-help",
        "elena",
        "",
        "I bind to port 0 and hand the address to the code under test.",
        56,
    );
    w.post(
        "c-help",
        "kai",
        "",
        "That is neat, trying it now :eyes:",
        53,
    );
    w.unread("c-help", 2, 0);
    w.post(
        "c-show-and-tell",
        "bram",
        "",
        "A tiny terminal chat client, finally usable! https://example.org/chat",
        200,
    );
    w
}

fn handle(shared: &Shared, method: &str, target: &str, body: &Value) -> Option<Value> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query: HashMap<&str, &str> = query.split('&').filter_map(|p| p.split_once('=')).collect();
    let path = path.strip_prefix("/api/v4/")?;
    let segments: Vec<&str> = path.split('/').collect();
    let mut w = lock(shared);
    let me = w.me.clone();
    let ok = json!({"status": "OK"});
    Some(match (method, segments.as_slice()) {
        ("GET", ["users", "me"]) => w.user(&me),
        ("GET", ["users", "me", "teams"]) => w
            .teams
            .iter()
            .map(|(id, name)| json!({"id": id, "name": name.to_lowercase(), "display_name": name}))
            .collect(),
        ("GET", ["users", "me", "teams", team, "channels"]) => w
            .channels
            .iter()
            .filter(|c| c["team_id"] == *team || c["team_id"] == "")
            .map(|c| w.channel_json(c))
            .collect(),
        ("GET", ["users", "me", "teams", team, "channels", "members"]) => w
            .channels
            .iter()
            .filter(|c| c["team_id"] == *team || c["team_id"] == "")
            .map(|c| {
                let id = c["id"].as_str().unwrap_or_default();
                let (read, mentions) = w.read.get(id).copied().unwrap_or((w.total(id), 0));
                json!({
                    "channel_id": id, "user_id": me, "msg_count": read, "mention_count": mentions,
                    "notify_props": {"mark_unread": "all"},
                })
            })
            .collect(),
        ("GET", ["users", "me", "teams", team, "channels", "categories"]) => {
            let (own, direct): (Vec<_>, Vec<_>) = w
                .channels
                .iter()
                .filter(|c| c["team_id"] == *team || c["team_id"] == "")
                .partition(|c| c["type"] != "D");
            let ids = |list: &[&Value]| list.iter().map(|c| c["id"].clone()).collect::<Vec<_>>();
            json!({
                "order": ["cat-channels", "cat-direct"],
                "categories": [
                    {"id": "cat-channels", "channel_ids": ids(&own)},
                    {"id": "cat-direct", "channel_ids": ids(&direct)},
                ],
            })
        }
        ("POST", ["channels", "members", "me", "view"]) => {
            let channel = body["channel_id"].as_str().unwrap_or_default().to_owned();
            let total = w.total(&channel);
            w.read.insert(channel, (total, 0));
            ok
        }
        ("GET", ["channels", channel, "posts"]) => {
            let mut posts: Vec<&Value> = w
                .posts
                .iter()
                .filter(|p| p["channel_id"] == *channel)
                .collect();
            posts.sort_by_key(|p| std::cmp::Reverse(p["create_at"].as_i64()));
            let older = query.contains_key("before");
            let order: Vec<&Value> = if older {
                Vec::new()
            } else {
                posts.iter().map(|p| &p["id"]).collect()
            };
            let map: serde_json::Map<String, Value> = posts
                .iter()
                .filter(|_| !older)
                .map(|p| {
                    (
                        p["id"].as_str().unwrap_or_default().to_owned(),
                        (*p).clone(),
                    )
                })
                .collect();
            json!({"order": order, "posts": map})
        }
        ("POST", ["posts"]) => {
            let channel = body["channel_id"].as_str().unwrap_or_default().to_owned();
            let root = body["root_id"].as_str().unwrap_or_default().to_owned();
            let message = body["message"].as_str().unwrap_or_default().to_owned();
            let id = w.post(&channel, &me, &root, &message, 0);
            let total = w.total(&channel);
            w.read.insert(channel.clone(), (total, 0));
            if w.scripted.as_ref().is_some_and(|(c, _)| *c == channel) {
                if let Some((channel, who)) = w.scripted.take() {
                    tokio::spawn(answer(shared.clone(), channel, who, id.clone()));
                }
            }
            w.find(&id)?.clone()
        }
        ("PUT", ["posts", id, "patch"]) => {
            let message = body["message"].as_str().unwrap_or_default().to_owned();
            w.edited(id, &message);
            w.find(id)?.clone()
        }
        ("DELETE", ["posts", id]) => {
            w.posts.retain(|p| p["id"] != *id);
            ok
        }
        ("POST", ["posts", _, "pin" | "unpin"]) => ok,
        ("POST", ["reactions"]) => {
            let post = body["post_id"].as_str().unwrap_or_default().to_owned();
            let emoji = body["emoji_name"].as_str().unwrap_or_default().to_owned();
            w.react(&post, &me, &emoji)
        }
        ("DELETE", ["users", _, "posts", post, "reactions", emoji]) => {
            if let Some(found) = w.find(post) {
                if let Some(items) = found["metadata"]["reactions"].as_array_mut() {
                    items.retain(|r| {
                        !(r["emoji_name"] == *emoji && r["user_id"] == format!("u-{me}"))
                    });
                }
            }
            ok
        }
        ("GET", ["users"]) => {
            let channel = query.get("in_channel").copied().unwrap_or_default();
            let direct = w
                .channels
                .iter()
                .find(|c| c["id"] == channel)
                .filter(|c| c["type"] == "D");
            let names: Vec<&String> = match direct {
                Some(c) => {
                    let name = c["name"].as_str().unwrap_or_default();
                    w.users
                        .iter()
                        .filter(|u| name.contains(&format!("u-{u}")))
                        .collect()
                }
                None => w.users.iter().collect(),
            };
            names.iter().map(|u| w.user(u)).collect()
        }
        ("POST", ["users", "ids"]) => body
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(|id| w.user(w.name_of(id)))
            .collect(),
        ("POST", ["users", "status", "ids"]) => body
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(|id| {
                let state = w
                    .status
                    .get(w.name_of(id))
                    .map_or("offline", String::as_str);
                json!({"user_id": id, "status": state})
            })
            .collect(),
        ("PUT", ["users", user, "status"]) => {
            let state = body["status"].as_str().unwrap_or("online").to_owned();
            let name = w.name_of(user).to_owned();
            w.status.insert(name, state);
            ok
        }
        ("GET", ["users", "username", name]) => w
            .users
            .contains(&(*name).to_owned())
            .then(|| w.user(name))?,
        ("POST", ["channels", "direct"]) => {
            let other = body
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(|id| w.name_of(id).to_owned())
                .find(|name| *name != me)?;
            w.direct(&other)
        }
        ("GET", ["teams", team, "channels", "name", name]) => {
            let found = w
                .channels
                .iter()
                .find(|c| c["team_id"] == *team && c["name"] == *name)?;
            w.channel_json(found)
        }
        ("POST", ["channels", _, "members"]) | ("DELETE", ["channels", _, "members", _]) => ok,
        _ => return None,
    })
}

async fn answer(shared: Shared, channel: String, who: String, reply_to: String) {
    tokio::time::sleep(Duration::from_millis(700)).await;
    {
        let w = lock(&shared);
        let data = json!({"parent_id": "", "user_id": format!("u-{who}")});
        w.announce("typing", &data, &channel);
    }
    tokio::time::sleep(Duration::from_millis(2600)).await;
    {
        let mut w = lock(&shared);
        let text = "Perfect, thanks Alex! :tada: I will watch the dashboards when it goes out.";
        let id = w.post(&channel, &who, "", text, 0);
        if let Some(post) = w.find(&id).cloned() {
            w.announce_post(&post);
        }
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let mut w = lock(&shared);
    let reaction = w.react(&reply_to, "maya", "+1");
    let data = json!({"reaction": reaction.to_string()});
    w.announce("reaction_added", &data, &channel);
}

struct Request {
    method: String,
    target: String,
    body: Value,
}

async fn read_request(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> Option<Request> {
    let end = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.ok().filter(|n| *n > 0)?;
        buffer.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
    let length = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::to_owned)
        })
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < end + length {
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.ok().filter(|n| *n > 0)?;
        buffer.extend_from_slice(&chunk[..n]);
    }
    let body = serde_json::from_slice(&buffer[end..end + length]).unwrap_or(Value::Null);
    buffer.drain(..end + length);
    let mut words = head.split_whitespace();
    Some(Request {
        method: words.next()?.to_owned(),
        target: words.next()?.to_owned(),
        body,
    })
}

async fn connection(mut stream: TcpStream, shared: Shared) {
    let mut start = [0u8; 22];
    loop {
        match stream.peek(&mut start).await {
            Ok(22) => break,
            Ok(0) | Err(_) => return,
            Ok(_) => tokio::time::sleep(Duration::from_millis(2)).await,
        }
    }
    if start.starts_with(b"GET /api/v4/websocket") {
        return socket(stream, shared).await;
    }
    let mut buffer = Vec::new();
    while let Some(request) = read_request(&mut stream, &mut buffer).await {
        let answer = handle(&shared, &request.method, &request.target, &request.body);
        let (status, body) = match answer {
            Some(value) => ("200 OK", value),
            None => (
                "404 Not Found",
                json!({"id": "not_found", "message": "not found"}),
            ),
        };
        let body = body.to_string();
        let head = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        if stream
            .write_all(format!("{head}{body}").as_bytes())
            .await
            .is_err()
        {
            return;
        }
    }
}

async fn socket(stream: TcpStream, shared: Shared) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let mut events = lock(&shared).events.subscribe();
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(text) => {
                    if ws.send(Message::text(text)).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
            frame = ws.next() => {
                if !matches!(frame, Some(Ok(_))) {
                    return;
                }
            }
        }
    }
}

async fn serve(listener: TcpListener, shared: Shared) {
    loop {
        if let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(connection(stream, shared.clone()));
        }
    }
}

fn main() -> Result<()> {
    let home = tempfile::tempdir()?;
    let (ports, found) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async {
            let (Ok(a), Ok(b)) = (
                TcpListener::bind("127.0.0.1:0").await,
                TcpListener::bind("127.0.0.1:0").await,
            ) else {
                return;
            };
            let ports = ports.send((
                a.local_addr().map(|a| a.port()).unwrap_or(0),
                b.local_addr().map(|a| a.port()).unwrap_or(0),
            ));
            if ports.is_ok() {
                tokio::join!(
                    serve(a, Arc::new(Mutex::new(work()))),
                    serve(b, Arc::new(Mutex::new(community())))
                );
            }
        });
    });
    let (work_port, club_port) = found.recv().context("the fake servers did not start")?;

    let config = home.path().join("mm");
    std::fs::create_dir_all(&config)?;
    let servers = config.join("servers.toml");
    std::fs::write(
        &servers,
        format!(
            "[[server]]\nname = \"work\"\nurl = \"http://127.0.0.1:{work_port}\"\nlogin = \"alex\"\ntoken = \"demo\"\n\n\
             [[server]]\nname = \"club\"\nurl = \"http://127.0.0.1:{club_port}\"\nlogin = \"alex\"\ntoken = \"demo\"\n"
        ),
    )?;
    std::fs::set_permissions(&servers, std::fs::Permissions::from_mode(0o600))?;

    let beside = std::env::current_exe()?;
    let program = beside
        .parent()
        .and_then(|examples| examples.parent())
        .map(|target| target.join("mm"))
        .context("cannot find the mm binary")?;
    Command::new(&program)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .status()
        .with_context(|| format!("running {}", program.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(shared: &Shared, path: &str) -> Value {
        handle(shared, "GET", &format!("/api/v4/{path}"), &Value::Null)
            .unwrap_or_else(|| panic!("{path} is not served"))
    }

    #[test]
    fn every_read_the_app_makes_is_served() {
        for world in [work(), community()] {
            let shared: Shared = Arc::new(Mutex::new(world));
            assert_eq!(get(&shared, "users/me")["username"], "alex");
            let teams = get(&shared, "users/me/teams");
            let team = teams[0]["id"].as_str().unwrap_or_default().to_owned();
            let base = format!("users/me/teams/{team}/channels");
            let channels = get(&shared, &base);
            assert!(channels.as_array().is_some_and(|c| !c.is_empty()));
            let members = get(&shared, &format!("{base}/members"));
            assert_eq!(
                members.as_array().map(Vec::len),
                channels.as_array().map(Vec::len)
            );
            assert!(get(&shared, &format!("{base}/categories"))["categories"].is_array());
            let first = channels[0]["id"].as_str().unwrap_or_default().to_owned();
            assert!(
                get(&shared, &format!("channels/{first}/posts?per_page=60"))["order"].is_array()
            );
            assert!(get(&shared, &format!("users?in_channel={first}&per_page=200")).is_array());
        }
    }

    #[test]
    fn unread_counts_follow_the_seeded_read_state() {
        let shared: Shared = Arc::new(Mutex::new(work()));
        let members = get(&shared, "users/me/teams/t-eng/channels/members");
        let channels = get(&shared, "users/me/teams/t-eng/channels");
        let number = |list: &Value, key: &str| {
            list.as_array()
                .into_iter()
                .flatten()
                .find(|c| c["id"] == "c-releases" || c["channel_id"] == "c-releases")
                .and_then(|c| c[key].as_i64())
        };
        let total = number(&channels, "total_msg_count");
        let read = number(&members, "msg_count");
        assert_eq!(total.zip(read).map(|(t, r)| t - r), Some(2));
    }

    #[test]
    fn a_written_post_is_kept_and_read_back() {
        let shared: Shared = Arc::new(Mutex::new(work()));
        let body = json!({"channel_id": "c-general", "message": "hello", "root_id": ""});
        let post = handle(&shared, "POST", "/api/v4/posts", &body);
        assert_eq!(post.map(|p| p["message"].clone()), Some(json!("hello")));
        let list = get(&shared, "channels/c-general/posts?per_page=60");
        assert_eq!(list["posts"].as_object().map(serde_json::Map::len), Some(5));
    }

    #[test]
    fn a_direct_channel_is_created_once() {
        let shared: Shared = Arc::new(Mutex::new(work()));
        let body = json!(["u-alex", "u-jonas"]);
        let first = handle(&shared, "POST", "/api/v4/channels/direct", &body);
        let again = handle(&shared, "POST", "/api/v4/channels/direct", &body);
        assert!(first.is_some() && first == again);
    }
}
