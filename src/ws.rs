//! The event stream: frames, their double-encoded payloads, and the reconnecting loop.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_tungstenite::tungstenite::{self, Message as Frame};

use crate::api::types::{ChannelId, Post, PostId, Reaction, Status, UserId};
use crate::api::Client;

const IDLE: Duration = Duration::from_secs(120);

/// How long to wait between attempts to reconnect: doubling from `min` up to `max`.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub min: Duration,
    pub max: Duration,
}

/// The waits used for real.
pub const BACKOFF: Backoff = Backoff {
    min: Duration::from_secs(1),
    max: Duration::from_secs(30),
};

/// Something that happened on the server.
#[derive(Debug, Clone)]
pub enum Event {
    Posted { post: Post, mentions: Vec<UserId> },
    Edited(Post),
    Deleted(Post),
    ReactionAdded(Reaction),
    ReactionRemoved(Reaction),
    Viewed(ChannelId),
    Typing { channel: ChannelId, user: UserId },
    Status { user: UserId, status: Status },
}

/// Something to tell the server over the open connection.
#[derive(Debug, Clone)]
pub enum Action {
    /// The reader is typing in a channel, or in the thread of `parent`.
    Typing {
        channel: ChannelId,
        parent: Option<PostId>,
    },
}

impl Action {
    fn frame(&self, seq: u64) -> String {
        match self {
            Self::Typing { channel, parent } => serde_json::json!({
                "action": "user_typing",
                "seq": seq,
                "data": {
                    "channel_id": channel.0,
                    "parent_id": parent.as_ref().map_or("", |p| p.0.as_str()),
                },
            })
            .to_string(),
        }
    }
}

/// What the connection loop reports.
#[derive(Debug)]
pub enum Ws {
    /// The stream is open; `resync` is set when it is not the first time.
    Up {
        resync: bool,
    },
    /// The stream ended, with the reason.
    Down(String),
    Event(Box<Event>),
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    event: String,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    broadcast: Value,
}

/// A payload the server sends as a string holding JSON.
fn inner<T: DeserializeOwned>(data: &Value, key: &str) -> Option<T> {
    serde_json::from_str(data.get(key)?.as_str()?).ok()
}

/// Reads one text frame; none when it is not an event we handle or is malformed.
pub fn parse(text: &str) -> Option<Event> {
    let Envelope {
        event,
        data,
        broadcast,
    } = serde_json::from_str(text).ok()?;
    match event.as_str() {
        "posted" => Some(Event::Posted {
            post: inner(&data, "post")?,
            mentions: inner(&data, "mentions").unwrap_or_default(),
        }),
        "post_edited" => Some(Event::Edited(inner(&data, "post")?)),
        "post_deleted" => Some(Event::Deleted(inner(&data, "post")?)),
        "reaction_added" => Some(Event::ReactionAdded(inner(&data, "reaction")?)),
        "reaction_removed" => Some(Event::ReactionRemoved(inner(&data, "reaction")?)),
        "channel_viewed" => Some(Event::Viewed(ChannelId(
            data.get("channel_id")?.as_str()?.to_owned(),
        ))),
        "status_change" => Some(Event::Status {
            user: UserId(data.get("user_id")?.as_str()?.to_owned()),
            status: Status::parse(data.get("status")?.as_str()?),
        }),
        "typing" => Some(Event::Typing {
            channel: ChannelId(broadcast.get("channel_id")?.as_str()?.to_owned()),
            user: UserId(data.get("user_id")?.as_str()?.to_owned()),
        }),
        _ => None,
    }
}

async fn connect(
    client: &Client,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tungstenite::Error,
> {
    use tungstenite::client::IntoClientRequest;
    let mut request = client.ws_url().into_client_request()?;
    let bearer = tungstenite::http::HeaderValue::from_str(&format!("Bearer {}", client.token()))
        .map_err(tungstenite::http::Error::from)?;
    request.headers_mut().insert("Authorization", bearer);
    Ok(tokio_tungstenite::connect_async(request).await?.0)
}

fn dump(path: &OsStr, text: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{text}");
    }
}

