//! Servers, teams, channels and messages, with the rules that read them.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Datelike, Local, TimeZone};

use crate::api::types::{
    CategoryList, Channel, ChannelId, ChannelKind, ChannelMember, Post, PostId, PostList,
    Team as WireTeam, User, UserId,
};
use crate::api::Client;

const GROUP_WINDOW_MS: i64 = 5 * 60 * 1000;
const PALETTE: [u8; 12] = [1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14];

/// A post, ready to be shown.
#[derive(Debug, Clone)]
pub struct Message {
    pub id: PostId,
    pub at: DateTime<Local>,
    pub ms: i64,
    pub user: UserId,
    pub text: String,
    pub root: Option<PostId>,
    pub edited: bool,
    pub system: bool,
    pub files: Vec<String>,
}

impl Message {
    fn from_post(post: &Post) -> Self {
        Self {
            id: post.id.clone(),
            at: Local
                .timestamp_millis_opt(post.create_at)
                .single()
                .unwrap_or_default(),
            ms: post.create_at,
            user: post.user_id.clone(),
            text: post.message.clone(),
            root: Some(post.root_id.clone())
                .filter(|r| !r.is_empty())
                .map(PostId),
            edited: post.edit_at > 0,
            system: post.kind.starts_with("system_"),
            files: post
                .metadata
                .files
                .iter()
                .flatten()
                .map(|f| f.name.clone())
                .collect(),
        }
    }
}

/// Posts of a page oldest first, without the deleted ones.
pub fn messages_of(list: &PostList) -> Vec<Message> {
    let mut messages: Vec<Message> = list
        .ordered()
        .filter(|p| p.delete_at == 0)
        .map(Message::from_post)
        .collect();
    messages.sort_by_key(|m| m.ms);
    messages
}

/// Whether `message` continues the group of `prev`, so its author and time are not repeated.
pub fn group_with_previous(prev: &Message, message: &Message) -> bool {
    let gap = message.ms - prev.ms;
    prev.user == message.user
        && prev.at.date_naive() == message.at.date_naive()
        && (0..GROUP_WINDOW_MS).contains(&gap)
}

/// Index of the first message of the group that holds message `i`.
pub fn group_start(messages: &[Message], mut i: usize) -> usize {
    while i > 0 && group_with_previous(&messages[i - 1], &messages[i]) {
        i -= 1;
    }
    i
}

/// A day written for a separator line: Today, Yesterday, or the date.
pub fn day_label(day: DateTime<Local>, now: DateTime<Local>) -> String {
    let (today, date) = (now.date_naive(), day.date_naive());
    if date == today {
        return "Today".into();
    }
    if today.pred_opt() == Some(date) {
        return "Yesterday".into();
    }
    match day.year() == now.year() {
        true => day.format("%A %-d %B").to_string(),
        false => day.format("%A %-d %B %Y").to_string(),
    }
}

/// A stable ANSI color index for a user.
pub fn user_color(id: &UserId) -> u8 {
    let hash =
        id.0.bytes()
            .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    PALETTE[hash as usize % PALETTE.len()]
}

/// A channel with the reader's membership.
#[derive(Debug, Clone)]
pub struct Chan {
    pub info: Channel,
    pub member: Option<ChannelMember>,
}

impl Chan {
    /// Whether the tab should be marked: new posts, or a mention in a muted channel.
    pub fn unread(&self) -> bool {
        let Some(member) = &self.member else {
            return false;
        };
        let muted = member.notify_props.mark_unread == "mention";
        self.mentions() || (!muted && self.info.last_post_at > member.last_viewed_at)
    }

    /// Whether someone mentioned the reader since the channel was last viewed.
    pub fn mentions(&self) -> bool {
        self.member.as_ref().is_some_and(|m| m.mention_count > 0)
    }

    /// Index of the first message newer than the last view, when it is not the first one.
    pub fn first_unread(&self, messages: &[Message]) -> Option<usize> {
        let last = self.member.as_ref()?.last_viewed_at;
        messages
            .iter()
            .position(|m| m.ms > last)
            .filter(|&i| i > 0 && last > 0)
    }
}

/// A team of a server, with its channels once they are loaded.
#[derive(Debug, Clone)]
pub struct Team {
    pub info: WireTeam,
    pub chans: Option<Vec<Chan>>,
    pub loading: bool,
    pub cur: usize,
}

