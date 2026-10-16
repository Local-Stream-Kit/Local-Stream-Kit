//! The viewers' cards, for the control panel's Activity tab: Find a viewer,
//! opening a card (its chat log lines, then what Twitch says about them),
//! older lines, and moderating them. Answers that wait on the disk or on
//! Twitch go to the panel's connection when they're ready.

use protocol::panel::{PanelEvent, PanelRequest};
use protocol::viewers::{Access, ViewerAction, ViewerCard, is_login, twitch_card_url};
use tokio::sync::mpsc;

use crate::panel::PanelState;
use crate::services::chat_log::{Card, LogChange, Someone};

/// A viewer request from one panel, with the card it has open; any other
/// request comes back.
pub fn handle(
    request: PanelRequest,
    panel: &PanelState,
    open: &mut Option<String>,
    replies: &mpsc::Sender<PanelEvent>,
) -> Option<PanelRequest> {
    match request {
        PanelRequest::FindViewers { text } => {
            let viewers = panel.chat_log.find(&text);
            reply(replies, PanelEvent::ViewersFound { text, viewers });
        }
        PanelRequest::OpenViewer { id, name } => {
            // Its lines go out once the card is known (`opened`).
            *open = Some(String::new());
            let (panel, replies) = (panel.clone(), replies.clone());
            tokio::spawn(async move { open_card(&panel, id, name, &replies).await });
        }
        PanelRequest::OlderViewerLines { id, before } => {
            let (chat_log, replies) = (panel.chat_log.clone(), replies.clone());
            tokio::task::spawn_blocking(move || {
                if let Some((lines, older)) = chat_log.older(&id, before) {
                    reply(&replies, PanelEvent::ViewerOlder { id, lines, older });
                }
            });
        }
        PanelRequest::CloseViewer => *open = None,
        PanelRequest::Moderate { id, action } => {
            let (panel, replies) = (panel.clone(), replies.clone());
            tokio::spawn(async move { moderate(&panel, id, action, &replies).await });
        }
        PanelRequest::OpenTwitchCard { login } => {
            let channel = panel.store.current().connections.twitch_channel;
            if !is_login(&login) || !is_login(&channel) {
                tracing::warn!(login, "that isn't a Twitch login; nothing was opened");
            } else {
                let url = twitch_card_url(&channel, &login);
                if let Err(error) = panel.desktop.open(&url) {
                    tracing::warn!("couldn't open the browser ({error}); open {url} yourself");
                }
            }
        }
        other => return Some(other),
    }
    None
}

/// Which card the panel has open, once `Viewer` has said: its id.
pub fn opened(event: &PanelEvent, open: &mut Option<String>) {
    if let (PanelEvent::Viewer { card }, Some(open)) = (event, open.as_mut()) {
        *open = card.id.clone();
    }
}

/// What a change to the log tells a panel with card `open`.
pub fn change(change: LogChange, open: Option<&str>) -> Option<PanelEvent> {
    match change {
        LogChange::Line { viewer, line } if Some(viewer.as_str()) == open => {
            Some(PanelEvent::ViewerLine { id: viewer, line })
        }
        LogChange::Deleted { viewer, message } if Some(viewer.as_str()) == open => {
            Some(PanelEvent::ViewerMessageDeleted { id: viewer, message })
        }
        _ => None,
    }
}

fn reply(replies: &mpsc::Sender<PanelEvent>, event: PanelEvent) {
    // A panel that has gone needs no answer.
    let _ = replies.try_send(event);
}

fn access(panel: &PanelState) -> Access {
    panel.moderation.as_ref().map_or(Access::LoggedOut, |moderation| moderation.access())
}

fn card_of(card: Card, access: Access) -> ViewerCard {
    let Card { id, login, name, color, badges, messages, lines, older } = card;
    ViewerCard { id, login, name, color, badges, messages, lines, older, access }
}

/// Opens a card: the log's, else one for a viewer it hasn't seen (found on
/// Twitch); then what Twitch says.
async fn open_card(panel: &PanelState, id: Option<String>, name: String, replies: &mpsc::Sender<PanelEvent>) {
    let chat_log = panel.chat_log.clone();
    let id = id.filter(|id| !id.is_empty()).or_else(|| chat_log.id_of(&name));
    let logged = match id.clone() {
        Some(id) => tokio::task::spawn_blocking(move || chat_log.card(&id)).await.ok().flatten(),
        None => None,
    };
    let access = access(panel);
    let card = match logged {
        Some(card) => card_of(card, access.clone()),
        None => match unseen(panel, id, &name).await {
            Ok(someone) => ViewerCard {
                id: someone.id,
                login: someone.login,
                name: someone.name,
                color: None,
                badges: Vec::new(),
                messages: 0,
                lines: Vec::new(),
                older: 0,
                access: access.clone(),
            },
            Err(problem) => {
                reply(replies, PanelEvent::ViewerMissing { name, problem });
                return;
            }
        },
    };
    let id = card.id.clone();
    reply(replies, PanelEvent::Viewer { card });
    send_info(panel, id, replies).await;
}

/// A viewer the log hasn't seen: one named in Activity (with their id), or
/// a name typed, looked up on Twitch.
async fn unseen(panel: &PanelState, id: Option<String>, name: &str) -> Result<Someone, String> {
    let name = name.trim();
    let login = name.to_lowercase();
    if let Some(moderation) = &panel.moderation
        && access(panel) != Access::LoggedOut
        && is_login(&login)
    {
        return match moderation.look_up(id.as_deref(), &login).await {
            Ok(Some(someone)) => Ok(someone),
            Ok(None) => Err(format!("No Twitch account is called {name}.")),
            Err(problem) => Err(problem),
        };
    }
    match id {
        // Named in Activity: they're someone, if not someone who has chatted.
        Some(id) => {
            Ok(Someone { id, login: if is_login(&login) { login } else { String::new() }, name: name.into() })
        }
        None => Err(format!("Nobody called {name} has chatted yet.")),
    }
}

/// What Twitch says about viewer `id`, when it can be asked (logged in as
/// the chat's channel, Moderating ticked or not).
async fn send_info(panel: &PanelState, id: String, replies: &mpsc::Sender<PanelEvent>) {
    let Some(moderation) = &panel.moderation else { return };
    if !moderation.can_ask() {
        return;
    }
    let event = match moderation.info(&id).await {
        Ok(info) => PanelEvent::ViewerInfo { id, info: Some(info), problem: None },
        Err(problem) => PanelEvent::ViewerInfo { id, info: None, problem: Some(problem) },
    };
    reply(replies, event);
}

async fn moderate(panel: &PanelState, id: String, action: ViewerAction, replies: &mpsc::Sender<PanelEvent>) {
    let Some(moderation) = &panel.moderation else {
        let problem = Some("Log in with Twitch on Status to moderate.".to_string());
        reply(replies, PanelEvent::Moderated { id, action, problem });
        return;
    };
    match moderation.act(&id, &action).await {
        Ok(done) => {
            if let Some(done) = done {
                let someone = match panel.chat_log.someone(&id) {
                    Some(someone) => Some(someone),
                    None => moderation.look_up(Some(&id), "").await.ok().flatten(),
                };
                if let Some(someone) = someone {
                    panel.chat_log.note(someone, done);
                }
            }
            tracing::info!(viewer = id, ?action, "moderated a viewer");
            reply(replies, PanelEvent::Moderated { id: id.clone(), action, problem: None });
            send_info(panel, id, replies).await;
        }
        Err(problem) => reply(replies, PanelEvent::Moderated { id, action, problem: Some(problem) }),
    }
}
