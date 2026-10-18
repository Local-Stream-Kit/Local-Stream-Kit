//! What cards ask plugins' server code (or a plugin's program, on the
//! control API) while they run: where one of its flow nodes sends an event
//! on (`route`), and its value nodes' values for an event (`compute`). A
//! card never waits for the answer: the event is parked (a flow node's as
//! a run, kept as effect runs are but apart from them,
//! [`Activity::remember_run`]; one waiting for value nodes as a
//! [`Computing`]) and the answer carries the card on, through the
//! pipeline's resumes, as a queued effect's Started and Ended do. What a
//! plugin won't answer because its server code stopped (or its program
//! went) is let go at once ([`Activity::unanswered`]).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use protocol::activity::ItemOrigin;
use protocol::cards::NodeId;
use protocol::plugin::RunViewer;
use serde_json::{Map, Value as Json};
use tokio::sync::mpsc;

use super::{Activity, Cause, Resume};
use crate::cards::{Then, Value};
use crate::pipeline::now_ms;

/// How many chat messages and events a plugin's server code may have
/// waiting to be heard; more push out the oldest.
pub const MAX_HEARD: usize = 500;
/// The log says that heard ones were dropped at most this often.
const HEARD_WARN_MS: u64 = 60_000;

/// What a plugin that hears the chat or events hears (`hears`).
#[derive(Debug, Clone)]
pub enum Heard {
    /// A chat message or a moderator taking some off: a
    /// [`PluginMessage::Chat`](protocol::plugin::PluginMessage::Chat),
    /// `ChatRemoved`, `ChatUserRemoved` or `ChatCleared`.
    Chat(protocol::plugin::PluginMessage),
    /// An event, as the built-in event node `kind` has it.
    Event {
        kind: String,
        summary: String,
        viewer: Option<RunViewer>,
        /// That node's outputs, as an effect node's run has values.
        values: Map<String, Json>,
        origin: ItemOrigin,
    },
}

/// What a plugin's server code has to hear: kept apart from its other
/// calls, which never wait behind more than one of these.
#[derive(Default)]
struct HeardQueue {
    waiting: VecDeque<Heard>,
    /// A wake ([`Asked::Heard`]) is on its way to the plugin.
    woken: bool,
    dropped: u64,
    warned_ms: Option<u64>,
}

impl HeardQueue {
    /// Adds one, pushing out the oldest when it's full. Returns whether
    /// to wake the plugin, and how many were dropped if it's time to say
    /// so.
    fn push(&mut self, heard: Heard, now_ms: u64) -> (bool, Option<u64>) {
        if self.waiting.len() >= MAX_HEARD {
            self.waiting.pop_front();
            self.dropped += 1;
        }
        self.waiting.push_back(heard);
        let warn = (self.dropped > 0
            && self.warned_ms.is_none_or(|at| now_ms.saturating_sub(at) >= HEARD_WARN_MS))
        .then(|| {
            self.warned_ms = Some(now_ms);
            std::mem::take(&mut self.dropped)
        });
        let wake = !self.woken;
        self.woken = true;
        (wake, warn)
    }

    /// The oldest one, and whether more wait (then the plugin is woken
    /// again).
    fn pop(&mut self) -> (Option<Heard>, bool) {
        let heard = self.waiting.pop_front();
        self.woken = !self.waiting.is_empty();
        (heard, self.woken)
    }
}

/// How many runs a plugin may leave unanswered, of its flow nodes and of
/// its value nodes ([`Pending`]).
const MAX_PENDING: usize = 1000;

/// Runs a plugin was asked about and answers later (server code's
/// `route-later` and `compute-later`, a program's every answer), with
/// their node, oldest first: at most [`MAX_PENDING`], the oldest forgotten.
#[derive(Default)]
pub struct Pending(BTreeMap<u64, String>);

impl Pending {
    pub fn park(&mut self, run: u64, node: &str) {
        self.0.insert(run, node.to_string());
        while self.0.len() > MAX_PENDING {
            self.0.pop_first();
        }
    }

    /// Run `run`'s node, if it waits.
    pub fn node(&self, run: u64) -> Option<&str> {
        self.0.get(&run).map(String::as_str)
    }

    /// Run `run`'s node, if it waits; it no longer does.
    pub fn take(&mut self, run: u64) -> Option<String> {
        self.0.remove(&run)
    }

    /// Every run waiting; none from now.
    pub fn drain(&mut self) -> impl Iterator<Item = u64> + use<> {
        std::mem::take(&mut self.0).into_keys()
    }
}

