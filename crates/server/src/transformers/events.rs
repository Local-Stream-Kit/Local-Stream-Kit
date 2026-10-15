use std::sync::atomic::{AtomicBool, Ordering};

use crate::emote_text::EmoteText;
use serde_json::Value;

use crate::model::{Input, InputKind, Message, PlatformEvent, PluginValue, Signal, SignalKind, SignalName};
use crate::pipeline::{Ctx, Transformer};
use crate::transformers::emotes::find_emotes_in_text;

/// Logged once: cheer words can't be told apart without the login.
static CHEERS_UNKNOWN_LOGGED: AtomicBool = AtomicBool::new(false);

/// Turns platform events into signals 1:1, and every chat message into a
/// `ChatMessage` signal, so cards only ever deal with signals. Messages get
/// their emotes marked here (see `EmoteText`): the platform's own by their
/// positions, the channel's third-party ones by code; a reward's text, which
/// comes without positions, by code only.
pub struct Events;

impl Transformer for Events {
    fn produces(&self) -> &[SignalName] {
        &[
            SignalName::ChatMessage,
            SignalName::Support,
            SignalName::Membership,
            SignalName::GiftBatch,
            SignalName::Raid,
            SignalName::RewardRedeemed,
            SignalName::Follow,
            SignalName::HypeTrain,
            SignalName::Custom,
            SignalName::Plugin,
        ]
    }

    fn on_input(&mut self, input: &Input, ctx: &mut Ctx<'_>, out: &mut Vec<Signal>) {
        let (platform, channel) = (&input.origin.platform, &input.origin.channel.id);
        let marked = |message: &Message| {
            EmoteText::from_message(&message.text, &message.emotes, |code| {
                ctx.emotes.lookup_code(platform, channel, code)
            })
        };
        // A cheer's message without its cheer words. Their prefixes come from
        // Twitch's API, which needs the login; without it they stay.
        let cheer = |message: &Message| {
            if !ctx.emotes.knows_cheers(platform, channel) {
                if !CHEERS_UNKNOWN_LOGGED.swap(true, Ordering::Relaxed) {
                    tracing::info!(
                        "cheer words (Cheer100) stay in cheer messages until the channel's cheer prefixes \
                         load, which needs the Twitch login"
                    );
                }
                return marked(message);
            }
            EmoteText::from_cheer(
                &message.text,
                &message.emotes,
                |code| ctx.emotes.lookup_code(platform, channel, code),
                |word| ctx.emotes.is_cheer(platform, channel, word),
            )
        };
        let words = |text: String| {
            EmoteText::from_message(&text, &[], |code| ctx.emotes.lookup_any_code(platform, channel, code))
        };
        let event = match &input.kind {
            InputKind::Event(event) => event,
            InputKind::Message(message) => {
                let kind = SignalKind::ChatMessage {
                    user: message.user.clone(),
                    text: marked(message),
                    first: message.marks.first,
                };
                out.push(Signal::new(input.origin.clone(), kind));
                return;
            }
            InputKind::ChannelJoined | InputKind::Moderation(_) => return,
        };
        let kind = match event.clone() {
            PlatformEvent::Support { user, amount, message } => {
                let bits = amount.unit() == "bits";
                let message =
                    message.as_ref().map(|message| if bits { cheer(message) } else { marked(message) });
                SignalKind::Support { user, amount, message }
            }
            PlatformEvent::Membership { user, kind, tier, message } => {
                SignalKind::Membership { user, kind, tier, message: message.as_ref().map(marked) }
            }
            PlatformEvent::GiftBatch { gifter, count, tier, .. } => {
                SignalKind::GiftBatch { gifter, count, tier }
            }
            PlatformEvent::Raid { from, viewers } => SignalKind::Raid { from, viewers },
            PlatformEvent::Redemption { user, reward, input } => {
                SignalKind::RewardRedeemed { user, reward, input: input.map(words) }
            }
            PlatformEvent::Follow { user } => SignalKind::Follow { user },
            PlatformEvent::HypeTrain { change, train } => SignalKind::HypeTrain { change, train },
            PlatformEvent::Custom { name, data } => SignalKind::Custom { name, data },
            PlatformEvent::Plugin { plugin, node, values, output, to } => {
                let values = values
                    .into_iter()
                    .filter_map(|(name, value)| {
                        let value = match value {
                            Value::Number(number) => PluginValue::Number(number.as_f64()?),
                            Value::String(text) => PluginValue::Text(words(text)),
                            Value::Bool(yes) => PluginValue::Text(EmoteText::plain_text(yes.to_string())),
                            Value::Array(codes) => {
                                let codes: Vec<&str> = codes.iter().filter_map(Value::as_str).collect();
                                PluginValue::Emotes(find_emotes_in_text(
                                    &codes.join(" "),
                                    &input.origin,
                                    ctx.emotes,
                                ))
                            }
                            Value::Null | Value::Object(_) => return None,
                        };
                        Some((name, value))
                    })
                    .collect();
                SignalKind::Plugin { plugin, node, values, output, to }
            }
        };
        out.push(Signal::new(input.origin.clone(), kind));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Amount, Input, Message};
    use crate::pipeline::testing::{FixedRandom, origin, user};
    use crate::services::emotes::EmoteRegistry;
    use crate::services::overlay_hub::OverlayHub;
    use crate::services::speech::Speech;

    /// The plain text of the message on the signal for a cheer.
    fn cheered(registry: &EmoteRegistry, text: &str) -> String {
        let message =
            Message { user: user("c"), text: text.into(), emotes: Vec::new(), ..Default::default() };
        let cheer = Input {
            at_ms: 0,
            origin: origin(),
            kind: InputKind::Event(PlatformEvent::Support {
                user: user("c"),
                amount: Amount::Units { value: 100, unit: "bits".into() },
                message: Some(message),
            }),
        };
        let (hub, speech, mut rng) = (OverlayHub::new(), Speech::off(), FixedRandom::new(&[0.5]));
        let activity = crate::services::activity::Activity::for_tests(hub.clone(), speech.clone());
        let mut ctx = Ctx::for_tests(&mut rng, registry, &hub, &speech, &activity);
        let mut out = Vec::new();
        Events.on_input(&cheer, &mut ctx, &mut out);
        match &out[..] {
            [Signal { kind: SignalKind::Support { message: Some(message), .. }, .. }] => message.plain(),
            other => panic!("one cheer: {other:?}"),
        }
    }

    #[test]
    fn cheers_lose_the_channels_cheer_words() {
        let registry = EmoteRegistry::for_tests();
        // Before the prefixes load (no login), the words stay.
        assert_eq!(cheered(&registry, "Cheer100 nice"), "Cheer100 nice");
        let channel = &origin().channel.id;
        registry.set_cheer_prefixes("twitch", channel, vec!["Cheer".into(), "ChannelHype".into()]);
        assert_eq!(cheered(&registry, "Cheer100 nice channelhype5 one"), "nice one");
        assert!(registry.is_cheer("twitch", channel, "CHEER1"));
        assert!(!registry.is_cheer("twitch", channel, "Cheer"));
        assert!(!registry.is_cheer("twitch", channel, "100"));
        assert!(!registry.is_cheer("twitch", "other", "Cheer100"));
    }
}
