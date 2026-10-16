//! Moderating viewers with the broadcaster's login, from their cards in
//! Activity: timeouts, bans, warnings, shoutouts, VIPs and mods, deleting
//! a message; and what Twitch says about a viewer (their account's age,
//! whether they follow, sub, are banned, VIP or a mod).
//!
//! The login's token comes from `Auth` (kept fresh while the app runs).
//! As the broadcaster, the login moderates its own channel, so only the
//! chat's channel can be moderated: a login for another one is refused.
//! Only what's ticked on Status → Twitch is done or asked (Moderating;
//! VIPs and mods), and only what the login was given.

use protocol::config::TwitchPermissions;
use protocol::viewers::{Access, Banned, Done, MAX_TIMEOUT_SECS, Sub, ViewerAction, ViewerInfo};
use tokio::sync::watch;
use twitch_api::HelixClient;
use twitch_api::helix::{
    ClientRequestError, EmptyBody, HelixRequestDeleteError, HelixRequestGetError, HelixRequestPatchError,
    HelixRequestPostError, HelixRequestPutError,
};
use twitch_oauth2::{TwitchToken, UserToken};

use crate::services::chat_log::Someone;

#[derive(Clone)]
pub struct TwitchModeration {
    helix: HelixClient<'static, reqwest::Client>,
    tokens: watch::Receiver<Option<UserToken>>,
    chat_channel: watch::Receiver<String>,
    allowed: watch::Receiver<TwitchPermissions>,
}

type Result<T> = std::result::Result<T, String>;

impl TwitchModeration {
    pub(super) fn new(
        http: reqwest::Client,
        tokens: watch::Receiver<Option<UserToken>>,
        chat_channel: watch::Receiver<String>,
        allowed: watch::Receiver<TwitchPermissions>,
    ) -> TwitchModeration {
        TwitchModeration { helix: HelixClient::with_client(http), tokens, chat_channel, allowed }
    }

    /// Whether the chat's viewers can be moderated.
    pub fn access(&self) -> Access {
        let (allowed, granted) = match &*self.tokens.borrow() {
            None => return Access::LoggedOut,
            Some(token) if !token.login.as_str().eq_ignore_ascii_case(&self.chat_channel.borrow()) => {
                return Access::OtherChannel { login: token.login.to_string() };
            }
            Some(token) => (*self.allowed.borrow(), super::auth::granted(token.scopes())),
        };
        if !allowed.moderating {
            Access::Off
        } else if !granted.moderating {
            Access::NewLogin
        } else {
            Access::Ready { roles: allowed.roles && granted.roles }
        }
    }

    /// The token, for the chat's own channel; else why not.
    fn own_channel(&self) -> Result<UserToken> {
        let token = self.tokens.borrow().clone().ok_or_else(|| LOGGED_OUT.to_string())?;
        let channel = self.chat_channel.borrow().clone();
        if !token.login.as_str().eq_ignore_ascii_case(&channel) {
            let login = &token.login;
            return Err(format!(
                "The Twitch login is for {login}, not {channel}: log in as {channel} on Status."
            ));
        }
        Ok(token)
    }

    /// The token, if it can moderate the chat's channel; else why not.
    fn token(&self) -> Result<UserToken> {
        let token = self.own_channel()?;
        match self.access() {
            Access::Ready { .. } => Ok(token),
            Access::Off => Err("Moderating is off: tick it on Status → Twitch.".into()),
            Access::NewLogin => Err("Moderating needs a new login: Status → Twitch.".into()),
            Access::LoggedOut | Access::OtherChannel { .. } => Err(LOGGED_OUT.into()),
        }
    }

    /// The Twitch account with this id, else this login, if there is one.
    /// Any login will do for looking up.
    pub async fn look_up(&self, id: Option<&str>, login: &str) -> Result<Option<Someone>> {
        let token = self.tokens.borrow().clone().ok_or_else(|| LOGGED_OUT.to_string())?;
        let user = match id {
            Some(id) => self.helix.get_user_from_id(id, &token).await,
            None => self.helix.get_user_from_login(login, &token).await,
        }
        .map_err(problem)?;
        Ok(user.map(|user| Someone {
            id: user.id.to_string(),
            login: user.login.to_string(),
            name: user.display_name.to_string(),
        }))
    }

    /// Whether Twitch can be asked about the chat's viewers: logged in, as
    /// the chat's channel.
    pub fn can_ask(&self) -> bool {
        self.own_channel().is_ok()
    }