/// How long a value node has to answer; after that it gives nothing.
pub const COMPUTE_MS: u64 = 10_000;
/// How many events of a card wait for value nodes at once; more are
/// dropped (and logged).
pub const MAX_COMPUTING: usize = 100;

/// One of a plugin's value nodes to ask for an event.
pub struct ValueAsk {
    pub plugin: String,
    /// The node, on its card.
    pub node: NodeId,
    /// Its node type's id.
    pub ty: String,
    /// Its inputs and settings, as an effect node's run has them.
    pub values: Map<String, Json>,
}

/// An event waiting for value nodes' answers.
struct Computing {
    then: Then,
    cause: Cause,
    /// The answers still to come: by run, the plugin and the node.
    waiting: HashMap<u64, (String, NodeId)>,
    until_ms: u64,
}

/// The events waiting for value nodes.
#[derive(Default)]
pub(super) struct Computings {
    next_run: u64,
    events: Vec<Computing>,
}

/// Something a card asks a plugin's server code.
#[derive(Debug)]
pub enum Asked {
    /// One of its flow nodes got an event on `input`: where does it go?
    Route {
        /// The node type's id.
        node: String,
        input: String,
        /// Its other inputs and its settings, as an effect node's run has
        /// them.
        values: Map<String, Json>,
        viewer: Option<RunViewer>,
        /// The parked event: what the answer names.
        run: u64,
        origin: ItemOrigin,
    },
    /// One of its value nodes is needed for an event: its outputs' values.
    Compute {
        /// The node type's id.
        node: String,
        values: Map<String, Json>,
        /// What the answer names.
        run: u64,
        origin: ItemOrigin,
    },
    /// Something waits to be heard ([`Asks::next_heard`]).
    Heard,
}

/// The running plugins' inboxes for what cards ask, by plugin.
#[derive(Default)]
pub(super) struct Inboxes {
    next: u64,
    by_plugin: HashMap<String, Inbox>,
}

struct Inbox {
    id: u64,
    sender: mpsc::UnboundedSender<Asked>,
    heard: Arc<Mutex<HeardQueue>>,
}

/// A plugin's server code hears what cards ask it ([`Activity::asks`])
/// until this is dropped.
pub struct Asks {
    activity: Activity,
    plugin: String,
    id: u64,
    heard: Arc<Mutex<HeardQueue>>,
}

impl Asks {
    /// The oldest chat message or event it has to hear (after an
    /// [`Asked::Heard`]), and whether more wait: then wake it again.
    pub fn next_heard(&self) -> (Option<Heard>, bool) {
        self.heard.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).pop()
    }
}

impl Drop for Asks {
    fn drop(&mut self) {
        let mut inboxes = self.activity.inboxes();
        // A newer copy of the plugin may have taken over already.
        if inboxes.by_plugin.get(&self.plugin).is_some_and(|inbox| inbox.id == self.id) {
            inboxes.by_plugin.remove(&self.plugin);
        }
    }
}

impl Activity {
    fn inboxes(&self) -> std::sync::MutexGuard<'_, Inboxes> {
        self.inner.inboxes.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `plugin`'s server code is running: what cards ask it comes to the
    /// returned inbox (instead of an older one) until [`Asks`] is dropped.
    pub fn asks(&self, plugin: &str) -> (Asks, mpsc::UnboundedReceiver<Asked>) {
        let (sender, inbox) = mpsc::unbounded_channel();
        let mut inboxes = self.inboxes();
        inboxes.next += 1;
        let id = inboxes.next;
        let heard = Arc::new(Mutex::new(HeardQueue::default()));
        inboxes.by_plugin.insert(plugin.to_string(), Inbox { id, sender, heard: Arc::clone(&heard) });
        (Asks { activity: self.clone(), plugin: plugin.to_string(), id, heard }, inbox)
    }

    /// `plugin`'s server code hears `heard`, if it's running.
    pub fn hear(&self, plugin: &str, heard: Heard) {
        let (sender, queue) = match self.inboxes().by_plugin.get(plugin) {
            Some(inbox) => (inbox.sender.clone(), Arc::clone(&inbox.heard)),
            None => return,
        };
        let (wake, dropped) =
            queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(heard, now_ms());
        if let Some(dropped) = dropped {
            tracing::warn!(
                plugin,
                dropped,
                "the plugin's server code doesn't keep up with the chat and events; the oldest were dropped"
            );
        }
        if wake {
            let _ = sender.send(Asked::Heard);
        }
    }