/// Puts channels in sidebar order: categories first, then what they leave out.
/// Direct and group messages the categories do not list are hidden.
pub fn order_channels(
    channels: Vec<Channel>,
    members: Vec<ChannelMember>,
    categories: Option<&CategoryList>,
) -> Vec<Chan> {
    let mut members: HashMap<_, _> = members
        .into_iter()
        .map(|m| (m.channel_id.clone(), m))
        .collect();
    let mut channels: Vec<Channel> = channels.into_iter().filter(|c| c.delete_at == 0).collect();
    let is_direct = |c: &Channel| matches!(c.kind, ChannelKind::Direct | ChannelKind::Group);

    channels.sort_by(|a, b| match (is_direct(a), is_direct(b)) {
        (false, false) => a
            .display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase()),
        (true, true) => b.last_post_at.cmp(&a.last_post_at),
        (left, right) => left.cmp(&right),
    });

    let ordered: Vec<Channel> = match categories {
        Some(list) => {
            let mut rest: Vec<Option<Channel>> = channels.into_iter().map(Some).collect();
            let mut ordered = Vec::new();
            for id in list.channel_ids() {
                if let Some(slot) = rest
                    .iter_mut()
                    .find(|c| c.as_ref().is_some_and(|c| &c.id == id))
                {
                    ordered.extend(slot.take());
                }
            }
            ordered.extend(rest.into_iter().flatten().filter(|c| !is_direct(c)));
            ordered
        }
        None => channels,
    };

    ordered
        .into_iter()
        .map(|info| Chan {
            member: members.remove(&info.id),
            info,
        })
        .collect()
}

/// A logged-in server, or one that is still connecting.
#[derive(Debug)]
pub struct Server {
    pub name: String,
    pub client: Option<Arc<Client>>,
    pub me: Option<User>,
    pub users: HashMap<UserId, User>,
    pub asked: HashSet<UserId>,
    pub teams: Vec<Team>,
    pub messages: HashMap<ChannelId, Vec<Message>>,
    pub loading: HashSet<ChannelId>,
}

impl Server {
    /// A server that is not connected yet.
    pub fn new(name: String) -> Self {
        Self {
            name,
            client: None,
            me: None,
            users: HashMap::new(),
            asked: HashSet::new(),
            teams: Vec::new(),
            messages: HashMap::new(),
            loading: HashSet::new(),
        }
    }

    /// The name of a user, or `?` while it is being fetched.
    pub fn username(&self, id: &UserId) -> &str {
        self.users.get(id).map_or("?", |u| u.username.as_str())
    }

    /// The tab label of a channel: `#` public, `+` private, `@` direct and group.
    pub fn label(&self, chan: &Chan) -> String {
        let info = &chan.info;
        let name = if info.display_name.is_empty() {
            &info.name
        } else {
            &info.display_name
        };
        match info.kind {
            ChannelKind::Public => format!("#{name}"),
            ChannelKind::Private => format!("+{name}"),
            ChannelKind::Group => format!("@{name}"),
            ChannelKind::Direct => match self.other_of(info) {
                Some(other) => format!("@{}", self.username(&other)),
                None => "@…".into(),
            },
        }
    }

    /// The other person of a direct channel, whose name is `id__id`.
    pub fn other_of(&self, info: &Channel) -> Option<UserId> {
        let me = &self.me.as_ref()?.id.0;
        info.name
            .split("__")
            .find(|id| id != me)
            .map(|id| UserId(id.to_owned()))
    }

