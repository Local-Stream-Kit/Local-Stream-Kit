//! The running config: what the control panel reads and saves, and what the
//! file watcher reloads. Every change goes through here, so the file, the
//! pipeline, the overlays and every open panel stay in step.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use protocol::config::AppConfig;
use protocol::plugin::Catalog;
use tokio::sync::{mpsc, watch};

use crate::config::{self, Config};
use crate::pipeline::Pipeline;
use crate::services::overlay_hub::OverlayHub;
use crate::services::status::StatusHub;

pub struct ConfigStore {
    path: PathBuf,
    /// The config the app started with: connection settings only apply
    /// after a restart, so changes are compared with this.
    running: AppConfig,
    state: Mutex<State>,
    pipelines: mpsc::Sender<Pipeline>,
    /// The plugins' node types, for the cards.
    plugins: Mutex<Catalog>,
    hub: OverlayHub,
    status: StatusHub,
    changes: watch::Sender<Arc<AppConfig>>,
    /// Held from reading the config to applying the change, so changes
    /// from the panel and the control API don't undo each other.
    writing: tokio::sync::Mutex<()>,
}

struct State {
    current: AppConfig,
    /// The file's text as last written or read, so the watcher can tell our
    /// own saves from hand edits.
    text: Option<String>,
    /// Bumped with every change to `current`, so a panel's save can tell
    /// whether it was made on the config as it is now.
    revision: u64,
}

/// Why a save from the panel didn't happen.
#[derive(Debug, PartialEq)]
pub enum SaveError {
    /// The config changed since the revision the save was made on.
    Outdated,
    /// It fails the checks (or couldn't be written).
    Problems(Vec<String>),
}

