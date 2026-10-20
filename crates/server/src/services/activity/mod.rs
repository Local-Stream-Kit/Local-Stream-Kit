//! The queue and the history.
//!
//! **The queue.** Everything that takes the stage (plugins' effects that
//! wait their turn: alerts, spoken messages, sounds and media set to wait)
//! waits here and plays one at a time, first in, first out: the item on
//! stage goes to the overlays showing its plugin's page, and the page says
//! when it's done. Nothing is dropped and nothing expires; the queue and
//! whether it's paused are kept in [`QUEUE_FILE`] across restarts. While no
//! overlay shows the next item's page, it waits. A spoken message (an
//! input read aloud) is made shortly before its turn (the speech cache only
//! keeps the newest few), and made again if its file is gone.
//!
//! **The history.** The last [`HISTORY_LIMIT`](protocol::activity::HISTORY_LIMIT) events of each kind that
//! matter on stream (see `history::describe`; not test events), in [`HISTORY_FILE`],
//! each with the cards it set off and the queue items they made. An entry
//! can be replayed through the cards (by the pipeline). The seen marker's
//! place (the newest entry marked seen) is kept with it.
//!
//! Panels follow both through [`Activity::subscribe`].

mod asked;
mod history;
mod waiting;

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use protocol::Command;
use protocol::activity::{
    EventKind, HistoryEntry, HistoryFilter, ItemOrigin, ItemRecord, ItemState, Moved, NameColor, Prepared,
    QUEUE_LIST_LIMIT, QueueAction, QueueGroup, QueueItem, QueueState, Skipped, within_limit,
};
use protocol::plugin::{
    ENDED_OUTPUT, PluginCommand, PluginMessage, QUEUE_LIMIT_MS, STARTED_OUTPUT, TextPart,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, broadcast, mpsc};

use crate::cards::Then;
use crate::model::Signal;
use crate::pipeline::now_ms;
use crate::services::overlay_hub::OverlayHub;
use crate::services::speech::{Job, Made, Speech};

pub use asked::{Asked, Asks, Heard, MAX_COMPUTING, Pending, ValueAsk};
pub use history::{describe, name_colors};
use waiting::Chain;
pub use waiting::{Hearers, KeptRun, PreparedEvent, WaitingEvent};

/// The queue, next to the config.
pub const QUEUE_FILE: &str = "lsk_queue.json";
/// The history, next to the config.
pub const HISTORY_FILE: &str = "lsk_history.json";

/// The log warns when this many are waiting, and every this many more.
const WARN_EVERY: usize = 50;
/// Spoken messages made ahead of their turn.
const LOOKAHEAD: usize = 3;
/// How long a change waits before the files are written, so a flood of
/// events (a gift bomb, a follow train) writes them once a second, not
/// several times per event.
const SAVE_AFTER: Duration = Duration::from_secs(1);
/// After an item's length, the queue moves on without a `Done` (an overlay
/// closed mid-alert).
const MARGIN_MS: u64 = 10_000;

/// What plays when an item takes the stage: a plugin's effect node's run,
/// with its values as when it fired (or when its event was last prepared
/// again: `waiting.rs`); its page plays it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Play {
    pub plugin: String,
    pub node: String,
    /// The node's title, for people.
    pub title: String,
    /// What it is, for people ([`NodeType::description`]); empty: the
    /// title.
    ///
    /// [`NodeType::description`]: protocol::plugin::NodeType::description
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// `text` with its emotes and text styles, when it has some
    /// ([`NodeType::description_parts`]), for the queue and the history.
    ///
    /// [`NodeType::description_parts`]: protocol::plugin::NodeType::description_parts
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<TextPart>,
    pub input: String,
    pub values: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub changes: serde_json::Map<String, serde_json::Value>,
    /// For a plugin with presets: its own settings for this run.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub settings: serde_json::Map<String, serde_json::Value>,
    /// Its input read aloud: made before its turn, and added to that
    /// input's value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech: Option<Spoken>,
    /// Its node's Started and Ended outputs wired: what they carry on from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub then: Option<Box<Then>>,
    /// Its plugin's server code plays it (and says when it's done), not
    /// its page: it plays while the code runs, overlays or not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub server: bool,
}

/// A queued run went on stage (`output`: Started) or has ended (Ended),
/// or server code fired one of its node's own outputs
/// ([`Activity::carry_on`]), for the card it came from to carry on
/// ([`Play::then`]).
#[derive(Debug, Clone)]
pub struct Resume {
    pub then: Then,
    pub output: String,
    /// As when it was queued.
    pub cause: Cause,
}

/// A plugin run's input read aloud, and the message to make of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spoken {
    pub input: String,
    pub job: Job,
}

impl Play {
    /// For people.
    fn text(&self) -> String {
        if self.text.is_empty() { self.title.clone() } else { self.text.clone() }
    }

    fn speech(&self) -> Option<&Job> {
        self.speech.as_ref().map(|spoken| &spoken.job)
    }
}

/// Where a queued item comes from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cause {
    /// The history entry of the event (none for test events and chat).
    pub entry: Option<u64>,
    /// The event, for people ("TestViewer cheered 500 bits").
    pub event: Option<String>,
    /// Its kind, for clearing one kind (none for chat).
    pub kind: Option<EventKind>,
    pub origin: ItemOrigin,
    /// The card's name; empty for previews.
    pub card: String,
    /// Its viewers' name colours.
    pub names: Vec<NameColor>,
    /// The event's seed: its random draws come from it
    /// ([`Ctx::rng_at`](crate::pipeline::Ctx::rng_at)).
    pub seed: u64,
    /// The event's id among waiting events (0: none, for tests that run a
    /// card on its own), and the cards it goes to when it's prepared again.
    pub event_id: u64,
    pub hearers: Hearers,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Item {
    id: u64,
    queued_at_ms: u64,
    #[serde(default)]
    card: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entry: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event_kind: Option<EventKind>,
    #[serde(default)]
    origin: ItemOrigin,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    names: Vec<NameColor>,
    /// Its event's seed ([`Cause::seed`]).
    #[serde(default)]
    seed: u64,
    /// Its place in its waiting event, to be prepared again (none for
    /// previews, and items queued before there were waiting events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chain: Option<Chain>,
    /// Its event prepared again no longer queues it: it stays in its place,
    /// unlisted, until the queue has played past it, in case a change
    /// brings it back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    withdrawn: bool,
    play: Play,
    /// Its spoken message, once made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    made: Option<Made>,
}

impl Item {
    fn view(&self) -> QueueItem {
        QueueItem {
            id: self.id,
            label: self.play.title.clone(),
            text: self.play.text(),
            parts: self.play.parts.clone(),
            card: self.card.clone(),
            event: self.event.clone(),
            origin: self.origin,
            group: self.group(),
            names: self.names.clone(),
            queued_at_ms: self.queued_at_ms,
        }
    }

    fn group(&self) -> QueueGroup {
        QueueGroup::of(self.event_kind)
    }

    /// What the overlay plays: the run, to its plugin's page. Only once its
    /// speech is made.
    fn command(&self) -> Command {
        let play = &self.play;
        let mut values = play.values.clone();
        if let Some(spoken) = &play.speech
            && let Some(made) = &self.made
        {
            crate::plugins::add_speech(&mut values, &spoken.input, made.clip(spoken.job.volume));
        }
        Command::Plugin(PluginCommand {
            plugin: play.plugin.clone(),
            message: PluginMessage::Run {
                node: play.node.clone(),
                input: play.input.clone(),
                values,
                changes: play.changes.clone(),
                settings: play.settings.clone(),
                queue_id: Some(self.id),
                run_id: None,
                preview: None,
                origin: self.origin,
            },
        })
    }

    /// What stops it early (the panel's Skip).
    fn skip_command(&self) -> Command {
        Command::Plugin(PluginCommand {
            plugin: self.play.plugin.clone(),
            message: PluginMessage::Skip { queue_id: self.id },
        })
    }

    /// How long it plays at most, as far as the server knows: its page says
    /// when it's done.
    fn length_ms(&self) -> u64 {
        QUEUE_LIMIT_MS
    }

    fn record(&self, state: ItemState) -> ItemRecord {
        ItemRecord {
            id: self.id,
            label: self.play.title.clone(),
            text: self.play.text(),
            parts: self.play.parts.clone(),
            state,
        }
    }

    /// For its card to carry on from `output`, if one of them is wired.
    fn resume(&self, output: &'static str) -> Option<Resume> {
        let then = self.play.then.as_deref()?.clone();
        let cause = Cause {
            entry: self.entry,
            event: self.event.clone(),
            kind: self.event_kind,
            origin: self.origin,
            card: self.card.clone(),
            names: self.names.clone(),
            seed: self.seed,
            event_id: self.chain.map_or(0, |chain| chain.event),
            hearers: Hearers::Own,
        };
        Some(Resume { then, output: output.into(), cause })
    }
}

/// The history entry, and the event itself for a replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    #[serde(flatten)]
    view: HistoryEntry,
    signal: Signal,
}

/// What [`QUEUE_FILE`] holds (read back entry by entry: [`load`]).
#[derive(Serialize)]
struct SavedQueue<'a> {
    paused: bool,
    next_id: u64,
    /// The item that was on stage first: it plays again from the start.
    items: Vec<Item>,
    /// The events they come from, to prepare them again (read back each
    /// on its own: one that can't be read is left out, and its items play
    /// as they are).
    events: &'a [WaitingEvent],
    next_event: u64,
}

/// What [`HISTORY_FILE`] holds (read back entry by entry: [`load`]).
#[derive(Serialize)]
struct SavingHistory<'a> {
    next_id: u64,
    /// The seen marker: the newest entry marked seen.
    seen: u64,
    entries: &'a VecDeque<Entry>,
}

struct Stage {
    item: Item,
    /// Unix ms when the queue moves on without a `Done`.
    until_ms: u64,
}

#[derive(Default)]
struct State {
    paused: bool,
    next_item: u64,
    waiting: VecDeque<Item>,
    stage: Option<Stage>,
    /// Overlays with alerts connected.
    players: usize,
    /// Overlays showing each plugin's page, by plugin.
    pages: BTreeMap<String, usize>,
    /// Plugins whose server code is running (counted: a restarted plugin's
    /// new code may start before the old one has stopped).
    servers: BTreeMap<String, usize>,
    /// Items whose spoken message is being made.
    making: HashSet<u64>,
    next_entry: u64,
    /// Newest first.
    history: VecDeque<Entry>,
    /// The newest entry marked seen: the newer ones are new.
    seen: u64,
    /// The events with items waiting or on stage, and the next one's id.
    events: Vec<WaitingEvent>,
    next_event: u64,
    unsaved: Unsaved,
}

/// What changed since the files were last written.
#[derive(Default)]
struct Unsaved {
    queue: bool,
    history: bool,
    /// A save is on its way.
    coming: bool,
}

