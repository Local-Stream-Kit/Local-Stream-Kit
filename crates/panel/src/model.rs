//! The panel's state: the config as edited here, the server's live status,
//! whether changes are saved, and undo / redo.
//!
//! The config here is the server's with the panel's unsaved edits on top.
//! A config the server sends (a Stream Deck toggle, another panel, a hand
//! edit) doesn't replace it: what changed there is made here too
//! (`rebase`), in the undo and redo steps as well, so neither a save nor an
//! undo puts back what was changed elsewhere. Saves name the revision they
//! were made on, and the server refuses one made on an older config.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::rc::Rc;

use protocol::activity::{HistoryEntry, Prepared, QueueState, within_limit};
use protocol::cards::Card;
use protocol::config::AppConfig;
use protocol::panel::{KnownEmote, MediaFolder, PanelEvent, Status};
use protocol::plugin::PluginLook;
use protocol::plugin::{Catalog, PluginFailure};
use protocol::viewers::{FoundViewer, LineKind, ViewerAction, ViewerCard, ViewerInfo};
use yew::Reducible;

use crate::rebase::rebase;

/// How many config versions undo keeps.
const UNDO_LIMIT: usize = 100;
/// Edits with the same key within this many ms are one undo step (a slider
/// being dragged, a field being typed into).
const COALESCE_MS: f64 = 1_500.0;

#[derive(Debug, Clone, PartialEq)]
pub enum SaveState {
    Saved,
    /// Edited; a save is due shortly.
    Pending,
    Saving,
    Failed(Vec<String>),
}

#[derive(Clone, PartialEq)]
pub struct Model {
    pub connected: bool,
    /// `None` until the server has sent it.
    pub config: Option<AppConfig>,
    /// The server's config as far as this panel knows: what `config`, undo
    /// and redo are edits of.
    server: Option<AppConfig>,
    /// Its revision, which saves name.
    pub revision: u64,
    /// Saves sent without an answer yet, oldest first.
    sending: VecDeque<AppConfig>,
    pub status: Status,
    pub save: SaveState,
    /// Bumped on every local edit; the app saves after it settles.
    pub edits: u64,
    /// The panel build this page runs: the first one the server named.
    build: Option<String>,
    /// The server now serves a different build: reload once edits are
    /// saved.
    pub reload: bool,
    /// The chat channel's emotes, for completing codes; asked for when an
    /// emote field gets the focus.
    pub emotes: Rc<Vec<KnownEmote>>,
    /// The pictures alerts can show (asked for on the Plugins page).
    pub pictures: Rc<Vec<String>>,
    /// The folders in the pictures folder, with their files.
    pub picture_folders: Rc<Vec<MediaFolder>>,
    /// The sounds there are (asked for where a sound is picked).
    pub sounds: Rc<Vec<String>>,
    /// The folders in the sounds folder, with their files.
    pub sound_folders: Rc<Vec<MediaFolder>>,
    /// The Speak box's Try text is being made or read.
    pub speech_reading: bool,
    pub queue: Rc<QueueState>,
    /// What preparing the waiting events again last changed, until the
    /// note on it is closed.
    pub queue_note: Option<Prepared>,
    /// The waiting items whose text changed last (their event prepared
    /// again): they flash while the note shows.
    pub queue_changed: Rc<HashSet<u64>>,
    /// Newest first.
    pub history: Rc<Vec<HistoryEntry>>,
    /// The seen marker: the newest entry marked seen.
    pub history_seen: u64,
    /// Plugins' file settings' looks, as saved, by plugin and key.
    pub plugin_files: Rc<PluginFiles>,
    /// A look as edited here and not saved, by plugin and key.
    pub file_drafts: Rc<BTreeMap<(String, String), FileDraft>>,
    /// Which plugins' secret settings have a value saved: keys by plugin
    /// (never the values).
    pub plugin_secrets: Rc<BTreeMap<String, Vec<String>>>,
    /// The lists of choices plugins' server code filled in, by plugin and
    /// list name (`suggest_from`).
    pub plugin_options: Rc<BTreeMap<String, BTreeMap<String, Vec<protocol::plugin::Choice>>>>,
    /// The installed plugins: their node types are attached to the
    /// config's plugin nodes as it arrives.
    pub plugins: Rc<Catalog>,
    /// Plugin folders the server couldn't load, with why.
    pub plugin_failures: Rc<Vec<PluginFailure>>,
    /// Why a plugin couldn't be removed or forgotten, for the Plugins page.
    pub plugin_problem: Option<String>,
    /// The viewer card open in Activity.
    pub viewer: Option<Rc<ViewerState>>,
    /// The card asked for and not here yet: its id, else the name typed
    /// (lowercase). Only its answer is taken.
    opening: Option<String>,
    /// Find a viewer: the text the server answered for, and who it found.
    pub found: Rc<(String, Vec<FoundViewer>)>,
    /// Why the viewer asked for has no card: the name, and why.
    pub viewer_missing: Option<(String, String)>,
    /// The empty card just made on the New card page, as it was made:
    /// left unchanged, it goes again. One from a preset stays.
    new_card: Option<Card>,
    undo: Vec<AppConfig>,
    redo: Vec<AppConfig>,
    last_edit: Option<(String, f64)>,
}