    /// A card's event reached one of `plugin`'s flow nodes (`then`'s
    /// node): parks it, and asks the plugin where it goes. False (nothing
    /// parked) when the plugin's server code isn't running: the event
    /// goes nowhere.
    #[allow(clippy::too_many_arguments)]
    pub fn route(
        &self,
        plugin: &str,
        node: &str,
        input: &str,
        values: Map<String, Json>,
        viewer: Option<RunViewer>,
        then: Then,
        cause: &Cause,
    ) -> bool {
        let Some(sender) = self.inboxes().by_plugin.get(plugin).map(|inbox| inbox.sender.clone()) else {
            return false;
        };
        let run = self.keep_run(plugin, super::RunKind::Parked, then, cause);
        let asked = Asked::Route {
            node: node.to_string(),
            input: input.to_string(),
            values,
            viewer,
            run,
            origin: cause.origin,
        };
        if sender.send(asked).is_err() {
            self.forget_run(plugin, run);
            return false;
        }
        true
    }

    /// Something `plugin`'s server code was asked and won't answer, as it
    /// stopped: an event parked at its flow node goes nowhere (as if it
    /// weren't running), its value node gives nothing (the inputs wired
    /// from it fall back to their settings) at once.
    pub fn unanswered(&self, plugin: &str, asked: Asked) {
        match asked {
            Asked::Route { run, .. } => self.forget_run(plugin, run),
            // (Already answered, or too late: nothing waits.)
            Asked::Compute { run, .. } => {
                let _ = self.computed(plugin, run, Vec::new());
            }
            Asked::Heard => {}
        }
    }

    fn computings(&self) -> std::sync::MutexGuard<'_, Computings> {
        self.inner.computings.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// An event waits for plugins' value nodes (`asks`) before it goes
    /// into `then.into`: asks each, and resumes the card once all have
    /// answered, or after [`COMPUTE_MS`]. One whose plugin's server code
    /// isn't running gives nothing at once. False (nothing parked) when
    /// the card has [`MAX_COMPUTING`] waiting already.
    pub fn compute(&self, mut then: Then, asks: Vec<ValueAsk>, cause: &Cause) -> bool {
        let mut computings = self.computings();
        if computings.events.iter().filter(|computing| computing.then.card == then.card).count()
            >= MAX_COMPUTING
        {
            return false;
        }
        let inboxes = self.inboxes();
        let mut waiting = HashMap::new();
        for ask in asks {
            computings.next_run += 1;
            let run = computings.next_run;
            let asked = Asked::Compute { node: ask.ty, values: ask.values, run, origin: cause.origin };
            let sent =
                inboxes.by_plugin.get(&ask.plugin).is_some_and(|inbox| inbox.sender.send(asked).is_ok());
            if sent {
                waiting.insert(run, (ask.plugin, ask.node));
            } else {
                then.computed(ask.node, Vec::new());
            }
        }
        drop(inboxes);
        if waiting.is_empty() {
            drop(computings);
            self.resume_card(Resume { then, output: String::new(), cause: cause.clone() });
            return true;
        }
        let until_ms = now_ms() + COMPUTE_MS;
        computings.events.push(Computing { then, cause: cause.clone(), waiting, until_ms });
        true
    }

    /// `plugin`'s value node answered run `run` with `values` by output.
    /// Refused (why) for a run it wasn't asked, or one too late.
    pub fn computed(&self, plugin: &str, run: u64, values: Vec<(String, Value)>) -> Result<(), String> {
        let mut computings = self.computings();
        let index = computings
            .events
            .iter()
            .position(|computing| computing.waiting.get(&run).is_some_and(|(asked, _)| asked == plugin));
        let Some(index) = index else {
            return Err(format!(
                "no value node waits for run {run} (answered, more than 10 seconds old, or not its own)"
            ));
        };
        let computing = &mut computings.events[index];
        let (_, node) = computing.waiting.remove(&run).expect("found above");
        computing.then.computed(node, values);
        if computing.waiting.is_empty() {
            let Computing { then, cause, .. } = computings.events.remove(index);
            drop(computings);
            self.resume_card(Resume { then, output: String::new(), cause });
        }
        Ok(())
    }

    /// Events whose value nodes didn't all answer in time carry on (the
    /// pipeline's tick): those give nothing.
    pub fn compute_overdue(&self, now_ms: u64) {
        let overdue: Vec<Computing> = {
            let mut computings = self.computings();
            let (overdue, waiting) = std::mem::take(&mut computings.events)
                .into_iter()
                .partition(|computing| computing.until_ms <= now_ms);
            computings.events = waiting;
            overdue
        };
        for Computing { mut then, cause, waiting, .. } in overdue {
            for (plugin, node) in waiting.into_values() {
                tracing::warn!(plugin, card = then.card, "a value node didn't answer within 10 seconds");
                then.computed(node, Vec::new());
            }
            self.resume_card(Resume { then, output: String::new(), cause });
        }
    }

