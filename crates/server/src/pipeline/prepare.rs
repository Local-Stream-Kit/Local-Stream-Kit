//! Preparing the waiting events again when the cards change.

use std::collections::HashMap;

use protocol::activity::{ItemOrigin, Prepared};

use super::{Ctx, Pipeline, StdRandom};
use crate::cards::{CardRun, CardRuntime, Start};
use crate::media::MediaFolders;
use crate::services::activity::{Activity, Hearers, KeptRun, PreparedEvent, WaitingEvent};
use crate::services::emotes::EmoteRegistry;
use crate::services::speech::Speech;

impl Pipeline {
    /// Prepares the waiting events again with the cards as they are now:
    /// each of their runs from where it started, as it decided then, on
    /// copies of the cards (whose state, for what's new, goes through the
    /// events in order), with nothing sent out. Cards that hear an event
    /// now and didn't then run for it too. What comes out different
    /// changes in the queue; what changed.
    pub fn prepare_again(
        &self,
        activity: &Activity,
        emotes: &EmoteRegistry,
        media: &MediaFolders,
        speech: &Speech,
    ) -> Prepared {
        let events = activity.waiting_events();
        if events.is_empty() {
            return Prepared::default();
        }
        // (Draws come from each event's seed.)
        let mut rng = StdRandom::new();
        let mut ctx = Ctx::preparing(&mut rng, emotes, media, speech);
        let mut copies: HashMap<String, CardRuntime> = HashMap::new();
        let mut held = 0;
        let mut prepared = Vec::new();
        for event in &events {
            let mut runs = Vec::new();
            for kept in &event.runs {
                let run = self.prepare_run(event, kept, &mut copies, &mut ctx);
                if run.as_ref().is_some_and(|run| run.unpreparable) {
                    held += 1;
                    runs.push(None);
                } else {
                    runs.push(run);
                }
            }
            let mut added = Vec::new();
            for card in self.cards.iter().filter(|card| hears(event, card)) {
                let came_in = |kept: &KeptRun| kept.card == card.id() && kept.start == Start::Event;
                if event.runs.iter().any(came_in) {
                    continue;
                }
                let kept = KeptRun {
                    card: card.id().to_string(),
                    name: card.name().to_string(),
                    start: Start::Event,
                    at_ms: event.at_ms,
                    decided: Vec::new(),
                };
                if let Some(run) = self.prepare_run(event, &kept, &mut copies, &mut ctx)
                    && !run.unpreparable
                    && !run.queued.is_empty()
                {
                    added.push(run);
                }
            }
            prepared.push(PreparedEvent { id: event.id, runs, added });
        }
        let note = activity.prepared_again(prepared);
        if note.changed() {
            tracing::info!(
                updated = note.updated,
                added = note.added,
                withdrawn = note.withdrawn,
                back = note.back,
                "the cards changed: the waiting events were prepared again"
            );
        }
        if held > 0 {
            tracing::info!(
                runs = held,
                "some waiting items stay as they were: their card now holds the event (at a Delay, or a \
                 plugin's flow or value node) where it didn't before"
            );
        }
        note
    }

    /// Run `kept` of waiting event `event` prepared again, on its card's
    /// copy. A card that's off (one not tested), gone or with a problem
    /// queues nothing; `None` only if its card is that.
    fn prepare_run(
        &self,
        event: &WaitingEvent,
        kept: &KeptRun,
        copies: &mut HashMap<String, CardRuntime>,
        ctx: &mut Ctx<'_>,
    ) -> Option<CardRun> {
        let runs = |card: &&CardRuntime| match &kept.start {
            Start::Event => hears(event, card),
            // As a queued run carries on (`Pipeline::handle_resume`).
            Start::Then { .. } => card.enabled() || event.cause.origin == ItemOrigin::Test,
        };
        let Some(card) = self.cards.iter().filter(runs).find(|card| card.id() == kept.card) else {
            // Nothing from it waits; what it decided is kept, should it
            // run again.
            return Some(CardRun {
                card: kept.card.clone(),
                name: kept.name.clone(),
                start: kept.start.clone(),
                decided: kept.decided.clone(),
                queued: Vec::new(),
                unpreparable: false,
            });
        };
        let copy = copies.entry(kept.card.clone()).or_insert_with(|| card.copy());
        ctx.cause = event.cause.clone();
        ctx.cause.card = card.name().to_string();
        ctx.now_ms = kept.at_ms;
        ctx.recording.prepare_next(kept.decided.clone());
        match &kept.start {
            Start::Event => copy.on_signal(&event.signal, ctx, &self.effects),
            Start::Then { then, output } => copy.resume(then, output, ctx, &self.effects),
        };
        ctx.recording.take().pop()
    }
}

