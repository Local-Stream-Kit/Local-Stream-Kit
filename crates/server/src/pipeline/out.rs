//! Where what the pipeline does goes ([`Out`]): out to the overlays, the
//! queue, the history and plugins' server code; or, while waiting events
//! are prepared again, nowhere: what the card runs would queue stays in
//! their recording ([`crate::cards::Recording`]). Card runs reach the
//! outside only through here, so a card prepared again can't play, queue,
//! ask or record anything.

use std::sync::{Arc, Mutex};

use protocol::activity::ItemOrigin;
use protocol::plugin::{PluginMessage, RunViewer};
use protocol::{Command, PluginCommand};
use serde_json::{Map, Value as Json};

use crate::cards::{CardRun, Then};
use crate::model::Signal;
use crate::services::activity::{Activity, Cause, Heard, Spoken, ValueAsk};
use crate::services::overlay_hub::OverlayHub;
use crate::services::speech::{Made, Speech};

/// Where what the pipeline does goes.
pub enum Out<'a> {
    Live(Live<'a>),
    Preparing(Preparing<'a>),
}

/// Out for real: to the overlays, the queue, the history and plugins'
/// server code.
pub struct Live<'a> {
    overlay: &'a OverlayHub,
    speech: &'a Speech,
    activity: &'a Activity,
}

/// Nowhere.
pub struct Preparing<'a> {
    /// Only asked which voices can speak.
    speech: &'a Speech,
}

/// A run that plays at once (it doesn't wait in the queue): to its
/// plugin's page on the overlays, or its server code. One with an input
/// read aloud is sent once its message is made (without it, if it can't
/// be).
pub struct Instant {
    pub plugin: String,
    pub node: String,
    pub input: String,
    pub values: Map<String, Json>,
    pub changes: Map<String, Json>,
    pub settings: Map<String, Json>,
    /// For its server code to fire the node's own outputs with.
    pub run_id: Option<u64>,
    pub spoken: Option<Spoken>,
    pub origin: ItemOrigin,
}

