//! The pipeline: Input → transformers → cards → effects.
//!
//! One task owns every transformer, card and effect, so stages run in a
//! fixed order without locks. Stage traits are synchronous; a stage that
//! needs async work spawns its own task and talks to it over a channel.

mod order;
mod out;
mod prepare;

pub use out::{Instant, Out};

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::bail;
use protocol::cards::{Card, NodeId};
use protocol::config::AppConfig;
use protocol::plugin::Catalog;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use tokio::sync::mpsc;

use protocol::activity::ItemOrigin;

use crate::cards::{CardRuntime, Recording, Start};
use crate::effects::Effects;
use crate::media::MediaFolders;
use crate::model::{ChannelRef, Input, Origin, Signal, SignalKind, SignalName};
use crate::providers::replay::Recorder;
use crate::services::Services;
#[cfg(test)]
use crate::services::activity::Activity;
use crate::services::activity::{Cause, Hearers, Resume, describe, name_colors};
use crate::services::emotes::EmoteRegistry;
#[cfg(test)]
use crate::services::overlay_hub::OverlayHub;
use crate::services::speech::Speech;
use crate::transformers;

/// How often transformers get `on_tick`.
const TICK: Duration = Duration::from_millis(250);

pub trait Transformer: Send {
    /// Signal kinds this transformer emits. Used to order transformers.
    fn produces(&self) -> &[SignalName];
    /// Signal kinds it wants from other transformers, via `on_signal`.
    fn consumes(&self) -> &[SignalName] {
        &[]
    }
    /// Emitted signals' `transformer` field is filled in by the pipeline.
    fn on_input(&mut self, _input: &Input, _ctx: &mut Ctx<'_>, _out: &mut Vec<Signal>) {}
    fn on_signal(&mut self, _signal: &Signal, _ctx: &mut Ctx<'_>, _out: &mut Vec<Signal>) {}
    fn on_tick(&mut self, _ctx: &mut Ctx<'_>, _out: &mut Vec<Signal>) {}
}

/// Randomness, injectable so tests are deterministic.
pub trait RandomSource: Send {
    /// A number in `0.0..1.0`.
    fn next_f64(&mut self) -> f64;

    /// An index in `0..len` (`len` must be > 0).
    fn index(&mut self, len: usize) -> usize {
        ((self.next_f64() * len as f64) as usize).min(len.saturating_sub(1))
    }

    /// A new event's seed, which its draws come from ([`Ctx::rng_at`]).
    fn seed(&mut self) -> u64;

    /// The draws of one place in a card run, from `key`: the same key
    /// gives the same draws.
    fn stream(&mut self, key: u64) -> Box<dyn RandomSource>;
}

pub struct StdRandom(StdRng);

impl StdRandom {
    pub fn new() -> StdRandom {
        StdRandom(StdRng::from_rng(&mut rand::rng()))
    }
}

impl RandomSource for StdRandom {
    fn next_f64(&mut self) -> f64 {
        self.0.random::<f64>()
    }

    fn seed(&mut self) -> u64 {
        self.0.random::<u64>()
    }

    fn stream(&mut self, key: u64) -> Box<dyn RandomSource> {
        Box::new(StdRandom(StdRng::seed_from_u64(key)))
    }
}

/// Where in a card run a draw is: from the event's seed, the card, the
/// node and how many times that node has drawn in this run. Stable across
/// builds (it's kept with waiting events).
fn draw_key(seed: u64, card: &str, node: NodeId, visit: u32) -> u64 {
    // FNV-1a for the card's id, then SplitMix64 to mix each part in.
    let card = card.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    let mix = |hash: u64, part: u64| {
        let mut z = (hash ^ part).wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    [card, u64::from(node), u64::from(visit)].into_iter().fold(seed, mix)
}

/// What stages can see while they run, and where what they do goes.
pub struct Ctx<'a> {
    /// Unix time in milliseconds.
    pub now_ms: u64,
    pub rng: &'a mut dyn RandomSource,
    pub emotes: &'a EmoteRegistry,
    /// The only way out: to the overlays, the queue, the history and
    /// plugins' server code, or, while preparing, nowhere.
    pub out: Out<'a>,
    /// The pictures and sounds folders, for settings that name a folder.
    pub media: &'a MediaFolders,
    /// Where what's queued now comes from: the event and the card (set by
    /// the pipeline as cards run), and the event's seed.
    pub cause: Cause,
    /// The card run going on: its draws, decisions and queued runs.
    pub recording: Recording,
}

impl<'a> Ctx<'a> {
    pub fn new(rng: &'a mut dyn RandomSource, services: &'a Services) -> Ctx<'a> {
        Ctx {
            now_ms: now_ms(),
            rng,
            emotes: &services.emotes,
            out: Out::live(&services.overlay, &services.speech, &services.activity),
            media: &services.media,
            cause: Cause::default(),
            recording: Recording::default(),
        }
    }

    /// For preparing waiting events again: nothing goes out.
    pub fn preparing(
        rng: &'a mut dyn RandomSource,
        emotes: &'a EmoteRegistry,
        media: &'a MediaFolders,
        speech: &'a Speech,
    ) -> Ctx<'a> {
        Ctx {
            now_ms: now_ms(),
            rng,
            emotes,
            out: Out::preparing(speech),
            media,
            cause: Cause::default(),
            recording: Recording::default(),
        }
    }

    /// The random numbers node `node` of card `card` draws: from the
    /// event's seed, the card, the node and how many times it has drawn
    /// in this run, so an event prepared again draws the same there,
    /// whatever else in the card changed.
    pub fn rng_at(&mut self, card: &str, node: NodeId) -> Box<dyn RandomSource> {
        let visit = self.recording.draw(node);
        self.rng.stream(draw_key(self.cause.seed, card, node, visit))
    }

    /// A run of card `card` starts at `start` (an event comes into it, or
    /// carries on from an output): its nodes' draws and decisions count
    /// from the start.
    pub fn begin_run(&mut self, card: &str, start: Start) {
        let name = self.cause.card.clone();
        self.recording.begin(card, &name, start);
    }

    /// The card run has ended (`signal`: the event, when it came into the
    /// card). Live, what it queued waits its turn, kept with the run (and
    /// the event's runs before it that decided something) so the event
    /// can be prepared again; preparing, the run is kept here
    /// ([`Recording::take`]).
    pub fn end_run(&mut self, signal: Option<&Signal>) {
        let Some(run) = self.recording.finish() else { return };
        if self.out.preparing_again() {
            self.recording.ended(run);
            return;
        }
        let event = self.cause.event_id;
        let mut runs = self.recording.pending(event);
        runs.push(run);
        if let Some(runs) = self.out.ran(signal, &self.cause, self.now_ms, runs) {
            self.recording.keep_pending(event, runs);
        }
    }

    /// Whether this is an event from the history, replayed.
    pub fn replay(&self) -> bool {
        self.cause.origin == ItemOrigin::Replay
    }

    #[cfg(test)]
    pub fn for_tests(
        rng: &'a mut dyn RandomSource,
        emotes: &'a EmoteRegistry,
        overlay: &'a OverlayHub,
        speech: &'a Speech,
        activity: &'a Activity,
    ) -> Ctx<'a> {
        Ctx {
            now_ms: 0,
            rng,
            emotes,
            out: Out::live(overlay, speech, activity),
            media: no_media(),
            cause: Cause::default(),
            recording: Recording::default(),
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// No pictures or sounds folders (tests).
#[cfg(test)]
pub fn no_media() -> &'static MediaFolders {
    static NO_MEDIA: std::sync::LazyLock<MediaFolders> = std::sync::LazyLock::new(MediaFolders::default);
    &NO_MEDIA
}

struct NamedTransformer {
    name: String,
    consumes: Vec<SignalName>,
    inner: Box<dyn Transformer>,
}

pub struct Pipeline {
    /// In run order: producers before their consumers.
    transformers: Vec<NamedTransformer>,
    /// Cards without problems; the ones that are off only run for a test
    /// aimed at them.
    cards: Vec<CardRuntime>,
    effects: Effects,
    /// Where the last live input came from: the channel Timers tick in.
    origin: Option<Origin>,
    /// The History's seen marker goes above everything when the stream
    /// starts (its filter's setting).
    seen_at_stream_start: bool,
}

impl Pipeline {
    /// The built-in transformers, and the config's cards and effects, with
    /// the plugins' node types attached. Cards that are off or have
    /// problems don't run (problems are logged).
    pub fn new(config: &AppConfig, plugins: &Catalog) -> Pipeline {
        let transformers: Vec<(String, Box<dyn Transformer>)> = vec![
            ("emotes".into(), Box::new(transformers::Emotes)),
            ("events".into(), Box::new(transformers::Events)),
        ];
        let mut cards = config.cards.clone();
        for card in &mut cards {
            card.attach_plugins(plugins);
        }
        let effects = Effects::new(&config.effects)
            .with_plugins(&config.plugins)
            .with_catalog(plugins)
            .with_text_styles(&config.text_styles, config.text_styles_enabled);
        let mut pipeline =
            Pipeline::from_parts(transformers, &cards, effects).expect("the built-in transformers are valid");
        pipeline.seen_at_stream_start = config.history.seen_marker && config.history.seen_at_stream_start;
        pipeline
    }

    fn from_parts(
        transformers: Vec<(String, Box<dyn Transformer>)>,
        cards: &[Card],
        effects: Effects,
    ) -> anyhow::Result<Pipeline> {
        let mut names = std::collections::HashSet::new();
        for (name, _) in &transformers {
            if !names.insert(name.as_str()) {
                bail!("two transformers are named {name:?}; names must be unique");
            }
        }

        let nodes: Vec<order::Node<'_>> = transformers
            .iter()
            .map(|(name, t)| order::Node { name, produces: t.produces(), consumes: t.consumes() })
            .collect();
        let order = order::run_order(&nodes)?;
        for (transformer, signal) in &order.unproduced {
            tracing::warn!(
                transformer,
                ?signal,
                "transformer consumes a signal no other transformer produces"
            );
        }
        drop(nodes);

        let mut slots: Vec<Option<(String, Box<dyn Transformer>)>> =
            transformers.into_iter().map(Some).collect();
        let transformers = order
            .run_order
            .iter()
            .map(|&index| {
                let (name, inner) = slots[index].take().expect("each index appears once");
                NamedTransformer { consumes: inner.consumes().to_vec(), name, inner }
            })
            .collect();

        let mut runtimes = Vec::new();
        for card in cards {
            let problems = card.problems();
            if problems.is_empty() {
                runtimes.push(CardRuntime::new(card.clone()));
            } else if card.enabled {
                let problems: Vec<&str> = problems.iter().map(|problem| problem.message.as_str()).collect();
                tracing::warn!(
                    card = card.name,
                    problems = problems.join(" "),
                    "card has problems; it won't run"
                );
            }
        }
        let running = runtimes.iter().filter(|card| card.enabled()).count();
        tracing::info!(running, cards = cards.len(), "cards loaded");
        Ok(Pipeline { transformers, cards: runtimes, effects, origin: None, seen_at_stream_start: false })
    }

    pub fn module_configs(&self) -> Vec<protocol::Command> {
        self.effects.module_configs()
    }

    /// Runs one round for an input and the cards it triggers. Returns every
    /// signal from the round. Test events (from the panel) aren't kept in
    /// the history.
    pub fn handle_input(&mut self, input: &Input, ctx: &mut Ctx<'_>) -> Vec<Signal> {
        self.effects.chat(input, ctx);
        let signals = self.round(Some(input), ctx);
        let origin = if input.origin.provider == TEST_PROVIDER {
            ItemOrigin::Test
        } else {
            self.origin = Some(input.origin.clone());
            ItemOrigin::Live
        };
        self.dispatch(&signals, None, origin, None, ctx);
        signals
    }

    /// As [`handle_input`](Self::handle_input), and the card with this id
    /// reacts even if it's off; or its Timers tick.
    pub fn handle_test(&mut self, test: &CardTest, ctx: &mut Ctx<'_>) {
        match &test.test {
            Tested::Input(input) => {
                let signals = self.round(Some(input), ctx);
                self.dispatch(&signals, Some(&test.card), ItemOrigin::Test, None, ctx);
            }
            Tested::Timers(origin) => {
                if let Some(card) = self.cards.iter_mut().find(|card| card.id() == test.card)
                    && card.test_timers(origin, ctx, &self.effects)
                {
                    tracing::debug!(card = card.name(), "card fired");
                }
            }
        }
    }

    /// Takes over the running cards' state that outlives a config change
    /// (`CardRuntime::carry_over`) from the pipeline this one replaces.
    pub fn carry_over(&mut self, old: Pipeline) {
        let mut old_cards: std::collections::HashMap<String, CardRuntime> =
            old.cards.into_iter().map(|card| (card.id().to_string(), card)).collect();
        for card in &mut self.cards {
            if let Some(old) = old_cards.remove(card.id()) {
                card.carry_over(old);
            }
        }
        self.origin = old.origin;
    }

    /// History entry `entry`'s event, through the cards that are on, as
    /// they are now (a new entry, marked as a replay).
    pub fn handle_replay(&mut self, entry: u64, ctx: &mut Ctx<'_>) {
        let Some(signal) = ctx.out.signal(entry) else {
            tracing::info!(entry, "that event is no longer in the history; nothing was replayed");
            return;
        };
        self.dispatch(std::slice::from_ref(&signal), None, ItemOrigin::Replay, Some(entry), ctx);
    }

    /// A queued run went on stage or ended: its card carries on from that
    /// output, as the card is now, if it's on (or the run came from a
    /// test) and still runs.
    pub fn handle_resume(&mut self, resume: &Resume, ctx: &mut Ctx<'_>) {
        let test = resume.cause.origin == ItemOrigin::Test;
        let Some(card) =
            self.cards.iter_mut().find(|card| card.id() == resume.then.card && (card.enabled() || test))
        else {
            return;
        };
        ctx.cause = resume.cause.clone();
        ctx.cause.card = card.name().to_string();
        if card.resume(&resume.then, &resume.output, ctx, &self.effects) {
            tracing::debug!(card = card.name(), output = resume.output, "card carried on");
            if let Some(entry) = ctx.cause.entry {
                ctx.out.fired(entry, card.id(), card.name());
            }
        }
        ctx.cause = Cause::default();
    }

    /// Transformers' ticks, then the cards': Delays send on what they held,
    /// Timers tick (in the channel live inputs last came from).
    pub fn handle_tick(&mut self, ctx: &mut Ctx<'_>) -> Vec<Signal> {
        // Events whose plugins' value nodes didn't answer in time carry on.
        ctx.out.compute_overdue(ctx.now_ms);
        let signals = self.round(None, ctx);
        self.dispatch(&signals, None, ItemOrigin::Live, None, ctx);
        let origin = self.origin.clone().unwrap_or_else(|| Origin {
            provider: "timer".into(),
            platform: "twitch".into(),
            channel: ChannelRef { id: String::new(), name: String::new() },
        });
        for card in &mut self.cards {
            card.on_tick(&origin, ctx, &self.effects);
            card.prune(ctx.now_ms);
        }
        signals
    }

    /// Transformers run in order; each gets the input (or the tick) plus
    /// every signal emitted earlier in this round that it consumes.
    fn round(&mut self, input: Option<&Input>, ctx: &mut Ctx<'_>) -> Vec<Signal> {
        let mut round: Vec<Signal> = Vec::new();
        for transformer in &mut self.transformers {
            let mut out = Vec::new();
            match input {
                Some(input) => transformer.inner.on_input(input, ctx, &mut out),
                None => transformer.inner.on_tick(ctx, &mut out),
            }
            if !transformer.consumes.is_empty() {
                for signal in &round {
                    if signal.transformer != transformer.name
                        && transformer.consumes.contains(&signal.kind.name())
                    {
                        transformer.inner.on_signal(signal, ctx, &mut out);
                    }
                }
            }
            for signal in &mut out {
                signal.transformer.clone_from(&transformer.name);
            }
            round.extend(out);
        }
        round
    }

    /// Signals to the cards that are on, and to `tested` (a card's id).
    /// Events that matter go in the history first (not test events), so
    /// what the cards queue is listed with them.
    fn dispatch(
        &mut self,
        signals: &[Signal],
        tested: Option<&str>,
        origin: ItemOrigin,
        replay_of: Option<u64>,
        ctx: &mut Ctx<'_>,
    ) {
        for signal in signals {
            if origin == ItemOrigin::Live && self.seen_at_stream_start && stream_started(signal) {
                tracing::info!("the stream started: the history is seen up to here");
                ctx.out.mark_all_seen();
            }
            let entry = match origin {
                ItemOrigin::Test => None,
                ItemOrigin::Live | ItemOrigin::Replay => ctx.out.record(signal, replay_of),
            };
            let described = describe(signal);
            let kind = described.as_ref().map(|described| described.kind);
            let event = described.map(|described| described.summary);
            // Its own seed: its draws are its own, and the same again when
            // it's prepared again (a replay rolls afresh).
            let seed = ctx.rng.seed();
            let event_id = ctx.out.next_event();
            let names = name_colors(signal);
            // Prepared again, it goes to every card that's on (a test of one card:
            // that one).
            let hearers = tested.map_or(Hearers::Cards, |card| Hearers::Card(card.to_string()));
            ctx.cause =
                Cause { entry, event, kind, origin, card: String::new(), names, seed, event_id, hearers };
            self.effects.hear_event(signal, origin, ctx);
            for card in self.cards.iter_mut().filter(|card| card.enabled() || Some(card.id()) == tested) {
                ctx.cause.card = card.name().to_string();
                if card.on_signal(signal, ctx, &self.effects) {
                    tracing::debug!(card = card.name(), "card fired");
                    if let Some(entry) = entry {
                        ctx.out.fired(entry, card.id(), card.name());
                    }
                }
            }
        }
        ctx.cause = Cause::default();
    }
}

/// The OBS plugin's Stream started.
fn stream_started(signal: &Signal) -> bool {
    matches!(&signal.kind, SignalKind::Plugin { plugin, node, .. }
        if plugin == protocol::plugin::OBS_PLUGIN && node == "stream_started")
}

/// The provider name of the panel's test events.
pub const TEST_PROVIDER: &str = "test";

/// A test for one card ("Test this card"), which reacts even if it's off.
pub struct CardTest {
    pub test: Tested,
    /// The card's id.
    pub card: String,
}

#[allow(clippy::large_enum_variant)] // one at a time, from the panel
pub enum Tested {
    /// A test event.
    Input(Input),
    /// Its Timers tick, in this channel.
    Timers(Origin),
}

/// The pipeline's inputs besides the providers': from the panel and the
/// config.
pub struct Requests {
    pub tests: mpsc::Receiver<CardTest>,
    /// History entries to replay.
    pub replays: mpsc::Receiver<u64>,
    /// Queued runs that went on stage or ended (`Activity::resumes`).
    pub resumes: mpsc::UnboundedReceiver<Resume>,
    pub updates: mpsc::Receiver<Pipeline>,
}

/// The pipeline task: feeds inputs and ticks through the pipeline until every
/// input sender is gone.
///
/// A new pipeline from `updates` (the config changed) replaces the running
/// one between inputs. Every Nth counts, Timers and Delays carry over
/// ([`Pipeline::carry_over`]); cooldowns and transformer state start over.
/// The waiting events are prepared again with it ([`Pipeline::prepare_again`]).
/// (Not at startup: the channel's emotes haven't loaded yet, and an
/// event's emotes would come out missing.)
pub async fn run(
    mut pipeline: Pipeline,
    mut inputs: mpsc::Receiver<Input>,
    mut requests: Requests,
    services: Services,
    mut recorder: Option<Recorder>,
) {
    let mut rng = StdRandom::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            input = inputs.recv() => {
                let Some(input) = input else { break };
                guarded("an input", || {
                    services.emotes.observe(&input);
                    if let Some(recorder) = &mut recorder {
                        recorder.record(&input);
                    }
                    let mut ctx = Ctx::new(&mut rng, &services);
                    let signals = pipeline.handle_input(&input, &mut ctx);
                    for signal in &signals {
                        tracing::debug!(signal = ?signal.kind.name(), transformer = %signal.transformer, "signal");
                    }
                });
            }
            Some(test) = requests.tests.recv() => {
                guarded("a test", || {
                    if let Tested::Input(input) = &test.test {
                        services.emotes.observe(input);
                    }
                    pipeline.handle_test(&test, &mut Ctx::new(&mut rng, &services));
                });
            }
            Some(entry) = requests.replays.recv() => {
                guarded("a replay", || pipeline.handle_replay(entry, &mut Ctx::new(&mut rng, &services)));
            }
            Some(resume) = requests.resumes.recv() => {
                guarded("a queued run", || pipeline.handle_resume(&resume, &mut Ctx::new(&mut rng, &services)));
            }
            _ = tick.tick() => {
                guarded("a tick", || {
                    pipeline.handle_tick(&mut Ctx::new(&mut rng, &services));
                });
            }
            Some(update) = requests.updates.recv() => {
                let old = std::mem::replace(&mut pipeline, update);
                guarded("a config change", || pipeline.carry_over(old));
                // What waits plays as the cards are now.
                guarded("preparing the waiting events again", || {
                    pipeline.prepare_again(&services.activity, &services.emotes, &services.media, &services.speech);
                });
            }
        }
    }

