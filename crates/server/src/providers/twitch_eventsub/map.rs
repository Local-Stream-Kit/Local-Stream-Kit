//! EventSub events → platform-neutral `Input`s.

use twitch_api::common::hypetrain::HypeTrainType;
use twitch_api::eventsub::channel::{
    ChannelFollowV2Payload, ChannelPointsCustomRewardRedemptionAddV1Payload,
};
use twitch_api::eventsub::event::Event;
use twitch_api::eventsub::{EventSubscription, Message, Payload};
use twitch_api::types::{Timestamp, UserId, UserName};

use crate::model::{
    ChannelRef, HypeTrain, HypeTrainChange, Input, InputKind, Origin, PlatformEvent, RewardRef, Roles, User,
};

pub const PROVIDER: &str = "twitch_eventsub";
const PLATFORM: &str = "twitch";

/// The `Input` for a notification, or `None` for events this provider doesn't
/// subscribe to. `trains` tells a hype train's level-ups, which Twitch
/// doesn't send as such.
#[allow(deprecated)] // version 1 of the hype train events: the Twitch CLI's
pub fn input(event: &Event, at_ms: u64, trains: &mut HypeTrains) -> Option<Input> {
    // Twitch sends hype trains as version 2. The Twitch CLI only makes
    // version 1, which has no kind of train and no shared trains.
    let train = match event {
        Event::ChannelPointsCustomRewardRedemptionAddV1(payload) => {
            return notification(payload).map(|redemption| redemption_input(redemption, at_ms));
        }
        Event::ChannelFollowV2(payload) => {
            return notification(payload).map(|follow| follow_input(follow, at_ms));
        }
        Event::ChannelHypeTrainBeginV2(payload) => notification(payload).map(|event| TrainFields {
            running: Some((event.progress, event.goal, &event.expires_at)),
            kind: Some(&event.type_),
            shared: event.is_shared_train,
            ..TrainFields::new(
                Phase::Begin,
                event.id.as_str(),
                &event.broadcaster_user_id,
                &event.broadcaster_user_login,
            )
            .counts(event.level, event.total)
        }),
        Event::ChannelHypeTrainProgressV2(payload) => notification(payload).map(|event| TrainFields {
            running: Some((event.progress, event.goal, &event.expires_at)),
            kind: Some(&event.type_),
            shared: event.is_shared_train,
            ..TrainFields::new(
                Phase::Progress,
                event.id.as_str(),
                &event.broadcaster_user_id,
                &event.broadcaster_user_login,
            )
            .counts(event.level, event.total)
        }),
        Event::ChannelHypeTrainEndV2(payload) => notification(payload).map(|event| TrainFields {
            kind: Some(&event.type_),
            shared: event.is_shared_train,
            ..TrainFields::new(
                Phase::End,
                event.id.as_str(),
                &event.broadcaster_user_id,
                &event.broadcaster_user_login,
            )
            .counts(event.level, event.total)
        }),
        Event::ChannelHypeTrainBeginV1(payload) => notification(payload).map(|event| TrainFields {
            running: Some((event.progress, event.goal, &event.expires_at)),
            ..TrainFields::new(
                Phase::Begin,
                event.id.as_str(),
                &event.broadcaster_user_id,
                &event.broadcaster_user_login,
            )
            .counts(event.level, event.total)
        }),
        Event::ChannelHypeTrainProgressV1(payload) => notification(payload).map(|event| TrainFields {
            running: Some((event.progress, event.goal, &event.expires_at)),
            ..TrainFields::new(
                Phase::Progress,
                event.id.as_str(),
                &event.broadcaster_user_id,
                &event.broadcaster_user_login,
            )
            .counts(event.level, event.total)
        }),
        Event::ChannelHypeTrainEndV1(payload) => notification(payload).map(|event| {
            TrainFields::new(
                Phase::End,
                event.id.as_str(),
                &event.broadcaster_user_id,
                &event.broadcaster_user_login,
            )
            .counts(event.level, event.total)
        }),
        _ => None,
    }?;
    Some(train.input(trains, at_ms))
}

