//! Waiting events: the events whose queued items still wait, kept with
//! their card runs, so they can be prepared again when the cards change.
//! An item names its place in its event ([`Chain`]); the event keeps where
//! each of its runs started and what was decided on the way, and which of
//! its items have left the queue. It's kept while any of its items waits
//! or is on stage.

use std::collections::{BTreeMap, BTreeSet};

use protocol::activity::{ItemState, Prepared};
use protocol::cards::NodeId;
use serde::{Deserialize, Serialize};

use super::{Activity, Cause, Changes, Item, Play, State, set_state};
use crate::cards::{CardRun, Decided, Start};
use crate::model::Signal;

/// Which cards a waiting event goes to again when it's prepared again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hearers {
    /// Every card that's on (an event).
    #[default]
    Cards,
    /// This card (its id), on or off: a test of one card.
    Card(String),
    /// None: only its own runs carry on again (a Timer's tick, or what
    /// carried on after its event's items had all left).
    Own,
}

/// An item's place in its waiting event: the event, its card run there,
/// its effect node and the how-manyth time that node queued in the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(super) struct Chain {
    pub event: u64,
    pub run: u32,
    pub node: NodeId,
    pub nth: u32,
}

impl Chain {
    /// Its place within its event.
    fn key(self) -> (u32, NodeId, u32) {
        (self.run, self.node, self.nth)
    }
}

/// An event with items waiting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitingEvent {
    pub id: u64,
    pub signal: Signal,
    /// Unix ms when it came.
    pub at_ms: u64,
    pub hearers: Hearers,
    /// Where its items come from (the card's name aside: each run's).
    pub cause: Cause,
    /// Its card runs that decided or queued something; items name theirs
    /// by place.
    pub runs: Vec<KeptRun>,
    /// Its items that have left the queue (on stage, played, skipped,
    /// cleared), by place: not queued again.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub gone: BTreeSet<(u32, NodeId, u32)>,
}

/// A card run of a waiting event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeptRun {
    /// The card's id and name.
    pub card: String,
    pub name: String,
    pub start: Start,
    /// Unix ms when it ran.
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decided: Vec<Decided>,
}

impl Activity {
    /// A new waiting event's id.
    pub fn next_event(&self) -> u64 {
        let mut state = self.lock();
        let id = state.next_event;
        state.next_event += 1;
        id
    }

    /// Card runs of an event ended (`signal`: the event, when they started
    /// from it coming into the card): what they queued waits its turn,
    /// and the runs are kept with the event while any of it waits. When
    /// nothing is kept (the event has nothing waiting and they queued
    /// nothing), the runs come back.
    pub fn ran(
        &self,
        signal: Option<&Signal>,
        cause: &Cause,
        now_ms: u64,
        runs: Vec<CardRun>,
    ) -> Option<Vec<CardRun>> {
        let queued = runs.iter().any(|run| !run.queued.is_empty());
        let kept = self.update(|state, changes| {
            let found = state.events.iter().position(|event| event.id == cause.event_id);
            let index = match found {
                Some(index) => index,
                None if !queued => return Some(runs),
                None => match waiting_event(signal, cause, now_ms, &runs) {
                    Some(event) => {
                        state.events.push(event);
                        state.events.len() - 1
                    }
                    // (Tests that run a card on its own: nothing to keep.)
                    None => {
                        for run in runs {
                            let mut cause = cause.clone();
                            cause.card = run.name;
                            for queued in run.queued {
                                state.push(queued.play, &cause, None, changes);
                            }
                        }
                        return None;
                    }
                },
            };
            for run in runs {
                if run.decided.is_empty() && run.queued.is_empty() {
                    continue;
                }
                let event = &mut state.events[index];
                let place = event.runs.len() as u32;
                event.runs.push(KeptRun {
                    card: run.card,
                    name: run.name.clone(),
                    start: run.start,
                    at_ms: now_ms,
                    decided: run.decided,
                });
                let mut cause = event.cause.clone();
                cause.card = run.name;
                for queued in run.queued {
                    let chain =
                        Chain { event: cause.event_id, run: place, node: queued.node, nth: queued.nth };
                    state.push(queued.play, &cause, Some(chain), changes);
                }
            }
            None
        });
        if kept.is_none() && queued {
            self.inner.wake.notify_one();
        }
        kept
    }
}

