//! The control panel: a Yew app served at `/panel/`, usable in a browser tab
//! and as an OBS custom browser dock. It edits the whole config (cards as
//! node graphs, effects, connections) through the server's WebSocket, shows
//! live status, logs in to Twitch and fires test events.

mod connection;
mod dropdown;
mod editor;
mod model;
mod rebase;
mod route;
mod util;
mod views;

use std::rc::Rc;

use gloo::events::EventListener;
use gloo::timers::callback::Timeout;
use protocol::config::AppConfig;
use protocol::panel::{LoginStatus, PanelRequest};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use yew::prelude::*;

use connection::Connection;
use model::{Action, Model, SaveState};
use route::Route;

/// Saves go out this long after the last edit.
const SAVE_AFTER_MS: u32 = 500;

/// What every view gets: the state, and ways to change it.
#[derive(Clone, PartialEq)]
pub struct Panel {
    pub model: UseReducerHandle<Model>,
    connection: Connection,
}

impl Panel {
    /// Sends a request to the server; `false` if it couldn't go (not
    /// connected).
    pub fn send(&self, request: PanelRequest) -> bool {
        self.connection.send(&request)
    }

    /// Sends the config as edited here, if there's a connection, naming
    /// the server's revision it was made on.
    pub fn save_now(&self) {
        let revision = Some(self.model.revision);
        if let Some(config) = self.model.config.clone()
            && self.send(PanelRequest::SaveConfig { config: config.clone(), revision })
        {
            self.model.dispatch(Action::Saving(config));
        }
    }

    /// Changes the config. Edits with the same `key` in quick succession
    /// (typing, dragging a slider) are one undo step.
    pub fn edit(&self, key: Option<&str>, change: impl Fn(&mut AppConfig) + 'static) {
        self.model.dispatch(Action::Edit { key: key.map(str::to_string), change: Rc::new(change) });
    }

    /// Adds a card made on the New card page. Left unchanged, it goes
    /// again when its page is left.
    pub fn add_card(&self, card: protocol::cards::Card) {
        self.model.dispatch(Action::AddCard(card));
    }

    /// The Activity view's note on what the cards' last change did to the
    /// queue was closed.
    pub fn close_queue_note(&self) {
        self.model.dispatch(Action::QueueNoteClosed);
    }

    pub fn undo(&self) {
        self.model.dispatch(Action::Undo);
    }

    pub fn redo(&self) {
        self.model.dispatch(Action::Redo);
    }

    /// A plugin's file setting's look as edited here; `None`: as saved.
    pub fn file_draft(&self, plugin: &str, key: &str, draft: Option<model::FileDraft>) {
        self.model.dispatch(Action::FileDraft { plugin: plugin.to_string(), key: key.to_string(), draft });
    }

    /// A plugin's file setting's looks, as saved.
    pub fn looks(&self, plugin: &str, key: &str) -> Vec<protocol::plugin::PluginLook> {
        self.model.plugin_files.get(plugin).and_then(|files| files.get(key)).cloned().unwrap_or_default()
    }

    /// A plugin's file setting's text as shown here: as typed, or the look
    /// its value (`picked`) picks.
    pub fn look_text(&self, plugin: &str, key: &str, picked: &str) -> String {
        if let Some(draft) = self.model.file_drafts.get(&(plugin.to_string(), key.to_string())) {
            return draft.text.clone();
        }
        let looks = self.looks(plugin, key);
        protocol::plugin::picked_look(&looks, picked).map(|look| look.text.clone()).unwrap_or_default()
    }
}

#[function_component]
fn App() -> Html {
    let model = use_reducer(Model::default);
    let route = route::use_route();

    let connection = {
        let model = model.clone();
        use_memo((), move |_| {
            let events = model.clone();
            Connection::open(
                Callback::from(move |event| events.dispatch(Action::Server(event))),
                Callback::from(move |connected| model.dispatch(Action::Connected(connected))),
            )
        })
    };
    let panel = Panel { model: model.clone(), connection: (*connection).clone() };

    // Save shortly after the last edit. Offline, it stays due.
    {
        let panel = panel.clone();
        use_effect_with(model.edits, move |_| {
            let pending = panel.model.save == SaveState::Pending;
            let timeout = pending.then(|| Timeout::new(SAVE_AFTER_MS, move || panel.save_now()));
            move || drop(timeout)
        });
    }

    // The server has a new build of the panel: reload, once nothing is
    // waiting to be saved (a failed save waits until it's fixed or undone).
    {
        let reload = model.reload && model.save == SaveState::Saved;
        use_effect_with(reload, move |reload| {
            if *reload {
                let _ = gloo::utils::window().location().reload();
            }
        });
    }

    // Leaving a new card's page (back, or anywhere else) with nothing
    // changed takes it out again.
    {
        let panel = panel.clone();
        use_effect_with(route.clone(), move |route| {
            if let Some(id) = panel.model.new_card_id()
                && *route != Route::Card(id.to_string())
            {
                panel.model.dispatch(Action::LeftNewCard);
            }
        });
    }

    // Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y, outside text fields.
    {
        let panel = panel.clone();
        use_effect_with((), move |_| {
            let document = gloo::utils::document();
            let listener = EventListener::new(&document, "keydown", move |event| {
                let event = event.dyn_ref::<web_sys::KeyboardEvent>().expect("a keyboard event");
                if util::typing_in_field(event) || !(event.ctrl_key() || event.meta_key()) {
                    return;
                }
                match event.key().to_lowercase().as_str() {
                    "z" if event.shift_key() => panel.redo(),
                    "z" => panel.undo(),
                    "y" => panel.redo(),
                    _ => return,
                }
                event.prevent_default();
            });
            move || drop(listener)
        });
    }

    let body = if model.config.is_none() {
        html! {
            <main class="loading">
                <p>{ if model.connected { "Loading…" } else { "Connecting to Local Stream Kit…" } }</p>
            </main>
        }
    } else {
        match &route {
            // The header stays above the editor, so the tabs are a click away.
            Route::Card(id) => html! {
                <div class="dock editing">
                    <Header route={route.clone()} />
                    <editor::CardEditor id={id.clone()} />
                </div>
            },
            route => html! {
                <div class="dock">
                    <Header route={route.clone()} />
                    <Banners />
                    <main class="dock-main">
                        { match route {
                            Route::Cards => html! { <views::cards::Cards /> },
                            Route::Activity => html! { <views::activity::Activity /> },
                            Route::NewCard => html! { <views::new_card::NewCard /> },
                            Route::Test => html! { <views::test::Test /> },
                            Route::Status => html! { <views::status::StatusView /> },
                            // The pages they were until 2026-10-14: Status,
                            // at their box.
                            Route::Login => html! { <views::status::StatusView focus="twitch" /> },
                            Route::Control => html! { <views::status::StatusView focus="control" /> },
                            Route::Plugins => html! { <views::plugins::PluginsView /> },
                            Route::Plugin(id) => html! { <views::plugins::PluginsView focus={id.clone()} /> },
                            Route::Card(_) => html! {},
                        } }
                    </main>
                </div>
            },
        }
    };
    html! {
        <ContextProvider<Panel> context={panel}>
            { body }
            <dropdown::Dropdowns />
        </ContextProvider<Panel>>
    }
}

