//! Cards: what reacts to what, as small node graphs.
//!
//! A card is a named graph of nodes joined by wires, like a Blender shader:
//! event nodes (a raid, bits, a channel point reward, a combo in chat, …)
//! send events through flow nodes (chance, cooldown) into effect nodes (an alert, emote rain),
//! and value nodes compute the effects' inputs (bits ÷ 100 → rain count).
//!
//! The server runs cards; the control panel edits them. Both build from this
//! module, so sockets, validation, premade cards and presets are defined
//! once.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub use crate::overrides::Overrides;
use crate::overrides::{Field, SPEECH_FIELDS};
use crate::plugin::{Attached, Catalog, ENDED_OUTPUT, NodeType, STARTED_OUTPUT, SettingKind};

pub type NodeId = u32;

/// One graph: a name, an on/off switch, nodes and the wires between them.
/// Cards are self-contained: every card has its own event nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    /// Stable id, unique among cards.
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub wires: Vec<Wire>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Unique within its card.
    pub id: NodeId,
    /// Position on the canvas, in canvas pixels.
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    pub kind: NodeKind,
}

/// A wire from an output socket to an input socket.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Wire {
    pub from: SocketRef,
    pub to: SocketRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SocketRef {
    pub node: NodeId,
    pub socket: String,
}

impl SocketRef {
    pub fn new(node: NodeId, socket: &str) -> SocketRef {
        SocketRef { node, socket: socket.to_string() }
    }
}

/// What travels along a wire. Wires only join sockets of the same type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketType {
    /// "It happened", carrying everything known about it.
    Event,
    Number,
    Text,
    /// A list of emotes (the ones in a chat message).
    Emotes,
}

impl SocketType {
    /// Whether a wire from an output of type `from` may go into an input of
    /// this type: the same type, or a number into text (it's written out).
    pub fn accepts(self, from: SocketType) -> bool {
        self == from || (self == SocketType::Text && from == SocketType::Number)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Events,
    Flow,
    Values,
    Effects,
}

impl Category {
    pub const ALL: [Category; 4] = [Category::Events, Category::Flow, Category::Values, Category::Effects];

    pub fn title(self) -> &'static str {
        match self {
            Category::Events => "Events",
            Category::Flow => "Flow",
            Category::Values => "Values",
            Category::Effects => "Effects",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Socket {
    /// Stable key, used in wires and as the `{name}` in text.
    pub name: String,
    pub label: String,
    pub ty: SocketType,
}

fn socket(name: &str, label: &str, ty: SocketType) -> Socket {
    Socket { name: name.to_string(), label: label.to_string(), ty }
}

/// One way out of a rarity node: taken `percent` % of the time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tier {
    /// Unique within its node; names the output, so wires survive renames.
    pub id: u32,
    pub name: String,
    pub percent: f64,
}

impl Tier {
    /// The output socket's name.
    pub fn socket(&self) -> String {
        format!("tier-{}", self.id)
    }
}

/// The rarity tiers a new rarity node starts with.
pub fn default_tiers() -> Vec<Tier> {
    let tier = |id, name: &str, percent| Tier { id, name: name.into(), percent };
    vec![tier(1, "Legendary", 1.0), tier(2, "Epic", 10.0), tier(3, "Rare", 20.0)]
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MathOp {
    Add,
    Subtract,
    Multiply,
    #[default]
    Divide,
    Min,
    Max,
}

impl MathOp {
    pub const ALL: [MathOp; 6] =
        [MathOp::Add, MathOp::Subtract, MathOp::Multiply, MathOp::Divide, MathOp::Min, MathOp::Max];

    pub fn title(self) -> &'static str {
        match self {
            MathOp::Add => "Add",
            MathOp::Subtract => "Subtract",
            MathOp::Multiply => "Multiply",
            MathOp::Divide => "Divide",
            MathOp::Min => "Smallest",
            MathOp::Max => "Largest",
        }
    }

    /// Division by zero gives zero rather than infinity.
    pub fn apply(self, a: f64, b: f64) -> f64 {
        match self {
            MathOp::Add => a + b,
            MathOp::Subtract => a - b,
            MathOp::Multiply => a * b,
            MathOp::Divide if b == 0.0 => 0.0,
            MathOp::Divide => a / b,
            MathOp::Min => a.min(b),
            MathOp::Max => a.max(b),
        }
    }
}

/// How a Compare text node tests its text against each of its texts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextTest {
    #[default]
    Is,
    Contains,
    /// As whole words, punctuation around them ignored (as in Chat words);
    /// several words in a row for a text of several.
    ContainsWord,
    StartsWith,
    EndsWith,
}

impl TextTest {
    pub const ALL: [TextTest; 5] =
        [TextTest::Is, TextTest::Contains, TextTest::ContainsWord, TextTest::StartsWith, TextTest::EndsWith];

    pub fn title(self) -> &'static str {
        match self {
            TextTest::Is => "Is",
            TextTest::Contains => "Contains",
            TextTest::ContainsWord => "Contains word",
            TextTest::StartsWith => "Starts with",
            TextTest::EndsWith => "Ends with",
        }
    }

    /// Whether `text` passes against `against`. Spaces around either don't
    /// count, and an empty `against` never passes.
    pub fn passes(self, text: &str, against: &str, ignore_case: bool) -> bool {
        let fold =
            |text: &str| if ignore_case { text.trim().to_lowercase() } else { text.trim().to_string() };
        let (text, against) = (fold(text), fold(against));
        if against.is_empty() {
            return false;
        }
        match self {
            TextTest::Is => text == against,
            TextTest::Contains => text.contains(&against),
            TextTest::StartsWith => text.starts_with(&against),
            TextTest::EndsWith => text.ends_with(&against),
            TextTest::ContainsWord => {
                let words = |text: &str| -> Vec<String> {
                    text.split_whitespace()
                        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
                        .filter(|word| !word.is_empty())
                        .collect()
                };
                let (text, against) = (words(&text), words(&against));
                !against.is_empty() && text.windows(against.len()).any(|run| run == against.as_slice())
            }
        }
    }
}

/// How a Compare number node tests its value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumberTest {
    #[default]
    AtLeast,
    AtMost,
    /// From one number to the other, both included, in either order.
    Between,
    Is,
}

impl NumberTest {
    pub const ALL: [NumberTest; 4] =
        [NumberTest::AtLeast, NumberTest::AtMost, NumberTest::Between, NumberTest::Is];

    pub fn title(self) -> &'static str {
        match self {
            NumberTest::AtLeast => "At least",
            NumberTest::AtMost => "At most",
            NumberTest::Between => "Between",
            NumberTest::Is => "Is",
        }
    }

    /// Whether `value` passes against `number` (and, for Between, `and`).
    /// Is allows for rounding (0.1 + 0.2 is 0.3).
    pub fn passes(self, value: f64, number: f64, and: f64) -> bool {
        match self {
            NumberTest::AtLeast => value >= number,
            NumberTest::AtMost => value <= number,
            NumberTest::Between => value >= number.min(and) && value <= number.max(and),
            NumberTest::Is => (value - number).abs() < 1e-9,
        }
    }
}

/// Every kind of node, with its settings. A setting named like an input
/// socket is that input's value while nothing is wired into it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeKind {
    // Events
    EmotesInChat,
    ChatMessage,
    /// A chat message the platform marks as the viewer's first in the
    /// channel.
    FirstChatter,
    ChatWords {
        /// Case-insensitive whole words; punctuation around them is ignored.
        words: Vec<String>,
    },
    /// A chat message that starts with `command` ("!tts", "tts:"; any
    /// case); the rest of the message is its Text.
    ChatCommand {
        command: String,
    },
    /// The same emote from at least `viewers` different viewers within
    /// `seconds`. It fires once the crowd is complete, then counts afresh.
    EmoteCombo {
        viewers: f64,
        seconds: f64,
    },
    /// One of `words` (as in Chat words) from at least `viewers` different
    /// viewers within `seconds`; then it counts afresh.
    Chant {
        words: Vec<String>,
        viewers: f64,
        seconds: f64,
    },
    Bits {
        /// Only cheers of at least this many bits.
        min: f64,
    },
    Sub {
        new: bool,
        resub: bool,
        gifted: bool,
        /// Gifted subs that are part of a gift bomb (which has its own event).
        in_gift_bomb: bool,
        /// A Prime sub switched to a paid one.
        #[serde(default = "yes")]
        prime_to_paid: bool,
        /// A gifted sub the viewer now pays for themselves.
        #[serde(default = "yes")]
        gift_continued: bool,
    },
    GiftBomb,
    /// Someone followed the channel (needs the broadcaster's login).
    Follow,
    /// Twitch's hype train (needs the broadcaster's login): what happening
    /// to it this node fires for. A contribution that takes it up a level
    /// is both progress and a level-up; the node fires once for it.
    HypeTrain {
        started: bool,
        levelled_up: bool,
        progressed: bool,
        ended: bool,
    },
    Raid {
        min_viewers: f64,
    },
    Reward {
        /// The reward's title (or id); empty = any reward.
        reward: String,
    },
    Custom {
        /// The custom event's name; empty = any.
        name: String,
    },
    // Flow
    /// The event goes out of Hit `percent` % of the time, else out of Miss.
    /// Chained through Miss, chances make a rarity table.
    Chance {
        /// 0–100.
        percent: f64,
    },
    Cooldown {
        seconds: f64,
        per_viewer: bool,
    },
    /// An event into Open opens the gate for `seconds` (again: from then);
    /// while it's open, events into Event go through.
    Gate {
        seconds: f64,
    },
    /// Lets the event through when its viewer has one of the ticked roles,
    /// else out of Miss (as do events without a viewer).
    ViewerRoles {
        broadcaster: bool,
        moderator: bool,
        vip: bool,
        subscriber: bool,
    },
    /// Lets the event through when its text (wired in, or this template
    /// filled from the event) has anything but spaces in it, else out of
    /// Empty: a resub with or without a message, a reward with or without
    /// the viewer's text.
    HasText {
        text: String,
    },
    /// Lets the event through (Matches) when its text (wired in, or this
    /// template filled from the event) passes `test` against any of `to`
    /// (with `all`, every one), else out of Miss. A text wired into To
    /// stands in for the list.
    CompareText {
        text: String,
        test: TextTest,
        to: Vec<String>,
        all: bool,
        #[serde(default = "yes")]
        ignore_case: bool,
    },
    /// Lets the event through (Matches) when its Value passes `test`
    /// against Number (and And, for Between), else out of Miss. Each is
    /// wired in or this setting.
    CompareNumber {
        test: NumberTest,
        value: f64,
        number: f64,
        #[serde(default)]
        and: f64,
    },
    /// Sends the event on `seconds` later (wired in, or this), with the
    /// values it had. Waiting events live with the running card.
    Delay {
        seconds: f64,
    },
    /// Counts the events in; every `every`th goes out of Nth, the others
    /// out of Miss. An event into Reset starts the count over.
    EveryNth {
        every: f64,
    },
    /// While running, an event out of Tick every `minutes`, the first one
    /// `minutes` after it starts. Start (again: from then) and Stop take
    /// events; with `from_start` it runs as the card starts.
    Timer {
        minutes: f64,
        #[serde(default = "yes")]
        from_start: bool,
    },
    /// The event goes out of exactly one tier, picked by the tiers' chances;
    /// whatever chance is left goes out of `rest` (e.g. "Common").
    Rarity {
        tiers: Vec<Tier>,
        rest: String,
    },
    // Values
    Number {
        value: f64,
    },
    Math {
        op: MathOp,
        a: f64,
        b: f64,
    },
    Clamp {
        value: f64,
        min: f64,
        max: f64,
    },
    Random {
        min: f64,
        max: f64,
    },
    Text {
        /// `{name}` placeholders become inputs.
        template: String,
        /// Resized on the canvas: the node's width and its text box's
        /// height, in canvas pixels; none = the default size.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<[f64; 2]>,
    },
    /// The emotes in a text (a cheer's message, a reward's text), found by
    /// their codes as in chat.
    EmotesInText,
    /// A node type from a plugin (see `crate::plugin`): its title, sockets
    /// and fields come from the type attached after loading.
    Plugin {
        /// The plugin's id.
        plugin: String,
        /// The node type's id in the plugin.
        node: String,
        /// By key; a missing or wrong one is the setting's default.
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        settings: serde_json::Map<String, serde_json::Value>,
        /// The plugin's own settings this node changes for itself (its
        /// type's `changes`), by key.
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        changes: serde_json::Map<String, serde_json::Value>,
        /// Changes to the text to speech settings for the input it reads
        /// aloud (`overrides::SPEECH_FIELDS`).
        #[serde(default, skip_serializing_if = "Overrides::is_empty")]
        voice: Overrides,
        #[serde(skip)]
        ty: Attached,
    },
}

