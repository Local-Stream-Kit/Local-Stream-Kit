//! Finds the emotes in a chat message, including third-party emotes and
//! zero-width stacks.

use crate::model::{
    EmoteRef, EmoteSpan, Input, InputKind, Message, Origin, PostedEmote, Signal, SignalKind, SignalName,
};
use crate::pipeline::{Ctx, Transformer};
use crate::services::emotes::{EmoteInfo, EmoteRegistry};

pub struct Emotes;

impl Transformer for Emotes {
    fn produces(&self) -> &[SignalName] {
        &[SignalName::EmotesPosted]
    }

    fn on_input(&mut self, input: &Input, ctx: &mut Ctx<'_>, out: &mut Vec<Signal>) {
        let InputKind::Message(message) = &input.kind else { return };
        let emotes = find_emotes(message, &input.origin, ctx.emotes);
        if !emotes.is_empty() {
            out.push(Signal::new(
                input.origin.clone(),
                SignalKind::EmotesPosted { user: message.user.clone(), emotes },
            ));
        }
    }
}

enum Token {
    Emote { emote: EmoteRef, zero_width: bool },
    Text,
}

/// The emotes in a message, in order:
///
/// - the platform's own emotes from `message.emotes`;
/// - third-party emotes: whitespace-separated tokens that exactly match a code
///   the channel can use (no splitting on punctuation: `:)` and `D:` are codes);
/// - a zero-width emote directly after an emote (only whitespace between) is
///   stacked on top of it; one with no emote right before it stands alone.
pub fn find_emotes(message: &Message, origin: &Origin, registry: &EmoteRegistry) -> Vec<PostedEmote> {
    find(&message.text, &message.emotes, |word| {
        registry.lookup_code(&origin.platform, &origin.channel.id, word)
    })
}

/// The emotes in a text that didn't come with the platform's own emote
/// positions (a cheer's message, a reward's text): every whitespace-separated
/// token that's a code the channel can use, the platform's own emotes
/// included (as listed by its API, or seen in chat). Zero-width emotes stack
/// as in chat.
pub fn find_emotes_in_text(text: &str, origin: &Origin, registry: &EmoteRegistry) -> Vec<PostedEmote> {
    find(text, &[], |word| registry.lookup_any_code(&origin.platform, &origin.channel.id, word))
}