impl Default for Model {
    fn default() -> Self {
        Model {
            connected: false,
            config: None,
            server: None,
            revision: 0,
            sending: VecDeque::new(),
            status: Status::default(),
            save: SaveState::Saved,
            edits: 0,
            build: None,
            reload: false,
            emotes: Rc::default(),
            pictures: Rc::default(),
            picture_folders: Rc::default(),
            sounds: Rc::default(),
            sound_folders: Rc::default(),
            speech_reading: false,
            queue: Rc::default(),
            queue_note: None,
            queue_changed: Rc::default(),
            history: Rc::default(),
            history_seen: 0,
            plugin_files: Rc::default(),
            file_drafts: Rc::default(),
            plugin_secrets: Rc::default(),
            plugin_options: Rc::default(),
            plugins: Rc::default(),
            plugin_failures: Rc::default(),
            plugin_problem: None,
            viewer: None,
            opening: None,
            found: Rc::default(),
            viewer_missing: None,
            new_card: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
        }
    }
}

impl Model {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The empty card just made on the New card page, while its page is
    /// open.
    pub fn new_card_id(&self) -> Option<&str> {
        self.new_card.as_ref().map(|card| card.id.as_str())
    }
}

/// A change to the config.
pub type Change = Rc<dyn Fn(&mut AppConfig)>;

/// The viewer card open in Activity, and what's going on with it.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewerState {
    pub card: ViewerCard,
    /// What Twitch says about them, once it has.
    pub info: Option<ViewerInfo>,
    /// Why Twitch couldn't be asked.
    pub info_problem: Option<String>,
    /// An action sent and not answered yet.
    pub pending: Option<ViewerAction>,
    /// Why the last action didn't happen, for people.
    pub problem: Option<String>,
}

// Actions live for one dispatch; boxing the server's events would only
// make every match on them clumsier.
#[allow(clippy::large_enum_variant)]
pub enum Action {
    Connected(bool),
    Server(PanelEvent),
    /// A local edit. Edits with the same `key` in quick succession are one
    /// undo step.
    Edit {
        key: Option<String>,
        change: Change,
    },
    /// A card made on the New card page, added at the end. An empty one
    /// is kept track of until its page is left.
    AddCard(Card),
    /// The page isn't the new card's any more: unchanged, it's taken out
    /// again, with its undo step.
    LeftNewCard,
    Undo,
    Redo,
    /// The pending save went out, with this config.
    Saving(AppConfig),
    /// The note on what preparing the waiting events again changed was
    /// closed.
    QueueNoteClosed,
    /// A plugin's file setting's look edited here (`None`: as saved again).
    FileDraft {
        plugin: String,
        key: String,
        draft: Option<FileDraft>,
    },
    /// A viewer's card was asked for: by id, else by the name typed.
    OpenViewer(String),
    CloseViewer,
    /// An action on the open card's viewer went out.
    Moderating(ViewerAction),
}

/// Every plugin's file settings' looks, by plugin and key.
pub type PluginFiles = BTreeMap<String, BTreeMap<String, Vec<PluginLook>>>;

/// A file setting's look as typed here, not saved yet.
#[derive(Debug, Clone, PartialEq)]
pub struct FileDraft {
    /// The look it's saved as: one of the user's, or a new copy.
    pub name: String,
    pub text: String,
    /// A new copy: of this one of the plugin's own looks (it went back to
    /// it if thrown away).
    pub copy_of: Option<String>,
}