    /// Sends a card on (the pipeline's resumes).
    pub(super) fn resume_card(&self, resume: Resume) {
        if let Some(sender) = &*self.inner.resumes.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) {
            let _ = sender.send(resume);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cleared(platform: &str) -> Heard {
        Heard::Chat(protocol::plugin::PluginMessage::ChatCleared { platform: platform.into() })
    }

    fn platform(heard: &Heard) -> &str {
        match heard {
            Heard::Chat(protocol::plugin::PluginMessage::ChatCleared { platform }) => platform,
            _ => "",
        }
    }

    #[test]
    fn a_flood_drops_the_oldest_and_says_so_once_a_minute() {
        let mut queue = HeardQueue::default();
        assert_eq!(queue.push(cleared("0"), 0), (true, None), "the first wakes the plugin");
        for index in 1..MAX_HEARD {
            assert_eq!(queue.push(cleared(&index.to_string()), 0), (false, None), "woken already");
        }
        assert_eq!(queue.push(cleared("500"), 1), (false, Some(1)), "full: one dropped, said once");
        assert_eq!(queue.push(cleared("501"), 2), (false, None), "not again within the minute");
        assert_eq!(queue.push(cleared("502"), HEARD_WARN_MS + 1), (false, Some(2)), "a minute later");
        let (oldest, more) = queue.pop();
        assert_eq!((oldest.as_ref().map(platform), more), (Some("3"), true), "the oldest went");
        while queue.pop().1 {}
        assert_eq!(queue.push(cleared("again"), HEARD_WARN_MS + 2), (true, None), "emptied: woken again");
    }

    #[test]
    fn heard_only_by_a_running_plugin() {
        let activity = Activity::for_tests(
            crate::services::overlay_hub::OverlayHub::new(),
            crate::services::speech::Speech::off(),
        );
        activity.hear("chat-bot", cleared("twitch"));
        let (asks, mut inbox) = activity.asks("chat-bot");
        activity.hear("chat-bot", cleared("twitch"));
        activity.hear("chat-bot", cleared("youtube"));
        assert!(matches!(inbox.try_recv(), Ok(Asked::Heard)));
        assert!(inbox.try_recv().is_err(), "one wake for both");
        let (first, more) = asks.next_heard();
        assert_eq!((first.as_ref().map(platform), more), (Some("twitch"), true), "not what came before");
        assert!(!asks.next_heard().1);
    }

    fn activity() -> Activity {
        Activity::for_tests(
            crate::services::overlay_hub::OverlayHub::new(),
            crate::services::speech::Speech::off(),
        )
    }

    fn then() -> Then {
        use crate::pipeline::testing::{origin, user};
        let raid = crate::model::Signal::new(
            origin(),
            crate::model::SignalKind::Raid { from: user("r"), viewers: 3 },
        );
        Then::for_tests("card", 2, raid)
    }

    #[test]
    fn an_older_copy_stopping_leaves_the_newer_ones_inbox() {
        let activity = activity();
        let (older, _) = activity.asks("splitter");
        let (_newer, mut inbox) = activity.asks("splitter");
        drop(older);
        activity.hear("splitter", cleared("twitch"));
        assert!(matches!(inbox.try_recv(), Ok(Asked::Heard)), "still the newer copy's");
    }

    #[test]
    fn what_a_stopped_plugin_wont_answer_is_let_go() {
        let activity = activity();
        let mut resumes = activity.resumes();
        let (_asks, mut inbox) = activity.asks("money");
        assert!(activity.route("money", "split", "event", Map::new(), None, then(), &Cause::default()));
        let mut waiting = then();
        waiting.into = vec![(3, "trigger".into())];
        let ask = ValueAsk { plugin: "money".into(), node: 4, ty: "money".into(), values: Map::new() };
        assert!(activity.compute(waiting, vec![ask], &Cause::default()));
        let mut parked = None;
        while let Ok(asked) = inbox.try_recv() {
            if let Asked::Route { run, .. } = asked {
                parked = Some(run);
            }
            activity.unanswered("money", asked);
        }
        let resume = resumes.try_recv().expect("the value node gave nothing, at once");
        assert_eq!(resume.then.into, [(3, "trigger".to_string())]);
        assert!(resumes.try_recv().is_err(), "the parked event went nowhere");
        let forgotten = activity.carry_on("money", parked.expect("a route"), "event").unwrap_err();
        assert!(forgotten.starts_with("no run"), "{forgotten}");
    }
}
