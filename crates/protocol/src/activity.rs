//! The queue and the history of events, as the control panel sees them.
//!
//! Everything that takes the stage (alerts, spoken messages, sounds set to
//! wait) waits in one queue on the server and plays one at a time on the
//! overlays with alerts. Events that matter on stream are kept in a history,
//! with what they caused, and can be replayed through the cards.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::AlertPart;
use crate::plugin::TextPart;

/// The history keeps this many events of each kind, the newest, so many
/// follows don't push out the subs.
pub const HISTORY_LIMIT: usize = 1_000;

/// The queue lists this many waiting items; the rest are only counted.
pub const QUEUE_LIST_LIMIT: usize = 100;

/// Something that waits in the queue, or is on stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueItem {
    /// Unique, kept across restarts.
    pub id: u64,
    /// What it is: its node's title ("Alert", "Speak"; every queued item
    /// is a plugin's effect).
    pub label: String,
    /// What it shows or says, for people.
    pub text: String,
    /// `text` with its emotes and text styles (2026-10-15); empty when it
    /// has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<TextPart>,
    /// The card that made it; empty for previews.
    pub card: String,
    /// What it came from, for people ("TestViewer cheered 500 bits"); `None`
    /// for chat messages and previews.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    pub origin: ItemOrigin,
    /// The kind of event behind it, for clearing one kind.
    #[serde(default)]
    pub group: QueueGroup,
    /// The viewers in its event who picked a name colour in chat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<NameColor>,
    /// Unix ms.
    pub queued_at_ms: u64,
}

/// A viewer's name as it reads in a summary, and the colour they picked
/// for it in chat (`#rrggbb`). Only events that come through chat have one:
/// EventSub (rewards, follows) doesn't send it, and those names have an
/// empty colour (since 2026-10-16; only coloured names were kept before).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameColor {
    pub name: String,
    pub color: String,
    /// The viewer's platform user id, which opens their card; empty in
    /// entries from before 2026-10-16 (the card is found by name then).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
}

/// What a queue item came from, as far as clearing goes: a kind of event
/// (the history's kinds), or something else (chat read aloud, previews).
/// The queue's Clear can be limited to one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueGroup {
    Sub,
    GiftBomb,
    Bits,
    Raid,
    Reward,
    Follow,
    HypeTrain,
    Support,
    #[default]
    Other,
}

impl QueueGroup {
    pub const ALL: [QueueGroup; 9] = [
        QueueGroup::Sub,
        QueueGroup::GiftBomb,
        QueueGroup::Bits,
        QueueGroup::Raid,
        QueueGroup::Reward,
        QueueGroup::Follow,
        QueueGroup::HypeTrain,
        QueueGroup::Support,
        QueueGroup::Other,
    ];

    pub fn of(kind: Option<EventKind>) -> QueueGroup {
        match kind {
            Some(EventKind::Sub) => QueueGroup::Sub,
            Some(EventKind::GiftBomb) => QueueGroup::GiftBomb,
            Some(EventKind::Bits) => QueueGroup::Bits,
            Some(EventKind::Raid) => QueueGroup::Raid,
            Some(EventKind::Reward) => QueueGroup::Reward,
            Some(EventKind::Follow) => QueueGroup::Follow,
            Some(EventKind::HypeTrain) => QueueGroup::HypeTrain,
            Some(EventKind::Support) => QueueGroup::Support,
            None => QueueGroup::Other,
        }
    }

    pub fn event_kind(self) -> Option<EventKind> {
        EventKind::ALL.into_iter().find(|kind| QueueGroup::of(Some(*kind)) == self)
    }

    /// As in JSON and the control API (`?kind=follow`).
    pub fn name(self) -> &'static str {
        match self {
            QueueGroup::Sub => "sub",
            QueueGroup::GiftBomb => "gift_bomb",
            QueueGroup::Bits => "bits",
            QueueGroup::Raid => "raid",
            QueueGroup::Reward => "reward",
            QueueGroup::Follow => "follow",
            QueueGroup::HypeTrain => "hype_train",
            QueueGroup::Support => "support",
            QueueGroup::Other => "other",
        }
    }

    pub fn parse(name: &str) -> Option<QueueGroup> {
        QueueGroup::ALL.into_iter().find(|group| group.name() == name)
    }

    /// "Follows", like the history's chips; "Chat & other".
    pub fn plural(self) -> &'static str {
        self.event_kind().map_or("Chat & other", EventKind::plural)
    }
}