impl Reducible for Model {
    type Action = Action;

    fn reduce(self: Rc<Self>, action: Action) -> Rc<Self> {
        let mut model = (*self).clone();
        match action {
            Action::Connected(connected) => {
                model.connected = connected;
                // Saves sent on the old connection get no answer; the
                // server's config on connecting says what's there.
                model.sending.clear();
                if model.save == SaveState::Saving {
                    model.save = SaveState::Pending;
                }
            }
            Action::Server(PanelEvent::Build { id }) => match &model.build {
                None => model.build = Some(id),
                Some(build) => model.reload |= *build != id,
            },
            Action::Server(PanelEvent::Config { mut config, revision }) => {
                config.attach_plugins(&model.plugins);
                model.server_changed(config, revision);
                // Edits waiting to be saved (made offline, or refused as
                // outdated) go out shortly, made on this config.
                if model.save == SaveState::Pending && model.config != model.server {
                    model.edit_done();
                }
            }
            Action::Server(PanelEvent::Plugins { plugins, failures }) => {
                let catalog = Catalog::new(plugins);
                for config in model
                    .config
                    .iter_mut()
                    .chain(&mut model.server)
                    .chain(&mut model.sending)
                    .chain(&mut model.undo)
                    .chain(&mut model.redo)
                {
                    config.attach_plugins(&catalog);
                }
                model.plugins = Rc::new(catalog);
                model.plugin_failures = Rc::new(failures);
                model.plugin_problem = None;
            }
            Action::Server(PanelEvent::PluginProblem { message }) => model.plugin_problem = Some(message),
            Action::Server(PanelEvent::Status { status }) => model.status = status,
            Action::Server(PanelEvent::Emotes { emotes }) => model.emotes = Rc::new(emotes),
            Action::Server(PanelEvent::Pictures { files, folders }) => {
                model.pictures = Rc::new(files);
                model.picture_folders = Rc::new(folders);
            }
            Action::Server(PanelEvent::Sounds { files, folders }) => {
                model.sounds = Rc::new(files);
                model.sound_folders = Rc::new(folders);
            }
            Action::Server(PanelEvent::SpeechPreview { reading }) => model.speech_reading = reading,
            Action::Server(PanelEvent::Queue { queue }) => {
                let before: BTreeMap<u64, &str> =
                    model.queue.waiting.iter().map(|item| (item.id, item.text.as_str())).collect();
                let changed: HashSet<u64> = queue
                    .waiting
                    .iter()
                    .filter(|item| before.get(&item.id).is_some_and(|text| *text != item.text))
                    .map(|item| item.id)
                    .collect();
                if !changed.is_empty() {
                    model.queue_changed = Rc::new(changed);
                }
                model.queue = Rc::new(queue);
            }
            Action::Server(PanelEvent::QueuePrepared { prepared }) => model.queue_note = Some(prepared),
            Action::Server(PanelEvent::History { entries }) => model.history = Rc::new(entries),
            Action::Server(PanelEvent::HistorySeen { up_to }) => model.history_seen = up_to,
            Action::Server(PanelEvent::HistoryEntry { entry }) => {
                let history = Rc::make_mut(&mut model.history);
                match history.iter_mut().find(|known| known.id == entry.id) {
                    Some(known) => *known = entry,
                    None => {
                        history.insert(0, entry);
                        history.retain(within_limit(|entry: &HistoryEntry| entry.kind));
                    }
                }
            }
            // As a text box gives it back, so an untouched one reads as
            // unchanged.
            Action::Server(PanelEvent::PluginFiles { mut files }) => {
                for look in files.values_mut().flat_map(|files| files.values_mut()).flatten() {
                    look.text = look.text.replace("\r\n", "\n");
                }
                model.plugin_files = Rc::new(files);
            }
            Action::Server(PanelEvent::PluginSecrets { saved }) => model.plugin_secrets = Rc::new(saved),
            Action::OpenViewer(key) => {
                model.opening = Some(key);
                model.viewer_missing = None;
            }
            Action::CloseViewer => {
                model.viewer = None;
                model.opening = None;
            }
            Action::Moderating(action) => {
                let Some(viewer) = &mut model.viewer else { return self };
                let viewer = Rc::make_mut(viewer);
                viewer.pending = Some(action);
                viewer.problem = None;
            }
            Action::Server(PanelEvent::ViewersFound { text, viewers }) => {
                model.found = Rc::new((text, viewers))
            }
            Action::Server(PanelEvent::Viewer { card }) => {
                let asked = model.opening.as_deref().is_some_and(|key| {
                    key == card.id || key == card.login.to_lowercase() || key == card.name.to_lowercase()
                });
                // Asked for, or the open one again (after a reconnect).
                let again = model.viewer.as_ref().is_some_and(|open| open.card.id == card.id);
                if !asked && !again {
                    return self;
                }
                let keep = model.viewer.take().filter(|open| open.card.id == card.id);
                model.opening = None;
                model.viewer = Some(Rc::new(ViewerState {
                    info: keep.as_ref().and_then(|open| open.info.clone()),
                    info_problem: None,
                    pending: None,
                    problem: None,
                    card,
                }));
            }
            // The answer to the card asked for last.
            Action::Server(PanelEvent::ViewerMissing { name, problem }) => {
                if model.opening.is_none() {
                    return self;
                }
                model.opening = None;
                model.viewer_missing = Some((name, problem));
            }
            Action::Server(PanelEvent::ViewerInfo { id, info, problem }) => {
                let Some(viewer) = model.viewer.as_mut().filter(|open| open.card.id == id) else {
                    return self;
                };
                let viewer = Rc::make_mut(viewer);
                viewer.info = info.or(viewer.info.take());
                viewer.info_problem = problem;
            }
            Action::Server(PanelEvent::ViewerOlder { id, lines, older }) => {
                let Some(viewer) = model.viewer.as_mut().filter(|open| open.card.id == id) else {
                    return self;
                };
                let card = &mut Rc::make_mut(viewer).card;
                if older < card.older {
                    let mut all = lines;
                    all.append(&mut card.lines);
                    card.lines = all;
                    card.older = older;
                }
            }
            Action::Server(PanelEvent::ViewerLine { id, line }) => {
                let Some(viewer) = model.viewer.as_mut().filter(|open| open.card.id == id) else {
                    return self;
                };
                let card = &mut Rc::make_mut(viewer).card;
                if matches!(line.kind, LineKind::Message { .. }) {
                    card.messages += 1;
                }
                card.lines.push(line);
            }
            Action::Server(PanelEvent::ViewerMessageDeleted { id, message }) => {
                let Some(viewer) = model.viewer.as_mut().filter(|open| open.card.id == id) else {
                    return self;
                };
                for line in &mut Rc::make_mut(viewer).card.lines {
                    if let LineKind::Message { id, deleted, .. } = &mut line.kind
                        && *id == message
                    {
                        *deleted = true;
                    }
                }
            }
            Action::Server(PanelEvent::Moderated { id, action, problem }) => {
                let Some(viewer) = model.viewer.as_mut().filter(|open| open.card.id == id) else {
                    return self;
                };
                let viewer = Rc::make_mut(viewer);
                viewer.pending = None;
                viewer.problem = problem
                    .map(|problem| format!("Couldn't {} {}: {problem}", action.verb(), viewer.card.name));
            }
            Action::Server(PanelEvent::PluginOptions { options }) => model.plugin_options = Rc::new(options),
            Action::QueueNoteClosed => model.queue_note = None,
            Action::FileDraft { plugin, key, draft } => {
                let drafts = Rc::make_mut(&mut model.file_drafts);
                match draft {
                    Some(draft) => drafts.insert((plugin, key), draft),
                    None => drafts.remove(&(plugin, key)),
                };
            }
            Action::Server(PanelEvent::Saved { revision }) => {
                // The server has what was sent: undo and redo are edits of
                // it as they are.
                if let Some(sent) = model.sending.pop_front() {
                    model.server = Some(sent);
                    model.revision = revision;
                }
                if model.save == SaveState::Saving && model.sending.is_empty() {
                    model.save = SaveState::Saved;
                }
            }
            Action::Server(PanelEvent::SaveOutdated) => {
                // Made on an older config: the newer one comes (or came)
                // with its changes, the edits go on top, and are saved
                // again shortly.
                model.sending.pop_front();
                if !matches!(model.save, SaveState::Failed(_)) {
                    model.edit_done();
                }
            }
            Action::Server(PanelEvent::SaveFailed { problems }) => {
                model.sending.pop_front();
                model.save = SaveState::Failed(problems);
            }
            Action::Edit { key, change } => {
                if !model.edit(key, &*change) {
                    return self;
                }
            }
            Action::AddCard(card) => {
                let added = card.clone();
                if !model.edit(None, &move |config: &mut AppConfig| config.cards.push(added.clone())) {
                    return self;
                }
                model.new_card = card.nodes.is_empty().then_some(card);
            }
            Action::LeftNewCard => {
                let Some(card) = model.new_card.take() else { return self };
                let Some(config) = &model.config else { return Rc::new(model) };
                if !config.cards.contains(&card) {
                    return Rc::new(model);
                }
                let mut without = config.clone();
                without.cards.retain(|each| each.id != card.id);
                // Usually the step that added it is the last one: it goes,
                // as if it had never been made. Otherwise taking it out is
                // a step of its own.
                if model.undo.last() == Some(&without) {
                    model.undo.pop();
                } else {
                    model.undo.push(config.clone());
                }
                model.redo.clear();
                model.config = Some(without);
                model.last_edit = None;
                model.edit_done();
            }
            Action::Undo => {
                let (Some(previous), Some(current)) = (model.undo.pop(), model.config.take()) else {
                    return self;
                };
                model.redo.push(current);
                model.config = Some(previous);
                model.last_edit = None;
                model.edit_done();
            }
            Action::Redo => {
                let (Some(next), Some(current)) = (model.redo.pop(), model.config.take()) else {
                    return self;
                };
                model.undo.push(current);
                model.config = Some(next);
                model.last_edit = None;
                model.edit_done();
            }
            Action::Saving(config) => {
                model.sending.push_back(config);
                model.save = SaveState::Saving;
            }
        }
        Rc::new(model)
    }
}