async fn listen<S>(
    stream: &mut S,
    actions: &mut UnboundedReceiver<Action>,
    send: &impl Fn(Ws) -> bool,
) -> String
where
    S: Stream<Item = Result<Frame, tungstenite::Error>>
        + Sink<Frame, Error = tungstenite::Error>
        + Unpin,
{
    let dump_to = std::env::var_os("MM_WS_DUMP");
    let mut seq = 0;
    loop {
        tokio::select! {
            Some(action) = actions.recv() => {
                seq += 1;
                if let Err(e) = stream.send(Frame::text(action.frame(seq))).await {
                    return e.to_string();
                }
            }
            read = tokio::time::timeout(IDLE, stream.next()) => match read {
                Err(_) => return format!("no data for {} s", IDLE.as_secs()),
                Ok(None) => return "connection closed".into(),
                Ok(Some(Err(e))) => return e.to_string(),
                Ok(Some(Ok(Frame::Close(_)))) => return "closed by the server".into(),
                Ok(Some(Ok(Frame::Text(text)))) => {
                    if let Some(path) = &dump_to {
                        dump(path, text.as_str());
                    }
                    if let Some(event) = parse(text.as_str()) {
                        if !send(Ws::Event(Box::new(event))) {
                            return "stopped".into();
                        }
                    }
                }
                Ok(Some(Ok(_))) => {}
            },
        }
    }
}

