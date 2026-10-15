//! Text that may have emotes in it, as pieces: words, and emotes (with
//! zero-width emotes stacked on them). Cards pass it along their text wires,
//! so an alert can show a viewer's emotes as images while text to speech
//! reads the same message as words.
//!
//! Emotes are marked where text comes in, never guessed later: a chat, sub or
//! cheer message by the platform's own emote positions and the channel's
//! third-party codes, a reward's text by codes only, and a template by its
//! `{e:code}` placeholders. Anything else (names, titles, numbers) is plain
//! text, and plain text is never searched for markers, so a viewer typing
//! `{e:Kappa}` just shows those characters. Likewise only a template marks
//! parts with a text style (`[highlight]…[/]`).
//!
//! There's deliberately no `Display` or `Deref<Target = str>`: whatever uses
//! one picks [`EmoteText::plain`] (codes as words) or
//! [`EmoteText::alert_parts`] (images).

use protocol::{AlertPart, EmoteImage, EmoteSprite, TextStyle};
use serde::{Deserialize, Serialize};

use crate::model::{EmoteRef, EmoteSpan, PostedEmote};
use crate::services::emotes::EmoteInfo;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EmoteText {
    pieces: Vec<Piece>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Piece {
    Text {
        text: String,
        /// The text style a template marked it with.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<TextStyle>,
    },
    Emote {
        emote: PostedEmote,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<TextStyle>,
    },
}

impl EmoteText {
    /// Text without emotes, taken as it is.
    pub fn plain_text(text: impl Into<String>) -> EmoteText {
        let mut out = EmoteText::default();
        out.push_text(&text.into());
        out
    }

    /// A message with the platform's own emote positions (`spans`, in
    /// characters) and other emotes found by code (`lookup`), word by word
    /// as chat shows them: "Kappa!" is a word, not an emote.
    pub fn from_message(
        text: &str,
        spans: &[EmoteSpan],
        lookup: impl Fn(&str) -> Option<EmoteInfo>,
    ) -> EmoteText {
        EmoteText::build(text, spans, lookup, |_| false)
    }

    /// A cheer's message, as [`EmoteText::from_message`], without its cheer
    /// words ("Cheer100 nice" → "nice"): they're the bits, counted already,
    /// not something said. `is_cheer` knows the channel's prefixes.
    pub fn from_cheer(
        text: &str,
        spans: &[EmoteSpan],
        lookup: impl Fn(&str) -> Option<EmoteInfo>,
        is_cheer: impl Fn(&str) -> bool,
    ) -> EmoteText {
        EmoteText::build(text, spans, lookup, is_cheer)
    }

    /// Words that `leave_out` picks go, with the space after them (or, for
    /// the last word, the space before).
    fn build(
        text: &str,
        spans: &[EmoteSpan],
        lookup: impl Fn(&str) -> Option<EmoteInfo>,
        leave_out: impl Fn(&str) -> bool,
    ) -> EmoteText {
        let mut out = EmoteText::default();
        let mut word = String::new();
        let mut start = 0; // the word's first character
        let mut left_out = false; // the last word
        let length = text.chars().count();
        for (index, c) in text.chars().chain(std::iter::once(' ')).enumerate() {
            if !c.is_whitespace() {
                if word.is_empty() {
                    start = index;
                }
                word.push(c);
                continue;
            }
            let after_word = !word.is_empty();
            if after_word {
                let end = start + word.chars().count();
                left_out = leave_out(&word);
                if left_out {
                    // Not even its space.
                } else if let Some(span) = spans.iter().find(|span| span.start == start && span.end == end) {
                    out.push_emote(span.emote.clone(), false, None);
                } else if spans.iter().any(|span| span.start < end && start < span.end) {
                    // A platform emote that doesn't line up with whole words.
                    out.push_text(&word);
                } else if let Some(info) = lookup(&word) {
                    out.push_emote(info.emote, info.zero_width, None);
                } else {
                    out.push_text(&word);
                }
                word.clear();
            }
            if index < length && !(after_word && left_out) {
                out.push_text(&c.to_string());
            }
        }
        if left_out && let Some(Piece::Text { text, .. }) = out.pieces.last_mut() {
            text.truncate(text.trim_end().len());
            if text.is_empty() {
                out.pieces.pop();
            }
        }
        out
    }

    /// An emote as it was posted, with what's stacked on it.
    pub fn push_posted(&mut self, emote: PostedEmote) {
        self.pieces.push(Piece::Emote { emote, style: None });
    }

    /// Emotes, one after another with a space between.
    pub fn from_emotes(emotes: &[PostedEmote]) -> EmoteText {
        let mut out = EmoteText::default();
        for (index, posted) in emotes.iter().enumerate() {
            if index > 0 {
                out.push_text(" ");
            }
            out.pieces.push(Piece::Emote { emote: posted.clone(), style: None });
        }
        out
    }

    /// Fills a template typed by the user: `{name}` from `value` (left as
    /// it is when there's none), and `{e:code}` with that emote (`emote`
    /// finds it; an unknown code is written out). `[name]`, a text style
    /// `style` knows, gives what follows that style until `[/]` (or the
    /// next style); other square brackets are just text. A `{` or `[` with
    /// another one before its closing bracket is just a character.
    pub fn fill(
        template: &str,
        value: impl Fn(&str) -> Option<EmoteText>,
        emote: impl Fn(&str) -> Option<EmoteInfo>,
        style: impl Fn(&str) -> Option<Option<TextStyle>>,
    ) -> EmoteText {
        let mut out = EmoteText::default();
        let mut current: Option<TextStyle> = None;
        // A style's mark is open (styles off: with no look).
        let mut marked = false;
        let mut rest = template;
        while let Some(open) = rest.find(['{', '[']) {
            out.push_styled(&rest[..open], &current);
            let bracket = rest.as_bytes()[open];
            let close = if bracket == b'{' { '}' } else { ']' };
            let after = &rest[open + 1..];
            // The name runs to the closing bracket, unless another opening
            // one comes first: then this one is a character.
            let Some(end) = after.find(close).filter(|end| !after[..*end].contains(bracket as char)) else {
                out.push_styled(&rest[open..=open], &current);
                rest = after;
                continue;
            };
            let name = &after[..end];
            if bracket == b'[' {
                if name == "/" && marked {
                    (current, marked) = (None, false);
                } else if let Some(found) = style(name) {
                    (current, marked) = (found, true);
                } else {
                    out.push_styled("[", &current);
                    rest = after;
                    continue;
                }
            } else if let Some(code) = name.strip_prefix("e:").map(str::trim) {
                match emote(code) {
                    Some(info) => out.push_emote(info.emote, info.zero_width, current.clone()),
                    None => out.push_styled(code, &current),
                }
            } else if let Some(filled) = value(name) {
                out.push_with(filled, &current);
            } else {
                out.push_styled(&format!("{{{name}}}"), &current);
            }
            rest = &after[end + 1..];
        }
        out.push_styled(rest, &current);
        out
    }

    /// What follows `command` when the text starts with it (any case):
    /// "!tts Hello" → "Hello". The command must end there: "!ttsx" isn't
    /// "!tts", though "tts:x" is "tts:".
    pub fn after_command(&self, command: &str) -> Option<EmoteText> {
        let command = command.trim();
        let Some(Piece::Text { text: first, style }) = self.pieces.first() else { return None };
        let first = first.trim_start();
        let mut chars = first.char_indices();
        for wanted in command.chars() {
            let (_, got) = chars.next()?;
            if !got.to_lowercase().eq(wanted.to_lowercase()) {
                return None;
            }
        }
        let rest_start = chars.next().map_or(first.len(), |(index, _)| index);
        let rest = &first[rest_start..];
        let ends_in_a_word = command.chars().last().is_some_and(char::is_alphanumeric);
        if command.is_empty() || (ends_in_a_word && rest.chars().next().is_some_and(char::is_alphanumeric)) {
            return None;
        }
        let mut out = EmoteText::default();
        out.push_styled(rest.trim_start(), style);
        for piece in &self.pieces[1..] {
            match piece {
                Piece::Text { text, style } if out.pieces.is_empty() => {
                    out.push_styled(text.trim_start(), style)
                }
                piece => out.pieces.push(piece.clone()),
            }
        }
        Some(out)
    }

    pub fn push_text(&mut self, text: &str) {
        self.push_styled(text, &None);
    }

    /// Text in a text style (or none), joined to the text before it when
    /// that has the same.
    fn push_styled(&mut self, text: &str, style: &Option<TextStyle>) {
        if text.is_empty() {
            return;
        }
        match self.pieces.last_mut() {
            Some(Piece::Text { text: last, style: last_style }) if last_style == style => last.push_str(text),
            _ => self.pieces.push(Piece::Text { text: text.to_string(), style: style.clone() }),
        }
    }

    pub fn push(&mut self, other: EmoteText) {
        self.push_with(other, &None);
    }

    /// `other`, its pieces without a style of their own taking `style`.
    fn push_with(&mut self, other: EmoteText, style: &Option<TextStyle>) {
        for piece in other.pieces {
            match piece {
                Piece::Text { text, style: own } => {
                    self.push_styled(&text, if own.is_some() { &own } else { style })
                }
                Piece::Emote { emote, style: own } => {
                    self.pieces.push(Piece::Emote { emote, style: own.or_else(|| style.clone()) })
                }
            }
        }
    }

    /// A zero-width emote stacks on the emote right before it (with or
    /// without one space between), as in chat; on its own it stands alone.
    fn push_emote(&mut self, emote: EmoteRef, zero_width: bool, style: Option<TextStyle>) {
        if zero_width {
            let spaced = matches!(self.pieces.last(), Some(Piece::Text { text, .. }) if text == " ");
            let below =
                if spaced { self.pieces.len().checked_sub(2) } else { self.pieces.len().checked_sub(1) };
            if let Some(Piece::Emote { emote: below, .. }) =
                below.and_then(|index| self.pieces.get_mut(index))
            {
                below.overlays.push(emote);
                if spaced {
                    self.pieces.pop();
                }
                return;
            }
        }
        self.pieces.push(Piece::Emote { emote: PostedEmote { emote, overlays: Vec::new() }, style });
    }

    /// The text with every emote as its code, as chat's text reads: for
    /// text to speech, numbers, matching words and logs.
    pub fn plain(&self) -> String {
        let mut out = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Text { text, .. } => out.push_str(text),
                Piece::Emote { emote, .. } => {
                    out.push_str(&emote.emote.code);
                    // (One from a plugin may come without its code.)
                    for overlay in emote.overlays.iter().filter(|overlay| !overlay.code.is_empty()) {
                        out.push(' ');
                        out.push_str(&overlay.code);
                    }
                }
            }
        }
        out
    }

    /// Nothing but whitespace.
    pub fn is_blank(&self) -> bool {
        self.pieces.iter().all(|piece| matches!(piece, Piece::Text { text, .. } if text.trim().is_empty()))
    }

    /// The pieces for an alert; empty when there are no emotes (the alert
    /// then shows [`EmoteText::plain`]).
    pub fn alert_parts(&self) -> Vec<AlertPart> {
        if !self.pieces.iter().any(|piece| matches!(piece, Piece::Emote { .. })) {
            return Vec::new();
        }
        self.parts()
    }

    /// Every piece, text and emotes, as the overlay shows them.
    pub fn parts(&self) -> Vec<AlertPart> {
        self.pieces
            .iter()
            .map(|piece| match piece {
                Piece::Text { text, style } => AlertPart::Text { text: text.clone(), style: style.clone() },
                Piece::Emote { emote, style } => AlertPart::Emote {
                    code: emote.emote.code.clone(),
                    sprite: EmoteSprite {
                        image: image(&emote.emote),
                        overlays: emote.overlays.iter().map(image).collect(),
                    },
                    style: style.clone(),
                },
            })
            .collect()
    }
}