impl NodeKind {
    /// One of each kind, with sensible settings: the add-node menu.
    pub fn catalog() -> Vec<NodeKind> {
        vec![
            NodeKind::EmotesInChat,
            NodeKind::ChatMessage,
            NodeKind::FirstChatter,
            NodeKind::ChatWords { words: vec!["hype".into()] },
            NodeKind::ChatCommand { command: "!tts".into() },
            NodeKind::EmoteCombo { viewers: 5.0, seconds: 10.0 },
            NodeKind::Chant { words: vec!["hype".into()], viewers: 5.0, seconds: 20.0 },
            NodeKind::Bits { min: 0.0 },
            NodeKind::Sub {
                new: true,
                resub: true,
                gifted: true,
                in_gift_bomb: false,
                prime_to_paid: true,
                gift_continued: true,
            },
            NodeKind::GiftBomb,
            NodeKind::Follow,
            NodeKind::HypeTrain { started: true, levelled_up: true, progressed: false, ended: true },
            NodeKind::Raid { min_viewers: 0.0 },
            NodeKind::Reward { reward: String::new() },
            NodeKind::Custom { name: String::new() },
            NodeKind::Chance { percent: 50.0 },
            NodeKind::Cooldown { seconds: 30.0, per_viewer: false },
            NodeKind::Gate { seconds: 60.0 },
            NodeKind::Delay { seconds: 5.0 },
            NodeKind::EveryNth { every: 10.0 },
            NodeKind::Timer { minutes: 15.0, from_start: true },
            NodeKind::ViewerRoles { broadcaster: true, moderator: true, vip: false, subscriber: false },
            NodeKind::HasText { text: "{message}".into() },
            NodeKind::CompareText {
                text: "{message}".into(),
                test: TextTest::Is,
                to: vec![String::new()],
                all: false,
                ignore_case: true,
            },
            NodeKind::CompareNumber { test: NumberTest::AtLeast, value: 0.0, number: 100.0, and: 500.0 },
            NodeKind::Rarity { tiers: default_tiers(), rest: "Common".into() },
            NodeKind::Number { value: 1.0 },
            NodeKind::Math { op: MathOp::Divide, a: 0.0, b: 100.0 },
            NodeKind::Clamp { value: 0.0, min: 1.0, max: 50.0 },
            NodeKind::Random { min: 1.0, max: 10.0 },
            NodeKind::text("{viewer}"),
            NodeKind::EmotesInText,
        ]
    }

    pub fn title(&self) -> &str {
        match self {
            NodeKind::Plugin { node, ty, .. } => ty.0.as_ref().map_or(node, |ty| &ty.title),
            NodeKind::EmotesInChat => "Emotes in chat",
            NodeKind::ChatMessage => "Chat message",
            NodeKind::FirstChatter => "First-time chatter",
            NodeKind::ChatWords { .. } => "Chat words",
            NodeKind::ChatCommand { .. } => "Chat command",
            NodeKind::EmoteCombo { .. } => "Emote combo",
            NodeKind::Chant { .. } => "Chant",
            NodeKind::Bits { .. } => "Bits",
            NodeKind::Sub { .. } => "Sub",
            NodeKind::GiftBomb => "Gift bomb",
            NodeKind::Follow => "Follow",
            NodeKind::HypeTrain { .. } => "Hype train",
            NodeKind::Raid { .. } => "Raid",
            NodeKind::Reward { .. } => "Channel point reward",
            NodeKind::Custom { .. } => "Custom event",
            NodeKind::Chance { .. } => "Chance",
            NodeKind::Cooldown { .. } => "Cooldown",
            NodeKind::Rarity { .. } => "Rarity",
            NodeKind::Gate { .. } => "Gate",
            NodeKind::ViewerRoles { .. } => "Viewer roles",
            NodeKind::HasText { .. } => "Has text",
            NodeKind::CompareText { .. } => "Compare text",
            NodeKind::CompareNumber { .. } => "Compare number",
            NodeKind::Delay { .. } => "Delay",
            NodeKind::EveryNth { .. } => "Every Nth",
            NodeKind::Timer { .. } => "Timer",
            NodeKind::Number { .. } => "Number",
            NodeKind::Math { .. } => "Math",
            NodeKind::Clamp { .. } => "Clamp",
            NodeKind::Random { .. } => "Random number",
            NodeKind::Text { .. } => "Text",
            NodeKind::EmotesInText => "Emotes in text",
        }
    }

    pub fn category(&self) -> Category {
        match self {
            NodeKind::EmotesInChat
            | NodeKind::ChatMessage
            | NodeKind::FirstChatter
            | NodeKind::ChatWords { .. }
            | NodeKind::ChatCommand { .. }
            | NodeKind::EmoteCombo { .. }
            | NodeKind::Chant { .. }
            | NodeKind::Bits { .. }
            | NodeKind::Sub { .. }
            | NodeKind::GiftBomb
            | NodeKind::Follow
            | NodeKind::HypeTrain { .. }
            | NodeKind::Raid { .. }
            | NodeKind::Reward { .. }
            | NodeKind::Custom { .. } => Category::Events,
            NodeKind::Chance { .. }
            | NodeKind::Cooldown { .. }
            | NodeKind::Rarity { .. }
            | NodeKind::Gate { .. }
            | NodeKind::ViewerRoles { .. }
            | NodeKind::HasText { .. }
            | NodeKind::CompareText { .. }
            | NodeKind::CompareNumber { .. }
            | NodeKind::Delay { .. }
            | NodeKind::EveryNth { .. }
            | NodeKind::Timer { .. } => Category::Flow,
            NodeKind::Number { .. }
            | NodeKind::Math { .. }
            | NodeKind::Clamp { .. }
            | NodeKind::Random { .. }
            | NodeKind::Text { .. }
            | NodeKind::EmotesInText => Category::Values,
            // A missing plugin's node: plugins only bring effects so far.
            NodeKind::Plugin { ty, .. } => ty.0.as_ref().map_or(Category::Effects, |ty| ty.category),
        }
    }

    /// A plugin node's type, when its plugin is installed.
    pub fn plugin_type(&self) -> Option<&NodeType> {
        match self {
            NodeKind::Plugin { ty, .. } => ty.0.as_deref(),
            _ => None,
        }
    }

    /// A plugin node whose plugin isn't installed (or no longer has it).
    pub fn is_missing_plugin(&self) -> bool {
        matches!(self, NodeKind::Plugin { ty: Attached(None), .. })
    }

    pub fn is_event(&self) -> bool {
        self.category() == Category::Events
    }