/// Whether `card` hears `event` coming in, prepared again.
fn hears(event: &WaitingEvent, card: &CardRuntime) -> bool {
    match &event.hearers {
        Hearers::Cards => card.enabled(),
        Hearers::Card(tested) => card.id() == tested,
        Hearers::Own => false,
    }
}

#[cfg(test)]
mod tests {
    use protocol::activity::ItemState;
    use protocol::cards::{Card, Category, Node, NodeKind, SocketRef, SocketType, Wire};
    use protocol::config::AppConfig;
    use protocol::panel::{TestEvent, TestHypeTrain};
    use protocol::plugin::Catalog;

    use super::*;
    use crate::pipeline::no_media;
    use crate::services::activity::Play;
    use crate::services::overlay_hub::OverlayHub;

    /// The app as the pipeline task has it, with no overlay connected: what
    /// it queues waits.
    struct App {
        emotes: EmoteRegistry,
        overlay: OverlayHub,
        speech: Speech,
        activity: Activity,
        catalog: Catalog,
    }

    impl App {
        fn new() -> App {
            let (overlay, speech) = (OverlayHub::new(), Speech::off());
            App {
                emotes: EmoteRegistry::for_tests(),
                activity: Activity::for_tests(overlay.clone(), speech.clone()),
                overlay,
                speech,
                catalog: crate::plugins::Plugins::load_from(std::path::Path::new("no such folder"), true)
                    .catalog(),
            }
        }

        fn pipeline(&self, config: &AppConfig) -> Pipeline {
            Pipeline::new(config, &self.catalog)
        }

        /// Live events come in, a second apart.
        fn events(&self, pipeline: &mut Pipeline, events: Vec<TestEvent>) {
            let mut rng = StdRandom::new();
            for (n, event) in events.into_iter().enumerate() {
                for mut input in crate::panel::unmarked_test_inputs(event, None) {
                    input.origin.provider = "twitch".into();
                    let mut ctx =
                        Ctx::for_tests(&mut rng, &self.emotes, &self.overlay, &self.speech, &self.activity);
                    ctx.now_ms = n as u64 * 1000;
                    pipeline.handle_input(&input, &mut ctx);
                }
            }
        }

        /// The config changed: its pipeline takes over, and the waiting
        /// events are prepared again.
        fn change(&self, old: Pipeline, config: &AppConfig) -> (Pipeline, Prepared) {
            let mut pipeline = self.pipeline(config);
            pipeline.carry_over(old);
            let note = pipeline.prepare_again(&self.activity, &self.emotes, no_media(), &self.speech);
            (pipeline, note)
        }

        /// What waits, as panels list it: ids and texts.
        fn waiting(&self) -> Vec<(u64, String)> {
            self.activity.queue().waiting.iter().map(|item| (item.id, item.text.clone())).collect()
        }

        fn texts(&self) -> Vec<String> {
            self.waiting().into_iter().map(|(_, text)| text).collect()
        }
    }

    fn raid(viewers: u32) -> TestEvent {
        TestEvent::Raid { viewers }
    }

    /// An alert showing `message`.
    fn alert_node(message: &str) -> NodeKind {
        let mut settings = serde_json::Map::new();
        settings.insert("message".into(), message.into());
        NodeKind::Plugin {
            plugin: protocol::plugin::ALERT_PLUGIN.into(),
            node: protocol::plugin::ALERT_NODE.into(),
            settings,
            changes: Default::default(),
            voice: Default::default(),
            ty: Default::default(),
        }
    }