#[derive(Properties, PartialEq)]
struct HeaderProps {
    route: Route,
}

#[function_component]
fn Header(props: &HeaderProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let status = &panel.model.status;
    let chat_ok = matches!(status.chat, protocol::panel::ChatStatus::Connected { .. });
    // Logged out is only chat (logging in is optional): no dot then.
    let points = match &status.login {
        LoginStatus::Off | LoginStatus::LoggedOut => None,
        LoginStatus::LoggedIn { listening: true, .. } | LoginStatus::TwitchCli { .. } => Some("ok"),
        LoginStatus::Waiting { .. } => Some("warn"),
        _ => Some("off"),
    };
    let overlays = status.overlays.len();
    let plugin_problems = views::plugins::problems(&panel.model);
    let queue = &panel.model.queue;
    let queued = queue.waiting_count + usize::from(queue.on_stage.is_some());
    let tab = |route: Route, label: &str| {
        let current = props.route == route
            || (route == Route::Cards && matches!(props.route, Route::NewCard | Route::Card(_)))
            || (route == Route::Plugins && matches!(props.route, Route::Plugin(_)))
            || (route == Route::Status && matches!(props.route, Route::Login | Route::Control));
        html! {
            <a href={route.href()} class={classes!("tab", current.then_some("current"))}
               aria-current={current.then_some("page")}>{ label.to_string() }</a>
        }
    };
    html! {
        <header class="dock-header">
            <div class="dock-title">
                <span class="app-name">{ "Local Stream Kit" }</span>
                <div class="dock-status">
                <a class="dots" href={Route::Status.href()} aria-label="Connection status">
                    <span class="dot-label"><span class={classes!("dot", if chat_ok { "ok" } else { "off" })}></span>{ "Chat" }</span>
                    if let Some(state) = points {
                        <span class="dot-label"><span class={classes!("dot", state)}></span>{ "Points" }</span>
                    }
                    <span class="dot-label">
                        <span class={classes!("dot", if overlays > 0 { "ok" } else { "off" })}></span>
                        { format!("{overlays} overlay{}", if overlays == 1 { "" } else { "s" }) }
                    </span>
                </a>
                if plugin_problems > 0 {
                    <a class="dots" id="header-plugins" href={Route::Plugins.href()}
                       title={if plugin_problems == 1 { "A plugin needs a look".to_string() } else { format!("{plugin_problems} plugins need a look") }}>
                        <span class="dot-label"><span class="dot warn"></span>{ "Plugins" }</span>
                    </a>
                }
                </div>
            </div>
            <nav class="tabs" aria-label="Sections">
                { tab(Route::Cards, "Cards") }
                { tab(Route::Activity, &if queued > 0 { format!("Activity ({queued})") } else { "Activity".to_string() }) }
                { tab(Route::Plugins, "Plugins") }
                { tab(Route::Test, "Test") }
                { tab(Route::Status, "Status") }
            </nav>
        </header>
    }
}

/// Connection, save and config problems, above every page.
#[function_component]
pub fn Banners() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let model = &panel.model;
    html! {
        <>
            if !model.connected {
                <div class="banner warn" role="status">{ "Reconnecting to Local Stream Kit… changes are kept here until it's back." }</div>
            }
            if let SaveState::Failed(problems) = &model.save {
                <div class="banner error" role="alert">
                    <strong>{ "Not saved: " }</strong>{ problems.join(" ") }
                </div>
            }
            if model.reload && model.save != SaveState::Saved {
                <div class="banner warn" role="status">{ "Local Stream Kit was updated; the panel reloads once your changes are saved." }</div>
            }
            if let Some(error) = &model.status.config_error {
                <div class="banner error" role="alert">
                    <strong>{ "lsk.json has a problem; the previous settings are in use. " }</strong>
                    <span class="mono">{ error.clone() }</span>
                </div>
            }
        </>
    }
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    yew::Renderer::<App>::new().render();
}
