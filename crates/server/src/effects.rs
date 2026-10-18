//! Effects: what cards' effect nodes do, turned into overlay commands: a
//! plugin's effect node's run (alerts, speech, sounds, the rain, the chat
//! display… are the built-in plugins), and the chat for pages that hear it.

use std::sync::atomic::{AtomicU64, Ordering};

use protocol::SpeechSettings;
use protocol::cards::Overrides;
use protocol::chat::{ChatBadge, ChatLine, ChatMarks, ChatReply, ChatRoles, ChatUser, LineSource, line_id};
use protocol::config;
use protocol::overrides::{SPEECH_FIELDS, apply};
use protocol::plugin::{NodeType, PluginMessage, RunViewer, SPEAK_PLUGIN, shared_settings};
use protocol::{Command, PluginCommand, ShellCommand};

use crate::emote_text::EmoteText;
use crate::model::{Input, InputKind, Message, Moderation, Origin, User};

/// Numbers chat lines whose message has no id from its platform.
static NEXT_LINE: AtomicU64 = AtomicU64::new(1);
use crate::pipeline::{Ctx, Instant, RandomSource};
use crate::services::activity::{Heard, Play, Spoken};
use crate::services::emotes::EmoteRegistry;
use crate::services::speech::Job;
use crate::services::speech::text::prepare;

pub struct Effects {
    config: config::Effects,
    /// Plugins switched off on the Plugins page: their nodes do nothing.
    plugins_off: std::collections::BTreeSet<String>,
    /// The plugins' stored settings and presets, by id.
    plugins: std::collections::BTreeMap<String, config::PluginConfig>,
    /// The plugins whose pages hear the chat (on, and allowed to).
    chat_listeners: Vec<String>,
    /// The plugins whose server code or program hears the chat (on, and
    /// allowed to).
    chat_hearers: Vec<String>,
    /// The plugins whose server code or program hears events (on, and
    /// allowed to).
    event_hearers: Vec<String>,
    /// The plugins with server code: it plays their queued runs.
    servers: std::collections::BTreeSet<String>,
    /// What texts typed into nodes can mark parts with.
    text_styles: Vec<protocol::TextStyle>,
    /// Their switch: off, marks give no look.
    text_styles_on: bool,
}

/// The text styles nodes' texts mark parts with, and their switch.
#[derive(Clone, Copy)]
pub struct TextStyles<'s> {
    list: &'s [protocol::TextStyle],
    on: bool,
}

impl TextStyles<'_> {
    /// What a mark `[name]` gives: `None` for a name that isn't a style
    /// (the bracket is text); `Some(None)` for one while styles are off
    /// (the mark is taken out, with no look).
    pub fn mark(&self, name: &str) -> Option<Option<protocol::TextStyle>> {
        protocol::text_style::find(self.list, name).map(|style| self.on.then(|| style.for_parts()))
    }
}

impl Effects {
    pub fn new(config: &config::Effects) -> Effects {
        Effects {
            config: config.clone(),
            plugins_off: Default::default(),
            plugins: Default::default(),
            chat_listeners: Vec::new(),
            chat_hearers: Vec::new(),
            event_hearers: Vec::new(),
            servers: Default::default(),
            text_styles: Vec::new(),
            text_styles_on: true,
        }
    }

    /// The text styles, from the config.
    pub fn with_text_styles(mut self, styles: &[protocol::TextStyle], on: bool) -> Effects {
        self.text_styles = styles.to_vec();
        self.text_styles_on = on;
        self
    }

