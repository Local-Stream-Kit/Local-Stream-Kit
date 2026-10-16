//! The Activity tab's viewer cards: finding a viewer, their chat as the
//! app saw it (its chat log), what Twitch says about them, and the
//! moderator's actions (timeout, ban, warn, shoutout, VIP, mod, deleting a
//! message).

use serde::{Deserialize, Serialize};

use crate::AlertPart;
use crate::chat::ChatBadge;

/// A card's log comes this many lines at a time ("Show older").
pub const LOG_PAGE: usize = 50;

/// Find a viewer lists this many matches.
pub const FOUND_LIMIT: usize = 8;

/// Twitch's longest timeout: two weeks.
pub const MAX_TIMEOUT_SECS: u32 = 14 * 24 * 60 * 60;

/// Twitch doesn't delete messages older than this.
pub const DELETABLE_FOR_MS: u64 = 6 * 60 * 60 * 1000;

/// The Timeout menu's lengths, in seconds.
pub const TIMEOUTS: [u32; 5] = [60, 10 * 60, 60 * 60, 24 * 60 * 60, 7 * 24 * 60 * 60];

/// A viewer Find a viewer offers: someone the chat log has seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FoundViewer {
    /// The platform's user id.
    pub id: String,
    /// As shown in chat ("MossyKnight").
    pub name: String,
    /// Their login ("mossyknight").
    pub login: String,
    /// `#rrggbb`, when they picked one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// When they last wrote (Unix ms).
    pub last_seen_ms: u64,
}

/// One viewer's card: who they are, and their latest log lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewerCard {
    /// The platform's user id.
    pub id: String,
    pub login: String,
    /// As shown in chat.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// The badges on their latest message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub badges: Vec<ChatBadge>,
    /// Their messages in the log (moderation lines not counted).
    pub messages: usize,
    /// The newest lines of their log, oldest first.
    pub lines: Vec<LogLine>,
    /// How many lines come before `lines`.
    pub older: usize,
    /// Whether they can be moderated from here.
    pub access: Access,
}

/// A line in a viewer's log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    /// Unix ms.
    pub at_ms: u64,
    #[serde(flatten)]
    pub kind: LineKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "line", rename_all = "snake_case")]
pub enum LineKind {
    /// Something they wrote.
    Message {
        /// The platform's message id (for deleting it); empty when it had
        /// none.
        id: String,
        text: String,
        /// `text` with its emotes.
        parts: Vec<AlertPart>,
        /// Taken out of the chat by a moderator.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        deleted: bool,
        /// Written as an action (`/me waves`).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        action: bool,
    },
    /// Timed out, by anyone (chat says so).
    TimedOut { secs: u64 },
    /// Banned, by anyone.
    Banned,
    /// What was done from the panel and chat doesn't say.
    Done { done: Done },
}

/// What a moderator did from the panel that chat doesn't announce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Done {
    Unbanned,
    TimeoutRemoved,
    Warned,
    Shoutout,
    VipAdded,
    VipRemoved,
    ModAdded,
    ModRemoved,
}

impl Done {
    /// How its log line reads.
    pub fn label(self) -> &'static str {
        match self {
            Done::Unbanned => "Unbanned",
            Done::TimeoutRemoved => "Timeout removed",
            Done::Warned => "Warned",
            Done::Shoutout => "Shoutout",
            Done::VipAdded => "Made VIP",
            Done::VipRemoved => "VIP removed",
            Done::ModAdded => "Made a mod",
            Done::ModRemoved => "Mod removed",
        }
    }
}

/// Whether the viewers can be moderated from the panel: it takes the
/// broadcaster's Twitch login, for the channel chat shows, with Moderating
/// ticked on Status → Twitch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "access", rename_all = "snake_case")]
pub enum Access {
    /// `roles`: VIPs and mods can be made and taken back too.
    Ready {
        #[serde(default)]
        roles: bool,
    },
    /// Not logged in to Twitch.
    LoggedOut,
    /// Logged in as `login`, but chat is another channel's.
    OtherChannel { login: String },
    /// Moderating isn't ticked.
    Off,
    /// Ticked, but the login was made without it: a new login gives it.
    NewLogin,
}