    tracing::info!("pipeline stopped: no more inputs");
}

/// Runs one step of the pipeline task, and carries on after a panic in it:
/// the panic hook has logged it (to Status too), and one bad input mustn't
/// stop the pipeline, and with it Twitch chat, until a restart. What the
/// step left half done stays so; the next input starts afresh.
fn guarded(what: &str, step: impl FnOnce()) -> bool {
    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)).is_ok();
    if !done {
        tracing::error!("the pipeline skipped {what} that failed (see the error above)");
    }
    done
}

#[cfg(test)]
pub mod testing {
    //! Helpers shared by tests across the crate.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::RandomSource;
    use crate::model::{ChannelRef, Origin, Roles, User};

    /// Returns the given numbers in a loop, in the order they're drawn,
    /// wherever in a card (its streams share the loop).
    pub struct FixedRandom {
        values: Arc<[f64]>,
        next: Arc<AtomicUsize>,
    }

    impl FixedRandom {
        pub fn new(values: &[f64]) -> FixedRandom {
            FixedRandom { values: values.into(), next: Arc::default() }
        }
    }

    impl RandomSource for FixedRandom {
        fn next_f64(&mut self) -> f64 {
            let next = self.next.fetch_add(1, Ordering::Relaxed);
            self.values[next % self.values.len()]
        }