    /// What Twitch says about viewer `user`, as far as what's ticked and
    /// the login allow: the account any login may see; whether they follow
    /// with Follows; sub, banned, VIP and mod with Moderating. Only the
    /// account itself is needed; what can't be asked is left out (and
    /// logged).
    pub async fn info(&self, user: &str) -> Result<ViewerInfo> {
        let token = self.own_channel()?;
        let may = self.allowed.borrow().and(super::auth::granted(token.scopes()));
        let channel = token.user_id.as_str();
        let helix = &self.helix;
        let account = helix.get_user_from_id(user, &token);
        let follow = async {
            if !may.follows {
                return Ok(None);
            }
            let request = twitch_api::helix::channels::GetChannelFollowersRequest::broadcaster_id(channel)
                .user_id(user);
            helix.req_get(request, &token).await.map(|response| response.data.into_iter().next())
        };
        let ids = [twitch_api::types::UserId::from(user.to_string())];
        let sub = async {
            if !may.moderating {
                return Ok(None);
            }
            let request =
                twitch_api::helix::subscriptions::GetBroadcasterSubscriptionsRequest::broadcaster_id(channel)
                    .subscriber(&ids[..]);
            helix.req_get(request, &token).await.map(|response| response.data.into_iter().next())
        };
        let ban = async { if may.moderating { self.banned(user, &token).await } else { Ok(None) } };
        let vip = async {
            if !may.moderating {
                return Ok(false);
            }
            let request =
                twitch_api::helix::channels::GetVipsRequest::broadcaster_id(channel).user_ids(&ids[..]);
            helix.req_get(request, &token).await.map(|response| !response.data.is_empty())
        };
        let moderator = async {
            if !may.moderating {
                return Ok(false);
            }
            let request = twitch_api::helix::moderation::GetModeratorsRequest::broadcaster_id(channel)
                .user_ids(&ids[..]);
            helix.req_get(request, &token).await.map(|response| !response.data.is_empty())
        };
        let (account, follow, sub, ban, vip, moderator) =
            tokio::join!(account, follow, sub, ban, vip, moderator);
        let account = account.map_err(problem)?.ok_or("Twitch has no such account any more.")?;
        let partly = |what: &str, error: &dyn std::fmt::Display| {
            tracing::warn!("couldn't ask Twitch whether a viewer {what}: {error}");
        };
        let following_since_ms = match follow {
            Ok(follower) => follower.and_then(|follower| ms(follower.followed_at.as_str())),
            Err(error) => {
                partly("follows", &error);
                None
            }
        };
        let sub = match sub {
            Ok(sub) => sub.map(|sub| Sub {
                tier: match sub.tier {
                    twitch_api::types::SubscriptionTier::Tier2 => 2,
                    twitch_api::types::SubscriptionTier::Tier3 => 3,
                    _ => 1,
                },
                gifted: sub.is_gift,
            }),
            Err(error) => {
                partly("subscribes", &error);
                None
            }
        };
        let banned = ban.unwrap_or_else(|error| {
            partly("is banned", &error);
            None
        });
        let vip = vip.unwrap_or_else(|error| {
            partly("is a VIP", &error);
            false
        });
        let moderator = moderator.unwrap_or_else(|error| {
            partly("is a mod", &error);
            false
        });
        Ok(ViewerInfo {
            created_ms: ms(account.created_at.as_str()).unwrap_or_default(),
            following_since_ms,
            sub,
            banned,
            vip,
            moderator,
        })
    }

    /// Whether `user` is banned or timed out, and till when.
    async fn banned(&self, user: &str, token: &UserToken) -> Result<Option<Banned>> {
        let ids = [twitch_api::types::UserId::from(user.to_string())];
        let request =
            twitch_api::helix::moderation::GetBannedUsersRequest::broadcaster_id(token.user_id.as_str())
                .users(&ids[..]);
        let response = self.helix.req_get(request, token).await.map_err(problem)?;
        Ok(response
            .data
            .first()
            .map(|ban| Banned { until_ms: ban.expires_at.as_ref().and_then(|at| ms(at.as_str())) }))
    }

