//! What the history records about an event: which events count, and how
//! they read.

use protocol::activity::{EventKind, NameColor};

use crate::cards::{gifter_name, name};
use crate::emote_text::EmoteText;
use crate::model::{HypeTrainChange, MembershipKind, Signal, SignalKind, UpgradedFrom};

/// An event for the history.
pub struct Described<'s> {
    pub kind: EventKind,
    /// "TestViewer cheered 500 bits".
    pub summary: String,
    /// Bits, a raid's viewers, a gift bomb's subs, a resub's months, a hype
    /// train's level; else 0.
    pub amount: u64,
    pub message: Option<&'s EmoteText>,
    /// Whether the history keeps it: not every contribution to a hype
    /// train (a busy one sends dozens), though what they queue is still
    /// of its kind.
    pub kept: bool,
}

/// The events that matter on stream: subs, gift bombs, bits (and other
/// support), raids, rewards, follows and hype trains. Not chat, not custom
/// events, and not the gifts in a gift bomb (the gift bomb is the event).
pub fn describe(signal: &Signal) -> Option<Described<'_>> {
    let (kind, summary, amount, message) = match &signal.kind {
        SignalKind::Support { user, amount, message } => {
            let bits = amount.unit() == "bits";
            let verb = if bits { "cheered" } else { "sent" };
            let kind = if bits { EventKind::Bits } else { EventKind::Support };
            (kind, format!("{} {verb} {}", name(user), amount.display()), amount.value(), message.as_ref())
        }
        SignalKind::Membership { user, kind, tier, message } => {
            let tier = match tier.as_deref() {
                Some("2000") => " (tier 2)",
                Some("3000") => " (tier 3)",
                _ => "",
            };
            let summary = match kind {
                MembershipKind::New => format!("{} subscribed{tier}", name(user)),
                MembershipKind::Renewal { months } => {
                    format!("{} resubscribed for {months} months{tier}", name(user))
                }
                MembershipKind::Gifted { batch_id: Some(_), .. } => return None,
                MembershipKind::Gifted { gifter, batch_id: None } => {
                    format!("{} gifted a sub to {}{tier}", gifter_name(gifter), name(user))
                }
                MembershipKind::Upgraded { from: UpgradedFrom::Prime } => {
                    format!("{} switched from Prime to a paid sub", name(user))
                }
                MembershipKind::Upgraded { from: UpgradedFrom::Gift { gifter } } => {
                    format!("{} is continuing the gift sub from {}", name(user), gifter_name(gifter))
                }
            };
            let months = match kind {
                MembershipKind::Renewal { months } => u64::from(*months),
                _ => 1,
            };
            (EventKind::Sub, summary, months, message.as_ref())
        }
        SignalKind::GiftBatch { gifter, count, .. } => {
            let subs = if *count == 1 { "sub" } else { "subs" };
            let summary = format!("{} gifted {count} {subs}", gifter_name(gifter));
            (EventKind::GiftBomb, summary, u64::from(*count), None)
        }
        SignalKind::Raid { from, viewers } => {
            let people = if *viewers == 1 { "viewer" } else { "viewers" };
            let summary = format!("{} raided with {viewers} {people}", name(from));
            (EventKind::Raid, summary, u64::from(*viewers), None)
        }
        SignalKind::RewardRedeemed { user, reward, input } => {
            let title = reward.title.as_deref().unwrap_or("a reward");
            (EventKind::Reward, format!("{} redeemed {title}", name(user)), 0, input.as_ref())
        }
        SignalKind::Follow { user } => (EventKind::Follow, format!("{} followed", name(user)), 0, None),
        SignalKind::HypeTrain { change, train } => {
            (EventKind::HypeTrain, train.summary(*change), u64::from(train.level), None)
        }
        SignalKind::EmotesPosted { .. }
        | SignalKind::ChatMessage { .. }
        | SignalKind::Custom { .. }
        | SignalKind::Plugin { .. } => {
            return None;
        }
    };
    let kept = !matches!(
        signal.kind,
        SignalKind::HypeTrain { change: HypeTrainChange::Progressed { level_up: false }, .. }
    );
    Some(Described { kind, summary, amount, message: message.filter(|message| !message.is_blank()), kept })
}