    fn node(id: u32, kind: NodeKind) -> Node {
        Node { id, x: 0.0, y: 0.0, kind }
    }

    fn wire(from: u32, output: &str, to: u32, input: &str) -> Wire {
        Wire { from: SocketRef::new(from, output), to: SocketRef::new(to, input) }
    }

    /// The raid alert alone, its message `message`.
    fn raid_alert(message: &str) -> AppConfig {
        let mut config = AppConfig { cards: protocol::cards::premade(), ..AppConfig::default() };
        config.cards.retain(|card| card.id == "raid-alert");
        config.cards[0].nodes[1].kind = alert_node(message);
        config
    }

    /// The raid alert with `kind` put between the raid and the alert.
    fn raid_alert_through(kind: NodeKind) -> AppConfig {
        let mut config = raid_alert("{raider} raided");
        let card = &mut config.cards[0];
        card.nodes.push(node(9, kind));
        card.wires = vec![wire(1, "event", 9, "event"), wire(9, "event", 2, "trigger")];
        config
    }

    #[test]
    fn a_fixed_message_shows_on_the_alerts_waiting() {
        let app = App::new();
        let mut pipeline = app.pipeline(&raid_alert("{raider} raided"));
        app.events(&mut pipeline, vec![raid(3), raid(5)]);
        let before = app.waiting();
        assert_eq!(app.texts(), ["TestViewer raided", "TestViewer raided"]);

        let (_, note) = app.change(pipeline, &raid_alert("{raider} raided with {viewers}!"));
        assert_eq!(note, Prepared { updated: 2, ..Prepared::default() });
        assert_eq!(app.texts(), ["TestViewer raided with 3!", "TestViewer raided with 5!"]);
        let ids = |waiting: Vec<(u64, String)>| waiting.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
        assert_eq!(ids(app.waiting()), ids(before), "the same items, in their places");
        assert_eq!(app.activity.history()[0].items[0].text, "TestViewer raided with 5!", "the history too");
    }

    #[test]
    fn a_card_switched_off_takes_its_items_out_and_on_again_brings_them_back() {
        let app = App::new();
        let config = raid_alert("{raider} raided");
        let mut pipeline = app.pipeline(&config);
        app.events(&mut pipeline, vec![raid(3), raid(5)]);
        let before = app.waiting();

        let mut off = config.clone();
        off.cards[0].enabled = false;
        let (pipeline, note) = app.change(pipeline, &off);
        assert_eq!(note, Prepared { withdrawn: 2, ..Prepared::default() });
        assert!(app.waiting().is_empty());
        assert_eq!(app.activity.history()[0].items[0].state, ItemState::Withdrawn);

        let (_, note) = app.change(pipeline, &config);
        assert_eq!(note, Prepared { back: 2, ..Prepared::default() });
        assert_eq!(app.waiting(), before, "where they were");
        assert_eq!(app.activity.history()[0].items[0].state, ItemState::Waiting);
    }

    #[test]
    fn withdrawn_items_leave_for_good_once_the_queue_reaches_them() {
        let app = App::new();
        let config = raid_alert("{raider} raided");
        let mut pipeline = app.pipeline(&config);
        app.events(&mut pipeline, vec![raid(3), raid(5)]);
        let mut off = config.clone();
        off.cards[0].enabled = false;
        let (pipeline, _) = app.change(pipeline, &off);

        app.activity.play_all();
        let states: Vec<ItemState> =
            app.activity.history().iter().map(|entry| entry.items[0].state).collect();
        assert_eq!(states, [ItemState::Withdrawn, ItemState::Withdrawn]);
        let (_, note) = app.change(pipeline, &config);
        assert!(!note.changed(), "too late to bring them back");
        assert!(app.waiting().is_empty());
    }