/// Why an item is in the queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemOrigin {
    /// Something that happened on stream.
    #[default]
    Live,
    /// An event from the history, replayed.
    Replay,
    /// A test event or a preview from the panel.
    Test,
}

impl ItemOrigin {
    pub fn is_live(&self) -> bool {
        *self == ItemOrigin::Live
    }
}

/// The queue as it is now.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueState {
    /// Paused: what's on stage finishes, the rest waits (new items still
    /// join).
    pub paused: bool,
    /// Overlays showing plugin pages connected. With none, the queue
    /// waits.
    pub players: usize,
    pub on_stage: Option<QueueItem>,
    /// The first [`QUEUE_LIST_LIMIT`] waiting, in order.
    pub waiting: Vec<QueueItem>,
    /// Everything waiting, listed or not.
    pub waiting_count: usize,
    /// Everything waiting, by what it came from (only those with some).
    #[serde(default)]
    pub by_group: BTreeMap<QueueGroup, usize>,
    /// The next item is a plugin's effect, and no connected overlay shows
    /// that plugin's page (its id): the queue waits for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for_page: Option<String>,
    /// The next item is a run of a plugin with server code, and its code
    /// isn't running (switched off or stopped; its id): the queue waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for_plugin: Option<String>,
}

/// What the panel and the control API can do with the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueAction {
    Pause,
    Resume,
    /// Pauses a running queue, resumes a paused one.
    Toggle,
    /// Ends what's on stage now, its speech too.
    Skip,
    /// Drops everything waiting (not what's on stage), or only one
    /// [`QueueGroup`].
    Clear,
}

impl QueueAction {
    pub const ALL: [QueueAction; 5] =
        [QueueAction::Pause, QueueAction::Resume, QueueAction::Toggle, QueueAction::Skip, QueueAction::Clear];

    /// As in JSON and the control API's paths.
    pub fn name(self) -> &'static str {
        match self {
            QueueAction::Pause => "pause",
            QueueAction::Resume => "resume",
            QueueAction::Toggle => "toggle",
            QueueAction::Skip => "skip",
            QueueAction::Clear => "clear",
        }
    }

    pub fn parse(name: &str) -> Option<QueueAction> {
        QueueAction::ALL.into_iter().find(|action| action.name() == name)
    }
}

/// What skipping one item by its id did (the panel's ×, the control API's
/// `/control/items/{id}/skip`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Skipped {
    /// It was on stage: it ended.
    OnStage,
    /// It was waiting: it was dropped.
    Waiting,
    /// It wasn't in the queue any more: nothing changed.
    Gone,
}

/// What moving one item to play next did (the panel's ⤒, the control
/// API's `/control/items/{id}/next`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Moved {
    /// It was playing already: nothing changed.
    OnStage,
    /// It was waiting: it plays next, once what's on stage ends.
    Waiting,
    /// It wasn't in the queue any more: nothing changed.
    Gone,
}

/// An event that mattered on stream, and what it caused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Unique, kept across restarts.
    pub id: u64,
    /// Unix ms.
    pub at_ms: u64,
    pub kind: EventKind,
    /// "TestViewer cheered 500 bits".
    pub summary: String,
    /// Its size, for filters: bits cheered, a raid's viewers, the subs in
    /// a gift bomb, a resub's months; 0 when it has none.
    #[serde(default)]
    pub amount: u64,
    /// What the viewer wrote with it, if anything.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// The message with its emotes as images; empty when it has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub message_parts: Vec<AlertPart>,
    /// Set when this is a replay of another entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_of: Option<u64>,
    /// The viewers in it who picked a name colour in chat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<NameColor>,
    /// The cards it set off, by name, in the order they fired.
    #[serde(default)]
    pub cards: Vec<String>,
    /// Their ids, in the same order, so the panel links to a card even
    /// after it's renamed. Empty in entries saved before 2026-10-14.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub card_ids: Vec<String>,
    /// What it put in the queue, and what became of it.
    #[serde(default)]
    pub items: Vec<ItemRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Sub,
    GiftBomb,
    Bits,
    Raid,
    Reward,
    Follow,
    /// A hype train started, went up a level or ended (not every
    /// contribution).
    HypeTrain,
    /// Support in another unit (money, from a later platform).
    Support,
}

impl EventKind {
    pub const ALL: [EventKind; 8] = [
        EventKind::Sub,
        EventKind::GiftBomb,
        EventKind::Bits,
        EventKind::Raid,
        EventKind::Reward,
        EventKind::Follow,
        EventKind::HypeTrain,
        EventKind::Support,
    ];

