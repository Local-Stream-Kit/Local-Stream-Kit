//! Twitch EventSub over WebSocket: channel point redemptions, follows and
//! hype trains.
//!
//! Unlike chat, this needs the broadcaster's login (see `auth`): Twitch only
//! sends a channel's redemptions to an app the broadcaster has approved. The
//! same login lists Twitch's own emotes for the registry (see `emotes`).
//!
//! For testing without a live stream, `--twitch-cli` points it at the Twitch
//! CLI's mock server instead (`twitch event websocket start-server`), with no
//! login; `twitch event trigger channel.channel_points_custom_reward_redemption.add
//! --transport=websocket` then sends a fake redemption,
//! `twitch event trigger channel.follow --transport=websocket` a fake follow,
//! and `channel.hype_train.begin` (`.progress`, `.end`) a hype train.

mod auth;
mod emotes;
mod map;

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use twitch_api::HelixClient;
use twitch_api::eventsub::channel::{
    ChannelFollowV2, ChannelHypeTrainBeginV1, ChannelHypeTrainBeginV2, ChannelHypeTrainEndV1,
    ChannelHypeTrainEndV2, ChannelHypeTrainProgressV1, ChannelHypeTrainProgressV2,
    ChannelPointsCustomRewardRedemptionAddV1,
};
use twitch_api::eventsub::event::Event;
use twitch_api::eventsub::event::websocket::EventsubWebsocketData;
use twitch_api::eventsub::{Status, Transport};
use twitch_api::helix::HelixRequestBody;
use twitch_api::helix::eventsub::CreateEventSubSubscriptionBody;
use twitch_oauth2::UserToken;

use crate::model::Input;
use crate::pipeline::now_ms;
use crate::providers::Provider;
use crate::services::Services;
use crate::services::status::StatusHub;
use auth::Auth;
use protocol::panel::LoginStatus;

/// The Client ID of the Twitch app registered for Local Stream Kit, as a
/// "Public" client. Public clients have no secret, and the ID itself isn't
/// one: it only says which app is asking.
pub const CLIENT_ID: &str = "3fea7vwrm186qhxkzoiwarl929japq";

const EVENTSUB_URL: &str = "wss://eventsub.wss.twitch.tv/ws";
/// Twitch's welcome message must arrive this soon after connecting.
const WELCOME_TIMEOUT: Duration = Duration::from_secs(10);
/// Added to the keepalive interval Twitch announces before the connection
/// counts as dead.
const KEEPALIVE_SLACK: Duration = Duration::from_secs(10);
/// Twitch asks apps to check their token at least this often.
const CHECK_LOGIN_EVERY: Duration = Duration::from_secs(60 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Twitch may send a notification twice; this many recent message ids are
/// remembered to skip repeats.
const REMEMBERED_IDS: usize = 256;
/// How long the old connection is still read after Twitch moves the session
/// (`session_reconnect`), if Twitch doesn't close it first.
const DRAIN_FOR: Duration = Duration::from_secs(30);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Where events come from.
pub enum Server {
    Twitch {
        /// The broadcaster's saved login.
        tokens_path: PathBuf,
        /// The chat channel, to warn if the login is for a different one.
        chat_channel: watch::Receiver<String>,
    },
    /// The Twitch CLI's mock server at this address (e.g. `127.0.0.1:8080`).
    TwitchCli { address: String },
}

/// What the control panel can ask of the login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginCommand {
    /// Start logging in (after a cancel or log out).
    Start,
    /// Stop waiting for the current code.
    Cancel,
    Logout,
}

pub struct TwitchEventSub {
    http: reqwest::Client,
    server: Server,
    commands: mpsc::Receiver<LoginCommand>,
}

impl TwitchEventSub {
    pub fn new(
        http: reqwest::Client,
        server: Server,
        commands: mpsc::Receiver<LoginCommand>,
    ) -> TwitchEventSub {
        TwitchEventSub { http, server, commands }
    }
}