    pub fn text_styles(&self) -> TextStyles<'_> {
        TextStyles { list: &self.text_styles, on: self.text_styles_on }
    }

    /// The plugins' switches and settings, from the config.
    pub fn with_plugins(
        mut self,
        plugins: &std::collections::BTreeMap<String, config::PluginConfig>,
    ) -> Effects {
        self.plugins_off =
            plugins.iter().filter(|(_, plugin)| !plugin.enabled).map(|(id, _)| id.clone()).collect();
        self.plugins = plugins.clone();
        self
    }

    /// From the plugins in `catalog`: those whose pages and those whose
    /// server code or program hear the chat or events (on, with a page,
    /// server code or a program, and allowed to hear it), and those with
    /// server code.
    pub fn with_catalog(mut self, catalog: &protocol::plugin::Catalog) -> Effects {
        self.servers = catalog
            .plugins()
            .iter()
            .filter(|info| info.server)
            .map(|info| info.manifest.id.clone())
            .collect();
        let hearing = |what: &str, page: bool| -> Vec<String> {
            catalog
                .plugins()
                .iter()
                .filter(|info| {
                    (if page { info.overlay } else { info.server || info.program })
                        && !self.plugin_off(&info.manifest.id)
                })
                .filter(|info| {
                    let allowed = self.plugins.get(&info.manifest.id).map(|stored| stored.hears.as_slice());
                    protocol::plugin::allowed_hears(info, allowed.unwrap_or_default())
                        .iter()
                        .any(|each| each == what)
                })
                .map(|info| info.manifest.id.clone())
                .collect()
        };
        let chat_listeners = hearing(protocol::plugin::HEARS_CHAT, true);
        let chat_hearers = hearing(protocol::plugin::HEARS_CHAT, false);
        let event_hearers = hearing(protocol::plugin::HEARS_EVENTS, false);
        Effects { chat_listeners, chat_hearers, event_hearers, ..self }
    }

    /// An event, for the server code (`on-event`) or program that hears
    /// events, as the built-in event node it's for has it. Plugins' own events and
    /// chat messages (heard as chat) aren't.
    pub fn hear_event(
        &self,
        signal: &crate::model::Signal,
        origin: protocol::activity::ItemOrigin,
        ctx: &Ctx<'_>,
    ) {
        if self.event_hearers.is_empty() {
            return;
        }
        let Some((kind, values)) = crate::cards::heard_event(signal) else { return };
        let summary = values.get("summary").map(|summary| summary.plain()).unwrap_or_default();
        let viewer = run_viewer(signal, ctx.emotes);
        let values: serde_json::Map<String, serde_json::Value> =
            values.iter().map(|(name, value)| (name.clone(), value.json())).collect();
        for plugin in &self.event_hearers {
            let heard = Heard::Event {
                kind: kind.to_string(),
                summary: summary.clone(),
                viewer: viewer.clone(),
                values: values.clone(),
                origin,
            };
            ctx.out.hear(plugin, heard);
        }
    }

    /// Whether a plugin is switched off.
    pub fn plugin_off(&self, plugin: &str) -> bool {
        self.plugins_off.contains(plugin)
    }

    /// What every overlay gets on connect: the master volume and mute.
    pub fn module_configs(&self) -> Vec<Command> {
        let sound = &self.config.sound;
        vec![Command::Shell(ShellCommand::Volume {
            volume: sound.volume.clamp(0.0, 1.0),
            muted: sound.muted,
        })]
    }

    /// A chat message, or a moderator taking some off, for every page that
    /// hears the chat (the chat display's): every message, commands and
    /// bots too, with what a reply answers.
    pub fn chat(&self, input: &Input, ctx: &mut Ctx<'_>) {
        if self.chat_listeners.is_empty() && self.chat_hearers.is_empty() {
            return;
        }
        let platform = &input.origin.platform;
        let message = match &input.kind {
            InputKind::Message(message) => {
                let channel = &input.origin.channel.id;
                let text = EmoteText::from_message(&message.text, &message.emotes, |code| {
                    ctx.emotes.lookup_code(platform, channel, code)
                });
                let id = match message.id.as_str() {
                    "" => format!("local:{}", NEXT_LINE.fetch_add(1, Ordering::Relaxed)),
                    id => line_id(platform, id),
                };
                PluginMessage::Chat {
                    line: ChatLine {
                        id,
                        platform: platform.clone(),
                        at_ms: input.at_ms,
                        source: LineSource::Chat,
                        user: Some(chat_user(&message.user, &input.origin, ctx.emotes)),
                        parts: text.parts(),
                        marks: chat_marks(message),
                    },
                }
            }
            InputKind::Moderation(Moderation::MessageDeleted { id }) => {
                PluginMessage::ChatRemoved { id: line_id(platform, id) }
            }
            InputKind::Moderation(Moderation::UserCleared { user_id, .. }) => {
                PluginMessage::ChatUserRemoved { platform: platform.clone(), user_id: user_id.clone() }
            }
            InputKind::Moderation(Moderation::ChatCleared) => {
                PluginMessage::ChatCleared { platform: platform.clone() }
            }
            InputKind::ChannelJoined | InputKind::Event(_) => return,
        };
        for plugin in &self.chat_listeners {
            ctx.out.send(Command::Plugin(PluginCommand { plugin: plugin.clone(), message: message.clone() }));
        }
        for plugin in &self.chat_hearers {
            ctx.out.hear(plugin, Heard::Chat(message.clone()));
        }
    }

    /// A plugin's effect node fired: its page on the overlays (or its server
    /// code) gets the node's values, right away or when its turn in the
    /// queue comes. An
    /// input it reads aloud (`voice`: the node's changes to the speech
    /// settings) gets its spoken message first. One that waits in the queue
    /// is given back, to queue (keeping `then`, for its Started and Ended
    /// outputs). Files picked from a folder come from the node's draws
    /// (`rng`).
    #[allow(clippy::too_many_arguments)]
    pub fn plugin(
        &self,
        plugin: &str,
        ty: &NodeType,
        input: &str,
        values: serde_json::Map<String, serde_json::Value>,
        changes: serde_json::Map<String, serde_json::Value>,
        voice: &Overrides,
        then: Option<crate::cards::Then>,
        rng: &mut dyn RandomSource,
        ctx: &mut Ctx<'_>,
    ) -> Option<Play> {
        if self.plugin_off(plugin) {
            return None;
        }
        // With presets: the node's preset's settings, with its changes.
        let settings = if ty.preset_settings.is_empty() {
            serde_json::Map::new()
        } else {
            let id =
                ty.preset_setting().and_then(|setting| values.get(&setting.key)).and_then(|id| id.as_str());
            let stored = self.plugins.get(plugin).cloned().unwrap_or_default();
            let mut settings = shared_settings(
                &ty.preset_settings,
                &stored.preset(id.unwrap_or_default()).settings,
                &mut |kind, folder, every| ctx.media.pick_files(kind, folder, every, rng),
            );
            settings.extend(changes.clone());
            settings
        };
        let spoken = ty.spoken_input().and_then(|spoken| {
            let text = values.get(&spoken.name)?.get("text")?.as_str()?;
            if self.plugin_off(SPEAK_PLUGIN) {
                return None;
            }
            let job = self.speech_job(text, voice);
            (!job.text.trim().is_empty() && ctx.out.can_speak(&job.voice))
                .then(|| Spoken { input: spoken.name.clone(), job })
        });
        if ty.queued(&values) {
            let play = Play {
                plugin: plugin.to_string(),
                node: ty.id.clone(),
                title: ty.title.clone(),
                text: ty.description(&values),
                parts: ty.description_parts(&values),
                input: input.to_string(),
                values,
                changes,
                settings,
                speech: spoken,
                then: then.map(Box::new),
                server: self.servers.contains(plugin),
            };
            return Some(play);
        }
        // One of its own outputs wired: an id its server code fires them by.
        let run_id = then.map(|then| ctx.out.remember_run(plugin, then, &ctx.cause));
        ctx.out.play_now(Instant {
            plugin: plugin.to_string(),
            node: ty.id.clone(),
            input: input.to_string(),
            values,
            changes,
            settings,
            run_id,
            spoken,
        });
        None
    }

    fn speech_job(&self, text: &str, overrides: &Overrides) -> Job {
        let settings = apply(&self.config.speech.settings, overrides, SPEECH_FIELDS);
        speech_job(text, &settings)
    }
}