    /// For filters: "Subs", "Gift bombs", …
    pub fn plural(self) -> &'static str {
        match self {
            EventKind::Sub => "Subs",
            EventKind::GiftBomb => "Gift bombs",
            EventKind::Bits => "Bits",
            EventKind::Raid => "Raids",
            EventKind::Reward => "Rewards",
            EventKind::Follow => "Follows",
            EventKind::HypeTrain => "Hype trains",
            EventKind::Support => "Support",
        }
    }
}

/// Which events the History shows (`AppConfig::history`), and how. It only
/// changes what's shown: every event is still kept, so loosening it brings
/// older ones back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryFilter {
    pub subs: bool,
    pub gift_bombs: bool,
    /// Gift bombs of at least this many subs.
    pub min_gift_bomb: u64,
    pub bits: bool,
    /// Cheers of at least this many bits.
    pub min_bits: u64,
    pub raids: bool,
    /// Raids of at least this many viewers.
    pub min_raid: u64,
    pub rewards: bool,
    pub follows: bool,
    pub hype_trains: bool,
    pub support: bool,
    /// The viewer's message under each entry, without opening it.
    pub messages: bool,
    /// The "Seen up to here" line (2026-10-14, off by default): the events
    /// above it are new, for thanking viewers.
    pub seen_marker: bool,
    /// Everything counts as seen when the stream starts (the OBS plugin's
    /// Stream started), so the line starts each stream at the top.
    pub seen_at_stream_start: bool,
}

impl Default for HistoryFilter {
    fn default() -> Self {
        HistoryFilter {
            subs: true,
            gift_bombs: true,
            min_gift_bomb: 0,
            bits: true,
            min_bits: 0,
            raids: true,
            min_raid: 0,
            rewards: true,
            follows: true,
            hype_trains: true,
            support: true,
            messages: true,
            seen_marker: false,
            seen_at_stream_start: false,
        }
    }
}

impl HistoryFilter {
    pub fn shows(&self, entry: &HistoryEntry) -> bool {
        match entry.kind {
            EventKind::Sub => self.subs,
            EventKind::GiftBomb => self.gift_bombs && entry.amount >= self.min_gift_bomb,
            EventKind::Bits => self.bits && entry.amount >= self.min_bits,
            EventKind::Raid => self.raids && entry.amount >= self.min_raid,
            EventKind::Reward => self.rewards,
            EventKind::Follow => self.follows,
            EventKind::HypeTrain => self.hype_trains,
            EventKind::Support => self.support,
        }
    }

    /// Whether it hides any events (the messages and the seen marker don't
    /// count).
    pub fn is_default(&self) -> bool {
        let shown =
            HistoryFilter { messages: true, seen_marker: false, seen_at_stream_start: false, ..self.clone() };
        shown == HistoryFilter::default()
    }

    /// Shows every event again; the messages and the seen marker stay as
    /// they are.
    pub fn show_everything(&mut self) {
        *self = HistoryFilter {
            messages: self.messages,
            seen_marker: self.seen_marker,
            seen_at_stream_start: self.seen_at_stream_start,
            ..HistoryFilter::default()
        };
    }

    /// The events it shows above the seen marker (`seen`: the newest entry
    /// id marked seen).
    pub fn new_events<'a>(&self, entries: impl IntoIterator<Item = &'a HistoryEntry>, seen: u64) -> usize {
        entries.into_iter().filter(|entry| entry.is_new(seen) && self.shows(entry)).count()
    }
}

impl HistoryEntry {
    /// Above the seen marker: newer than the last entry marked seen, and
    /// not a replay (the streamer set that off).
    pub fn is_new(&self, seen: u64) -> bool {
        self.id > seen && self.replay_of.is_none()
    }

    /// The addresses of the links in the viewer's message and in what its
    /// queue items showed or said: the only ones the panel may open
    /// (`PanelRequest::OpenLink`).
    pub fn links(&self) -> Vec<String> {
        let message_parts = self.message_parts.iter().filter_map(|part| match part {
            AlertPart::Text { text, .. } => Some(text.as_str()),
            AlertPart::Emote { .. } => None,
        });
        let items = self.items.iter().flat_map(|item| {
            let parts = item.parts.iter().filter_map(|part| match part {
                TextPart::Text { text, .. } => Some(text.as_str()),
                TextPart::Emote { .. } => None,
            });
            std::iter::once(item.text.as_str()).chain(parts)
        });
        std::iter::once(self.message.as_str())
            .chain(message_parts)
            .chain(items)
            .flat_map(crate::links::find)
            .map(|link| link.url)
            .collect()
    }
}

