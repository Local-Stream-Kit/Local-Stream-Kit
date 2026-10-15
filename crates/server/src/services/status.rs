//! Live status for the control panel: what's connected and logged in. Parts
//! are set by whoever knows them (providers, the overlay connections, the
//! config store); panels get a whole [`Status`] whenever any part changes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use protocol::panel::{ChatStatus, EmoteCount, LogStatus, LoginStatus, OverlayStatus, Status};
use protocol::speech::SpeechStatus;
use protocol::update::UpdateStatus;
use tokio::sync::watch;

use crate::services::emotes::EmoteRegistry;

#[derive(Clone)]
pub struct StatusHub {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    /// Bumped on every change; panels watch it.
    changed: watch::Sender<u64>,
}

#[derive(Default)]
struct State {
    chat: ChatStatus,
    /// The chat channel as `(platform, id, name)`, once joined.
    room: Option<(String, String, String)>,
    login: LoginStatus,
    /// Each connected overlay's plugin pages.
    overlays: BTreeMap<u64, OverlayStatus>,
    next_overlay: u64,
    rewards: Vec<String>,
    config_error: Option<String>,
    speech: SpeechStatus,
    control_key: String,
    log: LogStatus,
    update: UpdateStatus,
}

impl StatusHub {
    pub fn new() -> StatusHub {
        let (changed, _) = watch::channel(0);
        StatusHub { inner: Arc::new(Inner { state: Mutex::new(State::default()), changed }) }
    }

    fn update(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.inner.state.lock().unwrap());
        self.inner.changed.send_modify(|version| *version += 1);
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.changed.subscribe()
    }

    pub fn set_chat(&self, chat: ChatStatus) {
        self.update(|state| state.chat = chat);
    }

    pub fn set_room(&self, platform: &str, id: &str, name: &str) {
        self.update(|state| state.room = Some((platform.into(), id.into(), name.into())));
    }

    /// The chat channel as `(platform, id, name)`, once joined.
    pub fn room(&self) -> Option<(String, String, String)> {
        self.inner.state.lock().unwrap().room.clone()
    }

    /// Chat left the channel (it changed, or was cleared).
    pub fn clear_room(&self) {
        self.update(|state| state.room = None);
    }

    pub fn set_login(&self, login: LoginStatus) {
        self.update(|state| state.login = login);
    }

    pub fn login(&self) -> LoginStatus {
        self.inner.state.lock().unwrap().login.clone()
    }

    pub fn set_rewards(&self, rewards: Vec<String>) {
        self.update(|state| state.rewards = rewards);
    }

    pub fn set_config_error(&self, error: Option<String>) {
        self.update(|state| state.config_error = error);
    }

    pub fn set_speech(&self, speech: SpeechStatus) {
        self.update(|state| state.speech = speech);
    }

    pub fn speech(&self) -> SpeechStatus {
        self.inner.state.lock().unwrap().speech.clone()
    }

    pub fn set_control_key(&self, key: String) {
        self.update(|state| state.control_key = key);
    }

    pub fn set_log(&self, log: LogStatus) {
        self.update(|state| state.log = log);
    }

    pub fn set_update(&self, update: UpdateStatus) {
        self.update(|state| state.update = update);
    }

    /// Counts an overlay (the one its link names, if any), hosting these
    /// plugins' pages, as connected until the guard is dropped.
    pub fn overlay_connected(&self, overlay: Option<String>, plugins: Vec<String>) -> OverlayGuard {
        let mut id = 0;
        self.update(|state| {
            id = state.next_overlay;
            state.next_overlay += 1;
            state.overlays.insert(id, OverlayStatus { overlay, plugins });
        });
        OverlayGuard { hub: self.clone(), id }
    }

    /// The whole status, with emote counts for the chat channel.
    pub fn snapshot(&self, emotes: &EmoteRegistry, restart_needed: Vec<String>) -> Status {
        let state = self.inner.state.lock().unwrap();
        let (counts, global_emotes) = match &state.room {
            Some((platform, id, _)) => emotes.counts(platform, id),
            None => (Vec::new(), emotes.counts("twitch", "").1),
        };
        Status {
            chat: state.chat.clone(),
            login: state.login.clone(),
            overlays: state.overlays.values().cloned().collect(),
            emotes: counts.into_iter().map(|(source, count)| EmoteCount { source, count }).collect(),
            global_emotes,
            rewards: state.rewards.clone(),
            restart_needed,
            config_error: state.config_error.clone(),
            speech: state.speech.clone(),
            control_key: state.control_key.clone(),
            log: state.log.clone(),
            version: crate::update::RUNNING.to_string(),
            update: state.update.clone(),
        }
    }
}

pub struct OverlayGuard {
    hub: StatusHub,
    id: u64,
}

impl OverlayGuard {
    /// Its plugin pages changed (one switched on or off).
    pub fn set_plugins(&self, plugins: Vec<String>) {
        let id = self.id;
        self.hub.update(|state| {
            if let Some(overlay) = state.overlays.get_mut(&id) {
                overlay.plugins = plugins;
            }
        });
    }
}

impl Drop for OverlayGuard {
    fn drop(&mut self) {
        let id = self.id;
        self.hub.update(|state| {
            state.overlays.remove(&id);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlays_count_while_connected() {
        let hub = StatusHub::new();
        let registry = EmoteRegistry::for_tests();
        let first = hub.overlay_connected(Some("default".into()), vec![]);
        let second = hub.overlay_connected(Some("chat".into()), vec!["chat".into()]);
        assert_eq!(hub.snapshot(&registry, vec![]).overlays.len(), 2);
        drop(first);
        second.set_plugins(vec!["chat".into(), "rain".into()]);
        let status = hub.snapshot(&registry, vec![]);
        assert_eq!(
            status.overlays,
            [OverlayStatus { overlay: Some("chat".into()), plugins: vec!["chat".into(), "rain".into()] }]
        );
    }
}