    /// The platform whose own feature this node is about (`"twitch"`; the
    /// built-in OBS plugin's nodes `"obs"`), for its icon; `None` for what
    /// any platform with a chat has.
    pub fn platform(&self) -> Option<&'static str> {
        match self {
            NodeKind::Bits { .. }
            | NodeKind::Sub { .. }
            | NodeKind::GiftBomb
            | NodeKind::Follow
            | NodeKind::HypeTrain { .. }
            | NodeKind::Raid { .. }
            | NodeKind::Reward { .. } => Some("twitch"),
            NodeKind::Plugin { plugin, .. } if plugin == crate::plugin::OBS_PLUGIN => Some("obs"),
            _ => None,
        }
    }

    pub fn inputs(&self) -> Vec<Socket> {
        use SocketType::*;
        match self {
            NodeKind::Chance { .. }
            | NodeKind::Cooldown { .. }
            | NodeKind::Rarity { .. }
            | NodeKind::ViewerRoles { .. } => vec![socket("event", "Event", Event)],
            // Event first: it shares a row with the output of that name.
            NodeKind::Gate { .. } => vec![socket("event", "Event", Event), socket("open", "Open", Event)],
            NodeKind::HasText { .. } => vec![socket("event", "Event", Event), socket("text", "Text", Text)],
            NodeKind::CompareText { .. } => {
                vec![socket("event", "Event", Event), socket("text", "Text", Text), socket("to", "To", Text)]
            }
            NodeKind::CompareNumber { test, .. } => {
                let mut inputs = vec![
                    socket("event", "Event", Event),
                    socket("value", "Value", Number),
                    socket("number", test.title(), Number),
                ];
                if *test == NumberTest::Between {
                    inputs.push(socket("and", "And", Number));
                }
                inputs
            }
            NodeKind::Delay { .. } => {
                vec![socket("event", "Event", Event), socket("seconds", "Seconds", Number)]
            }
            NodeKind::EveryNth { .. } => {
                vec![socket("event", "Event", Event), socket("reset", "Reset", Event)]
            }
            NodeKind::Timer { .. } => vec![socket("start", "Start", Event), socket("stop", "Stop", Event)],
            NodeKind::Math { .. } => vec![socket("a", "Value", Number), socket("b", "By", Number)],
            NodeKind::Clamp { .. } => {
                vec![
                    socket("value", "Value", Number),
                    socket("min", "Min", Number),
                    socket("max", "Max", Number),
                ]
            }
            NodeKind::Random { .. } => vec![socket("min", "Min", Number), socket("max", "Max", Number)],
            NodeKind::Text { template, .. } => {
                placeholders(template).into_iter().map(|name| socket(&name, &name, Text)).collect()
            }
            NodeKind::EmotesInText => vec![socket("text", "Text", Text)],
            NodeKind::Plugin { ty, .. } => ty.0.as_ref().map(|ty| ty.inputs()).unwrap_or_default(),
            _ => Vec::new(), // event nodes and plain numbers have no inputs
        }
    }

    pub fn outputs(&self) -> Vec<Socket> {
        use SocketType::*;
        let event = || socket("event", "Event", Event);
        let summary = || socket("summary", "Summary", Text);
        match self {
            NodeKind::EmotesInChat => vec![
                event(),
                socket("emotes", "Emotes", Emotes),
                socket("viewer", "Viewer", Text),
                socket("count", "Count", Number),
                summary(),
            ],
            NodeKind::ChatMessage | NodeKind::FirstChatter => {
                vec![event(), socket("viewer", "Viewer", Text), socket("message", "Message", Text), summary()]
            }
            NodeKind::ChatWords { .. } => vec![
                event(),
                socket("viewer", "Viewer", Text),
                socket("word", "Word", Text),
                socket("message", "Message", Text),
                summary(),
            ],
            NodeKind::EmoteCombo { .. } => vec![
                event(),
                socket("emote", "Emote", Emotes),
                socket("viewers", "Viewers", Number),
                summary(),
            ],
            NodeKind::Chant { .. } => {
                vec![event(), socket("word", "Word", Text), socket("viewers", "Viewers", Number), summary()]
            }
            NodeKind::Bits { .. } => vec![
                event(),
                socket("amount", "Amount", Number),
                socket("viewer", "Viewer", Text),
                socket("message", "Message", Text),
                summary(),
            ],
            NodeKind::Sub { .. } => vec![
                event(),
                socket("viewer", "Viewer", Text),
                socket("months", "Months", Number),
                socket("tier", "Tier", Text),
                socket("gifter", "Gifter", Text),
                socket("message", "Message", Text),
                summary(),
            ],
            NodeKind::GiftBomb => vec![
                event(),
                socket("gifter", "Gifter", Text),
                socket("count", "Subs", Number),
                socket("tier", "Tier", Text),
                summary(),
            ],
            NodeKind::Follow => vec![event(), socket("viewer", "Viewer", Text), summary()],
            NodeKind::HypeTrain { .. } => vec![
                event(),
                socket("level", "Level", Number),
                socket("progress", "Progress", Number),
                socket("goal", "Goal", Number),
                socket("total", "Total", Number),
                socket("seconds_left", "Seconds left", Number),
                socket("train", "Train", Text),
                summary(),
            ],
            NodeKind::Raid { .. } => {
                vec![
                    event(),
                    socket("raider", "Raider", Text),
                    socket("viewers", "Viewers", Number),
                    summary(),
                ]
            }
            NodeKind::Reward { .. } => vec![
                event(),
                socket("viewer", "Viewer", Text),
                socket("reward", "Reward", Text),
                socket("text", "Viewer's text", Text),
                summary(),
            ],
            NodeKind::Custom { .. } => vec![event(), socket("name", "Name", Text), summary()],
            NodeKind::Chance { .. } => vec![socket("event", "Hit", Event), socket("miss", "Miss", Event)],
            NodeKind::ViewerRoles { .. } => {
                vec![socket("event", "Has a role", Event), socket("miss", "Miss", Event)]
            }
            NodeKind::HasText { .. } => {
                vec![socket("event", "Has text", Event), socket("miss", "Empty", Event)]
            }
            NodeKind::CompareText { .. } => vec![
                socket("event", "Matches", Event),
                socket("miss", "Miss", Event),
                socket("matched", "Matched", Text),
            ],
            NodeKind::CompareNumber { .. } => {
                vec![socket("event", "Matches", Event), socket("miss", "Miss", Event)]
            }
            NodeKind::EveryNth { .. } => vec![
                socket("event", "Nth", Event),
                socket("miss", "Miss", Event),
                socket("count", "Count", Number),
            ],
            NodeKind::Timer { .. } => vec![socket("event", "Tick", Event)],
            NodeKind::ChatCommand { .. } => vec![
                event(),
                socket("viewer", "Viewer", Text),
                socket("text", "Text after it", Text),
                socket("message", "Message", Text),
                summary(),
            ],
            NodeKind::Cooldown { .. } | NodeKind::Gate { .. } | NodeKind::Delay { .. } => vec![event()],
            NodeKind::Rarity { tiers, rest } => tiers
                .iter()
                .map(|tier| socket(&tier.socket(), &tier.name, Event))
                .chain([socket("rest", rest, Event)])
                .collect(),
            NodeKind::Number { .. } | NodeKind::Random { .. } => vec![socket("value", "Value", Number)],
            NodeKind::Math { .. } | NodeKind::Clamp { .. } => vec![socket("result", "Result", Number)],
            NodeKind::Text { .. } => vec![socket("text", "Text", Text)],
            NodeKind::EmotesInText => {
                vec![socket("emotes", "Emotes", Emotes), socket("count", "Count", Number)]
            }
            // An effect that waits in the queue: Started and Ended.
            NodeKind::Plugin { ty: Attached(Some(ty)), settings, .. } => {
                let mut outputs = ty.outputs();
                if ty.category == Category::Effects && ty.queued(settings) {
                    outputs.push(socket(STARTED_OUTPUT, "Started", Event));
                    outputs.push(socket(ENDED_OUTPUT, "Ended", Event));
                }
                outputs
            }
            NodeKind::Plugin { .. } => Vec::new(),
        }
    }

    /// A plugin node's voice changes (for the input it reads aloud), and the
    /// fields they can change.
    pub fn overrides(&self) -> Option<(&Overrides, &'static [Field])> {
        match self {
            NodeKind::Plugin { voice, ty: Attached(Some(ty)), .. } if ty.spoken_input().is_some() => {
                Some((voice, SPEECH_FIELDS))
            }
            _ => None,
        }
    }

    pub fn overrides_mut(&mut self) -> Option<&mut Overrides> {
        match self {
            NodeKind::Plugin { voice, ty: Attached(Some(ty)), .. } if ty.spoken_input().is_some() => {
                Some(voice)
            }
            _ => None,
        }
    }

    pub fn input(&self, name: &str) -> Option<Socket> {
        self.inputs().into_iter().find(|socket| socket.name == name)
    }

    pub fn output(&self, name: &str) -> Option<Socket> {
        self.outputs().into_iter().find(|socket| socket.name == name)
    }

    /// The setting that stands in for an unwired number input.
    pub fn number_setting(&self, input: &str) -> Option<f64> {
        match (self, input) {
            (NodeKind::Math { a, .. }, "a") => Some(*a),
            (NodeKind::Math { b, .. }, "b") => Some(*b),
            (NodeKind::Clamp { value, .. } | NodeKind::CompareNumber { value, .. }, "value") => Some(*value),
            (NodeKind::CompareNumber { number, .. }, "number") => Some(*number),
            (NodeKind::CompareNumber { and, .. }, "and") => Some(*and),
            (NodeKind::Delay { seconds }, "seconds") => Some(*seconds),
            (NodeKind::Clamp { min, .. } | NodeKind::Random { min, .. }, "min") => Some(*min),
            (NodeKind::Clamp { max, .. } | NodeKind::Random { max, .. }, "max") => Some(*max),
            (NodeKind::Plugin { settings, ty: Attached(Some(ty)), .. }, input) => {
                ty.input_setting(input, SocketType::Number)?.read(settings.get(input)).as_f64()
            }
            _ => None,
        }
    }

    /// The setting that stands in for an unwired text input.
    pub fn text_setting(&self, input: &str) -> Option<&str> {
        match (self, input) {
            (NodeKind::HasText { text } | NodeKind::CompareText { text, .. }, "text") => Some(text),
            (NodeKind::Plugin { settings, ty: Attached(Some(ty)), .. }, input) => {
                let setting = ty.input_setting(input, SocketType::Text)?;
                match (settings.get(input).and_then(|value| value.as_str()), &setting.kind) {
                    (Some(text), _) => Some(text),
                    (None, SettingKind::Text { default, .. }) => Some(default),
                    (None, _) => None,
                }
            }
            _ => None,
        }
    }

    /// Whether an event input can be left unwired: a Timer's Start and Stop
    /// (it can run from the start), an Every Nth's Reset.
    pub fn optional_input(&self, input: &str) -> bool {
        matches!(
            (self, input),
            (NodeKind::Timer { .. }, "start" | "stop") | (NodeKind::EveryNth { .. }, "reset")
        )
    }

    /// Whether events start from this node: an event node, or a Timer.
    pub fn is_source(&self) -> bool {
        self.is_event() || matches!(self, NodeKind::Timer { .. })
    }

    /// A Compare text node's verdict on `text`: the first of its texts it
    /// passes against (with `all`, once it passes against every one), or
    /// none. A `wired` text stands in for the list. Blank texts in the list
    /// don't count; a list with none matches nothing.
    pub fn compare_text(&self, text: &str, wired: Option<&str>) -> Option<String> {
        let NodeKind::CompareText { test, to, all, ignore_case, .. } = self else { return None };
        let to: Vec<&str> = match wired {
            Some(wired) => vec![wired],
            None => to.iter().map(String::as_str).filter(|each| !each.trim().is_empty()).collect(),
        };
        let passes = |against: &str| test.passes(text, against, *ignore_case);
        let first = to.iter().find(|each| passes(each))?;
        (!*all || to.iter().all(|each| passes(each))).then(|| first.trim().to_string())
    }

    /// A Text node with this template.
    pub fn text(template: impl Into<String>) -> NodeKind {
        NodeKind::Text { template: template.into(), size: None }
    }

    /// Attaches a plugin node's type from the installed plugins (nothing
    /// for other nodes).
    pub fn attach_plugin(&mut self, catalog: &Catalog) {
        if let NodeKind::Plugin { plugin, node, ty, .. } = self {
            *ty = Attached(catalog.node_type(plugin, node));
        }
    }
}