impl Model {
    /// The server's config changed (or came for the first time): what
    /// changed there is made in the config here and in every undo and redo
    /// step, so they keep only this panel's own edits.
    fn server_changed(&mut self, config: AppConfig, revision: u64) {
        self.revision = revision;
        let Some(old) = self.server.replace(config.clone()) else {
            self.config = Some(config);
            return;
        };
        if old == config {
            return;
        }
        let plugins = &self.plugins;
        for each in self.config.iter_mut().chain(&mut self.undo).chain(&mut self.redo) {
            *each = rebase(each, &old, &config, plugins);
        }
    }

    /// Applies a local edit; `false` if it changed nothing.
    fn edit(&mut self, key: Option<String>, change: &dyn Fn(&mut AppConfig)) -> bool {
        let Some(config) = &self.config else { return false };
        let mut edited = config.clone();
        change(&mut edited);
        if &edited == config {
            return false;
        }
        // The clock only for keyed edits (unit tests run outside a browser).
        let now = key.as_ref().map(|_| js_sys::Date::now());
        let coalesce = match (&key, &self.last_edit, now) {
            (Some(key), Some((last, at)), Some(now)) => key == last && now - at < COALESCE_MS,
            _ => false,
        };
        if !coalesce {
            self.undo.push(config.clone());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.last_edit = key.zip(now);
        self.redo.clear();
        self.config = Some(edited);
        self.edit_done();
        true
    }

    fn edit_done(&mut self) {
        self.edits += 1;
        self.save = SaveState::Pending;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(model: Rc<Model>, id: &str) -> Rc<Model> {
        model.reduce(Action::Server(PanelEvent::Build { id: id.into() }))
    }

    fn plain_card(id: &str, enabled: bool) -> Card {
        Card { id: id.into(), name: id.into(), enabled, nodes: Vec::new(), wires: Vec::new() }
    }

    /// Two cards, both off: "a" is switched here, "b" elsewhere.
    fn two_cards() -> AppConfig {
        AppConfig { cards: vec![plain_card("a", false), plain_card("b", false)], ..AppConfig::default() }
    }

    fn server_config(model: Rc<Model>, config: &AppConfig, revision: u64) -> Rc<Model> {
        model.reduce(Action::Server(PanelEvent::Config { config: config.clone(), revision }))
    }

    fn switch(id: &'static str, on: bool) -> Action {
        Action::Edit { key: None, change: Rc::new(move |config| card_mut(config, id).enabled = on) }
    }

    fn card_mut<'a>(config: &'a mut AppConfig, id: &str) -> &'a mut Card {
        config.cards.iter_mut().find(|card| card.id == id).unwrap()
    }