    #[test]
    fn a_card_switched_on_adds_to_the_events_waiting() {
        let app = App::new();
        let mut config = raid_alert("{raider} raided");
        let mut shout = config.cards[0].clone();
        (shout.id, shout.name, shout.enabled) = ("shout".into(), "Shout".into(), false);
        shout.nodes[1].kind = alert_node("Go follow {raider}");
        config.cards.push(shout);
        let mut pipeline = app.pipeline(&config);
        app.events(&mut pipeline, vec![raid(3), raid(5)]);

        config.cards[1].enabled = true;
        let (_, note) = app.change(pipeline, &config);
        assert_eq!(note, Prepared { added: 2, ..Prepared::default() });
        assert_eq!(
            app.texts(),
            ["TestViewer raided", "Go follow TestViewer", "TestViewer raided", "Go follow TestViewer"],
            "each next to its event's"
        );
        assert_eq!(app.activity.history()[0].cards, ["Raid alert", "Shout"], "the history lists it too");
    }

    #[test]
    fn a_cooldown_added_during_a_flood_keeps_its_first() {
        let app = App::new();
        let mut pipeline = app.pipeline(&raid_alert("{raider} raided"));
        app.events(&mut pipeline, vec![raid(3), raid(5), raid(7)]);
        assert_eq!(app.waiting().len(), 3);

        let cooldown = raid_alert_through(NodeKind::Cooldown { seconds: 60.0, per_viewer: false });
        let (_, note) = app.change(pipeline, &cooldown);
        assert_eq!(note, Prepared { withdrawn: 2, ..Prepared::default() });
        assert_eq!(app.waiting().len(), 1);
    }

    #[test]
    fn a_chain_held_where_it_wasnt_before_stays_as_it_was() {
        let app = App::new();
        let mut pipeline = app.pipeline(&raid_alert("{raider} raided"));
        app.events(&mut pipeline, vec![raid(3)]);
        let before = app.waiting();

        let (_, note) = app.change(pipeline, &raid_alert_through(NodeKind::Delay { seconds: 5.0 }));
        assert!(!note.changed());
        assert_eq!(app.waiting(), before);
    }