/// A message to read: the text after the settings' rules, at the speech
/// volume (the overlay applies the master volume).
pub fn speech_job(text: &str, settings: &SpeechSettings) -> Job {
    let volume = if settings.volume.is_finite() { settings.volume.clamp(0.0, 1.0) } else { 0.0 };
    Job { text: prepare(text, settings), voice: settings.voice.clone(), speed: settings.speed, volume }
}

/// Who set `signal` off, as plugins get them (an effect node's `viewer`, a
/// flow node's, an event heard); `None` for an event without one.
pub fn run_viewer(signal: &crate::model::Signal, emotes: &EmoteRegistry) -> Option<RunViewer> {
    let origin = &signal.origin;
    signal
        .user()
        .map(|user| RunViewer { platform: origin.platform.clone(), user: chat_user(user, origin, emotes) })
}

/// A viewer as the chat display shows them, with their badges' titles and
/// images once the platform's badges have loaded.
pub fn chat_user(user: &User, origin: &Origin, emotes: &EmoteRegistry) -> ChatUser {
    ChatUser {
        id: user.id.clone(),
        name: user.name.clone(),
        display_name: user.display_name.clone(),
        color: user.color.clone(),
        badges: user
            .badges
            .iter()
            .map(|badge| {
                match emotes.badge(&origin.platform, &origin.channel.id, &badge.set, &badge.version) {
                    Some(known) => ChatBadge {
                        set: badge.set.clone(),
                        title: known.title,
                        image: Some(known.image.path()),
                    },
                    None => ChatBadge { set: badge.set.clone(), title: badge.set.clone(), image: None },
                }
            })
            .collect(),
        roles: ChatRoles {
            broadcaster: user.roles.broadcaster,
            moderator: user.roles.moderator,
            vip: user.roles.vip,
            member: user.roles.member,
        },
    }
}