impl ConfigStore {
    pub fn new(
        config: &Config,
        pipelines: mpsc::Sender<Pipeline>,
        plugins: Catalog,
        hub: OverlayHub,
        status: StatusHub,
    ) -> Arc<ConfigStore> {
        let (changes, _) = watch::channel(Arc::new(config.app.clone()));
        Arc::new(ConfigStore {
            path: config.path.clone(),
            running: config.app.clone(),
            state: Mutex::new(State {
                current: config.app.clone(),
                text: std::fs::read_to_string(&config.path).ok(),
                // From the clock, so a panel that saw the app before a
                // restart doesn't take the new revisions for its own.
                revision: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(1, |since| since.as_millis() as u64),
            }),
            pipelines,
            plugins: Mutex::new(plugins),
            hub,
            status,
            changes,
            writing: tokio::sync::Mutex::new(()),
        })
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn current(&self) -> AppConfig {
        self.state.lock().unwrap().current.clone()
    }

    /// The current config and its revision, read together.
    pub fn current_revision(&self) -> (AppConfig, u64) {
        let state = self.state.lock().unwrap();
        (state.current.clone(), state.revision)
    }

    /// Notified whenever the config changes.
    pub fn subscribe(&self) -> watch::Receiver<Arc<AppConfig>> {
        self.changes.subscribe()
    }

    /// Saved connection settings that only apply after a restart.
    pub fn restart_needed(&self) -> Vec<String> {
        let current = self.state.lock().unwrap();
        self.running.needs_restart_for(&current.current).into_iter().map(str::to_string).collect()
    }

    /// Saves a config from the panel: checked, written, applied. With a
    /// `revision`, only if the config is still at it: otherwise the save
    /// would undo changes the panel hasn't seen. Answers the new revision.
    pub async fn save(&self, app: AppConfig, revision: Option<u64>) -> Result<u64, SaveError> {
        let _writing = self.writing.lock().await;
        if revision.is_some_and(|revision| revision != self.state.lock().unwrap().revision) {
            return Err(SaveError::Outdated);
        }
        self.write(app).await.map_err(SaveError::Problems)
    }

    /// Changes the current config (for the control API). `change` can
    /// refuse with an error, and nothing is saved; nor when it changed
    /// nothing.
    pub async fn update<T, E>(
        &self,
        change: impl FnOnce(&mut AppConfig) -> Result<T, E>,
    ) -> Result<Result<T, E>, Vec<String>> {
        let _writing = self.writing.lock().await;
        let mut app = self.current();
        let result = change(&mut app);
        if result.is_ok() && app != self.current() {
            self.write(app).await?;
        }
        Ok(result)
    }

    async fn write(&self, mut app: AppConfig) -> Result<u64, Vec<String>> {
        config::normalise(&mut app);
        let problems = app.problems();
        if !problems.is_empty() {
            return Err(problems);
        }
        config::write(&self.path, &app).map_err(|error| vec![format!("{error:#}")])?;
        let revision = {
            let mut state = self.state.lock().unwrap();
            state.text = std::fs::read_to_string(&self.path).ok();
            state.current = app.clone();
            state.revision += 1;
            state.revision
        };
        self.apply(app).await;
        Ok(revision)
    }

    /// The plugins changed (a program connected as one, or went): cards
    /// get the new node types. Nothing is rebuilt when they're the same.
    pub async fn set_plugins(&self, plugins: Catalog) {
        let _writing = self.writing.lock().await;
        {
            let mut current = self.plugins.lock().unwrap();
            let same = current.same_types(&plugins);
            *current = plugins;
            if same {
                return;
            }
        }
        self.apply(self.current()).await;
    }

    /// The watcher saw the file change: reload it, unless it's our own save.
    /// After any save in progress, so a hand edit read during a panel save
    /// isn't applied after it.
    pub async fn file_changed(&self) {
        let _writing = self.writing.lock().await;
        let text = std::fs::read_to_string(&self.path).ok();
        {
            let mut state = self.state.lock().unwrap();
            if text.is_none() || text == state.text {
                return;
            }
            state.text = text;
        }
        match Config::read(&self.path) {
            Ok(app) => {
                {
                    let mut state = self.state.lock().unwrap();
                    state.current = app.clone();
                    state.revision += 1;
                }
                self.apply(app).await;
                tracing::info!("reloaded {}", self.path.display());
            }
            Err(error) => {
                tracing::error!("{error:#}\nThe previous settings stay in use.");
                self.status.set_config_error(Some(format!("{error:#}")));
            }
        }
    }

    /// Builds a pipeline for `app` and swaps it in; overlays get the new
    /// effect settings straight away, and panels the new config.
    async fn apply(&self, app: AppConfig) {
        let restart = self.running.needs_restart_for(&app);
        if !restart.is_empty() {
            tracing::warn!(settings = restart.join(", "), "these changes take effect after a restart");
        }
        let pipeline = Pipeline::new(&app, &self.plugins.lock().unwrap());
        for command in pipeline.module_configs() {
            self.hub.set_module_config(command.clone());
            self.hub.send(command);
        }
        if self.pipelines.send(pipeline).await.is_err() {
            tracing::warn!("the pipeline has stopped; the new config isn't running");
        }
        self.status.set_config_error(None);
        self.changes.send_replace(Arc::new(app));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_hand_edit_waits_for_a_save_in_progress() {
        let dir = std::env::temp_dir().join(format!("lsk_config_store_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = Config::load_or_create(&dir).unwrap();
        let (pipelines, mut built) = mpsc::channel(4);
        let store =
            ConfigStore::new(&config, pipelines, Catalog::default(), OverlayHub::new(), StatusHub::new());
        let mut edited = config.app.clone();
        edited.connections.twitch_channel = "edited".into();
        config::write(&config.path, &edited).unwrap();

        let saving = store.writing.lock().await;
        let reloading = tokio::spawn({
            let store = Arc::clone(&store);
            async move { store.file_changed().await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!reloading.is_finished(), "waits for the save");
        assert_eq!(store.current().connections.twitch_channel, config.app.connections.twitch_channel);
        drop(saving);
        reloading.await.unwrap();
        assert_eq!(store.current().connections.twitch_channel, "edited");
        assert!(built.try_recv().is_ok(), "the edit is running");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