/// Keeps one server's stream open for as long as `send` accepts what it reports.
pub async fn run(
    client: Arc<Client>,
    backoff: Backoff,
    mut actions: UnboundedReceiver<Action>,
    send: impl Fn(Ws) -> bool,
) {
    let (mut wait, mut first) = (backoff.min, true);
    loop {
        let reason = match connect(&client).await {
            Ok(mut stream) => {
                if !send(Ws::Up { resync: !first }) {
                    return;
                }
                (wait, first) = (backoff.min, false);
                while actions.try_recv().is_ok() {}
                listen(&mut stream, &mut actions, &send).await
            }
            Err(e) => {
                if matches!(&e, tungstenite::Error::Http(r) if r.status() == 401) {
                    let _ = client.relogin().await;
                }
                e.to_string()
            }
        };
        if !send(Ws::Down(reason)) {
            return;
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(backoff.max);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::net::TcpListener;
    use tokio::sync::mpsc::unbounded_channel;
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
    use tokio_tungstenite::tungstenite::http::StatusCode;

    use super::*;
    use crate::config::Credentials;

    fn frame(label: &str) -> String {
        include_str!("../tests/fixtures/ws_events.txt")
            .lines()
            .find_map(|l| l.strip_prefix(label)?.strip_prefix('\t'))
            .unwrap()
            .to_owned()
    }

    #[test]
    fn posted_carries_the_post_and_the_mentions_decoded_twice() {
        let Some(Event::Posted { post, mentions }) = parse(&frame("posted")) else {
            panic!("not a post");
        };
        assert_eq!(post.id.0, "p9");
        assert_eq!(post.channel_id.0, "c1");
        assert_eq!(post.message, "hey @paul");
        assert_eq!(mentions, [UserId("u1".into())]);
    }

    #[test]
    fn every_other_event_is_read() {
        assert!(
            matches!(parse(&frame("edited")), Some(Event::Edited(p)) if p.message == "hey @paul!")
        );
        assert!(matches!(parse(&frame("deleted")), Some(Event::Deleted(p)) if p.id.0 == "p9"));
        assert!(
            matches!(parse(&frame("reaction_added")), Some(Event::ReactionAdded(r)) if r.emoji_name == "tada")
        );
        assert!(
            matches!(parse(&frame("reaction_removed")), Some(Event::ReactionRemoved(r)) if r.user_id.0 == "u2")
        );
        assert!(matches!(parse(&frame("viewed")), Some(Event::Viewed(c)) if c.0 == "c1"));
    }

    #[test]
    fn typing_names_the_channel_and_the_user() {
        let Some(Event::Typing { channel, user }) = parse(&frame("typing")) else {
            panic!("not typing");
        };
        assert_eq!((channel.0.as_str(), user.0.as_str()), ("c1", "u2"));
    }

    #[test]
    fn a_status_change_names_the_user_and_the_new_status() {
        let Some(Event::Status { user, status }) = parse(&frame("status")) else {
            panic!("not a status");
        };
        assert_eq!((user.0.as_str(), status), ("u2", Status::Away));
    }

    #[test]
    fn a_typing_action_is_the_documented_frame() {
        let action = Action::Typing {
            channel: ChannelId("c1".into()),
            parent: Some(PostId("p0".into())),
        };
        let sent: Value = serde_json::from_str(&action.frame(3)).unwrap();
        assert_eq!(
            sent,
            serde_json::json!({
                "action": "user_typing", "seq": 3,
                "data": {"channel_id": "c1", "parent_id": "p0"}
            })
        );
    }

    #[tokio::test]
    async fn actions_reach_the_server_with_a_sequence_number() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (frames, mut received) = unbounded_channel();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            while let Some(Ok(Frame::Text(text))) = ws.next().await {
                let _ = frames.send(text.as_str().to_owned());
            }
        });
        let client = Client::connect(&url, Credentials::Token("tok".into()), None)
            .await
            .unwrap();
        let (actions, receiver) = unbounded_channel();
        let (up, mut up_rx) = unbounded_channel();
        let backoff = Backoff {
            min: Duration::from_millis(5),
            max: Duration::from_millis(20),
        };
        tokio::spawn(run(Arc::new(client), backoff, receiver, move |w| {
            up.send(w).is_ok()
        }));
        assert!(matches!(up_rx.recv().await, Some(Ws::Up { resync: false })));
        for channel in ["c1", "c2"] {
            let channel = ChannelId(channel.into());
            actions
                .send(Action::Typing {
                    channel,
                    parent: None,
                })
                .unwrap();
        }
        let first: Value = serde_json::from_str(&received.recv().await.unwrap()).unwrap();
        let second: Value = serde_json::from_str(&received.recv().await.unwrap()).unwrap();
        assert_eq!(
            (first["seq"].as_u64(), first["data"]["channel_id"].as_str()),
            (Some(1), Some("c1"))
        );
        assert_eq!(
            (
                second["seq"].as_u64(),
                second["data"]["channel_id"].as_str()
            ),
            (Some(2), Some("c2"))
        );
    }

    #[test]
    fn replies_unknown_events_and_bad_payloads_are_ignored() {
        for label in ["reply", "bad_inner", "not_json"] {
            assert!(parse(&frame(label)).is_none(), "{label}");
        }
    }

    #[allow(clippy::result_large_err)]
    async fn serve(listener: TcpListener, rounds: usize, reject_first: bool) {
        let mut sent = false;
        for round in 0..rounds {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let reject = reject_first && round == 0;
            let check = move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
                let auth = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok());
                if reject || auth != Some("Bearer tok") {
                    let mut no = ErrorResponse::new(None);
                    *no.status_mut() = StatusCode::UNAUTHORIZED;
                    return Err(no);
                }
                Ok(resp)
            };
            let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(socket, check).await else {
                continue;
            };
            if !sent {
                sent = true;
                let _ = futures_util::SinkExt::send(&mut ws, Frame::text(frame("posted"))).await;
            }
            let _ = ws.close(None).await;
        }
    }

    async fn watch(reject_first: bool) -> Vec<String> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(serve(listener, 3, reject_first));
        let client = Client::connect(&url, Credentials::Token("tok".into()), None)
            .await
            .unwrap();
        let (tx, mut rx) = unbounded_channel();
        let backoff = Backoff {
            min: Duration::from_millis(5),
            max: Duration::from_millis(20),
        };
        let (_actions, receiver) = unbounded_channel();
        tokio::spawn(run(Arc::new(client), backoff, receiver, move |w| {
            tx.send(w).is_ok()
        }));
        let mut seen = Vec::new();
        while seen.len() < 5 {
            let Ok(Some(w)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await else {
                break;
            };
            seen.push(match w {
                Ws::Up { resync } => format!("up {resync}"),
                Ws::Down(_) => "down".into(),
                Ws::Event(_) => "event".into(),
            });
        }
        seen
    }

    #[tokio::test]
    async fn the_stream_comes_back_and_asks_for_a_resync() {
        assert_eq!(
            watch(false).await,
            ["up false", "event", "down", "up true", "down"]
        );
    }

    #[tokio::test]
    async fn a_rejected_upgrade_is_retried() {
        assert_eq!(
            watch(true).await,
            ["down", "up false", "event", "down", "up true"]
        );
    }
}