/// The server-specific half of a session: logging in and subscribing.
#[allow(clippy::large_enum_variant)] // there's only ever one
enum Backend {
    Twitch {
        auth: Auth,
        helix: HelixClient<'static, reqwest::Client>,
        chat_channel: watch::Receiver<String>,
        status: StatusHub,
        /// The working login, for loading Twitch's emote lists.
        tokens: watch::Sender<Option<UserToken>>,
    },
    TwitchCli {
        http: reqwest::Client,
        address: String,
    },
}

impl Backend {
    fn websocket_url(&self) -> String {
        match self {
            Backend::Twitch { .. } => EVENTSUB_URL.to_string(),
            Backend::TwitchCli { address, .. } => format!("ws://{address}/ws"),
        }
    }

    /// Logs in if needed (waiting for the broadcaster to approve), returning
    /// a description of the channel for logs.
    async fn prepare(&mut self) -> anyhow::Result<String> {
        match self {
            Backend::Twitch { auth, chat_channel, .. } => {
                let token = auth.token().await?;
                let channel = chat_channel.borrow().clone();
                if !channel.is_empty() && !channel.eq_ignore_ascii_case(token.login.as_str()) {
                    tracing::warn!(
                        login = %token.login,
                        chat = channel,
                        "the Twitch login is for a different channel than chat; channel points come from the login's channel"
                    );
                }
                Ok(token.login.to_string())
            }
            Backend::TwitchCli { address, .. } => Ok(format!("the Twitch CLI at {address}")),
        }
    }

    /// Subscribes the session to redemptions, follows and hype trains. Must
    /// happen within 10 seconds of the welcome, or Twitch closes the
    /// connection. Follows or hype trains failing doesn't stop the rest.
    #[allow(deprecated)] // version 1 of the hype train events: the Twitch CLI's
    async fn subscribe(&mut self, session_id: &str) -> anyhow::Result<()> {
        match self {
            Backend::Twitch { auth, helix, status, tokens, .. } => {
                let token = auth.token().await?;
                helix
                    .create_eventsub_subscription(
                        ChannelPointsCustomRewardRedemptionAddV1::broadcaster_user_id(token.user_id.clone()),
                        Transport::websocket(session_id),
                        &token,
                    )
                    .await?;
                // As the broadcaster, who counts as a moderator of their own
                // channel.
                let follows = helix
                    .create_eventsub_subscription(
                        ChannelFollowV2::new(token.user_id.clone(), token.user_id.clone()),
                        Transport::websocket(session_id),
                        &token,
                    )
                    .await;
                if let Err(error) = &follows {
                    tracing::warn!("couldn't listen for follows: {error}");
                }
                // Version 2: Twitch has withdrawn version 1.
                let hype_train = async {
                    let id = &token.user_id;
                    let transport = || Transport::websocket(session_id);
                    helix
                        .create_eventsub_subscription(
                            ChannelHypeTrainBeginV2::broadcaster_user_id(id.clone()),
                            transport(),
                            &token,
                        )
                        .await?;
                    helix
                        .create_eventsub_subscription(
                            ChannelHypeTrainProgressV2::broadcaster_user_id(id.clone()),
                            transport(),
                            &token,
                        )
                        .await?;
                    helix
                        .create_eventsub_subscription(
                            ChannelHypeTrainEndV2::broadcaster_user_id(id.clone()),
                            transport(),
                            &token,
                        )
                        .await?;
                    anyhow::Ok(())
                }
                .await;
                if let Err(error) = &hype_train {
                    tracing::warn!("couldn't listen for hype trains: {error}");
                }
                status.set_login(LoginStatus::LoggedIn {
                    login: token.login.to_string(),
                    listening: true,
                    follows: follows.is_ok(),
                    hype_train: hype_train.is_ok(),
                });
                tokens.send_replace(Some(token.clone()));
                // The channel's rewards, for the panel's reward picker.
                match helix.get_all_custom_rewards(&token.user_id, false, &token).await {
                    Ok(rewards) => {
                        let mut titles: Vec<String> =
                            rewards.into_iter().map(|reward| reward.title).collect();
                        titles.sort_by_key(|title| title.to_lowercase());
                        status.set_rewards(titles);
                    }
                    Err(error) => tracing::warn!("couldn't list the channel's rewards: {error}"),
                }
            }
            Backend::TwitchCli { http, address } => {
                // The same bodies twitch_api sends to Twitch. The mock doesn't
                // check the token, and its fake events use random channels.
                // It only makes version 1 of the hype train events.
                let transport = || Transport::websocket(session_id);
                let bodies = [
                    CreateEventSubSubscriptionBody::new(
                        ChannelPointsCustomRewardRedemptionAddV1::broadcaster_user_id("1"),
                        Transport::websocket(session_id),
                    )
                    .try_to_body()?,
                    CreateEventSubSubscriptionBody::new(ChannelFollowV2::new("1", "1"), transport())
                        .try_to_body()?,
                    CreateEventSubSubscriptionBody::new(
                        ChannelHypeTrainBeginV1::broadcaster_user_id("1"),
                        transport(),
                    )
                    .try_to_body()?,
                    CreateEventSubSubscriptionBody::new(
                        ChannelHypeTrainProgressV1::broadcaster_user_id("1"),
                        transport(),
                    )
                    .try_to_body()?,
                    CreateEventSubSubscriptionBody::new(
                        ChannelHypeTrainEndV1::broadcaster_user_id("1"),
                        transport(),
                    )
                    .try_to_body()?,
                ];
                for body in bodies {
                    http.post(format!("http://{address}/eventsub/subscriptions"))
                        .header("Client-Id", CLIENT_ID)
                        .header("Authorization", "Bearer twitch-cli")
                        .header("Content-Type", "application/json")
                        .body(body.to_vec())
                        .send()
                        .await?
                        .error_for_status()?;
                }
            }
        }
        Ok(())
    }