fn find(text: &str, spans: &[EmoteSpan], lookup: impl Fn(&str) -> Option<EmoteInfo>) -> Vec<PostedEmote> {
    let mut tokens = Vec::new();
    let mut position = 0; // character index
    for word in text.split(char::is_whitespace) {
        let start = position;
        let end = start + word.chars().count();
        position = end + 1;
        if word.is_empty() {
            continue;
        }
        let native = spans.iter().find(|span| span.start == start && span.end == end);
        let token = if let Some(span) = native {
            Token::Emote { emote: span.emote.clone(), zero_width: false }
        } else if spans.iter().any(|span| span.start < end && start < span.end) {
            // A platform emote that doesn't line up with whole words; skip the
            // word rather than guess.
            Token::Text
        } else if let Some(info) = lookup(word) {
            Token::Emote { emote: info.emote, zero_width: info.zero_width }
        } else {
            Token::Text
        };
        tokens.push(token);
    }

    let mut posted: Vec<PostedEmote> = Vec::new();
    let mut previous_was_emote = false;
    for token in tokens {
        match token {
            Token::Emote { emote, zero_width: true } if previous_was_emote => {
                posted.last_mut().expect("previous token was an emote").overlays.push(emote);
            }
            Token::Emote { emote, .. } => {
                posted.push(PostedEmote { emote, overlays: Vec::new() });
                previous_was_emote = true;
            }
            Token::Text => previous_was_emote = false,
        }
    }
    posted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::testing::{origin, user};
    use crate::services::emotes::EmoteInfo;

    fn third_party(code: &str, zero_width: bool) -> EmoteInfo {
        EmoteInfo {
            emote: EmoteRef { source: "7tv".into(), id: format!("7tv-{code}"), code: code.into() },
            image_url: format!("https://example.com/{code}"),
            zero_width,
        }
    }

    fn registry() -> EmoteRegistry {
        let registry = EmoteRegistry::for_tests();
        registry.set_channel_emotes(
            "twitch",
            &origin().channel.id,
            "7tv",
            vec![
                third_party("PogU", false),
                third_party("RainTime", true),
                third_party("SteerR", true),
                third_party(":)", false),
            ],
        );
        registry
    }

    #[test]
    fn emotes_in_a_text_without_positions() {
        let registry = registry();
        registry.set_platform_global(
            "twitch",
            vec![EmoteInfo {
                emote: EmoteRef { source: "twitch".into(), id: "25".into(), code: "Kappa".into() },
                image_url: "x".into(),
                zero_width: false,
            }],
        );
        let found = find_emotes_in_text("Cheer100 Kappa PogU RainTime hi", &origin(), &registry);
        let codes: Vec<(&str, usize)> =
            found.iter().map(|posted| (posted.emote.code.as_str(), posted.overlays.len())).collect();
        assert_eq!(codes, [("Kappa", 0), ("PogU", 1)], "Twitch's own by code; zero-width stacks as in chat");
    }

    fn message(text: &str, spans: &[(usize, usize, &str)]) -> Message {
        Message {
            user: user("a"),
            text: text.into(),
            emotes: spans
                .iter()
                .map(|&(start, end, id)| EmoteSpan {
                    start,
                    end,
                    emote: EmoteRef {
                        source: "twitch".into(),
                        id: id.into(),
                        code: text.chars().skip(start).take(end - start).collect(),
                    },
                })
                .collect(),
            ..Default::default()
        }
    }

    fn codes(posted: &[PostedEmote]) -> Vec<String> {
        posted
            .iter()
            .map(|p| {
                std::iter::once(p.emote.code.as_str())
                    .chain(p.overlays.iter().map(|o| o.code.as_str()))
                    .collect::<Vec<_>>()
                    .join("+")
            })
            .collect()
    }

    #[test]
    fn native_and_third_party_in_order() {
        let found = find_emotes(
            &message("Kappa hi PogU :) Kappa", &[(0, 5, "25"), (17, 22, "25")]),
            &origin(),
            &registry(),
        );
        assert_eq!(codes(&found), ["Kappa", "PogU", ":)", "Kappa"]);
        assert_eq!(found[0].emote.source, "twitch");
        assert_eq!(found[1].emote.source, "7tv");
    }

    #[test]
    fn zero_width_stacks_on_the_previous_emote() {
        let found = find_emotes(&message("PogU RainTime SteerR", &[]), &origin(), &registry());
        assert_eq!(codes(&found), ["PogU+RainTime+SteerR"]);
        // Also on a platform emote.
        let found = find_emotes(&message("Kappa RainTime", &[(0, 5, "25")]), &origin(), &registry());
        assert_eq!(codes(&found), ["Kappa+RainTime"]);
    }

    #[test]
    fn zero_width_after_text_or_first_stands_alone() {
        let found = find_emotes(&message("RainTime PogU hello SteerR", &[]), &origin(), &registry());
        assert_eq!(codes(&found), ["RainTime", "PogU", "SteerR"]);
    }

    #[test]
    fn extra_whitespace_and_unicode_positions() {
        // "é" is one character but two bytes; positions are characters.
        let found = find_emotes(&message("é  Kappa", &[(3, 8, "25")]), &origin(), &registry());
        assert_eq!(codes(&found), ["Kappa"]);
    }

    #[test]
    fn codes_are_case_sensitive_and_whole_words() {
        let found = find_emotes(&message("pogu PogU! PogU", &[]), &origin(), &registry());
        assert_eq!(codes(&found), ["PogU"]);
    }
}