/// Plain text, for tests and fixed strings.
impl From<&str> for EmoteText {
    fn from(text: &str) -> EmoteText {
        EmoteText::plain_text(text)
    }
}

impl From<String> for EmoteText {
    fn from(text: String) -> EmoteText {
        EmoteText::plain_text(text)
    }
}

fn image(emote: &EmoteRef) -> EmoteImage {
    EmoteImage { source: emote.source.clone(), id: emote.id.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emote(code: &str) -> EmoteRef {
        EmoteRef { source: "7tv".into(), id: format!("id-{code}"), code: code.into() }
    }

    fn known(code: &str) -> Option<EmoteInfo> {
        let info = |zero_width| Some(EmoteInfo { emote: emote(code), image_url: String::new(), zero_width });
        match code {
            "PogU" | "peepoHappy" => info(false),
            "RainTime" => info(true),
            _ => None,
        }
    }

    fn kappa_at(start: usize) -> EmoteSpan {
        EmoteSpan {
            start,
            end: start + 5,
            emote: EmoteRef { source: "twitch".into(), id: "25".into(), code: "Kappa".into() },
        }
    }

    /// Each piece as text, emotes in brackets with their overlays.
    fn shape(text: &EmoteText) -> String {
        text.pieces
            .iter()
            .map(|piece| match piece {
                Piece::Text { text, style: None } => text.clone(),
                Piece::Text { text, style: Some(style) } => format!("<{}>{text}</>", style.name),
                Piece::Emote { emote, .. } => {
                    let overlays: Vec<&str> = emote.overlays.iter().map(|o| o.code.as_str()).collect();
                    format!(
                        "[{}{}]",
                        emote.emote.code,
                        overlays.iter().map(|o| format!("+{o}")).collect::<String>()
                    )
                }
            })
            .collect()
    }

    #[test]
    fn messages_are_marked_where_chat_shows_emotes() {
        let message = "Kappa hi PogU RainTime Kappa! PogU";
        let text = EmoteText::from_message(message, &[kappa_at(0)], known);
        assert_eq!(shape(&text), "[Kappa] hi [PogU+RainTime] Kappa! [PogU]");
        assert_eq!(text.plain(), message, "read back as chat wrote it");
        // A word the platform didn't mark stays text, even if it's a code.
        assert_eq!(shape(&EmoteText::from_message("Kappa", &[], known)), "Kappa");
        // A zero-width emote alone stands on its own; whitespace is kept.
        assert_eq!(shape(&EmoteText::from_message("RainTime  ok", &[], known)), "[RainTime]  ok");
    }

    #[test]
    fn cheer_words_are_left_out_of_cheers() {
        let is_cheer = |word: &str| {
            let prefix = word.trim_end_matches(|c: char| c.is_ascii_digit());
            prefix.len() < word.len() && ["cheer", "biblethump"].contains(&prefix.to_lowercase().as_str())
        };
        let cheer = |text: &str| shape(&EmoteText::from_cheer(text, &[], known, is_cheer));
        assert_eq!(cheer("Cheer100 nice PogU"), "nice [PogU]");
        assert_eq!(cheer("so cheer50 good BibleThump10"), "so good");
        assert_eq!(cheer("Cheer100 Cheer100"), "");
        // Not cheers: no amount, an unknown prefix, a plain message.
        assert_eq!(cheer("Cheer Cheerful100 Cheer!"), "Cheer Cheerful100 Cheer!");
        assert_eq!(shape(&EmoteText::from_message("Cheer100 hi", &[], known)), "Cheer100 hi");
    }

    #[test]
    fn commands_and_what_follows() {
        let message = |text: &str| EmoteText::from_message(text, &[], known);
        let after = |text: &str, command: &str| message(text).after_command(command).map(|rest| shape(&rest));
        assert_eq!(after("!tts Hello streamer PogU", "!tts").as_deref(), Some("Hello streamer [PogU]"));
        assert_eq!(after("TTS: hi", "tts:").as_deref(), Some("hi"));
        assert_eq!(after("tts:hi", "tts:").as_deref(), Some("hi"));
        assert_eq!(after("!tts", "!tts").as_deref(), Some(""));
        assert_eq!(after("!ttsx hi", "!tts"), None, "a longer word isn't the command");
        assert_eq!(after("hi !tts", "!tts"), None, "only at the start");
        assert_eq!(after("!tts PogU", "!tts").as_deref(), Some("[PogU]"));
        assert_eq!(after("!t", "!tts"), None);
    }

    #[test]
    fn templates_fill_values_and_emotes() {
        let value = |name: &str| match name {
            "viewer" => Some(EmoteText::plain_text("PogU")),
            "message" => Some(EmoteText::from_message("so good PogU", &[], known)),
            _ => None,
        };
        let text = EmoteText::fill(
            "{e:peepoHappy} {viewer} said: {message} {e:Nope} {other}",
            value,
            known,
            no_style,
        );
        assert_eq!(shape(&EmoteText::fill("unclosed {", value, known, no_style)), "unclosed {");
        // A viewer named like an emote stays text; only marked emotes show.
        assert_eq!(shape(&text), "[peepoHappy] PogU said: so good [PogU] Nope {other}");
        assert_eq!(
            shape(&EmoteText::fill("{e:PogU} {e:RainTime}", |_| None, known, no_style)),
            "[PogU+RainTime]"
        );
        // Viewer text is never read for markers.
        assert_eq!(shape(&EmoteText::from_message("{e:PogU}", &[], known)), "{e:PogU}");
    }

    fn no_style(_: &str) -> Option<Option<TextStyle>> {
        None
    }

    #[test]
    fn templates_mark_parts_with_text_styles() {
        let style = |name: &str| {
            ["big", "small", "viewer"]
                .contains(&name)
                .then(|| Some(TextStyle { name: name.into(), ..TextStyle::default() }))
        };
        let value = |name: &str| match name {
            "viewer" => Some(EmoteText::plain_text("Ann")),
            "message" => Some(EmoteText::from_message("hi PogU [/]", &[], known)),
            _ => None,
        };
        let fill = |template: &str| shape(&EmoteText::fill(template, value, known, style));
        assert_eq!(fill("[big]{viewer}[/] subscribed"), "<big>Ann</> subscribed");
        // A value's pieces take the style; viewer text isn't read for marks.
        assert_eq!(fill("[small]{message}[/]!"), "<small>hi </>[PogU]<small> [/]</>!");
        // The next style switches; the end needs no `[/]`.
        assert_eq!(fill("[big]a[small]b"), "<big>a</><small>b</>");
        // Styles and values don't share names; a stray `[/]`, other square
        // brackets and unknown names stay.
        assert_eq!(fill("[viewer]{viewer}[/] [/] [VIP] {nope} [x"), "<viewer>Ann</> [/] [VIP] {nope} [x");
        // A bracket typed before a mark or a placeholder is a character.
        assert_eq!(fill("{{viewer} [[big]x"), "{Ann [<big>x</>");
        let text = EmoteText::fill("[big]{e:PogU} {viewer}[/]", value, known, style);
        assert_eq!(text.plain(), "PogU Ann", "styles don't show in the plain text");
        let parts = text.parts();
        assert!(parts.iter().all(|part| part.style().is_some_and(|style| style.name == "big")), "{parts:?}");
        let after =
            EmoteText::fill("[big]!tts hi[/] there", value, known, style).after_command("!tts").unwrap();
        assert_eq!(shape(&after), "<big>hi</> there");
        // Styles off: the marks are taken out, with no look.
        let off = |name: &str| (name == "big").then_some(None);
        let text = EmoteText::fill("[big]{viewer}[/] subscribed [VIP]", value, known, off);
        assert_eq!(shape(&text), "Ann subscribed [VIP]");
        assert!(text.parts().iter().all(|part| part.style().is_none()));
    }

    #[test]
    fn alerts_get_parts_only_with_emotes() {
        assert!(EmoteText::plain_text("no emotes").alert_parts().is_empty());
        let parts = EmoteText::from_message("hi PogU", &[], known).alert_parts();
        assert_eq!(
            parts,
            [
                AlertPart::Text { text: "hi ".into(), style: None },
                AlertPart::Emote {
                    code: "PogU".into(),
                    sprite: EmoteSprite { image: image(&emote("PogU")), overlays: Vec::new() },
                    style: None,
                },
            ]
        );
        assert!(EmoteText::plain_text("  ").is_blank());
        assert!(!EmoteText::from_message("PogU", &[], known).is_blank());
    }
}
