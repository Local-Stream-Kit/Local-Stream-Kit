//! The chat display (the built-in Chat display plugin's page, `crates/chat`):
//! chat messages on the overlay, their look set entirely by the streamer's
//! own CSS (the plugin's `look` file setting). The markup below is the
//! contract for that CSS; it's made here, once, by [`ChatLine::html`]. The
//! app sends pages that hear the chat every message as a [`ChatLine`].
//!
//! ```html
//! <div class="chat">                                  <!-- the module's layer -->
//!   <div class="msg msg--chat msg--from-twitch msg--text msg--moderator"
//!        data-id="twitch:…" data-platform="twitch" data-user="viewer" style="--name-color: #ff4500">
//!     <div class="msg__reply"><span class="msg__reply-name">Bob</span> <span class="msg__reply-text">hi</span></div>
//!     <div class="msg__head">
//!       <span class="msg__badges"><img class="badge badge--moderator" src="…" alt="Moderator"></span>
//!       <span class="msg__name">Viewer</span>
//!     </div>
//!     <div class="msg__body">hello <span class="emote"><img class="emote__image" src="…" alt="Kappa"></span></div>
//!     <!-- a part a card's text marked with a text style, `[highlight]…[/]`: -->
//!     <div class="msg__body"><span class="text-style text-style-highlight" style="color: #ffcc00">Ann</span> subscribed</div>
//!   </div>
//! </div>
//! ```
//!
//! Classes on `.msg`, each only when it applies:
//!
//! - Where it came from: `msg--chat` (a viewer's message) or `msg--card` (a
//!   card's Show in chat node, plus the node's own class); `msg--from-<platform>`
//!   (`msg--from-twitch`, `msg--from-youtube`, …).
//! - What's in it, exactly one: `msg--single-emote` (one emote and nothing
//!   else), `msg--emotes-only` (two or more emotes and nothing else), or
//!   `msg--text` (any text, with or without emotes).
//! - Who wrote it: `msg--broadcaster`, `msg--moderator`, `msg--vip`,
//!   `msg--member` (subscriber).
//! - What sets it apart: `msg--action` (`/me`), `msg--highlighted`,
//!   `msg--first` (first message in the channel), `msg--reward` (sent with
//!   a channel point reward), `msg--cheer`, `msg--reply`.
//! - Coming and going: `msg--restored` (shown again after the overlay
//!   reloaded, so CSS can skip its entrance), `msg--leaving` (added before
//!   it's removed; the overlay waits for its animation or transition).
//!
//! `msg__head` is left out when a line has no name; `msg__badges` when it
//! has no badges; `msg__reply` unless it's a reply. A badge's image is left
//! out until the badge registry knows it (it needs the Twitch login); its
//! class is always there.

use serde::{Deserialize, Serialize};

use crate::plugin::TextPart;
use crate::{AlertPart, EmoteImage, EmoteSprite};

/// One message on the chat display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatLine {
    /// Unique on the page: [`line_id`] for a viewer's message, else made up.
    pub id: String,
    /// `"twitch"`, `"youtube"`, …
    pub platform: String,
    /// When it was said (Unix ms), for its lifetime.
    pub at_ms: u64,
    pub source: LineSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<ChatUser>,
    /// The text, with emotes as images.
    pub parts: Vec<AlertPart>,
    #[serde(default)]
    pub marks: ChatMarks,
}