/// What changed while the state was locked, to save and tell panels.
#[derive(Default)]
struct Changes {
    queue: bool,
    entries: Vec<u64>,
    /// The whole history changed (cleared).
    history: bool,
    /// The seen marker moved.
    seen: bool,
    send: Vec<Command>,
    /// For the pipeline: queued runs that went on stage or ended.
    resumes: Vec<Resume>,
    /// The waiting events were prepared again, and this changed.
    prepared: Option<Prepared>,
}

/// A change for panels.
#[derive(Debug, Clone)]
pub enum Change {
    Queue(QueueState),
    Entry(HistoryEntry),
    /// The whole history, newest first (it was cleared).
    History(Vec<HistoryEntry>),
    /// The seen marker moved: the newest entry marked seen.
    Seen(u64),
    /// The waiting events were prepared again, and this changed.
    Prepared(Prepared),
}

struct Files {
    queue: PathBuf,
    history: PathBuf,
}

/// The queue and the history. Cheap to clone.
#[derive(Clone)]
pub struct Activity {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    /// Wakes [`Activity::run`]: something to play may have changed.
    wake: Notify,
    changes: broadcast::Sender<Change>,
    overlay: OverlayHub,
    speech: Speech,
    /// `None` in tests: nothing is saved.
    files: Option<Files>,
    /// Held while the files are written, so two saves never overlap (and
    /// an older one never lands after a newer one).
    writing: Mutex<()>,
    /// Where [`Resume`]s go (the pipeline), once it asks for them.
    resumes: Mutex<Option<mpsc::UnboundedSender<Resume>>>,
    /// Runs whose server code may fire their node's own outputs, or
    /// events parked at a plugin's flow node.
    runs: Mutex<Runs>,
    /// Running plugins' server code, for what cards ask it.
    inboxes: Mutex<asked::Inboxes>,
    /// Events waiting for plugins' value nodes.
    computings: Mutex<asked::Computings>,
}

/// The most runs [`Activity::carry_on`] remembers of each plugin, of each
/// kind ([`RunKind`]), and for how long.
const RUNS_KEPT: usize = 1000;
const RUN_KEPT_MS: u64 = 60 * 60 * 1000;

/// Runs of effect nodes whose own outputs are wired, and events parked at
/// plugins' flow nodes: what their cards carry on from, oldest first.
#[derive(Default)]
struct Runs {
    next_id: u64,
    waiting: VecDeque<(u64, CarryOn)>,
}

/// What a remembered run is, each kept apart, so a flood of one never
/// pushes out the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunKind {
    /// An effect node's run, whose own outputs its server code may fire.
    Effect,
    /// An event parked at a flow node, until its server code says where it
    /// goes.
    Parked,
}

struct CarryOn {
    plugin: String,
    kind: RunKind,
    then: Then,
    cause: Cause,
    at_ms: u64,
    /// The outputs fired already (each once).
    fired: Vec<String>,
}

impl Activity {
    /// The queue and history saved in `dir` (empty if there are none yet).
    /// Entries that can't be read are set aside in a `.bad-<unix ms>` file
    /// (the whole file, if it can't be read at all); the rest are kept.
    pub fn open(dir: &Path, overlay: OverlayHub, speech: Speech) -> Activity {
        let files = Files { queue: dir.join(QUEUE_FILE), history: dir.join(HISTORY_FILE) };
        let queue: Loaded<Item> = load(&files.queue, "items");
        let history: Loaded<Entry> = load(&files.history, "entries");
        // A history kept before the marker: all of it seen.
        let newest = history.entries.iter().map(|entry| entry.view.id).max().unwrap_or(0);
        let seen = history.fields.get("seen").and_then(serde_json::Value::as_u64).unwrap_or(newest);
        let events = waiting_events(&queue.fields);
        let mut state = State {
            paused: queue.bool("paused"),
            next_item: queue
                .u64("next_id")
                .max(queue.entries.iter().map(|item| item.id + 1).max().unwrap_or(1)),
            next_entry: history
                .u64("next_id")
                .max(history.entries.iter().map(|entry| entry.view.id + 1).max().unwrap_or(1)),
            next_event: queue
                .u64("next_event")
                .max(events.iter().map(|event| event.id + 1).max().unwrap_or(1)),
            waiting: queue.entries.into(),
            history: history.entries.into(),
            seen,
            events,
            ..State::default()
        };
        // Entries kept before they had sizes or name colours get theirs.
        for entry in &mut state.history {
            if let Some(described) = describe(&entry.signal) {
                entry.view.amount = described.amount;
            }
            entry.view.names = name_colors(&entry.signal);
        }
        state.forget_events();
        // What was on stage plays again.
        let waiting: Vec<(Option<u64>, u64)> =
            state.waiting.iter().map(|item| (item.entry, item.id)).collect();
        for (entry, id) in waiting {
            set_state(&mut state, entry, id, ItemState::Waiting, &mut Changes::default());
        }
        if !state.waiting.is_empty() {
            tracing::info!(
                waiting = state.waiting.len(),
                paused = state.paused,
                "the queue carries on from last time once an overlay with alerts connects"
            );
        }
        Activity::new(state, overlay, speech, Some(files))
    }

    #[cfg(test)]
    pub fn for_tests(overlay: OverlayHub, speech: Speech) -> Activity {
        let state = State { next_item: 1, next_entry: 1, next_event: 1, ..State::default() };
        Activity::new(state, overlay, speech, None)
    }

