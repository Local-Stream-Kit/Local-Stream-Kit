//! Runs cards (see `protocol::cards`): a signal that matches an event node
//! flows along the card's event wires through flow nodes (chance, cooldown)
//! into effect nodes, whose inputs are computed from value wires at that
//! moment, from the same event. Combo and chant nodes remember who posted
//! what lately, so they fire only once a crowd is complete. An effect that
//! waits in the queue carries on from its Started and Ended outputs when
//! its run goes on stage and when it has ended ([`Then`]), with the same
//! event. Delay nodes hold events and Timers tick, both on the pipeline's
//! tick ([`CardRuntime::on_tick`]).

mod runs;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use protocol::activity::ItemOrigin;
use protocol::cards::{self, Card, Category, Node, NodeId, NodeKind, NumberTest, SocketType};
use protocol::plugin::{Attached, ENDED_OUTPUT, NodeType, STARTED_OUTPUT, SettingKind};
use serde::{Deserialize, Serialize};

use crate::effects::Effects;
use crate::emote_text::EmoteText;
use crate::model::{
    HypeTrainChange, MembershipKind, Origin, PluginValue, PostedEmote, Signal, SignalKind, UpgradedFrom, User,
};
use crate::pipeline::Ctx;
use crate::plugins;
use crate::services::activity::{Cause, Hearers};
use crate::transformers::find_emotes_in_text;

#[cfg(test)]
pub use runs::Queued;
pub use runs::{CardRun, Decided, Decision, Recording, Start};

/// A value on a wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Number(f64),
    Text(EmoteText),
    Emotes(Vec<PostedEmote>),
}

impl Value {
    fn number(&self) -> Option<f64> {
        match self {
            Value::Number(number) => Some(*number),
            Value::Text(text) => text.plain().trim().parse().ok(),
            Value::Emotes(emotes) => Some(emotes.len() as f64),
        }
    }

    /// As a plugin gets it (a run's values): a number, a text with its
    /// emotes, emotes.
    pub fn json(&self) -> serde_json::Value {
        match self {
            Value::Number(number) => serde_json::Value::from(*number),
            Value::Text(text) => plugins::text_value(text),
            Value::Emotes(emotes) => plugins::emotes_value(emotes),
        }
    }

    pub fn plain(&self) -> String {
        self.text().plain()
    }

    /// As an output of type `ty` carries it: a text that reads as a number
    /// for a number, anything as text for a text; `None` if it can't be. A
    /// number is finite: NaN and infinity (from a plugin, or a text that
    /// says so) are nothing, as JSON can't keep them (a queued run's
    /// [`Then`] is saved).
    pub fn into_type(self, ty: SocketType) -> Option<Value> {
        match ty {
            SocketType::Number => self.number().filter(|number| number.is_finite()).map(Value::Number),
            SocketType::Text => Some(Value::Text(self.text())),
            SocketType::Emotes => matches!(self, Value::Emotes(_)).then_some(self),
            SocketType::Event => None,
        }
    }

    fn text(&self) -> EmoteText {
        match self {
            Value::Number(number) if number.fract() == 0.0 && number.abs() < 1e15 => {
                EmoteText::plain_text(format!("{}", *number as i64))
            }
            Value::Number(number) => EmoteText::plain_text(format!("{number:.2}")),
            Value::Text(text) => text.clone(),
            Value::Emotes(emotes) => EmoteText::from_emotes(emotes),
        }
    }
}

/// What an event node knows about the event that matched it, by output.
pub type EventValues = BTreeMap<String, Value>;

/// What plugins' nodes gave the event: their value outputs' values (a flow
/// node's it went through, a value node's worked out for it), and the
/// value nodes worked out for it already (with or without values). Kept
/// in a [`Then`] as its `filled` and `computed`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Filled {
    /// By `"<node>/<output>"` (a string, so it keeps as JSON).
    #[serde(rename = "filled", default, skip_serializing_if = "BTreeMap::is_empty")]
    values: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    computed: BTreeSet<NodeId>,
}

impl Filled {
    /// Node `node` worked out for this event, giving `values` by output
    /// (none: the inputs wired from it fall back to their settings).
    fn computed(&mut self, node: NodeId, values: Vec<(String, Value)>) {
        self.computed.insert(node);
        for (output, value) in values {
            self.values.insert(filled_key(node, &output), value);
        }
    }
}

/// What a queued effect node's Started and Ended outputs carry on from:
/// its card and node, and the event that set it off. Kept with the run in
/// the queue (so across restarts too), only when one of them is wired.
/// The same for an event parked at a plugin's flow node, until its server
/// code says where it goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Then {
    /// The card's id.
    pub card: String,
    pub node: NodeId,
    /// The event node it matched, and what it knew.
    source: NodeId,
    values: EventValues,
    signal: Signal,
    #[serde(flatten)]
    filled: Filled,
    /// Waiting for plugins' value nodes: the nodes and inputs the event
    /// goes into once they're worked out, instead of on from `node`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub into: Vec<(NodeId, String)>,
}

impl Then {
    /// What its node's value outputs carry from here on (a flow node's
    /// answer), by output.
    pub fn fill(&mut self, values: Vec<(String, Value)>) {
        self.filled.computed(self.node, values);
    }

    /// Value node `node` worked out for this event, giving `values` by
    /// output (none: its inputs fall back to their settings).
    pub fn computed(&mut self, node: NodeId, values: Vec<(String, Value)>) {
        self.filled.computed(node, values);
    }

    /// The event that set it off.
    pub fn signal(&self) -> &Signal {
        &self.signal
    }

    fn activation<'t>(&'t self, effects: &'t Effects) -> Activation<'t> {
        Activation {
            source: self.source,
            values: self.values.clone(),
            signal: &self.signal,
            text_styles: effects.text_styles(),
            filled: self.filled.clone(),
        }
    }
}

#[cfg(test)]
impl Then {
    /// Carrying on from node `node` of card `card`, after `signal`.
    pub fn for_tests(card: &str, node: NodeId, signal: Signal) -> Then {
        Then {
            card: card.into(),
            node,
            source: 1,
            values: EventValues::new(),
            signal,
            filled: Filled::default(),
            into: Vec::new(),
        }
    }
}

/// A plugin's event node placed on a card, as its server code names it
/// (`placed-node.id`, `emit-to`).
pub fn placed_id(card: &str, node: NodeId) -> String {
    format!("{card}/{node}")
}

fn filled_key(node: NodeId, output: &str) -> String {
    format!("{node}/{output}")
}

/// One event flowing through a card.
struct Activation<'s> {
    /// The event node it matched.
    source: NodeId,
    values: EventValues,
    signal: &'s Signal,
    /// What texts typed into nodes can mark parts with.
    text_styles: crate::effects::TextStyles<'s>,
    filled: Filled,
}

impl Activation<'_> {
    /// Where it is now (at node `node` of card `card`), to carry on from
    /// later.
    fn then(&self, card: &str, node: NodeId) -> Then {
        Then {
            card: card.to_string(),
            node,
            source: self.source,
            values: self.values.clone(),
            signal: self.signal.clone(),
            filled: self.filled.clone(),
            into: Vec::new(),
        }
    }
}

/// A card, ready to run, with its flow nodes' state.
pub struct CardRuntime {
    card: Card,
    /// Whether it has a plugin's value node, which events may wait for.
    has_plugin_values: bool,
    cooldowns: HashMap<NodeId, Cooldown>,
    crowds: HashMap<NodeId, Crowds>,
    /// Gate nodes: open until this time.
    gates: HashMap<NodeId, u64>,
    /// Every Nth nodes: the events counted since the start or a reset.
    counts: HashMap<NodeId, u64>,
    /// Timer nodes: when each ticks next; `None` while stopped. A Timer
    /// gets its entry on the first tick the card is on.
    timers: HashMap<NodeId, Option<u64>>,
    /// Events Delay nodes hold, in the order they came.
    delayed: Vec<Delayed>,
}

/// How many events one Delay node holds at once; more are dropped (and
/// logged).
pub const MAX_DELAYED: usize = 100;

/// The event a Timer's Tick is, as its signal's name (it's never matched
/// against event nodes: it starts from the Timer).
const TIMER_EVENT: &str = "timer";

/// An event a Delay node holds until `due_ms`, as it came.
struct Delayed {
    node: NodeId,
    due_ms: u64,
    source: NodeId,
    values: EventValues,
    signal: Signal,
    filled: Filled,
    /// Where it came from, for what it queues once sent on.
    cause: Cause,
}

/// A combo or chant node's state: per emote code or word, when each viewer
/// (by id) last posted it.
type Crowds = HashMap<String, HashMap<String, u64>>;

#[derive(Default)]
struct Cooldown {
    last_ms: Option<u64>,
    per_viewer: HashMap<String, u64>,
}

impl CardRuntime {
    /// Only for a card without problems (`Card::problems`).
    pub fn new(card: Card) -> CardRuntime {
        let has_plugin_values = card.nodes.iter().any(|node| {
            matches!(node.kind, NodeKind::Plugin { .. }) && node.kind.category() == Category::Values
        });
        CardRuntime {
            card,
            has_plugin_values,
            cooldowns: HashMap::new(),
            crowds: HashMap::new(),
            gates: HashMap::new(),
            counts: HashMap::new(),
            timers: HashMap::new(),
            delayed: Vec::new(),
        }
    }

    /// Takes over what this card's Every Nth counts, Timers and Delays had
    /// as it ran before a config change (`old`, the same card), for the
    /// nodes it still has, of the same kind: editing a card (or another)
    /// doesn't start a count over. Timers of a card that's now off stop;
    /// so do the events its Delays held, but a test's. Cooldowns, gates and
    /// crowds start over.
    pub fn carry_over(&mut self, old: CardRuntime) {
        let card = &self.card;
        let same = |node: NodeId| match (card.node(node), old.card.node(node)) {
            (Some(new), Some(old)) => std::mem::discriminant(&new.kind) == std::mem::discriminant(&old.kind),
            _ => false,
        };
        let counts = old.counts.iter().filter(|(node, _)| same(**node)).map(|(node, count)| (*node, *count));
        let counts = counts.collect();
        let timers = match card.enabled {
            true => old
                .timers
                .iter()
                .filter(|(node, _)| same(**node))
                .map(|(node, next)| (*node, *next))
                .collect(),
            false => HashMap::new(),
        };
        let keep = |delayed: &Delayed| {
            same(delayed.node) && (card.enabled || delayed.cause.origin == ItemOrigin::Test)
        };
        let (delayed, _): (Vec<Delayed>, Vec<Delayed>) = old.delayed.into_iter().partition(keep);
        (self.counts, self.timers, self.delayed) = (counts, timers, delayed);
    }

    /// A copy to prepare a waiting event on: the card as it is, with its
    /// state starting empty (and thrown away after).
    pub fn copy(&self) -> CardRuntime {
        CardRuntime::new(self.card.clone())
    }

    pub fn name(&self) -> &str {
        &self.card.name
    }

    pub fn id(&self) -> &str {
        &self.card.id
    }

    /// Off cards only run for a test aimed at them.
    pub fn enabled(&self) -> bool {
        self.card.enabled
    }

    /// Runs the card for one signal. Returns whether an effect fired.
    pub fn on_signal(&mut self, signal: &Signal, ctx: &mut Ctx<'_>, effects: &Effects) -> bool {
        ctx.begin_run(&self.card.id, Start::Event);
        let fired = self.matched(signal, ctx, effects);
        ctx.end_run(Some(signal));
        fired
    }

    /// The signal into each event node that matches it.
    fn matched(&mut self, signal: &Signal, ctx: &mut Ctx<'_>, effects: &Effects) -> bool {
        let mut fired = false;
        let mut matched: Vec<(NodeId, EventValues)> = Vec::new();
        for node in &self.card.nodes {
            match node.kind {
                // What a crowd completes depends on what came before: as
                // it did before, while preparing.
                NodeKind::EmoteCombo { .. } | NodeKind::Chant { .. } => {
                    let (before, visit, kind) = ctx.recording.decide(node.id, &node.kind);
                    let events = match before {
                        Some(Decision::Crowds(events)) => events,
                        _ => crowd_events(node, self.crowds.entry(node.id).or_default(), signal, ctx.now_ms),
                    };
                    ctx.recording.decided(node.id, visit, kind, Decision::Crowds(events.clone()));
                    matched.extend(events.into_iter().map(|values| (node.id, values)));
                }
                // A plugin that's switched off fires nothing; one aimed at
                // a placed copy (`emit-to`) only fires that one.
                NodeKind::Plugin { ref plugin, .. } if effects.plugin_off(plugin) => {}
                NodeKind::Plugin { .. }
                    if matches!(&signal.kind, SignalKind::Plugin { to: Some(to), .. }
                        if *to != placed_id(&self.card.id, node.id)) => {}
                _ => matched.extend(match_event(&node.kind, signal).map(|values| (node.id, values))),
            }
        }
        // A plugin's event node may fire another of its event outputs.
        let output = match &signal.kind {
            SignalKind::Plugin { output: Some(output), .. } => output.as_str(),
            _ => "event",
        };
        for (source, values) in matched {
            let activation = Activation {
                source,
                values,
                signal,
                text_styles: effects.text_styles(),
                filled: Filled::default(),
            };
            fired |= self.follow(source, output, &activation, ctx, effects);
        }
        fired
    }

    /// A queued run of one of its effect nodes went on stage (`output`:
    /// Started) or has ended (Ended), or a plugin's flow node said where
    /// the event goes: the card carries on from there, as it is now, with
    /// the event that set it off. An event that waited for value nodes
    /// (`then.into`) goes into the nodes it was going into. Returns
    /// whether an effect fired.
    pub fn resume(&mut self, then: &Then, output: &str, ctx: &mut Ctx<'_>, effects: &Effects) -> bool {
        let start = Start::Then { then: Box::new(then.clone()), output: output.to_string() };
        ctx.begin_run(&self.card.id, start);
        let activation = then.activation(effects);
        let fired = if !then.into.is_empty() {
            self.deliver(then.into.clone(), &activation, ctx, effects)
        } else if self.card.node(then.node).is_none_or(|node| node.kind.output(output).is_none()) {
            // The card changed since: the node, or that output, is gone.
            false
        } else {
            self.follow(then.node, output, &activation, ctx, effects)
        };
        ctx.end_run(None);
        fired
    }

    /// The pipeline's tick (every ~250 ms): Delay nodes send on the events
    /// whose time has come, and, while the card is on, its Timers whose
    /// time has come tick. A Tick comes from `origin`'s channel (for random
    /// emotes). What fires is marked on its event's history entry.
    pub fn on_tick(&mut self, origin: &Origin, ctx: &mut Ctx<'_>, effects: &Effects) {
        let now = ctx.now_ms;
        let (due, waiting): (Vec<Delayed>, Vec<Delayed>) =
            std::mem::take(&mut self.delayed).into_iter().partition(|delayed| delayed.due_ms <= now);
        self.delayed = waiting;
        for delayed in due {
            ctx.cause = delayed.cause.clone();
            ctx.cause.card = self.card.name.clone();
            let activation = Activation {
                source: delayed.source,
                values: delayed.values,
                signal: &delayed.signal,
                text_styles: effects.text_styles(),
                filled: delayed.filled,
            };
            let start = Start::Then {
                then: Box::new(activation.then(&self.card.id, delayed.node)),
                output: "event".into(),
            };
            ctx.begin_run(&self.card.id, start);
            let fired = self.follow(delayed.node, "event", &activation, ctx, effects);
            ctx.end_run(None);
            if fired && let Some(entry) = ctx.cause.entry {
                ctx.out.fired(entry, &self.card.id, &self.card.name);
            }
        }
        if self.card.enabled {
            let timers: Vec<(NodeId, f64, bool)> = self
                .card
                .nodes
                .iter()
                .filter_map(|node| match node.kind {
                    NodeKind::Timer { minutes, from_start } => Some((node.id, minutes, from_start)),
                    _ => None,
                })
                .collect();
            for (node, minutes, from_start) in timers {
                let period = timer_ms(minutes);
                let next = self
                    .timers
                    .entry(node)
                    .or_insert_with(|| from_start.then_some(now.saturating_add(period)));
                let Some(at) = next else { continue };
                // Its time shortened since it was set: from now.
                *at = (*at).min(now.saturating_add(period));
                if *at > now {
                    continue;
                }
                // On time, or (after the computer slept) from now.
                *at = if at.saturating_add(period) > now {
                    at.saturating_add(period)
                } else {
                    now.saturating_add(period)
                };
                self.tick(node, origin, ItemOrigin::Live, ctx, effects);
            }
        }
        ctx.cause = Cause::default();
    }