/// A message's marks, with what a reply answers.
fn chat_marks(message: &Message) -> ChatMarks {
    let marks = &message.marks;
    ChatMarks {
        action: marks.action,
        highlighted: marks.highlighted,
        first: marks.first,
        reward: marks.reward,
        cheer: marks.cheer,
        reply_to: marks
            .reply_to
            .as_ref()
            .map(|reply| ChatReply { user: reply.user.clone(), text: reply.text.clone() }),
        reply: marks.reply_to.is_some(),
    }
}

#[cfg(test)]
pub mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::model::{EmoteRef, PostedEmote};
    use crate::pipeline::testing::{FixedRandom, origin};
    use crate::services::activity::Activity;
    use crate::services::overlay_hub::OverlayHub;
    use crate::services::speech::Speech;

    #[test]
    fn text_styles_switched_off_take_marks_out() {
        let styles = protocol::TextStyle::starters();
        let on = Effects::new(&config::Effects::default()).with_text_styles(&styles, true);
        assert_eq!(
            on.text_styles().mark("highlight").flatten().map(|style| style.name),
            Some("highlight".into())
        );
        let off = Effects::new(&config::Effects::default()).with_text_styles(&styles, false);
        assert_eq!(off.text_styles().mark("highlight"), Some(None), "a mark, with no look");
        assert_eq!(off.text_styles().mark("VIP"), None, "not a style: just text");
    }

    pub fn emote(code: &str) -> EmoteRef {
        EmoteRef { source: "7tv".into(), id: format!("id-{code}"), code: code.into() }
    }

    pub fn posted(code: &str, overlays: &[&str]) -> PostedEmote {
        PostedEmote { emote: emote(code), overlays: overlays.iter().map(|c| emote(c)).collect() }
    }

    /// Every chat message the pages that hear the chat get, by plugin.
    fn heard(
        sent: &mut tokio::sync::broadcast::Receiver<Arc<protocol::Envelope>>,
    ) -> Vec<(String, PluginMessage)> {
        let mut got = Vec::new();
        while let Ok(envelope) = sent.try_recv() {
            let Command::Plugin(command) = &envelope.command else { continue };
            got.push((command.plugin.clone(), command.message.clone()));
        }
        got
    }

    fn listener(id: &str) -> protocol::plugin::PluginInfo {
        protocol::plugin::PluginInfo {
            manifest: protocol::plugin::Manifest {
                id: id.into(),
                hears: vec!["chat".into()],
                ..Default::default()
            },
            overlay: true,
            ..Default::default()
        }
    }

    #[test]
    fn chat_lines_have_the_viewer_and_the_emotes() {
        use crate::model::{Badge, EmoteSpan, MessageMarks, ReplyTo};
        use crate::pipeline::testing::user;
        use protocol::chat::Content;

        let catalog = protocol::plugin::Catalog::new(vec![protocol::plugin::PluginInfo {
            bundled: true,
            ..listener("chat")
        }]);
        let effects = Effects::new(&config::Effects::default()).with_catalog(&catalog);
        let registry = EmoteRegistry::for_tests();
        let hub = OverlayHub::new();
        let mut sent = hub.subscribe();
        let mut rng = FixedRandom::new(&[0.0]);
        let speech = Speech::off();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
        let mut viewer = user("viewer");
        viewer.color = Some("#ff0000".into());
        viewer.badges = vec![Badge { set: "vip".into(), version: "1".into() }];
        let message = Message {
            id: "m1".into(),
            user: viewer,
            text: "Kappa".into(),
            emotes: vec![EmoteSpan { start: 0, end: 5, emote: emote("Kappa") }],
            marks: MessageMarks {
                first: true,
                reply_to: Some(ReplyTo { user: "Bob".into(), text: "hi".into() }),
                ..MessageMarks::default()
            },
        };
        effects.chat(&Input { at_ms: 5, origin: origin(), kind: InputKind::Message(message) }, &mut ctx);
        let got = heard(&mut sent);
        let [(plugin, PluginMessage::Chat { line })] = &got[..] else { panic!("{got:?}") };
        assert_eq!(plugin, "chat", "a built-in plugin hears it from the start");
        assert_eq!((line.id.as_str(), line.content()), ("twitch:m1", Content::SingleEmote));
        assert!(
            line.marks.first && line.marks.reply && line.marks.reply_to.is_some(),
            "with what it answers"
        );
        let user = line.user.as_ref().unwrap();
        assert_eq!((user.color.as_deref(), user.badges[0].set.as_str()), (Some("#ff0000"), "vip"));
    }

    #[test]
    fn pages_that_hear_the_chat_get_every_message_once_allowed() {
        use crate::pipeline::testing::user;
        use protocol::plugin::Catalog;

        let catalog = Catalog::new(vec![listener("allowed"), listener("asking"), listener("off")]);
        let mut plugins = std::collections::BTreeMap::new();
        let allowed = config::PluginConfig { hears: vec!["chat".into()], ..Default::default() };
        plugins.insert("allowed".to_string(), allowed.clone());
        plugins.insert("off".to_string(), config::PluginConfig { enabled: false, ..allowed });
        let effects = Effects::new(&config::Effects::default()).with_plugins(&plugins).with_catalog(&catalog);

        let registry = EmoteRegistry::for_tests();
        let hub = OverlayHub::new();
        let mut sent = hub.subscribe();
        let mut rng = FixedRandom::new(&[0.0]);
        let speech = Speech::off();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
        let input = |kind| Input { at_ms: 5, origin: origin(), kind };
        let message = |id: &str, name: &str, text: &str| Message {
            id: id.into(),
            user: user(name),
            text: text.into(),
            emotes: Vec::new(),
            marks: Default::default(),
        };
        effects.chat(&input(InputKind::Message(message("m1", "nightbot", "!so someone"))), &mut ctx);
        effects.chat(&input(InputKind::Moderation(Moderation::MessageDeleted { id: "m1".into() })), &mut ctx);
        effects.chat(
            &input(InputKind::Moderation(Moderation::UserCleared {
                user_id: "id-a".into(),
                timeout_secs: Some(60),
            })),
            &mut ctx,
        );
        effects.chat(&input(InputKind::Moderation(Moderation::ChatCleared)), &mut ctx);
        let got = heard(&mut sent);
        let Some((_, PluginMessage::Chat { line })) = got.first() else { panic!("{got:?}") };
        assert_eq!(line.id, "twitch:m1", "commands and bots too: the page leaves out what it wants");
        assert_eq!(
            got.iter().map(|(plugin, _)| plugin.as_str()).collect::<Vec<_>>(),
            ["allowed"; 4],
            "not one that isn't allowed yet, or is off"
        );
        assert_eq!(got[1].1, PluginMessage::ChatRemoved { id: "twitch:m1".into() });
        assert_eq!(
            got[2].1,
            PluginMessage::ChatUserRemoved { platform: "twitch".into(), user_id: "id-a".into() }
        );
        assert_eq!(got[3].1, PluginMessage::ChatCleared { platform: "twitch".into() });
    }

    /// A plugin with server code (no page) that hears `hears`.
    fn hearer(id: &str, hears: &[&str]) -> protocol::plugin::PluginInfo {
        protocol::plugin::PluginInfo {
            manifest: protocol::plugin::Manifest {
                id: id.into(),
                hears: hears.iter().map(|what| what.to_string()).collect(),
                ..Default::default()
            },
            server: true,
            ..Default::default()
        }
    }

    /// Everything heard so far, by `next` (`Asks::next_heard`).
    fn all_heard(next: impl Fn() -> (Option<Heard>, bool)) -> Vec<Heard> {
        std::iter::from_fn(|| next().0).collect()
    }

    #[test]
    fn server_code_hears_the_chat_and_events_once_allowed() {
        use crate::model::{Amount, Signal, SignalKind};
        use crate::pipeline::testing::user;
        use protocol::activity::ItemOrigin;

        let catalog = protocol::plugin::Catalog::new(vec![
            hearer("both", &["chat", "events"]),
            hearer("asking", &["chat", "events"]),
            hearer("events-only", &["events"]),
        ]);
        let mut plugins = std::collections::BTreeMap::new();
        let allowed = |hears: &[&str]| config::PluginConfig {
            hears: hears.iter().map(|what| what.to_string()).collect(),
            ..Default::default()
        };
        plugins.insert("both".to_string(), allowed(&["chat", "events"]));
        plugins.insert("events-only".to_string(), allowed(&["events"]));
        let effects = Effects::new(&config::Effects::default()).with_plugins(&plugins).with_catalog(&catalog);

        let registry = EmoteRegistry::for_tests();
        let hub = OverlayHub::new();
        let mut sent = hub.subscribe();
        let mut rng = FixedRandom::new(&[0.0]);
        let speech = Speech::off();
        let activity = Activity::for_tests(hub.clone(), speech.clone());
        let (both, _) = activity.asks("both");
        let (asking, _) = activity.asks("asking");
        let (events_only, _) = activity.asks("events-only");
        let mut ctx = Ctx::for_tests(&mut rng, &registry, &hub, &speech, &activity);
        let message = Message {
            id: "m1".into(),
            user: user("ann"),
            text: "hello".into(),
            emotes: Vec::new(),
            marks: Default::default(),
        };
        effects.chat(&Input { at_ms: 5, origin: origin(), kind: InputKind::Message(message) }, &mut ctx);
        assert!(heard(&mut sent).is_empty(), "no page hears it");
        let cheer = Signal::new(
            origin(),
            SignalKind::Support {
                user: user("bo"),
                amount: Amount::Units { value: 100, unit: "bits".into() },
                message: None,
            },
        );
        effects.hear_event(&cheer, ItemOrigin::Test, &ctx);
        let chat = Signal::new(
            origin(),
            SignalKind::ChatMessage { user: user("ann"), text: "hello".into(), first: false },
        );
        effects.hear_event(&chat, ItemOrigin::Live, &ctx);

        let got = all_heard(|| both.next_heard());
        let [
            Heard::Chat(PluginMessage::Chat { line }),
            Heard::Event { kind, summary, viewer, values, origin },
        ] = &got[..]
        else {
            panic!("{got:?}")
        };
        assert_eq!(line.id, "twitch:m1");
        assert_eq!((kind.as_str(), summary.as_str()), ("bits", "BO cheered 100 bits"));
        assert_eq!(values["amount"], 100.0, "the Bits node's outputs");
        assert_eq!(viewer.as_ref().map(|viewer| viewer.user.name.as_str()), Some("bo"));
        assert_eq!(*origin, ItemOrigin::Test, "a test, marked so");
        assert!(all_heard(|| asking.next_heard()).is_empty(), "not allowed yet");
        assert!(
            matches!(&all_heard(|| events_only.next_heard())[..], [Heard::Event { .. }]),
            "events, not the chat"
        );
    }
}