    fn new(state: State, overlay: OverlayHub, speech: Speech, files: Option<Files>) -> Activity {
        let (changes, _) = broadcast::channel(256);
        Activity {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                wake: Notify::new(),
                changes,
                overlay,
                speech,
                files,
                writing: Mutex::new(()),
                resumes: Mutex::new(None),
                runs: Mutex::default(),
                inboxes: Mutex::default(),
                computings: Mutex::default(),
            }),
        }
    }

    /// Queued runs going on stage and ending, for their cards to carry on
    /// (the pipeline's). One taker: asking again replaces the last.
    pub fn resumes(&self) -> mpsc::UnboundedReceiver<Resume> {
        let (sender, receiver) = mpsc::unbounded_channel();
        *self.inner.resumes.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sender);
        receiver
    }

    /// A run of one of `plugin`'s effect nodes whose own outputs are
    /// wired: its id, for its server code to fire them with
    /// ([`Activity::carry_on`]). Kept for an hour, at most [`RUNS_KEPT`]
    /// of the plugin's.
    pub fn remember_run(&self, plugin: &str, then: Then, cause: &Cause) -> u64 {
        self.keep_run(plugin, RunKind::Effect, then, cause)
    }

    /// Remembers a run of `plugin`'s of `kind`, pushing out the plugin's
    /// oldest of that kind when it has [`RUNS_KEPT`] already (never
    /// another plugin's, nor another kind), and any an hour old.
    fn keep_run(&self, plugin: &str, kind: RunKind, then: Then, cause: &Cause) -> u64 {
        let mut runs = self.inner.runs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = now_ms();
        while runs.waiting.front().is_some_and(|(_, run)| now.saturating_sub(run.at_ms) > RUN_KEPT_MS) {
            runs.waiting.pop_front();
        }
        let same = |run: &CarryOn| run.plugin == plugin && run.kind == kind;
        if runs.waiting.iter().filter(|(_, run)| same(run)).count() >= RUNS_KEPT
            && let Some(oldest) = runs.waiting.iter().position(|(_, run)| same(run))
        {
            runs.waiting.remove(oldest);
        }
        runs.next_id += 1;
        let id = runs.next_id;
        let run = CarryOn {
            plugin: plugin.to_string(),
            kind,
            then,
            cause: cause.clone(),
            at_ms: now,
            fired: Vec::new(),
        };
        runs.waiting.push_back((id, run));
        id
    }

    /// `plugin`'s server code fires `output` of run `run`'s node: its card
    /// carries on from there. Refused (why) for a run it doesn't know or
    /// isn't `plugin`'s, or an output fired already.
    pub fn carry_on(&self, plugin: &str, run: u64, output: &str) -> Result<(), String> {
        self.carry_on_with(plugin, run, output, Vec::new())
    }

    /// As [`carry_on`](Self::carry_on), with what the node's value outputs
    /// carry from there on (a flow node's), by output.
    pub fn carry_on_with(
        &self,
        plugin: &str,
        run: u64,
        output: &str,
        values: Vec<(String, crate::cards::Value)>,
    ) -> Result<(), String> {
        let resume = {
            let mut runs = self.inner.runs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some((_, waiting)) =
                runs.waiting.iter_mut().find(|(id, waiting)| *id == run && waiting.plugin == plugin)
            else {
                return Err(format!("no run {run} (an hour old, from before a restart, or not its own)"));
            };
            if waiting.fired.iter().any(|fired| fired == output) {
                return Err(format!("run {run} fired {output:?} already"));
            }
            waiting.fired.push(output.to_string());
            let mut then = waiting.then.clone();
            then.fill(values);
            Resume { then, output: output.to_string(), cause: waiting.cause.clone() }
        };
        self.resume_card(resume);
        Ok(())
    }

    /// Forgets run `run` of `plugin`'s (a flow node's that sent its event
    /// nowhere), if it has it.
    pub fn forget_run(&self, plugin: &str, run: u64) {
        let mut runs = self.inner.runs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        runs.waiting.retain(|(id, waiting)| *id != run || waiting.plugin != plugin);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        self.inner.changes.subscribe()
    }

    pub fn queue(&self) -> QueueState {
        queue_state(&self.lock())
    }

    /// Newest first.
    pub fn history(&self) -> Vec<HistoryEntry> {
        self.lock().history.iter().map(|entry| entry.view.clone()).collect()
    }

    /// History entry `id`, if it's still kept.
    pub fn history_entry(&self, id: u64) -> Option<HistoryEntry> {
        self.lock().history.iter().find(|entry| entry.view.id == id).map(|entry| entry.view.clone())
    }

    /// What queue item `id` shows or says: while it's in the queue, or as
    /// its history entry kept it once it has left (a test's or preview's,
    /// with no entry, is then gone).
    pub fn item_text(&self, id: u64) -> Option<String> {
        let state = self.lock();
        let queued =
            state.stage.iter().map(|stage| &stage.item).chain(&state.waiting).find(|item| item.id == id);
        queued.map(|item| item.play.text()).or_else(|| {
            let mut kept = state.history.iter().flat_map(|entry| &entry.view.items);
            kept.find(|item| item.id == id).map(|item| item.text.clone())
        })
    }

    /// The seen marker: the newest entry marked seen.
    pub fn seen(&self) -> u64 {
        self.lock().seen
    }

    /// How many events `filter` shows above the seen marker.
    pub fn new_events(&self, filter: &HistoryFilter) -> usize {
        let state = self.lock();
        filter.new_events(state.history.iter().map(|entry| &entry.view), state.seen)
    }

    /// Moves the seen marker under entry `up_to` (it and every older one
    /// are seen), or under the newest with `None` (everything is).
    pub fn mark_seen(&self, up_to: Option<u64>) {
        self.update(|state, changes| {
            let newest = state.next_entry.saturating_sub(1);
            let seen = up_to.unwrap_or(newest).min(newest);
            if seen != state.seen {
                state.seen = seen;
                changes.seen = true;
            }
        });
    }

    /// Records an event, if it's one that matters on stream; its entry's id.
    pub fn record(&self, signal: &Signal, replay_of: Option<u64>) -> Option<u64> {
        let described = describe(signal).filter(|described| described.kept)?;
        let message = described.message.cloned().unwrap_or_default();
        self.update(|state, changes| {
            let id = state.next_entry;
            state.next_entry += 1;
            let view = HistoryEntry {
                id,
                at_ms: now_ms(),
                kind: described.kind,
                summary: described.summary,
                amount: described.amount,
                message: message.plain(),
                message_parts: message.alert_parts(),
                replay_of,
                names: name_colors(signal),
                cards: Vec::new(),
                card_ids: Vec::new(),
                items: Vec::new(),
            };
            state.history.push_front(Entry { view, signal: signal.clone() });
            state.history.retain(within_limit(|entry: &Entry| entry.view.kind));
            changes.entries.push(id);
            Some(id)
        })
    }

    /// Card `id`, named `name`, reacted to the event of history entry
    /// `entry`.
    pub fn fired(&self, entry: u64, id: &str, name: &str) {
        self.update(|state, changes| state.fired(entry, id, name, changes));
    }

    /// The event of a history entry, to replay it.
    pub fn signal(&self, entry: u64) -> Option<Signal> {
        self.lock().history.iter().find(|found| found.view.id == entry).map(|entry| entry.signal.clone())
    }

    /// Puts something at the end of the queue (a preview: not an event's,
    /// so never prepared again); its id.
    pub fn push(&self, play: Play, cause: &Cause) -> u64 {
        let id = self.update(|state, changes| state.push(play, cause, None, changes));
        self.inner.wake.notify_one();
        id
    }

    pub fn control(&self, action: QueueAction) {
        self.update(|state, changes| {
            match action {
                QueueAction::Pause => state.paused = true,
                QueueAction::Resume => state.paused = false,
                QueueAction::Toggle => state.paused = !state.paused,
                QueueAction::Skip => skip_stage(state, changes),
                QueueAction::Clear => clear(state, None, changes),
            }
            tracing::info!(?action, "queue");
            changes.queue = true;
        });
        self.inner.wake.notify_one();
    }

    /// Skips item `id`, wherever it is: on stage, it ends now (as Skip);
    /// waiting, it's dropped (it never starts, so neither Started nor
    /// Ended fire). Gone already (played, skipped, cleared, its time ran
    /// out), nothing happens: whoever asked saw the queue as it was a
    /// moment ago, and the item after it isn't theirs to skip.
    pub fn skip(&self, id: u64) -> Skipped {
        let skipped = self.update(|state, changes| {
            if state.stage.as_ref().is_some_and(|stage| stage.item.id == id) {
                skip_stage(state, changes);
                return Skipped::OnStage;
            }
            let Some(index) = state.waiting.iter().position(|item| item.id == id) else {
                return Skipped::Gone;
            };
            let Some(item) = state.waiting.remove(index) else { return Skipped::Gone };
            state.making.remove(&item.id);
            state.left(&item, ItemState::Skipped, changes);
            state.forget_events();
            changes.queue = true;
            Skipped::Waiting
        });
        tracing::info!(id, ?skipped, "queue item skipped");
        self.inner.wake.notify_one();
        skipped
    }

    /// Moves waiting item `id` to the front, so it plays once what's on
    /// stage ends. By id, as [`Activity::skip`]: one that's on stage, or
    /// gone (played, skipped, cleared, withdrawn), is left alone.
    pub fn play_next(&self, id: u64) -> Moved {
        let moved = self.update(|state, changes| {
            if state.stage.as_ref().is_some_and(|stage| stage.item.id == id) {
                return Moved::OnStage;
            }
            let Some(index) = state.waiting.iter().position(|item| item.id == id && !item.withdrawn) else {
                return Moved::Gone;
            };
            if index > 0
                && let Some(item) = state.waiting.remove(index)
            {
                state.waiting.push_front(item);
                changes.queue = true;
            }
            Moved::Waiting
        });
        tracing::info!(id, ?moved, "queue item moved to play next");
        self.inner.wake.notify_one();
        moved
    }

    /// Drops what's waiting from one group (everything with `None`).
    pub fn clear(&self, only: Option<QueueGroup>) {
        self.update(|state, changes| {
            clear(state, only, changes);
            tracing::info!(only = only.map(QueueGroup::name), "queue cleared");
            changes.queue = true;
        });
        self.inner.wake.notify_one();
    }

    /// Forgets every history entry (what's queued stays, unlisted).
    pub fn clear_history(&self) {
        self.update(|state, changes| {
            let forgotten = state.history.len();
            state.history.clear();
            changes.history = true;
            tracing::info!(forgotten, "history cleared");
        });
    }

    /// An overlay finished playing item `id` (not one server code plays).
    pub fn done(&self, id: u64) {
        self.update(|state, changes| {
            if state.stage.as_ref().is_some_and(|stage| stage.item.id == id && !stage.item.play.server) {
                finish(state, ItemState::Played, changes);
            }
        });
        self.inner.wake.notify_one();
    }

    /// `plugin`'s server code finished playing item `id`. Refused (why)
    /// for one that isn't on stage or isn't its own.
    pub fn done_by_server(&self, plugin: &str, id: u64) -> Result<(), String> {
        let result = self.update(|state, changes| {
            let item = state.stage.as_ref().map(|stage| &stage.item).filter(|item| item.id == id);
            let Some(item) = item else {
                return Err(format!("{id} isn't on stage (skipped, or its time ran out)"));
            };
            if item.play.plugin != plugin || !item.play.server {
                return Err(format!("{id} isn't one of its runs"));
            }
            finish(state, ItemState::Played, changes);
            Ok(())
        });
        self.inner.wake.notify_one();
        result
    }

    /// `plugin`'s server code is running: the queue plays its queued runs
    /// until the returned [`Serving`] is dropped.
    pub fn serving(&self, plugin: &str) -> Serving {
        self.update(|state, changes| {
            *state.servers.entry(plugin.to_string()).or_insert(0) += 1;
            changes.queue = true;
        });
        self.inner.wake.notify_one();
        Serving { activity: self.clone(), plugin: plugin.to_string() }
    }

    /// An overlay shows these plugins' pages (the queue plays their queued
    /// effects while one does). Replace it when they change.
    pub fn pages(&self, plugins: Vec<String>) -> Pages {
        self.update(|state, changes| {
            for plugin in &plugins {
                *state.pages.entry(plugin.clone()).or_insert(0) += 1;
            }
            if !plugins.is_empty() {
                state.players += 1;
            }
            if let Some(stage) = &mut state.stage
                && plugins.contains(&stage.item.play.plugin)
            {
                stage.until_ms = now_ms() + stage.item.length_ms() + MARGIN_MS;
            }
            changes.queue = true;
        });
        self.inner.wake.notify_one();
        Pages { activity: self.clone(), plugins }
    }

    /// What's on stage, for an overlay that just connected.
    pub fn on_stage(&self) -> Option<Command> {
        self.lock().stage.as_ref().map(|stage| stage.item.command())
    }

    /// Plays the queue, for as long as the server runs.
    pub async fn run(self) {
        loop {
            let notified = self.inner.wake.notified();
            match self.step(now_ms()) {
                Some(until_ms) => {
                    let wait = Duration::from_millis(until_ms.saturating_sub(now_ms()));
                    tokio::select! {
                        () = notified => {}
                        () = tokio::time::sleep(wait) => {}
                    }
                }
                None => notified.await,
            }
        }
    }

    /// Moves the queue on as far as it can now: ends an item whose time is
    /// up, stages the next one (once its speech is made), and has the next
    /// few messages made. When to look again: the stage's time limit.
    fn step(&self, now_ms: u64) -> Option<u64> {
        self.update(|state, changes| {
            loop {
                if let Some(stage) = &state.stage {
                    if now_ms < stage.until_ms {
                        break;
                    }
                    tracing::info!(
                        id = stage.item.id,
                        "no overlay said the item on stage had ended; moving on"
                    );
                    finish(state, ItemState::Played, changes);
                }
                if state.paused {
                    break;
                }
                let Some(front) = state.waiting.front() else { break };
                // Withdrawn, and the queue has come to its place: gone for good
                // once it could play it, or what comes next (a queue that's
                // stuck hasn't passed it yet).
                if front.withdrawn {
                    let next_plays = listed(state).next().is_some_and(|next| can_play(state, next));
                    if !can_play(state, front) && !next_plays {
                        break;
                    }
                    if let Some(item) = state.waiting.pop_front() {
                        state.left(&item, ItemState::Withdrawn, changes);
                        state.forget_events();
                    }
                    changes.queue = true;
                    continue;
                }
                if !can_play(state, front) {
                    break;
                }
                let front = front.id;
                match self.speech_for(state, front, changes) {
                    Ready::Yes => {}
                    Ready::NotYet => break,
                    Ready::Failed => continue,
                }
                let Some(item) = state.waiting.pop_front() else { break };
                state.left(&item, ItemState::OnStage, changes);
                changes.send.push(item.command());
                changes.resumes.extend(item.resume(STARTED_OUTPUT));
                state.stage = Some(Stage { until_ms: now_ms + item.length_ms() + MARGIN_MS, item });
                changes.queue = true;
            }
            let ahead: Vec<u64> = listed(state).take(LOOKAHEAD).map(|item| item.id).collect();
            for id in ahead {
                let can = state.waiting.iter().find(|item| item.id == id).and_then(|item| item.play.speech());
                if can.is_some_and(|job| self.inner.speech.can_speak(&job.voice)) {
                    self.speech_for(state, id, changes);
                }
            }
            state.stage.as_ref().map(|stage| stage.until_ms)
        })
    }

    /// Whether item `id`'s spoken message is made; asks for it if not.
    /// One that can't be made is left out (an alert shows without it, a
    /// message on its own fails).
    fn speech_for(&self, state: &mut State, id: u64, changes: &mut Changes) -> Ready {
        let Some(item) = state.waiting.iter_mut().find(|item| item.id == id) else { return Ready::Failed };
        let Some(job) = item.play.speech().cloned() else { return Ready::Yes };
        if item.made.as_ref().is_some_and(|made| self.inner.speech.has(made)) {
            return Ready::Yes;
        }
        if state.making.contains(&id) {
            return Ready::NotYet;
        }
        item.made = None;
        let activity = self.clone();
        if self.inner.speech.make(job, move |made| activity.made(id, made)) {
            state.making.insert(id);
            return Ready::NotYet;
        }
        self.leave_out_speech(state, id, changes);
        Ready::Failed
    }

    /// Item `id`'s spoken message was made (or couldn't be).
    fn made(&self, id: u64, made: Option<Made>) {
        self.update(|state, changes| {
            if !state.making.remove(&id) {
                return;
            }
            match made {
                Some(made) => {
                    if let Some(item) = state.waiting.iter_mut().find(|item| item.id == id) {
                        item.made = Some(made);
                        changes.queue = true;
                    }
                }
                None => self.leave_out_speech(state, id, changes),
            }
        });
        self.inner.wake.notify_one();
    }

    fn leave_out_speech(&self, state: &mut State, id: u64, changes: &mut Changes) {
        let Some(index) = state.waiting.iter().position(|item| item.id == id) else { return };
        let item = &mut state.waiting[index];
        tracing::info!(id, "a queued effect's text couldn't be read aloud; it plays without it");
        item.play.speech = None;
        item.made = None;
        changes.queue = true;
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Changes the state; then tells panels, sends the overlay what it
    /// should play, and has what changed saved a moment later
    /// ([`SAVE_AFTER`]).
    fn update<R>(&self, change: impl FnOnce(&mut State, &mut Changes) -> R) -> R {
        let mut state = self.lock();
        let mut changes = Changes::default();
        let result = change(&mut state, &mut changes);
        if changes.queue {
            state.unsaved.queue = true;
            let _ = self.inner.changes.send(Change::Queue(queue_state(&state)));
        }
        if changes.history {
            let entries = state.history.iter().map(|entry| entry.view.clone()).collect();
            let _ = self.inner.changes.send(Change::History(entries));
        }
        if changes.seen {
            let _ = self.inner.changes.send(Change::Seen(state.seen));
        }
        if let Some(prepared) = changes.prepared {
            let _ = self.inner.changes.send(Change::Prepared(prepared));
        }
        if !changes.entries.is_empty() || changes.history || changes.seen {
            state.unsaved.history = true;
            let mut told = HashSet::new();
            for id in changes.entries {
                if told.insert(id)
                    && let Some(entry) = state.history.iter().find(|entry| entry.view.id == id)
                {
                    let _ = self.inner.changes.send(Change::Entry(entry.view.clone()));
                }
            }
        }
        for command in changes.send {
            self.inner.overlay.send(command);
        }
        if !changes.resumes.is_empty()
            && let Some(sender) = &*self.inner.resumes.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            for resume in changes.resumes {
                let _ = sender.send(resume);
            }
        }
        let save = self.inner.files.is_some()
            && (state.unsaved.queue || state.unsaved.history)
            && !state.unsaved.coming;
        state.unsaved.coming |= save;
        drop(state);
        if save {
            self.save_soon();
        }
        result
    }

    /// Saves what changed after [`SAVE_AFTER`], off the async threads; at
    /// once without a runtime (tests).
    fn save_soon(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.flush();
            return;
        };
        let activity = self.clone();
        runtime.spawn(async move {
            tokio::time::sleep(SAVE_AFTER).await;
            let _ = tokio::task::spawn_blocking(move || activity.flush()).await;
        });
    }

    /// Writes the queue and the history if they changed since they were
    /// last written: a moment after a change, and when the app stops.
    /// They're turned into JSON under the lock and written outside it.
    pub fn flush(&self) {
        self.inner.flush();
    }

    /// The items waiting, as panels see them (tests).
    #[cfg(test)]
    fn waiting(&self) -> Vec<QueueItem> {
        listed(&self.lock()).map(Item::view).collect()
    }

    /// What the items waiting play (tests).
    #[cfg(test)]
    pub fn waiting_plays(&self) -> Vec<Play> {
        listed(&self.lock()).map(|item| item.play.clone()).collect()
    }

    /// Plays everything that can play now, as an overlay showing every
    /// waiting item's plugin page (or its server code) that's done as soon
    /// as it's sent something would (tests).
    #[cfg(test)]
    pub fn play_all(&self) {
        self.play_all_then(|| {});
    }

    /// As [`play_all`](Self::play_all), calling `between` each time an item
    /// goes on stage and each time one ends (for the cards to carry on).
    #[cfg(test)]
    pub fn play_all_then(&self, mut between: impl FnMut()) {
        let mut pages = Vec::new();
        let mut serving = Vec::new();
        let mut shown: HashSet<String> = HashSet::new();
        loop {
            let new: Vec<String> = self
                .lock()
                .waiting
                .iter()
                .map(|item| item.play.plugin.clone())
                .filter(|plugin| !shown.contains(plugin))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if !new.is_empty() {
                shown.extend(new.iter().cloned());
                serving.extend(new.iter().map(|plugin| self.serving(plugin)));
                pages.push(self.pages(new));
            }
            self.step(0);
            let on_stage = self.lock().stage.as_ref().map(|stage| (stage.item.id, stage.item.play.clone()));
            let Some((id, play)) = on_stage else { break };
            between();
            if play.server {
                self.done_by_server(&play.plugin, id).unwrap();
            } else {
                self.done(id);
            }
            between();
        }
    }
}

