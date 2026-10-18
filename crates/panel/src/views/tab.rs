//! A plugin's own tab (its manifest's `tab`, `#/tab/<id>`): its page in a
//! sandboxed iframe, as overlay pages are hosted, filling the dock under
//! the header. The page gets the plugin's settings (as its overlay page
//! does) and what its server code sends it (`send-to-tab`); what it sends
//! (`{ type: "send", data }`) goes to the server code (`on-tab`). It's
//! loaded when the tab opens and goes when another page does.

use gloo::events::EventListener;
use protocol::panel::PanelRequest;
use protocol::plugin::{
    MediaKind, PluginMessage, TAB_MESSAGE_LIMIT, TabPageMessage, file_settings, server_hosts_allowed,
    shared_settings, tab_path,
};
use serde_json::Value;
use wasm_bindgen::JsCast;
use yew::prelude::*;

use super::icon;
use super::sounds::preview_files;
use crate::Panel;
use crate::route::Route;

#[derive(Properties, PartialEq)]
pub struct PluginTabProps {
    /// The plugin's id.
    pub plugin: String,
}

fn post(frame: &NodeRef, message: &PluginMessage) {
    let window = frame.cast::<web_sys::HtmlIFrameElement>().and_then(|frame| frame.content_window());
    let text = serde_json::to_string(message).expect("plugin messages serialise");
    if let (Some(window), Ok(message)) = (window, js_sys::JSON::parse(&text)) {
        let _ = window.post_message(&message, "*");
    }
}

#[function_component]
pub fn PluginTab(props: &PluginTabProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let frame = use_node_ref();
    let loaded = use_state(|| false);
    let model = &panel.model;
    let info = model.plugins.plugin(&props.plugin).filter(|info| !info.program).cloned();
    let stored = model.config.as_ref().map(|config| config.plugin(&props.plugin));
    let on = stored.as_ref().is_some_and(|stored| stored.enabled);
    let tab = info.as_ref().and_then(|info| info.manifest.tab.clone()).filter(|_| on);

    // No tab (the plugin switched off, removed, or without one): its box on
    // the Plugins page, or Cards if it's gone.
    {
        let installed = info.is_some();
        let id = props.plugin.clone();
        use_effect_with(tab.is_some(), move |has_tab| {
            if !*has_tab {
                if installed { Route::Plugin(id) } else { Route::Cards }.go();
            }
        });
    }

    // Its settings, as its overlay page gets them (a folder's first file,
    // a file setting's look as edited here), when it has loaded and again
    // whenever they change.
    let settings = match (&info, &stored) {
        (Some(info), Some(stored)) => {
            let manifest = &info.manifest;
            let mut pick = |kind, folder: &str, every| {
                let folders = match kind {
                    MediaKind::Sound => &model.sound_folders,
                    MediaKind::Picture | MediaKind::Media => &model.picture_folders,
                };
                preview_files(folder, folders, kind, every)
            };
            let mut values = shared_settings(&manifest.settings, stored.shared_settings(), &mut pick);
            for (key, _, _) in file_settings(manifest) {
                let picked = values.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
                values.insert(key.to_string(), Value::String(panel.look_text(&props.plugin, key, &picked)));
            }
            values
        }
        _ => serde_json::Map::new(),
    };
    {
        let frame = frame.clone();
        use_effect_with((settings, *loaded), move |(values, loaded)| {
            if *loaded {
                post(&frame, &PluginMessage::Settings { values: values.clone() });
            }
        });
    }

    // What its server code sends, from when the tab opened: passed on once
    // the page has loaded, each once.
    let passed = use_mut_ref(|| model.tab_messages.back().map_or(0, |last| last.number));
    {
        let (frame, passed, plugin) = (frame.clone(), passed.clone(), props.plugin.clone());
        use_effect_with((model.tab_messages.clone(), *loaded), move |(messages, loaded)| {
            if *loaded {
                let mut passed = passed.borrow_mut();
                let after = *passed;
                for message in messages.iter().filter(|message| message.number > after) {
                    if message.plugin == plugin {
                        post(&frame, &PluginMessage::Message { data: message.data.clone() });
                    }
                    *passed = message.number;
                }
            }
        });
    }

    // What the page sends: only from its own iframe, and not too big.
    {
        let (frame, panel, plugin) = (frame.clone(), panel.clone(), props.plugin.clone());
        use_effect_with((), move |_| {
            let listener = EventListener::new(&gloo::utils::window(), "message", move |event| {
                let Some(event) = event.dyn_ref::<web_sys::MessageEvent>() else { return };
                let own = frame.cast::<web_sys::HtmlIFrameElement>().and_then(|frame| frame.content_window());
                let from_own = match (event.source(), own) {
                    (Some(source), Some(own)) => js_sys::Object::is(&source, &own),
                    _ => false,
                };
                if !from_own {
                    return;
                }
                let Some(text) =
                    js_sys::JSON::stringify(&event.data()).ok().and_then(|text| text.as_string())
                else {
                    return;
                };
                if text.len() > TAB_MESSAGE_LIMIT {
                    gloo::console::warn!(format!(
                        "{plugin}'s tab sent {} bytes, more than its server code takes",
                        text.len()
                    ));
                    return;
                }
                if let Ok(TabPageMessage::Send { data }) = serde_json::from_str(&text) {
                    panel.send(PanelRequest::TabMessage { plugin: plugin.clone(), data });
                }
            });
            move || drop(listener)
        });
    }

    let (Some(info), Some(tab)) = (info, tab) else { return html! {} };
    let onload = {
        let loaded = loaded.clone();
        Callback::from(move |_: Event| loaded.set(true))
    };
    // Its server code doesn't run: stopped, or its hosts not allowed yet.
    let allowed = stored.as_ref().is_some_and(|stored| server_hosts_allowed(&info, &stored.hosts));
    let not_running = info.server && (info.stopped.is_some() || !allowed);
    html! {
        <main class="tab-main">
            if not_running {
                <div class="notice warn tab-notice" role="status" id="tab-not-running">
                    { icon("warn") }
                    <span>
                        { format!("{} isn't running: see ", info.manifest.name) }
                        <a href={Route::Plugin(info.manifest.id.clone()).href()}>{ "Plugins" }</a>
                        { "." }
                    </span>
                </div>
            }
            <iframe ref={frame} class="tab-frame" id="tab-page" sandbox="allow-scripts"
                    src={tab_path(&info.manifest.id, &tab)} title={tab.label.clone()} {onload}></iframe>
        </main>
    }
}
