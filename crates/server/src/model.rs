//! The platform-neutral data model.
//!
//! Nothing in here may contain platform-specific fields or raw protocol data
//! (no IRC tags, no Twitch-only enums). Platforms and emote sources are plain
//! strings, so adding one never changes these types. Providers translate
//! their platform into this model; everything downstream only sees this.

use std::collections::BTreeMap;

use crate::emote_text::EmoteText;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A platform's own identity for a channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChannelRef {
    pub id: String,
    pub name: String,
}

/// Where something came from. Every [`Input`] and [`Signal`] carries one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// Provider instance, e.g. `"twitch_chat"`.
    pub provider: String,
    /// `"twitch"`, `"youtube"`, …
    pub platform: String,
    pub channel: ChannelRef,
}

/// The common subset of chat roles across platforms.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Roles {
    pub broadcaster: bool,
    pub moderator: bool,
    pub vip: bool,
    pub member: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// Platform-scoped id.
    pub id: String,
    /// Login / handle.
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub roles: Roles,
    /// The name's colour in chat, `#rrggbb`, when the viewer picked one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// The badges shown next to the name, in the platform's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<Badge>,
}

/// A chat badge, as the platform names it: a set (`"subscriber"`,
/// `"moderator"`, …) and a version within it (`"12"`). Images and titles
/// come from the badge registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Badge {
    pub set: String,
    pub version: String,
}

/// Provider → transformers: what happened, parsed and normalised, not
/// interpreted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Input {
    /// Unix time in milliseconds.
    pub at_ms: u64,
    pub origin: Origin,
    pub kind: InputKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "input", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // a few hundred bytes per chat line is fine
pub enum InputKind {
    /// The provider is now receiving this channel.
    ChannelJoined,
    Message(Message),
    Event(PlatformEvent),
    /// Moderators took something out of the chat.
    Moderation(Moderation),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// The platform's id for this message (for taking it off the chat
    /// display when it's deleted); empty when there's none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub user: User,
    pub text: String,
    /// Emotes the platform itself identified in `text`.
    #[serde(default)]
    pub emotes: Vec<EmoteSpan>,
    #[serde(default, skip_serializing_if = "MessageMarks::is_plain")]
    pub marks: MessageMarks,
}

/// What sets a chat message apart from an ordinary one. All off for most.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MessageMarks {
    /// Written as an action (`/me waves`).
    pub action: bool,
    /// Paid to stand out (Twitch's "Highlight my message").
    pub highlighted: bool,
    /// The viewer's first message in this channel.
    pub first: bool,
    /// Sent with a channel point reward.
    pub reward: bool,
    /// Came with a cheer (bits).
    pub cheer: bool,
    /// The message it answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ReplyTo>,
}

impl MessageMarks {
    pub fn is_plain(&self) -> bool {
        self == &MessageMarks::default()
    }
}

/// The message a reply answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyTo {
    /// Who wrote it (display name).
    pub user: String,
    pub text: String,
}

/// Something moderators took out of the chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Moderation {
    /// One message deleted (its [`Message::id`]).
    MessageDeleted { id: String },
    /// A viewer banned or timed out: their messages go.
    UserCleared { user_id: String },
    /// The whole chat cleared.
    ChatCleared,
}

/// An emote the platform identified in a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmoteSpan {
    /// Character (not byte) index of the first character.
    pub start: usize,
    /// Character index one past the last character.
    pub end: usize,
    pub emote: EmoteRef,
}

/// Things platforms report directly. Shared concepts get one variant used by
/// every provider; a provider with something unique gets a new variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PlatformEvent {
    Support {
        user: User,
        amount: Amount,
        message: Option<Message>,
    },
    Membership {
        user: User,
        kind: MembershipKind,
        tier: Option<String>,
        message: Option<Message>,
    },
    GiftBatch {
        gifter: Option<User>,
        count: u32,
        tier: Option<String>,
        batch_id: String,
    },
    Raid {
        from: User,
        viewers: u32,
    },
    Redemption {
        user: User,
        reward: RewardRef,
        input: Option<String>,
    },
    /// Someone followed the channel.
    Follow {
        user: User,
    },
    /// Twitch's hype train: viewers' bits and subs fill it up, level by
    /// level, until it runs out of time.
    HypeTrain {
        change: HypeTrainChange,
        train: HypeTrain,
    },
    /// Ad-hoc triggers that don't deserve a type of their own.
    Custom {
        name: String,
        #[serde(default)]
        data: Map<String, Value>,
    },
    /// A plugin's event node fired, from a program on the control API:
    /// `values` by output name, as sent. `output` is the event output it
    /// fired when not `event`; `to` the one placed copy of the node it
    /// fires (`"<card id>/<node id>"`, server code's `emit-to`), none for
    /// every copy.
    Plugin {
        plugin: String,
        node: String,
        #[serde(default)]
        values: Map<String, Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
    },
}

/// What happened to a hype train.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HypeTrainChange {
    Started,
    /// Someone contributed; `level_up` when that took it up a level.
    Progressed {
        level_up: bool,
    },
    Ended,
}