    /// Does `action` to viewer `user`. What to note in their log, when chat
    /// won't say (it does for timeouts, bans and deleted messages).
    pub async fn act(&self, user: &str, action: &ViewerAction) -> Result<Option<Done>> {
        let token = self.token()?;
        let roles = matches!(
            action,
            ViewerAction::AddVip | ViewerAction::RemoveVip | ViewerAction::AddMod | ViewerAction::RemoveMod
        );
        if roles && self.access() != (Access::Ready { roles: true }) {
            let ticked = self.allowed.borrow().roles;
            return Err(if ticked {
                "VIPs and mods need a new login: Status → Twitch.".into()
            } else {
                "VIPs and mods are off: tick them on Status → Twitch.".into()
            });
        }
        let channel = token.user_id.as_str();
        let helix = &self.helix;
        Ok(match action {
            ViewerAction::Timeout { secs } => {
                let secs = (*secs).clamp(1, MAX_TIMEOUT_SECS);
                helix.ban_user(user, "", Some(secs), channel, channel, &token).await.map_err(problem)?;
                None
            }
            ViewerAction::Ban => {
                helix.ban_user(user, "", None, channel, channel, &token).await.map_err(problem)?;
                None
            }
            ViewerAction::Unban => {
                let timeout = self.banned(user, &token).await?.is_some_and(|ban| ban.until_ms.is_some());
                helix.unban_user(user, channel, channel, &token).await.map_err(problem)?;
                Some(if timeout { Done::TimeoutRemoved } else { Done::Unbanned })
            }
            ViewerAction::Warn { reason } => {
                let reason = reason.trim();
                if reason.is_empty() {
                    return Err("A warning needs a reason.".into());
                }
                let request = twitch_api::helix::moderation::WarnChatUserRequest::new(channel, channel);
                let body = twitch_api::helix::moderation::WarnChatUserBody::new(user, reason);
                helix.req_post(request, body, &token).await.map_err(problem)?;
                Some(Done::Warned)
            }
            ViewerAction::Shoutout => {
                let request = twitch_api::helix::chat::SendAShoutoutRequest::new(channel, user, channel);
                helix.req_post(request, EmptyBody, &token).await.map_err(problem)?;
                Some(Done::Shoutout)
            }
            ViewerAction::AddVip => {
                helix.add_channel_vip(channel, user, &token).await.map_err(problem)?;
                Some(Done::VipAdded)
            }
            ViewerAction::RemoveVip => {
                helix.remove_channel_vip(channel, user, &token).await.map_err(problem)?;
                Some(Done::VipRemoved)
            }
            ViewerAction::AddMod => {
                helix.add_channel_moderator(channel, user, &token).await.map_err(problem)?;
                Some(Done::ModAdded)
            }
            ViewerAction::RemoveMod => {
                helix.remove_channel_moderator(channel, user, &token).await.map_err(problem)?;
                Some(Done::ModRemoved)
            }
            ViewerAction::Delete { message } => {
                helix
                    .delete_chat_message(channel, channel, message.as_str(), &token)
                    .await
                    .map_err(problem)?;
                None
            }
        })
    }
}

const LOGGED_OUT: &str = "Log in with Twitch on Status to moderate.";

/// Unix ms from Twitch's RFC 3339 time.
fn ms(at: &str) -> Option<u64> {
    let at = chrono::DateTime::parse_from_rfc3339(at).ok()?;
    u64::try_from(at.timestamp_millis()).ok()
}

/// Why a request didn't work, for people: Twitch's own words when it
/// refused.
fn problem(error: ClientRequestError<reqwest::Error>) -> String {
    let refused = match &error {
        ClientRequestError::HelixRequestGetError(HelixRequestGetError::Error { message, .. })
        | ClientRequestError::HelixRequestPostError(HelixRequestPostError::Error { message, .. })
        | ClientRequestError::HelixRequestDeleteError(HelixRequestDeleteError::Error { message, .. })
        | ClientRequestError::HelixRequestPutError(HelixRequestPutError::Error { message, .. })
        | ClientRequestError::HelixRequestPatchError(HelixRequestPatchError::Error { message, .. }) => {
            Some(message.clone())
        }
        _ => None,
    };
    match refused {
        Some(message) => {
            tracing::info!("Twitch refused: {message}");
            refused_text(&message)
        }
        None => {
            tracing::warn!("a request to Twitch failed: {error}");
            match error {
                ClientRequestError::RequestError(_) => "Couldn't reach Twitch. Try again.".into(),
                _ => "Twitch's answer couldn't be read.".into(),
            }
        }
    }
}

/// Twitch's message, as a sentence.
fn refused_text(message: &str) -> String {
    let message = message.trim();
    if message.is_empty() {
        return "Twitch refused.".into();
    }
    if message.ends_with(['.', '!', '?']) { message.to_string() } else { format!("{message}.") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twitch_times() {
        assert_eq!(ms("2026-10-16T18:00:00Z"), Some(1_792_173_600_000));
        assert_eq!(ms("2026-10-16T18:00:00.5Z"), Some(1_792_173_600_500));
        assert_eq!(ms("soon"), None);
    }

    #[test]
    fn refusals_read_as_sentences() {
        assert_eq!(
            refused_text("The user specified in the user_id field may not be banned"),
            "The user specified in the user_id field may not be banned."
        );
        assert_eq!(refused_text("Already a VIP."), "Already a VIP.");
        assert_eq!(refused_text(" "), "Twitch refused.");
    }
}