    /// "Test this card": each of its Timers ticks once, now. Returns
    /// whether an effect fired.
    pub fn test_timers(&mut self, origin: &Origin, ctx: &mut Ctx<'_>, effects: &Effects) -> bool {
        let timers: Vec<NodeId> = self
            .card
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Timer { .. }))
            .map(|node| node.id)
            .collect();
        let mut fired = false;
        for node in timers {
            fired |= self.tick(node, origin, ItemOrigin::Test, ctx, effects);
        }
        ctx.cause = Cause::default();
        fired
    }

    /// A Timer's Tick: an event of its own, summed up as "Timer".
    fn tick(
        &mut self,
        node: NodeId,
        origin: &Origin,
        from: ItemOrigin,
        ctx: &mut Ctx<'_>,
        effects: &Effects,
    ) -> bool {
        let signal = Signal::new(
            origin.clone(),
            SignalKind::Custom { name: TIMER_EVENT.into(), data: Default::default() },
        );
        ctx.cause = Cause {
            event: Some("Timer".into()),
            origin: from,
            card: self.card.name.clone(),
            seed: ctx.rng.seed(),
            event_id: ctx.out.next_event(),
            // No other card hears a Timer's tick.
            hearers: Hearers::Own,
            ..Default::default()
        };
        let mut values = EventValues::new();
        values.insert("summary".into(), Value::Text("Timer".into()));
        let activation = Activation {
            source: node,
            values,
            signal: &signal,
            text_styles: effects.text_styles(),
            filled: Filled::default(),
        };
        let start =
            Start::Then { then: Box::new(activation.then(&self.card.id, node)), output: "event".into() };
        ctx.begin_run(&self.card.id, start);
        let fired = self.follow(node, "event", &activation, ctx, effects);
        ctx.end_run(None);
        fired
    }

    /// Forgets per-viewer cooldowns that have run out, and posts older than
    /// their combo's or chant's time.
    pub fn prune(&mut self, now_ms: u64) {
        for (node, crowds) in &mut self.crowds {
            let Some((_, seconds)) = self.card.node(*node).and_then(|node| crowd_settings(&node.kind)) else {
                continue;
            };
            for posts in crowds.values_mut() {
                posts.retain(|_, at| now_ms.saturating_sub(*at) < window_ms(seconds));
            }
            crowds.retain(|_, posts| !posts.is_empty());
        }
        for (node, cooldown) in &mut self.cooldowns {
            let Some(NodeKind::Cooldown { seconds, .. }) = self.card.node(*node).map(|node| &node.kind)
            else {
                continue;
            };
            let length = (seconds * 1000.0) as u64;
            cooldown.per_viewer.retain(|_, last| now_ms.saturating_sub(*last) < length);
        }
    }

    /// Sends the event out of `from`'s `socket` along every wire.
    fn follow(
        &mut self,
        from: NodeId,
        socket: &str,
        activation: &Activation<'_>,
        ctx: &mut Ctx<'_>,
        effects: &Effects,
    ) -> bool {
        let targets: Vec<(NodeId, String)> = self
            .card
            .wires
            .iter()
            .filter(|wire| wire.from.node == from && wire.from.socket == socket)
            .map(|wire| (wire.to.node, wire.to.socket.clone()))
            .collect();
        self.deliver(targets, activation, ctx, effects)
    }

    /// The event goes into each of `targets` (a node and its input). Those
    /// whose inputs need plugins' value nodes not worked out for this
    /// event wait for them ([`CardRuntime::compute`]).
    fn deliver(
        &mut self,
        targets: Vec<(NodeId, String)>,
        activation: &Activation<'_>,
        ctx: &mut Ctx<'_>,
        effects: &Effects,
    ) -> bool {
        let mut fired = false;
        let mut waiting = Vec::new();
        let mut needed = BTreeSet::new();
        // What's known about each value node, for every target at once.
        let mut missing = HashMap::new();
        for (to, input) in targets {
            if self.waits(to, activation, &mut needed, &mut missing) {
                waiting.push((to, input));
            } else {
                fired |= self.arrive(to, &input, activation, ctx, effects);
            }
        }
        if !waiting.is_empty() {
            fired |= self.compute(waiting, needed, activation, ctx, effects);
        }
        fired
    }

    /// Asks plugins' value nodes `needed` for their values, and parks the
    /// event until they've answered, to go into `waiting` then. Those of a
    /// plugin that's switched off give nothing at once.
    fn compute(
        &mut self,
        waiting: Vec<(NodeId, String)>,
        needed: BTreeSet<NodeId>,
        activation: &Activation<'_>,
        ctx: &mut Ctx<'_>,
        effects: &Effects,
    ) -> bool {
        let mut then = activation.then(&self.card.id, 0);
        then.into = waiting;
        let mut asks = Vec::new();
        for node in needed {
            let Some(NodeKind::Plugin { plugin, settings, ty: Attached(Some(ty)), .. }) =
                self.card.node(node).map(|node| node.kind.clone())
            else {
                continue;
            };
            if effects.plugin_off(&plugin) {
                then.computed(node, Vec::new());
                continue;
            }
            let values = self.plugin_values(node, &ty, &settings, activation, ctx);
            asks.push(crate::services::activity::ValueAsk { plugin, node, ty: ty.id.clone(), values });
        }
        if asks.is_empty() {
            let into = std::mem::take(&mut then.into);
            return self.deliver(into, &then.activation(effects), ctx, effects);
        }
        for ask in &asks {
            self.hold(ask.node, ctx);
        }
        if !ctx.out.compute(then, asks, &ctx.cause) {
            tracing::warn!(
                card = self.card.name,
                "{} events already wait for plugins' value nodes; this one was dropped",
                crate::services::activity::MAX_COMPUTING
            );
        }
        false
    }

    /// Whether an input of `node` needs plugins' value nodes not worked out
    /// for this event yet, adding to `needed` those that can be asked now
    /// (their own inputs need none still to be worked out): the deepest
    /// first. `missing` keeps what [`CardRuntime::missing`] found, by node.
    fn waits(
        &self,
        node: NodeId,
        activation: &Activation<'_>,
        needed: &mut BTreeSet<NodeId>,
        missing: &mut HashMap<NodeId, bool>,
    ) -> bool {
        if !self.has_plugin_values {
            return false;
        }
        let mut waits = false;
        for wire in self.card.wires.iter().filter(|wire| wire.to.node == node) {
            waits |= self.missing(wire.from.node, activation, needed, missing);
        }
        waits
    }

    /// Whether value node `node`'s outputs aren't ready for this event
    /// (it's a plugin's not worked out yet, or one feeding it is), adding
    /// to `needed` the ones to ask now. Each node is looked at once
    /// (`missing`), however many ways lead to it.
    fn missing(
        &self,
        node: NodeId,
        activation: &Activation<'_>,
        needed: &mut BTreeSet<NodeId>,
        missing: &mut HashMap<NodeId, bool>,
    ) -> bool {
        if let Some(known) = missing.get(&node) {
            return *known;
        }
        let Some(kind) = self.card.node(node).map(|node| &node.kind) else { return false };
        if kind.category() != Category::Values || activation.filled.computed.contains(&node) {
            missing.insert(node, false);
            return false;
        }
        let mut upstream = false;
        for wire in self.card.wires.iter().filter(|wire| wire.to.node == node) {
            upstream |= self.missing(wire.from.node, activation, needed, missing);
        }
        let found = if matches!(kind, NodeKind::Plugin { .. }) {
            if !upstream {
                needed.insert(node);
            }
            true
        } else {
            upstream
        };
        missing.insert(node, found);
        found
    }

    /// Node `node`, which keeps state, decides for this event: as it did
    /// before (while preparing, with its settings unchanged), else worked
    /// out now (`work_out`); recorded either way.
    fn decide(
        &mut self,
        node: NodeId,
        ctx: &mut Ctx<'_>,
        work_out: impl FnOnce(&mut Self, &mut Ctx<'_>) -> Decision,
    ) -> Decision {
        let Some(kind) = self.card.node(node).map(|node| &node.kind) else { return work_out(self, ctx) };
        let (before, visit, kind) = ctx.recording.decide(node, kind);
        let decision = match before {
            Some(decision) => decision,
            None => work_out(self, ctx),
        };
        ctx.recording.decided(node, visit, kind, decision.clone());
        decision
    }

    /// The event waits at node `node` (a Delay, a plugin's flow or value
    /// node): what comes after it is a run of its own, later. While
    /// preparing, one it didn't wait at before leaves the run as it was.
    fn hold(&mut self, node: NodeId, ctx: &mut Ctx<'_>) {
        self.decide(node, ctx, |_, ctx| {
            if ctx.out.preparing_again() {
                ctx.recording.unpreparable();
            }
            Decision::Held
        });
    }

    /// The event comes into `to`'s `input`.
    fn arrive(
        &mut self,
        to: NodeId,
        input: &str,
        activation: &Activation<'_>,
        ctx: &mut Ctx<'_>,
        effects: &Effects,
    ) -> bool {
        let input = input.to_string();
        let mut fired = false;
        let Some(kind) = self.card.node(to).map(|node| node.kind.clone()) else { return false };
        match kind {
            NodeKind::Chance { percent } => {
                let out =
                    if ctx.rng_at(&self.card.id, to).next_f64() * 100.0 < percent { "event" } else { "miss" };
                fired |= self.follow(to, out, activation, ctx, effects);
            }
            NodeKind::Rarity { tiers, .. } => {
                // Exactly one way out: the tiers share 0–100 in order,
                // whatever is left over is the rest.
                let roll = ctx.rng_at(&self.card.id, to).next_f64() * 100.0;
                let mut total = 0.0;
                let out = tiers
                    .iter()
                    .find(|tier| {
                        total += tier.percent;
                        roll < total
                    })
                    .map_or_else(|| "rest".to_string(), cards::Tier::socket);
                fired |= self.follow(to, &out, activation, ctx, effects);
            }
            NodeKind::Gate { seconds } if input == "open" => {
                self.gates.insert(to, ctx.now_ms.saturating_add(window_ms(seconds)));
            }
            NodeKind::ViewerRoles { broadcaster, moderator, vip, subscriber } => {
                let has_one = activation.signal.user().is_some_and(|user| {
                    let roles = &user.roles;
                    (broadcaster && roles.broadcaster)
                        || (moderator && roles.moderator)
                        || (vip && roles.vip)
                        || (subscriber && roles.member)
                });
                fired |= self.follow(to, if has_one { "event" } else { "miss" }, activation, ctx, effects);
            }
            NodeKind::HasText { .. } => {
                let text = self.text_input(to, "text", activation, ctx);
                let out = if text.is_blank() { "miss" } else { "event" };
                fired |= self.follow(to, out, activation, ctx, effects);
            }
            NodeKind::CompareText { .. } => {
                let out = if self.compare_text(to, activation, ctx).is_some() { "event" } else { "miss" };
                fired |= self.follow(to, out, activation, ctx, effects);
            }
            NodeKind::Gate { .. } => {
                let open = self.decide(to, ctx, |this, ctx| {
                    Decision::Passed(this.gates.get(&to).is_some_and(|until| ctx.now_ms < *until))
                });
                if open == Decision::Passed(true) {
                    fired |= self.follow(to, "event", activation, ctx, effects);
                }
            }
            NodeKind::CompareNumber { .. } => {
                let out = if self.compare_number(to, activation, ctx) { "event" } else { "miss" };
                fired |= self.follow(to, out, activation, ctx, effects);
            }
            // Held, with what the event had, until the tick after its
            // time.
            NodeKind::Delay { .. } => {
                self.hold(to, ctx);
                let seconds = self.number_input(to, "seconds", activation, ctx).unwrap_or(0.0);
                let seconds = if seconds.is_finite() { seconds } else { 0.0 };
                if ctx.out.preparing_again() {
                    // (What comes after it is a run of its own.)
                } else if self.delayed.iter().filter(|delayed| delayed.node == to).count() >= MAX_DELAYED {
                    tracing::warn!(
                        card = self.card.name,
                        "a Delay node already holds {MAX_DELAYED} events; this one was dropped"
                    );
                } else {
                    self.delayed.push(Delayed {
                        node: to,
                        due_ms: ctx.now_ms.saturating_add(window_ms(seconds)),
                        source: activation.source,
                        values: activation.values.clone(),
                        signal: activation.signal.clone(),
                        filled: activation.filled.clone(),
                        cause: ctx.cause.clone(),
                    });
                }
            }
            NodeKind::EveryNth { .. } if input == "reset" => {
                self.counts.remove(&to);
            }
            NodeKind::EveryNth { every } => {
                let counted = self.decide(to, ctx, |this, _| {
                    let count = this.counts.entry(to).or_default();
                    *count += 1;
                    Decision::Counted(*count)
                });
                // Its count from here on (its output), as it was then.
                let count = match counted {
                    Decision::Counted(count) => count,
                    _ => self.counts.get(&to).copied().unwrap_or_default(),
                };
                self.counts.insert(to, count);
                let every = every.round().max(1.0) as u64;
                let out = if count.is_multiple_of(every) { "event" } else { "miss" };
                fired |= self.follow(to, out, activation, ctx, effects);
            }
            // Start (again: from now) or Stop.
            NodeKind::Timer { minutes, .. } => {
                let next = (input == "start").then(|| ctx.now_ms.saturating_add(timer_ms(minutes)));
                self.timers.insert(to, next);
            }
            // A replay was asked for: it passes, and doesn't start the
            // cooldown.
            NodeKind::Cooldown { seconds, per_viewer } => {
                let passed = self.decide(to, ctx, |this, ctx| {
                    Decision::Passed(
                        ctx.replay() || this.cooldown_passes(to, seconds, per_viewer, activation, ctx.now_ms),
                    )
                });
                if passed == Decision::Passed(true) {
                    fired |= self.follow(to, "event", activation, ctx, effects);
                }
            }
            NodeKind::Plugin { plugin, settings, changes, voice, ty: Attached(Some(ty)), .. }
                if ty.category == Category::Effects
                    && ty
                        .inputs
                        .iter()
                        .any(|socket| socket.name == input && socket.ty == SocketType::Event) =>
            {
                let mut values = self.plugin_values(to, &ty, &settings, activation, ctx);
                if ty.viewer {
                    let viewer = crate::effects::run_viewer(activation.signal, ctx.emotes);
                    values.insert(
                        protocol::plugin::VIEWER_VALUE.into(),
                        serde_json::to_value(viewer).expect("viewers serialise"),
                    );
                }
                // Only the ones its type lets it change, read as the
                // plugin's own settings are.
                let mut rng = ctx.rng_at(&self.card.id, to);
                let media = ctx.media;
                let mut pick = |kind, folder: &str, every| media.pick_files(kind, folder, every, &mut *rng);
                let changes = ty
                    .changeable
                    .iter()
                    .filter_map(|setting| {
                        let stored = changes.get(&setting.key)?;
                        let value = protocol::plugin::shared_value(setting, Some(stored), &mut pick);
                        Some((setting.key.clone(), value))
                    })
                    .collect();
                // Its Started or Ended (while it waits in the queue),
                // or one of its own outputs (its server code fires
                // them), wired: what to carry on from.
                let queued = ty.queued(&settings);
                let carries_on = self.card.wires.iter().any(|wire| {
                    let output = wire.from.socket.as_str();
                    wire.from.node == to
                        && ((queued && [STARTED_OUTPUT, ENDED_OUTPUT].contains(&output))
                            || ty.outputs.iter().any(|own| own.name == output))
                });
                let then = carries_on.then(|| activation.then(&self.card.id, to));
                if let Some(play) =
                    effects.plugin(&plugin, &ty, &input, values, changes, &voice, then, &mut *rng, ctx)
                {
                    ctx.recording.queue(to, play);
                }
                fired = true;
            }
            // Parked while its server code says where it goes (a
            // switched-off plugin's lets nothing through).
            NodeKind::Plugin { plugin, settings, ty: Attached(Some(ty)), .. }
                if ty.category == Category::Flow
                    && ty
                        .inputs
                        .iter()
                        .any(|socket| socket.name == input && socket.ty == SocketType::Event)
                    && !effects.plugin_off(&plugin) =>
            {
                self.hold(to, ctx);
                let values = self.plugin_values(to, &ty, &settings, activation, ctx);
                let viewer = crate::effects::run_viewer(activation.signal, ctx.emotes);
                let then = activation.then(&self.card.id, to);
                if !ctx.out.route(&plugin, &ty.id, &input, values, viewer, then, &ctx.cause) {
                    tracing::debug!(plugin, card = self.card.name, "a flow node's server code isn't running");
                }
            }
            _ => {}
        }
        fired
    }

    /// What a plugin's effect node sends its page: every input that isn't
    /// an event (wired, or its setting), then the other settings, by name.
    fn plugin_values(
        &self,
        node: NodeId,
        ty: &NodeType,
        settings: &serde_json::Map<String, serde_json::Value>,
        activation: &Activation<'_>,
        ctx: &mut Ctx<'_>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut values = serde_json::Map::new();
        let wired = |name: &str| self.card.wire_into(node, name).is_some();
        for input in &ty.inputs {
            let is_wired = wired(&input.name);
            // A setting standing in for it that isn't shown now isn't sent.
            if !is_wired && ty.setting(&input.name).is_some_and(|setting| !ty.shown(setting, wired, settings))
            {
                continue;
            }
            let value = match input.ty {
                SocketType::Number => self.number_input(node, &input.name, activation, ctx).map(|number| {
                    let number = ty.setting(&input.name).map_or(number, |setting| setting.clamp(number));
                    serde_json::Value::from(number)
                }),
                SocketType::Text if is_wired || ty.input_setting(&input.name, SocketType::Text).is_some() => {
                    Some(plugins::text_value(&self.text_input(node, &input.name, activation, ctx)))
                }
                SocketType::Emotes => match self.wired(node, &input.name, activation, ctx) {
                    Some(Value::Emotes(emotes)) => Some(plugins::emotes_value(&emotes)),
                    Some(_) => None,
                    None => ty
                        .input_setting(&input.name, SocketType::Emotes)
                        .map(|setting| self.setting_emotes(node, ty, setting, settings, activation, ctx)),
                },
                SocketType::Text | SocketType::Event => None,
            };
            if let Some(value) = value {
                values.insert(input.name.clone(), value);
            }
        }
        let mut rng = ctx.rng_at(&self.card.id, node);
        for setting in ty.own_settings().filter(|setting| ty.shown(setting, wired, settings)) {
            let value = setting_value(setting, settings.get(&setting.key), activation, ctx, &mut *rng);
            values.insert(setting.key.clone(), value);
        }
        values
    }

    /// An emotes setting's emotes: random ones from the channel, or the
    /// listed codes the channel knows; with its count, that many picked at
    /// random from them.
    fn setting_emotes(
        &self,
        node: NodeId,
        ty: &NodeType,
        setting: &protocol::plugin::Setting,
        settings: &serde_json::Map<String, serde_json::Value>,
        activation: &Activation<'_>,
        ctx: &mut Ctx<'_>,
    ) -> serde_json::Value {
        let SettingKind::Emotes { count, .. } = &setting.kind else {
            return serde_json::Value::Array(Vec::new());
        };
        let count = count.as_ref().map(|name| {
            let number = if ty.inputs.iter().any(|input| input.name == *name) {
                self.number_input(node, name, activation, ctx)
            } else {
                ty.setting(name).and_then(|setting| setting.read(settings.get(name)).as_f64())
            };
            let number = number.filter(|number| number.is_finite()).unwrap_or(0.0);
            number.clamp(0.0, f64::from(protocol::plugin::MAX_SETTING_EMOTES)) as u32
        });
        let mut rng = ctx.rng_at(&self.card.id, node);
        let origin = &activation.signal.origin;
        let stored = setting.read(settings.get(&setting.key));
        let picked = match stored.as_array() {
            None => {
                let picked =
                    ctx.emotes.random(&origin.platform, &origin.channel.id, count.unwrap_or(1), &mut *rng);
                if picked.is_empty() && count != Some(0) && !ctx.out.preparing_again() {
                    tracing::info!(
                        channel = %origin.channel.name,
                        "a node wanted random emotes, but no emote sets have loaded for this channel yet"
                    );
                }
                picked
            }
            Some(codes) => {
                let mut known = Vec::new();
                let mut unknown = Vec::new();
                for code in
                    codes.iter().filter_map(serde_json::Value::as_str).filter(|code| !code.trim().is_empty())
                {
                    match ctx.emotes.lookup_any_code(&origin.platform, &origin.channel.id, code.trim()) {
                        Some(info) => known.push(info),
                        None => unknown.push(code),
                    }
                }
                if !unknown.is_empty() && !ctx.out.preparing_again() {
                    tracing::info!(
                        channel = %origin.channel.name,
                        "skipped emote codes this channel doesn't know: {} (codes are case-sensitive; \
                         Twitch's own emotes need the Twitch login, or to be seen in chat first)",
                        unknown.join(", ")
                    );
                }
                match count {
                    Some(_) if known.is_empty() => known,
                    Some(count) => (0..count).map(|_| known[rng.index(known.len())].clone()).collect(),
                    None => known,
                }
            }
        };
        plugins::emote_infos_value(&picked)
    }

    fn cooldown_passes(
        &mut self,
        node: NodeId,
        seconds: f64,
        per_viewer: bool,
        activation: &Activation<'_>,
        now_ms: u64,
    ) -> bool {
        let length = (seconds.max(0.0) * 1000.0) as u64;
        let state = self.cooldowns.entry(node).or_default();
        let viewer = activation.signal.user().map(|user| user.id.clone());
        let last = match (per_viewer, &viewer) {
            (true, Some(viewer)) => state.per_viewer.get(viewer).copied(),
            _ => state.last_ms,
        };
        if last.is_some_and(|last| now_ms.saturating_sub(last) < length) {
            return false;
        }
        match (per_viewer, viewer) {
            (true, Some(viewer)) => {
                state.per_viewer.insert(viewer, now_ms);
            }
            _ => state.last_ms = Some(now_ms),
        }
        true
    }

    /// What the wire into an input carries for this event, if anything.
    fn wired(
        &self,
        node: NodeId,
        socket: &str,
        activation: &Activation<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Option<Value> {
        let wire = self.card.wire_into(node, socket)?;
        self.output(wire.from.node, &wire.from.socket, activation, ctx)
    }

    /// An input's value: from its wire, else (nothing wired, or the wire
    /// carries nothing for this event) from the node's own setting.
    fn input(
        &self,
        node: NodeId,
        socket: &str,
        activation: &Activation<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Option<Value> {
        if let Some(value) = self.wired(node, socket, activation, ctx) {
            return Some(value);
        }
        let kind = &self.card.node(node)?.kind;
        if let Some(number) = kind.number_setting(socket) {
            return Some(Value::Number(number));
        }
        kind.text_setting(socket).map(|text| Value::Text(EmoteText::plain_text(text)))
    }

    fn number_input(
        &self,
        node: NodeId,
        socket: &str,
        activation: &Activation<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Option<f64> {
        self.input(node, socket, activation, ctx).and_then(|value| value.number())
    }

    /// A text input: what's wired into it, or its setting as a template
    /// with `{name}` filled from the event and `{e:code}` emotes.
    fn text_input(
        &self,
        node: NodeId,
        socket: &str,
        activation: &Activation<'_>,
        ctx: &mut Ctx<'_>,
    ) -> EmoteText {
        if let Some(value) = self.wired(node, socket, activation, ctx) {
            return value.text();
        }
        let template =
            self.card.node(node).and_then(|node| node.kind.text_setting(socket)).unwrap_or_default();
        fill(template, |name| activation.values.get(name).map(Value::text), activation, ctx)
    }

    /// A Compare text node's verdict for this event: the text it matched.
    fn compare_text(&self, node: NodeId, activation: &Activation<'_>, ctx: &mut Ctx<'_>) -> Option<String> {
        let text = self.text_input(node, "text", activation, ctx).plain();
        let wired = self.card.wire_into(node, "to").map(|_| {
            self.input(node, "to", activation, ctx).map(|value| value.text().plain()).unwrap_or_default()
        });
        self.card.node(node)?.kind.compare_text(&text, wired.as_deref())
    }

    /// A Compare number node's verdict for this event; a value that isn't a
    /// number misses.
    fn compare_number(&self, node: NodeId, activation: &Activation<'_>, ctx: &mut Ctx<'_>) -> bool {
        let Some(NodeKind::CompareNumber { test, .. }) = self.card.node(node).map(|node| &node.kind) else {
            return false;
        };
        let number = |name: &str, ctx: &mut Ctx<'_>| self.number_input(node, name, activation, ctx);
        let (Some(value), Some(than)) = (number("value", ctx), number("number", ctx)) else { return false };
        let and = match test {
            NumberTest::Between => number("and", ctx).unwrap_or(than),
            _ => than,
        };
        test.passes(value, than, and)
    }

    /// What an output carries for this event. An event node's outputs only
    /// have values for the event node that matched; other event nodes in
    /// the card give nothing, so inputs fall back to their settings.
    fn output(
        &self,
        node: NodeId,
        socket: &str,
        activation: &Activation<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Option<Value> {
        let kind = self.card.node(node)?.kind.clone();
        if kind.is_event() {
            return if node == activation.source { activation.values.get(socket).cloned() } else { None };
        }
        // A plugin's flow node: what it gave this event, if it went
        // through it.
        if let NodeKind::Plugin { .. } = kind {
            return activation.filled.values.get(&filled_key(node, socket)).cloned();
        }
        let number =
            |this: &Self, name: &str, ctx: &mut Ctx<'_>| this.number_input(node, name, activation, ctx);
        match kind {
            NodeKind::Number { value } => Some(Value::Number(value)),
            NodeKind::Math { op, .. } => {
                let (a, b) = (number(self, "a", ctx)?, number(self, "b", ctx)?);
                Some(Value::Number(op.apply(a, b)))
            }
            NodeKind::Clamp { .. } => {
                let (value, min, max) =
                    (number(self, "value", ctx)?, number(self, "min", ctx)?, number(self, "max", ctx)?);
                Some(Value::Number(value.clamp(min, max.max(min))))
            }
            NodeKind::Random { .. } => {
                let (min, max) = (number(self, "min", ctx)?, number(self, "max", ctx)?);
                Some(Value::Number(min + ctx.rng_at(&self.card.id, node).next_f64() * (max - min)))
            }
            NodeKind::Text { template, .. } => {
                let mut filled = BTreeMap::new();
                for name in cards::placeholders(&template) {
                    let value = match self.card.wire_into(node, &name) {
                        Some(_) => self.input(node, &name, activation, ctx).map(|value| value.text()),
                        None => activation.values.get(&name).map(Value::text),
                    };
                    if let Some(value) = value {
                        filled.insert(name, value);
                    }
                }
                Some(Value::Text(fill(&template, |name| filled.get(name).cloned(), activation, ctx)))
            }
            NodeKind::EmotesInText => {
                let text = self.text_input(node, "text", activation, ctx).plain();
                let emotes = find_emotes_in_text(&text, &activation.signal.origin, ctx.emotes);
                match socket {
                    "count" => Some(Value::Number(emotes.len() as f64)),
                    _ => Some(Value::Emotes(emotes)),
                }
            }
            NodeKind::CompareText { .. } => {
                self.compare_text(node, activation, ctx).map(|matched| Value::Text(matched.into()))
            }
            NodeKind::EveryNth { .. } => {
                Some(Value::Number(self.counts.get(&node).copied().unwrap_or_default() as f64))
            }
            _ => None,
        }
    }
}

/// Fills a template typed into a node, with emotes the event's channel can
/// use and the text styles.
fn fill(
    template: &str,
    value: impl Fn(&str) -> Option<EmoteText>,
    activation: &Activation<'_>,
    ctx: &Ctx<'_>,
) -> EmoteText {
    let origin = &activation.signal.origin;
    EmoteText::fill(
        template,
        value,
        |code| ctx.emotes.lookup_any_code(&origin.platform, &origin.channel.id, code),
        |name| activation.text_styles.mark(name),
    )
}

/// A combo's or chant's viewers and seconds.
fn crowd_settings(kind: &NodeKind) -> Option<(f64, f64)> {
    match kind {
        NodeKind::EmoteCombo { viewers, seconds } | NodeKind::Chant { viewers, seconds, .. } => {
            Some((*viewers, *seconds))
        }
        _ => None,
    }
}

fn window_ms(seconds: f64) -> u64 {
    (seconds.max(0.0) * 1000.0) as u64
}

/// A Timer's time between ticks: at least a second.
fn timer_ms(minutes: f64) -> u64 {
    (minutes * 60_000.0).max(1000.0) as u64
}

/// The events a combo or chant node fires for `signal`: one for each emote
/// (or word) whose crowd it completes. A crowd that fires starts afresh.
fn crowd_events(node: &Node, crowds: &mut Crowds, signal: &Signal, now_ms: u64) -> Vec<EventValues> {
    let Some((viewers, seconds)) = crowd_settings(&node.kind) else { return Vec::new() };
    // What the message adds to: each different emote / listed word once,
    // with the value the event carries for it.
    let (user, keys): (&User, Vec<(String, Value)>) = match (&node.kind, &signal.kind) {
        (NodeKind::EmoteCombo { .. }, SignalKind::EmotesPosted { user, emotes }) => {
            let mut keys: Vec<(String, Value)> = Vec::new();
            for posted in emotes {
                if !keys.iter().any(|(code, _)| *code == posted.emote.code) {
                    keys.push((posted.emote.code.clone(), Value::Emotes(vec![posted.clone()])));
                }
            }
            (user, keys)
        }
        (NodeKind::Chant { words, .. }, SignalKind::ChatMessage { user, text, .. }) => (
            user,
            words_in(&text.plain(), words)
                .into_iter()
                .map(|word| (word.clone(), Value::Text(word.into())))
                .collect(),
        ),
        _ => return Vec::new(),
    };
    let needed = viewers.ceil().max(1.0) as usize;
    let mut events = Vec::new();
    for (key, value) in keys {
        let posts = crowds.entry(key.clone()).or_default();
        posts.retain(|_, at| now_ms.saturating_sub(*at) < window_ms(seconds));
        posts.insert(user.id.clone(), now_ms);
        if posts.len() < needed {
            continue;
        }
        let count = posts.len();
        posts.clear();
        let mut values = EventValues::new();
        values.insert("viewers".into(), Value::Number(count as f64));
        let summary = match node.kind {
            NodeKind::EmoteCombo { .. } => {
                values.insert("emote".into(), value);
                format!("{key} combo from {count} viewers")
            }
            _ => {
                values.insert("word".into(), value);
                format!("{count} viewers chant {key}")
            }
        };
        values.insert("summary".into(), Value::Text(summary.into()));
        events.push(values);
    }
    events
}

/// The built-in event node an event is for, as plugins that hear events
/// get it (`on-event`): its id and what its outputs carry. Not chat
/// messages (heard as chat), nor plugins' events.
pub fn heard_event(signal: &Signal) -> Option<(&'static str, EventValues)> {
    let kinds = [
        ("bits", NodeKind::Bits { min: 0.0 }),
        (
            "sub",
            NodeKind::Sub {
                new: true,
                resub: true,
                gifted: true,
                in_gift_bomb: true,
                prime_to_paid: true,
                gift_continued: true,
            },
        ),
        ("gift_bomb", NodeKind::GiftBomb),
        ("raid", NodeKind::Raid { min_viewers: 0.0 }),
        ("reward", NodeKind::Reward { reward: String::new() }),
        ("follow", NodeKind::Follow),
        (
            "hype_train",
            NodeKind::HypeTrain { started: true, levelled_up: true, progressed: true, ended: true },
        ),
        ("custom", NodeKind::Custom { name: String::new() }),
    ];
    kinds.into_iter().find_map(|(id, kind)| match_event(&kind, signal).map(|values| (id, values)))
}

/// The values an event node offers, if `signal` is an event it reacts to.
pub fn match_event(kind: &NodeKind, signal: &Signal) -> Option<EventValues> {
    let mut values = EventValues::new();
    let mut text = |name: &str, value: EmoteText| {
        values.insert(name.to_string(), Value::Text(value));
    };
    // "Viewer: " and then their message, emotes and all.
    let said = |before: String, message: &EmoteText| {
        let mut summary = EmoteText::plain_text(before);
        summary.push(message.clone());
        summary
    };
    let summary = match (kind, &signal.kind) {
        (NodeKind::EmotesInChat, SignalKind::EmotesPosted { user, emotes }) => {
            text("viewer", name(user).into());
            let count = emotes.len();
            values.insert("emotes".into(), Value::Emotes(emotes.clone()));
            values.insert("count".into(), Value::Number(count as f64));
            format!("{} posted {count} emotes", name(user)).into()
        }
        (NodeKind::ChatMessage, SignalKind::ChatMessage { user, text: message, .. }) => {
            text("viewer", name(user).into());
            text("message", message.clone());
            said(format!("{}: ", name(user)), message)
        }
        (NodeKind::FirstChatter, SignalKind::ChatMessage { user, text: message, first: true }) => {
            text("viewer", name(user).into());
            text("message", message.clone());
            format!("{} chatted for the first time", name(user)).into()
        }
        (NodeKind::ChatWords { words }, SignalKind::ChatMessage { user, text: message, .. }) => {
            let word = first_word_in(&message.plain(), words)?;
            text("viewer", name(user).into());
            text("word", word.clone().into());
            text("message", message.clone());
            format!("{} said {word}", name(user)).into()
        }
        (NodeKind::ChatCommand { command }, SignalKind::ChatMessage { user, text: message, .. }) => {
            let rest = message.after_command(command)?;
            text("viewer", name(user).into());
            text("text", rest.clone());
            text("message", message.clone());
            said(format!("{}: ", name(user)), &rest)
        }
        (NodeKind::Bits { min }, SignalKind::Support { user, amount, message }) => {
            if amount.unit() != "bits" || (amount.value() as f64) < *min {
                return None;
            }
            text("viewer", name(user).into());
            text("message", message.clone().unwrap_or_default());
            values.insert("amount".into(), Value::Number(amount.value() as f64));
            format!("{} cheered {}", name(user), amount.display()).into()
        }
        (
            NodeKind::Sub { new, resub, gifted, in_gift_bomb, prime_to_paid, gift_continued },
            SignalKind::Membership { user, kind, tier, message },
        ) => {
            let wanted = match kind {
                MembershipKind::New => *new,
                MembershipKind::Renewal { .. } => *resub,
                MembershipKind::Gifted { batch_id, .. } => *gifted && (batch_id.is_none() || *in_gift_bomb),
                MembershipKind::Upgraded { from: UpgradedFrom::Prime } => *prime_to_paid,
                MembershipKind::Upgraded { from: UpgradedFrom::Gift { .. } } => *gift_continued,
            };
            if !wanted {
                return None;
            }
            text("viewer", name(user).into());
            text("tier", tier.clone().unwrap_or_default().into());
            text("message", message.clone().unwrap_or_default());
            let months = match kind {
                MembershipKind::Renewal { months } => *months,
                _ => 1,
            };
            values.insert("months".into(), Value::Number(f64::from(months)));
            let summary = match kind {
                MembershipKind::New => format!("{} subscribed", name(user)),
                MembershipKind::Renewal { months } => {
                    format!("{} resubscribed for {months} months", name(user))
                }
                MembershipKind::Gifted { gifter, .. } => {
                    let gifter = gifter_name(gifter);
                    values.insert("gifter".into(), Value::Text(gifter.clone().into()));
                    format!("{gifter} gifted a sub to {}", name(user))
                }
                MembershipKind::Upgraded { from: UpgradedFrom::Prime } => {
                    format!("{} switched from Prime to a paid sub", name(user))
                }
                MembershipKind::Upgraded { from: UpgradedFrom::Gift { gifter } } => {
                    let gifter = gifter_name(gifter);
                    values.insert("gifter".into(), Value::Text(gifter.clone().into()));
                    format!("{} is continuing the gift sub from {gifter}", name(user))
                }
            };
            summary.into()
        }
        (NodeKind::GiftBomb, SignalKind::GiftBatch { gifter, count, tier }) => {
            let gifter = gifter_name(gifter);
            text("gifter", gifter.clone().into());
            text("tier", tier.clone().unwrap_or_default().into());
            values.insert("count".into(), Value::Number(f64::from(*count)));
            let subs = if *count == 1 { "sub" } else { "subs" };
            format!("{gifter} gifted {count} {subs}").into()
        }
        (NodeKind::Raid { min_viewers }, SignalKind::Raid { from, viewers }) => {
            if f64::from(*viewers) < *min_viewers {
                return None;
            }
            text("raider", name(from).into());
            values.insert("viewers".into(), Value::Number(f64::from(*viewers)));
            format!("{} is raiding with {viewers} viewers", name(from)).into()
        }
        (NodeKind::Reward { reward: wanted }, SignalKind::RewardRedeemed { user, reward, input }) => {
            let title = reward.title.clone().unwrap_or_else(|| "a reward".to_string());
            let wanted = wanted.trim();
            if !wanted.is_empty() && !wanted.eq_ignore_ascii_case(&title) && wanted != reward.id {
                return None;
            }
            let input = input.clone().unwrap_or_default();
            text("viewer", name(user).into());
            text("reward", title.clone().into());
            text("text", input.clone());
            if input.is_blank() {
                format!("{} redeemed {title}", name(user)).into()
            } else {
                said(format!("{} redeemed {title}: ", name(user)), &input)
            }
        }
        (NodeKind::Follow, SignalKind::Follow { user }) => {
            text("viewer", name(user).into());
            format!("{} followed", name(user)).into()
        }
        (
            NodeKind::HypeTrain { started, levelled_up, progressed, ended },
            SignalKind::HypeTrain { change, train },
        ) => {
            let wanted = match change {
                HypeTrainChange::Started => *started,
                HypeTrainChange::Progressed { level_up } => *progressed || (*level_up && *levelled_up),
                HypeTrainChange::Ended => *ended,
            };
            if !wanted {
                return None;
            }
            text("train", train.name().into());
            // Not a socket: for texts (`{kind}`), such as the bar's class.
            text("kind", train.kind.clone().into());
            values.insert("level".into(), Value::Number(f64::from(train.level)));
            values.insert("progress".into(), Value::Number(train.progress as f64));
            values.insert("goal".into(), Value::Number(train.goal as f64));
            values.insert("total".into(), Value::Number(train.total as f64));
            values.insert("seconds_left".into(), Value::Number(f64::from(train.seconds_left)));
            train.summary(*change).into()
        }
        (NodeKind::Custom { name: wanted }, SignalKind::Custom { name: event, data }) => {
            if !wanted.trim().is_empty() && wanted.trim() != event {
                return None;
            }
            // The event's data (e.g. `?colour=red` on the control API) for
            // the card's texts: `{colour}`. Not sockets, as every event
            // brings its own; the node's own outputs win.
            for (field, value) in data {
                let value = match value {
                    serde_json::Value::String(text) => text.clone(),
                    serde_json::Value::Number(number) => number.to_string(),
                    serde_json::Value::Bool(yes) => yes.to_string(),
                    _ => continue,
                };
                if !matches!(field.as_str(), "event" | "name" | "summary")
                    && cards::placeholders(&format!("{{{field}}}")).len() == 1
                {
                    text(field, value.into());
                }
            }
            text("name", event.clone().into());
            event.clone().into()
        }
        (
            NodeKind::Plugin { plugin, node, settings, ty: Attached(Some(ty)), .. },
            SignalKind::Plugin { plugin: from, node: fired, values: sent, .. },
        ) => {
            if plugin != from || node != fired {
                return None;
            }
            // Its filters: each lets through only the events whose output
            // of the same name is one of its values (none: any). Its other
            // settings are for its server code.
            for setting in ty.settings.iter().filter(|setting| ty.is_filter(setting)) {
                let wanted = protocol::plugin::filter_values(&setting.read(settings.get(&setting.key)));
                let value = match sent.get(&setting.key) {
                    Some(PluginValue::Text(text)) => text.plain(),
                    Some(PluginValue::Number(number)) => number.to_string(),
                    _ => String::new(),
                };
                if !protocol::plugin::passes_filter(&wanted, &value) {
                    return None;
                }
            }
            // Each output gets what was sent under its name, as its type.
            for output in &ty.outputs {
                let Some(value) = sent.get(&output.name) else { continue };
                let value = match value {
                    PluginValue::Number(number) => Value::Number(*number),
                    PluginValue::Text(text) => Value::Text(text.clone()),
                    PluginValue::Emotes(emotes) => Value::Emotes(emotes.clone()),
                };
                if let Some(value) = value.into_type(output.ty) {
                    values.insert(output.name.clone(), value);
                }
            }
            match values.get("summary") {
                Some(summary) => summary.text(),
                None => ty.title.clone().into(),
            }
        }
        _ => return None,
    };
    values.insert("summary".into(), Value::Text(summary));
    Some(values)
}

pub(crate) fn name(user: &User) -> String {
    if user.display_name.is_empty() { user.name.clone() } else { user.display_name.clone() }
}

pub(crate) fn gifter_name(gifter: &Option<User>) -> String {
    gifter.as_ref().map(name).unwrap_or_else(|| "An anonymous gifter".to_string())
}

/// The first of `words` that appears in `text` as a whole word,
/// case-insensitively, ignoring punctuation around it ("Hype!" is "hype").
fn first_word_in(text: &str, words: &[String]) -> Option<String> {
    words_in(text, words).into_iter().next()
}

/// Each of `words` that appears in `text`, once, in the order they appear
/// (matched as in [`first_word_in`]).
fn words_in(text: &str, words: &[String]) -> Vec<String> {
    let normalise = |word: &str| word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    let wanted: Vec<String> =
        words.iter().map(|word| normalise(word)).filter(|word| !word.is_empty()).collect();
    let mut found: Vec<String> = Vec::new();
    for word in text.split_whitespace().map(normalise) {
        if wanted.contains(&word) && !found.contains(&word) {
            found.push(word);
        }
    }
    found
}

/// A plugin setting's value as a plugin gets it: a text filled from the
/// event (with its emotes), a picture or sound as its path (from a folder,
/// one picked at random), the rest as stored (or the default).
fn setting_value(
    setting: &protocol::plugin::Setting,
    stored: Option<&serde_json::Value>,
    activation: &Activation<'_>,
    ctx: &Ctx<'_>,
    rng: &mut dyn crate::pipeline::RandomSource,
) -> serde_json::Value {
    match &setting.kind {
        SettingKind::Text { .. } => {
            let stored = setting.read(stored);
            let template = stored.as_str().unwrap_or_default();
            let text = fill(template, |name| activation.values.get(name).map(Value::text), activation, ctx);
            plugins::text_value(&text)
        }
        _ => protocol::plugin::shared_value(setting, stored, &mut |kind, folder, every| {
            ctx.media.pick_files(kind, folder, every, rng)
        }),
    }
}

#[cfg(test)]
mod tests {
    use protocol::activity::ItemOrigin;
    use protocol::cards::{MathOp, Node, Overrides, SocketRef, Wire};
    use protocol::plugin::{ALERT_PLUGIN, SPEAK_PLUGIN};
    use protocol::text_style::TextStyle;
    use protocol::{Command, config};

    use super::*;
    use crate::effects::tests::posted;
    use crate::model::{Amount, EmoteRef, EmoteSpan, RewardRef, Roles};
    use crate::pipeline::testing::{FixedRandom, origin, user};
    use crate::services::activity::{Activity, Asked, Play};
    use crate::services::emotes::{EmoteInfo, EmoteRegistry};
    use crate::services::overlay_hub::OverlayHub;
    use crate::services::speech::Speech;

    fn signal(kind: SignalKind) -> Signal {
        Signal::new(origin(), kind)
    }

    fn bits(value: u64) -> Signal {
        signal(SignalKind::Support {
            user: user("a"),
            amount: Amount::Units { value, unit: "bits".into() },
            message: None,
        })
    }

    fn reward(title: &str, input: Option<&str>) -> Signal {
        signal(SignalKind::RewardRedeemed {
            user: user("v"),
            reward: RewardRef { id: "r1".into(), title: Some(title.into()) },
            input: input.map(EmoteText::from),
        })
    }

    /// Runs `card` for each signal at the given times; returns the overlay
    /// commands it caused.
    fn run(card: Card, signals: &[(u64, Signal)]) -> Vec<Command> {
        run_with(card, signals, &[0.3])
    }

    /// As [`run`], with the random numbers the card gets, in turn.
    fn run_with(card: Card, signals: &[(u64, Signal)], random: &[f64]) -> Vec<Command> {
        run_in(card, signals, random, &crate::media::MediaFolders::default())
    }

    /// As [`run_with`], with these pictures and sounds folders.
    fn run_in(
        card: Card,
        signals: &[(u64, Signal)],
        random: &[f64],
        media: &crate::media::MediaFolders,
    ) -> Vec<Command> {
        let steps: Vec<(u64, Step)> =
            signals.iter().map(|(at, signal)| (*at, Step::Signal(signal.clone()))).collect();
        run_steps(card, &steps, random, media)
    }

    /// What happens to a card at a moment in a test.
    #[allow(clippy::large_enum_variant)]
    enum Step {
        Signal(Signal),
        /// The pipeline's tick.
        Tick,
    }

    /// As [`run`], with ticks among the signals.
    fn run_ticking(card: Card, steps: &[(u64, Step)]) -> Vec<Command> {
        run_steps(card, steps, &[0.3], &crate::media::MediaFolders::default())
    }

    /// As [`run_in`], with ticks among the signals.
    fn run_steps(
        card: Card,
        steps: &[(u64, Step)],
        random: &[f64],
        media: &crate::media::MediaFolders,
    ) -> Vec<Command> {
        let registry = EmoteRegistry::for_tests();
        registry.register_native(
            "twitch",
            &origin().channel.id,
            EmoteInfo {
                emote: crate::effects::tests::emote("Kappa"),
                image_url: "x".into(),
                zero_width: false,
            },
        );
        let hub = OverlayHub::new();
        let mut receiver = hub.subscribe();
        let mut rng = FixedRandom::new(random);
        // With the text styles a new config starts with: highlight, small.
        let effects =
            Effects::new(&config::Effects::default()).with_text_styles(&TextStyle::starters(), true);
        let mut runtime = CardRuntime::new(card);
        let speech = Speech::off();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let mut resumes = activity.resumes();
        for (at, step) in steps {
            let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
            ctx.now_ms = *at;
            ctx.media = media;
            match step {
                Step::Signal(signal) => {
                    runtime.on_signal(signal, &mut ctx, &effects);
                }
                Step::Tick => runtime.on_tick(&origin(), &mut ctx, &effects),
            }
        }
        // Queued runs going on stage and ending: the card carries on, as
        // the pipeline has it.
        activity.play_all_then(|| {
            while let Ok(resume) = resumes.try_recv() {
                let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
                ctx.media = media;
                ctx.cause = resume.cause.clone();
                runtime.resume(&resume.then, &resume.output, &mut ctx, &effects);
            }
        });
        std::iter::from_fn(|| receiver.try_recv().ok().map(|envelope| envelope.command.clone())).collect()
    }

    /// The Alerts plugin's runs: their messages' text.
    fn alerts(commands: &[Command]) -> Vec<String> {
        alert_runs(commands)
            .iter()
            .filter_map(|run| run["values"]["message"]["text"].as_str().map(str::to_string))
            .collect()
    }

    /// The Alerts plugin's runs, as its page gets them.
    fn alert_runs(commands: &[Command]) -> Vec<serde_json::Value> {
        commands
            .iter()
            .filter_map(|command| match command {
                Command::Plugin(command) if command.plugin == ALERT_PLUGIN => {
                    Some(serde_json::to_value(&command.message).unwrap())
                }
                _ => None,
            })
            .collect()
    }

    /// A node of one of the built-in plugins, with these settings.
    fn built_in_node(plugin: &str, settings: serde_json::Value) -> NodeKind {
        let mut kind = built_in()
            .node_kinds()
            .into_iter()
            .find(|kind| matches!(kind, NodeKind::Plugin { plugin: own, .. } if own == plugin))
            .unwrap();
        if let NodeKind::Plugin { settings: own, .. } = &mut kind {
            own.extend(settings.as_object().unwrap().clone());
        }
        kind
    }

    /// An Alerts plugin node showing `message`.
    fn alert(message: &str) -> NodeKind {
        built_in_node(ALERT_PLUGIN, serde_json::json!({ "message": message }))
    }

    /// How many emotes each run of the Rain plugin rains, as its page
    /// counts them (`crates/rain`): the count when one is sent, else every
    /// emote; runs that rain nothing are left out.
    fn rain_counts(commands: &[Command]) -> Vec<usize> {
        commands
            .iter()
            .filter_map(|command| match command {
                Command::Plugin(command) if command.plugin == protocol::plugin::RAIN_PLUGIN => {
                    let protocol::plugin::PluginMessage::Run { values, .. } = &command.message else {
                        return None;
                    };
                    let emotes = values.get("emotes")?.as_array()?.len();
                    let count = values.get("count").and_then(serde_json::Value::as_f64);
                    let rains = match count {
                        _ if emotes == 0 => 0,
                        Some(count) => count as usize,
                        None => emotes,
                    };
                    (rains > 0).then_some(rains)
                }
                _ => None,
            })
            .collect()
    }

    /// The plugins built into the app, as the server loads them.
    fn built_in() -> protocol::plugin::Catalog {
        crate::plugins::Plugins::load_from(std::path::Path::new("no such folder"), true).catalog()
    }

    /// A preset's card, with the built-in plugins' node types.
    fn preset(title: &str, id: &str) -> Card {
        let mut card = (cards::presets().into_iter().find(|preset| preset.title == title).unwrap().card)(id);
        card.attach_plugins(&built_in());
        card
    }

    fn premade(id: &str) -> Card {
        let mut card = cards::premade().into_iter().find(|card| card.id == id).unwrap();
        card.enabled = true;
        card.attach_plugins(&built_in());
        card
    }

    /// A Rain plugin node raining `count` random emotes (unless emotes are
    /// wired in).
    fn rain_node(count: f64) -> NodeKind {
        let mut rain = built_in()
            .node_kinds()
            .into_iter()
            .find(|kind| matches!(kind, NodeKind::Plugin { plugin, .. } if plugin == protocol::plugin::RAIN_PLUGIN))
            .unwrap();
        if let NodeKind::Plugin { settings, .. } = &mut rain {
            settings.insert("count".into(), count.into());
        }
        rain
    }

    #[test]
    fn preparing_sends_nothing_and_keeps_what_would_wait() {
        let registry = EmoteRegistry::for_tests();
        let hub = OverlayHub::new();
        let mut sent = hub.subscribe();
        let speech = Speech::off();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let effects = Effects::new(&config::Effects::default());
        let mut cards = [CardRuntime::new(premade("bits-alert")), CardRuntime::new(premade("rain-on-bits"))];
        let cheer = bits(500);

        // Live: the rain falls at once, the alert waits.
        let mut rng = FixedRandom::new(&[0.3]);
        let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
        for card in &mut cards {
            card.on_signal(&cheer, &mut ctx, &effects);
        }
        let live: Vec<Command> =
            std::iter::from_fn(|| sent.try_recv().ok().map(|sent| sent.command.clone())).collect();
        assert!(
            live.iter().any(|command| matches!(command,
                Command::Plugin(command) if command.plugin == protocol::plugin::RAIN_PLUGIN)),
            "{live:?}"
        );
        let waiting = activity.waiting_plays();
        assert_eq!(waiting.len(), 1, "the alert");

        // Prepared again on copies: nothing goes out, and the same alert
        // is kept.
        let mut rng = FixedRandom::new(&[0.3]);
        let mut ctx = Ctx::preparing(&mut rng, &registry, crate::pipeline::no_media(), &speech);
        for card in &cards {
            card.copy().on_signal(&cheer, &mut ctx, &effects);
        }
        let runs = ctx.recording.take();
        assert!(sent.try_recv().is_err(), "nothing went to the overlays");
        assert_eq!(activity.waiting_plays(), waiting, "nothing was queued");
        let queued: Vec<Play> =
            runs.iter().flat_map(|run| run.queued.iter().map(|queued| queued.play.clone())).collect();
        assert_eq!(queued, waiting);
        assert!(runs.iter().all(|run| !run.unpreparable));
    }

    #[test]
    fn an_event_draws_the_same_at_a_node_however_the_rest_changed() {
        // Bits → alert, its message a random number; with or without a
        // Chance (that passes) and another Random drawing before it.
        let card = |more: bool| {
            let mut nodes = vec![
                (1, NodeKind::Bits { min: 0.0 }),
                (2, NodeKind::Random { min: 0.0, max: 1e9 }),
                (3, NodeKind::text("{n}")),
                (4, alert("")),
            ];
            let mut wires = vec![(2, "value", 3, "n"), (3, "text", 4, "message")];
            if more {
                nodes.push((5, NodeKind::Chance { percent: 100.0 }));
                nodes.push((6, NodeKind::Random { min: 0.0, max: 1.0 }));
                nodes.push((7, NodeKind::Math { op: MathOp::Add, a: 0.0, b: 0.0 }));
                wires.extend([(1, "event", 5, "event"), (5, "event", 4, "trigger"), (6, "value", 7, "a")]);
                wires.push((7, "result", 4, "duration"));
            } else {
                wires.push((1, "event", 4, "trigger"));
            }
            card_of(nodes, &wires)
        };
        let message = |card: Card, seed: u64| {
            let registry = EmoteRegistry::for_tests();
            let (hub, speech) = (OverlayHub::new(), Speech::off());
            let activity = Activity::for_tests(hub.clone(), speech.clone());
            let mut rng = crate::pipeline::StdRandom::new();
            let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
            ctx.cause.seed = seed;
            let mut card = CardRuntime::new(card);
            card.on_signal(&bits(5), &mut ctx, &Effects::new(&config::Effects::default()));
            let plays = activity.waiting_plays();
            plays[0].values["message"]["text"].as_str().unwrap().to_string()
        };
        assert_eq!(message(card(false), 7), message(card(false), 7), "the same seed, the same draw");
        assert_eq!(message(card(true), 7), message(card(false), 7), "whatever else draws first");
        assert_ne!(message(card(false), 7), message(card(false), 8), "another event draws afresh");
    }

    /// Prepares `card` again for `signal` at `at` (with what its run
    /// decided `before`): the run.
    fn prepare(card: &mut CardRuntime, signal: &Signal, at: u64, before: Vec<Decided>) -> CardRun {
        let registry = EmoteRegistry::for_tests();
        let speech = Speech::off();
        let mut rng = FixedRandom::new(&[0.3]);
        let mut ctx = Ctx::preparing(&mut rng, &registry, crate::pipeline::no_media(), &speech);
        ctx.now_ms = at;
        ctx.recording.prepare_next(before);
        card.on_signal(signal, &mut ctx, &Effects::new(&config::Effects::default()));
        ctx.recording.take().remove(0)
    }

    #[test]
    fn a_node_that_keeps_state_decides_as_it_did_unless_it_changed() {
        let hydrate = premade("hydrate");
        let water = reward("Hydrate", None);
        // In order on one copy: the first passes the 30 s cooldown, the
        // second, 10 s later, doesn't.
        let mut copy = CardRuntime::new(hydrate.clone());
        let first = prepare(&mut copy, &water, 0, Vec::new());
        let second = prepare(&mut copy, &water, 10_000, Vec::new());
        assert_eq!((first.queued.len(), second.queued.len()), (1, 0));
        let decisions =
            |run: &CardRun| run.decided.iter().map(|decided| decided.decision.clone()).collect::<Vec<_>>();
        assert_eq!(decisions(&second), [Decision::Passed(false)]);

        // The second again on its own (a fresh copy's cooldown would let
        // it through): as it decided, and recorded again.
        let again = prepare(&mut CardRuntime::new(hydrate.clone()), &water, 10_000, second.decided.clone());
        assert!(again.queued.is_empty());
        assert_eq!(again.decided, second.decided);

        // Its cooldown shortened since: worked out afresh.
        let mut shorter = hydrate;
        for node in &mut shorter.nodes {
            if let NodeKind::Cooldown { seconds, .. } = &mut node.kind {
                *seconds = 5.0;
            }
        }
        let afresh = prepare(&mut CardRuntime::new(shorter), &water, 10_000, second.decided);
        assert_eq!(afresh.queued.len(), 1);
    }

    #[test]
    fn a_run_held_where_it_wasnt_before_stays_as_it_was() {
        // Bits → Delay → alert: what comes after the Delay is a run of its
        // own.
        let delayed = card_of(
            vec![(1, NodeKind::Bits { min: 0.0 }), (2, NodeKind::Delay { seconds: 5.0 }), (3, alert("x"))],
            &[(1, "event", 2, "event"), (2, "event", 3, "trigger")],
        );
        let held = prepare(&mut CardRuntime::new(delayed.clone()), &bits(5), 0, Vec::new());
        assert!(held.queued.is_empty());
        assert_eq!(
            held.decided.iter().map(|decided| &decided.decision).collect::<Vec<_>>(),
            [&Decision::Held]
        );
        assert!(held.unpreparable, "it didn't wait there before");
        let again = prepare(&mut CardRuntime::new(delayed), &bits(5), 0, held.decided);
        assert!(!again.unpreparable, "it waited there before too");
    }

    #[test]
    fn alert_with_placeholders_and_cooldown() {
        // Hydrate: reward → cooldown 30 s → alert "{viewer}: drink some water!"
        let commands = run(
            premade("hydrate"),
            &[
                (0, reward("Hydrate", None)),
                (10_000, reward("Hydrate", None)),
                (40_000, reward("hydrate", None)),
                (50_000, reward("Something else", None)),
            ],
        );
        assert_eq!(alerts(&commands), ["V: drink some water!", "V: drink some water!"]);
    }

    #[test]
    fn a_custom_events_data_fills_placeholders() {
        let alert = alert("{colour} {name} x{count} {summary}{missing}");
        let card = card_of(
            vec![(1, NodeKind::Custom { name: "party".into() }), (2, alert)],
            &[(1, "event", 2, "trigger")],
        );
        let data = serde_json::json!({ "colour": "red", "count": 3, "name": "not this", "list": [1] });
        let serde_json::Value::Object(data) = data else { unreachable!() };
        let event = signal(SignalKind::Custom { name: "party".into(), data });
        assert_eq!(alerts(&run(card, &[(0, event)])), ["red party x3 party{missing}"], "unknown ones stay");
    }

    #[test]
    fn texts_mark_parts_with_text_styles() {
        // In a node's own text, and in a Text node's.
        let card = card_of(
            vec![
                (1, NodeKind::Custom { name: "party".into() }),
                (2, alert("[highlight]{name}[/] {colour}")),
                (3, alert("")),
                (4, NodeKind::text("[small]x{count}[/]")),
            ],
            &[(1, "event", 2, "trigger"), (1, "event", 3, "trigger"), (4, "text", 3, "message")],
        );
        let data = serde_json::json!({ "colour": "red", "count": 3 });
        let serde_json::Value::Object(data) = data else { unreachable!() };
        let runs = alert_runs(&run(card, &[(0, signal(SignalKind::Custom { name: "party".into(), data }))]));
        let parts = |run: &serde_json::Value| -> Vec<(String, Option<String>)> {
            run["values"]["message"]["parts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|part| {
                    (
                        part["text"].as_str().unwrap().to_string(),
                        part["style"]["name"].as_str().map(str::to_string),
                    )
                })
                .collect()
        };
        assert_eq!(runs[0]["values"]["message"]["text"], "party red", "marks don't show in the text");
        assert_eq!(parts(&runs[0]), [("party".into(), Some("highlight".into())), (" red".into(), None)]);
        assert_eq!(
            runs[0]["values"]["message"]["parts"][0]["style"]["colour"], "#ffcc00",
            "with the style itself"
        );
        assert_eq!(parts(&runs[1]), [("x3".into(), Some("small".into()))]);
    }

    #[test]
    fn a_replay_passes_a_cooldown_without_starting_it() {
        let registry = EmoteRegistry::for_tests();
        let hub = OverlayHub::new();
        let speech = Speech::off();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let mut rng = FixedRandom::new(&[0.5]);
        let effects = Effects::new(&config::Effects::default());
        let mut runtime = CardRuntime::new(premade("hydrate"));
        let mut fire = |at: u64, origin: ItemOrigin| {
            let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
            ctx.now_ms = at;
            ctx.cause.origin = origin;
            runtime.on_signal(&reward("Hydrate", None), &mut ctx, &effects)
        };
        assert!(fire(0, ItemOrigin::Live));
        assert!(!fire(10_000, ItemOrigin::Live), "cooling down");
        assert!(fire(10_000, ItemOrigin::Replay), "asked for");
        assert!(fire(31_000, ItemOrigin::Live), "the replay didn't start it again");
    }

    #[test]
    fn value_wires_scale_the_rain() {
        // Rain on bits: amount ÷ 100, clamped to 1–50.
        let commands = run(premade("rain-on-bits"), &[(0, bits(450)), (1, bits(99)), (2, bits(100_000))]);
        assert_eq!(rain_counts(&commands), [4, 50], "99 bits is under the card's 100-bit minimum");
    }

    #[test]
    fn emotes_found_in_a_cheer() {
        // The preset: bits → emotes in the message → rain. Kappa is a
        // Twitch emote (by code, no positions); Cheer100 isn't an emote.
        let card = preset("Rain the emotes in a cheer", "c");
        let cheer = |text: &str| {
            signal(SignalKind::Support {
                user: user("a"),
                amount: Amount::Units { value: 100, unit: "bits".into() },
                message: Some(text.into()),
            })
        };
        let commands =
            run(card, &[(0, cheer("Cheer100 Kappa nice Kappa")), (1, cheer("Cheer100 no emotes"))]);
        assert_eq!(rain_counts(&commands), [2], "a cheer without emotes rains nothing");

        // A count wired into the rain is how many rain: here bits ÷ 25.
        let mut card = preset("Rain the emotes in a cheer", "c");
        let divide = card.next_node_id();
        let kind = NodeKind::Math { op: cards::MathOp::Divide, a: 0.0, b: 25.0 };
        card.nodes.push(Node { id: divide, x: 0.0, y: 0.0, kind });
        card.wires.push(Wire { from: SocketRef::new(1, "amount"), to: SocketRef::new(divide, "a") });
        card.wires.push(Wire { from: SocketRef::new(divide, "result"), to: SocketRef::new(3, "count") });
        assert!(card.problems().is_empty(), "{:?}", card.problems());
        let commands = run(card, &[(0, cheer("Cheer100 Kappa nice Kappa"))]);
        assert_eq!(rain_counts(&commands), [4]);
    }

    #[test]
    fn emotes_wired_from_chat() {
        let message = signal(SignalKind::EmotesPosted {
            user: user("a"),
            emotes: vec![posted("Kappa", &[]), posted("Kappa", &[]), posted("PogU", &["RainTime"])],
        });
        assert_eq!(rain_counts(&run(premade("emote-rain"), &[(0, message)])), [3]);
    }

    #[test]
    fn chat_words_chance_and_text_template() {
        let mut card =
            Card { id: "c".into(), name: "c".into(), enabled: true, nodes: Vec::new(), wires: Vec::new() };
        let node = |id, kind| Node { id, x: 0.0, y: 0.0, kind };
        let wire = |from, output: &str, to, input: &str| Wire {
            from: SocketRef::new(from, output),
            to: SocketRef::new(to, input),
        };
        card.nodes = vec![
            node(1, NodeKind::ChatWords { words: vec!["Hype".into()] }),
            node(2, NodeKind::Chance { percent: 50.0 }),
            node(3, NodeKind::text("{who} said {word} x{n}")),
            node(4, NodeKind::Math { op: MathOp::Multiply, a: 0.0, b: 2.0 }),
            node(5, alert("")),
            node(6, NodeKind::Number { value: 21.0 }),
        ];
        card.wires = vec![
            wire(1, "event", 2, "event"),
            wire(2, "event", 5, "trigger"),
            wire(1, "viewer", 3, "who"),
            wire(6, "value", 4, "a"),
            wire(4, "result", 3, "n"),
            wire(3, "text", 5, "message"),
        ];
        assert_eq!(card.problems(), []);
        let chat =
            |text: &str| signal(SignalKind::ChatMessage { user: user("b"), text: text.into(), first: false });
        // FixedRandom gives 0.3 → 30 < 50, so the chance passes.
        let commands = run(card, &[(0, chat("so HYPE!")), (1, chat("hyped"))]);
        assert_eq!(alerts(&commands), ["B said hype x42"]);
    }

    #[test]
    fn per_viewer_cooldown_and_filters() {
        let mut card = premade("hydrate");
        card.nodes[1].kind = NodeKind::Cooldown { seconds: 30.0, per_viewer: true };
        let from = |viewer: &str| {
            signal(SignalKind::RewardRedeemed {
                user: user(viewer),
                reward: RewardRef { id: "r1".into(), title: Some("Hydrate".into()) },
                input: None,
            })
        };
        let commands = run(card, &[(0, from("a")), (1, from("b")), (2, from("a"))]);
        assert_eq!(alerts(&commands).len(), 2);

        let sub = |kind| signal(SignalKind::Membership { user: user("s"), kind, tier: None, message: None });
        let gifts = NodeKind::Sub {
            new: false,
            resub: false,
            gifted: true,
            in_gift_bomb: false,
            prime_to_paid: false,
            gift_continued: false,
        };
        let in_bomb = sub(MembershipKind::Gifted { gifter: None, batch_id: Some("b".into()) });
        let single = sub(MembershipKind::Gifted { gifter: Some(user("g")), batch_id: None });
        assert!(match_event(&gifts, &in_bomb).is_none());
        let values = match_event(&gifts, &single).unwrap();
        assert_eq!(values["summary"], Value::Text("G gifted a sub to S".into()));
        assert!(
            match_event(
                &NodeKind::Sub {
                    new: true,
                    resub: false,
                    gifted: false,
                    in_gift_bomb: false,
                    prime_to_paid: false,
                    gift_continued: false
                },
                &single
            )
            .is_none()
        );

        // Upgrades: a switch each, with the gifter when there was one.
        let all = |prime_to_paid, gift_continued| NodeKind::Sub {
            new: true,
            resub: true,
            gifted: true,
            in_gift_bomb: true,
            prime_to_paid,
            gift_continued,
        };
        let prime = sub(MembershipKind::Upgraded { from: UpgradedFrom::Prime });
        let continued =
            sub(MembershipKind::Upgraded { from: UpgradedFrom::Gift { gifter: Some(user("g")) } });
        assert!(match_event(&all(false, true), &prime).is_none());
        assert!(match_event(&all(true, false), &continued).is_none());
        let values = match_event(&all(true, false), &prime).unwrap();
        assert_eq!(values["summary"], Value::Text("S switched from Prime to a paid sub".into()));
        let values = match_event(&all(false, true), &continued).unwrap();
        assert_eq!(values["summary"], Value::Text("S is continuing the gift sub from G".into()));
        assert_eq!(values["gifter"], Value::Text("G".into()));
    }

    fn card_of(nodes: Vec<(NodeId, NodeKind)>, wires: &[(NodeId, &str, NodeId, &str)]) -> Card {
        let card = Card {
            id: "c".into(),
            name: "c".into(),
            enabled: true,
            nodes: nodes.into_iter().map(|(id, kind)| Node { id, x: 0.0, y: 0.0, kind }).collect(),
            wires: wires
                .iter()
                .map(|(from, output, to, input)| Wire {
                    from: SocketRef::new(*from, output),
                    to: SocketRef::new(*to, input),
                })
                .collect(),
        };
        assert_eq!(card.problems(), []);
        card
    }

    #[test]
    fn emote_combo_needs_different_viewers_in_time() {
        let card = card_of(
            vec![
                (1, NodeKind::EmoteCombo { viewers: 3.0, seconds: 10.0 }),
                (2, alert("{summary}: {emote} x{viewers}")),
            ],
            &[(1, "event", 2, "trigger")],
        );
        let posts = |who: &str, codes: &[&str]| {
            signal(SignalKind::EmotesPosted {
                user: user(who),
                emotes: codes.iter().map(|code| posted(code, &[])).collect(),
            })
        };
        let commands = run(
            card,
            &[
                (0, posts("a", &["Kappa", "Kappa", "PogU"])),
                (1_000, posts("a", &["Kappa"])), // the same viewer again doesn't count
                (2_000, posts("b", &["Kappa"])),
                (3_000, posts("c", &["Kappa", "PogU"])), // Kappa: a, b, c: a combo
                (4_000, posts("d", &["Kappa"])),         // counts afresh
                (5_000, posts("e", &["Kappa"])),
                (20_000, posts("f", &["Kappa", "PogU"])), // d and e are too long ago
                (21_000, posts("g", &["PogU"])),
                (22_000, posts("h", &["PogU"])), // PogU: f, g, h (a and c too long ago)
            ],
        );
        assert_eq!(
            alerts(&commands),
            ["Kappa combo from 3 viewers: Kappa x3", "PogU combo from 3 viewers: PogU x3"]
        );
    }

    #[test]
    fn a_combo_rains_its_emote() {
        let rain = rain_node(1.0);
        let card = card_of(
            vec![
                (1, NodeKind::EmoteCombo { viewers: 2.0, seconds: 10.0 }),
                (2, rain),
                (3, NodeKind::Math { op: MathOp::Multiply, a: 0.0, b: 5.0 }),
            ],
            &[
                (1, "event", 2, "trigger"),
                (1, "emote", 2, "emotes"),
                (1, "viewers", 3, "a"),
                (3, "result", 2, "count"),
            ],
        );
        let kappa = |who: &str| {
            signal(SignalKind::EmotesPosted { user: user(who), emotes: vec![posted("Kappa", &[])] })
        };
        assert_eq!(rain_counts(&run(card, &[(0, kappa("a")), (1, kappa("b"))])), [10]);
    }

    #[test]
    fn chant_counts_each_listed_word() {
        let card = card_of(
            vec![
                (1, NodeKind::Chant { words: vec!["GG".into(), "W".into()], viewers: 2.0, seconds: 30.0 }),
                (2, alert("{summary}")),
            ],
            &[(1, "event", 2, "trigger")],
        );
        let chat = |who: &str, text: &str| {
            signal(SignalKind::ChatMessage { user: user(who), text: text.into(), first: false })
        };
        let commands = run(
            card,
            &[
                (0, chat("a", "gg gg W")),
                (1, chat("a", "GG!")),
                (2, chat("b", "nice")),
                (3, chat("b", "gg w")),
            ],
        );
        assert_eq!(alerts(&commands), ["2 viewers chant gg", "2 viewers chant w"]);
    }

    #[test]
    fn chances_chained_through_miss_pick_one_rarity() {
        // 10 % legendary, else 50 % rare, else common.
        let card = card_of(
            vec![
                (1, NodeKind::Custom { name: String::new() }),
                (2, NodeKind::Chance { percent: 10.0 }),
                (3, NodeKind::Chance { percent: 50.0 }),
                (4, alert("legendary")),
                (5, alert("rare")),
                (6, alert("common")),
            ],
            &[
                (1, "event", 2, "event"),
                (2, "event", 4, "trigger"),
                (2, "miss", 3, "event"),
                (3, "event", 5, "trigger"),
                (3, "miss", 6, "trigger"),
            ],
        );
        let event = || signal(SignalKind::Custom { name: "x".into(), data: Default::default() });
        // Rolls in turn: 0.05 hits the first; 0.5 misses it and 0.2 hits the
        // second; 0.9 and 0.7 miss both.
        let commands =
            run_with(card, &[(0, event()), (1, event()), (2, event())], &[0.05, 0.5, 0.2, 0.9, 0.7]);
        assert_eq!(alerts(&commands), ["legendary", "rare", "common"]);
    }

    #[test]
    fn rarity_picks_exactly_one_tier() {
        let card = preset("Rarity alerts", "r");
        assert_eq!(card.problems(), []);
        let redeem = |n: u64| (n, reward("Anything", None));
        // 1 % legendary (0–1), 10 % epic (1–11), 20 % rare (11–31), else common.
        let commands = run_with(
            card,
            &[redeem(0), redeem(1), redeem(2), redeem(3), redeem(4)],
            &[0.005, 0.01, 0.3, 0.31, 0.99],
        );
        assert_eq!(
            alerts(&commands),
            [
                "V rolled LEGENDARY!",
                "V rolled Epic!",
                "V rolled Rare!",
                "V rolled Common!",
                "V rolled Common!"
            ]
        );
    }

    #[test]
    fn a_reward_opens_the_gate_for_emote_rain() {
        let card = preset("Emote rain for a while", "g");
        assert_eq!(card.problems(), []);
        let kappa =
            || signal(SignalKind::EmotesPosted { user: user("a"), emotes: vec![posted("Kappa", &[])] });
        let commands = run(
            card,
            &[
                (0, kappa()),                   // closed
                (1_000, reward("Rain", None)),  // opens until 61 s (and an alert)
                (2_000, kappa()),               // rains
                (60_000, kappa()),              // rains
                (61_000, kappa()),              // closed again
                (70_000, reward("Rain", None)), // opens again
                (75_000, kappa()),              // rains
            ],
        );
        assert_eq!(rain_counts(&commands), [1, 1, 1]);
        assert_eq!(
            alerts(&commands),
            ["V made every emote rain for 60 s!", "V made every emote rain for 60 s!"]
        );
    }

    #[test]
    fn overrides_go_with_the_effect() {
        let mut card = premade("emote-rain");
        if let NodeKind::Plugin { changes, .. } = &mut card.nodes[1].kind {
            changes.insert("restitution".into(), serde_json::json!(1.0));
        }
        let message =
            signal(SignalKind::EmotesPosted { user: user("a"), emotes: vec![posted("Kappa", &[])] });
        let runs = plugin_runs(&run(card, &[(0, message)]));
        assert_eq!(
            runs[0]["changes"],
            serde_json::json!({ "restitution": 1.0 }),
            "the rain brings its changes"
        );

        // An alert's change to its style: its run brings the style with it.
        let mut card = premade("raid-alert");
        if let NodeKind::Plugin { changes, .. } = &mut card.nodes[1].kind {
            changes.insert("picture".into(), "gold.gif".into());
        }
        let raid = signal(SignalKind::Raid { from: user("r"), viewers: 5 });
        let runs = alert_runs(&run(card, &[(0, raid)]));
        let settings = &runs[0]["settings"];
        assert_eq!(
            (&settings["picture"], &settings["position"]),
            (&"/pictures/gold%2Egif".into(), &"top".into())
        );
    }

    #[test]
    fn started_and_ended_carry_on_with_the_same_event() {
        // Bits → an alert; when it shows, the rain, as many as the bits;
        // once it's gone, a thank-you alert.
        let card = card_of(
            vec![
                (1, NodeKind::Bits { min: 0.0 }),
                (2, alert("{amount} bits")),
                (3, rain_node(1.0)),
                (4, alert("Thanks again")),
            ],
            &[
                (1, "event", 2, "trigger"),
                (2, STARTED_OUTPUT, 3, "trigger"),
                (1, "amount", 3, "count"),
                (2, ENDED_OUTPUT, 4, "trigger"),
            ],
        );
        // Both alerts wait; each one's rain comes when it shows, with its
        // own amount.
        let commands = run(card, &[(0, bits(3)), (1, bits(5))]);
        let order: Vec<&str> = commands
            .iter()
            .filter_map(|command| match command {
                Command::Plugin(command) => Some(command.plugin.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["alert", "rain", "alert", "rain", "alert", "alert"]);
        assert_eq!(rain_counts(&commands), [3, 5]);
        assert_eq!(alerts(&commands), ["3 bits", "5 bits", "Thanks again", "Thanks again"]);
    }

    #[test]
    fn a_card_changed_since_carries_on_as_it_is_now() {
        // With the rain on its Started, or without the rain.
        let card = |wired: bool| match wired {
            true => card_of(
                vec![(1, NodeKind::Bits { min: 0.0 }), (2, alert("Hi")), (3, rain_node(2.0))],
                &[(1, "event", 2, "trigger"), (2, STARTED_OUTPUT, 3, "trigger")],
            ),
            false => card_of(
                vec![(1, NodeKind::Bits { min: 0.0 }), (2, alert("Hi"))],
                &[(1, "event", 2, "trigger")],
            ),
        };
        let then = Then::for_tests("c", 2, bits(10));
        let registry = EmoteRegistry::for_tests();
        let (hub, speech) = (OverlayHub::new(), Speech::off());
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let mut rng = FixedRandom::new(&[0.5]);
        let effects = Effects::new(&config::Effects::default());
        let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
        assert!(CardRuntime::new(card(true)).resume(&then, STARTED_OUTPUT, &mut ctx, &effects));
        assert!(
            !CardRuntime::new(card(false)).resume(&then, STARTED_OUTPUT, &mut ctx, &effects),
            "unwired since"
        );
        let mut gone = card(true);
        gone.nodes.retain(|node| node.id != 2);
        assert!(!CardRuntime::new(gone).resume(&then, STARTED_OUTPUT, &mut ctx, &effects), "the node's gone");
    }

    #[test]
    fn a_folder_gives_a_file_at_random_each_run() {
        let dir = std::env::temp_dir().join(format!("lsk_cards_folders_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("alerts")).unwrap();
        for name in ["a.gif", "b.png"] {
            std::fs::write(dir.join("alerts").join(name), b"x").unwrap();
        }
        let media = crate::media::MediaFolders { pictures: dir.clone(), sounds: dir.clone() };

        // An alert's change to its style's picture.
        let mut card = premade("raid-alert");
        if let NodeKind::Plugin { changes, .. } = &mut card.nodes[1].kind {
            changes.insert("picture".into(), "alerts/".into());
        }
        let raid = signal(SignalKind::Raid { from: user("r"), viewers: 5 });
        let runs = alert_runs(&run_in(card, &[(0, raid.clone()), (1, raid)], &[0.0, 0.99], &media));
        let pictures: Vec<_> = runs.iter().map(|run| run["settings"]["picture"].clone()).collect();
        assert_eq!(pictures, ["/pictures/alerts%2Fa%2Egif", "/pictures/alerts%2Fb%2Epng"]);

        // A node's own picture setting; an empty folder is none.
        let mut burst = confetti().node_kinds().remove(0);
        if let NodeKind::Plugin { settings, .. } = &mut burst {
            settings.insert("picture".into(), "alerts/".into());
            settings.insert("ding".into(), "dings/".into());
        }
        let card = card_of(vec![(1, NodeKind::Bits { min: 0.0 }), (3, burst)], &[(1, "event", 3, "trigger")]);
        let runs = plugin_runs(&run_in(card, &[(0, bits(50))], &[0.99], &media));
        assert_eq!(runs[0]["values"]["picture"], "/pictures/alerts%2Fb%2Epng");
        assert_eq!(runs[0]["values"]["ding"], serde_json::Value::Null);

        // A setting with many: every file in the folder; shown (and sent)
        // only while From is Top.
        let mut burst = confetti().node_kinds().remove(0);
        if let NodeKind::Plugin { settings, .. } = &mut burst {
            settings.insert("pictures".into(), "alerts/".into());
        }
        let card = card_of(
            vec![(1, NodeKind::Bits { min: 0.0 }), (3, burst.clone())],
            &[(1, "event", 3, "trigger")],
        );
        let runs = plugin_runs(&run_in(card, &[(0, bits(50))], &[0.99], &media));
        assert_eq!(runs[0]["values"].get("pictures"), None, "From is Bottom: not shown");
        if let NodeKind::Plugin { settings, .. } = &mut burst {
            settings.insert("from".into(), "top".into());
        }
        let card = card_of(vec![(1, NodeKind::Bits { min: 0.0 }), (3, burst)], &[(1, "event", 3, "trigger")]);
        let runs = plugin_runs(&run_in(card, &[(0, bits(50))], &[0.99], &media));
        assert_eq!(
            runs[0]["values"]["pictures"],
            serde_json::json!(["/pictures/alerts%2Fa%2Egif", "/pictures/alerts%2Fb%2Epng"])
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn show_in_chat_runs_bring_the_message_and_the_viewer() {
        let card = card_of(
            vec![
                (1, NodeKind::Raid { min_viewers: 0.0 }),
                (
                    2,
                    built_in_node(
                        "chat",
                        serde_json::json!({ "message": "{raider} raided!", "class": "raid" }),
                    ),
                ),
            ],
            &[(1, "event", 2, "trigger")],
        );
        let mut raider = user("r");
        raider.color = Some("#00ff00".into());
        let commands = run(card, &[(0, signal(SignalKind::Raid { from: raider, viewers: 5 }))]);
        let runs: Vec<serde_json::Value> = commands
            .iter()
            .filter_map(|command| match command {
                Command::Plugin(command) if command.plugin == protocol::plugin::CHAT_PLUGIN => {
                    Some(serde_json::to_value(&command.message).unwrap())
                }
                _ => None,
            })
            .collect();
        let values = &runs[0]["values"];
        assert_eq!(values["message"]["text"], "R raided!");
        assert_eq!(values["class"]["text"], "raid");
        assert_eq!(values["show_viewer"], true);
        assert_eq!(
            (&values["viewer"]["display_name"], &values["viewer"]["color"]),
            (&"R".into(), &"#00ff00".into())
        );
    }

    #[test]
    fn alert_sounds_play_at_their_volume_and_the_overlay_applies_the_master() {
        let mut config = config::Effects::default();
        config.sound.volume = 0.5;
        let style = config::Preset {
            id: "default".into(),
            name: "Default".into(),
            settings: serde_json::json!({ "sound": "ding.wav", "sound_volume": 0.8 })
                .as_object()
                .unwrap()
                .clone(),
        };
        let mut plugins = std::collections::BTreeMap::new();
        plugins.insert(
            ALERT_PLUGIN.to_string(),
            config::PluginConfig { presets: vec![style], ..Default::default() },
        );
        let hub = OverlayHub::new();
        let mut receiver = hub.subscribe();
        let registry = EmoteRegistry::for_tests();
        let mut rng = FixedRandom::new(&[0.5]);
        let speech = Speech::off();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
        config.sound.muted = true;
        let card = card_of(
            vec![(1, NodeKind::Custom { name: String::new() }), (2, alert("hi"))],
            &[(1, "event", 2, "trigger")],
        );
        let custom = signal(SignalKind::Custom { name: String::new(), data: Default::default() });
        CardRuntime::new(card).on_signal(&custom, &mut ctx, &Effects::new(&config).with_plugins(&plugins));
        activity.play_all();
        let Ok(envelope) = receiver.try_recv() else { panic!("an alert") };
        let runs = alert_runs(std::slice::from_ref(&envelope.command));
        // Its own volume: the page applies the master volume and mute,
        // live, so changing them reaches what's playing.
        let settings = &runs[0]["settings"];
        assert_eq!(
            (&settings["sound"], &settings["sound_volume"]),
            (&"/sounds/ding%2Ewav".into(), &0.8.into())
        );
        assert!(
            Effects::new(&config)
                .module_configs()
                .contains(&Command::Shell(protocol::ShellCommand::Volume { volume: 0.5, muted: true }))
        );
    }

    #[test]
    fn an_alert_reads_its_text_aloud() {
        let card = card_of(
            vec![
                (1, NodeKind::Custom { name: String::new() }),
                (2, built_in_node(ALERT_PLUGIN, serde_json::json!({ "message": "Alert", "read": "Thanks" }))),
                (3, built_in_node(SPEAK_PLUGIN, serde_json::json!({ "text": "Alone" }))),
            ],
            &[(1, "event", 2, "trigger"), (1, "event", 3, "trigger")],
        );
        assert!(card.problems().is_empty(), "{:?}", card.problems());
        let hub = OverlayHub::new();
        let mut receiver = hub.subscribe();
        let registry = EmoteRegistry::for_tests();
        let mut rng = FixedRandom::new(&[0.5]);
        let speech = Speech::queue_only();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
        let event = signal(SignalKind::Custom { name: "x".into(), data: Default::default() });
        CardRuntime::new(card.clone()).on_signal(
            &event,
            &mut ctx,
            &Effects::new(&config::Effects::default()),
        );

        // Both wait in the queue, their messages made before their turns.
        let waiting: Vec<(String, String)> =
            activity.queue().waiting.into_iter().map(|item| (item.label, item.text)).collect();
        assert_eq!(waiting, [("Alert".to_string(), "Alert".to_string()), ("Speak".into(), "Alone".into())]);
        activity.play_all();
        assert!(receiver.try_recv().is_err(), "nothing goes to the overlay before its message is made");
        let made: Vec<String> = speech.queued().into_iter().map(|job| job.text).collect();
        assert_eq!(made, ["Thanks", "Alone"]);

        // Without text to speech, the alert doesn't wait.
        let runs = alert_runs(&run(card, &[(0, event)]));
        assert_eq!(runs[0]["values"]["read"]["text"], "Thanks");
        assert!(runs[0]["values"]["read"].get("speech").is_none());
    }

    #[test]
    fn alerts_show_emotes_unless_turned_off() {
        // The text always brings its emotes; the style says whether the page
        // shows them.
        let card = |changes: serde_json::Value| {
            let mut alert = alert("hi {e:Kappa}");
            if let NodeKind::Plugin { changes: own, .. } = &mut alert {
                *own = changes.as_object().unwrap().clone();
            }
            card_of(
                vec![(1, NodeKind::Custom { name: String::new() }), (2, alert)],
                &[(1, "event", 2, "trigger")],
            )
        };
        let run_of = |card: Card| {
            let event = signal(SignalKind::Custom { name: "x".into(), data: Default::default() });
            alert_runs(&run(card, &[(0, event)])).remove(0)
        };
        let shown = run_of(card(serde_json::json!({})));
        assert_eq!(shown["values"]["message"]["parts"].as_array().unwrap().len(), 2, "text and Kappa");
        assert_eq!(shown["settings"]["emotes"], true);
        let off = run_of(card(serde_json::json!({ "emotes": false })));
        assert_eq!(off["settings"]["emotes"], false, "only the text");
    }

    #[test]
    fn alerts_show_the_emotes_marked_in_messages() {
        let kappa = EmoteRef { source: "twitch".into(), id: "25".into(), code: "Kappa".into() };
        let message = EmoteText::from_message(
            "so hype Kappa",
            &[EmoteSpan { start: 8, end: 13, emote: kappa.clone() }],
            |_| None,
        );
        // A viewer named like an emote stays a name.
        let mut viewer = user("v");
        viewer.display_name = "Kappa".into();
        let sub = signal(SignalKind::Membership {
            user: viewer,
            kind: MembershipKind::New,
            tier: None,
            message: Some(message),
        });
        let card = card_of(
            vec![
                (
                    1,
                    NodeKind::Sub {
                        new: true,
                        resub: true,
                        gifted: true,
                        in_gift_bomb: false,
                        prime_to_paid: false,
                        gift_continued: false,
                    },
                ),
                (2, alert("{viewer} subscribed: {message}")),
            ],
            &[(1, "event", 2, "trigger")],
        );
        let runs = alert_runs(&run(card, &[(0, sub)]));
        let message = &runs[0]["values"]["message"];
        assert_eq!(message["text"], "Kappa subscribed: so hype Kappa");
        assert_eq!(
            message["parts"],
            serde_json::json!([
                { "type": "text", "text": "Kappa subscribed: so hype " },
                { "type": "emote", "code": "Kappa", "url": "/emote/twitch/25", "overlays": [] }
            ])
        );
    }

    #[test]
    fn follows_and_their_premade_card() {
        let follow = |name: &str| signal(SignalKind::Follow { user: user(name) });
        let values = match_event(&NodeKind::Follow, &follow("f")).unwrap();
        assert_eq!(values["summary"], Value::Text("F followed".into()));
        // The same viewer again within the hour (unfollow, refollow) is skipped;
        // others aren't.
        let commands = run(premade("follow-alert"), &[(0, follow("a")), (1, follow("b")), (2, follow("a"))]);
        assert_eq!(alerts(&commands), ["A followed!", "B followed!"]);
    }

    #[test]
    fn hype_trains_and_their_premade_card() {
        use crate::model::HypeTrain;
        let train = |change, level: u32| {
            signal(SignalKind::HypeTrain {
                change,
                train: HypeTrain {
                    id: "t".into(),
                    level,
                    progress: 340,
                    goal: 500,
                    total: 840,
                    kind: "regular".into(),
                    shared: false,
                    seconds_left: 200,
                },
            })
        };
        let started = train(HypeTrainChange::Started, 1);
        let progressed = train(HypeTrainChange::Progressed { level_up: false }, 2);
        let level_up = train(HypeTrainChange::Progressed { level_up: true }, 3);
        let ended = train(HypeTrainChange::Ended, 3);
        let node = |started, levelled_up, progressed, ended| NodeKind::HypeTrain {
            started,
            levelled_up,
            progressed,
            ended,
        };
        let fires = |node: &NodeKind| {
            [&started, &progressed, &level_up, &ended].map(|signal| match_event(node, signal).is_some())
        };
        assert_eq!(fires(&node(true, false, false, false)), [true, false, false, false]);
        assert_eq!(fires(&node(false, true, false, false)), [false, false, true, false]);
        // Progress is every contribution, level-ups included.
        assert_eq!(fires(&node(false, false, true, false)), [false, true, true, false]);
        assert_eq!(fires(&node(false, false, false, true)), [false, false, false, true]);

        let values = match_event(&node(true, true, true, true), &progressed).unwrap();
        let number = |name: &str| values[name].number();
        assert_eq!(
            [number("level"), number("progress"), number("goal"), number("total"), number("seconds_left")],
            [Some(2.0), Some(340.0), Some(500.0), Some(840.0), Some(200.0)]
        );
        assert_eq!(values["train"], Value::Text("Hype train".into()));
        assert_eq!(values["summary"], Value::Text("The hype train is at level 2: 340 of 500 points".into()));
        let summary =
            |signal: &Signal| match_event(&node(true, true, true, true), signal).unwrap()["summary"].text();
        assert_eq!(summary(&started), "A hype train started".into());
        assert_eq!(summary(&level_up), "The hype train reached level 3".into());
        assert_eq!(summary(&ended), "The hype train ended at level 3".into());
        let mut golden = started.clone();
        if let SignalKind::HypeTrain { train, .. } = &mut golden.kind {
            train.kind = "golden_kappa".into();
        }
        assert_eq!(summary(&golden), "A Golden Kappa train started".into());

        // The premade card: start, level-ups and end, not every contribution.
        let commands = run(
            premade("hype-train-alert"),
            &[(0, started.clone()), (1, progressed.clone()), (2, level_up.clone()), (3, ended.clone())],
        );
        assert_eq!(
            alerts(&commands),
            ["A hype train started", "The hype train reached level 3", "The hype train ended at level 3"]
        );
    }

    #[test]
    fn the_hype_train_bar_follows_the_train() {
        use crate::model::HypeTrain;
        let train = |change, level, progress, goal, seconds_left| {
            signal(SignalKind::HypeTrain {
                change,
                train: HypeTrain {
                    id: "t".into(),
                    level,
                    progress,
                    goal,
                    total: 0,
                    kind: "golden_kappa".into(),
                    shared: false,
                    seconds_left,
                },
            })
        };
        let commands = run(
            premade("hype-train-bar"),
            &[
                (0, train(HypeTrainChange::Started, 1, 100, 500, 300)),
                (1, train(HypeTrainChange::Progressed { level_up: false }, 1, 400, 500, 290)),
                (2, train(HypeTrainChange::Progressed { level_up: true }, 2, 50, 1000, 300)),
                (3, train(HypeTrainChange::Ended, 2, 0, 0, 0)),
            ],
        );
        let runs: Vec<serde_json::Value> = commands
            .iter()
            .filter_map(|command| match command {
                Command::Plugin(command) if command.plugin == protocol::plugin::HYPE_TRAIN_PLUGIN => {
                    let protocol::plugin::PluginMessage::Run { values, .. } = &command.message else {
                        return None;
                    };
                    Some(serde_json::Value::Object(values.clone()))
                }
                _ => None,
            })
            .collect();
        assert!(runs.iter().all(|values| values["class"]["text"] == "golden_kappa"), "{runs:?}");
        let shown: Vec<_> = runs
            .iter()
            .map(|values| {
                (
                    values["title"]["text"].as_str().unwrap().to_string(),
                    [&values["level"], &values["progress"], &values["goal"], &values["seconds_left"]]
                        .map(|value| value.as_f64().unwrap()),
                )
            })
            .collect();
        let title = "Golden Kappa train".to_string();
        assert_eq!(
            shown,
            [
                (title.clone(), [1.0, 100.0, 500.0, 300.0]),
                (title.clone(), [1.0, 400.0, 500.0, 290.0]),
                (title.clone(), [2.0, 50.0, 1000.0, 300.0]),
                (title, [2.0, 0.0, 0.0, 0.0]),
            ],
            "every contribution, right away"
        );
    }

    #[test]
    fn has_text_routes_by_whether_there_is_any() {
        // A resub: its message in the alert, or a plain alert without one.
        let card = card_of(
            vec![
                (
                    1,
                    NodeKind::Sub {
                        new: true,
                        resub: true,
                        gifted: true,
                        in_gift_bomb: false,
                        prime_to_paid: true,
                        gift_continued: true,
                    },
                ),
                (2, NodeKind::HasText { text: "{message}".into() }),
                (3, alert("{viewer}: {message}")),
                (4, alert("{viewer} resubscribed")),
            ],
            &[(1, "event", 2, "event"), (2, "event", 3, "trigger"), (2, "miss", 4, "trigger")],
        );
        assert!(card.problems().is_empty(), "{:?}", card.problems());
        let resub = |message: Option<&str>| {
            signal(SignalKind::Membership {
                user: user("v"),
                kind: MembershipKind::Renewal { months: 9 },
                tier: None,
                message: message.map(EmoteText::from),
            })
        };
        let commands = run(card, &[(0, resub(Some("hi"))), (1, resub(None)), (2, resub(Some("  ")))]);
        assert_eq!(alerts(&commands), ["V: hi", "V resubscribed", "V resubscribed"]);
    }

    #[test]
    fn compare_text_routes_by_its_texts() {
        // Chat that says one of the words: an alert with the word it said,
        // else one with the message.
        let compare = |all| NodeKind::CompareText {
            text: "{message}".into(),
            test: cards::TextTest::ContainsWord,
            to: vec!["hype".into(), "Pog".into()],
            all,
            ignore_case: true,
        };
        let card = |all| {
            card_of(
                vec![
                    (1, NodeKind::ChatMessage),
                    (2, compare(all)),
                    (3, alert("")),
                    (4, alert("Miss: {message}")),
                ],
                &[
                    (1, "event", 2, "event"),
                    (2, "event", 3, "trigger"),
                    (2, "matched", 3, "message"),
                    (2, "miss", 4, "trigger"),
                ],
            )
        };
        assert!(card(false).problems().is_empty(), "{:?}", card(false).problems());
        let chat =
            |text: &str| signal(SignalKind::ChatMessage { user: user("v"), text: text.into(), first: false });
        let said = [(0, chat("POG!")), (1, chat("so hyped")), (2, chat("hype pog"))];
        assert_eq!(alerts(&run(card(false), &said)), ["Pog", "Miss: so hyped", "hype"]);
        assert_eq!(alerts(&run(card(true), &said)), ["Miss: POG!", "Miss: so hyped", "hype"]);
    }

    #[test]
    fn compare_text_against_a_wired_text() {
        // A reward whose viewer's text names the reward: the wired text
        // stands in for the list.
        let card = card_of(
            vec![
                (1, NodeKind::Reward { reward: String::new() }),
                (
                    2,
                    NodeKind::CompareText {
                        text: "{text}".into(),
                        test: cards::TextTest::Is,
                        to: vec!["never".into()],
                        all: false,
                        ignore_case: true,
                    },
                ),
                (3, alert("Same: {reward}")),
            ],
            &[(1, "event", 2, "event"), (1, "reward", 2, "to"), (2, "event", 3, "trigger")],
        );
        assert!(card.problems().is_empty(), "{:?}", card.problems());
        let commands =
            run(card, &[(0, reward("Hydrate", Some("hydrate"))), (1, reward("Hydrate", Some("never")))]);
        assert_eq!(alerts(&commands), ["Same: Hydrate"]);
    }

    #[test]
    fn compare_number_routes_by_value() {
        // Cheers of 1,000 bits or more get the big alert.
        let card = |test, number, and| {
            card_of(
                vec![
                    (1, NodeKind::Bits { min: 0.0 }),
                    (2, NodeKind::CompareNumber { test, value: 0.0, number, and }),
                    (3, alert("Big: {amount}")),
                    (4, alert("Small: {amount}")),
                ],
                &[
                    (1, "event", 2, "event"),
                    (1, "amount", 2, "value"),
                    (2, "event", 3, "trigger"),
                    (2, "miss", 4, "trigger"),
                ],
            )
        };
        let at_least = card(NumberTest::AtLeast, 1000.0, 0.0);
        assert!(at_least.problems().is_empty(), "{:?}", at_least.problems());
        let cheers = [(0, bits(999)), (1, bits(1000)), (2, bits(5000))];
        assert_eq!(alerts(&run(at_least, &cheers)), ["Small: 999", "Big: 1000", "Big: 5000"]);
        // Between, the numbers either way round, both included.
        let between = card(NumberTest::Between, 5000.0, 1000.0);
        assert_eq!(alerts(&run(between, &cheers)), ["Small: 999", "Big: 1000", "Big: 5000"]);
        assert_eq!(alerts(&run(card(NumberTest::Is, 999.0, 0.0), &cheers[..2])), ["Big: 999", "Small: 1000"]);
    }

    #[test]
    fn delay_sends_the_event_on_later_with_its_values() {
        let card = card_of(
            vec![
                (1, NodeKind::Reward { reward: String::new() }),
                (2, NodeKind::Delay { seconds: 5.0 }),
                (3, alert("Later: {viewer} {reward}")),
            ],
            &[(1, "event", 2, "event"), (2, "event", 3, "trigger")],
        );
        assert!(card.problems().is_empty(), "{:?}", card.problems());
        let steps = |until: u64| {
            vec![
                (0, Step::Signal(reward("Hydrate", None))),
                (1000, Step::Signal(reward("Stretch", None))),
                (4999, Step::Tick),
                (until, Step::Tick),
            ]
        };
        assert!(alerts(&run_ticking(card.clone(), &steps(5999))).len() == 1, "only the first is due");
        assert_eq!(alerts(&run_ticking(card.clone(), &steps(4999))), Vec::<String>::new(), "not yet");
        assert_eq!(
            alerts(&run_ticking(card, &steps(6000))),
            ["Later: V Hydrate", "Later: V Stretch"],
            "each with its own event's values"
        );
    }

    #[test]
    fn a_delay_holds_only_so_many() {
        let card = card_of(
            vec![(1, NodeKind::ChatMessage), (2, NodeKind::Delay { seconds: 1.0 }), (3, rain_node(1.0))],
            &[(1, "event", 2, "event"), (2, "event", 3, "trigger")],
        );
        let chat = signal(SignalKind::ChatMessage { user: user("v"), text: "hi".into(), first: false });
        let mut steps: Vec<(u64, Step)> =
            (0..MAX_DELAYED + 5).map(|_| (0, Step::Signal(chat.clone()))).collect();
        steps.push((1000, Step::Tick));
        assert_eq!(rain_counts(&run_ticking(card, &steps)).len(), MAX_DELAYED, "the extra ones were dropped");
    }

    #[test]
    fn huge_times_wait_forever_instead_of_overflowing() {
        // A Delay, a Gate and a Timer with times far past u64 milliseconds:
        // nothing fires, then or much later (no overflow, so no panic in a
        // debug build and no firing at once in a release one).
        let card = card_of(
            vec![
                (1, NodeKind::ChatMessage),
                (2, NodeKind::Delay { seconds: 1e300 }),
                (3, alert("Delayed")),
                (4, NodeKind::Reward { reward: String::new() }),
                (5, NodeKind::Gate { seconds: 1e300 }),
                (6, alert("Through")),
                (7, NodeKind::Timer { minutes: 1e300, from_start: true }),
                (8, alert("Tick")),
            ],
            &[
                (1, "event", 2, "event"),
                (2, "event", 3, "trigger"),
                (1, "event", 5, "open"),
                (4, "event", 5, "event"),
                (5, "event", 6, "trigger"),
                (1, "event", 7, "start"),
                (7, "event", 8, "trigger"),
            ],
        );
        assert!(card.problems().is_empty(), "{:?}", card.problems());
        let chat = signal(SignalKind::ChatMessage { user: user("v"), text: "hi".into(), first: false });
        let steps = [
            (1000, Step::Tick),
            (2000, Step::Signal(chat)),
            (3000, Step::Tick),
            (4000, Step::Signal(reward("Late", None))),
            (u64::MAX - 1, Step::Tick),
        ];
        assert_eq!(alerts(&run_ticking(card, &steps)), ["Through"], "the gate is open, and stays so");
    }

    #[test]
    fn every_nth_counts_and_starts_over_on_reset() {
        // Every 3rd chat message shows its count; a reward resets.
        let card = card_of(
            vec![
                (1, NodeKind::ChatMessage),
                (2, NodeKind::EveryNth { every: 3.0 }),
                (3, alert("")),
                (4, NodeKind::Reward { reward: String::new() }),
                (5, alert("Miss {message}")),
            ],
            &[
                (1, "event", 2, "event"),
                (2, "event", 3, "trigger"),
                (2, "count", 3, "message"),
                (2, "miss", 5, "trigger"),
                (4, "event", 2, "reset"),
            ],
        );
        assert!(card.problems().is_empty(), "{:?}", card.problems());
        let chat =
            |text: &str| signal(SignalKind::ChatMessage { user: user("v"), text: text.into(), first: false });
        let said = [
            (0, chat("a")),
            (1, chat("b")),
            (2, chat("c")),
            (3, chat("d")),
            (4, reward("Reset", None)),
            (5, chat("e")),
            (6, chat("f")),
            (7, chat("g")),
        ];
        assert_eq!(alerts(&run(card, &said)), ["Miss a", "Miss b", "3", "Miss d", "Miss e", "Miss f", "3"]);
    }

    #[test]
    fn a_timer_ticks_while_running() {
        // Every minute from the start; a reward stops it, chat starts it
        // again (its first tick a minute later).
        let card = |from_start| {
            card_of(
                vec![
                    (1, NodeKind::Timer { minutes: 1.0, from_start }),
                    (2, alert("{summary}")),
                    (3, NodeKind::Reward { reward: String::new() }),
                    (4, NodeKind::ChatMessage),
                ],
                &[(1, "event", 2, "trigger"), (3, "event", 1, "stop"), (4, "event", 1, "start")],
            )
        };
        assert!(card(true).problems().is_empty(), "{:?}", card(true).problems());
        let chat = signal(SignalKind::ChatMessage { user: user("v"), text: "hi".into(), first: false });
        let steps = || {
            vec![
                (0, Step::Tick),
                (59_999, Step::Tick),
                (60_000, Step::Tick),
                (120_000, Step::Tick),
                (130_000, Step::Signal(reward("Stop", None))),
                (180_000, Step::Tick),
                (200_000, Step::Signal(chat.clone())),
                (250_000, Step::Tick),
                (260_000, Step::Tick),
            ]
        };
        assert_eq!(alerts(&run_ticking(card(true), &steps())), ["Timer", "Timer", "Timer"]);
        assert_eq!(alerts(&run_ticking(card(false), &steps())), ["Timer"], "only once started");

        // Off, it doesn't tick; "Test this card" ticks it once anyway.
        let mut off = card(true);
        off.enabled = false;
        assert!(alerts(&run_ticking(off.clone(), &steps())).is_empty());
        let registry = EmoteRegistry::for_tests();
        let hub = OverlayHub::new();
        let mut receiver = hub.subscribe();
        let mut rng = FixedRandom::new(&[0.3]);
        let speech = Speech::off();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
        let effects = Effects::new(&config::Effects::default());
        assert!(CardRuntime::new(off).test_timers(&origin(), &mut ctx, &effects));
        activity.play_all();
        let commands: Vec<Command> =
            std::iter::from_fn(|| receiver.try_recv().ok().map(|envelope| envelope.command.clone()))
                .collect();
        assert_eq!(alerts(&commands), ["Timer"]);
    }

    #[test]
    fn counts_timers_and_delays_carry_over_a_config_change() {
        let card = card_of(
            vec![
                (1, NodeKind::ChatMessage),
                (2, NodeKind::EveryNth { every: 3.0 }),
                (3, alert("Nth")),
                (4, NodeKind::Delay { seconds: 10.0 }),
                (5, NodeKind::Timer { minutes: 1.0, from_start: true }),
                (6, alert("Later")),
                (7, alert("Tick")),
            ],
            &[
                (1, "event", 2, "event"),
                (2, "event", 3, "trigger"),
                (1, "event", 4, "event"),
                (4, "event", 6, "trigger"),
                (5, "event", 7, "trigger"),
            ],
        );
        let registry = EmoteRegistry::for_tests();
        let hub = OverlayHub::new();
        let mut rng = FixedRandom::new(&[0.3]);
        let speech = Speech::off();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
        let effects = Effects::new(&config::Effects::default());
        let chat = signal(SignalKind::ChatMessage { user: user("v"), text: "hi".into(), first: false });
        let mut old = CardRuntime::new(card.clone());
        old.on_tick(&origin(), &mut ctx, &effects);
        old.on_signal(&chat, &mut ctx, &effects);
        old.on_signal(&chat, &mut ctx, &effects);

        let mut edited = card.clone();
        edited.name = "Renamed".into();
        let mut new = CardRuntime::new(edited);
        new.carry_over(old);
        assert_eq!(
            (new.counts.get(&2), new.timers.get(&5), new.delayed.len()),
            (Some(&2), Some(&Some(60_000)), 2)
        );

        // A node replaced by another kind under the same id starts afresh;
        // a card switched off drops its timers and live delays.
        let mut changed = card.clone();
        changed.nodes[1].kind = NodeKind::Chance { percent: 50.0 };
        changed.enabled = false;
        let mut newer = CardRuntime::new(changed);
        newer.carry_over(new);
        assert_eq!((newer.counts.len(), newer.timers.len(), newer.delayed.len()), (0, 0, 0));
    }

    #[test]
    fn first_time_chatters() {
        let card = card_of(
            vec![(1, NodeKind::FirstChatter), (2, alert("{summary}: {message}"))],
            &[(1, "event", 2, "trigger")],
        );
        let chat =
            |text: &str, first| signal(SignalKind::ChatMessage { user: user("v"), text: text.into(), first });
        let said = [(0, chat("hello", true)), (1, chat("again", false))];
        assert_eq!(alerts(&run(card, &said)), ["V chatted for the first time: hello"]);
    }

    #[test]
    fn chat_commands_for_the_right_roles() {
        let card = preset("Read chat aloud", "c");
        assert!(card.problems().is_empty(), "{:?}", card.problems());
        let mut values = None;
        let chat = |text: &str, roles: Roles| {
            let mut viewer = user("v");
            viewer.roles = roles;
            signal(SignalKind::ChatMessage { user: viewer, text: text.into(), first: false })
        };
        let command = &card.nodes[0].kind;
        if let Some(found) = match_event(command, &chat("!TTS Hello streamer", Roles::default())) {
            values = Some(found);
        }
        let values = values.expect("the command matches");
        assert_eq!(values["text"], Value::Text("Hello streamer".into()));
        assert_eq!(values["summary"], Value::Text("V: Hello streamer".into()));
        assert!(match_event(command, &chat("hi !tts", Roles::default())).is_none());

        // Viewer roles: a mod gets through; a viewer goes out of Miss.
        let roles = |kind: NodeKind| {
            card_of(
                vec![
                    (1, NodeKind::ChatCommand { command: "!tts".into() }),
                    (2, kind),
                    (3, alert("in")),
                    (4, alert("out")),
                ],
                &[(1, "event", 2, "event"), (2, "event", 3, "trigger"), (2, "miss", 4, "trigger")],
            )
        };
        let mods =
            NodeKind::ViewerRoles { broadcaster: true, moderator: true, vip: false, subscriber: false };
        let a_mod = Roles { moderator: true, ..Roles::default() };
        assert_eq!(alerts(&run(roles(mods.clone()), &[(0, chat("!tts hi", a_mod))])), ["in"]);
        assert_eq!(alerts(&run(roles(mods), &[(0, chat("!tts hi", Roles::default()))])), ["out"]);
        let subs =
            NodeKind::ViewerRoles { broadcaster: false, moderator: false, vip: false, subscriber: true };
        let a_sub = Roles { member: true, ..Roles::default() };
        assert_eq!(alerts(&run(roles(subs), &[(0, chat("!tts hi", a_sub))])), ["in"]);
    }

    #[test]
    fn alert_template_from_the_preset() {
        let mut card = preset("Channel point alert", "p");
        if let NodeKind::Reward { reward } = &mut card.nodes[0].kind {
            *reward = "Hydrate".into();
        }
        let commands = run(card, &[(0, reward("Hydrate", Some("water!"))), (1, reward("Hydrate", None))]);
        assert_eq!(alerts(&commands), ["V redeemed Hydrate: water!", "V redeemed Hydrate"]);
    }

    /// A plugin with one effect node: Pieces (a number input with its
    /// setting), Caption (a text input with its setting), Emotes (an input
    /// only), and From, Shout and Picture (settings of their own).
    fn confetti() -> protocol::plugin::Catalog {
        let manifest = serde_json::from_value(serde_json::json!({
            "id": "confetti", "name": "Confetti", "version": "1", "api": 1,
            "nodes": [{
                "id": "burst", "title": "Confetti", "category": "effects",
                "inputs": [
                    { "name": "trigger", "label": "Trigger", "type": "event" },
                    { "name": "pieces", "label": "Pieces", "type": "number" },
                    { "name": "caption", "label": "Caption", "type": "text" },
                    { "name": "emotes", "label": "Emotes", "type": "emotes" }
                ],
                "settings": [
                    { "key": "pieces", "label": "Pieces", "type": "number", "default": 150, "max": 1000 },
                    { "key": "caption", "label": "Caption", "type": "text", "default": "{viewer}!" },
                    { "key": "from", "label": "From", "type": "choice",
                      "options": [{ "value": "bottom", "label": "Bottom" }, { "value": "top", "label": "Top" }] },
                    { "key": "shout", "label": "Shout", "type": "text", "default": "{amount} bits {e:Kappa}" },
                    { "key": "picture", "label": "Picture", "type": "picture" },
                    { "key": "ding", "label": "Sound", "type": "sound", "default": "ding.mp3" },
                    { "key": "pictures", "label": "Pictures", "type": "picture", "many": true,
                      "when": { "is": { "from": "top" } } }
                ]
            }]
        }))
        .unwrap();
        protocol::plugin::Catalog::new(vec![protocol::plugin::PluginInfo {
            manifest,
            overlay: true,
            ..Default::default()
        }])
    }

    fn plugin_runs(commands: &[Command]) -> Vec<serde_json::Value> {
        commands
            .iter()
            .filter_map(|command| match command {
                Command::Plugin(command) => Some(serde_json::to_value(&command.message).unwrap()),
                _ => None,
            })
            .collect()
    }

    /// A rain-like plugin node: Emotes (or its setting, codes or random)
    /// and Count; "per message" only with emotes wired in, the count only
    /// without.
    fn shower() -> protocol::plugin::Catalog {
        let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
            "id": "shower", "name": "Shower", "version": "0.1.0", "api": 1,
            "settings": [{ "key": "lifetime_ms", "label": "Lifetime", "type": "number", "default": 8000,
                           "min": 1000, "max": 60000, "unit": "ms" }],
            "nodes": [{
                "id": "shower", "title": "Shower", "category": "effects", "changes": ["lifetime_ms"],
                "inputs": [
                    { "name": "trigger", "label": "Trigger", "type": "event" },
                    { "name": "emotes", "label": "Emotes", "type": "emotes" },
                    { "name": "count", "label": "Count", "type": "number" }
                ],
                "settings": [
                    { "key": "emotes", "label": "Emotes", "type": "emotes", "default": ["Kappa", "Nope"], "count": "count" },
                    { "key": "count", "label": "Count", "type": "number", "default": 3, "min": 0,
                      "when": { "unwired": "emotes" } },
                    { "key": "per_message", "label": "Rain", "type": "choice", "when": { "wired": "emotes" },
                      "options": [{ "value": "each", "label": "Every emote" }, { "value": "one", "label": "One" }] }
                ]
            }]
        }))
        .unwrap();
        assert_eq!(manifest.problems(), Vec::<String>::new());
        protocol::plugin::Catalog::new(vec![protocol::plugin::PluginInfo {
            manifest,
            overlay: true,
            ..Default::default()
        }])
    }

    /// A plugin node that reads its Text aloud, in the queue if ticked.
    fn teller() -> protocol::plugin::Catalog {
        let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
            "id": "teller", "name": "Teller", "version": "0.1.0", "api": 1,
            "nodes": [{
                "id": "tell", "title": "Tell", "category": "effects", "queue": "optional", "describe": "text",
                "inputs": [
                    { "name": "trigger", "label": "Trigger", "type": "event" },
                    { "name": "text", "label": "Text", "type": "text", "speak": true }
                ],
                "settings": [
                    { "key": "text", "label": "Text", "type": "text", "default": "Hello there" },
                    { "key": "queue", "label": "Wait in the queue", "type": "bool" }
                ]
            }]
        }))
        .unwrap();
        assert_eq!(manifest.problems(), Vec::<String>::new());
        protocol::plugin::Catalog::new(vec![protocol::plugin::PluginInfo {
            manifest,
            overlay: true,
            ..Default::default()
        }])
    }

    #[test]
    fn plugin_runs_bring_the_viewer_when_asked() {
        let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
            "id": "line", "name": "Line", "version": "0.1.0", "api": 1,
            "nodes": [{
                "id": "show", "title": "Line", "category": "effects", "viewer": true,
                "inputs": [{ "name": "trigger", "label": "Trigger", "type": "event" }]
            }]
        }))
        .unwrap();
        assert_eq!(manifest.problems(), Vec::<String>::new());
        let catalog = protocol::plugin::Catalog::new(vec![protocol::plugin::PluginInfo {
            manifest,
            overlay: true,
            ..Default::default()
        }]);
        let run = |start: NodeKind, event: Signal| {
            let card =
                card_of(vec![(1, start), (2, catalog.node_kinds().remove(0))], &[(1, "event", 2, "trigger")]);
            let hub = OverlayHub::new();
            let mut receiver = hub.subscribe();
            let registry = EmoteRegistry::for_tests();
            let mut rng = FixedRandom::new(&[0.5]);
            let speech = Speech::off();
            let activity = Activity::for_tests(hub.clone(), speech.clone());
            let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
            CardRuntime::new(card).on_signal(&event, &mut ctx, &Effects::new(&config::Effects::default()));
            let envelope = receiver.try_recv().expect("a run");
            plugin_runs(std::slice::from_ref(&envelope.command)).remove(0)["values"]["viewer"].clone()
        };
        let viewer = run(NodeKind::Bits { min: 1.0 }, bits(100));
        assert_eq!((viewer["platform"].as_str(), viewer["name"].as_str()), (Some("twitch"), Some("a")));
        let nobody = run(
            NodeKind::Custom { name: String::new() },
            signal(SignalKind::Custom { name: String::new(), data: Default::default() }),
        );
        assert_eq!(nobody, serde_json::Value::Null, "a Custom event has no viewer");
    }

    #[test]
    fn plugin_runs_bring_their_presets_settings() {
        let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
            "id": "banner", "name": "Banner", "version": "0.1.0", "api": 1,
            "presets": { "label": "Style" },
            "settings": [
                { "key": "size", "label": "Size", "type": "number", "default": 40, "min": 10, "max": 100 },
                { "key": "colour", "label": "Colour", "type": "colour", "default": "#ffffff" }
            ],
            "nodes": [{
                "id": "show", "title": "Banner", "category": "effects", "changes": ["size"],
                "inputs": [{ "name": "trigger", "label": "Trigger", "type": "event" }],
                "settings": [{ "key": "style", "label": "Style", "type": "preset" }]
            }]
        }))
        .unwrap();
        assert_eq!(manifest.problems(), Vec::<String>::new());
        let catalog = protocol::plugin::Catalog::new(vec![protocol::plugin::PluginInfo {
            manifest,
            overlay: true,
            ..Default::default()
        }]);
        let mut plugins = std::collections::BTreeMap::new();
        let preset = |id: &str, settings: serde_json::Value| config::Preset {
            id: id.into(),
            name: id.into(),
            settings: settings.as_object().unwrap().clone(),
        };
        plugins.insert(
            "banner".to_string(),
            config::PluginConfig {
                presets: vec![
                    preset("default", serde_json::json!({ "size": 50 })),
                    preset("loud", serde_json::json!({ "size": 90, "colour": "#ff0000" })),
                ],
                ..Default::default()
            },
        );
        let settings_of = |style: &str, changes: serde_json::Value| {
            let mut node = catalog.node_kinds().remove(0);
            if let NodeKind::Plugin { settings, changes: own, .. } = &mut node {
                settings.insert("style".into(), style.into());
                *own = changes.as_object().unwrap().clone();
            }
            let card = card_of(
                vec![(1, NodeKind::Custom { name: String::new() }), (2, node)],
                &[(1, "event", 2, "trigger")],
            );
            let hub = OverlayHub::new();
            let mut receiver = hub.subscribe();
            let registry = EmoteRegistry::for_tests();
            let mut rng = FixedRandom::new(&[0.5]);
            let speech = Speech::off();
            let activity = Activity::for_tests(hub.clone(), speech.clone());
            let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
            let effects = Effects::new(&config::Effects::default()).with_plugins(&plugins);
            let custom = signal(SignalKind::Custom { name: String::new(), data: Default::default() });
            CardRuntime::new(card).on_signal(&custom, &mut ctx, &effects);
            let envelope = receiver.try_recv().expect("a run");
            plugin_runs(std::slice::from_ref(&envelope.command)).remove(0)["settings"].clone()
        };
        // The node's preset, every setting with a value.
        assert_eq!(
            settings_of("loud", serde_json::json!({})),
            serde_json::json!({ "size": 90.0, "colour": "#ff0000" })
        );
        // None, or one that's gone: the first.
        assert_eq!(settings_of("", serde_json::json!({}))["size"], 50.0);
        assert_eq!(settings_of("gone", serde_json::json!({}))["colour"], "#ffffff");
        // Its changes on top.
        assert_eq!(settings_of("loud", serde_json::json!({ "size": 20 }))["size"], 20.0);
    }

    #[test]
    fn plugin_inputs_read_aloud() {
        let tell = |queue: bool, voice: Overrides| {
            let mut tell = teller().node_kinds().remove(0);
            if let NodeKind::Plugin { settings, voice: own, .. } = &mut tell {
                settings.insert("queue".into(), queue.into());
                *own = voice;
            }
            card_of(
                vec![(1, NodeKind::Custom { name: String::new() }), (2, tell)],
                &[(1, "event", 2, "trigger")],
            )
        };
        let custom = signal(SignalKind::Custom { name: String::new(), data: Default::default() });
        let made = |job: &crate::services::speech::Job| {
            Some(crate::services::speech::Made {
                file: format!("{}.wav", job.text),
                duration_ms: Some(1_500),
            })
        };
        let runs_from =
            |receiver: &mut tokio::sync::broadcast::Receiver<std::sync::Arc<protocol::Envelope>>| {
                let mut commands = Vec::new();
                while let Ok(envelope) = receiver.try_recv() {
                    commands.push(envelope.command.clone());
                }
                plugin_runs(&commands)
            };
        let fire = |card: Card, speech: &Speech, activity: &Activity, hub: &OverlayHub| {
            let registry = EmoteRegistry::for_tests();
            let mut rng = FixedRandom::new(&[0.5]);
            let mut ctx = Ctx::for_tests(&mut rng, &registry, hub, speech, activity);
            CardRuntime::new(card).on_signal(&custom, &mut ctx, &Effects::new(&config::Effects::default()));
        };

        // Right away: sent once its message is made, with the node's voice.
        let hub = OverlayHub::new();
        let mut receiver = hub.subscribe();
        let speech = Speech::queue_only();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let mut faster = Overrides::new();
        faster.insert("speed".into(), 1.5.into());
        fire(tell(false, faster), &speech, &activity, &hub);
        let early = runs_from(&mut receiver);
        assert!(early.is_empty(), "nothing before its message is made: {early:?}");
        let jobs = speech.queued();
        assert_eq!((jobs[0].text.as_str(), jobs[0].speed), ("Hello there", 1.5));
        speech.finish(made);
        let runs = runs_from(&mut receiver);
        let text = &runs[0]["values"]["text"];
        assert_eq!(text["text"], "Hello there");
        assert_eq!(text["speech"]["url"], "/speech/Hello%20there%2Ewav");
        assert_eq!(text["speech"]["duration_ms"], 1_500);

        // In the queue: made before its turn, then staged with it.
        let hub = OverlayHub::new();
        let mut receiver = hub.subscribe();
        let speech = Speech::queue_only();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let _page = activity.pages(vec!["teller".into()]);
        fire(tell(true, Overrides::new()), &speech, &activity, &hub);
        // Activity shows its text, under its node's title.
        let waiting = &activity.queue().waiting[0];
        assert_eq!((waiting.label.as_str(), waiting.text.as_str()), ("Tell", "Hello there"));
        activity.play_all();
        assert!(runs_from(&mut receiver).is_empty(), "not staged before its message is made");
        speech.finish(made);
        activity.play_all();
        let runs = runs_from(&mut receiver);
        assert_eq!(runs[0]["values"]["text"]["speech"]["url"], "/speech/Hello%20there%2Ewav");
        assert!(runs[0]["queue_id"].is_u64());

        // A message that can't be made: it plays without it.
        let hub = OverlayHub::new();
        let mut receiver = hub.subscribe();
        let speech = Speech::queue_only();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        fire(tell(false, Overrides::new()), &speech, &activity, &hub);
        speech.finish(|_| None);
        let runs = runs_from(&mut receiver);
        assert_eq!(runs[0]["values"]["text"]["text"], "Hello there");
        assert!(runs[0]["values"]["text"].get("speech").is_none());

        // Without text to speech: right away, without one.
        let runs = plugin_runs(&run(tell(false, Overrides::new()), &[(0, custom.clone())]));
        assert_eq!(runs[0]["values"]["text"]["text"], "Hello there");
        assert!(runs[0]["values"]["text"].get("speech").is_none());
    }

    #[test]
    fn plugin_emotes_settings_and_settings_shown_while_wired() {
        let shower = shower().node_kinds().remove(0);
        // Nothing wired into Emotes: its setting's known codes, Count times.
        let custom = signal(SignalKind::Custom { name: String::new(), data: Default::default() });
        let card = card_of(
            vec![(1, NodeKind::Custom { name: String::new() }), (2, shower.clone())],
            &[(1, "event", 2, "trigger")],
        );
        let runs = plugin_runs(&run(card, &[(0, custom.clone())]));
        let values = &runs[0]["values"];
        let codes: Vec<&str> = values["emotes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|emote| emote["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, ["Kappa", "Kappa", "Kappa"], "only the codes the channel knows");
        assert_eq!(values["count"], 3.0);
        assert!(values.get("per_message").is_none(), "only shown with emotes wired in");

        // Random: from the channel's emotes.
        let mut random = shower.clone();
        if let NodeKind::Plugin { settings, .. } = &mut random {
            settings.insert("emotes".into(), "random".into());
            settings.insert("count".into(), 2.into());
        }
        let card = card_of(
            vec![(1, NodeKind::Custom { name: String::new() }), (2, random)],
            &[(1, "event", 2, "trigger")],
        );
        let runs = plugin_runs(&run(card, &[(0, custom)]));
        assert_eq!(runs[0]["values"]["emotes"].as_array().unwrap().len(), 2);

        // Emotes wired in from chat: those, the per-message setting, and no
        // count (none is wired).
        let card = card_of(
            vec![(1, NodeKind::EmotesInChat), (2, shower)],
            &[(1, "event", 2, "trigger"), (1, "emotes", 2, "emotes")],
        );
        let message = signal(SignalKind::EmotesPosted {
            user: user("a"),
            emotes: vec![posted("PogU", &[]), posted("Kappa", &["RainTime"])],
        });
        let runs = plugin_runs(&run(card, &[(0, message)]));
        let values = &runs[0]["values"];
        assert_eq!(values["emotes"].as_array().unwrap().len(), 2);
        assert_eq!(values["emotes"][1]["overlays"].as_array().unwrap().len(), 1);
        assert_eq!(values["per_message"], "each");
        assert!(values.get("count").is_none(), "the count's setting isn't shown with emotes wired in");
        assert!(runs[0].get("changes").is_none(), "it changes none of the plugin's settings");

        // A node that changes one of the plugin's own settings: kept within
        // its bounds; one its type can't change isn't sent.
        let mut changing = self::shower().node_kinds().remove(0);
        if let NodeKind::Plugin { changes, .. } = &mut changing {
            changes.insert("lifetime_ms".into(), 999_999.into());
            changes.insert("gravity".into(), 3.into());
        }
        let card = card_of(
            vec![(1, NodeKind::Custom { name: String::new() }), (2, changing)],
            &[(1, "event", 2, "trigger")],
        );
        let custom = signal(SignalKind::Custom { name: String::new(), data: Default::default() });
        let runs = plugin_runs(&run(card, &[(0, custom)]));
        assert_eq!(runs[0]["changes"], serde_json::json!({ "lifetime_ms": 60000.0 }));
    }

    #[test]
    fn a_plugin_effect_gets_its_values() {
        let catalog = confetti();
        let mut burst = catalog.node_kinds().remove(0);
        if let NodeKind::Plugin { settings, .. } = &mut burst {
            settings.insert("from".into(), "top".into());
        }
        // Bits → confetti, with the amount × 10 wired into Pieces.
        let math = NodeKind::Math { op: MathOp::Multiply, a: 0.0, b: 10.0 };
        let card = card_of(
            vec![(1, NodeKind::Bits { min: 0.0 }), (2, math), (3, burst.clone())],
            &[(1, "event", 3, "trigger"), (1, "amount", 2, "a"), (2, "result", 3, "pieces")],
        );
        let runs = plugin_runs(&run(card, &[(0, bits(50)), (1, bits(500))]));
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["node"], "burst");
        assert_eq!(runs[0]["input"], "trigger");
        let values = &runs[0]["values"];
        assert_eq!(values["pieces"], 500.0);
        assert_eq!(values["caption"]["text"], "A!", "the setting, filled from the event");
        assert_eq!(values["from"], "top");
        assert_eq!(values["shout"]["text"], "50 bits Kappa");
        assert_eq!(values["shout"]["parts"][1]["type"], "emote", "{{e:Kappa}} is the emote");
        assert_eq!(values["picture"], serde_json::Value::Null);
        assert_eq!(values["ding"], protocol::sound::sound_path("ding.mp3"), "a sound is its path");
        assert!(values.get("emotes").is_none(), "nothing wired into it");
        assert_eq!(runs[1]["values"]["pieces"], 1000.0, "kept within the setting's max");

        // Without its plugin, the node keeps its place, and the card doesn't run.
        let mut missing =
            card_of(vec![(1, NodeKind::Bits { min: 0.0 }), (3, burst)], &[(1, "event", 3, "trigger")]);
        missing.attach_plugins(&protocol::plugin::Catalog::default());
        let problems = missing.problems();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].message.contains("confetti plugin isn't installed"));
        missing.drop_dangling_wires();
        assert_eq!(missing.wires.len(), 1, "its wires stay for when it's back");
    }

    #[test]
    fn a_plugin_event_fills_its_outputs() {
        let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
            "id": "billybilly", "name": "BillyBilly", "version": "1.0.0", "api": 1,
            "nodes": [{ "id": "membership", "title": "BillyBilly membership", "category": "events",
                "outputs": [
                    { "name": "event", "label": "Event", "type": "event" },
                    { "name": "ended", "label": "Ended", "type": "event" },
                    { "name": "viewer", "label": "Viewer", "type": "text" },
                    { "name": "months", "label": "Months", "type": "number" }
                ] }]
        }))
        .unwrap();
        let catalog = protocol::plugin::Catalog::new(vec![protocol::plugin::PluginInfo {
            manifest,
            program: true,
            connected: true,
            ..Default::default()
        }]);
        let membership = catalog.node_kinds().remove(0);
        let burst = confetti().node_kinds().remove(0);
        let card = card_of(
            vec![
                (1, membership),
                (2, alert("{viewer} for {months} months: {summary}")),
                (3, burst),
                (4, alert("{viewer} ended")),
            ],
            &[
                (1, "event", 2, "trigger"),
                (1, "event", 3, "trigger"),
                (1, "months", 3, "pieces"),
                (1, "ended", 4, "trigger"),
            ],
        );
        assert_eq!(card.problems(), []);
        let values = |json: serde_json::Value| -> BTreeMap<String, PluginValue> {
            json.as_object()
                .unwrap()
                .iter()
                .map(|(name, value)| {
                    let value = match value {
                        serde_json::Value::Number(number) => PluginValue::Number(number.as_f64().unwrap()),
                        other => PluginValue::Text(other.as_str().unwrap().into()),
                    };
                    (name.clone(), value)
                })
                .collect()
        };
        let event = |plugin: &str, node: &str, sent: serde_json::Value| {
            signal(SignalKind::Plugin {
                plugin: plugin.into(),
                node: node.into(),
                values: values(sent),
                output: None,
                to: None,
            })
        };
        // `emit-output`: another of its event outputs.
        let ended = signal(SignalKind::Plugin {
            plugin: "billybilly".into(),
            node: "membership".into(),
            values: values(serde_json::json!({ "viewer": "Bo" })),
            output: Some("ended".into()),
            to: None,
        });
        let commands = run(
            card,
            &[
                (0, event("billybilly", "membership", serde_json::json!({ "viewer": "Ann", "months": "3" }))),
                (1, event("billybilly", "other", serde_json::json!({}))),
                (2, event("someone-else", "membership", serde_json::json!({}))),
                (3, ended),
            ],
        );
        assert_eq!(alerts(&commands), ["Ann for 3 months: BillyBilly membership", "Bo ended"]);
        let runs: Vec<_> = plugin_runs(&commands).into_iter().filter(|run| run["node"] == "burst").collect();
        assert_eq!(runs.len(), 1, "only its own plugin's node");
        assert_eq!(runs[0]["values"]["pieces"], 3.0, "a text that reads as a number");
    }

    #[test]
    fn a_plugin_event_node_filters_by_its_settings() {
        let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
            "id": "obs", "name": "OBS", "version": "1.0.0", "api": 1,
            "nodes": [{ "id": "scene_changed", "title": "Scene changed", "category": "events",
                "outputs": [
                    { "name": "event", "label": "Event", "type": "event" },
                    { "name": "scene", "label": "Scene", "type": "text" }
                ],
                "settings": [
                    { "key": "scene", "label": "Scenes", "type": "text", "many": true },
                    { "key": "label", "label": "Label", "type": "text", "default": "Nope" },
                    { "key": "seconds", "label": "Seconds", "type": "number", "default": 5 }
                ] }]
        }))
        .unwrap();
        let catalog = protocol::plugin::Catalog::new(vec![protocol::plugin::PluginInfo {
            manifest,
            program: true,
            connected: true,
            ..Default::default()
        }]);
        let mut changed = catalog.node_kinds().remove(0);
        let NodeKind::Plugin { settings, .. } = &mut changed else { unreachable!() };
        settings.insert("scene".into(), serde_json::json!(["Gaming", " be right back "]));
        let card = card_of(vec![(1, changed), (2, alert("On air: {scene}"))], &[(1, "event", 2, "trigger")]);
        assert_eq!(card.problems(), []);
        let event = |scene: &str| {
            let values = BTreeMap::from([("scene".to_string(), PluginValue::Text(scene.into()))]);
            signal(SignalKind::Plugin {
                plugin: "obs".into(),
                node: "scene_changed".into(),
                values,
                output: None,
                to: None,
            })
        };
        let commands = run(
            card.clone(),
            &[(0, event("Gaming")), (1, event("Just chatting")), (2, event("Be right back"))],
        );
        assert_eq!(
            alerts(&commands),
            ["On air: Gaming", "On air: Be right back"],
            "the picked ones, any case; its other settings are its server code's"
        );

        // `emit-to`: one placed copy of the node fires, not another's.
        let aimed = |to: &str| {
            let mut event = event("Gaming");
            if let SignalKind::Plugin { to: aimed, .. } = &mut event.kind {
                *aimed = Some(to.into());
            }
            event
        };
        assert_eq!(placed_id("c", 1), "c/1");
        assert_eq!(alerts(&run(card.clone(), &[(0, aimed("c/1"))])), ["On air: Gaming"]);
        assert_eq!(alerts(&run(card.clone(), &[(0, aimed("other/1"))])), Vec::<String>::new());
        assert_eq!(alerts(&run(card, &[(0, aimed("c/2"))])), Vec::<String>::new(), "another node");
    }

    /// A plugin with server code and a flow node, Split: Event in; Event,
    /// Miss and Count out; a Label setting.
    fn splitter() -> protocol::plugin::Catalog {
        let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
            "id": "splitter", "name": "Splitter", "version": "0.1.0", "api": 1,
            "nodes": [{
                "id": "split", "title": "Split", "category": "flow",
                "inputs": [{ "name": "event", "label": "Event", "type": "event" }],
                "outputs": [
                    { "name": "event", "label": "Event", "type": "event" },
                    { "name": "miss", "label": "Miss", "type": "event" },
                    { "name": "count", "label": "Count", "type": "number" }
                ],
                "settings": [{ "key": "label", "label": "Label", "type": "text", "default": "{viewer} split" }]
            }]
        }))
        .unwrap();
        assert_eq!(manifest.problems(), Vec::<String>::new());
        protocol::plugin::Catalog::new(vec![protocol::plugin::PluginInfo {
            manifest,
            server: true,
            ..Default::default()
        }])
    }

    /// Bits → Split; its Event and its Miss each → Confetti with that
    /// caption, Split's Count wired into Pieces.
    fn split_card() -> Card {
        let burst = |caption: &str| {
            let mut burst = confetti().node_kinds().remove(0);
            if let NodeKind::Plugin { settings, .. } = &mut burst {
                settings.insert("caption".into(), caption.into());
            }
            burst
        };
        card_of(
            vec![
                (1, NodeKind::Bits { min: 0.0 }),
                (2, splitter().node_kinds().remove(0)),
                (3, burst("went on")),
                (4, burst("missed")),
            ],
            &[
                (1, "event", 2, "event"),
                (2, "event", 3, "trigger"),
                (2, "count", 3, "pieces"),
                (2, "miss", 4, "trigger"),
                (2, "count", 4, "pieces"),
            ],
        )
    }

    /// Confetti's runs: their caption and pieces.
    fn bursts(commands: &[Command]) -> Vec<(String, f64)> {
        plugin_runs(commands)
            .iter()
            .filter(|run| run["node"] == "burst")
            .map(|run| {
                let values = &run["values"];
                (values["caption"]["text"].as_str().unwrap().to_string(), values["pieces"].as_f64().unwrap())
            })
            .collect()
    }

    /// What a plugin's server code says to a `route` or a `compute`, as
    /// `server.rs` acts on it.
    enum Answer {
        Go(&'static str, Vec<(String, Value)>),
        Stop,
        Now(Vec<(String, Value)>),
        Later,
    }

    /// A stand-in for a plugin's server code: the nth question → its answer.
    type Answering<'a> = dyn FnMut(usize, &Asked) -> Answer + 'a;

    /// Runs `card` for each signal, with `plugin`'s server code answering
    /// the nth question with `answer(n, question)` (`None`: it isn't
    /// running), and the card carrying on from the answers as the pipeline
    /// does. Then `later` gets the activity and the runs answered later, to
    /// answer them (or let their time run out). Returns the overlay
    /// commands, and what the plugin was asked.
    fn run_asked(
        card: Card,
        signals: &[Signal],
        effects: &Effects,
        plugin: &str,
        answer: Option<&mut Answering>,
        later: impl FnOnce(&Activity, &[u64]),
    ) -> (Vec<Command>, Vec<Asked>) {
        let registry = EmoteRegistry::for_tests();
        let hub = OverlayHub::new();
        let mut receiver = hub.subscribe();
        let mut rng = FixedRandom::new(&[0.5]);
        let speech = Speech::off();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let mut resumes = activity.resumes();
        let mut running = answer.map(|answer| (activity.asks(plugin), answer));
        let mut runtime = CardRuntime::new(card);
        let mut asked = Vec::new();
        let mut waiting = Vec::new();
        // Answers and resumes, until nothing more comes of them.
        let mut settle = |runtime: &mut CardRuntime,
                          rng: &mut FixedRandom,
                          asked: &mut Vec<Asked>,
                          waiting: &mut Vec<u64>| loop {
            let mut more = false;
            if let Some(((_, inbox), answer)) = &mut running {
                while let Ok(question) = inbox.try_recv() {
                    more = true;
                    let run = match &question {
                        Asked::Route { run, .. } | Asked::Compute { run, .. } => *run,
                        Asked::Heard => continue,
                    };
                    match answer(asked.len(), &question) {
                        Answer::Go(output, values) => {
                            activity.carry_on_with(plugin, run, output, values).unwrap();
                            activity.forget_run(plugin, run);
                        }
                        Answer::Stop => activity.forget_run(plugin, run),
                        Answer::Now(values) => activity.computed(plugin, run, values).unwrap(),
                        Answer::Later => waiting.push(run),
                    }
                    asked.push(question);
                }
            }
            while let Ok(resume) = resumes.try_recv() {
                more = true;
                let mut ctx = Ctx::for_tests(rng, &registry, &hub, &speech, &activity);
                ctx.cause = resume.cause.clone();
                runtime.resume(&resume.then, &resume.output, &mut ctx, effects);
            }
            if !more {
                break;
            }
        };
        for signal in signals {
            let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
            runtime.on_signal(signal, &mut ctx, effects);
            settle(&mut runtime, &mut rng, &mut asked, &mut waiting);
        }
        later(&activity, &waiting);
        settle(&mut runtime, &mut rng, &mut asked, &mut waiting);
        (
            std::iter::from_fn(|| receiver.try_recv().ok().map(|envelope| envelope.command.clone()))
                .collect(),
            asked,
        )
    }

    #[test]
    fn a_plugin_flow_node_routes_by_its_answer() {
        let effects = Effects::new(&config::Effects::default());
        let mut answers = |index: usize, _: &Asked| match index {
            0 => Answer::Go("event", vec![("count".into(), Value::Number(7.0))]),
            _ => Answer::Go("miss", Vec::new()),
        };
        let (commands, asked) = run_asked(
            split_card(),
            &[bits(10), bits(20)],
            &effects,
            "splitter",
            Some(&mut answers),
            |_, _| {},
        );
        assert_eq!(
            bursts(&commands),
            [("went on".to_string(), 7.0), ("missed".to_string(), 150.0)],
            "what it gave for this event, else the setting"
        );
        assert_eq!(asked.len(), 2);
        let Asked::Route { node, input, values, viewer, origin, .. } = &asked[0] else { panic!("a route") };
        assert_eq!((node.as_str(), input.as_str()), ("split", "event"));
        assert_eq!(values["label"]["text"], "A split", "its settings, filled from the event");
        assert_eq!(viewer.as_ref().map(|viewer| viewer.user.name.as_str()), Some("a"), "always the viewer");
        assert_eq!(*origin, ItemOrigin::Live);
    }

    #[test]
    fn a_plugin_flow_node_stops_waits_or_lets_nothing_through() {
        let effects = Effects::new(&config::Effects::default());
        let run = |answer: Option<&mut Answering>, later: &dyn Fn(&Activity, &[u64])| {
            run_asked(split_card(), &[bits(10)], &effects, "splitter", answer, later)
        };
        let (commands, asked) = run(Some(&mut |_, _| Answer::Stop), &|_, _| {});
        assert_eq!((bursts(&commands), asked.len()), (Vec::new(), 1), "stopped");

        let (commands, _) = run(Some(&mut |_, _| Answer::Later), &|activity, runs| {
            let three = vec![("count".to_string(), Value::Number(3.0))];
            activity.carry_on_with("splitter", runs[0], "miss", three).unwrap();
            assert!(activity.carry_on_with("splitter", runs[0], "miss", Vec::new()).is_err(), "once");
        });
        assert_eq!(bursts(&commands), [("missed".to_string(), 3.0)], "answered later");

        let (commands, _) = run(None, &|_, _| {});
        assert_eq!(bursts(&commands), [], "its server code isn't running");

        let off = Effects::new(&config::Effects::default()).with_plugins(&plugins_off("splitter"));
        let mut go = |_, _: &Asked| Answer::Go("event", Vec::new());
        let (commands, asked) =
            run_asked(split_card(), &[bits(10)], &off, "splitter", Some(&mut go), |_, _| {});
        assert_eq!((bursts(&commands), asked.len()), (Vec::new(), 0), "switched off");
    }

    /// The plugins' stored settings with `plugin` switched off.
    fn plugins_off(plugin: &str) -> BTreeMap<String, config::PluginConfig> {
        BTreeMap::from([(
            plugin.to_string(),
            serde_json::from_value::<config::PluginConfig>(serde_json::json!({ "enabled": false })).unwrap(),
        )])
    }

    #[test]
    fn a_parked_event_keeps_as_json() {
        let mut then = Then::for_tests("c", 2, bits(10));
        then.fill(vec![("count".into(), Value::Number(3.0))]);
        then.into = vec![(3, "trigger".into())];
        let json = serde_json::to_value(&then).unwrap();
        assert_eq!(json["filled"]["2/count"]["Number"], 3.0);
        assert_eq!(json["computed"], serde_json::json!([2]));
        assert_eq!(serde_json::from_value::<Then>(json).unwrap(), then);
    }

    /// A plugin with server code and two value nodes: Money (Bits in, its
    /// setting 100; Money and Label out) and Double (Value in, its setting
    /// 1; Value out).
    fn money() -> protocol::plugin::Catalog {
        let manifest: protocol::plugin::Manifest = serde_json::from_value(serde_json::json!({
            "id": "money", "name": "Money", "version": "0.1.0", "api": 1,
            "nodes": [{
                "id": "money", "title": "Bits to money", "category": "values",
                "inputs": [{ "name": "bits", "label": "Bits", "type": "number" }],
                "outputs": [
                    { "name": "money", "label": "Money", "type": "number" },
                    { "name": "label", "label": "Label", "type": "text" }
                ],
                "settings": [{ "key": "bits", "label": "Bits", "type": "number", "default": 100 }]
            }, {
                "id": "double", "title": "Double", "category": "values",
                "inputs": [{ "name": "value", "label": "Value", "type": "number" }],
                "outputs": [{ "name": "value", "label": "Value", "type": "number" }],
                "settings": [{ "key": "value", "label": "Value", "type": "number", "default": 1 }]
            }]
        }))
        .unwrap();
        assert_eq!(manifest.problems(), Vec::<String>::new());
        protocol::plugin::Catalog::new(vec![protocol::plugin::PluginInfo {
            manifest,
            server: true,
            ..Default::default()
        }])
    }

    /// Bits → Confetti, the cheer's amount into Money's Bits, and
    /// `wires` from Money (node 2) and Double (node 4) into Confetti
    /// (node 3) and each other.
    fn money_card(wires: &[(NodeId, &str, NodeId, &str)]) -> Card {
        let kinds = money().node_kinds();
        let mut all = vec![(1, "event", 3, "trigger"), (1, "amount", 2, "bits")];
        all.extend_from_slice(wires);
        card_of(
            vec![
                (1, NodeKind::Bits { min: 0.0 }),
                (2, kinds[0].clone()),
                (3, confetti().node_kinds().remove(0)),
                (4, kinds[1].clone()),
            ],
            &all,
        )
    }

    /// A question's number value by name.
    fn asked_number(question: &Asked, name: &str) -> f64 {
        let Asked::Compute { values, .. } = question else { panic!("a compute") };
        values[name].as_f64().unwrap()
    }

    #[test]
    fn a_plugin_value_node_gives_an_effect_its_input() {
        let effects = Effects::new(&config::Effects::default());
        // Money's Money into Pieces, its Label into Caption: one question
        // per event for both.
        let card = money_card(&[(2, "money", 3, "pieces"), (2, "label", 3, "caption")]);
        let mut answer = |_, question: &Asked| {
            let bits = asked_number(question, "bits");
            Answer::Now(vec![
                ("money".into(), Value::Number(bits / 100.0)),
                ("label".into(), Value::Text(format!("${}", bits / 100.0).into())),
            ])
        };
        let (commands, asked) =
            run_asked(card, &[bits(500), bits(200)], &effects, "money", Some(&mut answer), |_, _| {});
        assert_eq!(bursts(&commands), [("$5".to_string(), 5.0), ("$2".to_string(), 2.0)]);
        assert_eq!(asked.len(), 2, "once per event");
        let Asked::Compute { node, origin, .. } = &asked[0] else { panic!("a compute") };
        assert_eq!((node.as_str(), *origin), ("money", ItemOrigin::Live));
    }

    #[test]
    fn plugin_value_nodes_in_a_chain_are_asked_in_order() {
        let effects = Effects::new(&config::Effects::default());
        // Money → Double → Pieces.
        let card = money_card(&[(2, "money", 4, "value"), (4, "value", 3, "pieces")]);
        let mut answer = |_, question: &Asked| match question {
            Asked::Compute { node, .. } if node == "money" => {
                Answer::Now(vec![("money".into(), Value::Number(asked_number(question, "bits") / 100.0))])
            }
            _ => Answer::Now(vec![("value".into(), Value::Number(asked_number(question, "value") * 2.0))]),
        };
        let (commands, asked) =
            run_asked(card, &[bits(300)], &effects, "money", Some(&mut answer), |_, _| {});
        assert_eq!(bursts(&commands), [("A!".to_string(), 6.0)]);
        let nodes: Vec<&str> = asked
            .iter()
            .map(|question| match question {
                Asked::Compute { node, .. } => node.as_str(),
                Asked::Route { .. } | Asked::Heard => "other",
            })
            .collect();
        assert_eq!(nodes, ["money", "double"], "the deepest first");
    }

    #[test]
    fn a_plugin_value_node_reached_two_ways_is_asked_once() {
        let effects = Effects::new(&config::Effects::default());
        // Money into Pieces, and through Double into Caption.
        let card =
            money_card(&[(2, "money", 3, "pieces"), (2, "money", 4, "value"), (4, "value", 3, "caption")]);
        let mut answer = |_, question: &Asked| match question {
            Asked::Compute { node, .. } if node == "money" => {
                Answer::Now(vec![("money".into(), Value::Number(asked_number(question, "bits") / 100.0))])
            }
            _ => Answer::Now(vec![("value".into(), Value::Number(asked_number(question, "value") * 2.0))]),
        };
        let (commands, asked) =
            run_asked(card, &[bits(300)], &effects, "money", Some(&mut answer), |_, _| {});
        assert_eq!(bursts(&commands), [("6".to_string(), 3.0)]);
        assert_eq!(asked.len(), 2, "Money once, then Double");
    }

    #[test]
    fn a_number_on_a_wire_is_finite() {
        for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(Value::Number(number).into_type(SocketType::Number), None, "{number}");
        }
        assert_eq!(Value::Text("inf".into()).into_type(SocketType::Number), None, "a text that says so");
        assert_eq!(Value::Text(" 2.5 ".into()).into_type(SocketType::Number), Some(Value::Number(2.5)));
    }

    #[test]
    fn a_plugin_value_node_answers_later_or_too_late() {
        let effects = Effects::new(&config::Effects::default());
        let card = || money_card(&[(2, "money", 3, "pieces")]);
        let (commands, _) = run_asked(
            card(),
            &[bits(500)],
            &effects,
            "money",
            Some(&mut |_, _| Answer::Later),
            |activity, runs| {
                activity.computed("money", runs[0], vec![("money".into(), Value::Number(9.0))]).unwrap();
                assert!(activity.computed("money", runs[0], Vec::new()).is_err(), "answered already");
            },
        );
        assert_eq!(bursts(&commands), [("A!".to_string(), 9.0)], "answered later");

        let (commands, _) = run_asked(
            card(),
            &[bits(500)],
            &effects,
            "money",
            Some(&mut |_, _| Answer::Later),
            |activity, runs| {
                activity.compute_overdue(crate::pipeline::now_ms() + 10_000);
                assert!(activity.computed("money", runs[0], Vec::new()).is_err(), "too late");
            },
        );
        assert_eq!(bursts(&commands), [("A!".to_string(), 150.0)], "too late: Pieces' own setting");

        let (commands, asked) = run_asked(card(), &[bits(500)], &effects, "money", None, |_, _| {});
        assert_eq!((bursts(&commands), asked.len()), (vec![("A!".to_string(), 150.0)], 0), "not running");

        let off = Effects::new(&config::Effects::default()).with_plugins(&plugins_off("money"));
        let mut never = |_, _: &Asked| Answer::Now(vec![("money".into(), Value::Number(1.0))]);
        let (commands, asked) = run_asked(card(), &[bits(500)], &off, "money", Some(&mut never), |_, _| {});
        assert_eq!((bursts(&commands), asked.len()), (vec![("A!".to_string(), 150.0)], 0), "switched off");
    }
}