/// A queue item an event made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemRecord {
    pub id: u64,
    /// As [`QueueItem::label`].
    pub label: String,
    pub text: String,
    /// `text` with its emotes and text styles, as it looked (2026-10-14);
    /// empty when it had none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<TextPart>,
    pub state: ItemState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    Waiting,
    OnStage,
    Played,
    Skipped,
    /// Dropped with Clear.
    Cleared,
    /// Its speech couldn't be made.
    Failed,
    /// Left out by a mute that skipped (only an earlier version did; kept
    /// so its history still reads).
    Muted,
    /// Its event prepared again after the cards changed no longer queues
    /// it (its card switched off, say). Until the queue has played past
    /// its place, a change that queues it again brings it back.
    Withdrawn,
}

impl ItemState {
    pub fn label(self) -> &'static str {
        match self {
            ItemState::Waiting => "waiting",
            ItemState::OnStage => "on stage",
            ItemState::Played => "played",
            ItemState::Skipped => "skipped",
            ItemState::Cleared => "cleared",
            ItemState::Failed => "failed",
            ItemState::Muted => "muted",
            ItemState::Withdrawn => "withdrawn",
        }
    }
}

/// What preparing the waiting events again changed in the queue, after the
/// cards changed: items that now play differently, joined, were withdrawn,
/// or came back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prepared {
    pub updated: usize,
    pub added: usize,
    pub withdrawn: usize,
    pub back: usize,
}

impl Prepared {
    /// Whether anything changed.
    pub fn changed(&self) -> bool {
        *self != Prepared::default()
    }

    /// For people: "Your change updated 7 waiting items and took out 2."
    pub fn note(&self) -> String {
        let items = |count: usize| {
            if count == 1 { "1 waiting item".to_string() } else { format!("{count} waiting items") }
        };
        let mut parts = Vec::new();
        if self.updated > 0 {
            parts.push(format!("updated {}", items(self.updated)));
        }
        if self.added > 0 {
            parts.push(format!("added {}", items(self.added)));
        }
        if self.withdrawn > 0 {
            parts.push(format!("took out {}", self.withdrawn));
        }
        if self.back > 0 {
            parts.push(format!("brought back {}", self.back));
        }
        let parts = match parts.split_last() {
            None => return String::new(),
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        };
        format!("Your change {parts}.")
    }
}

