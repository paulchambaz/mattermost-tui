//! Wire types for the Mattermost REST API.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

macro_rules! id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);
    };
}

id!(UserId, "Identifier of a user.");
id!(TeamId, "Identifier of a team.");
id!(ChannelId, "Identifier of a channel.");
id!(PostId, "Identifier of a post.");

/// A Mattermost account.
#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: UserId,
    pub username: String,
}

/// A team the reader belongs to.
#[derive(Debug, Clone, Deserialize)]
pub struct Team {
    pub id: TeamId,
    pub display_name: String,
}

/// The four kinds of channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ChannelKind {
    /// Public channel, shown with `#`.
    #[serde(rename = "O")]
    Public,
    /// Private channel, shown with `+`.
    #[serde(rename = "P")]
    Private,
    /// Direct message between two users, shown with `@`.
    #[serde(rename = "D")]
    Direct,
    /// Group message, shown with `@`.
    #[serde(rename = "G")]
    Group,
}

/// A channel, direct message or group message.
#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    #[serde(rename = "type")]
    pub kind: ChannelKind,
    #[serde(default)]
    pub display_name: String,
    pub name: String,
    #[serde(default)]
    pub last_post_at: i64,
    #[serde(default)]
    pub delete_at: i64,
}

/// Notification settings of one membership.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NotifyProps {
    #[serde(default)]
    pub mark_unread: String,
}

/// The reader's membership of a channel, which carries the read state.
#[derive(Debug, Clone, Deserialize)]
pub struct ChannelMember {
    pub channel_id: ChannelId,
    #[serde(default)]
    pub last_viewed_at: i64,
    #[serde(default)]
    pub mention_count: i64,
    #[serde(default)]
    pub notify_props: NotifyProps,
}

/// A file attached to a post.
#[derive(Debug, Clone, Deserialize)]
pub struct FileInfo {
    pub name: String,
}

/// Extra data the server sends along with a post.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Metadata {
    #[serde(default)]
    pub files: Option<Vec<FileInfo>>,
}

/// A message. An empty `root_id` means it is not a reply.
#[derive(Debug, Clone, Deserialize)]
pub struct Post {
    pub id: PostId,
    pub create_at: i64,
    #[serde(default)]
    pub edit_at: i64,
    #[serde(default)]
    pub delete_at: i64,
    pub user_id: UserId,
    #[serde(default)]
    pub root_id: String,
    #[serde(default)]
    pub message: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub metadata: Metadata,
}

/// One sidebar category and the channels it holds.
#[derive(Debug, Clone, Deserialize)]
pub struct Category {
    pub id: String,
    #[serde(default)]
    pub channel_ids: Vec<ChannelId>,
}

/// The sidebar categories of a team, with the order they are shown in.
#[derive(Debug, Clone, Deserialize)]
pub struct CategoryList {
    #[serde(default)]
    pub order: Vec<String>,
    pub categories: Vec<Category>,
}

impl CategoryList {
    /// Channel ids in sidebar order: categories as ordered, channels as listed.
    pub fn channel_ids(&self) -> impl Iterator<Item = &ChannelId> {
        let ordered = self
            .order
            .iter()
            .filter_map(|id| self.categories.iter().find(|c| &c.id == id));
        let unordered = self
            .categories
            .iter()
            .filter(|c| !self.order.contains(&c.id));
        ordered.chain(unordered).flat_map(|c| c.channel_ids.iter())
    }
}

/// A page of posts; `order` lists their ids newest first.
#[derive(Debug, Clone, Deserialize)]
pub struct PostList {
    pub order: Vec<PostId>,
    pub posts: HashMap<PostId, Post>,
}

impl PostList {
    /// The posts in the order the server listed them, newest first.
    pub fn ordered(&self) -> impl Iterator<Item = &Post> {
        self.order.iter().filter_map(|id| self.posts.get(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posts_come_newest_first_with_metadata() {
        let list: PostList =
            serde_json::from_str(include_str!("../../tests/fixtures/posts.json")).unwrap();
        let posts: Vec<_> = list.ordered().collect();
        assert_eq!(posts[0].id, PostId("p2".into()));
        assert_eq!(posts[0].root_id, "p1");
        assert!(posts[0].edit_at > 0);
        let files = posts[0].metadata.files.as_deref().unwrap_or_default();
        assert_eq!(files[0].name, "a.png");
        assert!(posts[1].metadata.files.is_none());
    }

    #[test]
    fn channels_cover_all_kinds() {
        let channels: Vec<Channel> =
            serde_json::from_str(include_str!("../../tests/fixtures/channels.json")).unwrap();
        let kinds: Vec<_> = channels.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            [
                ChannelKind::Public,
                ChannelKind::Direct,
                ChannelKind::Private,
                ChannelKind::Group
            ]
        );
        assert_eq!(channels[1].name, "u1__u2");
    }

    #[test]
    fn categories_give_the_sidebar_order() {
        let list: CategoryList =
            serde_json::from_str(include_str!("../../tests/fixtures/categories.json")).unwrap();
        let ids: Vec<_> = list.channel_ids().map(|c| c.0.as_str()).collect();
        assert_eq!(ids, ["c3", "c1", "c2"]);
    }

    #[test]
    fn members_carry_read_state() {
        let members: Vec<ChannelMember> =
            serde_json::from_str(include_str!("../../tests/fixtures/members.json")).unwrap();
        assert_eq!(members[0].mention_count, 1);
        assert_eq!(members[0].notify_props.mark_unread, "all");
    }
}