    fn switches(model: &Model) -> Vec<bool> {
        model.config.as_ref().unwrap().cards.iter().map(|card| card.enabled).collect()
    }

    /// "b" switched on elsewhere: the server's next config.
    fn b_on_elsewhere(model: &Model) -> AppConfig {
        let mut config = model.server.clone().unwrap();
        card_mut(&mut config, "b").enabled = true;
        config
    }

    #[test]
    fn a_change_elsewhere_while_a_save_is_due_is_kept() {
        let model = server_config(Rc::new(Model::default()), &two_cards(), 1);
        let model = model.reduce(switch("a", true));
        assert_eq!(model.save, SaveState::Pending);
        let edits = model.edits;
        let changed = b_on_elsewhere(&model);
        let model = server_config(model, &changed, 2);
        assert_eq!(switches(&model), [true, true], "both: the edit here and the one elsewhere");
        assert_eq!(model.revision, 2, "the save names the newer revision");
        assert!(model.edits > edits, "the save is due again, on the newer config");
    }

    #[test]
    fn a_change_elsewhere_while_a_save_is_on_its_way_is_kept() {
        let model = server_config(Rc::new(Model::default()), &two_cards(), 1);
        let model = model.reduce(switch("a", true));
        let sent = model.config.clone().unwrap();
        let model = model.reduce(Action::Saving(sent));
        // The change elsewhere comes first; the server refuses the save,
        // made on revision 1.
        let changed = b_on_elsewhere(&model);
        let model = server_config(model, &changed, 2);
        assert_eq!(switches(&model), [true, true]);
        assert_eq!(model.save, SaveState::Saving);
        let model = model.reduce(Action::Server(PanelEvent::SaveOutdated));
        assert_eq!(model.save, SaveState::Pending, "saved again shortly");
        // That one is saved, and comes back.
        let sent = model.config.clone().unwrap();
        let model = model.reduce(Action::Saving(sent.clone()));
        let model = model.reduce(Action::Server(PanelEvent::Saved { revision: 3 }));
        let model = server_config(model, &sent, 3);
        assert_eq!(model.save, SaveState::Saved);
        assert_eq!(switches(&model), [true, true]);

        // Undo takes back only this panel's switch.
        let model = model.reduce(Action::Undo);
        assert_eq!(switches(&model), [false, true]);
        let model = model.reduce(Action::Redo);
        assert_eq!(switches(&model), [true, true]);
    }