/// The event itself, from a notification.
fn notification<E: EventSubscription + Clone>(payload: &Payload<E>) -> Option<&E::Payload> {
    match &payload.message {
        Message::Notification(event) => Some(event),
        _ => None,
    }
}

/// Which hype train event it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Begin,
    Progress,
    End,
}

/// What the versions of the three hype train events have in common.
struct TrainFields<'a> {
    phase: Phase,
    id: &'a str,
    broadcaster_id: &'a UserId,
    broadcaster_login: &'a UserName,
    level: i64,
    total: i64,
    /// Progress, goal and when it runs out: not in an end event.
    running: Option<(i64, i64, &'a Timestamp)>,
    /// Version 2 only: regular, a Golden Kappa train, a Treasure train.
    kind: Option<&'a HypeTrainType>,
    shared: bool,
}

impl<'a> TrainFields<'a> {
    fn new(phase: Phase, id: &'a str, broadcaster_id: &'a UserId, broadcaster_login: &'a UserName) -> Self {
        TrainFields {
            phase,
            id,
            broadcaster_id,
            broadcaster_login,
            level: 0,
            total: 0,
            running: None,
            kind: None,
            shared: false,
        }
    }

    fn counts(self, level: i64, total: i64) -> Self {
        TrainFields { level, total, ..self }
    }

    fn input(self, trains: &mut HypeTrains, at_ms: u64) -> Input {
        let level = u32::try_from(self.level.max(0)).unwrap_or(u32::MAX);
        let change = match self.phase {
            Phase::Begin => {
                trains.started(self.id, level);
                HypeTrainChange::Started
            }
            Phase::Progress => HypeTrainChange::Progressed { level_up: trains.progressed(self.id, level) },
            Phase::End => {
                trains.ended(self.id);
                HypeTrainChange::Ended
            }
        };
        let points = |value: i64| u64::try_from(value).unwrap_or(0);
        let (progress, goal, seconds_left) = match self.running {
            Some((progress, goal, expires_at)) => {
                let left = unix_ms(expires_at.as_str()).map_or(0, |end| end.saturating_sub(at_ms) / 1000);
                (points(progress), points(goal), u32::try_from(left).unwrap_or(u32::MAX))
            }
            None => (0, 0, 0),
        };
        // As Twitch names it ("golden_kappa"), new kinds included.
        let kind = self
            .kind
            .and_then(|kind| serde_json::to_value(kind).ok())
            .and_then(|kind| kind.as_str().map(str::to_string))
            .unwrap_or_else(|| "regular".to_string());
        Input {
            at_ms,
            origin: Origin {
                provider: PROVIDER.to_string(),
                platform: PLATFORM.to_string(),
                channel: ChannelRef {
                    id: self.broadcaster_id.to_string(),
                    name: self.broadcaster_login.to_string(),
                },
            },
            kind: InputKind::Event(PlatformEvent::HypeTrain {
                change,
                train: HypeTrain {
                    id: self.id.to_string(),
                    level,
                    progress,
                    goal,
                    total: points(self.total),
                    kind,
                    shared: self.shared,
                    seconds_left,
                },
            }),
        }
    }
}

/// The current hype train's id and the highest level seen, to tell a
/// level-up: Twitch only sends progress, with the level the train is at.
/// Kept by the provider, which stays connected while the pipeline is
/// rebuilt on every change to the config.
#[derive(Debug, Default)]
pub struct HypeTrains {
    current: Option<(String, u32)>,
}

impl HypeTrains {
    fn started(&mut self, id: &str, level: u32) {
        self.current = Some((id.to_string(), level));
    }

    /// Whether this progress took the train up a level.
    fn progressed(&mut self, id: &str, level: u32) -> bool {
        match &mut self.current {
            Some((known, highest)) if known == id => {
                let up = level > *highest;
                *highest = (*highest).max(level);
                up
            }
            // Its start came before a restart (or was missed): nothing to
            // compare with.
            _ => {
                self.started(id, level);
                false
            }
        }
    }