    /// Twitch asks apps to check their token hourly. `false` when the login is
    /// gone and the session should end.
    async fn check(&mut self) -> bool {
        match self {
            Backend::Twitch { auth, .. } => {
                if let Err(error) = auth.check().await {
                    tracing::warn!("{error:#}");
                }
                auth.logged_in()
            }
            Backend::TwitchCli { .. } => true,
        }
    }

    /// The broadcaster disconnected the app from their account.
    fn forget_login(&mut self) {
        if let Backend::Twitch { auth, tokens, .. } = self {
            auth.forget();
            tokens.send_replace(None);
        }
    }

    async fn logout(&mut self) {
        if let Backend::Twitch { auth, status, tokens, .. } = self {
            auth.logout().await;
            status.set_rewards(Vec::new());
            tokens.send_replace(None);
        }
    }

    /// The connection dropped: not listening until it's back.
    fn disconnected(&mut self) {
        if let Backend::Twitch { status, .. } = self
            && let LoginStatus::LoggedIn { login, listening: true, .. } = status.login()
        {
            status.set_login(LoginStatus::LoggedIn {
                login,
                listening: false,
                follows: false,
                hype_train: false,
            });
        }
    }
}

enum Interrupted {
    Ended(anyhow::Result<SessionEnd>),
    Command(Option<LoginCommand>),
}

enum SessionEnd {
    /// The pipeline is gone; stop.
    PipelineGone,
}