    #[test]
    fn undo_after_a_change_elsewhere_keeps_it() {
        let model = server_config(Rc::new(Model::default()), &two_cards(), 1);
        let model = model.reduce(switch("a", true));
        let sent = model.config.clone().unwrap();
        let model = model.reduce(Action::Saving(sent.clone()));
        let model = model.reduce(Action::Server(PanelEvent::Saved { revision: 2 }));
        let model = server_config(model, &sent, 2);
        assert_eq!(model.save, SaveState::Saved);

        // "b" on elsewhere, then undo here: "a" off again, "b" stays on.
        let changed = b_on_elsewhere(&model);
        let model = server_config(model, &changed, 3);
        assert_eq!(model.save, SaveState::Saved, "nothing of ours to save");
        assert_eq!(switches(&model), [true, true]);
        let model = model.reduce(Action::Undo);
        assert_eq!(switches(&model), [false, true]);
        assert_eq!(model.save, SaveState::Pending);
        // Redo, after "b" went off again elsewhere: "a" on, "b" off.
        let mut off = model.server.clone().unwrap();
        card_mut(&mut off, "b").enabled = false;
        let model = server_config(model, &off, 4);
        assert_eq!(switches(&model), [false, false]);
        let model = model.reduce(Action::Redo);
        assert_eq!(switches(&model), [true, false]);
    }