/// The `{name}` placeholders in a text, in order, without repeats.
pub fn placeholders(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (start, end) in braces(text) {
        let name = &text[start + 1..end];
        if !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !names.iter().any(|known| known == name)
        {
            names.push(name.to_string());
        }
    }
    names
}

/// Where each `{…}` in a text starts and ends (byte offsets of the braces).
/// A `{` with another one before its `}` is just a character: `{{months}`
/// has `{months}` (so typing a brace before a placeholder keeps it).
pub fn braces(text: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut open = None;
    for (index, c) in text.char_indices() {
        match c {
            '{' => open = Some(index),
            '}' => {
                if let Some(start) = open.take() {
                    found.push((start, index));
                }
            }
            _ => {}
        }
    }
    found
}

/// A problem that keeps a card from running. `node` points at the node to
/// highlight, when there is one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    pub node: Option<NodeId>,
    pub message: String,
}

impl Card {
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// The wire into an input, if any (an input takes at most one).
    pub fn wire_into(&self, node: NodeId, socket: &str) -> Option<&Wire> {
        self.wires.iter().find(|wire| wire.to.node == node && wire.to.socket == socket)
    }

    /// Removes wires into inputs or out of outputs their node no longer has
    /// (a `{placeholder}` taken out of a text, a rarity tier removed): they
    /// couldn't be drawn, so they couldn't be removed by hand either.
    pub fn drop_dangling_wires(&mut self) {
        let nodes = &self.nodes;
        // A missing plugin's node keeps its wires, for when it's back.
        let exists = |socket: &SocketRef, input: bool| {
            nodes.iter().find(|node| node.id == socket.node).is_none_or(|node| {
                if node.kind.is_missing_plugin() {
                    true
                } else if input {
                    node.kind.input(&socket.socket).is_some()
                } else {
                    node.kind.output(&socket.socket).is_some()
                }
            })
        };
        self.wires.retain(|wire| exists(&wire.from, false) && exists(&wire.to, true));
    }

    /// Attaches the plugin nodes' types from the installed plugins.
    pub fn attach_plugins(&mut self, catalog: &Catalog) {
        for node in &mut self.nodes {
            node.kind.attach_plugin(catalog);
        }
    }

    pub fn next_node_id(&self) -> NodeId {
        self.nodes.iter().map(|node| node.id).max().map_or(1, |id| id + 1)
    }

    /// Everything that keeps this card from running. An empty list means it
    /// runs. Checked by the server before it runs a card, and by the panel
    /// while editing.
    pub fn problems(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        let mut seen = BTreeSet::new();
        for node in &self.nodes {
            if !seen.insert(node.id) {
                problems.push(Problem {
                    node: Some(node.id),
                    message: format!("node id {} is used twice", node.id),
                });
            }
        }

        let mut missing_plugin = false;
        for node in &self.nodes {
            if let NodeKind::Plugin { plugin, node: name, ty: Attached(None), .. } = &node.kind {
                missing_plugin = true;
                problems.push(Problem {
                    node: Some(node.id),
                    message: format!(
                        "The {plugin} plugin isn't installed, or has no “{name}” node any more."
                    ),
                });
            }
        }

        let mut inputs_wired = BTreeSet::new();
        for wire in &self.wires {
            let (Some(from), Some(to)) = (self.node(wire.from.node), self.node(wire.to.node)) else {
                problems.push(Problem { node: None, message: "a wire points at a missing node".into() });
                continue;
            };
            if from.kind.is_missing_plugin() || to.kind.is_missing_plugin() {
                continue;
            }
            let (Some(output), Some(input)) =
                (from.kind.output(&wire.from.socket), to.kind.input(&wire.to.socket))
            else {
                let message = match (from.kind.output(&wire.from.socket), to.kind.input(&wire.to.socket)) {
                    (_, None) => format!(
                        "A wire goes into “{}”, which this node doesn't have any more.",
                        wire.to.socket
                    ),
                    _ => format!(
                        "A wire comes from “{}”, which {} doesn't have any more.",
                        wire.from.socket,
                        from.kind.title()
                    ),
                };
                problems.push(Problem { node: Some(to.id), message });
                continue;
            };
            if !input.ty.accepts(output.ty) {
                problems.push(Problem {
                    node: Some(to.id),
                    message: format!("{} can't take {}", input.label, output.label),
                });
            }
            if !inputs_wired.insert((wire.to.node, wire.to.socket.clone())) {
                problems
                    .push(Problem { node: Some(to.id), message: format!("{} has two wires", input.label) });
            }
        }

        for node in &self.nodes {
            for input in node.kind.inputs() {
                if input.ty == SocketType::Event
                    && !node.kind.optional_input(&input.name)
                    && self.wire_into(node.id, &input.name).is_none()
                {
                    // Not "Connect an event to Event." (asked, 2026-10-15).
                    let message = if input.label == "Event" {
                        "Connect an event.".to_string()
                    } else {
                        format!("Connect an event to {}.", input.label)
                    };
                    problems.push(Problem { node: Some(node.id), message });
                }
            }
            if matches!(node.kind, NodeKind::EmotesInText) && self.wire_into(node.id, "text").is_none() {
                problems.push(Problem {
                    node: Some(node.id),
                    message: "Connect a text to find emotes in.".into(),
                });
            }
            if let Some(message) = settings_problem(&node.kind) {
                problems.push(Problem { node: Some(node.id), message });
            }
        }

        if self.has_cycle() {
            problems.push(Problem { node: None, message: "the wires go round in a circle".into() });
        }
        // With a plugin missing, that's the problem to fix first.
        if missing_plugin {
            return problems;
        }
        if !self.nodes.is_empty() && !self.nodes.iter().any(|node| node.kind.is_source()) {
            problems.push(Problem {
                node: None,
                message: "Add an event, so the card has something to react to.".into(),
            });
        }
        if !self.nodes.is_empty() && !self.nodes.iter().any(|node| node.kind.category() == Category::Effects)
        {
            problems
                .push(Problem { node: None, message: "Add an effect, so the card does something.".into() });
        }
        problems
    }

    fn has_cycle(&self) -> bool {
        let mut next: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
        for wire in &self.wires {
            next.entry(wire.from.node).or_default().push(wire.to.node);
        }
        // Repeatedly remove nodes nothing points into; whatever is left loops.
        let mut incoming: BTreeMap<NodeId, usize> = self.nodes.iter().map(|node| (node.id, 0)).collect();
        for targets in next.values() {
            for target in targets {
                *incoming.entry(*target).or_default() += 1;
            }
        }
        let mut ready: Vec<NodeId> =
            incoming.iter().filter(|(_, count)| **count == 0).map(|(id, _)| *id).collect();
        let mut removed = 0;
        while let Some(id) = ready.pop() {
            removed += 1;
            for target in next.get(&id).into_iter().flatten() {
                let count = incoming.get_mut(target).expect("every target is counted");
                *count -= 1;
                if *count == 0 {
                    ready.push(*target);
                }
            }
        }
        removed < incoming.len()
    }
}