    fn ended(&mut self, id: &str) {
        if self.current.as_ref().is_some_and(|(known, _)| known == id) {
            self.current = None;
        }
    }
}

/// An RFC 3339 time, as Twitch writes them ("2020-07-15T17:16:03.17106713Z"),
/// in Unix milliseconds.
fn unix_ms(timestamp: &str) -> Option<u64> {
    let (date, time) = timestamp.split_once(['T', 't'])?;
    let number = |text: &str| text.parse::<i64>().ok();
    let mut date = date.split('-');
    let (year, month, day) = (number(date.next()?)?, number(date.next()?)?, number(date.next()?)?);
    let (clock, offset_secs) = match time.strip_suffix(['Z', 'z']) {
        Some(clock) => (clock, 0),
        None => {
            let (clock, offset) = time.split_at(time.rfind(['+', '-'])?);
            let (hours, minutes) = offset[1..].split_once(':')?;
            let seconds = number(hours)? * 3600 + number(minutes)? * 60;
            (clock, if offset.starts_with('-') { -seconds } else { seconds })
        }
    };
    let (clock, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut clock = clock.split(':');
    let (hour, minute, second) = (number(clock.next()?)?, number(clock.next()?)?, number(clock.next()?)?);
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let millis = if fraction.is_empty() { 0 } else { number(&format!("{fraction:0<3}")[..3])? };
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs;
    u64::try_from(seconds * 1000 + millis).ok()
}

fn follow_input(follow: &ChannelFollowV2Payload, at_ms: u64) -> Input {
    Input {
        at_ms,
        origin: Origin {
            provider: PROVIDER.to_string(),
            platform: PLATFORM.to_string(),
            channel: ChannelRef {
                id: follow.broadcaster_user_id.to_string(),
                name: follow.broadcaster_user_login.to_string(),
            },
        },
        kind: InputKind::Event(PlatformEvent::Follow {
            user: User {
                id: follow.user_id.to_string(),
                name: follow.user_login.to_string(),
                display_name: follow.user_name.to_string(),
                roles: Roles::default(),
                ..Default::default()
            },
        }),
    }
}

fn redemption_input(redemption: &ChannelPointsCustomRewardRedemptionAddV1Payload, at_ms: u64) -> Input {
    Input {
        at_ms,
        origin: Origin {
            provider: PROVIDER.to_string(),
            platform: PLATFORM.to_string(),
            channel: ChannelRef {
                id: redemption.broadcaster_user_id.to_string(),
                name: redemption.broadcaster_user_login.to_string(),
            },
        },
        kind: InputKind::Event(PlatformEvent::Redemption {
            user: User {
                id: redemption.user_id.to_string(),
                name: redemption.user_login.to_string(),
                display_name: redemption.user_name.to_string(),
                // EventSub doesn't say; badges only come with chat messages.
                roles: Roles::default(),
                ..Default::default()
            },
            reward: RewardRef {
                id: redemption.reward.id.to_string(),
                title: Some(redemption.reward.title.clone()),
            },
            input: Some(redemption.user_input.clone()).filter(|text| !text.is_empty()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use twitch_api::eventsub::event::websocket::EventsubWebsocketData;

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/eventsub/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    /// The `Input` for a notification, with `trains` remembering hype
    /// trains.
    fn read(frame: &str, at_ms: u64, trains: &mut HypeTrains) -> Input {
        let EventsubWebsocketData::Notification { payload, .. } = Event::parse_websocket(frame).unwrap()
        else {
            panic!("not a notification");
        };
        input(&payload, at_ms, trains).expect("an input")
    }

    fn notification(name: &str) -> Input {
        read(&fixture(name), 7, &mut HypeTrains::default())
    }

    fn hype_train(input: Input) -> (HypeTrainChange, HypeTrain) {
        let InputKind::Event(PlatformEvent::HypeTrain { change, train }) = input.kind else {
            panic!("a hype train: {input:?}")
        };
        (change, train)
    }

    #[test]
    fn a_hype_train_from_start_to_end() {
        // Twitch's examples: it started at 17:16:03.171 and runs out at
        // 17:16:11.171.
        let started_ms = 1_594_833_363_171;
        let mut trains = HypeTrains::default();
        let begin = read(&fixture("hype_train_begin.json"), started_ms, &mut trains);
        assert_eq!(begin.origin.provider, "twitch_eventsub");
        assert_eq!(begin.origin.channel, ChannelRef { id: "1337".into(), name: "cool_user".into() });
        let (change, train) = hype_train(begin);
        assert_eq!(change, HypeTrainChange::Started);
        assert_eq!(
            train,
            HypeTrain {
                id: "1b0AsbInCHZW2SQFQkCzqN07Ib2".into(),
                level: 1,
                progress: 137,
                goal: 500,
                total: 137,
                kind: "regular".into(),
                shared: false,
                seconds_left: 8,
            }
        );

        let progress = fixture("hype_train_progress.json");
        let at_level = |level: u32| progress.replace(r#""level": 1"#, &format!(r#""level": {level}"#));
        let mut next = |frame: &str| hype_train(read(frame, started_ms + 1000, &mut trains));
        let (change, train) = next(&progress);
        assert_eq!(change, HypeTrainChange::Progressed { level_up: false });
        assert_eq!((train.level, train.progress, train.goal, train.seconds_left), (1, 137, 500, 7));
        assert_eq!(next(&at_level(2)).0, HypeTrainChange::Progressed { level_up: true });
        assert_eq!(next(&at_level(2)).0, HypeTrainChange::Progressed { level_up: false });
        // Out of order: a lower level isn't a level-up, nor is the level
        // it was already at.
        assert_eq!(next(&at_level(1)).0, HypeTrainChange::Progressed { level_up: false });
        assert_eq!(next(&at_level(2)).0, HypeTrainChange::Progressed { level_up: false });
        assert_eq!(next(&at_level(4)).0, HypeTrainChange::Progressed { level_up: true });

        let (change, train) = next(&fixture("hype_train_end.json"));
        assert_eq!(change, HypeTrainChange::Ended);
        assert_eq!(
            (train.level, train.total, train.progress, train.goal, train.seconds_left),
            (1, 137, 0, 0, 0)
        );
        assert!(trains.current.is_none());
    }

    #[test]
    fn a_hype_train_seen_first_in_the_middle_has_no_level_up_yet() {
        // After a restart: nothing to compare the first progress with.
        let mut trains = HypeTrains::default();
        let progress = fixture("hype_train_progress.json").replace(r#""level": 1"#, r#""level": 3"#);
        assert_eq!(
            hype_train(read(&progress, 0, &mut trains)).0,
            HypeTrainChange::Progressed { level_up: false }
        );
        let higher = progress.replace(r#""level": 3"#, r#""level": 4"#);
        assert_eq!(
            hype_train(read(&higher, 0, &mut trains)).0,
            HypeTrainChange::Progressed { level_up: true }
        );
        // A new train starts afresh.
        let other = higher.replace("1b0AsbInCHZW2SQFQkCzqN07Ib2", "another");
        assert_eq!(
            hype_train(read(&other, 0, &mut trains)).0,
            HypeTrainChange::Progressed { level_up: false }
        );
    }

    #[test]
    fn special_and_shared_hype_trains() {
        let begin = fixture("hype_train_begin.json");
        let as_kind = |kind: &str| {
            let frame = begin
                .replace(r#""type": "regular""#, &format!(r#""type": "{kind}""#))
                .replace(r#""is_shared_train": false"#, r#""is_shared_train": true"#);
            hype_train(read(&frame, 0, &mut HypeTrains::default())).1
        };
        let golden = as_kind("golden_kappa");
        assert_eq!(
            (golden.kind.as_str(), golden.shared, golden.name()),
            ("golden_kappa", true, "Golden Kappa train")
        );
        assert_eq!(as_kind("treasure").name(), "Treasure train");
        // A kind Twitch adds later passes through, and reads as a hype train.
        let new = as_kind("rocket");
        assert_eq!((new.kind.as_str(), new.name()), ("rocket", "Hype train"));
        // Long gone: no time left, not a negative one.
        assert_eq!(hype_train(read(&begin, u64::MAX / 2, &mut HypeTrains::default())).1.seconds_left, 0);
    }

    #[test]
    fn the_twitch_cli_makes_version_1_hype_trains() {
        // twitch-cli 1.1.24's begin, as its mock server sent it: started
        // 17:15:41.606, runs out 5 minutes later.
        let input = read(&fixture("hype_train_begin_v1.json"), 1_791_825_341_606, &mut HypeTrains::default());
        assert_eq!(
            input.origin.channel,
            ChannelRef { id: "46441857".into(), name: "testBroadcaster".into() }
        );
        let (change, train) = hype_train(input);
        assert_eq!(change, HypeTrainChange::Started);
        assert_eq!(
            (
                train.level,
                train.progress,
                train.goal,
                train.total,
                train.kind.as_str(),
                train.shared,
                train.seconds_left
            ),
            (4, 19, 73_702, 19, "regular", false, 300)
        );
    }

    #[test]
    fn twitch_times() {
        assert_eq!(unix_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix_ms("2020-07-15T17:16:03.17106713Z"), Some(1_594_833_363_171));
        assert_eq!(unix_ms("2026-10-12T16:23:58.467917Z"), Some(1_791_822_238_467));
        assert_eq!(unix_ms("2000-02-29T23:59:59Z"), Some(951_868_799_000), "a leap day");
        assert_eq!(unix_ms("2020-07-15T19:16:03.171+02:00"), Some(1_594_833_363_171));
        assert_eq!(unix_ms("2020-07-15T15:16:03.1-02:00"), Some(1_594_833_363_100));
        assert_eq!(unix_ms("yesterday"), None);
        assert_eq!(unix_ms("2020-07-15T17:16:03.x1Z"), None);
        assert_eq!(unix_ms("1969-12-31T23:59:59Z"), None, "before 1970");
    }

    #[test]
    fn follow() {
        let input = notification("follow.json");
        assert_eq!(input.origin.channel, ChannelRef { id: "1337".into(), name: "cool_user".into() });
        let InputKind::Event(PlatformEvent::Follow { user }) = input.kind else { panic!("a follow") };
        assert_eq!(
            (user.id.as_str(), user.name.as_str(), user.display_name.as_str()),
            ("1234", "cool_user2", "Cool_User2")
        );
    }

    #[test]
    fn redemption_with_text() {
        let input = notification("redemption.json");
        assert_eq!(input.at_ms, 7);
        assert_eq!(input.origin.provider, "twitch_eventsub");
        assert_eq!(input.origin.channel, ChannelRef { id: "1337".into(), name: "cool_user".into() });
        let InputKind::Event(PlatformEvent::Redemption { user, reward, input }) = input.kind else {
            panic!()
        };
        assert_eq!(
            (user.id.as_str(), user.name.as_str(), user.display_name.as_str()),
            ("9001", "cooler_user", "Cooler_User")
        );
        assert_eq!(
            reward,
            RewardRef { id: "92af127c-7326-4483-a52b-b0da0be61c01".into(), title: Some("title".into()) }
        );
        assert_eq!(input.as_deref(), Some("pogchamp"));
    }

    #[test]
    fn empty_text_is_no_input() {
        let frame = fixture("redemption.json").replace(r#""user_input": "pogchamp""#, r#""user_input": """#);
        let EventsubWebsocketData::Notification { payload, .. } = Event::parse_websocket(&frame).unwrap()
        else {
            panic!("not a notification");
        };
        let InputKind::Event(PlatformEvent::Redemption { input, .. }) =
            super::input(&payload, 0, &mut HypeTrains::default()).unwrap().kind
        else {
            panic!()
        };
        assert_eq!(input, None);
    }
}
