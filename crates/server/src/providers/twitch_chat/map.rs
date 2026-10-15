//! Twitch IRC → platform-neutral `Input`s. Tag names were confirmed against
//! captured chat (`tests/fixtures/irc/`).

use std::collections::HashSet;

use super::parse::IrcMessage;
use crate::model::{
    Amount, Badge, ChannelRef, EmoteRef, EmoteSpan, Input, InputKind, MembershipKind, Message, MessageMarks,
    Moderation, Origin, PlatformEvent, ReplyTo, RewardRef, Roles, UpgradedFrom, User,
};

pub const PROVIDER: &str = "twitch_chat";
pub const PLATFORM: &str = "twitch";
/// Twitch marks anonymous gifts with this login.
const ANONYMOUS_GIFTER: &str = "ananonymousgifter";

/// Image URL for a Twitch emote: `default` is the animated GIF when the emote
/// has one, else a PNG; `2.0` is the 2x size.
pub fn emote_image_url(id: &str) -> String {
    format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/2.0")
}

/// Maps one connection's lines. Create a new one per connection: the first
/// `ROOMSTATE` after each join becomes `ChannelJoined`; later ones only report
/// room-setting changes and are ignored.
#[derive(Default)]
pub struct Mapper {
    joined: HashSet<String>,
}

impl Mapper {
    pub fn new() -> Mapper {
        Mapper::default()
    }

    pub fn map(&mut self, message: &IrcMessage, now_ms: u64) -> Vec<Input> {
        let Some(channel) = message.params.first().and_then(|p| p.strip_prefix('#')) else {
            return Vec::new();
        };
        let Some(room_id) = message.tag("room-id") else { return Vec::new() };
        let origin = Origin {
            provider: PROVIDER.to_string(),
            platform: PLATFORM.to_string(),
            channel: ChannelRef { id: room_id.to_string(), name: channel.to_string() },
        };
        let input = |kind| Input { at_ms: now_ms, origin: origin.clone(), kind };

        match message.command.as_str() {
            "ROOMSTATE" if self.joined.insert(channel.to_string()) => vec![input(InputKind::ChannelJoined)],
            "PRIVMSG" => {
                let Some(text) = message.params.get(1) else { return Vec::new() };
                let user = user(message, message.tag("login").or(message.nick()));
                let chat = chat_message(message, user.clone(), text);
                let mut inputs = vec![input(InputKind::Message(chat.clone()))];
                if let Some(bits) = message.tag("bits").and_then(|bits| bits.parse().ok()) {
                    inputs.push(input(InputKind::Event(PlatformEvent::Support {
                        user: user.clone(),
                        amount: Amount::Units { value: bits, unit: "bits".to_string() },
                        message: Some(chat.clone()),
                    })));
                }
                if let Some(reward_id) = message.tag("custom-reward-id") {
                    inputs.push(input(InputKind::Event(PlatformEvent::Redemption {
                        user,
                        reward: RewardRef { id: reward_id.to_string(), title: None },
                        input: Some(chat.text),
                    })));
                }
                inputs
            }
            "USERNOTICE" => {
                usernotice(message).map(|event| vec![input(InputKind::Event(event))]).unwrap_or_default()
            }
            "CLEARMSG" => match message.tag("target-msg-id") {
                Some(id) => {
                    vec![input(InputKind::Moderation(Moderation::MessageDeleted { id: id.to_string() }))]
                }
                None => Vec::new(),
            },
            // With a target, a ban or timeout; without, the whole chat.
            "CLEARCHAT" => {
                let moderation = match message.tag("target-user-id") {
                    Some(user_id) => Moderation::UserCleared { user_id: user_id.to_string() },
                    None => Moderation::ChatCleared,
                };
                vec![input(InputKind::Moderation(moderation))]
            }
            _ => Vec::new(),
        }
    }
}

