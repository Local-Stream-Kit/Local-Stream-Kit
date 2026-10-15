//! What a card run leaves for the queue, so a waiting event can be
//! prepared again when the cards change: where the run started, what its
//! nodes that keep state decided, and what it queued.

use std::collections::HashMap;

use protocol::cards::{NodeId, NodeKind};
use serde::{Deserialize, Serialize};

use super::{EventValues, Then};
use crate::services::activity::Play;

/// Where a card run started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Start {
    /// The event came into the card: its event nodes matched it.
    Event,
    /// It carried on from `then`'s node's `output` (Started, Ended, a Delay
    /// sending it on, a Timer's tick, a plugin's flow node's answer or own
    /// output), or into `then.into` once plugins' value nodes answered.
    Then { then: Box<Then>, output: String },
}

/// What a node that keeps state decided in a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// A Cooldown or a Gate let the event through, or not.
    Passed(bool),
    /// An Every Nth's count once it had counted the event.
    Counted(u64),
    /// What an Emote combo or a Chant completed with it, if anything.
    Crowds(Vec<EventValues>),
    /// It held the event there (a Delay, a plugin's flow or value node):
    /// what comes after is a run of its own, later.
    Held,
}

/// A node's decision at its `visit`-th visit in a run, with its settings
/// then (`kind`, a hash of them: changed since, it's worked out afresh).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decided {
    pub node: NodeId,
    pub visit: u32,
    pub kind: u64,
    pub decision: Decision,
}

/// A run of an effect node that waits in the queue: which node, the
/// `nth` time it queued in its card run, and what it plays.
#[derive(Debug, Clone, PartialEq)]
pub struct Queued {
    pub node: NodeId,
    pub nth: u32,
    pub play: Play,
}

/// A card run, as the queue keeps it.
#[derive(Debug, Clone, PartialEq)]
pub struct CardRun {
    /// The card's id.
    pub card: String,
    /// Its name, for people.
    pub name: String,
    pub start: Start,
    pub decided: Vec<Decided>,
    /// In the order it queued them.
    pub queued: Vec<Queued>,
    /// While preparing: it reached something that holds events where it
    /// didn't before, so it can't be prepared without waiting.
    pub unpreparable: bool,
}

/// The card run going on, as it's recorded ([`crate::pipeline::Ctx`]):
/// its nodes' draws, what they decide, what it queues.
#[derive(Default)]
pub struct Recording {
    run: Option<CardRun>,
    /// While preparing: what was decided in this run before, and in the
    /// next one.
    before: Vec<Decided>,
    next_before: Vec<Decided>,
    /// How many times each node has drawn, decided and queued in it.
    draws: HashMap<NodeId, u32>,
    visits: HashMap<NodeId, u32>,
    queued: HashMap<NodeId, u32>,
    /// While preparing: the runs that ended.
    ended: Vec<CardRun>,
    /// Live: the runs of event `pending_event` that decided something but
    /// queued nothing, kept until one of its runs queues something (then
    /// they're kept with it).
    pending: Vec<CardRun>,
    pending_event: u64,
}

impl Recording {
    /// The next run is a run prepared again: what was decided in it
    /// before.
    pub fn prepare_next(&mut self, before: Vec<Decided>) {
        self.next_before = before;
    }

    /// A run of card `card` (named `name`) starts at `start`.
    pub fn begin(&mut self, card: &str, name: &str, start: Start) {
        self.run = Some(CardRun {
            card: card.to_string(),
            name: name.to_string(),
            start,
            decided: Vec::new(),
            queued: Vec::new(),
            unpreparable: false,
        });
        self.before = std::mem::take(&mut self.next_before);
        self.draws.clear();
        self.visits.clear();
        self.queued.clear();
    }

    /// The run has ended: it, if there was one.
    pub fn finish(&mut self) -> Option<CardRun> {
        self.before.clear();
        self.run.take()
    }

    /// While preparing, a run that ended, kept until taken.
    pub fn ended(&mut self, run: CardRun) {
        self.ended.push(run);
    }

    /// While preparing, the runs that have ended, in order.
    pub fn take(&mut self) -> Vec<CardRun> {
        std::mem::take(&mut self.ended)
    }

    /// Live, the runs of event `event` kept until one of its runs queues
    /// something.
    pub fn pending(&mut self, event: u64) -> Vec<CardRun> {
        let pending = std::mem::take(&mut self.pending);
        if self.pending_event == event { pending } else { Vec::new() }
    }

    /// Keeps event `event`'s runs that decided something, until one of its
    /// runs queues something.
    pub fn keep_pending(&mut self, event: u64, runs: Vec<CardRun>) {
        self.pending_event = event;
        self.pending = runs.into_iter().filter(|run| !run.decided.is_empty()).collect();
    }

    /// The how-manyth draw of node `node` this is in the run.
    pub fn draw(&mut self, node: NodeId) -> u32 {
        next(&mut self.draws, node)
    }

    /// Node `node` (of `kind`) decides: what it decided before at this
    /// visit, if its settings are the same (preparing), and the visit, for
    /// [`Recording::decided`].
    pub fn decide(&mut self, node: NodeId, kind: &NodeKind) -> (Option<Decision>, u32, u64) {
        let visit = next(&mut self.visits, node);
        let kind = kind_hash(kind);
        let before = self
            .before
            .iter()
            .find(|decided| (decided.node, decided.visit, decided.kind) == (node, visit, kind));
        (before.map(|decided| decided.decision.clone()), visit, kind)
    }

    /// What node `node` decided at `visit` (with its settings' hash
    /// `kind`).
    pub fn decided(&mut self, node: NodeId, visit: u32, kind: u64, decision: Decision) {
        if let Some(run) = &mut self.run {
            run.decided.push(Decided { node, visit, kind, decision });
        }
    }

    /// While preparing, the run reached something that holds events where
    /// it didn't before.
    pub fn unpreparable(&mut self) {
        if let Some(run) = &mut self.run {
            run.unpreparable = true;
        }
    }

    /// Node `node` queued `play`.
    pub fn queue(&mut self, node: NodeId, play: Play) {
        let nth = next(&mut self.queued, node);
        if let Some(run) = &mut self.run {
            run.queued.push(Queued { node, nth, play });
        }
    }
}

fn next(counts: &mut HashMap<NodeId, u32>, node: NodeId) -> u32 {
    let count = counts.entry(node).or_insert(0);
    *count += 1;
    *count - 1
}

/// A node's settings, hashed (FNV-1a over them as JSON), to see whether
/// they changed. Stable across builds (it's kept with waiting events).
fn kind_hash(kind: &NodeKind) -> u64 {
    let json = serde_json::to_string(kind).unwrap_or_default();
    json.bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}