impl<'a> Out<'a> {
    pub fn live(overlay: &'a OverlayHub, speech: &'a Speech, activity: &'a Activity) -> Out<'a> {
        Out::Live(Live { overlay, speech, activity })
    }

    pub fn preparing(speech: &'a Speech) -> Out<'a> {
        Out::Preparing(Preparing { speech })
    }

    /// Whether waiting events are being prepared again.
    pub fn preparing_again(&self) -> bool {
        matches!(self, Out::Preparing(_))
    }

    /// Whether `voice` can read aloud (asked, the same both ways).
    pub fn can_speak(&self, voice: &str) -> bool {
        match self {
            Out::Live(live) => live.speech.can_speak(voice),
            Out::Preparing(preparing) => preparing.speech.can_speak(voice),
        }
    }

    /// A new event's id in the queue (`Activity::next_event`).
    pub fn next_event(&self) -> u64 {
        match self {
            Out::Live(live) => live.activity.next_event(),
            Out::Preparing(_) => 0,
        }
    }

    /// Card runs of an event ended: what they queued waits its turn, kept
    /// with them (`Activity::ran`). The runs back when nothing was kept.
    pub fn ran(
        &self,
        signal: Option<&Signal>,
        cause: &Cause,
        now_ms: u64,
        runs: Vec<CardRun>,
    ) -> Option<Vec<CardRun>> {
        match self {
            Out::Live(live) => live.activity.ran(signal, cause, now_ms, runs),
            Out::Preparing(_) => Some(runs),
        }
    }

    /// A run that plays now.
    pub fn play_now(&mut self, run: Instant) {
        if let Out::Live(live) = self {
            live.play_now(run);
        }
    }

    /// An effect node's run whose own outputs are wired: its id, for its
    /// server code to fire them with (`Activity::remember_run`).
    pub fn remember_run(&mut self, plugin: &str, then: Then, cause: &Cause) -> u64 {
        match self {
            Out::Live(live) => live.activity.remember_run(plugin, then, cause),
            Out::Preparing(_) => 0,
        }
    }

    /// An event waits for plugins' value nodes (`Activity::compute`).
    pub fn compute(&mut self, then: Then, asks: Vec<ValueAsk>, cause: &Cause) -> bool {
        match self {
            Out::Live(live) => live.activity.compute(then, asks, cause),
            // (Held there: `CardRuntime::hold`.)
            Out::Preparing(_) => true,
        }
    }

    /// An event reached a plugin's flow node (`Activity::route`).
    #[allow(clippy::too_many_arguments)]
    pub fn route(
        &mut self,
        plugin: &str,
        node: &str,
        input: &str,
        values: Map<String, Json>,
        viewer: Option<RunViewer>,
        then: Then,
        cause: &Cause,
    ) -> bool {
        match self {
            Out::Live(live) => live.activity.route(plugin, node, input, values, viewer, then, cause),
            Out::Preparing(_) => true,
        }
    }

    /// Card `id`, named `name`, reacted to the event of history entry
    /// `entry`.
    pub fn fired(&self, entry: u64, id: &str, name: &str) {
        if let Out::Live(live) = self {
            live.activity.fired(entry, id, name);
        }
    }

    /// Records an event in the history, if it's one that matters on
    /// stream; its entry's id.
    pub fn record(&self, signal: &Signal, replay_of: Option<u64>) -> Option<u64> {
        match self {
            Out::Live(live) => live.activity.record(signal, replay_of),
            Out::Preparing(_) => None,
        }
    }

    /// The history is seen up to its newest entry.
    pub fn mark_all_seen(&self) {
        if let Out::Live(live) = self {
            live.activity.mark_seen(None);
        }
    }

    /// The event of a history entry, to replay it.
    pub fn signal(&self, entry: u64) -> Option<Signal> {
        match self {
            Out::Live(live) => live.activity.signal(entry),
            Out::Preparing(_) => None,
        }
    }

    /// Events whose plugins' value nodes didn't answer in time carry on.
    pub fn compute_overdue(&self, now_ms: u64) {
        if let Out::Live(live) = self {
            live.activity.compute_overdue(now_ms);
        }
    }

    /// For a plugin's server code that hears the chat or events.
    pub fn hear(&self, plugin: &str, heard: Heard) {
        if let Out::Live(live) = self {
            live.activity.hear(plugin, heard);
        }
    }

    /// To the overlays (a chat line for the pages that hear the chat).
    pub fn send(&self, command: Command) {
        if let Out::Live(live) = self {
            live.overlay.send(command);
        }
    }
}

impl Live<'_> {
    fn play_now(&self, run: Instant) {
        let Instant { plugin, node, input, values, changes, settings, run_id, spoken, origin } = run;
        let command = move |values| {
            let message = PluginMessage::Run {
                node,
                input,
                values,
                changes,
                settings,
                queue_id: None,
                run_id,
                preview: None,
                origin,
            };
            Command::Plugin(PluginCommand { plugin, message })
        };
        let Some(Spoken { input: spoken, job }) = spoken else {
            self.overlay.send(command(values));
            return;
        };
        // Sent once its message is made (without it, if it can't be).
        let volume = job.volume;
        let waiting = Arc::new(Mutex::new(Some((values, command))));
        let take = |waiting: &Mutex<Option<_>>| waiting.lock().ok().and_then(|mut waiting| waiting.take());
        let on_made = {
            let (overlay, waiting) = (self.overlay.clone(), Arc::clone(&waiting));
            move |made: Option<Made>| {
                let Some((mut values, command)) = take(&waiting) else { return };
                if let Some(made) = made {
                    crate::plugins::add_speech(&mut values, &spoken, made.clip(volume));
                }
                overlay.send(command(values));
            }
        };
        if !self.speech.make(job, on_made)
            && let Some((values, command)) = take(&waiting)
        {
            self.overlay.send(command(values));
        }
    }
}
