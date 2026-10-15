//! Overlays (a box on Status): the overlays (OBS Browser Sources), each
//! with its link to copy or drag onto OBS and the plugins' pages it
//! shows, and the ones open in OBS that aren't on the list.

use protocol::config::{AppConfig, Overlay};
use protocol::panel::OverlayStatus;
use protocol::plugin::Catalog;
use yew::prelude::*;

use super::boxes::{CopyField, FoldBox, summary};
use super::icon;
use crate::{Panel, util};

/// Changes the overlay at `index`.
fn edit_overlay(panel: &Panel, index: usize, key: Option<String>, change: impl Fn(&mut Overlay) + 'static) {
    panel.edit(key.as_deref(), move |config: &mut AppConfig| {
        if let Some(overlay) = config.overlays.get_mut(index) {
            change(overlay);
        }
    });
}

/// The plugins with an overlay page, as `(id, name, own source)`.
fn page_plugins(catalog: &Catalog) -> Vec<(String, String, bool)> {
    catalog
        .plugins()
        .iter()
        .filter(|plugin| plugin.overlay)
        .map(|plugin| (plugin.manifest.id.clone(), plugin.manifest.name.clone(), plugin.manifest.own_source))
        .collect()
}

/// The size its Browser Source wants: the plugin's own (its preview's)
/// when it shows only one, otherwise OBS's canvas (`canvas`, while the OBS
/// plugin is connected; 2026-10-14, asked), or 1920 × 1080.
fn source_size(overlay: &Overlay, catalog: &Catalog, canvas: Option<(u32, u32)>) -> (u32, u32) {
    let whole = canvas.unwrap_or((1920, 1080));
    match overlay.plugins.as_deref() {
        Some([only]) => catalog
            .plugin(only)
            .and_then(|plugin| plugin.manifest.preview.as_ref())
            .map_or(whole, |preview| (preview.width, preview.height)),
        _ => whole,
    }
}

/// OBS's canvas, as the OBS plugin tells it (its `canvas` list,
/// "2560x1440", while it's connected and switched on).
fn obs_canvas(panel: &Panel) -> Option<(u32, u32)> {
    let config = panel.model.config.as_ref()?;
    if !config.plugin("obs").enabled {
        return None;
    }
    let size = &panel.model.plugin_options.get("obs")?.get("canvas")?.first()?.value;
    let (width, height) = size.split_once('x')?;
    let size = (width.parse().ok()?, height.parse().ok()?);
    (size.0 > 0 && size.1 > 0).then_some(size)
}

/// The port the app serves on now: the panel's own (the config's may be
/// waiting for a restart).
fn port(config: &AppConfig) -> u16 {
    gloo::utils::window()
        .location()
        .port()
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(config.connections.overlay_port)
}

/// A name and id for a new overlay: "Overlay", "Overlay 2", …
fn new_overlay(overlays: &[Overlay]) -> Overlay {
    let name = (1..)
        .map(|n| if n == 1 { "Overlay".to_string() } else { format!("Overlay {n}") })
        .find(|name| !overlays.iter().any(|overlay| overlay.name == *name))
        .expect("some number is free");
    Overlay { id: Overlay::free_id(&name, overlays), name, plugins: Some(Vec::new()) }
}