        fn seed(&mut self) -> u64 {
            0
        }

        fn stream(&mut self, _key: u64) -> Box<dyn RandomSource> {
            Box::new(FixedRandom { values: Arc::clone(&self.values), next: Arc::clone(&self.next) })
        }
    }

    pub fn origin() -> Origin {
        Origin {
            provider: "twitch_chat".into(),
            platform: "twitch".into(),
            channel: ChannelRef { id: "100".into(), name: "chan".into() },
        }
    }

    pub fn user(name: &str) -> User {
        User {
            id: format!("id-{name}"),
            name: name.into(),
            display_name: name.to_uppercase(),
            roles: Roles::default(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{Command, config};

    use super::*;
    use crate::model::{InputKind, PlatformEvent, SignalKind};
    use crate::services::emotes::EmoteRegistry;
    use testing::{FixedRandom, origin, user};

    /// Emits `Custom { name }` for every input.
    struct Source(&'static str);
    impl Transformer for Source {
        fn produces(&self) -> &[SignalName] {
            &[SignalName::Custom]
        }
        fn on_input(&mut self, input: &Input, _ctx: &mut Ctx<'_>, out: &mut Vec<Signal>) {
            out.push(Signal::new(
                input.origin.clone(),
                SignalKind::Custom { name: self.0.into(), data: Default::default() },
            ));
        }
    }

    /// Turns every `Custom` it sees into a `Raid` (just to have a distinct kind).
    struct Relay;
    impl Transformer for Relay {
        fn produces(&self) -> &[SignalName] {
            &[SignalName::Raid]
        }
        fn consumes(&self) -> &[SignalName] {
            &[SignalName::Custom]
        }
        fn on_signal(&mut self, signal: &Signal, _ctx: &mut Ctx<'_>, out: &mut Vec<Signal>) {
            out.push(Signal::new(
                signal.origin.clone(),
                SignalKind::Raid { from: user("relay"), viewers: 1 },
            ));
        }
    }

    fn input(kind: InputKind) -> Input {
        Input { at_ms: 0, origin: origin(), kind }
    }

    fn no_effects() -> Effects {
        Effects::new(&config::Effects::default())
    }

    #[test]
    fn chained_transformers_see_earlier_signals_in_the_same_round() {
        // The relay is listed first but must run after the source it consumes.
        let mut pipeline = Pipeline::from_parts(
            vec![("relay".into(), Box::new(Relay)), ("source".into(), Box::new(Source("hello")))],
            &[],
            no_effects(),
        )
        .unwrap();
        let emotes = EmoteRegistry::for_tests();
        let overlay = OverlayHub::new();
        let mut rng = FixedRandom::new(&[0.0]);
        let speech = Speech::off();
        let activity = Activity::for_tests(overlay.clone(), speech.clone());
        let mut ctx = Ctx::for_tests(&mut rng, &emotes, &overlay, &speech, &activity);
        let custom =
            input(InputKind::Event(PlatformEvent::Custom { name: "x".into(), data: Default::default() }));
        let signals = pipeline.handle_input(&custom, &mut ctx);
        let kinds: Vec<_> = signals.iter().map(|s| (s.transformer.as_str(), s.kind.name())).collect();
        assert_eq!(kinds, [("source", SignalName::Custom), ("relay", SignalName::Raid)]);
    }

    #[test]
    fn refuses_loops_and_duplicate_names() {
        struct Loop(SignalName, SignalName);
        impl Transformer for Loop {
            fn produces(&self) -> &[SignalName] {
                std::slice::from_ref(&self.0)
            }
            fn consumes(&self) -> &[SignalName] {
                std::slice::from_ref(&self.1)
            }
        }
        let looping = Pipeline::from_parts(
            vec![
                ("a".into(), Box::new(Loop(SignalName::Raid, SignalName::Custom))),
                ("b".into(), Box::new(Loop(SignalName::Custom, SignalName::Raid))),
            ],
            &[],
            no_effects(),
        );
        assert!(looping.err().unwrap().to_string().contains("loop"));
        let duplicate = Pipeline::from_parts(
            vec![("a".into(), Box::new(Source("1"))), ("a".into(), Box::new(Source("2")))],
            &[],
            no_effects(),
        );
        assert!(duplicate.err().unwrap().to_string().contains("unique"));
    }

    #[tokio::test]
    async fn the_pipeline_carries_on_after_a_panic_in_one_input() {
        /// Panics on a raid with no viewers; counts the others.
        struct Fragile(std::sync::Arc<std::sync::atomic::AtomicUsize>);
        impl Transformer for Fragile {
            fn produces(&self) -> &[SignalName] {
                &[SignalName::Custom]
            }
            fn on_input(&mut self, input: &Input, _ctx: &mut Ctx<'_>, _out: &mut Vec<Signal>) {
                if let InputKind::Event(PlatformEvent::Raid { viewers: 0, .. }) = input.kind {
                    panic!("a bad input");
                }
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pipeline = Pipeline::from_parts(
            vec![("fragile".into(), Box::new(Fragile(std::sync::Arc::clone(&seen))))],
            &[],
            no_effects(),
        )
        .unwrap();
        let overlay = OverlayHub::new();
        let speech = Speech::off();
        let activity = Activity::for_tests(overlay.clone(), speech.clone());
        let services = Services {
            emotes: std::sync::Arc::new(EmoteRegistry::for_tests()),
            overlay,
            status: crate::services::status::StatusHub::new(),
            speech,
            activity: activity.clone(),
            media: Default::default(),
        };
        let (_tests, tests) = mpsc::channel(1);
        let (_replays, replays) = mpsc::channel(1);
        let (_updates, updates) = mpsc::channel(1);
        let requests = Requests { tests, replays, resumes: activity.resumes(), updates };
        let (inputs, receiver) = mpsc::channel(4);
        let raid = |viewers| input(InputKind::Event(PlatformEvent::Raid { from: user("r"), viewers }));
        inputs.send(raid(0)).await.unwrap();
        inputs.send(raid(5)).await.unwrap();
        drop(inputs);
        run(pipeline, receiver, requests, services, None).await;
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1, "the input after the panic");
    }

    #[test]
    fn premade_cards_react_to_events_and_skip_disabled_or_broken_ones() {
        let mut config = AppConfig { cards: protocol::cards::premade(), ..AppConfig::default() };
        // The premade examples are off; a broken card is skipped.
        let mut broken = config.cards[0].clone();
        broken.id = "broken".into();
        broken.wires.clear();
        config.cards.push(broken);
        let built_in =
            crate::plugins::Plugins::load_from(std::path::Path::new("no such folder"), true).catalog();
        let mut pipeline = Pipeline::new(&config, &built_in);
        assert_eq!(pipeline.cards.len(), 12, "the premade cards; the broken one is left out");
        assert_eq!(pipeline.cards.iter().filter(|card| card.enabled()).count(), 6, "six are on");

        let emotes = EmoteRegistry::for_tests();
        let overlay = OverlayHub::new();
        let mut receiver = overlay.subscribe();
        let mut rng = FixedRandom::new(&[0.0]);
        let speech = Speech::off();
        let activity = Activity::for_tests(overlay.clone(), speech.clone());
        let mut ctx = Ctx::for_tests(&mut rng, &emotes, &overlay, &speech, &activity);
        let raid = input(InputKind::Event(PlatformEvent::Raid { from: user("r"), viewers: 7 }));
        pipeline.handle_input(&raid, &mut ctx);
        activity.play_all();
        let envelope = receiver.try_recv().unwrap();
        let Command::Plugin(command) = &envelope.command else { panic!("an alert") };
        let protocol::plugin::PluginMessage::Run { values, settings, .. } = &command.message else {
            panic!("a run")
        };
        assert_eq!(command.plugin, protocol::plugin::ALERT_PLUGIN);
        assert_eq!(
            (&values["message"]["text"], &values["duration"]),
            (&"R is raiding with 7 viewers".into(), &6.0.into())
        );
        assert_eq!(settings["position"], "top", "the first style");
    }

    #[test]
    fn events_are_kept_with_what_they_caused_and_replayed() {
        let mut config = AppConfig { cards: protocol::cards::premade(), ..AppConfig::default() };
        config.cards.retain(|card| card.id == "raid-alert");
        let built_in =
            crate::plugins::Plugins::load_from(std::path::Path::new("no such folder"), true).catalog();
        let mut pipeline = Pipeline::new(&config, &built_in);
        let emotes = EmoteRegistry::for_tests();
        let overlay = OverlayHub::new();
        let mut rng = FixedRandom::new(&[0.0]);
        let speech = Speech::off();
        let activity = Activity::for_tests(overlay.clone(), speech.clone());
        let mut ctx = Ctx::for_tests(&mut rng, &emotes, &overlay, &speech, &activity);
        let raid = input(InputKind::Event(PlatformEvent::Raid { from: user("r"), viewers: 7 }));
        pipeline.handle_input(&raid, &mut ctx);

        let history = activity.history();
        assert_eq!(history.len(), 1);
        let entry = &history[0];
        assert_eq!(
            (entry.summary.as_str(), entry.cards.clone()),
            ("R raided with 7 viewers", vec!["Raid alert".to_string()])
        );
        assert_eq!(entry.items[0].state, protocol::activity::ItemState::Waiting);
        let waiting = activity.queue().waiting;
        assert_eq!(waiting[0].event.as_deref(), Some("R raided with 7 viewers"));
        assert_eq!(waiting[0].card, "Raid alert");

        pipeline.handle_replay(entry.id, &mut ctx);
        let history = activity.history();
        assert_eq!((history.len(), history[0].replay_of), (2, Some(entry.id)));
        assert_eq!(activity.queue().waiting[1].origin, ItemOrigin::Replay);

        // Test events go through the queue, not into the history.
        let mut test = raid.clone();
        test.origin.provider = TEST_PROVIDER.into();
        pipeline.handle_input(&test, &mut ctx);
        assert_eq!(activity.history().len(), 2);
        assert_eq!(activity.queue().waiting[2].origin, ItemOrigin::Test);
    }

    #[test]
    fn the_stream_starting_marks_the_history_seen_when_asked() {
        let started = || {
            input(InputKind::Event(PlatformEvent::Plugin {
                plugin: protocol::plugin::OBS_PLUGIN.into(),
                node: "stream_started".into(),
                values: Default::default(),
                output: None,
                to: None,
            }))
        };
        let seen_after_start = |seen_marker: bool, at_start: bool| {
            let mut config = AppConfig { cards: protocol::cards::premade(), ..AppConfig::default() };
            config.cards.clear();
            config.history.seen_marker = seen_marker;
            config.history.seen_at_stream_start = at_start;
            let mut pipeline = Pipeline::new(&config, &Catalog::default());
            let emotes = EmoteRegistry::for_tests();
            let overlay = OverlayHub::new();
            let mut rng = FixedRandom::new(&[0.0]);
            let speech = Speech::off();
            let activity = Activity::for_tests(overlay.clone(), speech.clone());
            let mut ctx = Ctx::for_tests(&mut rng, &emotes, &overlay, &speech, &activity);
            pipeline.handle_input(
                &input(InputKind::Event(PlatformEvent::Raid { from: user("r"), viewers: 7 })),
                &mut ctx,
            );
            let mut test = started();
            test.origin.provider = TEST_PROVIDER.into();
            pipeline.handle_input(&test, &mut ctx);
            assert_eq!(activity.seen(), 0, "not a test's");
            pipeline.handle_input(&started(), &mut ctx);
            activity.seen()
        };
        assert_eq!(seen_after_start(true, true), 1);
        assert_eq!(seen_after_start(true, false), 0);
        assert_eq!(seen_after_start(false, true), 0, "not while the marker is off");
    }

    #[test]
    fn a_card_that_is_off_reacts_only_to_its_own_tests() {
        let mut config = AppConfig { cards: protocol::cards::premade(), ..AppConfig::default() };
        config.cards.retain(|card| card.id == "raid-alert");
        config.cards[0].enabled = false;
        let built_in =
            crate::plugins::Plugins::load_from(std::path::Path::new("no such folder"), true).catalog();
        let mut pipeline = Pipeline::new(&config, &built_in);
        let emotes = EmoteRegistry::for_tests();
        let overlay = OverlayHub::new();
        let mut rng = FixedRandom::new(&[0.0]);
        let speech = Speech::off();
        let activity = Activity::for_tests(overlay.clone(), speech.clone());
        let mut ctx = Ctx::for_tests(&mut rng, &emotes, &overlay, &speech, &activity);
        let raid = || input(InputKind::Event(PlatformEvent::Raid { from: user("r"), viewers: 7 }));
        let waiting = || activity.queue().waiting_count;

        pipeline.handle_input(&raid(), &mut ctx);
        assert_eq!(waiting(), 0, "off: a real raid does nothing");
        pipeline.handle_test(&CardTest { test: Tested::Input(raid()), card: "other".into() }, &mut ctx);
        assert_eq!(waiting(), 0, "a test for another card does nothing");
        pipeline.handle_test(&CardTest { test: Tested::Input(raid()), card: "raid-alert".into() }, &mut ctx);
        assert_eq!(waiting(), 1, "its own test shows the alert");
    }
}