    /// Ids among `wanted` that were never asked for, marked as asked.
    pub fn unknown<'a>(&mut self, wanted: impl Iterator<Item = &'a UserId>) -> Vec<UserId> {
        let mut ids = Vec::new();
        for id in wanted {
            if !self.users.contains_key(id) && self.asked.insert(id.clone()) {
                ids.push(id.clone());
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(user: &str, ms: i64) -> Message {
        Message {
            id: PostId(format!("p{ms}")),
            at: Local.timestamp_millis_opt(ms).single().unwrap(),
            ms,
            user: UserId(user.into()),
            text: String::new(),
            root: None,
            edited: false,
            system: false,
            files: Vec::new(),
        }
    }

    fn channel(id: &str, kind: ChannelKind, name: &str, last: i64) -> Channel {
        Channel {
            id: ChannelId(id.into()),
            kind,
            display_name: name.into(),
            name: name.into(),
            last_post_at: last,
            delete_at: 0,
        }
    }

    fn member(id: &str, viewed: i64, mentions: i64, mark: &str) -> ChannelMember {
        serde_json::from_value(serde_json::json!({
            "channel_id": id, "last_viewed_at": viewed, "mention_count": mentions,
            "notify_props": {"mark_unread": mark}
        }))
        .unwrap()
    }

    fn chan(last_post: i64, member: Option<ChannelMember>) -> Chan {
        Chan {
            info: channel("c", ChannelKind::Public, "c", last_post),
            member,
        }
    }

    const NOON: i64 = 1_700_000_000_000;

    #[test]
    fn same_author_within_five_minutes_groups() {
        assert!(group_with_previous(
            &msg("a", NOON),
            &msg("a", NOON + 60_000)
        ));
        assert!(!group_with_previous(
            &msg("a", NOON),
            &msg("b", NOON + 60_000)
        ));
        assert!(!group_with_previous(
            &msg("a", NOON),
            &msg("a", NOON + 6 * 60_000)
        ));
        assert!(!group_with_previous(&msg("a", NOON), &msg("a", NOON - 1)));
    }

    #[test]
    fn group_start_walks_back() {
        let list = [
            msg("a", NOON),
            msg("a", NOON + 1000),
            msg("b", NOON + 2000),
            msg("b", NOON + 3000),
        ];
        assert_eq!(group_start(&list, 1), 0);
        assert_eq!(group_start(&list, 3), 2);
    }

    #[test]
    fn days_are_named() {
        let now = Local.with_ymd_and_hms(2026, 7, 23, 12, 0, 0).unwrap();
        let day = |y, m, d| Local.with_ymd_and_hms(y, m, d, 9, 0, 0).unwrap();
        assert_eq!(day_label(day(2026, 7, 23), now), "Today");
        assert_eq!(day_label(day(2026, 7, 22), now), "Yesterday");
        assert_eq!(day_label(day(2026, 7, 14), now), "Tuesday 14 July");
        assert_eq!(day_label(day(2025, 7, 14), now), "Monday 14 July 2025");
    }

    #[test]
    fn colors_are_stable_ansi() {
        let id = UserId("abc".into());
        assert_eq!(user_color(&id), user_color(&id));
        assert!(PALETTE.contains(&user_color(&id)));
    }

    #[test]
    fn unread_follows_timestamps_and_mutes() {
        assert!(chan(10, Some(member("c", 5, 0, "all"))).unread());
        assert!(!chan(5, Some(member("c", 5, 0, "all"))).unread());
        assert!(!chan(10, Some(member("c", 5, 0, "mention"))).unread());
        assert!(chan(10, Some(member("c", 5, 2, "mention"))).unread());
        assert!(!chan(10, None).unread());
    }

    #[test]
    fn first_unread_skips_the_first_message() {
        let c = chan(0, Some(member("c", NOON + 500, 0, "all")));
        let list = [msg("a", NOON), msg("a", NOON + 1000), msg("b", NOON + 2000)];
        assert_eq!(c.first_unread(&list), Some(1));
        assert_eq!(c.first_unread(&list[1..]), None);
    }

    #[test]
    fn categories_order_channels_and_hide_unlisted_direct_ones() {
        let channels = vec![
            channel("c1", ChannelKind::Public, "Zed", 0),
            channel("c2", ChannelKind::Direct, "", 9),
            channel("c3", ChannelKind::Private, "Alpha", 0),
            channel("c4", ChannelKind::Direct, "", 5),
            channel("c5", ChannelKind::Public, "Beta", 0),
        ];
        let list: CategoryList = serde_json::from_value(serde_json::json!({
            "order": ["a"], "categories": [{"id": "a", "channel_ids": ["c3", "c2"]}]
        }))
        .unwrap();
        let names = |chans: Vec<Chan>| {
            chans
                .iter()
                .map(|c| c.info.id.0.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(order_channels(channels.clone(), vec![], Some(&list))),
            ["c3", "c2", "c5", "c1"]
        );
        assert_eq!(
            names(order_channels(channels, vec![], None)),
            ["c3", "c5", "c1", "c2", "c4"]
        );
    }

    #[test]
    fn direct_channels_are_labelled_with_the_other_user() {
        let mut server = Server::new("work".into());
        let user = |id: &str, name: &str| User {
            id: UserId(id.into()),
            username: name.into(),
        };
        server.me = Some(user("u1", "paul"));
        server.users.insert(UserId("u2".into()), user("u2", "ana"));
        let dm = Chan {
            info: Channel {
                name: "u1__u2".into(),
                ..channel("c", ChannelKind::Direct, "", 0)
            },
            member: None,
        };
        assert_eq!(server.label(&dm), "@ana");
        assert_eq!(server.label(&chan(0, None)), "#c");
    }

    #[test]
    fn unknown_users_are_asked_once() {
        let mut server = Server::new("work".into());
        let ids = [UserId("u2".into()), UserId("u2".into())];
        assert_eq!(server.unknown(ids.iter()).len(), 1);
        assert!(server.unknown(ids.iter()).is_empty());
    }
}