    #[test]
    fn edits_made_offline_go_on_top_of_the_config_on_reconnecting() {
        let model = server_config(Rc::new(Model::default()), &two_cards(), 1);
        let model = model.reduce(Action::Connected(false)).reduce(switch("a", true));
        // The app restarted meanwhile, with "b" on.
        let mut changed = two_cards();
        card_mut(&mut changed, "b").enabled = true;
        let edits = model.edits;
        let model = server_config(model.reduce(Action::Connected(true)), &changed, 1_000);
        assert_eq!(switches(&model), [true, true]);
        assert_eq!((model.revision, model.save.clone()), (1_000, SaveState::Pending));
        assert!(model.edits > edits, "a save is due");

        // A save that got no answer before the connection dropped is due
        // again too.
        let sent = model.config.clone().unwrap();
        let model = model.reduce(Action::Saving(sent)).reduce(Action::Connected(false));
        assert_eq!(model.save, SaveState::Pending);
        let edits = model.edits;
        let model = server_config(model.reduce(Action::Connected(true)), &changed, 1_000);
        assert!(model.edits > edits);
    }

    #[test]
    fn history_entries_are_added_on_top_or_replaced() {
        let entry = |id: u64, summary: &str| HistoryEntry {
            id,
            at_ms: 0,
            kind: protocol::activity::EventKind::Raid,
            summary: summary.into(),
            amount: 0,
            message: String::new(),
            message_parts: Vec::new(),
            replay_of: None,
            names: Vec::new(),
            cards: Vec::new(),
            card_ids: Vec::new(),
            items: Vec::new(),
        };
        let model = Rc::new(Model::default())
            .reduce(Action::Server(PanelEvent::History { entries: vec![entry(1, "old")] }));
        let model = model.reduce(Action::Server(PanelEvent::HistoryEntry { entry: entry(2, "new") }));
        let model = model.reduce(Action::Server(PanelEvent::HistoryEntry { entry: entry(1, "changed") }));
        let summaries: Vec<&str> = model.history.iter().map(|entry| entry.summary.as_str()).collect();
        assert_eq!(summaries, ["new", "changed"]);
    }

    #[test]
    fn an_unchanged_empty_new_card_goes_when_left() {
        let card = |id: &str| Card {
            id: id.into(),
            name: id.into(),
            enabled: true,
            nodes: Vec::new(),
            wires: Vec::new(),
        };
        let config = AppConfig { cards: vec![card("old")], ..AppConfig::default() };
        let model = Rc::new(Model::default())
            .reduce(Action::Server(PanelEvent::Config { config: config.clone(), revision: 1 }));

        // Made and left as it was: gone, and nothing to undo.
        let left = model.clone().reduce(Action::AddCard(card("new"))).reduce(Action::LeftNewCard);
        assert_eq!(left.config.as_ref(), Some(&config));
        assert!(!left.can_undo());

        // From a preset (it has nodes): it stays, unchanged or not.
        let preset =
            protocol::cards::presets().into_iter().find(|preset| !(preset.card)("x").nodes.is_empty());
        let from_preset = (preset.unwrap().card)("new");
        let kept = model.clone().reduce(Action::AddCard(from_preset.clone())).reduce(Action::LeftNewCard);
        assert_eq!(kept.config.as_ref().unwrap().cards.last(), Some(&from_preset));
        assert!(kept.can_undo());

        // Changed: it stays.
        let renamed = model
            .clone()
            .reduce(Action::AddCard(card("new")))
            .reduce(Action::Edit {
                key: None,
                change: Rc::new(|config| config.cards[1].name = "Renamed".into()),
            })
            .reduce(Action::LeftNewCard);
        assert_eq!(renamed.config.as_ref().unwrap().cards.len(), 2);

        // Changed and changed back by undo: unchanged after all.
        let undone = model
            .reduce(Action::AddCard(card("new")))
            .reduce(Action::Edit { key: None, change: Rc::new(|config| config.cards[1].enabled = false) })
            .reduce(Action::Undo)
            .reduce(Action::LeftNewCard);
        assert_eq!(undone.config.as_ref(), Some(&config));
        assert!(!undone.can_undo() && !undone.can_redo());
    }

    #[test]
    fn a_new_build_asks_for_a_reload() {
        let model = build(Rc::new(Model::default()), "aa");
        assert!(!model.reload, "the first build is this page's own");
        let model = build(model, "aa");
        assert!(!model.reload, "reconnecting to the same build");
        let model = build(model, "bb");
        assert!(model.reload);
        let model = build(model, "aa");
        assert!(model.reload, "once asked, it stays asked");
    }
}