#[function_component]
pub fn OverlaysBox() -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    // The overlay that's open (its name, what it shows, Remove), by id.
    let open = use_state(|| None::<String>);
    let Some(config) = &panel.model.config else { return html! {} };
    let connected = &panel.model.status.overlays;
    let add = {
        let (panel, open) = (panel.clone(), open.clone());
        let id = new_overlay(&config.overlays).id;
        Callback::from(move |_: MouseEvent| {
            panel.edit(None, |config: &mut AppConfig| {
                let overlay = new_overlay(&config.overlays);
                config.overlays.push(overlay);
            });
            open.set(Some(id.clone()));
        })
    };
    // Open in OBS, but not on the list (removed, or a link typed by hand).
    let unlisted: Vec<&OverlayStatus> = connected
        .iter()
        .filter(|status| match &status.overlay {
            Some(id) => !config.overlays.iter().any(|overlay| overlay.id == *id),
            None => true,
        })
        .collect();

    let canvas = obs_canvas(&panel);
    // In OBS: the overlays with a Browser Source open, listed or not.
    let in_obs = config
        .overlays
        .iter()
        .filter(|overlay| {
            connected.iter().any(|status| status.overlay.as_deref() == Some(overlay.id.as_str()))
        })
        .count()
        + unlisted.len();
    let head =
        if in_obs > 0 { summary("ok", format!("{in_obs} in OBS")) } else { summary("off", "None in OBS") };
    html! {
        <FoldBox id="overlays" title="Overlays" summary={head} open={in_obs == 0}>
            { for config.overlays.iter().enumerate().map(|(index, overlay)| {
                let count = connected.iter().filter(|status| status.overlay.as_deref() == Some(overlay.id.as_str())).count();
                let is_open = open.as_deref() == Some(overlay.id.as_str());
                let flip = {
                    let (open, id) = (open.clone(), overlay.id.clone());
                    Callback::from(move |_: MouseEvent| open.set((!is_open).then(|| id.clone())))
                };
                html! {
                    <OverlayRow key={overlay.id.clone()} {index} overlay={overlay.clone()} connected={count}
                                open={is_open} onflip={flip} port={port(config)} {canvas} />
                }
            }) }
            { for unlisted.iter().enumerate().map(|(index, status)| {
                let names: Vec<String> = status
                    .plugins
                    .iter()
                    .map(|id| panel.model.plugins.plugin(id).map_or_else(|| id.clone(), |info| info.manifest.name.clone()))
                    .collect();
                let (title, text) = match &status.overlay {
                    Some(id) => (format!("“{id}”"), "Open in OBS, but not on the list: it shows nothing.".to_string()),
                    None => (
                        "A link naming plugins".to_string(),
                        if names.is_empty() { "Shows nothing.".to_string() } else { format!("Shows {}.", names.join(", ")) },
                    ),
                };
                let add_back = status.overlay.clone().map(|id| {
                    let panel = panel.clone();
                    Callback::from(move |_: MouseEvent| {
                        let id = id.clone();
                        panel.edit(None, move |config: &mut AppConfig| {
                            if !config.overlays.iter().any(|overlay| overlay.id == id) {
                                config.overlays.push(Overlay { id: id.clone(), name: id.clone(), plugins: None });
                            }
                        });
                    })
                });
                html! {
                    <div class="overlay-row unlisted" id={format!("overlay-unlisted-{index}")}>
                        <div class="overlay-head">
                            <span class="overlay-name"><span class="dot warn"></span>{ title }</span>
                            if let Some(add_back) = add_back {
                                <button type="button" class="button ghost small" onclick={add_back}>{ "Add it back" }</button>
                            }
                        </div>
                        <div class="muted small">{ text }</div>
                    </div>
                }
            }) }
            <div>
                <button type="button" class="button ghost small" id="overlays-add" onclick={add}>
                    { icon("plus") }{ "Overlay" }
                </button>
            </div>
        </FoldBox>
    }
}

#[derive(Properties, PartialEq)]
struct OverlayRowProps {
    index: usize,
    overlay: Overlay,
    /// How many Browser Sources have it open.
    connected: usize,
    open: bool,
    onflip: Callback<MouseEvent>,
    port: u16,
    /// OBS's canvas, when it's known.
    canvas: Option<(u32, u32)>,
}