impl State {
    /// Card `id`, named `name`, reacted to the event of history entry
    /// `entry`.
    fn fired(&mut self, entry: u64, id: &str, name: &str, changes: &mut Changes) {
        if let Some(entry) = self.history.iter_mut().find(|found| found.view.id == entry)
            && !entry.view.card_ids.iter().any(|fired| fired == id)
        {
            entry.view.cards.push(name.to_string());
            entry.view.card_ids.push(id.to_string());
            changes.entries.push(entry.view.id);
        }
    }

    /// Puts `play` at the end of the queue (`chain`: its place in its
    /// waiting event); its id.
    fn push(&mut self, play: Play, cause: &Cause, chain: Option<Chain>, changes: &mut Changes) -> u64 {
        self.insert(self.waiting.len(), play, cause, chain, changes)
    }

    /// Puts `play` in the queue at `at` (as [`State::push`]).
    fn insert(
        &mut self,
        at: usize,
        play: Play,
        cause: &Cause,
        chain: Option<Chain>,
        changes: &mut Changes,
    ) -> u64 {
        let id = self.next_item;
        self.next_item += 1;
        let item = Item {
            id,
            queued_at_ms: now_ms(),
            card: cause.card.clone(),
            entry: cause.entry,
            event: cause.event.clone(),
            event_kind: cause.kind,
            origin: cause.origin,
            names: cause.names.clone(),
            seed: cause.seed,
            chain,
            withdrawn: false,
            play,
            made: None,
        };
        if let Some(entry) = cause.entry
            && let Some(entry) = self.history.iter_mut().find(|found| found.view.id == entry)
        {
            entry.view.items.push(item.record(ItemState::Waiting));
            changes.entries.push(entry.view.id);
        }
        self.waiting.insert(at.min(self.waiting.len()), item);
        let waiting = self.waiting.len();
        if waiting.is_multiple_of(WARN_EVERY) {
            tracing::warn!(
                waiting,
                "the queue is getting long; the control panel's Activity page can pause, skip or clear it"
            );
        }
        changes.queue = true;
        id
    }
}

enum Ready {
    Yes,
    NotYet,
    /// Its speech was left out; the queue changed.
    Failed,
}

/// Counts a plugin's server code while it runs.
pub struct Serving {
    activity: Activity,
    plugin: String,
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.activity.update(|state, changes| {
            if let Some(count) = state.servers.get_mut(&self.plugin) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    state.servers.remove(&self.plugin);
                }
            }
            back_to_waiting(state, changes);
        });
        self.activity.inner.wake.notify_one();
    }
}

/// Counts an overlay's plugin pages while it shows them.
pub struct Pages {
    activity: Activity,
    plugins: Vec<String>,
}

impl Pages {
    pub fn plugins(&self) -> &[String] {
        &self.plugins
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        self.activity.update(|state, changes| {
            if !self.plugins.is_empty() {
                state.players = state.players.saturating_sub(1);
            }
            for plugin in &self.plugins {
                if let Some(count) = state.pages.get_mut(plugin) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        state.pages.remove(plugin);
                    }
                }
            }
            back_to_waiting(state, changes);
        });
        self.activity.inner.wake.notify_one();
    }
}

/// Whether a connected overlay (or its plugin's running server code) can
/// play an item.
fn can_play(state: &State, item: &Item) -> bool {
    let players = if item.play.server { &state.servers } else { &state.pages };
    players.get(&item.play.plugin).is_some_and(|count| *count > 0)
}

/// The last overlay (or the server code) that could play the item on stage
/// closed or stopped mid-item: it plays again, from the start, once one
/// connects (or the code runs again).
fn back_to_waiting(state: &mut State, changes: &mut Changes) {
    if state.stage.as_ref().is_some_and(|stage| !can_play(state, &stage.item))
        && let Some(stage) = state.stage.take()
    {
        set_state(state, stage.item.entry, stage.item.id, ItemState::Waiting, changes);
        state.waiting.push_front(stage.item);
    }
    changes.queue = true;
}

/// The item on stage has ended. Played (not skipped), its card carries on
/// from Ended.
fn finish(state: &mut State, outcome: ItemState, changes: &mut Changes) {
    if let Some(stage) = state.stage.take() {
        set_state(state, stage.item.entry, stage.item.id, outcome, changes);
        if outcome == ItemState::Played {
            changes.resumes.extend(stage.item.resume(ENDED_OUTPUT));
        }
        state.forget_events();
        changes.queue = true;
    }
}

/// Ends the item on stage now, if there is one, and tells what plays it.
fn skip_stage(state: &mut State, changes: &mut Changes) {
    if let Some(stage) = &state.stage {
        changes.send.push(stage.item.skip_command());
        finish(state, ItemState::Skipped, changes);
    }
}

/// Drops what's waiting from one group (everything with `None`).
fn clear(state: &mut State, only: Option<QueueGroup>, changes: &mut Changes) {
    let (cleared, kept): (Vec<Item>, Vec<Item>) =
        state.waiting.drain(..).partition(|item| only.is_none_or(|group| item.group() == group));
    state.waiting = kept.into();
    for item in cleared {
        state.making.remove(&item.id);
        let to = if item.withdrawn { ItemState::Withdrawn } else { ItemState::Cleared };
        state.left(&item, to, changes);
    }
    state.forget_events();
}

/// An item's state in its event's history entry.
fn set_state(state: &mut State, entry: Option<u64>, id: u64, to: ItemState, changes: &mut Changes) {
    let Some(entry) = entry.and_then(|entry| state.history.iter_mut().find(|found| found.view.id == entry))
    else {
        return;
    };
    if let Some(record) = entry.view.items.iter_mut().find(|record| record.id == id)
        && record.state != to
    {
        record.state = to;
        changes.entries.push(entry.view.id);
    }
}

/// The items waiting, as panels see them: not those withdrawn.
fn listed(state: &State) -> impl Iterator<Item = &Item> {
    state.waiting.iter().filter(|item| !item.withdrawn)
}