/// The signal's viewers' names, for the panel: with the colours they
/// picked in chat (events from EventSub, rewards and follows, have none),
/// and their ids, which open their cards.
pub fn name_colors(signal: &Signal) -> Vec<NameColor> {
    let mut names: Vec<NameColor> = Vec::new();
    for user in signal.people() {
        let color = user.color.as_deref().filter(|color| protocol::alert::parse_hex(color).is_some());
        let name = name(user);
        if !name.is_empty() && !names.iter().any(|known| known.name == name) {
            names.push(NameColor { name, color: color.unwrap_or_default().to_string(), id: user.id.clone() });
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Amount, RewardRef, User};
    use crate::pipeline::testing::{origin, user};

    fn summary(kind: SignalKind) -> Option<String> {
        describe(&Signal::new(origin(), kind)).map(|described| described.summary)
    }

    #[test]
    fn events_that_matter() {
        let bits = SignalKind::Support {
            user: user("v"),
            amount: Amount::Units { value: 500, unit: "bits".into() },
            message: Some("hype".into()),
        };
        let bits = Signal::new(origin(), bits);
        let described = describe(&bits).unwrap();
        assert_eq!((described.kind, described.summary.as_str()), (EventKind::Bits, "V cheered 500 bits"));
        assert_eq!(described.amount, 500);
        assert_eq!(described.message.map(EmoteText::plain).as_deref(), Some("hype"));
        let tier3 = SignalKind::Membership {
            user: user("v"),
            kind: MembershipKind::Renewal { months: 4 },
            tier: Some("3000".into()),
            message: None,
        };
        assert_eq!(summary(tier3).as_deref(), Some("V resubscribed for 4 months (tier 3)"));
        let in_a_bomb = SignalKind::Membership {
            user: user("v"),
            kind: MembershipKind::Gifted { gifter: None, batch_id: Some("b".into()) },
            tier: None,
            message: None,
        };
        assert_eq!(summary(in_a_bomb), None, "the gift bomb is the event");
        let bomb = SignalKind::GiftBatch { gifter: None, count: 5, tier: None };
        assert_eq!(summary(bomb).as_deref(), Some("An anonymous gifter gifted 5 subs"));
        let raid = SignalKind::Raid { from: user("r"), viewers: 1 };
        assert_eq!(summary(raid).as_deref(), Some("R raided with 1 viewer"));
        let reward = SignalKind::RewardRedeemed {
            user: user("v"),
            reward: RewardRef { id: "1".into(), title: Some("Hydrate".into()) },
            input: Some(" ".into()),
        };
        let reward = Signal::new(origin(), reward);
        let described = describe(&reward).unwrap();
        assert_eq!(described.summary, "V redeemed Hydrate");
        assert!(described.message.is_none(), "nothing written");
        assert_eq!(
            summary(SignalKind::ChatMessage { user: user("v"), text: "hi".into(), first: false }),
            None
        );
    }

    #[test]
    fn hype_trains_without_every_contribution() {
        use crate::model::HypeTrain;
        let train = |change| {
            let train = HypeTrain {
                id: "t".into(),
                level: 3,
                progress: 100,
                goal: 1500,
                total: 3100,
                kind: "regular".into(),
                shared: false,
                seconds_left: 120,
            };
            let signal = Signal::new(origin(), SignalKind::HypeTrain { change, train });
            let described = describe(&signal).unwrap();
            assert_eq!((described.kind, described.amount), (EventKind::HypeTrain, 3));
            (described.summary, described.kept)
        };
        assert_eq!(train(HypeTrainChange::Started), ("A hype train started".into(), true));
        assert_eq!(
            train(HypeTrainChange::Progressed { level_up: true }),
            ("The hype train reached level 3".into(), true)
        );
        assert_eq!(
            train(HypeTrainChange::Progressed { level_up: false }),
            ("The hype train is at level 3: 100 of 1500 points".into(), false)
        );
        assert_eq!(train(HypeTrainChange::Ended), ("The hype train ended at level 3".into(), true));
    }

    #[test]
    fn names_with_the_colours_chat_gave_them() {
        let colored = |login: &str, color: &str| User { color: Some(color.into()), ..user(login) };
        let name = |name: &str, color: &str, id: &str| NameColor {
            name: name.into(),
            color: color.into(),
            id: id.into(),
        };
        let gift = SignalKind::Membership {
            user: user("r"),
            kind: MembershipKind::Gifted { gifter: Some(colored("g", "#ff4500")), batch_id: None },
            tier: None,
            message: None,
        };
        let names = name_colors(&Signal::new(origin(), gift));
        assert_eq!(
            names,
            vec![name("R", "", "id-r"), name("G", "#ff4500", "id-g")],
            "the recipient has no colour"
        );
        let follow = SignalKind::Follow { user: user("f") };
        assert_eq!(name_colors(&Signal::new(origin(), follow)), [name("F", "", "id-f")]);
        let odd = SignalKind::Raid { from: colored("r", "red; x"), viewers: 3 };
        assert_eq!(name_colors(&Signal::new(origin(), odd)), [name("R", "", "id-r")], "only #rrggbb");
    }
}