#[function_component]
fn OverlayRow(props: &OverlayRowProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let (index, overlay) = (props.index, &props.overlay);
    let catalog = &panel.model.plugins;
    let pages = page_plugins(catalog);
    let base = format!("overlay-{}", overlay.id);
    let link = overlay.link(props.port);
    let (width, height) = source_size(overlay, catalog, props.canvas);
    let drop_link = overlay.obs_drop_link(props.port, (width, height));

    let summary = match &overlay.plugins {
        None => "Every effect".to_string(),
        Some(plugins) if plugins.is_empty() => "Nothing yet".to_string(),
        Some(plugins) => plugins
            .iter()
            .map(|id| catalog.plugin(id).map_or_else(|| id.clone(), |info| info.manifest.name.clone()))
            .collect::<Vec<_>>()
            .join(", "),
    };
    // A click does nothing: it's for dragging.
    let no_click = Callback::from(|event: MouseEvent| event.prevent_default());
    let rename = {
        let (panel, key) = (panel.clone(), format!("{base}-name"));
        Callback::from(move |event: InputEvent| {
            let name = util::value_of(&event);
            edit_overlay(&panel, index, Some(key.clone()), move |overlay| overlay.name = name.clone());
        })
    };
    let every = {
        let (panel, pages) = (panel.clone(), pages.clone());
        Callback::from(move |event: Event| {
            let every = util::value_of(&event) == "every";
            // "Only these" starts from what "Every effect" shows.
            let shown: Vec<String> =
                pages.iter().filter(|(_, _, own)| !own).map(|(id, _, _)| id.clone()).collect();
            edit_overlay(&panel, index, None, move |overlay| {
                overlay.plugins = if every { None } else { Some(shown.clone()) };
            });
        })
    };
    let remove = {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            panel.edit(None, move |config: &mut AppConfig| {
                if index < config.overlays.len() {
                    config.overlays.remove(index);
                }
            });
        })
    };
    let state = match props.connected {
        0 => ("off", "Not in OBS".to_string()),
        1 => ("ok", "In OBS".to_string()),
        n => ("ok", format!("In OBS ({n} sources)")),
    };

    html! {
        <div class="overlay-row" id={base.clone()}>
            <div class="overlay-head">
                <button type="button" class={classes!("box-fold", "overlay-name", props.open.then_some("open"))}
                        id={format!("{base}-open")} aria-expanded={props.open.to_string()} onclick={props.onflip.clone()}>
                    { icon("chevron") }
                    <span class={classes!("dot", state.0)} title={state.1.clone()}></span>
                    { if overlay.name.trim().is_empty() { overlay.id.clone() } else { overlay.name.clone() } }
                </button>
                <span class="muted small overlay-state">{ state.1 }</span>
            </div>
            <div class="muted small overlay-summary" id={format!("{base}-summary")}>
                { format!("{summary} · {width} × {height}") }
            </div>
            <div class="overlay-link">
                <CopyField id={format!("{base}-link")} value={link.clone()} what="the overlay's link" />
                <a class="button ghost small obs-drag" id={format!("{base}-drag")} href={drop_link} draggable="true"
                   title="Drag onto OBS's preview: it adds a Browser Source with this link, name and size"
                   onclick={no_click}>
                    { icon("grip") }{ "Drag into OBS" }
                </a>
            </div>
            if props.open {
                <div class="overlay-body">
                    <div class="field">
                        <label for={format!("{base}-name")}>{ "Name" }</label>
                        <input type="text" id={format!("{base}-name")} value={overlay.name.clone()} oninput={rename} />
                    </div>
                    <div class="field">
                        <label for={format!("{base}-shows")}>{ "Shows" }</label>
                        <select id={format!("{base}-shows")} onchange={every}>
                            <option value="every" selected={overlay.plugins.is_none()}>{ "Every effect, new ones too" }</option>
                            <option value="only" selected={overlay.plugins.is_some()}>{ "Only these" }</option>
                        </select>
                    </div>
                    if overlay.plugins.is_none() {
                        { for pages.iter().filter(|(_, _, own)| *own).map(|(_, name, _)| html! {
                            <p class="muted small">{ format!("Not {name}: it needs a Browser Source of its own.") }</p>
                        }) }
                    }
                    if let Some(shown) = &overlay.plugins {
                        <fieldset class="field overlay-plugins" aria-label="Plugins it shows">
                            { for pages.iter().map(|(id, name, _)| {
                                let on = shown.contains(id);
                                let switched_off = !panel.model.config.as_ref().is_some_and(|config| config.plugin(id).enabled);
                                let check_id = format!("{base}-plugin-{id}");
                                let onchange = {
                                    let (panel, id) = (panel.clone(), id.clone());
                                    Callback::from(move |event: Event| {
                                        let on = util::checked_of(&event);
                                        let id = id.clone();
                                        edit_overlay(&panel, index, None, move |overlay| {
                                            let plugins = overlay.plugins.get_or_insert_with(Vec::new);
                                            plugins.retain(|plugin| *plugin != id);
                                            if on {
                                                plugins.push(id.clone());
                                            }
                                        });
                                    })
                                };
                                html! {
                                    <div class="check-field">
                                        <input type="checkbox" id={check_id.clone()} checked={on} {onchange} />
                                        <label for={check_id}>
                                            { name.clone() }
                                            if switched_off { <span class="muted">{ " (switched off)" }</span> }
                                        </label>
                                    </div>
                                }
                            }) }
                        </fieldset>
                    }
                    <div class="button-row">
                        <button type="button" class="button ghost small" id={format!("{base}-remove")}
                                title="A Browser Source with its link then shows nothing; Ctrl+Z brings it back"
                                onclick={remove}>{ icon("trash") }{ "Remove" }</button>
                    </div>
                </div>
            }
        </div>
    }
}