fn settings_problem(kind: &NodeKind) -> Option<String> {
    match kind {
        NodeKind::ChatWords { words } | NodeKind::Chant { words, .. }
            if words.iter().all(|word| word.trim().is_empty()) =>
        {
            Some("Add at least one word.".into())
        }
        NodeKind::EmoteCombo { viewers, .. } | NodeKind::Chant { viewers, .. }
            if viewers.is_nan() || *viewers < 2.0 =>
        {
            Some("It takes at least 2 viewers.".into())
        }
        NodeKind::EmoteCombo { seconds, .. } | NodeKind::Chant { seconds, .. }
            if !(seconds.is_finite() && *seconds > 0.0) =>
        {
            Some("The time must be more than 0 seconds.".into())
        }
        NodeKind::ChatCommand { command } if command.trim().is_empty() => {
            Some("Type the command, e.g. !tts.".into())
        }
        NodeKind::ViewerRoles { broadcaster, moderator, vip, subscriber }
            if !broadcaster && !moderator && !vip && !subscriber =>
        {
            Some("Tick at least one role.".into())
        }
        NodeKind::Sub { new, resub, gifted, prime_to_paid, gift_continued, .. }
            if !new && !resub && !gifted && !prime_to_paid && !gift_continued =>
        {
            Some("Pick at least one kind of sub.".into())
        }
        NodeKind::HypeTrain { started, levelled_up, progressed, ended }
            if !started && !levelled_up && !progressed && !ended =>
        {
            Some("Tick at least one.".into())
        }
        NodeKind::Chance { percent } if !(0.0..=100.0).contains(percent) => {
            Some("The chance must be from 0 to 100 %.".into())
        }
        NodeKind::Cooldown { seconds, .. } if !seconds.is_finite() || *seconds < 0.0 => {
            Some("The cooldown can't be negative.".into())
        }
        NodeKind::Gate { seconds } if !seconds.is_finite() || *seconds <= 0.0 => {
            Some("The gate must stay open for more than 0 seconds.".into())
        }
        NodeKind::Delay { seconds } if !seconds.is_finite() || *seconds < 0.0 => {
            Some("The delay can't be negative.".into())
        }
        NodeKind::EveryNth { every } if !every.is_finite() || every.round() < 1.0 => {
            Some("Every must be 1 or more.".into())
        }
        NodeKind::Timer { minutes, .. } if !minutes.is_finite() || *minutes <= 0.0 => {
            Some("The timer needs more than 0 minutes.".into())
        }
        NodeKind::Rarity { tiers, .. } if tiers.is_empty() => Some("Add at least one tier.".into()),
        NodeKind::Rarity { tiers, .. }
            if tiers.iter().any(|tier| !(tier.percent.is_finite() && tier.percent >= 0.0)) =>
        {
            Some("A chance can't be negative.".into())
        }
        NodeKind::Rarity { tiers, .. }
            if tiers.iter().map(|tier| tier.percent).sum::<f64>() > 100.0 + 1e-9 =>
        {
            Some("The chances add up to more than 100 %.".into())
        }
        NodeKind::Plugin { settings, ty: Attached(Some(ty)), .. } => ty.settings.iter().find_map(|setting| {
            let stored = setting.read(settings.get(&setting.key));
            let empty = |name: &str| name.trim().is_empty();
            match &setting.kind {
                SettingKind::Emotes { .. }
                    if stored
                        .as_array()
                        .is_some_and(|codes| codes.iter().filter_map(|code| code.as_str()).all(empty)) =>
                {
                    Some("List at least one emote.".into())
                }
                _ if setting.required && stored.as_str().is_none_or(empty) => {
                    Some(format!("Pick a {}.", setting.label.to_lowercase()))
                }
                _ => None,
            }
        }),
        NodeKind::Clamp { min, max, .. } if min > max => Some("Min is more than max.".into()),
        NodeKind::Random { min, max } if min > max => Some("Min is more than max.".into()),
        _ => None,
    }
}

/// A card as Export saves it and Import reads it:
/// `{ "lsk": "card", "version": 1, "card": {…} }`. `version` is the
/// config's format ([`CONFIG_VERSION`](crate::config::CONFIG_VERSION)), so
/// a later version can convert an older card as it converts the config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardFile {
    pub lsk: String,
    pub version: u32,
    pub card: Card,
}

/// What a card file's `lsk` says.
const CARD_FILE_TAG: &str = "card";

/// Why a file wasn't imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardFileError {
    /// It isn't a card file (or not one this version can read).
    NotACard,
    /// It's from a newer version of the app.
    Newer,
}

impl CardFile {
    pub fn new(card: Card) -> CardFile {
        CardFile { lsk: CARD_FILE_TAG.into(), version: crate::config::CONFIG_VERSION, card }
    }

    /// The file's text, indented: people read and share these too.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a card is JSON")
    }

    /// The card in a file's text.
    pub fn parse(text: &str) -> Result<Card, CardFileError> {
        #[derive(Deserialize)]
        struct Head {
            lsk: String,
            version: u32,
        }
        // Notepad may start it with a byte order mark.
        let text = text.trim_start_matches('\u{feff}');
        let head: Head = serde_json::from_str(text).map_err(|_| CardFileError::NotACard)?;
        if head.lsk != CARD_FILE_TAG {
            return Err(CardFileError::NotACard);
        }
        if head.version > crate::config::CONFIG_VERSION {
            return Err(CardFileError::Newer);
        }
        let file: CardFile = serde_json::from_str(text).map_err(|_| CardFileError::NotACard)?;
        Ok(file.card)
    }
}

/// Builds cards in code: premade cards and presets.
struct CardBuilder {
    card: Card,
}

impl CardBuilder {
    fn new(id: &str, name: &str, enabled: bool) -> CardBuilder {
        CardBuilder {
            card: Card { id: id.into(), name: name.into(), enabled, nodes: Vec::new(), wires: Vec::new() },
        }
    }

    fn node(&mut self, kind: NodeKind, x: f64, y: f64) -> NodeId {
        let id = self.card.next_node_id();
        self.card.nodes.push(Node { id, x, y, kind });
        id
    }

    fn wire(&mut self, from: NodeId, output: &str, to: NodeId, input: &str) -> &mut Self {
        self.card.wires.push(Wire { from: SocketRef::new(from, output), to: SocketRef::new(to, input) });
        self
    }

    fn alert(&mut self, event: NodeKind) -> Card {
        let source = self.node(event, 40.0, 80.0);
        let alert = self.node(alert_node("{summary}", 6.0), 360.0, 80.0);
        self.wire(source, "event", alert, "trigger");
        self.card.clone()
    }
}

/// A node of the built-in Rain plugin: random emotes (or the ones wired
/// in), `count` of them unless emotes are wired in. Its type is attached
/// with the other plugins'.
fn rain_node(count: f64) -> NodeKind {
    let mut settings = serde_json::Map::new();
    settings.insert("count".into(), count.into());
    NodeKind::Plugin {
        plugin: crate::plugin::RAIN_PLUGIN.into(),
        node: crate::plugin::RAIN_NODE.into(),
        settings,
        changes: serde_json::Map::new(),
        voice: Overrides::new(),
        ty: Attached(None),
    }
}

/// A node of the built-in Alerts plugin: `message` for `duration` seconds,
/// in the first style. Its type is attached with the other plugins'.
fn alert_node(message: impl Into<String>, duration: f64) -> NodeKind {
    let mut settings = serde_json::Map::new();
    settings.insert("message".into(), serde_json::Value::String(message.into()));
    settings.insert("duration".into(), duration.into());
    NodeKind::Plugin {
        plugin: crate::plugin::ALERT_PLUGIN.into(),
        node: crate::plugin::ALERT_NODE.into(),
        settings,
        changes: serde_json::Map::new(),
        voice: Overrides::new(),
        ty: Attached(None),
    }
}

/// A node of the built-in Speak plugin, reading `text` in its turn in the
/// queue.
fn speak_node(text: &str) -> NodeKind {
    let mut settings = serde_json::Map::new();
    settings.insert("text".into(), text.into());
    NodeKind::Plugin {
        plugin: crate::plugin::SPEAK_PLUGIN.into(),
        node: crate::plugin::SPEAK_NODE.into(),
        settings,
        changes: serde_json::Map::new(),
        voice: Overrides::new(),
        ty: Attached(None),
    }
}

/// A node of the built-in Hype train bar plugin, titled with the train's
/// name.
fn hype_train_bar_node() -> NodeKind {
    NodeKind::Plugin {
        plugin: crate::plugin::HYPE_TRAIN_PLUGIN.into(),
        node: crate::plugin::HYPE_TRAIN_BAR_NODE.into(),
        settings: serde_json::Map::new(),
        changes: serde_json::Map::new(),
        voice: Overrides::new(),
        ty: Attached(None),
    }
}

/// A node of the built-in Moderation plugin: times the event's viewer out
/// for `seconds`.
fn timeout_node(seconds: f64) -> NodeKind {
    let mut settings = serde_json::Map::new();
    settings.insert("seconds".into(), seconds.into());
    NodeKind::Plugin {
        plugin: crate::plugin::MODERATION_PLUGIN.into(),
        node: crate::plugin::TIMEOUT_NODE.into(),
        settings,
        changes: serde_json::Map::new(),
        voice: Overrides::new(),
        ty: Attached(None),
    }
}

/// Example cards: a few on, more off. The tests start from them; a fresh
/// install starts with no cards, and New card's presets are where a
/// streamer starts from (2026-10-15).
pub fn premade() -> Vec<Card> {
    let mut cards = vec![emote_rain("emote-rain")];

    cards.push(CardBuilder::new("bits-alert", "Bits alert", true).alert(NodeKind::Bits { min: 0.0 }));
    cards.push(sub_alert("sub-alert"));
    cards.push(CardBuilder::new("gift-bomb-alert", "Gift bomb alert", true).alert(NodeKind::GiftBomb));
    cards.push(CardBuilder::new("raid-alert", "Raid alert", true).alert(NodeKind::Raid { min_viewers: 0.0 }));
    cards.push(
        CardBuilder::new("channel-points-alert", "Channel points alert", true)
            .alert(NodeKind::Reward { reward: String::new() }),
    );
    cards.push(follow_alert("follow-alert", false));
    cards.push(hype_train_alert("hype-train-alert", false));
    cards.push(hype_train_bar("hype-train-bar", false));

    cards.push(rain_on_bits("rain-on-bits", false));
    cards.push(hydrate("hydrate", false));
    cards.push(hype_words("hype-words", false));
    cards
}

/// Every emote in chat falls on the overlay.
fn emote_rain(id: &str) -> Card {
    let mut card = CardBuilder::new(id, "Emote rain", true);
    let chat = card.node(NodeKind::EmotesInChat, 40.0, 80.0);
    let fall = card.node(rain_node(1.0), 360.0, 80.0);
    card.wire(chat, "event", fall, "trigger").wire(chat, "emotes", fall, "emotes");
    card.card
}