fn usernotice(message: &IrcMessage) -> Option<PlatformEvent> {
    let msg_id = message.tag("msg-id")?;
    let user = || user(message, message.tag("login"));
    let attached = || message.params.get(1).map(|text| chat_message(message, user(), text));
    let tier = message.tag("msg-param-sub-plan").map(str::to_string);
    let number = |name: &str| message.tag(name).and_then(|value| value.parse::<u32>().ok());
    let gifter = || (message.tag("login") != Some(ANONYMOUS_GIFTER)).then(user);
    let batch_id = message.tag("msg-param-community-gift-id").map(str::to_string);

    let event = match msg_id {
        "sub" => {
            PlatformEvent::Membership { user: user(), kind: MembershipKind::New, tier, message: attached() }
        }
        "resub" => PlatformEvent::Membership {
            user: user(),
            kind: MembershipKind::Renewal { months: number("msg-param-cumulative-months").unwrap_or(1) },
            tier,
            message: attached(),
        },
        "subgift" => {
            let recipient = User {
                id: message.tag("msg-param-recipient-id")?.to_string(),
                name: message.tag("msg-param-recipient-user-name")?.to_string(),
                display_name: message
                    .tag("msg-param-recipient-display-name")
                    .or(message.tag("msg-param-recipient-user-name"))?
                    .to_string(),
                ..Default::default()
            };
            PlatformEvent::Membership {
                user: recipient,
                kind: MembershipKind::Gifted { gifter: gifter(), batch_id },
                tier,
                message: None,
            }
        }
        // "Viewer converted from a Prime sub to a Tier 1 sub!"
        "primepaidupgrade" => PlatformEvent::Membership {
            user: user(),
            kind: MembershipKind::Upgraded { from: UpgradedFrom::Prime },
            tier,
            message: attached(),
        },
        // "Viewer is continuing the Gift Sub they got from Gifter!"
        "giftpaidupgrade" | "anongiftpaidupgrade" => {
            let gifter = (msg_id == "giftpaidupgrade")
                .then(|| message.tag("msg-param-sender-login"))
                .flatten()
                .map(|login| User {
                    id: String::new(),
                    name: login.to_string(),
                    display_name: message.tag("msg-param-sender-name").unwrap_or(login).to_string(),
                    ..Default::default()
                });
            PlatformEvent::Membership {
                user: user(),
                kind: MembershipKind::Upgraded { from: UpgradedFrom::Gift { gifter } },
                tier,
                message: attached(),
            }
        }
        "submysterygift" => PlatformEvent::GiftBatch {
            gifter: gifter(),
            count: number("msg-param-mass-gift-count").unwrap_or(1),
            tier,
            batch_id: batch_id
                .or_else(|| message.tag("msg-param-origin-id").map(str::to_string))
                .or_else(|| message.tag("id").map(str::to_string))
                .unwrap_or_default(),
        },
        "raid" => PlatformEvent::Raid {
            from: User {
                id: message.tag("user-id").unwrap_or_default().to_string(),
                name: message.tag("msg-param-login").or(message.tag("login")).unwrap_or_default().to_string(),
                display_name: message
                    .tag("msg-param-displayName")
                    .or(message.tag("display-name"))
                    .unwrap_or_default()
                    .to_string(),
                roles: roles(message),
                color: color(message),
                badges: badges(message),
            },
            viewers: number("msg-param-viewerCount").unwrap_or(0),
        },
        other => {
            tracing::debug!(msg_id = other, "ignoring USERNOTICE");
            return None;
        }
    };
    Some(event)
}

fn user(message: &IrcMessage, login: Option<&str>) -> User {
    let name = login.unwrap_or_default().to_string();
    User {
        id: message.tag("user-id").unwrap_or_default().to_string(),
        display_name: message.tag("display-name").map(str::to_string).unwrap_or_else(|| name.clone()),
        name,
        roles: roles(message),
        color: color(message),
        badges: badges(message),
    }
}

/// `#RRGGBB`, lowercased; empty when the viewer never picked one.
fn color(message: &IrcMessage) -> Option<String> {
    message
        .tag("color")
        .filter(|color| color.len() == 7 && color.starts_with('#'))
        .map(str::to_ascii_lowercase)
}

/// `subscriber/12,premium/1`, in order.
fn badges(message: &IrcMessage) -> Vec<Badge> {
    message
        .tag("badges")
        .unwrap_or_default()
        .split(',')
        .filter_map(|badge| badge.split_once('/'))
        .map(|(set, version)| Badge { set: set.to_string(), version: version.to_string() })
        .collect()
}

fn roles(message: &IrcMessage) -> Roles {
    let badges: Vec<&str> = message
        .tag("badges")
        .map(|badges| badges.split(',').map(|badge| badge.split('/').next().unwrap_or_default()).collect())
        .unwrap_or_default();
    let has = |name: &str| badges.contains(&name);
    Roles {
        broadcaster: has("broadcaster"),
        moderator: has("moderator") || message.tag("mod") == Some("1"),
        vip: has("vip") || message.tag("vip") == Some("1"),
        member: has("subscriber") || has("founder") || message.tag("subscriber") == Some("1"),
    }
}