/// For `retain` over a history, newest first: keeps the newest
/// [`HISTORY_LIMIT`] of each kind.
pub fn within_limit<T>(kind: impl Fn(&T) -> EventKind) -> impl FnMut(&T) -> bool {
    let mut counts = std::collections::HashMap::new();
    move |entry| {
        let count = counts.entry(kind(entry)).or_insert(0);
        *count += 1;
        *count <= HISTORY_LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_preparing_again_changed_reads_as_one_sentence() {
        let note = |updated, added, withdrawn, back| Prepared { updated, added, withdrawn, back }.note();
        assert_eq!(note(0, 0, 0, 0), "");
        assert_eq!(note(1, 0, 0, 0), "Your change updated 1 waiting item.");
        assert_eq!(note(7, 0, 2, 0), "Your change updated 7 waiting items and took out 2.");
        assert_eq!(
            note(1, 2, 3, 4),
            "Your change updated 1 waiting item, added 2 waiting items, took out 3 and brought back 4."
        );
    }

    #[test]
    fn hiding_messages_hides_no_events() {
        let mut filter = HistoryFilter { messages: false, seen_marker: true, ..HistoryFilter::default() };
        assert!(filter.is_default());
        filter.min_bits = 100;
        assert!(!filter.is_default());
        filter.show_everything();
        assert!(filter.is_default());
        assert!(!filter.messages, "Show everything leaves the messages as they are");
        assert!(filter.seen_marker, "and the seen marker");
    }

    #[test]
    fn new_events_are_above_the_marker_and_shown() {
        let entry = |id, kind, replay_of| HistoryEntry {
            id,
            at_ms: 0,
            kind,
            summary: String::new(),
            amount: 0,
            message: String::new(),
            message_parts: Vec::new(),
            replay_of,
            names: Vec::new(),
            cards: Vec::new(),
            card_ids: Vec::new(),
            items: Vec::new(),
        };
        let entries = [
            entry(5, EventKind::Follow, None),
            entry(4, EventKind::Sub, Some(1)),
            entry(3, EventKind::Sub, None),
            entry(2, EventKind::Sub, None),
        ];
        let filter = HistoryFilter::default();
        assert_eq!(filter.new_events(&entries, 2), 2, "a replay isn't new");
        assert_eq!(filter.new_events(&entries, 5), 0);
        let no_follows = HistoryFilter { follows: false, ..filter };
        assert_eq!(no_follows.new_events(&entries, 2), 1, "a hidden one isn't counted");
    }

    #[test]
    fn each_kind_keeps_its_newest() {
        let mut entries: Vec<EventKind> =
            std::iter::repeat_n(EventKind::Follow, HISTORY_LIMIT + 5).chain([EventKind::Sub]).collect();
        entries.retain(within_limit(|kind: &EventKind| *kind));
        assert_eq!(entries.len(), HISTORY_LIMIT + 1, "the old sub stays");
        assert_eq!(entries.last(), Some(&EventKind::Sub));
    }

    #[test]
    fn shapes() {
        let action = serde_json::to_value(QueueAction::Skip).unwrap();
        assert_eq!(action, "skip");
        let entry = HistoryEntry {
            id: 3,
            at_ms: 1,
            kind: EventKind::GiftBomb,
            summary: "A gifted 5 subs".into(),
            amount: 5,
            message: String::new(),
            message_parts: Vec::new(),
            replay_of: None,
            names: Vec::new(),
            cards: vec!["Gift bomb alert".into()],
            card_ids: vec!["gift-bomb-alert".into()],
            items: vec![ItemRecord {
                id: 9,
                label: "Alert".into(),
                text: "x".into(),
                parts: Vec::new(),
                state: ItemState::OnStage,
            }],
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["kind"], "gift_bomb");
        assert_eq!(value["items"][0]["state"], "on_stage");
        assert_eq!(value["card_ids"][0], "gift-bomb-alert");
        assert!(value.get("message").is_none());
        assert_eq!(serde_json::from_value::<HistoryEntry>(value).unwrap(), entry);
    }

    #[test]
    fn filters_hide_kinds_and_small_ones() {
        let entry = |kind, amount| HistoryEntry {
            id: 1,
            at_ms: 0,
            kind,
            summary: String::new(),
            amount,
            message: String::new(),
            message_parts: Vec::new(),
            replay_of: None,
            names: Vec::new(),
            cards: Vec::new(),
            card_ids: Vec::new(),
            items: Vec::new(),
        };
        let filter = HistoryFilter { min_raid: 10, rewards: false, ..HistoryFilter::default() };
        assert!(!filter.shows(&entry(EventKind::Raid, 3)), "a small raid");
        assert!(filter.shows(&entry(EventKind::Raid, 10)));
        assert!(!filter.shows(&entry(EventKind::Reward, 0)));
        assert!(filter.shows(&entry(EventKind::Follow, 0)));
        assert!(!HistoryFilter { follows: false, ..filter.clone() }.shows(&entry(EventKind::Follow, 0)));
        assert!(filter.shows(&entry(EventKind::Bits, 1)));
        assert!(HistoryFilter::default().is_default() && !filter.is_default());
        let old: HistoryFilter = serde_json::from_value(serde_json::json!({ "raids": false })).unwrap();
        assert!(!old.raids && old.subs, "missing settings show");
    }

    #[test]
    fn an_entrys_links_are_in_its_message_and_items() {
        let item = |text: &str, parts| ItemRecord {
            id: 1,
            label: "Alert".into(),
            text: text.into(),
            parts,
            state: ItemState::Played,
        };
        let entry = HistoryEntry {
            id: 1,
            at_ms: 0,
            kind: EventKind::Bits,
            summary: "Talker cheered 100 bits".into(),
            amount: 100,
            message: "see twitch.tv/talker!".into(),
            message_parts: vec![AlertPart::text("see twitch.tv/talker!")],
            replay_of: None,
            names: Vec::new(),
            cards: Vec::new(),
            card_ids: Vec::new(),
            items: vec![
                item("Thanks! see twitch.tv/talker!", Vec::new()),
                item("x", vec![TextPart::Text { text: "https://example.com/a".into(), style: None }]),
            ],
        };
        let links = entry.links();
        assert!(links.contains(&"https://twitch.tv/talker".to_string()), "{links:?}");
        assert!(links.contains(&"https://example.com/a".to_string()), "{links:?}");
        assert!(!links.iter().any(|link| link.contains("Talker")), "not in the summary: {links:?}");
    }
}
