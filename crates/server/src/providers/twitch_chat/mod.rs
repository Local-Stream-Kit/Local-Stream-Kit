//! Twitch chat over anonymous IRC: no login, read-only. It follows the
//! configured channel: a change switches to the new channel's chat right
//! away, and no channel means no connection.
//!
//! TLS on port 6697, with the system's root certificates (as EventSub's
//! WebSocket has them). Chat is public and no credentials are sent, but
//! without TLS someone on the network path could slip in fake bits and subs.

mod map;
pub use map::{PLATFORM, emote_image_url};
pub mod parse;

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use protocol::panel::{ChatStatus, LoginStatus};

use crate::model::{Input, InputKind, PlatformEvent};
use crate::pipeline::now_ms;
use crate::providers::Provider;
use crate::services::Services;
use crate::services::emotes::EmoteInfo;
use map::Mapper;
use parse::parse_line;

const HOST: &str = "irc.chat.twitch.tv";
const PORT: u16 = 6697;
/// Twitch pings about every 5 minutes; silence for longer means a dead
/// connection.
const IDLE_TIMEOUT: Duration = Duration::from_secs(6 * 60);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

pub struct TwitchChat {
    /// The configured channel (as the config store has it).
    channel: watch::Receiver<String>,
}

impl TwitchChat {
    pub fn new(channel: watch::Receiver<String>) -> TwitchChat {
        TwitchChat { channel }
    }
}

/// Whether EventSub reports every redemption now (the login listening, or
/// the Twitch CLI): then chat's own (rewards that ask for text, with the
/// reward's id but not its title) would be reported twice.
fn eventsub_reports_redemptions(services: &Services) -> bool {
    matches!(
        services.status.login(),
        LoginStatus::LoggedIn { listening: true, .. } | LoginStatus::TwitchCli { .. }
    )
}

enum SessionEnd {
    /// Twitch sent RECONNECT.
    Reconnect,
    /// The configured channel changed.
    ChannelChanged,
    /// The pipeline is gone; stop.
    PipelineGone,
}

impl Provider for TwitchChat {
    fn spawn(self: Box<Self>, inputs: mpsc::Sender<Input>, services: Services) -> JoinHandle<()> {
        let TwitchChat { mut channel } = *self;
        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                let current = channel.borrow_and_update().clone();
                if current.is_empty() {
                    services.status.set_chat(ChatStatus::Off);
                    tracing::warn!("no Twitch channel set: set one in the control panel (Status)");
                    if channel.changed().await.is_err() {
                        return;
                    }
                    continue;
                }
                let started = Instant::now();
                let ended = tokio::select! {
                    ended = session(&current, &inputs, &services) => ended,
                    changed = channel.changed() => match changed {
                        Ok(()) => Ok(SessionEnd::ChannelChanged),
                        Err(_) => return, // the app is shutting down
                    },
                };
                match ended {
                    Ok(SessionEnd::PipelineGone) => return,
                    Ok(SessionEnd::ChannelChanged) => {
                        tracing::info!(from = current, "the Twitch channel changed; switching chat");
                        leave(&services);
                        backoff = Duration::from_secs(1);
                        continue;
                    }
                    Ok(SessionEnd::Reconnect) => tracing::info!("Twitch asked us to reconnect"),
                    Err(error) => tracing::warn!("Twitch chat connection lost: {error:#}"),
                }
                if started.elapsed() > MAX_BACKOFF {
                    backoff = Duration::from_secs(1);
                }
                tracing::info!(seconds = backoff.as_secs(), "reconnecting to Twitch chat");
                // A channel change cuts the wait short.
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    changed = channel.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        leave(&services);
                        backoff = Duration::from_secs(1);
                        continue;
                    }
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        })
    }
}