fn rain_on_bits(id: &str, enabled: bool) -> Card {
    let mut card = CardBuilder::new(id, "Rain on bits", enabled);
    let bits = card.node(NodeKind::Bits { min: 100.0 }, 40.0, 60.0);
    let divide = card.node(NodeKind::Math { op: MathOp::Divide, a: 0.0, b: 100.0 }, 320.0, 240.0);
    let clamp = card.node(NodeKind::Clamp { value: 0.0, min: 1.0, max: 50.0 }, 560.0, 240.0);
    let fall = card.node(rain_node(1.0), 800.0, 60.0);
    card.wire(bits, "event", fall, "trigger")
        .wire(bits, "amount", divide, "a")
        .wire(divide, "result", clamp, "value")
        .wire(clamp, "result", fall, "count");
    card.card
}

fn hydrate(id: &str, enabled: bool) -> Card {
    let mut card = CardBuilder::new(id, "Hydrate", enabled);
    let reward = card.node(NodeKind::Reward { reward: "Hydrate".into() }, 40.0, 60.0);
    let cooldown = card.node(NodeKind::Cooldown { seconds: 30.0, per_viewer: false }, 360.0, 60.0);
    let alert = card.node(alert_node("{viewer}: drink some water!", 6.0), 640.0, 60.0);
    card.wire(reward, "event", cooldown, "event").wire(cooldown, "event", alert, "trigger");
    card.card
}

/// Off at first: a big channel may not want one per follow. The cooldown
/// skips the same viewer following again within an hour (unfollow and
/// refollow spam), not a burst of different viewers (after a raid).
/// An alert for new subs, resubs and gifted subs (not each gift in a gift
/// bomb: that has an alert of its own).
fn sub_alert(id: &str) -> Card {
    CardBuilder::new(id, "Sub alert", true).alert(NodeKind::Sub {
        new: true,
        resub: true,
        gifted: true,
        in_gift_bomb: false,
        prime_to_paid: true,
        gift_continued: true,
    })
}

fn follow_alert(id: &str, enabled: bool) -> Card {
    let mut card = CardBuilder::new(id, "Follow alert", enabled);
    let follow = card.node(NodeKind::Follow, 40.0, 60.0);
    let cooldown = card.node(NodeKind::Cooldown { seconds: 3600.0, per_viewer: true }, 360.0, 60.0);
    let alert = card.node(alert_node("{viewer} followed!", 5.0), 640.0, 60.0);
    card.wire(follow, "event", cooldown, "event").wire(cooldown, "event", alert, "trigger");
    card.card
}

/// Off at first: only Affiliates and Partners have hype trains. An alert
/// as one starts, at each new level and as it ends.
fn hype_train_alert(id: &str, enabled: bool) -> Card {
    let mut card = CardBuilder::new(id, "Hype train alert", enabled);
    card.alert(NodeKind::HypeTrain { started: true, levelled_up: true, progressed: false, ended: true })
}

/// Off at first, like the alert. The bar follows everything that happens to
/// the train: every contribution fills it up.
fn hype_train_bar(id: &str, enabled: bool) -> Card {
    let mut card = CardBuilder::new(id, "Hype train bar", enabled);
    let train = card.node(
        NodeKind::HypeTrain { started: true, levelled_up: true, progressed: true, ended: true },
        40.0,
        60.0,
    );
    let bar = card.node(hype_train_bar_node(), 360.0, 60.0);
    card.wire(train, "event", bar, "trigger");
    for (output, input) in [
        ("train", "title"),
        ("level", "level"),
        ("progress", "progress"),
        ("goal", "goal"),
        ("seconds_left", "seconds_left"),
    ] {
        card.wire(train, output, bar, input);
    }
    card.card
}

fn hype_words(id: &str, enabled: bool) -> Card {
    let mut card = CardBuilder::new(id, "Hype words", enabled);
    let words = card.node(NodeKind::ChatWords { words: vec!["hype".into(), "letsgo".into()] }, 40.0, 60.0);
    let chance = card.node(NodeKind::Chance { percent: 50.0 }, 340.0, 60.0);
    let cooldown = card.node(NodeKind::Cooldown { seconds: 30.0, per_viewer: false }, 560.0, 60.0);
    let fall = card.node(rain_node(20.0), 800.0, 60.0);
    card.wire(words, "event", chance, "event")
        .wire(chance, "event", cooldown, "event")
        .wire(cooldown, "event", fall, "trigger");
    card.card
}

/// A starting point for a new card.
pub struct Preset {
    pub group: PresetGroup,
    pub title: &'static str,
    pub description: &'static str,
    pub card: fn(id: &str) -> Card,
}

/// Where the New card page lists a preset (2026-10-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetGroup {
    /// Empty card, on its own above the groups.
    Empty,
    Alerts,
    EmoteRain,
    More,
}

impl PresetGroup {
    pub const ALL: [PresetGroup; 4] =
        [PresetGroup::Empty, PresetGroup::Alerts, PresetGroup::EmoteRain, PresetGroup::More];

    /// The group's box title; none for Empty card's.
    pub fn title(self) -> &'static str {
        match self {
            PresetGroup::Empty => "",
            PresetGroup::Alerts => "Alerts",
            PresetGroup::EmoteRain => "Emote rain",
            PresetGroup::More => "More",
        }
    }
}