impl Provider for TwitchEventSub {
    fn spawn(self: Box<Self>, inputs: mpsc::Sender<Input>, services: Services) -> JoinHandle<()> {
        let TwitchEventSub { http, server, mut commands } = *self;
        tokio::spawn(async move {
            if CLIENT_ID.is_empty() {
                tracing::error!(
                    "this build has no Twitch Client ID (CLIENT_ID in providers/twitch_eventsub): \
                     no logging in for channel points, follows and hype trains"
                );
                return;
            }
            let status = services.status.clone();
            // No saved login: wait until someone logs in from the panel,
            // rather than asking for one at every start.
            let saved = match &server {
                Server::Twitch { tokens_path, .. } => tokens_path.exists(),
                Server::TwitchCli { .. } => true,
            };
            let mut backend = match server {
                Server::Twitch { tokens_path, chat_channel } => {
                    let (tokens, token) = watch::channel(None);
                    emotes::spawn(http.clone(), services.emotes.clone(), token);
                    Backend::Twitch {
                        auth: Auth::new(http.clone(), CLIENT_ID, tokens_path, status.clone()),
                        helix: HelixClient::with_client(http.clone()),
                        chat_channel,
                        status: status.clone(),
                        tokens,
                    }
                }
                Server::TwitchCli { address } => {
                    tracing::warn!(
                        address,
                        "channel points come from the Twitch CLI's mock server, not Twitch"
                    );
                    status.set_login(LoginStatus::TwitchCli { address: address.clone() });
                    Backend::TwitchCli { http: http.clone(), address }
                }
            };
            let mut seen = RecentIds::default();
            let mut trains = map::HypeTrains::default();
            let mut backoff = Duration::from_secs(1);
            // Logged out or cancelled from the panel (or never logged in):
            // wait to be asked.
            let mut paused = !saved;
            loop {
                if paused {
                    status.set_login(LoginStatus::LoggedOut);
                    loop {
                        match commands.recv().await {
                            Some(LoginCommand::Start) => break,
                            Some(_) => continue,
                            None => return,
                        }
                    }
                    paused = false;
                }
                let mut subscribed = false;
                let interrupted = tokio::select! {
                    result = session(&mut backend, &inputs, &mut seen, &mut trains, &mut subscribed) => Interrupted::Ended(result),
                    command = commands.recv() => Interrupted::Command(command),
                };
                backend.disconnected();
                match interrupted {
                    Interrupted::Ended(Ok(SessionEnd::PipelineGone)) | Interrupted::Command(None) => return,
                    Interrupted::Ended(Err(error)) => {
                        tracing::warn!("Twitch EventSub connection lost: {error:#}")
                    }
                    Interrupted::Command(Some(LoginCommand::Logout)) => {
                        backend.logout().await;
                        paused = true;
                        continue;
                    }
                    // Only a login waiting for approval can be cancelled.
                    Interrupted::Command(Some(LoginCommand::Cancel))
                        if matches!(status.login(), LoginStatus::Waiting { .. }) =>
                    {
                        paused = true;
                        continue;
                    }
                    Interrupted::Command(Some(_)) => continue,
                }
                // A connection that worked starts the delays over; only
                // repeated failures to get that far back off further.
                if subscribed {
                    backoff = Duration::from_secs(1);
                }
                tracing::info!(seconds = backoff.as_secs(), "reconnecting to Twitch EventSub");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        })
    }
}

async fn session(
    backend: &mut Backend,
    inputs: &mpsc::Sender<Input>,
    seen: &mut RecentIds,
    trains: &mut map::HypeTrains,
    subscribed: &mut bool,
) -> anyhow::Result<SessionEnd> {
    let channel = backend.prepare().await?;
    let (socket, keepalive) = connect(&backend.websocket_url()).await?;
    backend.subscribe(&socket.session_id).await.context("couldn't subscribe to channel point redemptions")?;
    tracing::info!(channel, "listening for channel point redemptions, follows and hype trains");
    *subscribed = true;
    listen(backend, socket, keepalive, inputs, seen, trains).await
}

/// Reads a subscribed connection until it fails or the pipeline is gone.
///
/// On `session_reconnect` it connects to the new address; Twitch moves the
/// subscriptions over by itself. The old connection is still read until
/// Twitch closes it (or [`DRAIN_FOR`] passes): what Twitch sent to it before
/// the new one was welcomed is in it. Both go through the same `seen`, so an
/// event sent to both counts once.
async fn listen(
    backend: &mut Backend,
    mut socket: Connection,
    mut keepalive: Duration,
    inputs: &mpsc::Sender<Input>,
    seen: &mut RecentIds,
    trains: &mut map::HypeTrains,
) -> anyhow::Result<SessionEnd> {
    let mut old: Option<Draining> = None;
    let mut check =
        tokio::time::interval_at(tokio::time::Instant::now() + CHECK_LOGIN_EVERY, CHECK_LOGIN_EVERY);
    loop {
        tokio::select! {
            frame = tokio::time::timeout(keepalive + KEEPALIVE_SLACK, socket.stream.next()) => {
                let text = match frame.context("no message or keepalive from Twitch in time")? {
                    Some(Ok(WsMessage::Text(text))) => text,
                    Some(Ok(WsMessage::Close(frame))) => bail!("Twitch closed the connection: {frame:?}"),
                    Some(Ok(_)) => continue, // pings are answered by tungstenite
                    Some(Err(error)) => return Err(error.into()),
                    None => bail!("the connection closed"),
                };
                match on_message(&text, backend, inputs, seen, trains).await? {
                    Some(Flow::PipelineGone) => return Ok(SessionEnd::PipelineGone),
                    Some(Flow::Reconnect(url)) => {
                        tracing::info!("Twitch moved the EventSub connection");
                        let (new_socket, new_keepalive) = connect(&url).await?;
                        let moved = std::mem::replace(&mut socket, new_socket);
                        old = Some(Draining {
                            stream: moved.stream,
                            until: tokio::time::Instant::now() + DRAIN_FOR,
                        });
                        keepalive = new_keepalive;
                    }
                    None => {}
                }
            }
            text = drain(&mut old), if old.is_some() => {
                let Some(text) = text else {
                    tracing::debug!("the old EventSub connection is closed");
                    old = None;
                    continue;
                };
                match on_message(&text, backend, inputs, seen, trains).await? {
                    Some(Flow::PipelineGone) => return Ok(SessionEnd::PipelineGone),
                    // Only the current connection can be moved.
                    Some(Flow::Reconnect(_)) | None => {}
                }
            }
            _ = check.tick() => {
                if !backend.check().await {
                    bail!("the Twitch login is no longer valid");
                }
            }
        }
    }
}

/// What a message asks of the connection.
enum Flow {
    PipelineGone,
    /// Connect to this address instead (`session_reconnect`).
    Reconnect(String),
}

/// Handles one message from Twitch: a new notification goes to the
/// pipeline.
async fn on_message(
    text: &str,
    backend: &mut Backend,
    inputs: &mpsc::Sender<Input>,
    seen: &mut RecentIds,
    trains: &mut map::HypeTrains,
) -> anyhow::Result<Option<Flow>> {
    tracing::debug!(frame = %text, "EventSub message");
    let Some(data) = parse(text) else { return Ok(None) };
    match data {
        EventsubWebsocketData::Notification { metadata, payload } => {
            if seen.insert(&metadata.message_id)
                && let Some(input) = map::input(&payload, now_ms(), trains)
                && inputs.send(input).await.is_err()
            {
                return Ok(Some(Flow::PipelineGone));
            }
        }
        EventsubWebsocketData::Reconnect { payload, .. } => {
            let url = payload.session.reconnect_url.context("a reconnect message without a URL")?;
            return Ok(Some(Flow::Reconnect(url.to_string())));
        }
        EventsubWebsocketData::Revocation { payload, .. } => {
            let status = revocation_status(&payload);
            if status == Some(Status::AuthorizationRevoked) {
                tracing::warn!("the broadcaster disconnected Local Stream Kit from their Twitch account");
                backend.forget_login();
            }
            bail!("Twitch ended the subscription ({status:?})");
        }
        _ => {} // keepalives, and welcomes only come right after connecting
    }
    Ok(None)
}

/// A connection Twitch moved away from, read until it closes.
struct Draining {
    stream: Socket,
    /// Given up on then, closed or not.
    until: tokio::time::Instant,
}

/// The next text from the connection being drained; `None` once it's
/// closed, broken or past its time.
async fn drain(old: &mut Option<Draining>) -> Option<String> {
    let draining = old.as_mut()?;
    loop {
        match tokio::time::timeout_at(draining.until, draining.stream.next()).await {
            Ok(Some(Ok(WsMessage::Text(text)))) => return Some(text.to_string()),
            Ok(Some(Ok(WsMessage::Close(_)) | Err(_)) | None) | Err(_) => return None,
            Ok(Some(Ok(_))) => continue,
        }
    }
}

struct Connection {
    stream: Socket,
    session_id: String,
}

/// Connects and waits for Twitch's welcome. Returns the connection and the
/// keepalive interval.
async fn connect(url: &str) -> anyhow::Result<(Connection, Duration)> {
    let (mut stream, _) = tokio::time::timeout(WELCOME_TIMEOUT, tokio_tungstenite::connect_async(url))
        .await
        .context("timed out connecting to Twitch EventSub")?
        .context("couldn't connect to Twitch EventSub")?;
    let deadline = tokio::time::Instant::now() + WELCOME_TIMEOUT;
    loop {
        let frame =
            tokio::time::timeout_at(deadline, stream.next()).await.context("no welcome from Twitch")?;
        let text = match frame {
            Some(Ok(WsMessage::Text(text))) => text,
            Some(Ok(WsMessage::Close(frame))) => bail!("Twitch closed the connection: {frame:?}"),
            Some(Ok(_)) => continue,
            Some(Err(error)) => return Err(error.into()),
            None => bail!("the connection closed before Twitch's welcome"),
        };
        let EventsubWebsocketData::Welcome { payload, .. } =
            Event::parse_websocket(&text).context("unexpected EventSub message")?
        else {
            bail!("expected a welcome first, got {text}");
        };
        let keepalive = payload.session.keepalive_timeout_seconds.unwrap_or(10).max(1) as u64;
        let session_id = payload.session.id.to_string();
        // `twitch event websocket close --session=…` needs it.
        tracing::info!(session = session_id, "connected to EventSub");
        return Ok((Connection { stream, session_id }, Duration::from_secs(keepalive)));
    }
}

/// A message from Twitch, or `None` (logged) for one that can't be read: an
/// event twitch_api doesn't know, or one Twitch has changed. Skipping it
/// keeps the connection, and every other event with it; ending the
/// session would drop it again each time such a message came.
fn parse(text: &str) -> Option<EventsubWebsocketData<'_>> {
    match Event::parse_websocket(text) {
        Ok(data) => Some(data),
        Err(error) => {
            tracing::warn!(frame = %text, "skipped an EventSub message that couldn't be read: {error}");
            None
        }
    }
}