async fn session(
    channel: &str,
    inputs: &mpsc::Sender<Input>,
    services: &Services,
) -> anyhow::Result<SessionEnd> {
    services.status.set_chat(ChatStatus::Connecting { channel: channel.to_string() });
    let (read, mut write) = tokio::io::split(connect().await?);
    let nick = format!("justinfan{}", rand::random_range(10_000..100_000));
    // Without these capabilities there are no tags (no emote ids, no
    // room-id) and no USERNOTICE events.
    let handshake =
        format!("CAP REQ :twitch.tv/tags twitch.tv/commands\r\nNICK {nick}\r\nJOIN #{channel}\r\n");
    write.write_all(handshake.as_bytes()).await?;
    write.flush().await?;
    tracing::info!(channel, "connected to Twitch chat");

    let mut lines = BufReader::new(read).lines();
    let mut mapper = Mapper::new();
    loop {
        let line = tokio::time::timeout(IDLE_TIMEOUT, lines.next_line())
            .await
            .context("nothing received for 6 minutes")??;
        let Some(line) = line else { bail!("Twitch closed the connection") };
        let Some(message) = parse_line(&line) else {
            tracing::debug!(line, "unparseable IRC line");
            continue;
        };
        match message.command.as_str() {
            "PING" => {
                let pong = format!("PONG :{}\r\n", message.trailing().unwrap_or("tmi.twitch.tv"));
                write.write_all(pong.as_bytes()).await?;
                write.flush().await?;
                continue;
            }
            "RECONNECT" => return Ok(SessionEnd::Reconnect),
            "CAP" if message.params.iter().any(|param| param == "NAK") => {
                bail!("Twitch refused the tags/commands capabilities")
            }
            "NOTICE" => {
                let text = message.trailing().unwrap_or_default();
                tracing::warn!(notice = text, "Twitch notice");
                if text.contains("Login authentication failed") {
                    bail!("Twitch rejected the login: {text}");
                }
            }
            _ => {}
        }
        for input in mapper.map(&message, now_ms()) {
            if matches!(input.kind, InputKind::Event(PlatformEvent::Redemption { .. }))
                && eventsub_reports_redemptions(services)
            {
                continue;
            }
            if matches!(input.kind, InputKind::ChannelJoined) {
                let room = &input.origin.channel;
                services.status.set_room(&input.origin.platform, &room.id, &room.name);
                services.status.set_chat(ChatStatus::Connected { channel: room.name.clone() });
            }
            register_emotes(&input, services);
            if inputs.send(input).await.is_err() {
                return Ok(SessionEnd::PipelineGone);
            }
        }
    }
}

/// Connects to Twitch chat, over TLS.
async fn connect() -> anyhow::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let connect = async {
        let tcp = TcpStream::connect((HOST, PORT)).await.context("couldn't connect")?;
        let name = ServerName::try_from(HOST).context("the chat server's name")?;
        tls().connect(name, tcp).await.context("couldn't set up TLS")
    };
    tokio::time::timeout(Duration::from_secs(10), connect).await.context("timed out connecting")?
}

/// Connects TLS with the system's root certificates, loaded once.
fn tls() -> TlsConnector {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    let config = CONFIG.get_or_init(|| {
        let found = rustls_native_certs::load_native_certs();
        for error in &found.errors {
            tracing::warn!("couldn't load a root certificate: {error}");
        }
        let mut roots = RootCertStore::empty();
        roots.add_parsable_certificates(found.certs);
        Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
    });
    TlsConnector::from(Arc::clone(config))
}

/// Chat is leaving the channel it was in: it's no longer the chat room, and
/// its emote sets needn't be kept fresh.
fn leave(services: &Services) {
    if let Some((platform, id, _)) = services.status.room() {
        services.emotes.unwatch(&platform, &id);
        services.status.clear_room();
    }
}

/// Twitch's own emotes are learned as they're seen in chat.
fn register_emotes(input: &Input, services: &Services) {
    for message in map::messages(input) {
        for span in &message.emotes {
            services.emotes.register_native(
                PLATFORM,
                &input.origin.channel.id,
                EmoteInfo {
                    emote: span.emote.clone(),
                    image_url: map::emote_image_url(&span.emote.id),
                    zero_width: false,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against Twitch: `cargo test -p lsk chat_over_tls -- --ignored`.
    #[tokio::test]
    #[ignore = "connects to Twitch"]
    async fn chat_over_tls() {
        let (read, mut write) = tokio::io::split(connect().await.unwrap());
        write.write_all(b"NICK justinfan12345\r\n").await.unwrap();
        write.flush().await.unwrap();
        let mut lines = BufReader::new(read).lines();
        let welcome = tokio::time::timeout(Duration::from_secs(10), lines.next_line()).await;
        let line = welcome.unwrap().unwrap().unwrap();
        assert!(line.contains(" 001 "), "{line}");
    }
}