/// The "New card" page, in its order: what streamers most often want
/// first (asked 2026-10-15: subs, bits, channel points, raids).
pub fn presets() -> Vec<Preset> {
    vec![
        Preset {
            group: PresetGroup::Empty,
            title: "Empty card",
            description: "Start with an empty canvas.",
            card: |id| CardBuilder::new(id, "New card", true).card,
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Sub alert",
            description: "New subs, resubs and gifted subs (a gift bomb gets one alert of its own).",
            card: |id| sub_alert(id),
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Bits alert",
            description: "Every cheer.",
            card: |id| CardBuilder::new(id, "Bits alert", true).alert(NodeKind::Bits { min: 0.0 }),
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Channel point alert",
            description: "Pick a reward; show an alert with the viewer and their text.",
            card: |id| {
                CardBuilder::new(id, "Channel point alert", true)
                    .alert(NodeKind::Reward { reward: String::new() })
            },
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Raid alert",
            description: "Every raid. Set At least on its Raid node to skip small ones.",
            card: |id| CardBuilder::new(id, "Raid alert", true).alert(NodeKind::Raid { min_viewers: 0.0 }),
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Gift bomb alert",
            description: "One alert when a viewer gifts several subs at once.",
            card: |id| CardBuilder::new(id, "Gift bomb alert", true).alert(NodeKind::GiftBomb),
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Follow alert",
            description: "A new follower (needs the Twitch login).",
            card: |id| follow_alert(id, true),
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Hype train alert",
            description: "An alert as a hype train starts, at each new level and as it ends.",
            card: |id| hype_train_alert(id, true),
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Bigger alerts for big cheers",
            description: "Cheers of 1,000 bits or more get a longer alert of their own.",
            card: |id| {
                let mut card = CardBuilder::new(id, "Cheer alerts", true);
                let bits = card.node(NodeKind::Bits { min: 0.0 }, 40.0, 220.0);
                let compare = card.node(
                    NodeKind::CompareNumber {
                        test: NumberTest::AtLeast,
                        value: 0.0,
                        number: 1000.0,
                        and: 5000.0,
                    },
                    340.0,
                    220.0,
                );
                let big = card.node(alert_node("{viewer} cheered {amount} bits!!", 10.0), 600.0, 20.0);
                let small = card.node(alert_node("{summary}", 6.0), 600.0, 420.0);
                card.wire(bits, "event", compare, "event")
                    .wire(bits, "amount", compare, "value")
                    .wire(compare, "event", big, "trigger")
                    .wire(compare, "miss", small, "trigger");
                card.card
            },
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Welcome first-time chatters",
            description: "An alert for a viewer's first message in your chat.",
            card: |id| {
                let mut card = CardBuilder::new(id, "Welcome", true);
                let first = card.node(NodeKind::FirstChatter, 40.0, 60.0);
                let alert = card.node(alert_node("Welcome, {viewer}!", 6.0), 360.0, 60.0);
                card.wire(first, "event", alert, "trigger");
                card.card
            },
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Chant → alert",
            description: "An alert when 5 viewers say the same word (like “gg”) within 20 s.",
            card: |id| {
                let mut card = CardBuilder::new(id, "Chant", true);
                card.alert(NodeKind::Chant { words: vec!["gg".into()], viewers: 5.0, seconds: 20.0 })
            },
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Rarity alerts",
            description: "A channel point reward rolls a rarity: 1 % legendary, 10 % epic, 20 % rare, else common.",
            card: |id| {
                let mut card = CardBuilder::new(id, "Rarity alerts", true);
                let reward = card.node(NodeKind::Reward { reward: String::new() }, 40.0, 520.0);
                let rarity = card.node(
                    NodeKind::Rarity { tiers: default_tiers(), rest: "Common".into() },
                    340.0,
                    520.0,
                );
                let outputs: Vec<String> =
                    default_tiers().iter().map(Tier::socket).chain(["rest".into()]).collect();
                let names = ["LEGENDARY", "Epic", "Rare", "Common"];
                for (n, (output, name)) in outputs.iter().zip(names).enumerate() {
                    let message = format!("{{viewer}} rolled {name}!");
                    let alert = card.node(alert_node(message, 6.0), 680.0, 20.0 + 400.0 * n as f64);
                    card.wire(rarity, output, alert, "trigger");
                }
                card.wire(reward, "event", rarity, "event");
                card.card
            },
        },
        Preset {
            group: PresetGroup::Alerts,
            title: "Hydrate reminder",
            description: "An alert every 30 minutes. Wire Stream started and Stream ended (OBS) into Start and Stop for only while live.",
            card: |id| {
                let mut card = CardBuilder::new(id, "Hydrate reminder", true);
                let timer = card.node(NodeKind::Timer { minutes: 30.0, from_start: true }, 40.0, 60.0);
                let alert = card.node(alert_node("Time for some water!", 6.0), 320.0, 60.0);
                card.wire(timer, "event", alert, "trigger");
                card.card
            },
        },
        Preset {
            group: PresetGroup::EmoteRain,
            title: "Emote rain from chat",
            description: "Every emote in a chat message falls on the overlay.",
            card: |id| emote_rain(id),
        },
        Preset {
            group: PresetGroup::EmoteRain,
            title: "Rain on bits",
            description: "One emote per 100 bits, between 1 and 50.",
            card: |id| rain_on_bits(id, true),
        },
        Preset {
            group: PresetGroup::EmoteRain,
            title: "Rain the emotes in a cheer",
            description: "The emotes in a cheer's message rain down.",
            card: |id| {
                let mut card = CardBuilder::new(id, "Cheer emotes", true);
                let bits = card.node(NodeKind::Bits { min: 0.0 }, 40.0, 60.0);
                let find = card.node(NodeKind::EmotesInText, 340.0, 200.0);
                let fall = card.node(rain_node(1.0), 600.0, 60.0);
                card.wire(bits, "event", fall, "trigger")
                    .wire(bits, "message", find, "text")
                    .wire(find, "emotes", fall, "emotes");
                card.card
            },
        },
        Preset {
            group: PresetGroup::EmoteRain,
            title: "Emote combo → rain",
            description: "The same emote from 5 viewers within 10 s rains that emote, 4 per viewer.",
            card: |id| {
                let mut card = CardBuilder::new(id, "Emote combo", true);
                let combo = card.node(NodeKind::EmoteCombo { viewers: 5.0, seconds: 10.0 }, 40.0, 60.0);
                let times = card.node(NodeKind::Math { op: MathOp::Multiply, a: 0.0, b: 4.0 }, 340.0, 220.0);
                let fall = card.node(rain_node(1.0), 600.0, 60.0);
                card.wire(combo, "event", fall, "trigger")
                    .wire(combo, "emote", fall, "emotes")
                    .wire(combo, "viewers", times, "a")
                    .wire(times, "result", fall, "count");
                card.card
            },
        },
        Preset {
            group: PresetGroup::EmoteRain,
            title: "Chat words → rain",
            description: "Words like “hype” make it rain, with a chance and a cooldown.",
            card: |id| hype_words(id, true),
        },
        Preset {
            group: PresetGroup::EmoteRain,
            title: "Emote rain for a while",
            description: "A channel point reward makes every emote in chat rain for 60 s.",
            card: |id| {
                let mut card = CardBuilder::new(id, "Emote rain for a while", true);
                let reward = card.node(NodeKind::Reward { reward: String::new() }, 40.0, 40.0);
                let chat = card.node(NodeKind::EmotesInChat, 40.0, 320.0);
                let gate = card.node(NodeKind::Gate { seconds: 60.0 }, 360.0, 200.0);
                let fall = card.node(rain_node(1.0), 640.0, 420.0);
                let alert =
                    card.node(alert_node("{viewer} made every emote rain for 60 s!", 6.0), 640.0, 20.0);
                card.wire(reward, "event", gate, "open")
                    .wire(reward, "event", alert, "trigger")
                    .wire(chat, "event", gate, "event")
                    .wire(gate, "event", fall, "trigger")
                    .wire(chat, "emotes", fall, "emotes");
                card.card
            },
        },
        Preset {
            group: PresetGroup::More,
            title: "Read chat aloud",
            description: "Mods (and you) write “!tts hello” and it's read out; once a minute each.",
            card: |id| {
                let mut card = CardBuilder::new(id, "Read chat aloud", true);
                let command = card.node(NodeKind::ChatCommand { command: "!tts".into() }, 40.0, 60.0);
                let roles = card.node(
                    NodeKind::ViewerRoles {
                        broadcaster: true,
                        moderator: true,
                        vip: false,
                        subscriber: false,
                    },
                    340.0,
                    60.0,
                );
                let cooldown = card.node(NodeKind::Cooldown { seconds: 60.0, per_viewer: true }, 580.0, 60.0);
                let speak = card.node(speak_node("{text}"), 820.0, 60.0);
                card.wire(command, "event", roles, "event")
                    .wire(roles, "event", cooldown, "event")
                    .wire(cooldown, "event", speak, "trigger");
                card.card
            },
        },
        Preset {
            group: PresetGroup::More,
            title: "Hype train bar",
            description: "A bar on the overlay that fills up as a hype train goes.",
            card: |id| hype_train_bar(id, true),
        },
        Preset {
            group: PresetGroup::More,
            title: "Vanish",
            description: "“!vanish” times the viewer out for 10 s, clearing their messages (needs the Twitch login, with Moderating).",
            card: |id| {
                let mut card = CardBuilder::new(id, "Vanish", true);
                let command = card.node(NodeKind::ChatCommand { command: "!vanish".into() }, 40.0, 60.0);
                let timeout = card.node(timeout_node(10.0), 360.0, 60.0);
                card.wire(command, "event", timeout, "trigger");
                card.card
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The built-in plugins the premade cards use.
    fn built_in() -> Catalog {
        let manifest = |text: &str| crate::plugin::PluginInfo {
            manifest: serde_json::from_str(text).unwrap(),
            overlay: true,
            bundled: true,
            ..Default::default()
        };
        Catalog::new(vec![
            manifest(include_str!("../../server/plugins/rain/plugin.json")),
            manifest(include_str!("../../server/plugins/alert/plugin.json")),
            manifest(include_str!("../../server/plugins/speak/plugin.json")),
            manifest(include_str!("../../server/plugins/hype-train/plugin.json")),
            manifest(include_str!("../../server/plugins/moderation/plugin.json")),
        ])
    }

    #[test]
    fn presets_come_in_their_groups_most_wanted_first() {
        let presets = presets();
        let groups: Vec<PresetGroup> = presets.iter().map(|preset| preset.group).collect();
        let mut sorted = groups.clone();
        sorted.sort_by_key(|group| PresetGroup::ALL.iter().position(|each| each == group));
        assert_eq!(groups, sorted, "each group's presets together, in the groups' order");
        assert_eq!(presets[0].title, "Empty card");
        let alerts: Vec<&str> = presets
            .iter()
            .filter(|preset| preset.group == PresetGroup::Alerts)
            .map(|preset| preset.title)
            .take(4)
            .collect();
        assert_eq!(alerts, ["Sub alert", "Bits alert", "Channel point alert", "Raid alert"]);
    }

    #[test]
    fn premade_cards_and_presets_have_no_problems() {
        let catalog = built_in();
        for mut card in premade() {
            card.attach_plugins(&catalog);
            assert_eq!(card.problems(), [], "{}", card.name);
        }
        for preset in presets() {
            let mut card = (preset.card)("id");
            card.attach_plugins(&catalog);
            if !card.nodes.is_empty() {
                assert_eq!(card.problems(), [], "{}", preset.title);
            }
        }
        let ids: BTreeSet<_> = premade().into_iter().map(|card| card.id).collect();
        assert_eq!(ids.len(), premade().len(), "premade card ids are unique");
    }

    #[test]
    fn json_shape() {
        let card = hydrate("hydrate", true);
        let json = serde_json::to_value(&card).unwrap();
        assert_eq!(json["nodes"][0]["kind"], serde_json::json!({ "type": "reward", "reward": "Hydrate" }));
        assert_eq!(json["wires"][0]["from"], serde_json::json!({ "node": 1, "socket": "event" }));
        let back: Card = serde_json::from_value(json).unwrap();
        assert_eq!(back, card);
    }

    #[test]
    fn card_files() {
        let mut card = hydrate("hydrate", true);
        card.attach_plugins(&built_in());
        let text = CardFile::new(card.clone()).to_json();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!((json["lsk"].as_str(), json["version"].as_u64()), (Some("card"), Some(1)));
        assert!(!text.contains("\"inputs\""), "plugin nodes' types aren't saved");
        assert_eq!(CardFile::parse(&text), Ok(card.clone()));
        assert_eq!(CardFile::parse(&format!("\u{feff}{text}")), Ok(card), "a byte order mark is skipped");

        let newer = text.replacen("\"version\": 1", "\"version\": 2", 1);
        assert_eq!(CardFile::parse(&newer), Err(CardFileError::Newer));
        // Nodes copied in the editor, a config, anything else.
        assert_eq!(CardFile::parse(r#"{"lsk":"nodes","nodes":[],"wires":[]}"#), Err(CardFileError::NotACard));
        assert_eq!(CardFile::parse(r#"{"version":1,"cards":[]}"#), Err(CardFileError::NotACard));
        assert_eq!(CardFile::parse("not json"), Err(CardFileError::NotACard));
        let unknown = text.replacen("\"reward\",", "\"no-such-node\",", 1);
        assert_ne!(unknown, text);
        assert_eq!(CardFile::parse(&unknown), Err(CardFileError::NotACard));
    }

    #[test]
    fn problems_name_the_node() {
        let mut card = hydrate("h", true);
        card.attach_plugins(&built_in());
        card.wires.retain(|wire| wire.to.socket != "trigger");
        assert_eq!(
            card.problems(),
            [Problem { node: Some(3), message: "Connect an event to Trigger.".into() }]
        );
        // An input called Event isn't named twice.
        let mut card = hydrate("h", true);
        card.attach_plugins(&built_in());
        card.nodes.push(Node { id: 9, x: 0.0, y: 0.0, kind: NodeKind::Gate { seconds: 60.0 } });
        let gate: Vec<String> = card
            .problems()
            .into_iter()
            .filter(|problem| problem.node == Some(9))
            .map(|problem| problem.message)
            .collect();
        assert_eq!(gate, ["Connect an event.", "Connect an event to Open."]);

        let mut card = rain_on_bits("r", true);
        card.wires.push(Wire { from: SocketRef::new(1, "viewer"), to: SocketRef::new(2, "b") });
        assert!(card.problems().iter().any(|problem| problem.message == "By can't take Viewer"));

        let mut card = hype_words("w", true);
        card.wires.push(Wire { from: SocketRef::new(3, "event"), to: SocketRef::new(2, "event") });
        let problems = card.problems();
        assert!(problems.iter().any(|problem| problem.message.contains("circle")), "{problems:?}");
        assert!(problems.iter().any(|problem| problem.message == "Event has two wires"), "{problems:?}");
    }

    #[test]
    fn effects_waiting_in_the_queue_have_started_and_ended() {
        let catalog = built_in();
        let attached = |mut kind: NodeKind| {
            kind.attach_plugin(&catalog);
            kind
        };
        let names =
            |kind: &NodeKind| kind.outputs().into_iter().map(|socket| socket.name).collect::<Vec<_>>();
        assert_eq!(names(&attached(alert_node("Hi", 5.0))), [STARTED_OUTPUT, ENDED_OUTPUT]);
        assert!(names(&attached(rain_node(3.0))).is_empty(), "the rain never waits");

        // Speak waits only while ticked; unticked, a wire from Started goes.
        let speak = attached(speak_node("Hi"));
        assert_eq!(names(&speak), [STARTED_OUTPUT, ENDED_OUTPUT]);
        let mut card = Card {
            id: "c".into(),
            name: "C".into(),
            enabled: true,
            nodes: vec![
                Node { id: 1, x: 0.0, y: 0.0, kind: speak },
                Node { id: 2, x: 0.0, y: 0.0, kind: attached(rain_node(3.0)) },
            ],
            wires: vec![Wire { from: SocketRef::new(1, STARTED_OUTPUT), to: SocketRef::new(2, "trigger") }],
        };
        card.drop_dangling_wires();
        assert_eq!(card.wires.len(), 1);
        if let NodeKind::Plugin { settings, .. } = &mut card.nodes[0].kind {
            settings.insert(crate::plugin::QUEUE_SETTING.into(), false.into());
        }
        card.drop_dangling_wires();
        assert!(card.wires.is_empty());
    }

    #[test]
    fn wires_into_removed_placeholders_go() {
        let mut card = rain_on_bits("r", true);
        let text = card.next_node_id();
        card.nodes.push(Node { id: text, x: 0.0, y: 0.0, kind: NodeKind::text("{a} {b}") });
        card.wires.push(Wire { from: SocketRef::new(1, "viewer"), to: SocketRef::new(text, "a") });
        card.wires.push(Wire { from: SocketRef::new(1, "message"), to: SocketRef::new(text, "b") });
        card.nodes.last_mut().unwrap().kind = NodeKind::text("{a}");
        assert!(
            card.problems().iter().any(|problem| problem.message.contains("“b”")),
            "{:?}",
            card.problems()
        );
        let wires = card.wires.len();
        card.drop_dangling_wires();
        assert_eq!(card.wires.len(), wires - 1);
        assert!(card.wires.iter().any(|wire| wire.to.socket == "a"));
    }

    #[test]
    fn text_inputs_come_from_placeholders() {
        let text = NodeKind::text("{viewer} gave {amount} {viewer} {} {no space}");
        let names: Vec<String> = text.inputs().into_iter().map(|socket| socket.name).collect();
        assert_eq!(names, ["viewer", "amount"]);
        // Emotes are not inputs (the server fills them; see `EmoteText`).
        let emote = NodeKind::text("{e:Kappa} {viewer}");
        let names: Vec<String> = emote.inputs().into_iter().map(|socket| socket.name).collect();
        assert_eq!(names, ["viewer"]);
        // A brace typed before a placeholder (starting an emote, say)
        // doesn't hide it, so its wire stays; text styles' marks aren't
        // placeholders.
        assert_eq!(placeholders("{{months} {e:{viewer}"), ["months", "viewer"]);
        assert_eq!(placeholders("[big]{viewer}[/] {"), ["viewer"]);
    }

    #[test]
    fn numbers_go_into_text_but_not_back() {
        assert!(SocketType::Text.accepts(SocketType::Number));
        assert!(!SocketType::Number.accepts(SocketType::Text));
        assert!(!SocketType::Event.accepts(SocketType::Number));
    }

    #[test]
    fn rarity_outputs_and_problems() {
        let rarity = |percents: &[f64]| NodeKind::Rarity {
            tiers: percents
                .iter()
                .enumerate()
                .map(|(n, percent)| Tier { id: n as u32 + 1, name: format!("T{n}"), percent: *percent })
                .collect(),
            rest: "Common".into(),
        };
        let names: Vec<String> =
            rarity(&[1.0, 10.0]).outputs().into_iter().map(|socket| socket.name).collect();
        assert_eq!(names, ["tier-1", "tier-2", "rest"]);
        assert_eq!(rarity(&[1.0, 10.0]).outputs()[2].label, "Common");
        assert_eq!(settings_problem(&rarity(&[50.0, 50.0])), None);
        assert!(settings_problem(&rarity(&[50.0, 50.5])).unwrap().contains("more than 100"));
        assert!(settings_problem(&rarity(&[-1.0])).is_some());
        assert!(settings_problem(&rarity(&[])).is_some());
    }

    #[test]
    fn crowds_need_two_viewers_and_some_time() {
        let problem = |kind: NodeKind| settings_problem(&kind);
        assert_eq!(problem(NodeKind::EmoteCombo { viewers: 5.0, seconds: 10.0 }), None);
        assert!(problem(NodeKind::EmoteCombo { viewers: 1.0, seconds: 10.0 }).is_some());
        assert!(problem(NodeKind::EmoteCombo { viewers: f64::NAN, seconds: 10.0 }).is_some());
        assert!(problem(NodeKind::Chant { words: vec!["gg".into()], viewers: 3.0, seconds: 0.0 }).is_some());
        assert!(problem(NodeKind::Chant { words: vec![" ".into()], viewers: 3.0, seconds: 5.0 }).is_some());
    }

    #[test]
    fn math() {
        assert_eq!(MathOp::Divide.apply(450.0, 100.0), 4.5);
        assert_eq!(MathOp::Divide.apply(1.0, 0.0), 0.0);
        assert_eq!(MathOp::Max.apply(1.0, 3.0), 3.0);
    }

    #[test]
    fn number_tests() {
        use NumberTest::*;
        assert!(AtLeast.passes(100.0, 100.0, 0.0) && !AtLeast.passes(99.0, 100.0, 0.0));
        assert!(AtMost.passes(100.0, 100.0, 0.0) && !AtMost.passes(101.0, 100.0, 0.0));
        assert!(Between.passes(5.0, 10.0, 1.0), "either way round");
        assert!(Between.passes(10.0, 1.0, 10.0) && !Between.passes(10.5, 1.0, 10.0));
        assert!(Is.passes(0.1 + 0.2, 0.3, 0.0) && !Is.passes(1.0, 2.0, 0.0));
        assert!(!AtLeast.passes(f64::NAN, 0.0, 0.0));
        let compare = |test| NodeKind::CompareNumber { test, value: 0.0, number: 1.0, and: 2.0 };
        let names = |kind: NodeKind| kind.inputs().into_iter().map(|socket| socket.label).collect::<Vec<_>>();
        assert_eq!(names(compare(AtLeast)), ["Event", "Value", "At least"]);
        assert_eq!(names(compare(Between)), ["Event", "Value", "Between", "And"]);
    }

    #[test]
    fn a_timer_is_where_events_start() {
        // A Timer into an alert: no event node needed, nor Start or Stop
        // wired; an Every Nth's Reset needn't be wired either.
        let mut card = CardBuilder::new("t", "T", true);
        let timer = card.node(NodeKind::Timer { minutes: 15.0, from_start: true }, 40.0, 60.0);
        let nth = card.node(NodeKind::EveryNth { every: 2.0 }, 300.0, 60.0);
        let alert = card.node(alert_node("Hi", 5.0), 560.0, 60.0);
        card.wire(timer, "event", nth, "event").wire(nth, "event", alert, "trigger");
        let mut card = card.card;
        card.attach_plugins(&built_in());
        assert_eq!(card.problems(), []);

        let problem = |kind: NodeKind| settings_problem(&kind);
        assert!(problem(NodeKind::Timer { minutes: 0.0, from_start: true }).is_some());
        assert!(problem(NodeKind::EveryNth { every: 0.4 }).is_some());
        assert_eq!(problem(NodeKind::EveryNth { every: 0.6 }), None, "rounds to 1");
        assert!(problem(NodeKind::Delay { seconds: -1.0 }).is_some());
        assert_eq!(problem(NodeKind::Delay { seconds: 0.0 }), None);
    }

    #[test]
    fn text_tests() {
        use TextTest::*;
        assert!(Is.passes(" Gaming ", "gaming", true));
        assert!(!Is.passes("Gaming", "gaming", false));
        assert!(Contains.passes("Just Chatting", "chat", true));
        assert!(StartsWith.passes("Just Chatting", "just", true));
        assert!(EndsWith.passes("Just Chatting", "Chatting", false));
        assert!(!Contains.passes("anything", "  ", true), "an empty text never matches");
        assert!(ContainsWord.passes("Let's go, hype!", "HYPE", true));
        assert!(!ContainsWord.passes("hyped up", "hype", true), "whole words only");
        assert!(ContainsWord.passes("good game, everyone", "Good game!", true), "several words in a row");
        assert!(!ContainsWord.passes("good old game", "good game", true));

        let compare = |to: &[&str], all: bool| NodeKind::CompareText {
            text: String::new(),
            test: Contains,
            to: to.iter().map(|each| each.to_string()).collect(),
            all,
            ignore_case: true,
        };
        let any = compare(&["", "red", "blue"], false);
        assert_eq!(any.compare_text("Blue sky", None).as_deref(), Some("blue"));
        assert_eq!(any.compare_text("green", None), None);
        assert_eq!(
            any.compare_text("green", Some("GREEN")).as_deref(),
            Some("GREEN"),
            "a wired text instead"
        );
        let all = compare(&["red", "blue"], true);
        assert_eq!(all.compare_text("red and blue", None).as_deref(), Some("red"));
        assert_eq!(all.compare_text("only blue", None), None);
        assert_eq!(compare(&[" "], true).compare_text("x", None), None, "nothing to match");
    }
}