/// A chat message with Twitch's own emotes as spans. `/me` messages arrive
/// wrapped in `\x01ACTION …\x01`; emote positions are relative to the inner
/// text.
fn chat_message(message: &IrcMessage, user: User, text: &str) -> Message {
    let inner = text.strip_prefix("\u{1}ACTION ").and_then(|inner| inner.strip_suffix('\u{1}'));
    let action = inner.is_some();
    let text = inner.unwrap_or(text).to_string();
    let emotes = emote_spans(message.tag("emotes"), &text);
    let reply_to = message.tag("reply-parent-msg-id").map(|_| ReplyTo {
        user: message
            .tag("reply-parent-display-name")
            .or(message.tag("reply-parent-user-login"))
            .unwrap_or_default()
            .to_string(),
        text: message.tag("reply-parent-msg-body").unwrap_or_default().to_string(),
    });
    let marks = MessageMarks {
        action,
        highlighted: message.tag("msg-id") == Some("highlighted-message"),
        first: message.tag("first-msg") == Some("1"),
        reward: message.tag("custom-reward-id").is_some(),
        cheer: message.tag("bits").is_some(),
        reply_to,
    };
    Message { id: message.tag("id").unwrap_or_default().to_string(), user, text, emotes, marks }
}

/// Parses `id:start-end,start-end/id2:start-end` (inclusive code-point
/// positions) into spans, sorted by position.
fn emote_spans(tag: Option<&str>, text: &str) -> Vec<EmoteSpan> {
    let Some(tag) = tag else { return Vec::new() };
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    for part in tag.split('/') {
        let Some((id, ranges)) = part.split_once(':') else { continue };
        for range in ranges.split(',') {
            let Some((start, end)) = range.split_once('-') else { continue };
            let (Ok(start), Ok(end)) = (start.parse::<usize>(), end.parse::<usize>()) else { continue };
            if start > end || end >= chars.len() {
                continue;
            }
            spans.push(EmoteSpan {
                start,
                end: end + 1,
                emote: EmoteRef {
                    source: PLATFORM.to_string(),
                    id: id.to_string(),
                    code: chars[start..=end].iter().collect(),
                },
            });
        }
    }
    spans.sort_by_key(|span| span.start);
    spans
}