    /// Every node kind, so one added later doesn't compile here until
    /// `NodeKind::catalog` (which [`every_kind`] wires up) has it.
    fn kind_name(kind: &NodeKind) -> &'static str {
        match kind {
            NodeKind::EmotesInChat => "emotes in chat",
            NodeKind::ChatMessage => "chat message",
            NodeKind::FirstChatter => "first chatter",
            NodeKind::ChatWords { .. } => "chat words",
            NodeKind::ChatCommand { .. } => "chat command",
            NodeKind::EmoteCombo { .. } => "emote combo",
            NodeKind::Chant { .. } => "chant",
            NodeKind::Bits { .. } => "bits",
            NodeKind::Sub { .. } => "sub",
            NodeKind::GiftBomb => "gift bomb",
            NodeKind::Follow => "follow",
            NodeKind::HypeTrain { .. } => "hype train",
            NodeKind::Raid { .. } => "raid",
            NodeKind::Reward { .. } => "reward",
            NodeKind::Custom { .. } => "custom",
            NodeKind::Chance { .. } => "chance",
            NodeKind::Cooldown { .. } => "cooldown",
            NodeKind::Gate { .. } => "gate",
            NodeKind::ViewerRoles { .. } => "viewer roles",
            NodeKind::HasText { .. } => "has text",
            NodeKind::CompareText { .. } => "compare text",
            NodeKind::CompareNumber { .. } => "compare number",
            NodeKind::Delay { .. } => "delay",
            NodeKind::EveryNth { .. } => "every nth",
            NodeKind::Timer { .. } => "timer",
            NodeKind::Rarity { .. } => "rarity",
            NodeKind::Number { .. } => "number",
            NodeKind::Math { .. } => "math",
            NodeKind::Clamp { .. } => "clamp",
            NodeKind::Random { .. } => "random",
            NodeKind::Text { .. } => "text",
            NodeKind::EmotesInText => "emotes in text",
            // (The premade cards' effects.)
            NodeKind::Plugin { .. } => "plugin",
        }
    }

    /// A card for each kind of node, wired so it queues alerts: an event
    /// node into an alert; a flow node between a chat message and an alert
    /// on each of its ways out; a value node's first output into an
    /// alert's message on chat messages.
    fn every_kind() -> Vec<Card> {
        let catalog = NodeKind::catalog();
        let names: std::collections::BTreeSet<&str> = catalog.iter().map(kind_name).collect();
        assert_eq!(names.len(), 32, "every kind but plugins' nodes");
        let mut cards = Vec::new();
        for (index, kind) in catalog.into_iter().enumerate() {
            let mut nodes = vec![node(1, kind.clone())];
            let mut wires = Vec::new();
            match kind.category() {
                Category::Events => {
                    nodes.push(node(2, alert_node("{summary}")));
                    wires.push(wire(1, "event", 2, "trigger"));
                }
                Category::Flow => {
                    nodes.push(node(2, NodeKind::ChatMessage));
                    // (A Gate's Open before the event; an Every Nth's Reset
                    // before it counts.)
                    let inputs = kind.inputs();
                    for input in inputs.iter().rev().filter(|input| input.ty == SocketType::Event) {
                        wires.push(wire(2, "event", 1, &input.name));
                    }
                    let outputs = kind.outputs();
                    let ways_out = outputs.iter().filter(|output| output.ty == SocketType::Event);
                    for (n, output) in ways_out.enumerate() {
                        let alert = 3 + n as u32;
                        nodes.push(node(alert, alert_node("{summary}")));
                        wires.push(wire(1, &output.name, alert, "trigger"));
                    }
                }
                Category::Values | Category::Effects => {
                    nodes.push(node(2, NodeKind::ChatMessage));
                    nodes.push(node(3, alert_node("")));
                    wires.push(wire(2, "event", 3, "trigger"));
                    for input in kind.inputs().iter().filter(|input| input.ty == SocketType::Text) {
                        wires.push(wire(2, "message", 1, &input.name));
                    }
                    let outputs = kind.outputs();
                    let shown = outputs
                        .iter()
                        .find(|output| matches!(output.ty, SocketType::Number | SocketType::Text));
                    if let Some(output) = shown {
                        wires.push(wire(1, &output.name, 3, "message"));
                    }
                }
            }
            cards.push(Card {
                id: format!("kind-{index}"),
                name: kind.title().into(),
                enabled: true,
                nodes,
                wires,
            });
        }
        cards
    }

    #[test]
    fn prepared_again_with_nothing_changed_nothing_changes() {
        let app = App::new();
        let mut config = AppConfig { cards: protocol::cards::premade(), ..AppConfig::default() };
        for card in &mut config.cards {
            card.enabled = true;
        }
        config.cards.extend(every_kind());
        let mut pipeline = app.pipeline(&config);
        for card in &config.cards {
            let mut card = card.clone();
            card.attach_plugins(&app.catalog);
            assert_eq!(card.problems(), [], "{}", card.name);
        }
        let chat = |text: &str, viewer: &str, viewers, first| TestEvent::Chat {
            text: text.into(),
            viewer: viewer.into(),
            viewers,
            first,
        };
        app.events(
            &mut pipeline,
            vec![
                chat("hype hype", "", 5, false),
                chat("hello", "New", 1, true),
                chat("!tts read this", "", 1, false),
                TestEvent::Bits { amount: 500, message: "Cheer500 hype".into() },
                TestEvent::Sub { months: 3, message: "three months".into() },
                TestEvent::GiftedSub,
                TestEvent::GiftBomb { count: 5 },
                raid(7),
                TestEvent::Reward { reward: "Hydrate".into(), text: "water".into() },
                TestEvent::Custom { name: "x".into() },
                TestEvent::Follow,
                TestEvent::HypeTrain { change: TestHypeTrain::Whole, level: 1 },
                chat("hype again", "", 2, false),
            ],
        );
        let before: Vec<Play> = app.activity.waiting_plays();
        assert!(before.len() > 40, "{} queued", before.len());

        let (_, note) = app.change(pipeline, &config);
        assert_eq!(note, Prepared::default());
        assert_eq!(app.activity.waiting_plays(), before);
    }
}
