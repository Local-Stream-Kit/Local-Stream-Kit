//! Twitch's own emotes, listed by its API: the global ones (Kappa, LUL, …)
//! and each chat channel's (sub, follower and bits emotes), and the
//! channel's cheer prefixes (`Cheer`, `BibleThump`, its own), so cheer words
//! can be left out of cheer messages, and the chat badges (global and the
//! channel's own) for the chat display. The API needs a login, so this runs
//! alongside EventSub with the broadcaster's token. Without one, Twitch
//! emotes are only known once they're seen in chat, and cheer words stay.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;
use twitch_api::HelixClient;
use twitch_api::helix::bits::GetCheermotesRequest;
use twitch_api::helix::chat::{BadgeSet, GetChannelChatBadgesRequest, GetGlobalChatBadgesRequest};
use twitch_oauth2::UserToken;

use crate::model::EmoteRef;
use crate::providers::twitch_chat::{PLATFORM, emote_image_url};
use crate::services::emotes::{BadgeInfo, EmoteInfo, EmoteRegistry};

/// Emote lists are loaded again after this long.
const RELOAD_AFTER: Duration = Duration::from_secs(60 * 60);
/// After a failed load, try again this much later.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);
/// How often to look for newly joined chat channels.
const LOOK_EVERY: Duration = Duration::from_secs(10);

fn info(id: &str, code: String) -> EmoteInfo {
    EmoteInfo {
        emote: EmoteRef { source: PLATFORM.to_string(), id: id.to_string(), code },
        image_url: emote_image_url(id),
        zero_width: false,
    }
}

fn badges(sets: Vec<BadgeSet>) -> Vec<BadgeInfo> {
    sets.into_iter()
        .flat_map(|set| {
            let id = set.set_id.to_string();
            set.versions.into_iter().map(move |badge| BadgeInfo {
                set: id.clone(),
                version: badge.id.to_string(),
                title: badge.title,
                image_url: badge.image_url_2x,
            })
        })
        .collect()
}

/// When each list is due to be loaded.
#[derive(Default)]
struct Due {
    global: Option<Instant>,
    channels: HashMap<String, Instant>,
}

fn due(at: Option<&Instant>) -> bool {
    at.is_none_or(|at| Instant::now() >= *at)
}

/// Keeps the registry's Twitch emotes loaded while there's a token. A new
/// token (a login, or a session starting) or a resync loads everything
/// again.
pub fn spawn(
    http: reqwest::Client,
    registry: Arc<EmoteRegistry>,
    mut tokens: watch::Receiver<Option<UserToken>>,
) {
    tokio::spawn(async move {
        let helix: HelixClient<'static, reqwest::Client> = HelixClient::with_client(http);
        let mut next = Due::default();
        let mut resyncs = registry.resyncs();
        loop {
            let token = tokens.borrow_and_update().clone();
            if let Some(token) = token {
                if due(next.global.as_ref()) {
                    let loaded = helix.get_global_emotes(&token).await;
                    next.global =
                        Some(Instant::now() + if loaded.is_ok() { RELOAD_AFTER } else { RETRY_AFTER });
                    match loaded {
                        Ok(emotes) => {
                            tracing::info!(count = emotes.len(), "loaded Twitch's global emotes");
                            let emotes = emotes.into_iter().map(|emote| info(emote.id.as_str(), emote.name));
                            registry.set_platform_global(PLATFORM, emotes.collect());
                        }
                        Err(error) => tracing::warn!("couldn't load Twitch's global emotes: {error}"),
                    }
                    match helix.req_get(GetGlobalChatBadgesRequest::new(), &token).await {
                        Ok(response) => {
                            let badges = badges(response.data);
                            tracing::info!(count = badges.len(), "loaded Twitch's global chat badges");
                            registry.set_badges(PLATFORM, None, badges);
                        }
                        Err(error) => tracing::warn!("couldn't load Twitch's global chat badges: {error}"),
                    }
                }
                for channel in registry.watched(PLATFORM) {
                    if !due(next.channels.get(&channel)) {
                        continue;
                    }
                    let loaded = helix.get_channel_emotes_from_id(channel.as_str(), &token).await;
                    let again = if loaded.is_ok() { RELOAD_AFTER } else { RETRY_AFTER };
                    next.channels.insert(channel.clone(), Instant::now() + again);
                    match loaded {
                        Ok(emotes) => {
                            tracing::info!(channel_id = %channel, count = emotes.len(), "loaded the channel's Twitch emotes");
                            let emotes = emotes.into_iter().map(|emote| info(emote.id.as_str(), emote.name));
                            registry.set_platform_channel(PLATFORM, &channel, emotes.collect());
                        }
                        Err(error) => {
                            tracing::warn!(channel_id = %channel, "couldn't load the channel's Twitch emotes: {error}")
                        }
                    }
                    let request = GetCheermotesRequest::broadcaster_id(channel.as_str());
                    match helix.req_get(request, &token).await {
                        Ok(response) => {
                            let prefixes: Vec<String> =
                                response.data.into_iter().map(|cheermote| cheermote.prefix).collect();
                            tracing::info!(channel_id = %channel, count = prefixes.len(), "loaded the channel's cheer words");
                            registry.set_cheer_prefixes(PLATFORM, &channel, prefixes);
                        }
                        Err(error) => {
                            tracing::warn!(channel_id = %channel, "couldn't load the channel's cheer words: {error}")
                        }
                    }
                    let request = GetChannelChatBadgesRequest::broadcaster_id(channel.as_str());
                    match helix.req_get(request, &token).await {
                        Ok(response) => {
                            let badges = badges(response.data);
                            tracing::info!(channel_id = %channel, count = badges.len(), "loaded the channel's chat badges");
                            registry.set_badges(PLATFORM, Some(&channel), badges);
                        }
                        Err(error) => {
                            tracing::warn!(channel_id = %channel, "couldn't load the channel's chat badges: {error}")
                        }
                    }
                }
            }
            tokio::select! {
                changed = tokens.changed() => {
                    if changed.is_err() {
                        return; // EventSub has stopped
                    }
                    next = Due::default();
                }
                Ok(()) = resyncs.changed() => next = Due::default(),
                _ = tokio::time::sleep(LOOK_EVERY) => {}
            }
        }
    });
}