/// Messages an input carries, for registering the emotes in them.
pub fn messages(input: &Input) -> impl Iterator<Item = &Message> {
    let message = match &input.kind {
        InputKind::Message(message) => Some(message),
        InputKind::Event(
            PlatformEvent::Support { message, .. } | PlatformEvent::Membership { message, .. },
        ) => message.as_ref(),
        _ => None,
    };
    message.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::twitch_chat::parse::parse_line;

    fn fixture(name: &str) -> Vec<IrcMessage> {
        let path = format!("{}/tests/fixtures/irc/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path).unwrap().lines().map(|line| parse_line(line).unwrap()).collect()
    }

    fn map_all(mapper: &mut Mapper, messages: &[IrcMessage]) -> Vec<Input> {
        messages.iter().flat_map(|message| mapper.map(message, 0)).collect()
    }

    #[test]
    fn first_roomstate_per_channel_joins() {
        let mut mapper = Mapper::new();
        let lines = fixture("roomstate.txt");
        let inputs = map_all(&mut mapper, &lines);
        assert_eq!(inputs.len(), lines.len());
        assert!(inputs.iter().all(|input| input.kind == InputKind::ChannelJoined));
        assert_eq!(inputs[0].origin.channel.name, "kaicenat");
        assert!(!inputs[0].origin.channel.id.is_empty());
        // A later room-setting change (shape per Twitch docs; none was
        // captured) is ignored.
        let change = parse_line("@room-id=641972806;slow=10 :tmi.twitch.tv ROOMSTATE #kaicenat").unwrap();
        assert!(mapper.map(&change, 0).is_empty());
    }

    #[test]
    fn chat_messages_with_emotes() {
        let inputs = map_all(&mut Mapper::new(), &fixture("privmsg.txt"));
        let messages: Vec<&Message> = inputs
            .iter()
            .filter_map(|input| match &input.kind {
                InputKind::Message(message) => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(messages.len(), 12);
        let first = messages[0];
        assert_eq!(first.text, "LUL LUL LUL");
        assert_eq!(first.emotes.len(), 3);
        assert_eq!((first.emotes[1].start, first.emotes[1].end), (4, 7));
        assert_eq!(
            first.emotes[1].emote,
            EmoteRef { source: "twitch".into(), id: "425618".into(), code: "LUL".into() }
        );
        assert_eq!(first.user.name, "viewer1");
        assert!(first.user.roles.member);
        assert_eq!(first.id, "a1527442-4772-4b2b-93e8-4ef15e220fb3");
        assert_eq!(first.user.color.as_deref(), Some("#daa520"));
        assert_eq!(
            first.user.badges,
            [
                Badge { set: "subscriber".into(), version: "12".into() },
                Badge { set: "pichu".into(), version: "1".into() }
            ]
        );
        assert!(first.marks.is_plain());
        assert_eq!(messages[3].user.color, None, "no colour picked");
        assert_eq!(messages.iter().filter(|message| message.marks.first).count(), 3);
        // Every span's code matches the text at its position.
        for message in &messages {
            for span in &message.emotes {
                let text: String =
                    message.text.chars().skip(span.start).take(span.end - span.start).collect();
                assert_eq!(text, span.emote.code);
            }
        }
    }

    #[test]
    fn bits_become_support_as_well_as_a_message() {
        let inputs = map_all(&mut Mapper::new(), &fixture("privmsg.txt"));
        let bits: Vec<u64> = inputs
            .iter()
            .filter_map(|input| match &input.kind {
                InputKind::Event(PlatformEvent::Support {
                    amount: Amount::Units { value, unit },
                    message,
                    ..
                }) => {
                    assert_eq!(unit, "bits");
                    assert!(message.as_ref().is_some_and(|m| m.text.starts_with("Cheer")));
                    Some(*value)
                }
                _ => None,
            })
            .collect();
        assert_eq!(bits, [1, 100, 7, 1500]);
    }

    #[test]
    fn subs_resubs_raids_and_ignored_notices() {
        let events: Vec<PlatformEvent> = map_all(&mut Mapper::new(), &fixture("usernotice.txt"))
            .into_iter()
            .filter_map(|input| match input.kind {
                InputKind::Event(event) => Some(event),
                _ => None,
            })
            .collect();
        let count = |pred: &dyn Fn(&PlatformEvent) -> bool| events.iter().filter(|e| pred(e)).count();
        assert_eq!(
            count(&|e| matches!(
                e,
                PlatformEvent::Membership { kind: MembershipKind::New, tier: Some(_), .. }
            )),
            2
        );
        assert_eq!(
            count(
                &|e| matches!(e, PlatformEvent::Membership { kind: MembershipKind::Renewal { months }, .. } if *months > 1)
            ),
            2
        );
        let raid = events.iter().find_map(|e| match e {
            PlatformEvent::Raid { from, viewers } => Some((from, viewers)),
            _ => None,
        });
        let (from, viewers) = raid.expect("a raid");
        assert!(from.name.starts_with("viewer") && from.display_name.starts_with("Viewer"));
        assert!(*viewers > 0);
        // announcement and viewermilestone are ignored.
        assert!(events.iter().all(|e| !matches!(e, PlatformEvent::Custom { .. })));
        assert_eq!(events.len(), 2 + 2 + 1 + 2 + 1, "sub, resub, raid, standalone gifts, one batch");
    }

    #[test]
    fn gift_batch_links_its_gifts() {
        let events: Vec<PlatformEvent> = map_all(&mut Mapper::new(), &fixture("gift_batch.txt"))
            .into_iter()
            .filter_map(|input| match input.kind {
                InputKind::Event(event) => Some(event),
                _ => None,
            })
            .collect();
        let PlatformEvent::GiftBatch { gifter, count, batch_id, .. } = &events[0] else {
            panic!("{:?}", events[0])
        };
        assert_eq!(*count, 5);
        assert!(gifter.is_some());
        let gifts: Vec<_> = events[1..]
            .iter()
            .map(|e| match e {
                PlatformEvent::Membership {
                    kind: MembershipKind::Gifted { batch_id: Some(id), gifter: Some(_) },
                    user,
                    ..
                } => {
                    assert!(user.name.starts_with("viewer"));
                    id.clone()
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(gifts.len(), 5);
        assert!(gifts.iter().all(|id| id == batch_id));
    }

    #[test]
    fn upgraded_subs() {
        let upgraded = |line: &str| {
            let inputs = Mapper::new().map(&parse_line(line).unwrap(), 0);
            match &inputs[..] {
                [Input { kind: InputKind::Event(PlatformEvent::Membership { kind, user, .. }), .. }] => {
                    (kind.clone(), user.display_name.clone())
                }
                other => panic!("one sub: {other:?}"),
            }
        };
        let (kind, viewer) = upgraded(
            "@badges=subscriber/0;display-name=Viewer;login=viewer;msg-id=primepaidupgrade;\
             msg-param-sub-plan=1000;room-id=1;user-id=2 :tmi.twitch.tv USERNOTICE #chan",
        );
        assert_eq!(
            (kind, viewer.as_str()),
            (MembershipKind::Upgraded { from: UpgradedFrom::Prime }, "Viewer")
        );
        let (kind, _) = upgraded(
            "@display-name=Viewer;login=viewer;msg-id=giftpaidupgrade;msg-param-sender-login=gifter;\
             msg-param-sender-name=Gifter;room-id=1;user-id=2 :tmi.twitch.tv USERNOTICE #chan",
        );
        let MembershipKind::Upgraded { from: UpgradedFrom::Gift { gifter: Some(gifter) } } = kind else {
            panic!("{kind:?}")
        };
        assert_eq!((gifter.name.as_str(), gifter.display_name.as_str()), ("gifter", "Gifter"));
        let (kind, _) = upgraded(
            "@display-name=Viewer;login=viewer;msg-id=anongiftpaidupgrade;room-id=1;user-id=2 \
             :tmi.twitch.tv USERNOTICE #chan",
        );
        assert_eq!(kind, MembershipKind::Upgraded { from: UpgradedFrom::Gift { gifter: None } });
    }

    #[test]
    fn anonymous_gifts_have_no_gifter() {
        let line = "@badges=;display-name=AnAnonymousGifter;login=ananonymousgifter;msg-id=subgift;\
            msg-param-recipient-display-name=Viewer9;msg-param-recipient-id=9;msg-param-recipient-user-name=viewer9;\
            msg-param-sub-plan=1000;room-id=1;user-id=274598607 :tmi.twitch.tv USERNOTICE #chan";
        let inputs = Mapper::new().map(&parse_line(line).unwrap(), 0);
        let InputKind::Event(PlatformEvent::Membership {
            kind: MembershipKind::Gifted { gifter, batch_id },
            user,
            ..
        }) = &inputs[0].kind
        else {
            panic!("{inputs:?}")
        };
        assert_eq!(*gifter, None);
        assert_eq!(*batch_id, None);
        assert_eq!(user.display_name, "Viewer9");
    }

    #[test]
    fn me_messages_and_rewards() {
        let line = "@emotes=25:0-4;room-id=1;user-id=2;display-name=A;custom-reward-id=abc :a!a@a.tmi.twitch.tv PRIVMSG #chan :\u{1}ACTION Kappa hi\u{1}";
        let inputs = Mapper::new().map(&parse_line(line).unwrap(), 0);
        let InputKind::Message(message) = &inputs[0].kind else { panic!() };
        assert_eq!(message.text, "Kappa hi");
        assert!(message.marks.action && message.marks.reward && !message.marks.cheer);
        assert_eq!(message.emotes[0].emote.code, "Kappa");
        assert!(
            matches!(&inputs[1].kind, InputKind::Event(PlatformEvent::Redemption { reward, .. }) if reward.id == "abc")
        );
    }

    #[test]
    fn highlights_and_replies() {
        let line = "@id=m1;msg-id=highlighted-message;reply-parent-msg-id=p1;reply-parent-display-name=Bob;\
            reply-parent-msg-body=hi\\sthere;room-id=1;user-id=2;display-name=A :a!a@a.tmi.twitch.tv PRIVMSG #chan :@Bob yes";
        let inputs = Mapper::new().map(&parse_line(line).unwrap(), 0);
        let InputKind::Message(message) = &inputs[0].kind else { panic!() };
        assert!(message.marks.highlighted && !message.marks.action);
        assert_eq!(message.marks.reply_to, Some(ReplyTo { user: "Bob".into(), text: "hi there".into() }));
    }

    #[test]
    fn moderation() {
        let cleared: Vec<Moderation> = map_all(&mut Mapper::new(), &fixture("other.txt"))
            .into_iter()
            .map(|input| match input.kind {
                InputKind::Moderation(moderation) => moderation,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(cleared.len(), 6, "timeouts");
        assert_eq!(cleared[0], Moderation::UserCleared { user_id: "1000032".into() });
        let map = |line: &str| Mapper::new().map(&parse_line(line).unwrap(), 0).remove(0).kind;
        assert_eq!(
            map("@room-id=1;tmi-sent-ts=1 :tmi.twitch.tv CLEARCHAT #chan"),
            InputKind::Moderation(Moderation::ChatCleared)
        );
        assert_eq!(
            map("@login=a;room-id=1;target-msg-id=m1;tmi-sent-ts=1 :tmi.twitch.tv CLEARMSG #chan :bad words"),
            InputKind::Moderation(Moderation::MessageDeleted { id: "m1".into() })
        );
    }
}