/// A new waiting event for runs that queued something: from the event
/// (`signal`) coming into a card, or carrying on from a run of its own
/// (its items had all left). `None` for runs with no event (tests).
fn waiting_event(
    signal: Option<&Signal>,
    cause: &Cause,
    now_ms: u64,
    runs: &[CardRun],
) -> Option<WaitingEvent> {
    if cause.event_id == 0 {
        return None;
    }
    let first = runs.first()?;
    let (signal, hearers) = match (&first.start, signal) {
        (Start::Event, Some(signal)) => (signal.clone(), cause.hearers.clone()),
        (Start::Then { then, .. }, _) => (then.signal().clone(), Hearers::Own),
        (Start::Event, None) => return None,
    };
    let mut cause = cause.clone();
    cause.card.clear();
    Some(WaitingEvent {
        id: cause.event_id,
        signal,
        at_ms: now_ms,
        hearers,
        cause,
        runs: Vec::new(),
        gone: BTreeSet::new(),
    })
}

impl State {
    /// Item `item` has left the queue (`to`: on stage, skipped, cleared):
    /// its event won't queue it again. (Then [`State::forget_events`],
    /// once it's on stage or gone.)
    pub(super) fn left(&mut self, item: &Item, to: ItemState, changes: &mut Changes) {
        set_state(self, item.entry, item.id, to, changes);
        if let Some(chain) = item.chain
            && let Some(event) = self.events.iter_mut().find(|event| event.id == chain.event)
        {
            event.gone.insert(chain.key());
        }
    }

    /// Forgets the waiting events with nothing waiting or on stage.
    pub(super) fn forget_events(&mut self) {
        let mut kept = BTreeSet::new();
        let items = self.stage.iter().map(|stage| &stage.item).chain(&self.waiting);
        kept.extend(items.filter_map(|item| item.chain.map(|chain| chain.event)));
        self.events.retain(|event| kept.contains(&event.id));
    }
}

/// A waiting event prepared again (`Pipeline::prepare_again`): what each
/// of its runs comes out as (`None`: it can't be prepared without
/// waiting, so its items stay as they were), and the runs of cards that
/// hear it now and didn't then.
#[derive(Debug, Default)]
pub struct PreparedEvent {
    pub id: u64,
    pub runs: Vec<Option<CardRun>>,
    pub added: Vec<CardRun>,
}

impl Activity {
    /// The events with items waiting (not only one on stage), in the order
    /// they came: to prepare again.
    pub fn waiting_events(&self) -> Vec<WaitingEvent> {
        let state = self.lock();
        let waiting: BTreeSet<u64> =
            state.waiting.iter().filter_map(|item| item.chain.map(|chain| chain.event)).collect();
        let mut events: Vec<WaitingEvent> =
            state.events.iter().filter(|event| waiting.contains(&event.id)).cloned().collect();
        events.sort_by_key(|event| event.id);
        events
    }

    /// What preparing the waiting events again gave: an item is replaced
    /// where it comes out different, withdrawn where it no longer comes
    /// out, back where it does again, and a new one joins after its
    /// event's last item. What that changed (panels are told too).
    pub fn prepared_again(&self, prepared: Vec<PreparedEvent>) -> Prepared {
        let note = self.update(|state, changes| {
            let mut note = Prepared::default();
            for event in prepared {
                let Some(index) = state.events.iter().position(|kept| kept.id == event.id) else { continue };
                for (run, result) in event.runs.into_iter().enumerate() {
                    let Some(result) = result else { continue };
                    if let Some(kept) = state.events[index].runs.get_mut(run) {
                        kept.name.clone_from(&result.name);
                        kept.decided.clone_from(&result.decided);
                    }
                    state.prepared(event.id, run as u32, &result, changes, &mut note);
                }
                for result in event.added {
                    let kept = &mut state.events[index];
                    let run = kept.runs.len() as u32;
                    kept.runs.push(KeptRun {
                        card: result.card.clone(),
                        name: result.name.clone(),
                        start: result.start.clone(),
                        at_ms: kept.at_ms,
                        decided: result.decided.clone(),
                    });
                    state.prepared(event.id, run, &result, changes, &mut note);
                }
            }
            if note.changed() {
                changes.queue = true;
                changes.prepared = Some(note);
            }
            note
        });
        self.inner.wake.notify_one();
        note
    }
}