/// A hype train as it was when something happened to it. Points are the
/// platform's (on Twitch a bit is one, a sub 500, 1,000 or 2,500 by tier).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HypeTrain {
    pub id: String,
    pub level: u32,
    /// Points towards the next level; 0 once it has ended.
    pub progress: u64,
    /// The points the next level needs; 0 once it has ended.
    pub goal: u64,
    /// Points in the whole train.
    pub total: u64,
    /// The platform's kind of train, as it names it (Twitch: `regular`,
    /// `golden_kappa`, `treasure`), like a sub's `tier`.
    pub kind: String,
    /// Shared with other channels (Twitch's Shared Chat).
    pub shared: bool,
    /// How long it had left when this happened; 0 once it has ended.
    pub seconds_left: u32,
}

impl HypeTrain {
    /// "Hype train", or the special kind's own name ("Golden Kappa train").
    pub fn name(&self) -> &'static str {
        match self.kind.as_str() {
            "golden_kappa" => "Golden Kappa train",
            "treasure" => "Treasure train",
            _ => "Hype train",
        }
    }

    /// The name inside a sentence: "a hype train started", "a Golden Kappa
    /// train started".
    pub fn name_in_sentence(&self) -> String {
        match self.name() {
            "Hype train" => "hype train".to_string(),
            name => name.to_string(),
        }
    }

    /// What happened, as a sentence: "A hype train started", "The hype
    /// train reached level 3".
    pub fn summary(&self, change: HypeTrainChange) -> String {
        let name = self.name_in_sentence();
        match change {
            HypeTrainChange::Started => format!("A {name} started"),
            HypeTrainChange::Progressed { level_up: true } => {
                format!("The {name} reached level {}", self.level)
            }
            HypeTrainChange::Progressed { level_up: false } => {
                format!("The {name} is at level {}: {} of {} points", self.level, self.progress, self.goal)
            }
            HypeTrainChange::Ended => format!("The {name} ended at level {}", self.level),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MembershipKind {
    New,
    Renewal {
        months: u32,
    },
    Gifted {
        /// `None` for anonymous gifts.
        gifter: Option<User>,
        /// Set when this gift is part of a [`PlatformEvent::GiftBatch`].
        batch_id: Option<String>,
    },
    /// A sub that cost the viewer nothing becomes one they pay for.
    Upgraded {
        from: UpgradedFrom,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UpgradedFrom {
    Prime,
    /// They continue a gifted sub; `gifter` is `None` when it was
    /// anonymous.
    Gift {
        gifter: Option<User>,
    },
}

/// An amount of support. Amounts are only ever compared within one unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Amount {
    /// E.g. 500 `"bits"`.
    Units { value: u64, unit: String },
    /// E.g. 500 `"USD"` = $5.00.
    Money { minor: u64, currency: String },
}

impl Amount {
    pub fn value(&self) -> u64 {
        match self {
            Amount::Units { value, .. } => *value,
            Amount::Money { minor, .. } => *minor,
        }
    }

    pub fn unit(&self) -> &str {
        match self {
            Amount::Units { unit, .. } => unit,
            Amount::Money { currency, .. } => currency,
        }
    }

    /// Human-readable, e.g. "500 bits", "1 bit", "$5.00", "5.00 SEK".
    pub fn display(&self) -> String {
        match self {
            Amount::Units { value, unit } => {
                let unit = if *value == 1 { unit.strip_suffix('s').unwrap_or(unit) } else { unit };
                format!("{value} {unit}")
            }
            Amount::Money { minor, currency } => {
                let amount = format!("{}.{:02}", minor / 100, minor % 100);
                match currency.to_ascii_uppercase().as_str() {
                    "USD" => format!("${amount}"),
                    "EUR" => format!("€{amount}"),
                    "GBP" => format!("£{amount}"),
                    other => format!("{amount} {other}"),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardRef {
    pub id: String,
    pub title: Option<String>,
}

/// An emote, identified by source + id (both plain strings).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EmoteRef {
    /// `"twitch"`, `"7tv"`, `"bttv"`, `"ffz"`, …
    pub source: String,
    pub id: String,
    /// The text that shows the emote in chat.
    pub code: String,
}

/// An emote as posted, with any zero-width emotes stacked on top of it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PostedEmote {
    pub emote: EmoteRef,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<EmoteRef>,
}

/// Transformer → cards (and other transformers): something meaningful
/// happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    pub origin: Origin,
    /// The transformer instance that emitted it. Filled in by the pipeline.
    pub transformer: String,
    pub kind: SignalKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum SignalKind {
    EmotesPosted {
        user: User,
        emotes: Vec<PostedEmote>,
    },
    /// Every chat message, for cards that react to text.
    ChatMessage {
        user: User,
        text: EmoteText,
        /// The viewer's first message in the channel (`MessageMarks::first`).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        first: bool,
    },
    Support {
        user: User,
        amount: Amount,
        message: Option<EmoteText>,
    },
    Membership {
        user: User,
        kind: MembershipKind,
        tier: Option<String>,
        message: Option<EmoteText>,
    },
    GiftBatch {
        gifter: Option<User>,
        count: u32,
        tier: Option<String>,
    },
    Raid {
        from: User,
        viewers: u32,
    },
    RewardRedeemed {
        user: User,
        reward: RewardRef,
        input: Option<EmoteText>,
    },
    Follow {
        user: User,
    },
    HypeTrain {
        change: HypeTrainChange,
        train: HypeTrain,
    },
    Custom {
        name: String,
        #[serde(default)]
        data: Map<String, Value>,
    },
    /// For the plugin's event node `node`; its outputs' values by name.
    /// `output`: the event output it fires, when not `event`; `to`: the
    /// one placed copy it fires (`"<card id>/<node id>"`), none for every
    /// copy.
    Plugin {
        plugin: String,
        node: String,
        values: BTreeMap<String, PluginValue>,
        output: Option<String>,
        to: Option<String>,
    },
}

/// A value a program sent with a plugin event: a number, a text (with the
/// emotes in it), or emotes from a list of codes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginValue {
    Number(f64),
    Text(EmoteText),
    Emotes(Vec<PostedEmote>),
}

/// A field-less twin of [`SignalKind`], used by transformers to declare what
/// they produce and consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalName {
    EmotesPosted,
    ChatMessage,
    Support,
    Membership,
    GiftBatch,
    Raid,
    RewardRedeemed,
    Follow,
    HypeTrain,
    Custom,
    Plugin,
}

impl SignalKind {
    pub fn name(&self) -> SignalName {
        match self {
            SignalKind::EmotesPosted { .. } => SignalName::EmotesPosted,
            SignalKind::ChatMessage { .. } => SignalName::ChatMessage,
            SignalKind::Support { .. } => SignalName::Support,
            SignalKind::Membership { .. } => SignalName::Membership,
            SignalKind::GiftBatch { .. } => SignalName::GiftBatch,
            SignalKind::Raid { .. } => SignalName::Raid,
            SignalKind::RewardRedeemed { .. } => SignalName::RewardRedeemed,
            SignalKind::Follow { .. } => SignalName::Follow,
            SignalKind::HypeTrain { .. } => SignalName::HypeTrain,
            SignalKind::Custom { .. } => SignalName::Custom,
            SignalKind::Plugin { .. } => SignalName::Plugin,
        }
    }
}

impl Signal {
    /// A signal for a transformer to emit; the pipeline fills in `transformer`.
    pub fn new(origin: Origin, kind: SignalKind) -> Signal {
        Signal { origin, transformer: String::new(), kind }
    }

    /// The one person this signal is about, if there is exactly one.
    pub fn user(&self) -> Option<&User> {
        match &self.kind {
            SignalKind::EmotesPosted { user, .. }
            | SignalKind::ChatMessage { user, .. }
            | SignalKind::Support { user, .. }
            | SignalKind::Membership { user, .. }
            | SignalKind::RewardRedeemed { user, .. }
            | SignalKind::Follow { user } => Some(user),
            SignalKind::Raid { from, .. } => Some(from),
            SignalKind::GiftBatch { gifter, .. } => gifter.as_ref(),
            SignalKind::HypeTrain { .. } | SignalKind::Custom { .. } | SignalKind::Plugin { .. } => None,
        }
    }

    /// Everyone this signal names: its viewer, and a gifter.
    pub fn people(&self) -> Vec<&User> {
        let gifter = match &self.kind {
            SignalKind::Membership { kind: MembershipKind::Gifted { gifter, .. }, .. }
            | SignalKind::Membership {
                kind: MembershipKind::Upgraded { from: UpgradedFrom::Gift { gifter } },
                ..
            } => gifter.as_ref(),
            _ => None,
        };
        self.user().into_iter().chain(gifter).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_display() {
        assert_eq!(Amount::Units { value: 500, unit: "bits".into() }.display(), "500 bits");
        assert_eq!(Amount::Units { value: 1, unit: "bits".into() }.display(), "1 bit");
        assert_eq!(Amount::Money { minor: 500, currency: "USD".into() }.display(), "$5.00");
        assert_eq!(Amount::Money { minor: 1234, currency: "sek".into() }.display(), "12.34 SEK");
    }

    #[test]
    fn input_json_roundtrip() {
        let input = Input {
            at_ms: 1,
            origin: Origin {
                provider: "twitch_chat".into(),
                platform: "twitch".into(),
                channel: ChannelRef { id: "1".into(), name: "chan".into() },
            },
            kind: InputKind::Event(PlatformEvent::Membership {
                user: User {
                    id: "2".into(),
                    name: "a".into(),
                    display_name: "A".into(),
                    roles: Roles::default(),
                    ..Default::default()
                },
                kind: MembershipKind::Renewal { months: 3 },
                tier: Some("1000".into()),
                message: None,
            }),
        };
        let text = serde_json::to_string(&input).unwrap();
        assert_eq!(serde_json::from_str::<Input>(&text).unwrap(), input);
    }
}