/// What Twitch says about a viewer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerInfo {
    /// When their account was made (Unix ms).
    pub created_ms: u64,
    /// Since when they follow the channel; `None`: they don't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub following_since_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<Sub>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banned: Option<Banned>,
    #[serde(default)]
    pub vip: bool,
    #[serde(default)]
    pub moderator: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sub {
    /// 1, 2 or 3.
    pub tier: u8,
    #[serde(default)]
    pub gifted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Banned {
    /// When a timeout ends (Unix ms); `None`: banned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<u64>,
}

/// What a moderator does to a viewer from their card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ViewerAction {
    /// For `secs`, at most [`MAX_TIMEOUT_SECS`].
    Timeout {
        secs: u32,
    },
    Ban,
    /// Ends a ban or a timeout.
    Unban,
    /// Twitch's warning: they read `reason` and must acknowledge it before
    /// they can chat again.
    Warn {
        reason: String,
    },
    Shoutout,
    AddVip,
    RemoveVip,
    AddMod,
    RemoveMod,
    /// Takes one of their messages out of the chat.
    Delete {
        message: String,
    },
}

impl ViewerAction {
    /// What it does, for "Couldn't … MossyKnight: …".
    pub fn verb(&self) -> &'static str {
        match self {
            ViewerAction::Timeout { .. } => "time out",
            ViewerAction::Ban => "ban",
            ViewerAction::Unban => "unban",
            ViewerAction::Warn { .. } => "warn",
            ViewerAction::Shoutout => "give a shoutout to",
            ViewerAction::AddVip => "make VIP",
            ViewerAction::RemoveVip => "remove VIP from",
            ViewerAction::AddMod => "make a mod of",
            ViewerAction::RemoveMod => "remove mod from",
            ViewerAction::Delete { .. } => "delete the message of",
        }
    }
}

/// "1 min", "10 min", "1 hour", "3 days", "2 weeks": a timeout's length.
pub fn duration(secs: u64) -> String {
    let plural = |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        3600..86_400 => plural(secs / 3600, "hour", "hours"),
        _ if secs.is_multiple_of(7 * 86_400) => plural(secs / (7 * 86_400), "week", "weeks"),
        _ => plural(secs / 86_400, "day", "days"),
    }
}

/// Whether `login` can be a Twitch login (letters, digits and `_`, at most
/// 25): only such a name is looked up or put in a link.
pub fn is_login(login: &str) -> bool {
    !login.is_empty() && login.len() <= 25 && login.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Twitch's own viewer card for `login` in `channel`'s chat, which shows
/// its moderators all of their messages there.
pub fn twitch_card_url(channel: &str, login: &str) -> String {
    format!(
        "https://www.twitch.tv/popout/{}/viewercard/{}",
        channel.to_ascii_lowercase(),
        login.to_ascii_lowercase()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        let read: Vec<String> = TIMEOUTS.iter().map(|secs| duration(u64::from(*secs))).collect();
        assert_eq!(read, ["1 min", "10 min", "1 hour", "1 day", "1 week"]);
        assert_eq!(duration(30), "30 s");
        assert_eq!(duration(10_800), "3 hours");
        assert_eq!(duration(u64::from(MAX_TIMEOUT_SECS)), "2 weeks");
        assert_eq!(duration(3 * 86_400), "3 days");
    }

    #[test]
    fn logins() {
        assert!(is_login("mossy_knight42"));
        assert!(!is_login("nite owl"));
        assert!(!is_login("../x"));
        assert!(!is_login(""));
        assert_eq!(
            twitch_card_url("Vedal987", "MossyKnight"),
            "https://www.twitch.tv/popout/vedal987/viewercard/mossyknight"
        );
    }

    #[test]
    fn shapes() {
        let line = LogLine { at_ms: 5, kind: LineKind::TimedOut { secs: 600 } };
        assert_eq!(
            serde_json::to_value(&line).unwrap(),
            serde_json::json!({ "at_ms": 5, "line": "timed_out", "secs": 600 })
        );
        let done = LogLine { at_ms: 5, kind: LineKind::Done { done: Done::VipAdded } };
        let back: LogLine = serde_json::from_str(&serde_json::to_string(&done).unwrap()).unwrap();
        assert_eq!(back, done);
        let action = serde_json::to_value(ViewerAction::Timeout { secs: 60 }).unwrap();
        assert_eq!(action, serde_json::json!({ "action": "timeout", "secs": 60 }));
    }
}