fn queue_state(state: &State) -> QueueState {
    QueueState {
        paused: state.paused,
        players: state.players,
        on_stage: state.stage.as_ref().map(|stage| stage.item.view()),
        waiting: listed(state).take(QUEUE_LIST_LIMIT).map(Item::view).collect(),
        waiting_count: listed(state).count(),
        by_group: listed(state).fold(BTreeMap::new(), |mut counts, item| {
            *counts.entry(item.group()).or_insert(0) += 1;
            counts
        }),
        waiting_for_page: match (&state.stage, listed(state).next()) {
            (None, Some(next)) if !next.play.server && !can_play(state, next) => {
                Some(next.play.plugin.clone())
            }
            _ => None,
        },
        waiting_for_plugin: match (&state.stage, listed(state).next()) {
            (None, Some(next)) if next.play.server && !can_play(state, next) => {
                Some(next.play.plugin.clone())
            }
            _ => None,
        },
    }
}

impl Inner {
    fn flush(&self) {
        let Some(files) = &self.files else { return };
        let _writing = self.writing.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (queue, history) = {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let unsaved = std::mem::take(&mut state.unsaved);
            let queue = unsaved.queue.then(|| serde_json::to_string(&saved_queue(&state)));
            let history = unsaved.history.then(|| {
                let saving =
                    SavingHistory { next_id: state.next_entry, seen: state.seen, entries: &state.history };
                serde_json::to_string(&saving)
            });
            (queue, history)
        };
        if let Some(json) = queue {
            save(&files.queue, json);
        }
        if let Some(json) = history {
            save(&files.history, json);
        }
    }
}

/// The last changes are saved when the app stops (if nothing else still
/// holds the activity, [`Activity::flush`] is called by `main` too).
impl Drop for Inner {
    fn drop(&mut self) {
        self.flush();
    }
}

fn saved_queue(state: &State) -> SavedQueue<'_> {
    SavedQueue {
        paused: state.paused,
        next_id: state.next_item,
        events: &state.events,
        next_event: state.next_event,
        items: state
            .stage
            .iter()
            .map(|stage| stage.item.clone())
            .chain(state.waiting.iter().cloned())
            .collect(),
    }
}

/// A saved file read entry by entry: its other fields, and the entries in
/// its list that could be read.
struct Loaded<T> {
    fields: serde_json::Map<String, serde_json::Value>,
    entries: Vec<T>,
}

impl<T> Loaded<T> {
    fn u64(&self, field: &str) -> u64 {
        self.fields.get(field).and_then(serde_json::Value::as_u64).unwrap_or(0)
    }

    fn bool(&self, field: &str) -> bool {
        self.fields.get(field).and_then(serde_json::Value::as_bool).unwrap_or(false)
    }
}

/// Reads `path` (an object with a `list` of entries) entry by entry, so a
/// change to the saved format loses only the entries it breaks: those are
/// set aside in a `.bad-<unix ms>` file next to it (the whole file, if it
/// isn't such an object at all), and the rest are kept.
fn load<T: DeserializeOwned>(path: &Path, list: &str) -> Loaded<T> {
    let empty = || Loaded { fields: serde_json::Map::new(), entries: Vec::new() };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return empty(),
        Err(error) => {
            tracing::warn!(%error, "couldn't read {}; starting empty", path.display());
            return empty();
        }
    };
    let bad = || {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".bad-{}", now_ms()));
        path.with_file_name(name)
    };
    let mut fields = match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(serde_json::Value::Object(fields)) => fields,
        _ => {
            let bad = bad();
            let _ = std::fs::rename(path, &bad);
            tracing::warn!("{} couldn't be read (kept as {}); starting empty", path.display(), bad.display());
            return empty();
        }
    };
    let values = match fields.remove(list) {
        Some(serde_json::Value::Array(values)) => values,
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(other) => vec![other],
    };
    let mut entries = Vec::with_capacity(values.len());
    let mut set_aside = Vec::new();
    let mut first_error = None;
    for value in values {
        match T::deserialize(&value) {
            Ok(entry) => entries.push(entry),
            Err(error) => {
                first_error.get_or_insert(error);
                set_aside.push(value);
            }
        }
    }
    if let Some(error) = first_error {
        let bad = bad();
        let count = set_aside.len();
        let mut aside = serde_json::Map::new();
        aside.insert(list.to_string(), serde_json::Value::Array(set_aside));
        let written = serde_json::to_string_pretty(&aside)
            .map_err(std::io::Error::other)
            .and_then(|json| std::fs::write(&bad, json));
        match written {
            Ok(()) => tracing::warn!(
                %error,
                count,
                kept = entries.len(),
                "some of {} couldn't be read; they're kept in {}",
                path.display(),
                bad.display()
            ),
            Err(write_error) => tracing::warn!(
                %error,
                count,
                "some of {} couldn't be read, nor set aside in {} ({write_error}); they're left out",
                path.display(),
                bad.display()
            ),
        }
    }
    Loaded { fields, entries }
}

/// The waiting events saved with the queue, each read on its own: one that
/// can't be read is left out (its items play as they are).
fn waiting_events(fields: &serde_json::Map<String, serde_json::Value>) -> Vec<WaitingEvent> {
    let Some(serde_json::Value::Array(events)) = fields.get("events") else { return Vec::new() };
    let read = |event: &serde_json::Value| match WaitingEvent::deserialize(event) {
        Ok(event) => Some(event),
        Err(error) => {
            tracing::warn!(%error, "a waiting event in the queue couldn't be read; its items play as they are");
            None
        }
    };
    events.iter().filter_map(read).collect()
}