impl State {
    /// Run `run` of waiting event `event`, prepared again, came out as
    /// `result`: its items change to match what it queues, adding up what
    /// changed in `note`. One that went on stage before (and came back when
    /// its overlay closed mid-play) stays as it is. A card that adds to the
    /// event is listed with it in the history.
    fn prepared(
        &mut self,
        event: u64,
        run: u32,
        result: &CardRun,
        changes: &mut Changes,
        note: &mut Prepared,
    ) {
        let (name, queued) = (result.name.as_str(), result.queued.clone());
        let Some(kept) = self.events.iter().find(|kept| kept.id == event) else { return };
        let gone = kept.gone.clone();
        let cause = Cause { card: name.to_string(), ..kept.cause.clone() };
        let mut results: BTreeMap<(NodeId, u32), Play> =
            queued.into_iter().map(|queued| ((queued.node, queued.nth), queued.play)).collect();
        let mut states = Vec::new();
        let mut changed = Vec::new();
        for item in &mut self.waiting {
            let Some(chain) = item.chain.filter(|chain| chain.event == event && chain.run == run) else {
                continue;
            };
            let result = results.remove(&(chain.node, chain.nth));
            if gone.contains(&chain.key()) {
                continue;
            }
            match result {
                Some(play) => {
                    if std::mem::take(&mut item.withdrawn) {
                        note.back += 1;
                        states.push((item.entry, item.id, ItemState::Waiting));
                    } else if item.play != play {
                        note.updated += 1;
                    }
                    if item.play != play {
                        // Its message made again, if it changed.
                        if item.play.speech != play.speech {
                            item.made = None;
                        }
                        item.play = play;
                        changed.push(item.id);
                    }
                    name.clone_into(&mut item.card);
                }
                None if !item.withdrawn => {
                    item.withdrawn = true;
                    note.withdrawn += 1;
                    states.push((item.entry, item.id, ItemState::Withdrawn));
                }
                None => {}
            }
        }
        for (entry, id, to) in states {
            set_state(self, entry, id, to, changes);
        }
        for id in changed {
            // (A message being made for what it was is let go.)
            self.making.remove(&id);
            self.rerecord(id, changes);
        }
        for ((node, nth), play) in results {
            if gone.contains(&(run, node, nth)) {
                continue;
            }
            let at = self
                .waiting
                .iter()
                .rposition(|item| item.chain.is_some_and(|chain| chain.event == event))
                .map_or(self.waiting.len(), |last| last + 1);
            self.insert(at, play, &cause, Some(Chain { event, run, node, nth }), changes);
            note.added += 1;
            if let Some(entry) = cause.entry {
                self.fired(entry, &result.card, name, changes);
            }
        }
    }

    /// Item `id` changed: its history entry lists it as it is now.
    fn rerecord(&mut self, id: u64, changes: &mut Changes) {
        let Some(item) = self.waiting.iter().find(|item| item.id == id) else { return };
        let (entry, record) = (item.entry, item.record(ItemState::Waiting));
        if let Some(entry) =
            entry.and_then(|entry| self.history.iter_mut().find(|found| found.view.id == entry))
            && let Some(kept) = entry.view.items.iter_mut().find(|kept| kept.id == id)
        {
            (kept.label, kept.text, kept.parts) = (record.label, record.text, record.parts);
            changes.entries.push(entry.view.id);
        }
    }
}