#[allow(deprecated)] // version 1 of the hype train events: the Twitch CLI's
fn revocation_status(event: &Event) -> Option<Status> {
    match event {
        Event::ChannelPointsCustomRewardRedemptionAddV1(payload) => Some(payload.subscription.status.clone()),
        Event::ChannelFollowV2(payload) => Some(payload.subscription.status.clone()),
        Event::ChannelHypeTrainBeginV2(payload) => Some(payload.subscription.status.clone()),
        Event::ChannelHypeTrainProgressV2(payload) => Some(payload.subscription.status.clone()),
        Event::ChannelHypeTrainEndV2(payload) => Some(payload.subscription.status.clone()),
        Event::ChannelHypeTrainBeginV1(payload) => Some(payload.subscription.status.clone()),
        Event::ChannelHypeTrainProgressV1(payload) => Some(payload.subscription.status.clone()),
        Event::ChannelHypeTrainEndV1(payload) => Some(payload.subscription.status.clone()),
        _ => None,
    }
}

/// Recent notification ids, oldest dropped first.
#[derive(Default)]
struct RecentIds {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl RecentIds {
    /// `false` if the id was already seen.
    fn insert(&mut self, id: &str) -> bool {
        if self.set.contains(id) {
            return false;
        }
        if self.order.len() == REMEMBERED_IDS
            && let Some(oldest) = self.order.pop_front()
        {
            self.set.remove(&oldest);
        }
        self.order.push_back(id.to_string());
        self.set.insert(id.to_string());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_notifications_are_skipped() {
        let mut seen = RecentIds::default();
        assert!(seen.insert("a"));
        assert!(!seen.insert("a"));
        for index in 0..REMEMBERED_IDS {
            assert!(seen.insert(&index.to_string()));
        }
        // "a" has been pushed out by now.
        assert!(seen.insert("a"));
    }

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/eventsub/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    #[test]
    fn a_message_that_cant_be_read_is_skipped() {
        let follow = fixture("follow.json");
        assert!(matches!(parse(&follow), Some(EventsubWebsocketData::Notification { .. })));
        // A type twitch_api doesn't know, and a known one Twitch changed.
        let unknown = follow.replace("channel.follow", "channel.something_new");
        assert!(parse(&unknown).is_none());
        let changed = follow.replace(r#""user_id": "1234""#, r#""user_id": 1234"#);
        assert_ne!(changed, follow);
        assert!(parse(&changed).is_none());
    }

    /// A welcome or reconnect message, as Twitch sends them.
    fn session_message(kind: &str, session: &str, reconnect_url: Option<&str>) -> WsMessage {
        let url = reconnect_url.map_or("null".to_string(), |url| format!("\"{url}\""));
        let (status, keepalive) =
            if reconnect_url.is_some() { ("reconnecting", "null") } else { ("connected", "10") };
        WsMessage::text(format!(
            r#"{{"metadata":{{"message_id":"{kind}-{session}","message_type":"{kind}","message_timestamp":"2023-07-19T14:56:51.634234626Z"}},"payload":{{"session":{{"id":"{session}","status":"{status}","connected_at":"2023-07-19T14:56:51.616329898Z","keepalive_timeout_seconds":{keepalive},"reconnect_url":{url}}}}}}}"#
        ))
    }

    #[tokio::test]
    async fn after_a_reconnect_the_old_connection_is_read_until_it_closes() {
        use futures_util::SinkExt;
        use tokio::net::TcpListener;

        let follow = fixture("follow.json");
        let follow = move |id: &str, login: &str| {
            WsMessage::text(
                follow.replace("3a5c6d2e-6f1b-4c1e-9d2a-0b7f2e4c8a11", id).replace("cool_user2", login),
            )
        };
        let old = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let old_url = format!("ws://{}", old.local_addr().unwrap());
        let new = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let new_url = format!("ws://{}", new.local_addr().unwrap());
        let (welcomed, on_welcomed) = tokio::sync::oneshot::channel();
        let reconnect = session_message("session_reconnect", "old", Some(&new_url));
        let to_old = follow.clone();
        tokio::spawn(async move {
            let mut socket = tokio_tungstenite::accept_async(old.accept().await.unwrap().0).await.unwrap();
            socket.send(session_message("session_welcome", "old", None)).await.unwrap();
            socket.send(to_old("1", "one")).await.unwrap();
            socket.send(reconnect).await.unwrap();
            // Sent before the new connection is welcomed: only here.
            socket.send(to_old("2", "two")).await.unwrap();
            on_welcomed.await.unwrap();
            // Sent to both.
            socket.send(to_old("3", "three")).await.unwrap();
            socket.close(None).await.unwrap();
        });
        tokio::spawn(async move {
            let mut socket = tokio_tungstenite::accept_async(new.accept().await.unwrap().0).await.unwrap();
            socket.send(session_message("session_welcome", "new", None)).await.unwrap();
            welcomed.send(()).unwrap();
            socket.send(follow("3", "three")).await.unwrap();
            socket.send(follow("4", "four")).await.unwrap();
            std::future::pending::<()>().await;
        });

        let (socket, keepalive) = connect(&old_url).await.unwrap();
        let mut backend = Backend::TwitchCli { http: reqwest::Client::new(), address: "unused".into() };
        let (inputs, mut received) = mpsc::channel(8);
        let (mut seen, mut trains) = (RecentIds::default(), map::HypeTrains::default());
        let listening = listen(&mut backend, socket, keepalive, &inputs, &mut seen, &mut trains);
        // Long enough for everything sent to arrive (and a repeat with it).
        tokio::select! {
            ended = listening => panic!("the connection ended: {:?}", ended.err()),
            () = tokio::time::sleep(Duration::from_millis(1500)) => {}
        }
        let mut names = Vec::new();
        while let Ok(input) = received.try_recv() {
            let crate::model::InputKind::Event(crate::model::PlatformEvent::Follow { user }) = input.kind
            else {
                panic!("a follow")
            };
            names.push(user.name);
        }
        names.sort();
        assert_eq!(names, ["four", "one", "three", "two"], "each once, none lost");
    }
}