/// Writes to a temporary file, then renames it, so a crash can't leave
/// half a file.
fn save(path: &Path, json: serde_json::Result<String>) {
    let written = json.map_err(std::io::Error::other).and_then(|json| {
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, json).and_then(|()| std::fs::rename(&temp, path))
    });
    if let Err(error) = written {
        tracing::warn!(%error, "couldn't save {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Amount, SignalKind};
    use crate::pipeline::testing::{origin, user};

    /// An Alerts plugin run showing `text`.
    fn alert(text: &str) -> Play {
        let mut values = serde_json::Map::new();
        values.insert("message".into(), protocol::plugin::plain_text_value(text));
        Play {
            plugin: "alert".into(),
            node: "alert".into(),
            title: "Alert".into(),
            text: text.into(),
            parts: Vec::new(),
            input: "trigger".into(),
            values,
            changes: serde_json::Map::new(),
            settings: serde_json::Map::new(),
            speech: None,
            then: None,
            server: false,
        }
    }

    /// A Speak plugin run reading `text`.
    fn speak(text: &str) -> Play {
        let mut values = serde_json::Map::new();
        values.insert("text".into(), protocol::plugin::plain_text_value(text));
        Play {
            plugin: "speak".into(),
            node: "speak".into(),
            title: "Speak".into(),
            values,
            speech: Some(Spoken { input: "text".into(), job: job(text) }),
            ..alert(text)
        }
    }

    /// An overlay showing the alerts' and Speak's pages.
    fn player(activity: &Activity) -> Pages {
        activity.pages(vec!["alert".into(), "speak".into()])
    }

    fn job(text: &str) -> Job {
        Job { text: text.into(), voice: "bm_george".into(), speed: 1.0, volume: 0.5 }
    }

    fn live() -> Cause {
        Cause { card: "Card".into(), ..Cause::default() }
    }

    /// What plugin pages were sent.
    fn sent(receiver: &mut broadcast::Receiver<Arc<protocol::Envelope>>) -> Vec<PluginMessage> {
        let mut messages = Vec::new();
        while let Ok(envelope) = receiver.try_recv() {
            if let Command::Plugin(command) = &envelope.command {
                messages.push(command.message.clone());
            }
        }
        messages
    }

    /// A run's queue id and the text of its message (or its Text).
    fn shown(message: &PluginMessage) -> (u64, String) {
        match message {
            PluginMessage::Run { queue_id: Some(id), values, .. } => {
                let text = values.get("message").or_else(|| values.get("text"));
                (*id, text.and_then(|text| text["text"].as_str()).unwrap_or_default().to_string())
            }
            other => panic!("not a queued run: {other:?}"),
        }
    }

    #[test]
    fn one_at_a_time_and_only_with_an_overlay() {
        let hub = OverlayHub::new();
        let mut overlay = hub.subscribe();
        let activity = Activity::for_tests(hub, Speech::off());
        let first = activity.push(alert("one"), &live());
        activity.push(alert("two"), &live());
        assert_eq!(activity.step(0), None);
        assert!(sent(&mut overlay).is_empty(), "nobody to play it");

        let player = player(&activity);
        let until = activity.step(1_000).unwrap();
        assert_eq!(until, 1_000 + QUEUE_LIMIT_MS + MARGIN_MS);
        assert_eq!(sent(&mut overlay).iter().map(shown).collect::<Vec<_>>(), [(first, "one".to_string())]);
        assert_eq!(activity.waiting().len(), 1);
        activity.step(2_000);
        assert!(sent(&mut overlay).is_empty(), "the first is still on stage");

        activity.done(first);
        activity.step(3_000);
        assert_eq!(shown(&sent(&mut overlay)[0]).1, "two");

        // No Done: it moves on after the limit and a margin.
        activity.step(3_000 + QUEUE_LIMIT_MS + MARGIN_MS);
        assert!(activity.queue().on_stage.is_none());
        drop(player);
    }

    #[test]
    fn pause_skip_clear() {
        let hub = OverlayHub::new();
        let mut overlay = hub.subscribe();
        let activity = Activity::for_tests(hub, Speech::off());
        let _player = player(&activity);
        activity.control(QueueAction::Pause);
        for text in ["a", "b", "c"] {
            activity.push(alert(text), &live());
        }
        activity.step(0);
        assert!(sent(&mut overlay).is_empty(), "paused");
        activity.control(QueueAction::Resume);
        activity.step(0);
        let (a, _) = shown(&sent(&mut overlay)[0]);

        activity.control(QueueAction::Skip);
        assert_eq!(sent(&mut overlay), [PluginMessage::Skip { queue_id: a }]);
        activity.step(0);
        assert_eq!(shown(&sent(&mut overlay)[0]).1, "b");
        activity.control(QueueAction::Clear);
        let queue = activity.queue();
        assert_eq!((queue.waiting_count, queue.on_stage.map(|item| item.text)), (0, Some("b".into())));
    }

    #[test]
    fn any_item_can_be_skipped_by_its_id() {
        let hub = OverlayHub::new();
        let mut overlay = hub.subscribe();
        let activity = Activity::for_tests(hub, Speech::off());
        let mut resumes = activity.resumes();
        let raid = Signal::new(origin(), SignalKind::Raid { from: user("r"), viewers: 3 });
        let entry = activity.record(&raid, None).unwrap();
        let cause = Cause { entry: Some(entry), ..live() };
        let carrying_on =
            |text| Play { then: Some(Box::new(Then::for_tests("card", 2, raid.clone()))), ..alert(text) };
        let _player = player(&activity);
        activity.control(QueueAction::Pause);
        let a = activity.push(alert("a"), &live());
        let b = activity.push(carrying_on("b"), &cause);
        let c = activity.push(alert("c"), &live());

        // A waiting one: dropped, nothing told, nothing carries on.
        assert_eq!(activity.skip(b), Skipped::Waiting);
        assert!(sent(&mut overlay).is_empty());
        assert_eq!(activity.waiting().iter().map(|item| item.id).collect::<Vec<_>>(), [a, c]);
        assert_eq!(activity.history()[0].items[0].state, ItemState::Skipped);
        assert_eq!(activity.skip(b), Skipped::Gone, "twice: nothing more");

        // The one on stage: ended, its page told.
        activity.control(QueueAction::Resume);
        activity.step(0);
        assert_eq!(shown(&sent(&mut overlay)[0]).0, a);
        assert_eq!(activity.skip(a), Skipped::OnStage);
        assert_eq!(sent(&mut overlay), [PluginMessage::Skip { queue_id: a }]);
        activity.step(0);
        assert_eq!(shown(&sent(&mut overlay)[0]).0, c);
        assert!(resumes.try_recv().is_err(), "b never started");

        // An id that never was.
        assert_eq!(activity.skip(c + 100), Skipped::Gone);
        assert_eq!(activity.queue().on_stage.map(|item| item.id), Some(c));
    }

    #[test]
    fn any_waiting_item_can_be_played_next_by_its_id() {
        let hub = OverlayHub::new();
        let mut overlay = hub.subscribe();
        let activity = Activity::for_tests(hub, Speech::off());
        let _player = player(&activity);
        let a = activity.push(alert("a"), &live());
        let b = activity.push(alert("b"), &live());
        let c = activity.push(alert("c"), &live());
        let d = activity.push(alert("d"), &live());
        activity.step(0);
        assert_eq!(shown(&sent(&mut overlay)[0]).0, a);
        let ids = |activity: &Activity| activity.waiting().iter().map(|item| item.id).collect::<Vec<_>>();

        // A waiting one goes to the front; what's on stage plays on.
        assert_eq!(activity.play_next(c), Moved::Waiting);
        assert!(sent(&mut overlay).is_empty());
        assert_eq!(ids(&activity), [c, b, d]);
        assert_eq!(activity.play_next(c), Moved::Waiting, "at the front already");
        assert_eq!(ids(&activity), [c, b, d]);
        assert_eq!(activity.play_next(a), Moved::OnStage);
        assert!(sent(&mut overlay).is_empty());
        activity.done(a);
        activity.step(0);
        assert_eq!(shown(&sent(&mut overlay)[0]).0, c);

        // Gone (played, skipped, withdrawn, never was): nothing changes.
        assert_eq!(activity.play_next(a), Moved::Gone);
        activity.skip(d);
        assert_eq!(activity.play_next(d), Moved::Gone);
        let e = activity.push(alert("e"), &live());
        activity.lock().waiting.iter_mut().find(|item| item.id == e).unwrap().withdrawn = true;
        assert_eq!(activity.play_next(e), Moved::Gone);
        assert_eq!(activity.play_next(e + 100), Moved::Gone);
        assert_eq!(ids(&activity), [b]);
    }

    /// The panel shows `first` on stage and the user clicks its Skip; before
    /// the click arrives, `first` ends and `second` goes on stage. The
    /// click skips nothing: `second` plays on. The same for an item
    /// cleared, or skipped from another panel, while the click was on its
    /// way.
    #[test]
    fn skipping_an_item_that_has_gone_skips_nothing() {
        let hub = OverlayHub::new();
        let mut overlay = hub.subscribe();
        let activity = Activity::for_tests(hub, Speech::off());
        let _player = player(&activity);
        let first = activity.push(alert("first"), &live());
        let second = activity.push(alert("second"), &live());
        let third = activity.push(alert("third"), &live());
        let fourth = activity.push(alert("fourth"), &live());
        activity.step(0);
        let seen = activity.queue().on_stage.unwrap().id;
        assert_eq!(seen, first, "what the panel shows");

        // It ends, and the next goes on stage, before the click arrives.
        activity.done(first);
        activity.step(0);
        sent(&mut overlay);
        assert_eq!(activity.skip(seen), Skipped::Gone);
        assert!(sent(&mut overlay).is_empty(), "no Skip sent");
        assert_eq!(activity.queue().on_stage.map(|item| item.id), Some(second), "second plays on");

        // Its time ran out, likewise.
        activity.step(QUEUE_LIMIT_MS + MARGIN_MS);
        activity.step(QUEUE_LIMIT_MS + MARGIN_MS);
        sent(&mut overlay);
        assert_eq!(activity.skip(second), Skipped::Gone);
        assert_eq!(activity.queue().on_stage.map(|item| item.id), Some(third));

        // Waiting, then cleared or skipped elsewhere: the rest stay.
        let fifth = activity.push(alert("fifth"), &live());
        activity.skip(fourth);
        assert_eq!(activity.skip(fourth), Skipped::Gone, "a second panel's click");
        activity.clear(None);
        assert_eq!(activity.skip(fifth), Skipped::Gone);
        assert!(sent(&mut overlay).is_empty());
        assert_eq!(activity.queue().on_stage.map(|item| item.id), Some(third), "third plays on");
    }

    #[test]
    fn clear_takes_one_kind() {
        let activity = Activity::for_tests(OverlayHub::new(), Speech::off());
        let follow = Cause { kind: Some(EventKind::Follow), ..live() };
        let raid = Cause { kind: Some(EventKind::Raid), ..live() };
        activity.push(alert("f1"), &follow);
        activity.push(alert("r1"), &raid);
        activity.push(speak("chat"), &live());
        activity.push(alert("f2"), &follow);
        let queue = activity.queue();
        let counts: Vec<(QueueGroup, usize)> = queue.by_group.into_iter().collect();
        assert_eq!(counts, [(QueueGroup::Raid, 1), (QueueGroup::Follow, 2), (QueueGroup::Other, 1)]);
        assert_eq!(queue.waiting[0].group, QueueGroup::Follow);

        activity.clear(Some(QueueGroup::Follow));
        let texts: Vec<String> = activity.waiting().into_iter().map(|item| item.text).collect();
        assert_eq!(texts, ["r1", "chat"]);
        activity.clear(Some(QueueGroup::Other));
        assert_eq!(activity.waiting().len(), 1);
        activity.clear(None);
        assert!(activity.waiting().is_empty());
    }

    #[test]
    fn the_history_can_be_cleared() {
        let activity = Activity::for_tests(OverlayHub::new(), Speech::off());
        let mut changes = activity.subscribe();
        let raid = Signal::new(origin(), SignalKind::Raid { from: user("r"), viewers: 3 });
        let entry = activity.record(&raid, None).unwrap();
        activity.push(alert("raid!"), &Cause { entry: Some(entry), ..live() });
        activity.clear_history();
        assert!(activity.history().is_empty());
        assert_eq!(activity.waiting().len(), 1, "the queue isn't touched");
        let mut cleared = false;
        while let Ok(change) = changes.try_recv() {
            cleared |= matches!(change, Change::History(entries) if entries.is_empty());
        }
        assert!(cleared, "panels are told");
        let next = activity.record(&raid, None).unwrap();
        assert!(next > entry, "ids aren't reused");
    }

    #[test]
    fn server_code_fires_its_runs_own_outputs_once() {
        let activity = Activity::for_tests(OverlayHub::new(), Speech::off());
        let mut resumes = activity.resumes();
        let raid = Signal::new(origin(), SignalKind::Raid { from: user("r"), viewers: 3 });
        let run = activity.remember_run("obs", Then::for_tests("card", 2, raid), &live());

        activity.carry_on("obs", run, "started").unwrap();
        let resume = resumes.try_recv().unwrap();
        assert_eq!((resume.output.as_str(), resume.then.node), ("started", 2));
        assert_eq!(resume.cause, live(), "as when it fired");
        assert!(activity.carry_on("obs", run, "started").is_err(), "once");
        assert!(activity.carry_on("other", run, "ended").is_err(), "only its own plugin's");
        assert!(activity.carry_on("obs", run + 1, "ended").is_err(), "no such run");
        activity.carry_on("obs", run, "ended").unwrap();
        assert_eq!(resumes.try_recv().unwrap().output, "ended");
        assert!(resumes.try_recv().is_err());
    }

    #[test]
    fn a_flood_of_parked_events_only_pushes_out_that_plugins_own() {
        let activity = Activity::for_tests(OverlayHub::new(), Speech::off());
        let _resumes = activity.resumes();
        let raid = Signal::new(origin(), SignalKind::Raid { from: user("r"), viewers: 3 });
        let then = || Then::for_tests("card", 2, raid.clone());
        let effect = activity.remember_run("splitter", then(), &live());
        let another = activity.keep_run("other", RunKind::Parked, then(), &live());
        let oldest = activity.keep_run("splitter", RunKind::Parked, then(), &live());
        for _ in 1..RUNS_KEPT {
            activity.keep_run("splitter", RunKind::Parked, then(), &live());
        }
        assert!(activity.carry_on("splitter", oldest, "event").is_ok(), "{RUNS_KEPT} kept");
        activity.keep_run("splitter", RunKind::Parked, then(), &live());
        let gone = activity.carry_on("splitter", oldest, "miss").unwrap_err();
        assert!(gone.starts_with("no run"), "the oldest of its parked events went: {gone}");
        assert!(activity.carry_on("splitter", effect, "started").is_ok(), "not its effect run");
        assert!(activity.carry_on("other", another, "event").is_ok(), "nor another plugin's");
    }

    #[test]
    fn started_and_ended_go_to_the_card_but_not_for_a_skip_or_a_clear() {
        let activity = Activity::for_tests(OverlayHub::new(), Speech::off());
        let mut resumes = activity.resumes();
        let raid = Signal::new(origin(), SignalKind::Raid { from: user("r"), viewers: 3 });
        let carrying_on =
            |text| Play { then: Some(Box::new(Then::for_tests("card", 2, raid.clone()))), ..alert(text) };
        let _player = player(&activity);
        let replay = Cause { origin: ItemOrigin::Replay, ..live() };
        let played = activity.push(carrying_on("played"), &replay);
        activity.push(carrying_on("skipped"), &live());
        activity.push(carrying_on("timed out"), &live());
        activity.push(carrying_on("cleared"), &live());
        activity.push(alert("nothing wired"), &live());

        activity.step(0);
        let started = resumes.try_recv().unwrap();
        assert_eq!((started.output.as_str(), started.then.node), (STARTED_OUTPUT, 2));
        assert_eq!((started.cause.origin, started.cause.card.as_str()), (ItemOrigin::Replay, "Card"));
        activity.done(played);
        activity.step(0);
        activity.control(QueueAction::Skip);
        activity.step(0);
        activity.clear(Some(QueueGroup::Other));
        activity.step(QUEUE_LIMIT_MS + MARGIN_MS);
        let outputs: Vec<String> =
            std::iter::from_fn(|| resumes.try_recv().ok()).map(|resume| resume.output).collect();
        assert_eq!(
            outputs,
            [ENDED_OUTPUT, STARTED_OUTPUT, STARTED_OUTPUT, ENDED_OUTPUT],
            "played: ended; skipped: started only; timed out: ended; cleared: nothing"
        );
    }

    #[test]
    fn the_last_overlay_closing_puts_the_item_back() {
        let hub = OverlayHub::new();
        let mut overlay = hub.subscribe();
        let activity = Activity::for_tests(hub, Speech::off());
        let id = activity.push(alert("again"), &live());
        let player = player(&activity);
        activity.step(0);
        assert_eq!(sent(&mut overlay).len(), 1);
        drop(player);
        assert_eq!(activity.waiting()[0].id, id, "back at the front");
        let _player = self::player(&activity);
        activity.step(0);
        assert_eq!(shown(&sent(&mut overlay)[0]), (id, "again".to_string()), "from the start");
    }

    #[test]
    fn plugin_effects_wait_for_their_page() {
        let hub = OverlayHub::new();
        let mut overlay = hub.subscribe();
        let activity = Activity::for_tests(hub, Speech::off());
        let media = |title: &str| Play {
            plugin: "media".into(),
            node: "show".into(),
            title: title.into(),
            text: String::new(),
            parts: Vec::new(),
            input: "trigger".into(),
            values: serde_json::Map::new(),
            changes: serde_json::Map::new(),
            settings: serde_json::Map::new(),
            speech: None,
            then: None,
            server: false,
        };
        let first = activity.push(media("one"), &live());
        let second = activity.push(media("two"), &live());
        let other = activity.pages(vec!["confetti".into()]);
        assert_eq!(activity.step(0), None, "no overlay shows the media page");
        assert!(sent(&mut overlay).is_empty());

        let pages = activity.pages(vec!["media".into()]);
        assert_eq!(activity.step(0), Some(QUEUE_LIMIT_MS + MARGIN_MS));
        let shown = sent(&mut overlay);
        assert!(
            matches!(&shown[..], [PluginMessage::Run { node, queue_id: Some(id), .. }] if node == "show" && *id == first),
            "{shown:?}"
        );
        assert_eq!(activity.queue().on_stage.unwrap().label, "one");

        // The page closing puts it back; it plays again once one opens.
        drop(pages);
        assert_eq!(activity.waiting()[0].id, first);
        let _pages = activity.pages(vec!["media".into(), "confetti".into()]);
        drop(other);
        activity.step(0);
        assert_eq!(sent(&mut overlay).len(), 1, "again, from the start");

        // Done moves on; Skip tells the page.
        activity.done(first);
        activity.step(0);
        assert!(
            matches!(&sent(&mut overlay)[..], [PluginMessage::Run { queue_id: Some(id), .. }] if *id == second)
        );
        activity.control(QueueAction::Skip);
        assert_eq!(sent(&mut overlay), [PluginMessage::Skip { queue_id: second }]);
        assert!(activity.queue().on_stage.is_none());
    }

    #[test]
    fn server_code_plays_its_plugins_runs() {
        let hub = OverlayHub::new();
        let mut sent_out = hub.subscribe();
        let activity = Activity::for_tests(hub, Speech::off());
        let mut resumes = activity.resumes();
        let raid = Signal::new(origin(), SignalKind::Raid { from: user("r"), viewers: 3 });
        let then = Then::for_tests("brb", 2, raid);
        let show = |title: &str| Play {
            plugin: "obs".into(),
            node: "show_source".into(),
            title: title.into(),
            text: String::new(),
            parts: Vec::new(),
            input: "trigger".into(),
            values: serde_json::Map::new(),
            changes: serde_json::Map::new(),
            settings: serde_json::Map::new(),
            speech: None,
            then: Some(Box::new(then.clone())),
            server: true,
        };
        let first = activity.push(show("one"), &live());
        let second = activity.push(show("two"), &live());

        // Overlays don't play it; its code, once running, does.
        let _pages = activity.pages(vec!["obs".into()]);
        assert_eq!(activity.step(0), None, "its server code isn't running");
        assert_eq!(activity.queue().waiting_for_plugin.as_deref(), Some("obs"));
        assert_eq!(activity.queue().waiting_for_page, None);
        let serving = activity.serving("obs");
        activity.step(0);
        assert!(
            matches!(&sent(&mut sent_out)[..], [PluginMessage::Run { queue_id: Some(id), .. }] if *id == first)
        );
        assert_eq!(resumes.try_recv().unwrap().output, STARTED_OUTPUT);

        // Only its own code ends it, not an overlay or another plugin.
        activity.done(first);
        assert_eq!(activity.queue().on_stage.map(|item| item.id), Some(first));
        assert!(activity.done_by_server("media", first).is_err());
        assert!(activity.done_by_server("obs", second).is_err(), "not on stage");

        // Its code stopping puts it back; it plays again when it runs again.
        drop(serving);
        assert_eq!(activity.waiting()[0].id, first);
        let _serving = activity.serving("obs");
        activity.step(0);
        assert_eq!(sent(&mut sent_out).len(), 1, "again, from the start");
        assert_eq!(resumes.try_recv().unwrap().output, STARTED_OUTPUT);

        activity.done_by_server("obs", first).unwrap();
        assert_eq!(resumes.try_recv().unwrap().output, ENDED_OUTPUT);
        activity.step(0);
        assert!(
            matches!(&sent(&mut sent_out)[..], [PluginMessage::Run { queue_id: Some(id), .. }] if *id == second)
        );
        assert_eq!(resumes.try_recv().unwrap().output, STARTED_OUTPUT);

        // Skip tells it, and Ended doesn't fire.
        activity.control(QueueAction::Skip);
        assert_eq!(sent(&mut sent_out), [PluginMessage::Skip { queue_id: second }]);
        assert!(resumes.try_recv().is_err());
        assert!(activity.done_by_server("obs", second).is_err(), "skipped already");
    }

    #[test]
    fn speech_is_made_before_its_turn() {
        let hub = OverlayHub::new();
        let mut overlay = hub.subscribe();
        let speech = Speech::queue_only();
        let activity = Activity::for_tests(hub, speech.clone());
        let mut with_speech = alert("sub!");
        with_speech.speech = Some(Spoken { input: "read".into(), job: job("thanks") });
        with_speech.values.insert("read".into(), protocol::plugin::plain_text_value("thanks"));
        activity.push(with_speech, &live());
        activity.push(speak("hello"), &live());
        activity.push(speak("broken"), &live());

        // Made ahead, even before an overlay connects.
        activity.step(0);
        let texts = |speech: &Speech| speech.queued().into_iter().map(|job| job.text).collect::<Vec<_>>();
        assert_eq!(texts(&speech), ["thanks", "hello", "broken"]);
        speech.finish(|job| {
            (job.text != "broken")
                .then(|| Made { file: format!("{}.wav", job.text), duration_ms: Some(2_000) })
        });
        assert_eq!(activity.waiting().len(), 3, "one that can't be made plays without it");

        let _player = player(&activity);
        activity.step(0);
        let PluginMessage::Run { values, queue_id: Some(id), .. } = &sent(&mut overlay)[0] else { panic!() };
        let clip = &values["read"]["speech"];
        assert_eq!((&clip["duration_ms"], &clip["volume"]), (&2_000.into(), &0.5.into()));
        activity.done(*id);
        activity.step(0);
        let PluginMessage::Run { values, queue_id: Some(id), .. } = &sent(&mut overlay)[0] else { panic!() };
        assert!(values["text"]["speech"]["url"].as_str().unwrap().contains("hello"));
        activity.done(*id);
        activity.step(0);
        let PluginMessage::Run { values, .. } = &sent(&mut overlay)[0] else { panic!() };
        assert!(values["text"].get("speech").is_none(), "broken: without it");
    }

    #[test]
    fn history_follows_the_items() {
        let hub = OverlayHub::new();
        let activity = Activity::for_tests(hub, Speech::off());
        let mut changes = activity.subscribe();
        let bits = Signal::new(
            origin(),
            SignalKind::Support {
                user: user("v"),
                amount: Amount::Units { value: 100, unit: "bits".into() },
                message: None,
            },
        );
        let entry = activity.record(&bits, None).unwrap();
        assert!(
            activity
                .record(
                    &Signal::new(
                        origin(),
                        SignalKind::ChatMessage { user: user("v"), text: "hi".into(), first: false }
                    ),
                    None
                )
                .is_none()
        );
        activity.fired(entry, "bits-alert", "Bits alert");
        activity.fired(entry, "bits-alert", "Bits alert");
        let cause = Cause { entry: Some(entry), event: Some("V cheered 100 bits".into()), ..live() };
        let id = activity.push(alert("100 bits!"), &cause);
        let _player = player(&activity);
        activity.step(0);
        activity.done(id);

        let history = activity.history();
        assert_eq!(history[0].cards, ["Bits alert"]);
        assert_eq!(history[0].card_ids, ["bits-alert"]);
        assert_eq!(history[0].items[0].state, ItemState::Played);
        assert_eq!(activity.signal(entry), Some(bits.clone()));
        let replay = activity.record(&bits, Some(entry)).unwrap();
        assert_eq!(activity.history()[0].replay_of, Some(entry), "newest first");
        assert_ne!(replay, entry);
        let mut told = Vec::new();
        while let Ok(change) = changes.try_recv() {
            if let Change::Entry(entry) = change {
                told.push(entry.items.first().map(|item| item.state));
            }
        }
        assert!(
            told.contains(&Some(ItemState::OnStage)) && told.contains(&Some(ItemState::Played)),
            "{told:?}"
        );
    }

    /// What the panel may open a link in: a queue item's text while it's
    /// queued, and after, as its history entry kept it.
    #[test]
    fn an_items_text_while_queued_and_after() {
        let activity = Activity::for_tests(OverlayHub::new(), Speech::off());
        let raid = Signal::new(origin(), SignalKind::Raid { from: user("r"), viewers: 3 });
        let entry = activity.record(&raid, None).unwrap();
        let live_one = activity.push(alert("see twitch.tv/r"), &Cause { entry: Some(entry), ..live() });
        let test_one = activity.push(alert("a test"), &live());
        assert_eq!(activity.item_text(live_one).as_deref(), Some("see twitch.tv/r"), "waiting");
        assert_eq!(activity.item_text(test_one).as_deref(), Some("a test"));
        let _player = player(&activity);
        activity.step(0);
        assert_eq!(activity.item_text(live_one).as_deref(), Some("see twitch.tv/r"), "on stage");
        activity.done(live_one);
        activity.step(0);
        activity.done(test_one);
        assert_eq!(activity.item_text(live_one).as_deref(), Some("see twitch.tv/r"), "in the history");
        assert_eq!(activity.item_text(test_one), None, "no entry kept it");
        assert_eq!(activity.item_text(999), None);
    }

    #[test]
    fn each_kind_keeps_its_own_thousand() {
        let activity = Activity::for_tests(OverlayHub::new(), Speech::off());
        let raid = Signal::new(origin(), SignalKind::Raid { from: user("r"), viewers: 3 });
        let follow = Signal::new(origin(), SignalKind::Follow { user: user("f") });
        activity.record(&raid, None);
        for _ in 0..protocol::activity::HISTORY_LIMIT + 5 {
            activity.record(&follow, None);
        }
        let history = activity.history();
        assert_eq!(history.len(), protocol::activity::HISTORY_LIMIT + 1);
        assert_eq!(history.last().map(|entry| entry.summary.as_str()), Some("R raided with 3 viewers"));
    }

    #[test]
    fn waiting_events_are_kept_while_their_items_wait() {
        use crate::cards::{CardRun, Decided, Decision, Queued, Start};

        let dir = std::env::temp_dir().join(format!("lsk_waiting_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let activity = Activity::open(&dir, OverlayHub::new(), Speech::off());
        let raid = Signal::new(origin(), SignalKind::Raid { from: user("r"), viewers: 3 });
        let cause = Cause { event_id: activity.next_event(), ..live() };
        let run = |decided: Vec<Decided>, queued: Vec<Queued>| CardRun {
            card: "c".into(),
            name: "Card".into(),
            start: Start::Event,
            decided,
            queued,
            unpreparable: false,
        };
        let blocked = vec![Decided { node: 2, visit: 0, kind: 1, decision: Decision::Passed(false) }];
        let alerts = vec![
            Queued { node: 3, nth: 0, play: alert("one") },
            Queued { node: 3, nth: 1, play: alert("two") },
        ];

        // A run that only decided something isn't kept on its own; with
        // one that queued something, both are.
        let back = activity.ran(Some(&raid), &cause, 5, vec![run(blocked.clone(), Vec::new())]);
        assert_eq!(back.map(|runs| runs.len()), Some(1), "nothing waits: it comes back");
        let kept =
            activity.ran(Some(&raid), &cause, 5, vec![run(blocked, Vec::new()), run(Vec::new(), alerts)]);
        assert!(kept.is_none());
        {
            let state = activity.lock();
            let event = &state.events[0];
            assert_eq!((event.hearers.clone(), event.runs.len(), event.at_ms), (Hearers::Cards, 2, 5));
            let places: Vec<_> = state
                .waiting
                .iter()
                .map(|item| item.chain.map(|chain| (chain.run, chain.node, chain.nth)))
                .collect();
            assert_eq!(places, [Some((1, 3, 0)), Some((1, 3, 1))]);
        }

        // Kept across a restart.
        drop(activity);
        let again = Activity::open(&dir, OverlayHub::new(), Speech::off());
        assert_eq!(again.lock().events.len(), 1);
        assert!(again.next_event() > cause.event_id, "ids go on");

        // One on stage has left: its event won't queue it again. Both
        // played, the event is forgotten.
        let _player = player(&again);
        again.step(0);
        assert_eq!(again.lock().events[0].gone, std::collections::BTreeSet::from([(1, 3, 0)]));
        again.play_all();
        assert!(again.lock().events.is_empty());
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kept_across_restarts() {
        let dir = std::env::temp_dir().join(format!("lsk_activity_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let hub = OverlayHub::new();
        let activity = Activity::open(&dir, hub.clone(), Speech::off());
        let raid = Signal::new(origin(), SignalKind::Raid { from: user("r"), viewers: 3 });
        let entry = activity.record(&raid, None).unwrap();
        let cause = Cause { entry: Some(entry), ..live() };
        let first = activity.push(alert("on stage"), &cause);
        activity.push(alert("waiting"), &live());
        activity.control(QueueAction::Pause);
        let player = player(&activity);
        activity.control(QueueAction::Resume);
        activity.step(0);
        activity.control(QueueAction::Pause);
        assert!(activity.queue().on_stage.is_some());
        std::mem::forget(player); // the server stops with an overlay connected
        drop(activity);

        let again = Activity::open(&dir, hub, Speech::off());
        let queue = again.queue();
        assert!(queue.paused);
        let texts: Vec<String> = queue.waiting.iter().map(|item| item.text.clone()).collect();
        assert_eq!(texts, ["on stage", "waiting"], "what was on stage plays again");
        assert_eq!(again.history()[0].items[0].state, ItemState::Waiting);
        assert!(again.push(alert("new"), &live()) > first, "ids go on");
        std::fs::write(dir.join(HISTORY_FILE), "{ not json").unwrap();
        let broken = Activity::open(&dir, OverlayHub::new(), Speech::off());
        assert!(broken.history().is_empty());
        assert_eq!(bad_files(&dir, HISTORY_FILE), ["{ not json"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The seen marker moves under a row, or above everything, panels are
    /// told, and it's kept; a history kept before it is all seen.
    #[test]
    fn the_seen_marker() {
        let dir = std::env::temp_dir().join(format!("lsk_activity_seen_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let activity = Activity::open(&dir, OverlayHub::new(), Speech::off());
        let mut changes = activity.subscribe();
        let follow = Signal::new(origin(), SignalKind::Follow { user: user("f") });
        let ids: Vec<u64> = (0..4).map(|_| activity.record(&follow, None).unwrap()).collect();
        let shown = HistoryFilter::default();
        assert_eq!(activity.new_events(&shown), 4, "a new history: everything is new");
        activity.mark_seen(Some(ids[1]));
        assert_eq!((activity.seen(), activity.new_events(&shown)), (ids[1], 2));
        let mut told = Vec::new();
        while let Ok(change) = changes.try_recv() {
            if let Change::Seen(seen) = change {
                told.push(seen);
            }
        }
        assert_eq!(told, [ids[1]]);
        let replay = activity.record(&follow, Some(ids[0])).unwrap();
        assert_eq!(activity.new_events(&shown), 2, "a replay isn't new");
        let hidden = HistoryFilter { follows: false, ..HistoryFilter::default() };
        assert_eq!(activity.new_events(&hidden), 0);
        activity.mark_seen(None);
        assert_eq!((activity.seen(), activity.new_events(&shown)), (replay, 0));
        activity.mark_seen(Some(replay + 10));
        assert_eq!(activity.seen(), replay, "not past the newest");
        activity.mark_seen(Some(ids[2]));
        drop(activity);

        let again = Activity::open(&dir, OverlayHub::new(), Speech::off());
        assert_eq!(again.seen(), ids[2], "kept");
        let path = dir.join(HISTORY_FILE);
        let mut saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        saved.as_object_mut().unwrap().remove("seen");
        std::fs::write(&path, saved.to_string()).unwrap();
        drop(again);
        let old = Activity::open(&dir, OverlayHub::new(), Speech::off());
        assert_eq!((old.seen(), old.new_events(&shown)), (replay, 0), "kept before the marker: all seen");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A flood of events writes the files a moment later, once, not on
    /// every change; what's left is written when the app stops.
    #[tokio::test(flavor = "multi_thread")]
    async fn saved_a_moment_later_not_on_every_change() {
        let dir = std::env::temp_dir().join(format!("lsk_activity_later_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let saved = |file: &str| -> Option<serde_json::Value> {
            std::fs::read_to_string(dir.join(file)).ok().map(|text| serde_json::from_str(&text).unwrap())
        };
        let activity = Activity::open(&dir, OverlayHub::new(), Speech::off());
        let follow = Signal::new(origin(), SignalKind::Follow { user: user("f") });
        for _ in 0..100 {
            let entry = activity.record(&follow, None).unwrap();
            activity.push(alert("followed"), &Cause { entry: Some(entry), ..live() });
        }
        assert!(saved(HISTORY_FILE).is_none() && saved(QUEUE_FILE).is_none(), "not yet");

        let started = std::time::Instant::now();
        while saved(QUEUE_FILE).is_none() {
            assert!(started.elapsed() < Duration::from_secs(10), "saved within a moment");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(started.elapsed() >= Duration::from_millis(500));
        assert_eq!(saved(HISTORY_FILE).unwrap()["entries"].as_array().unwrap().len(), 100);
        assert_eq!(saved(QUEUE_FILE).unwrap()["items"].as_array().unwrap().len(), 100);

        // The app stops right after a change: it's written then.
        activity.control(QueueAction::Pause);
        activity.flush();
        assert_eq!(saved(QUEUE_FILE).unwrap()["paused"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The `.bad-…` files set aside next to `file`, their contents.
    fn bad_files(dir: &Path, file: &str) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|found| found.file_name().to_string_lossy().starts_with(&format!("{file}.bad-")))
            .map(|found| std::fs::read_to_string(found.path()).unwrap())
            .collect();
        found.sort();
        found
    }

    /// A change to the saved format (an entry or item that no longer
    /// reads) loses only that entry: the rest stay, and it's set aside.
    #[test]
    fn an_entry_that_cant_be_read_is_set_aside_alone() {
        let dir = std::env::temp_dir().join(format!("lsk_activity_bad_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let activity = Activity::open(&dir, OverlayHub::new(), Speech::off());
        let raid = Signal::new(origin(), SignalKind::Raid { from: user("r"), viewers: 3 });
        let follow = Signal::new(origin(), SignalKind::Follow { user: user("f") });
        activity.record(&raid, None);
        activity.record(&follow, None);
        activity.control(QueueAction::Pause);
        activity.push(alert("first"), &live());
        activity.push(alert("second"), &live());
        drop(activity);

        // The older entry and the first item no longer read.
        let edit = |file: &str, change: &dyn Fn(&mut serde_json::Value)| {
            let path = dir.join(file);
            let text = std::fs::read_to_string(&path).unwrap();
            let mut json: serde_json::Value = serde_json::from_str(&text).unwrap();
            change(&mut json);
            std::fs::write(&path, json.to_string()).unwrap();
        };
        edit(HISTORY_FILE, &|json| {
            json["entries"][1]["signal"]["kind"] = serde_json::json!({ "input": "gone" })
        });
        edit(QUEUE_FILE, &|json| json["items"][0]["play"] = serde_json::json!(7));

        let again = Activity::open(&dir, OverlayHub::new(), Speech::off());
        let history = again.history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].summary, "F followed");
        let queue = again.queue();
        assert!(queue.paused, "the other fields stay");
        assert_eq!(queue.waiting.iter().map(|item| item.text.as_str()).collect::<Vec<_>>(), ["second"]);
        assert!(again.push(alert("new"), &live()) > 2, "ids go on past the one set aside");

        let history_bad = bad_files(&dir, HISTORY_FILE);
        assert_eq!(history_bad.len(), 1);
        let aside: serde_json::Value = serde_json::from_str(&history_bad[0]).unwrap();
        assert_eq!(aside["entries"][0]["signal"]["kind"]["input"], "gone");
        let queue_bad = bad_files(&dir, QUEUE_FILE);
        assert_eq!(queue_bad.len(), 1);
        let aside: serde_json::Value = serde_json::from_str(&queue_bad[0]).unwrap();
        assert_eq!(aside["items"][0]["play"], 7);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