/// Where a line came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LineSource {
    /// A viewer's chat message.
    Chat,
    /// A card's Show in chat node, with the node's class (may be empty).
    Card { class: String },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatUser {
    pub id: String,
    /// Login / handle.
    pub name: String,
    pub display_name: String,
    /// `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<ChatBadge>,
    #[serde(default)]
    pub roles: ChatRoles,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatRoles {
    pub broadcaster: bool,
    pub moderator: bool,
    pub vip: bool,
    pub member: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatBadge {
    /// The badge's set, as the platform names it (`"subscriber"`).
    pub set: String,
    /// For a person (`"6-Month Subscriber"`); the set when unknown.
    pub title: String,
    /// Where its image loads from; `None` while unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// What sets a message apart. Mirrors the server's `MessageMarks`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatMarks {
    pub action: bool,
    pub highlighted: bool,
    pub first: bool,
    pub reward: bool,
    pub cheer: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ChatReply>,
    /// Answers another message, whether or not `reply_to` shows which.
    pub reply: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatReply {
    pub user: String,
    pub text: String,
}

/// The id of a viewer's message on the page.
pub fn line_id(platform: &str, message_id: &str) -> String {
    format!("{platform}:{message_id}")
}

/// What a line's text is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    SingleEmote,
    EmotesOnly,
    Text,
}

impl ChatLine {
    pub fn content(&self) -> Content {
        let mut emotes = 0;
        for part in &self.parts {
            match part {
                AlertPart::Emote { .. } => emotes += 1,
                AlertPart::Text { text, .. } if !text.trim().is_empty() => return Content::Text,
                AlertPart::Text { .. } => {}
            }
        }
        match emotes {
            0 => Content::Text,
            1 => Content::SingleEmote,
            _ => Content::EmotesOnly,
        }
    }

    /// The classes on `.msg` (see the module docs).
    pub fn classes(&self) -> String {
        let mut classes = vec!["msg".to_string()];
        match &self.source {
            LineSource::Chat => classes.push("msg--chat".into()),
            LineSource::Card { class } => {
                classes.push("msg--card".into());
                classes.extend(class.split_whitespace().map(class_name).filter(|class| !class.is_empty()));
            }
        }
        let platform = class_name(&self.platform);
        if !platform.is_empty() {
            classes.push(format!("msg--from-{platform}"));
        }
        classes.push(
            match self.content() {
                Content::SingleEmote => "msg--single-emote",
                Content::EmotesOnly => "msg--emotes-only",
                Content::Text => "msg--text",
            }
            .into(),
        );
        if let Some(user) = &self.user {
            let roles = &user.roles;
            for (on, class) in [
                (roles.broadcaster, "msg--broadcaster"),
                (roles.moderator, "msg--moderator"),
                (roles.vip, "msg--vip"),
                (roles.member, "msg--member"),
            ] {
                if on {
                    classes.push(class.into());
                }
            }
        }
        let marks = &self.marks;
        for (on, class) in [
            (marks.action, "msg--action"),
            (marks.highlighted, "msg--highlighted"),
            (marks.first, "msg--first"),
            (marks.reward, "msg--reward"),
            (marks.cheer, "msg--cheer"),
            (marks.reply || marks.reply_to.is_some(), "msg--reply"),
        ] {
            if on {
                classes.push(class.into());
            }
        }
        classes.join(" ")
    }

    /// The line's markup: one `.msg` element. Every value is escaped, and
    /// the colour checked, so nothing a viewer types can add markup.
    pub fn html(&self) -> String {
        let mut html = format!(
            "<div class=\"{}\" data-id=\"{}\" data-platform=\"{}\"",
            self.classes(),
            escape(&self.id),
            escape(&self.platform)
        );
        if let Some(user) = &self.user {
            html.push_str(&format!(" data-user=\"{}\"", escape(&user.name)));
            if let Some((r, g, b)) = user.color.as_deref().and_then(crate::alert::parse_hex) {
                html.push_str(&format!(" style=\"--name-color: #{r:02x}{g:02x}{b:02x}\""));
            }
        }
        html.push('>');
        // First, so a look that puts the name and text on one line can.
        if let Some(reply) = &self.marks.reply_to {
            html.push_str(&format!(
                "<div class=\"msg__reply\"><span class=\"msg__reply-name\">{}</span> \
                 <span class=\"msg__reply-text\">{}</span></div>",
                escape(&reply.user),
                escape(&reply.text)
            ));
        }
        if let Some(user) = self.user.as_ref().filter(|user| !user.display_name.trim().is_empty()) {
            html.push_str("<div class=\"msg__head\">");
            if !user.badges.is_empty() {
                html.push_str("<span class=\"msg__badges\">");
                for badge in &user.badges {
                    let class = class_name(&badge.set);
                    match &badge.image {
                        Some(image) => html.push_str(&format!(
                            "<img class=\"badge badge--{class}\" src=\"{}\" alt=\"{}\" title=\"{}\">",
                            escape(image),
                            escape(&badge.title),
                            escape(&badge.title)
                        )),
                        None => html.push_str(&format!(
                            "<span class=\"badge badge--{class}\" title=\"{}\"></span>",
                            escape(&badge.title)
                        )),
                    }
                }
                html.push_str("</span>");
            }
            html.push_str(&format!("<span class=\"msg__name\">{}</span></div>", escape(&user.display_name)));
        }
        html.push_str("<div class=\"msg__body\">");
        for part in &self.parts {
            // A text style's CSS is built from checked values (`TextStyle::css`).
            if let Some(style) = part.style() {
                html.push_str(&format!(
                    "<span class=\"text-style {}\" style=\"{}\">",
                    style.class(),
                    escape(&style.css())
                ));
            }
            match part {
                AlertPart::Text { text, .. } => html.push_str(&escape(text)),
                AlertPart::Emote { code, sprite, .. } => {
                    html.push_str(&format!(
                        "<span class=\"emote\"><img class=\"emote__image\" src=\"{}\" alt=\"{}\">",
                        escape(&sprite.image.path()),
                        escape(code)
                    ));
                    for overlay in &sprite.overlays {
                        html.push_str(&format!(
                            "<img class=\"emote__overlay\" src=\"{}\" alt=\"\">",
                            escape(&overlay.path())
                        ));
                    }
                    html.push_str("</span>");
                }
            }
            if part.style().is_some() {
                html.push_str("</span>");
            }
        }
        html.push_str("</div></div>");
        html
    }
}

/// Letters, digits, `-` and `_`, lowercased: safe as (part of) a class.
pub fn class_name(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Escapes text for HTML content and double-quoted attributes.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A text value's parts (a Show in chat node's message) as a line's: its
/// emotes' addresses back to their source and id (one that isn't the
/// app's emote address shows as its code).
pub fn line_parts(parts: &[TextPart]) -> Vec<AlertPart> {
    parts
        .iter()
        .map(|part| match part {
            TextPart::Text { text, style } => AlertPart::Text { text: text.clone(), style: style.clone() },
            TextPart::Emote { emote, style } => match emote_image(&emote.url) {
                Some(image) => AlertPart::Emote {
                    code: emote.code.clone(),
                    sprite: EmoteSprite {
                        image,
                        overlays: emote.overlays.iter().filter_map(|url| emote_image(url)).collect(),
                    },
                    style: style.clone(),
                },
                None => AlertPart::Text { text: emote.code.clone(), style: style.clone() },
            },
        })
        .collect()
}

/// An emote's source and id from its address on the app ([`crate::emote_path`]).
pub fn emote_image(path: &str) -> Option<EmoteImage> {
    let rest = path.strip_prefix("/emote/")?;
    let (source, id) = rest.split_once('/')?;
    Some(EmoteImage { source: decode_segment(source)?, id: decode_segment(id)? })
}

/// Undoes `encode_segment`.
fn decode_segment(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = segment.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok().filter(|text| !text.is_empty() && !text.contains('/'))
}

/// Example lines of every kind, for the preview.
pub fn samples() -> Vec<ChatLine> {
    use crate::{EmoteImage, EmoteSprite};
    let emote = |code: &str, id: &str| AlertPart::Emote {
        code: code.into(),
        sprite: EmoteSprite {
            image: EmoteImage { source: "twitch".into(), id: id.into() },
            overlays: Vec::new(),
        },
        style: None,
    };
    let text = |text: &str| AlertPart::text(text);
    let user = |name: &str, color: &str, roles: ChatRoles, badges: &[(&str, &str)]| ChatUser {
        id: format!("sample-{}", name.to_lowercase()),
        name: name.to_lowercase(),
        display_name: name.into(),
        color: Some(color.into()),
        badges: badges
            .iter()
            .map(|(set, title)| ChatBadge { set: (*set).into(), title: (*title).into(), image: None })
            .collect(),
        roles,
    };
    let line = |n: u32, user: Option<ChatUser>, parts: Vec<AlertPart>, marks: ChatMarks| ChatLine {
        id: format!("sample:{n}"),
        platform: "twitch".into(),
        at_ms: 0,
        source: LineSource::Chat,
        user,
        parts,
        marks,
    };
    let viewer = || user("Viewer", "#1e90ff", ChatRoles::default(), &[]);
    let member = || {
        user(
            "LongTimeSub",
            "#ff7f50",
            ChatRoles { member: true, ..ChatRoles::default() },
            &[("subscriber", "Subscriber")],
        )
    };
    let moderator = || {
        user(
            "Mod",
            "#00ff7f",
            ChatRoles { moderator: true, member: true, ..ChatRoles::default() },
            &[("moderator", "Moderator"), ("subscriber", "Subscriber")],
        )
    };
    vec![
        line(1, Some(viewer()), vec![text("hello chat, how is everyone?")], ChatMarks::default()),
        line(2, Some(member()), vec![emote("Kappa", "25")], ChatMarks::default()),
        line(
            3,
            Some(moderator()),
            vec![text("that was close "), emote("LUL", "425618")],
            ChatMarks::default(),
        ),
        // A command: shown only when the display's Commands is ticked.
        line(10, Some(member()), vec![text("!lurk")], ChatMarks::default()),
        line(
            4,
            Some(member()),
            vec![emote("LUL", "425618"), text(" "), emote("LUL", "425618"), text(" "), emote("Kappa", "25")],
            ChatMarks::default(),
        ),
        line(
            5,
            Some(user("FirstTimer", "#daa520", ChatRoles::default(), &[])),
            vec![text("first time here, hi!")],
            ChatMarks { first: true, ..ChatMarks::default() },
        ),
        line(
            6,
            Some(viewer()),
            vec![text("this message is highlighted")],
            ChatMarks { highlighted: true, ..ChatMarks::default() },
        ),
        line(
            7,
            Some(moderator()),
            vec![text("@Viewer welcome!")],
            ChatMarks {
                reply_to: Some(ChatReply {
                    user: "Viewer".into(),
                    text: "hello chat, how is everyone?".into(),
                }),
                ..ChatMarks::default()
            },
        ),
        line(8, Some(viewer()), vec![text("waves")], ChatMarks { action: true, ..ChatMarks::default() }),
        ChatLine {
            id: "sample:9".into(),
            platform: "twitch".into(),
            at_ms: 0,
            source: LineSource::Card { class: "sub".into() },
            user: None,
            parts: vec![text("LongTimeSub subscribed for 12 months!")],
            marks: ChatMarks::default(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EmoteImage, EmoteSprite};

    fn emote(code: &str) -> AlertPart {
        AlertPart::Emote {
            code: code.into(),
            sprite: EmoteSprite {
                image: EmoteImage { source: "7tv".into(), id: format!("id-{code}") },
                overlays: vec![EmoteImage { source: "7tv".into(), id: "zw".into() }],
            },
            style: None,
        }
    }

    fn text(text: &str) -> AlertPart {
        AlertPart::Text { text: text.into(), style: None }
    }

    fn line(parts: Vec<AlertPart>) -> ChatLine {
        ChatLine {
            id: line_id("twitch", "m1"),
            platform: "twitch".into(),
            at_ms: 0,
            source: LineSource::Chat,
            user: Some(ChatUser {
                id: "1".into(),
                name: "viewer".into(),
                display_name: "Viewer".into(),
                color: Some("#FF4500".into()),
                badges: vec![ChatBadge {
                    set: "moderator".into(),
                    title: "Moderator".into(),
                    image: Some("/badge/twitch/moderator%2F1".into()),
                }],
                roles: ChatRoles { moderator: true, ..ChatRoles::default() },
            }),
            parts,
            marks: ChatMarks::default(),
        }
    }

    #[test]
    fn content_classes() {
        assert_eq!(line(vec![emote("Kappa")]).content(), Content::SingleEmote);
        assert_eq!(line(vec![emote("Kappa"), text("  "), emote("LUL")]).content(), Content::EmotesOnly);
        assert_eq!(line(vec![text("hi "), emote("Kappa")]).content(), Content::Text);
        assert_eq!(line(vec![text("hi")]).content(), Content::Text);
        assert_eq!(line(Vec::new()).content(), Content::Text);
        assert_eq!(
            line(vec![emote("Kappa")]).classes(),
            "msg msg--chat msg--from-twitch msg--single-emote msg--moderator"
        );
    }

    #[test]
    fn card_lines_take_their_class() {
        let mut line = line(vec![text("hi")]);
        line.source = LineSource::Card { class: "Big-Sub \"><x".into() };
        line.platform = "You Tube".into();
        line.marks.reply = true;
        assert!(!line.html().contains("msg__reply"), "a reply without what it answers");
        assert_eq!(
            line.classes(),
            "msg msg--card big-sub x msg--from-youtube msg--text msg--moderator msg--reply"
        );
    }

    #[test]
    fn html_is_escaped() {
        let mut line = line(vec![text("<b>hi</b> & "), emote("Kappa")]);
        if let Some(user) = &mut line.user {
            user.display_name = "<Viewer>".into();
            user.color = Some("red; background: url(x)".into());
        }
        let html = line.html();
        assert!(html.starts_with(
            "<div class=\"msg msg--chat msg--from-twitch msg--text msg--moderator\" data-id=\"twitch:m1\" \
             data-platform=\"twitch\" data-user=\"viewer\">"
        ));
        assert!(html.contains("<span class=\"msg__name\">&lt;Viewer&gt;</span>"), "{html}");
        assert!(html.contains("&lt;b&gt;hi&lt;/b&gt; &amp; "), "{html}");
        assert!(html.contains(
            "<span class=\"emote\"><img class=\"emote__image\" src=\"/emote/7tv/id-Kappa\" alt=\"Kappa\">\
             <img class=\"emote__overlay\" src=\"/emote/7tv/zw\" alt=\"\"></span>"
        ));
        assert!(html.contains("<img class=\"badge badge--moderator\""), "{html}");
        assert!(!html.contains("style="), "a bad colour is left out: {html}");
        line.user.as_mut().unwrap().color = Some("#FF4500".into());
        assert!(line.html().contains("style=\"--name-color: #ff4500\""));
    }

    #[test]
    fn parts_in_a_text_style_get_a_span() {
        let style = crate::TextStyle {
            name: "big".into(),
            font: "Bangers".into(),
            colour: "#ffcc00".into(),
            ..crate::TextStyle::default()
        };
        let mut kappa = emote("Kappa");
        if let AlertPart::Emote { style: emote_style, .. } = &mut kappa {
            *emote_style = Some(style.clone());
        }
        let html =
            line(vec![AlertPart::Text { text: "Ann".into(), style: Some(style) }, text(" hi"), kappa]).html();
        let open = "<span class=\"text-style text-style-big\" \
                    style=\"font-family: &quot;Bangers&quot;, system-ui, &quot;Segoe UI&quot;, sans-serif; color: #ffcc00\">";
        assert!(
            html.contains(&format!(
                "<div class=\"msg__body\">{open}Ann</span> hi{open}<span class=\"emote\">"
            )),
            "{html}"
        );
    }

    #[test]
    fn lines_roundtrip_and_take_a_text_values_emotes() {
        for sample in samples() {
            let json = serde_json::to_string(&sample).unwrap();
            assert_eq!(serde_json::from_str::<ChatLine>(&json).unwrap(), sample);
        }
        let parts = line_parts(&[
            TextPart::Text { text: "hi ".into(), style: None },
            TextPart::Emote {
                emote: crate::plugin::EmoteValue {
                    code: "Kappa".into(),
                    url: crate::emote_path("twitch", "25"),
                    overlays: vec![crate::emote_path("7tv", "a b")],
                },
                style: None,
            },
            TextPart::Emote {
                emote: crate::plugin::EmoteValue {
                    code: "Odd".into(),
                    url: "https://x/y".into(),
                    overlays: Vec::new(),
                },
                style: None,
            },
        ]);
        assert_eq!(parts[0], AlertPart::Text { text: "hi ".into(), style: None });
        let AlertPart::Emote { code, sprite, .. } = &parts[1] else { panic!("{parts:?}") };
        assert_eq!(
            (code.as_str(), sprite.image.source.as_str(), sprite.image.id.as_str()),
            ("Kappa", "twitch", "25")
        );
        assert_eq!(sprite.overlays[0].id, "a b");
        assert_eq!(parts[2], AlertPart::Text { text: "Odd".into(), style: None }, "not the app's: its code");
        assert_eq!(emote_image("/emote/x/%2E%2E%2Fa"), None, "no / in an id");
    }
}
