//! Plugins (a tab, the Effects page's until 2026-10-12): every plugin
//! with its switch, folded to its title until it's clicked (or linked to,
//! or needs a look), the hosts its server code wants to reach (to allow;
//! once allowed, to take back, for plugins someone installed), its own
//! settings (a row each, the labels in a column of their own), and
//! removing or forgetting it; installing from a zip,
//! and looking for new ones in the plugins folder. The Sounds and Speak
//! boxes also have the app's own sound and speech settings.

use protocol::config::{AppConfig, PluginConfig, Preset};
use protocol::overrides::Unit;
use protocol::panel::{PLUGIN_INSTALL_PATH, PanelRequest};
use protocol::plugin::{
    Choice, MediaKind, NodeType, PluginInfo, PluginMessage, PluginNote, PreviewRun, SPEAK_PLUGIN, Setting,
    SettingKind, ShellMessage, StatusLevel, When, copy_name, file_settings, files_path, is_set,
    look_name_problem, options_list, page_path, picked_look, server_hosts_allowed, shared_settings,
};
use protocol::sound::SOUND_PLUGIN;
use serde_json::{Map, Value};
use wasm_bindgen::JsCast;
use yew::prelude::*;

use super::activity::use_confirm;
use super::icon;
use super::position::PositionPick;
use super::sounds::{PicturePick, PictureUpload, SoundPick, SoundUpload, chosen_file, preview_files, upload};
use crate::editor::fields::{NumberField, PickField};
use crate::model::{FileDraft, Model};
use crate::{Panel, util};

/// Changes one plugin's entry in the config; an entry back at the
/// defaults is dropped (presets back at the one it starts with too).
fn edit_plugin(panel: &Panel, id: &str, key: Option<String>, change: impl Fn(&mut PluginConfig) + 'static) {
    let id = id.to_string();
    panel.edit(key.as_deref(), move |config: &mut AppConfig| {
        let entry = config.plugins.entry(id.clone()).or_default();
        change(entry);
        if entry.presets == [Preset::first()] {
            entry.presets.clear();
        }
        if *entry == PluginConfig::default() {
            config.plugins.remove(&id);
        }
    });
}

/// Whether a plugin that's on waits for something to be allowed (not a
/// built-in's): the hosts its server code connects to, where its page
/// loads from, what it hears or its moderating.
fn waits_for_you(plugin: &PluginInfo, settings: &PluginConfig) -> bool {
    let manifest = &plugin.manifest;
    let all = |wanted: &[String], allowed: &[String]| wanted.iter().all(|each| allowed.contains(each));
    settings.enabled
        && ((plugin.server && !server_hosts_allowed(plugin, &settings.hosts))
            || (!plugin.bundled
                && (!all(&manifest.page_hosts, &settings.page_hosts)
                    || !all(&manifest.hears, &settings.hears)
                    || (manifest.moderates && !settings.moderates))))
}

/// How many plugins need a look (the header's Plugins dot): their server
/// code stopped or says there's a problem, they didn't load, or they wait
/// for something to be allowed.
pub fn problems(model: &Model) -> usize {
    let Some(config) = &model.config else { return 0 };
    let plugins = model.plugins.plugins();
    let troubled = plugins
        .iter()
        .filter(|plugin| {
            plugin.stopped.is_some()
                || plugin.status.as_ref().is_some_and(|status| status.level == StatusLevel::Problem)
                || waits_for_you(plugin, &config.plugin(&plugin.manifest.id))
        })
        .count();
    troubled + model.plugin_failures.len()
}

/// The settings being edited: the plugin's, or one of its presets'.
fn own_settings_mut<'a>(entry: &'a mut PluginConfig, preset: Option<&str>) -> &'a mut Map<String, Value> {
    let Some(preset) = preset else { return &mut entry.settings };
    let presets = entry.presets_mut();
    let index = presets.iter().position(|each| each.id == preset).unwrap_or(0);
    &mut presets[index].settings
}

/// An id for a new preset named `name`: its letters and digits, unique.
fn new_preset_id(name: &str, taken: &[Preset]) -> String {
    let base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = if base.is_empty() { "preset".to_string() } else { base };
    let free = |id: &str| !taken.iter().any(|preset| preset.id == id);
    if free(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|id| free(id)).expect("some number is free")
}

#[derive(Properties, PartialEq)]
pub struct PluginsViewProps {
    /// The plugin whose box to scroll to (`#/plugins/<id>`).
    #[prop_or_default]
    pub focus: Option<AttrValue>,
}

#[function_component]
pub fn PluginsView(props: &PluginsViewProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    {
        // For pickers and previews: the folders' files.
        let panel = panel.clone();
        use_effect_with((), move |()| {
            panel.send(PanelRequest::Pictures);
            panel.send(PanelRequest::Sounds);
        });
    }
    {
        // Again once the plugins and the config are in, if the page opened
        // before they were (the boxes need both).
        let count = panel.model.plugins.plugins().len();
        let config = panel.model.config.is_some();
        use_effect_with((props.focus.clone(), count, config), |(focus, ..)| {
            let scroll = {
                let focus = focus.clone();
                move || {
                    let document = gloo::utils::document();
                    let Some(target) =
                        focus.as_ref().and_then(|id| document.get_element_by_id(&format!("plugin-{id}")))
                    else {
                        return;
                    };
                    // Just below the header, which stays at the top.
                    let header = document
                        .query_selector(".dock-header")
                        .ok()
                        .flatten()
                        .map_or(0.0, |header| header.get_bounding_client_rect().height());
                    let top = target.get_bounding_client_rect().top() - header - 8.0;
                    gloo::utils::window().scroll_by_with_x_and_y(0.0, top);
                }
            };
            scroll();
            // Again once the boxes above have settled (previews size
            // themselves, pictures' lists arrive), unless the page was
            // scrolled since: someone who has moved on isn't taken back
            // (nor is a click aimed at where something was).
            let scroll_y = || gloo::utils::window().scroll_y().unwrap_or_default();
            let placed = scroll_y();
            let again = gloo::timers::callback::Timeout::new(400, move || {
                if (scroll_y() - placed).abs() < 1.0 {
                    scroll();
                }
            });
            move || drop(again)
        });
    }
    let Some(config) = &panel.model.config else { return html! {} };
    // What the last install said: the plugin's id, or why not.
    let installed = use_state(|| None::<Result<String, String>>);
    let install = {
        let (panel, installed) = (panel.clone(), installed.clone());
        Callback::from(move |file: web_sys::File| {
            let (panel, installed) = (panel.clone(), installed.clone());
            yew::platform::spawn_local(async move {
                let result = upload(PLUGIN_INSTALL_PATH, &file).await;
                if result.is_ok() {
                    panel.send(PanelRequest::Pictures);
                }
                installed.set(Some(result));
            });
        })
    };
    let on_install = {
        let install = install.clone();
        Callback::from(move |event: Event| {
            if let Some(file) = chosen_file(&event) {
                install.emit(file);
            }
        })
    };
    // A zip dropped anywhere on the page installs it; the page is outlined
    // while files are dragged over it.
    let dragging = use_state(|| false);
    let carries_files = |event: &DragEvent| {
        event.data_transfer().is_some_and(|data| data.types().includes(&"Files".into(), 0))
    };
    let on_drag_over = {
        let dragging = dragging.clone();
        Callback::from(move |event: DragEvent| {
            if carries_files(&event) {
                // Without this, the browser opens the file in place of the panel.
                event.prevent_default();
                dragging.set(true);
            }
        })
    };
    let on_drag_leave = {
        let dragging = dragging.clone();
        Callback::from(move |event: DragEvent| {
            // Leaving the page, not just moving from one of its children to another.
            let into = event.related_target().and_then(|target| target.dyn_into::<web_sys::Node>().ok());
            let page = event.current_target().and_then(|target| target.dyn_into::<web_sys::Node>().ok());
            if !page.is_some_and(|page| page.contains(into.as_ref())) {
                dragging.set(false);
            }
        })
    };
    let on_drop = {
        let (dragging, installed) = (dragging.clone(), installed.clone());
        Callback::from(move |event: DragEvent| {
            if !carries_files(&event) {
                return;
            }
            event.prevent_default();
            dragging.set(false);
            let Some(file) =
                event.data_transfer().and_then(|data| data.files()).and_then(|files| files.get(0))
            else {
                return;
            };
            if file.name().to_lowercase().ends_with(".zip") {
                install.emit(file);
            } else {
                installed.set(Some(Err(format!("{} isn't a .zip file", file.name()))));
            }
        })
    };
    let on_reload = {
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::ReloadPlugins);
        })
    };
    let plugins = panel.model.plugins.plugins().to_vec();
    let installed_name = |id: &str| {
        plugins
            .iter()
            .find(|plugin| plugin.manifest.id == id)
            .map_or(id.to_string(), |plugin| format!("{} {}", plugin.manifest.name, plugin.manifest.version))
    };

    // The plugins' boxes and the text styles' (which looks like one), by
    // name.
    let mut boxes: Vec<(String, Html)> = plugins
        .iter()
        .map(|plugin| {
            let settings = config.plugin(&plugin.manifest.id);
            let key = plugin.manifest.id.clone();
            let focused = props.focus.as_deref() == Some(plugin.manifest.id.as_str());
            let html = html! { <PluginBox {key} plugin={plugin.clone()} {settings} {focused} /> };
            (plugin.manifest.name.to_lowercase(), html)
        })
        .collect();
    boxes.push(("text styles".into(), html! { <super::text_styles::TextStyles key="text-styles" /> }));
    boxes.sort_by(|(a, _), (b, _)| a.cmp(b));

    html! {
        <section class={classes!("page", dragging.then_some("dropping"))} id="plugins-page"
                 ondragover={on_drag_over} ondragleave={on_drag_leave} ondrop={on_drop}>
            if !panel.model.plugin_failures.is_empty() {
                <section class="panel-box" id="plugins-failures">
                    <h2>{ "Not loaded" }</h2>
                    { for panel.model.plugin_failures.iter().map(|failure| html! {
                        <>
                            <div class="status-row"><span><span class="dot warn"></span>{ failure.folder.clone() }</span></div>
                            { for failure.problems.iter().map(|problem| html! { <div class="muted small">{ problem.clone() }</div> }) }
                        </>
                    }) }
                </section>
            }

            { for boxes.into_iter().map(|(_, html)| html) }

            <section class="panel-box">
                <h2>{ "Add a plugin" }</h2>
                <div class="button-row">
                    <label class="button small" title="A plugin as a .zip file">
                        { "Install from a zip" }
                        <input id="plugins-install" type="file" class="visually-hidden" accept=".zip" onchange={on_install} />
                    </label>
                    <button type="button" class="button ghost small" id="plugins-reload" onclick={on_reload}>{ "Look for new plugins" }</button>
                </div>
                <p class="muted small">{ "Or drop a zip on this page, or copy a plugin's folder into the plugins folder, then look for new plugins. Only install plugins you trust: their pages and code run on your stream." }</p>
                { match &*installed {
                    Some(Ok(id)) => html! { <p class="small" role="status" id="plugins-installed">{ format!("Installed {}.", installed_name(id)) }</p> },
                    Some(Err(error)) => html! { <div class="notice warn" role="alert">{ icon("warn") }<div>{ format!("Not installed: {error}") }</div></div> },
                    None => html! {},
                } }
                if let Some(message) = &panel.model.plugin_problem {
                    <div class="notice warn" role="alert">{ icon("warn") }<div>{ message.clone() }</div></div>
                }
            </section>
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct PluginBoxProps {
    plugin: PluginInfo,
    settings: PluginConfig,
    /// Linked to (`#/plugins/<id>`): it opens.
    focused: bool,
}

#[function_component]
fn PluginBox(props: &PluginBoxProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let confirm = use_confirm();
    let plugin = &props.plugin;
    let manifest = &plugin.manifest;
    let id = manifest.id.clone();
    let base = format!("plugin-{id}");
    // Folded to its head, unless linked to or it needs a look.
    let open =
        use_state(|| props.focused || plugin.stopped.is_some() || waits_for_you(plugin, &props.settings));
    // Unfolds when linked to, or when its server code stops while shown.
    {
        let open = open.clone();
        use_effect_with((props.focused, plugin.stopped.is_some()), move |(focused, stopped)| {
            if *focused || *stopped {
                open.set(true);
            }
        });
    }
    let flip = {
        let open = open.clone();
        Callback::from(move |_: MouseEvent| open.set(!*open))
    };

    let on_toggle = {
        let (panel, id) = (panel.clone(), id.clone());
        Callback::from(move |event: Event| {
            let on = util::checked_of(&event);
            edit_plugin(&panel, &id, None, move |entry| entry.enabled = on);
        })
    };
    let set_hosts = |hosts: Vec<String>| {
        let (panel, id) = (panel.clone(), id.clone());
        Callback::from(move |_: MouseEvent| {
            let hosts = hosts.clone();
            edit_plugin(&panel, &id, None, move |entry| entry.hosts = hosts.clone());
        })
    };
    let remove = {
        let (panel, id, confirm) = (panel.clone(), id.clone(), confirm.clone());
        Callback::from(move |_: MouseEvent| {
            if *confirm {
                panel.send(PanelRequest::RemovePlugin { id: id.clone() });
            }
            confirm.set(!*confirm);
        })
    };
    // With presets: the one being edited (the first if it's gone), whether
    // its name is being typed, and whether its ⋯ menu is open.
    let editing = use_state(String::new);
    let renaming = use_state(|| false);
    let menu = use_state(|| false);
    let name_field = use_node_ref();
    {
        // Into the name, all of it picked to type over, once renaming starts.
        let name_field = name_field.clone();
        use_effect_with(*renaming, move |renaming| {
            if *renaming && let Some(input) = name_field.cast::<web_sys::HtmlInputElement>() {
                let _ = input.focus();
                input.select();
            }
        });
    }
    let presets = manifest.presets.as_ref().map(|spec| (spec.label.clone(), props.settings.presets()));
    let preset = presets.as_ref().map(|_| props.settings.preset(&editing));
    let stored = preset.as_ref().map_or(&props.settings.settings, |preset| &preset.settings);
    let reset = {
        let (panel, id, menu) = (panel.clone(), id.clone(), menu.clone());
        let preset = preset.as_ref().map(|preset| preset.id.clone());
        Callback::from(move |_: MouseEvent| {
            menu.set(false);
            let preset = preset.clone();
            edit_plugin(&panel, &id, None, move |entry| own_settings_mut(entry, preset.as_deref()).clear());
        })
    };
    let preset_bar = match (&presets, &preset) {
        (Some((label, all)), Some(preset)) => {
            let pick = {
                let editing = editing.clone();
                Callback::from(move |event: Event| editing.set(util::value_of(&event)))
            };
            let duplicate = {
                let (panel, id, editing, renaming, preset, all) = (
                    panel.clone(),
                    id.clone(),
                    editing.clone(),
                    renaming.clone(),
                    preset.clone(),
                    all.clone(),
                );
                Callback::from(move |_: MouseEvent| {
                    let name = format!("{} copy", preset.name);
                    let copy =
                        Preset { id: new_preset_id(&name, &all), name, settings: preset.settings.clone() };
                    editing.set(copy.id.clone());
                    // Named next, most likely: its name opens to type over.
                    renaming.set(true);
                    let after = preset.id.clone();
                    edit_plugin(&panel, &id, None, move |entry| {
                        let presets = entry.presets_mut();
                        let at = presets
                            .iter()
                            .position(|each| each.id == after)
                            .map_or(presets.len(), |at| at + 1);
                        presets.insert(at, copy.clone());
                    });
                })
            };
            let delete = {
                let (panel, id, editing, menu, gone) =
                    (panel.clone(), id.clone(), editing.clone(), menu.clone(), preset.id.clone());
                Callback::from(move |_: MouseEvent| {
                    menu.set(false);
                    editing.set(String::new());
                    let gone = gone.clone();
                    edit_plugin(&panel, &id, None, move |entry| {
                        let presets = entry.presets_mut();
                        if presets.len() > 1 && presets[0].id != gone {
                            presets.retain(|each| each.id != gone);
                        }
                    });
                })
            };
            let rename = {
                let (panel, id, which) = (panel.clone(), id.clone(), preset.id.clone());
                let key = format!("{base}-preset-name");
                Callback::from(move |event: InputEvent| {
                    let (name, which) = (util::value_of(&event), which.clone());
                    edit_plugin(&panel, &id, Some(key.clone()), move |entry| {
                        if let Some(each) = entry.presets_mut().iter_mut().find(|each| each.id == which) {
                            each.name = name.clone();
                        }
                    });
                })
            };
            let start_renaming = {
                let (renaming, menu) = (renaming.clone(), menu.clone());
                Callback::from(move |_: MouseEvent| {
                    menu.set(false);
                    renaming.set(true);
                })
            };
            let done = {
                let renaming = renaming.clone();
                Callback::from(move |_: MouseEvent| renaming.set(false))
            };
            // Enter or Escape (the name is kept as typed), or a click
            // elsewhere, finishes too.
            let name_keys = {
                let renaming = renaming.clone();
                Callback::from(move |event: KeyboardEvent| {
                    if matches!(event.key().as_str(), "Enter" | "Escape") {
                        event.prevent_default();
                        renaming.set(false);
                    }
                })
            };
            let name_left = {
                let renaming = renaming.clone();
                Callback::from(move |_: FocusEvent| renaming.set(false))
            };
            let toggle_menu = {
                let menu = menu.clone();
                Callback::from(move |_: MouseEvent| menu.set(!*menu))
            };
            let close_menu = {
                let menu = menu.clone();
                Callback::from(move |_: MouseEvent| menu.set(false))
            };
            let menu_keys = {
                let menu = menu.clone();
                Callback::from(move |event: KeyboardEvent| {
                    if event.key() == "Escape" {
                        menu.set(false);
                    }
                })
            };
            let first = all.first();
            let is_first = first.is_some_and(|first| first.id == preset.id);
            let first_name = first.map(|first| first.name.clone()).unwrap_or_default();
            let thing = label.to_lowercase();
            let (name_id, pick_id) = (format!("{base}-preset-name"), format!("{base}-preset"));
            let control = if *renaming {
                html! {
                    <div class="preset-control">
                        <input type="text" id={name_id.clone()} ref={name_field.clone()} value={preset.name.clone()}
                               spellcheck="false" oninput={rename} onkeydown={name_keys} onblur={name_left} />
                        <button type="button" class="button ghost small" id={format!("{base}-preset-done")}
                                onclick={done}>{ "Done" }</button>
                    </div>
                }
            } else {
                html! {
                    <div class="preset-control">
                        <select id={pick_id.clone()} onchange={pick}>
                            { for all.iter().map(|each| html! {
                                <option value={each.id.clone()} selected={each.id == preset.id}>{ each.name.clone() }</option>
                            }) }
                        </select>
                        <button type="button" class="icon-button flat" id={format!("{base}-preset-rename")}
                                aria-label="Rename" title="Rename" onclick={start_renaming}>{ icon("pencil") }</button>
                        <button type="button" class="icon-button flat" id={format!("{base}-preset-duplicate")}
                                aria-label="Duplicate" title="Duplicate" onclick={duplicate}>{ icon("duplicate") }</button>
                        <div class="bar-menu" onkeydown={menu_keys}>
                            <button type="button" class="icon-button flat" id={format!("{base}-preset-more")}
                                    aria-label="More" title="More" aria-haspopup="menu" aria-expanded={menu.to_string()}
                                    onclick={toggle_menu}>{ icon("more") }</button>
                            if *menu {
                                <div class="bar-menu-backdrop" onclick={close_menu}></div>
                                <div class="bar-menu-list preset-menu" role="menu">
                                    <button type="button" class="menu-item two-line" role="menuitem" id={format!("{base}-reset")}
                                            disabled={stored.is_empty()} onclick={reset.clone()}>
                                        <span>{ "Reset to defaults" }</span>
                                        <span class="menu-note">{ format!("Every setting of “{}”", preset.name) }</span>
                                    </button>
                                    <button type="button" class="menu-item two-line danger" role="menuitem"
                                            id={format!("{base}-preset-delete")} disabled={is_first} onclick={delete}>
                                        <span>{ format!("Delete this {thing}") }</span>
                                        <span class="menu-note">
                                            { if is_first {
                                                format!("The first {thing} can't be deleted")
                                            } else {
                                                format!("Nodes using it get “{first_name}”")
                                            } }
                                        </span>
                                    </button>
                                </div>
                            }
                        </div>
                    </div>
                }
            };
            html! {
                <div class="setting-row preset-row">
                    <label class="setting-label" for={if *renaming { name_id } else { pick_id }}>{ label.clone() }</label>
                    { control }
                </div>
            }
        }
        _ => html! {},
    };
    let forget = {
        let (panel, id) = (panel.clone(), id.clone());
        Callback::from(move |_: MouseEvent| {
            panel.send(PanelRequest::ForgetProgram { id: id.clone() });
        })
    };

    // The plugin's help page: beside its (first) file setting's picker,
    // where it's about that file (the markup a stylesheet styles); else a
    // fold of its own.
    let help = (!manifest.help.is_empty() && plugin.overlay)
        .then(|| format!("{}{}", files_path(&id), manifest.help));
    let help_beside = file_settings(manifest).next().is_some();

    // The preview: its page with these settings (the picked preset's).
    let preview = manifest.preview.as_ref().filter(|_| plugin.overlay).and_then(|spec| {
        let ty = panel.model.plugins.node_type(&id, &spec.node);
        if ty.is_none() && !spec.samples {
            return None;
        }
        // A folder: its first file, so the preview holds still.
        let mut pick = |kind, folder: &str, every| {
            let folders = match kind {
                MediaKind::Sound => &panel.model.sound_folders,
                MediaKind::Picture | MediaKind::Media => &panel.model.picture_folders,
            };
            preview_files(folder, folders, kind, every)
        };
        let mut shared = shared_settings(&manifest.settings, stored, &mut pick);
        // A file setting's look as edited here, so the preview follows
        // each key.
        for (key, _, _) in file_settings(manifest) {
            let picked = shared.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
            shared.insert(key.to_string(), Value::String(panel.look_text(&id, key, &picked)));
        }
        let settings = if presets.is_some() { shared.clone() } else { Map::new() };
        let values = ty.as_ref().map(|ty| ty.preview_values(&spec.values)).unwrap_or_default();
        let size = (f64::from(spec.width.max(1)), f64::from(spec.height.max(1)));
        let shown = spec.shown_height.map_or(size.1, |shown| f64::from(shown.max(1)).min(size.1));
        Some(html! {
            <PluginPreview plugin={id.clone()} {ty} {values} {shared} {settings} {size} {shown}
                           samples={spec.samples} />
        })
    });
    let kind = match (plugin.bundled, plugin.program, plugin.connected) {
        (true, _, _) => "built in".to_string(),
        (_, true, true) => format!("{}, a program, connected", manifest.version),
        (_, true, false) => format!("{}, a program, not connected", manifest.version),
        _ => manifest.version.clone(),
    };
    let wants_hosts = plugin.server && !manifest.hosts.is_empty();
    // Where its page loads from: a built-in's are allowed already.
    let page_hosts = manifest.page_hosts.clone();
    let pages_allowed =
        plugin.bundled || page_hosts.iter().all(|host| props.settings.page_hosts.contains(host));
    let set_page_hosts = |hosts: Vec<String>| {
        let (panel, id) = (panel.clone(), id.clone());
        Callback::from(move |_: MouseEvent| {
            let hosts = hosts.clone();
            edit_plugin(&panel, &id, None, move |entry| entry.page_hosts = hosts.clone());
        })
    };
    // A built-in's are allowed already.
    let allowed = server_hosts_allowed(plugin, &props.settings.hosts);
    // What it hears without a card (the chat, events): a built-in's is
    // allowed.
    let hears = manifest.hears.clone();
    let heard = hears
        .iter()
        .map(|what| match what.as_str() {
            protocol::plugin::HEARS_EVENTS => "every event (cheers, subs, raids…), with who set it off",
            _ => "the chat: every message, with who sent it",
        })
        .collect::<Vec<_>>()
        .join("; and ");
    let hears_short = if hears.len() > 1 {
        "the chat and events"
    } else if hears.iter().any(|what| what == protocol::plugin::HEARS_EVENTS) {
        "events"
    } else {
        "the chat"
    };
    let hears_allowed = plugin.bundled || hears.iter().all(|what| props.settings.hears.contains(what));
    let set_hears = |hears: Vec<String>| {
        let (panel, id) = (panel.clone(), id.clone());
        Callback::from(move |_: MouseEvent| {
            let hears = hears.clone();
            edit_plugin(&panel, &id, None, move |entry| entry.hears = hears.clone());
        })
    };
    // Its moderating: a built-in's is allowed.
    let moderates_allowed = plugin.bundled || props.settings.moderates;
    let set_moderates = |moderates: bool| {
        let (panel, id) = (panel.clone(), id.clone());
        Callback::from(move |_: MouseEvent| {
            edit_plugin(&panel, &id, None, move |entry| entry.moderates = moderates);
        })
    };

    html! {
        <section class="panel-box plugin-box" id={base.clone()}>
            <div class="box-head">
                <input class="switch" type="checkbox" id={format!("{base}-on")} checked={props.settings.enabled}
                       onchange={on_toggle} aria-label={format!("{} on", manifest.name)} />
                <button type="button" class={classes!("box-title", "box-fold", open.then_some("open"))}
                        id={format!("{base}-open")} aria-expanded={open.to_string()} aria-controls={format!("{base}-body")}
                        onclick={flip}>
                    { icon("chevron") }
                    { manifest.name.clone() }
                </button>
                if let Some(status) = &plugin.status {
                    <span class="plugin-status" id={format!("{base}-status")} title={status.text.clone()}>
                        <span class={classes!("dot", match status.level {
                            StatusLevel::Ok => Some("ok"),
                            StatusLevel::Waiting => None,
                            StatusLevel::Problem => Some("warn"),
                        })}></span>
                        { status.text.clone() }
                    </span>
                }
                <span class="muted small">{ kind }</span>
                // With presets, Reset is in the preset row's ⋯ menu.
                if *open && !manifest.settings.is_empty() && presets.is_none() {
                    <button type="button" class="button ghost small" id={format!("{base}-reset")}
                            title="Put its settings back to their defaults"
                            disabled={stored.is_empty()} onclick={reset}>{ "Reset" }</button>
                }
            </div>
            if *open {
            <div class="box-body" id={format!("{base}-body")}>
            if !manifest.description.is_empty() {
                <p class="muted small">{ manifest.description.clone() }</p>
            }
            if let Some(reason) = &plugin.stopped {
                <div class="notice warn" role="status">
                    { icon("warn") }
                    <div>{ format!("Its server code stopped: {reason}. Switch it off and on to try again.") }</div>
                </div>
            }
            // A note on the plugin itself (`set-note` with no setting).
            if let Some(note) = plugin.notes.get("") {
                { note_view(&format!("{base}-note"), note) }
            }
            if wants_hosts && !allowed {
                { allow_notice(
                    format!("{base}-hosts"),
                    format!("Its server code wants to connect to {}. It doesn't run until you allow it.", manifest.hosts.join(", ")),
                    format!("{base}-allow"),
                    set_hosts(manifest.hosts.clone()),
                ) }
            } else if wants_hosts && !plugin.bundled {
                // What was allowed, to take back (a built-in's come with the
                // app, so there's nothing to say).
                <div class="status-row small">
                    <span class="muted">{ format!("May connect to {}", manifest.hosts.join(", ")) }</span>
                    <button type="button" class="button ghost small" id={format!("{base}-disallow")}
                            onclick={set_hosts(Vec::new())}>{ "Take back" }</button>
                </div>
            }
            if !page_hosts.is_empty() && !pages_allowed {
                { allow_notice(
                    format!("{base}-page-hosts"),
                    format!("Its overlay page wants to load from {}. It doesn't until you allow it (then reload the overlay's Browser Source).", page_hosts.join(", ")),
                    format!("{base}-allow-page"),
                    set_page_hosts(page_hosts.clone()),
                ) }
            } else if !page_hosts.is_empty() && !plugin.bundled {
                <div class="status-row small">
                    <span class="muted">{ format!("Its page may load from {}", page_hosts.join(", ")) }</span>
                    <button type="button" class="button ghost small" id={format!("{base}-disallow-page")}
                            onclick={set_page_hosts(Vec::new())}>{ "Take back" }</button>
                </div>
            }
            if !hears.is_empty() && !hears_allowed {
                { allow_notice(
                    format!("{base}-hears"),
                    format!("It wants to hear {heard}. It doesn't until you allow it."),
                    format!("{base}-allow-hears"),
                    set_hears(hears.clone()),
                ) }
            } else if !hears.is_empty() && !plugin.bundled {
                <div class="status-row small">
                    <span class="muted">{ format!("It hears {hears_short}") }</span>
                    <button type="button" class="button ghost small" id={format!("{base}-disallow-hears")}
                            onclick={set_hears(Vec::new())}>{ "Take back" }</button>
                </div>
            }
            if manifest.moderates && !moderates_allowed {
                { allow_notice(
                    format!("{base}-moderates"),
                    "It wants to time out and ban viewers and delete messages, with your Twitch login. It doesn't until you allow it.".to_string(),
                    format!("{base}-allow-moderates"),
                    set_moderates(true),
                ) }
            } else if manifest.moderates && !plugin.bundled {
                <div class="status-row small">
                    <span class="muted">{ "It moderates the chat" }</span>
                    <button type="button" class="button ghost small" id={format!("{base}-disallow-moderates")}
                            onclick={set_moderates(false)}>{ "Take back" }</button>
                </div>
            }
            { preset_bar }
            if let (Some(help), false) = (&help, help_beside) {
                <details class="plugin-help" id={format!("{base}-help")}>
                    <summary>{ "Help" }</summary>
                    <iframe sandbox="" src={help.clone()} title="Help"></iframe>
                </details>
            }
            if let Some(preview) = preview {
                { preview }
            }
            if !manifest.settings.is_empty() {
                <div class="plugin-settings">
                    { settings_view(&panel, &id, &manifest.settings, stored,
                                    preset.as_ref().map(|preset| preset.id.as_str()),
                                    help.as_deref().filter(|_| help_beside)) }
                </div>
            }
            if plugin.bundled && id == SOUND_PLUGIN {
                <super::sounds::Sounds />
            }
            if plugin.bundled && id == SPEAK_PLUGIN {
                <super::speech::Speech />
            }
            if !plugin.bundled && !plugin.program {
                <div class="button-row">
                    <button type="button" class={classes!("button", "small", if *confirm { "danger" } else { "ghost" })}
                            id={format!("{base}-remove")} onclick={remove}>
                        { if *confirm { "Click again to delete its folder" } else { "Remove" } }
                    </button>
                </div>
            }
            if plugin.program && !plugin.connected {
                <div class="button-row">
                    <button type="button" class="button ghost small" id={format!("{base}-forget")} onclick={forget}>{ "Forget" }</button>
                </div>
            }
            </div>
            }
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct PluginPreviewProps {
    plugin: AttrValue,
    /// The node Play runs; none when the page draws samples.
    ty: Option<std::sync::Arc<NodeType>>,
    /// The page's size in OBS, shown scaled down.
    size: (f64, f64),
    /// How much of its height shows: the bottom of it.
    shown: f64,
    /// Play asks the page for its samples, as loading does.
    samples: bool,
    /// The run's values (the manifest's samples).
    values: Map<String, Value>,
    /// The plugin's settings as its page gets them.
    shared: Map<String, Value>,
    /// The run's `settings`: the same, for a plugin with presets.
    settings: Map<String, Value>,
}

/// A plugin's overlay page in a sandboxed iframe, as the overlay hosts it:
/// it gets its settings (again as they're edited) and the master volume,
/// and once it has loaded a quiet run of its preview node (`preview:
/// show`; or it draws its samples). In the strip under it, ▶ Play runs it
/// again (`play`; or draws them), Show on overlay on the overlays.
#[function_component]
fn PluginPreview(props: &PluginPreviewProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let frame = use_node_ref();
    let host = use_node_ref();
    let scale = use_state(|| 0.18_f64);
    let loaded = use_state(|| false);
    let (stage_width, stage_height) = props.size;
    {
        let (host, scale) = (host.clone(), scale.clone());
        use_effect_with(stage_width, move |stage_width| {
            let stage_width = *stage_width;
            let element = host.cast::<web_sys::HtmlElement>();
            let measure = {
                let element = element.clone();
                move || {
                    // The room it has: its own width (the box's, at most the
                    // page's).
                    let Some(element) = &element else { return };
                    let width = f64::from(element.client_width());
                    if width > 0.0 {
                        // Never bigger than the page itself.
                        scale.set((width / stage_width).min(1.0));
                    }
                }
            };
            measure();
            // Whenever its width changes: the dock resized, or a scrollbar
            // came or went (not every change is a window resize).
            let callback = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(measure);
            let observer =
                web_sys::ResizeObserver::new(wasm_bindgen::JsCast::unchecked_ref(callback.as_ref())).ok();
            if let (Some(observer), Some(element)) = (&observer, &element) {
                observer.observe(element);
            }
            move || {
                if let Some(observer) = observer {
                    observer.disconnect();
                }
                drop(callback);
            }
        });
    }
    let post = |frame: &NodeRef, message: String| {
        let window = frame.cast::<web_sys::HtmlIFrameElement>().and_then(|frame| frame.content_window());
        if let (Some(window), Ok(message)) = (window, js_sys::JSON::parse(&message)) {
            let _ = window.post_message(&message, "*");
        }
    };
    let sound = panel.model.config.as_ref().map(|config| config.effects.sound.clone()).unwrap_or_default();
    {
        let frame = frame.clone();
        let settings = serde_json::to_string(&PluginMessage::Settings { values: props.shared.clone() })
            .expect("plugin messages serialise");
        let volume =
            serde_json::to_string(&ShellMessage::Volume { volume: sound.volume, muted: sound.muted })
                .expect("shell messages serialise");
        use_effect_with((settings, volume, *loaded), move |(settings, volume, loaded)| {
            if *loaded {
                post(&frame, settings.clone());
                post(&frame, volume.clone());
            }
        });
    }
    let onload = {
        let loaded = loaded.clone();
        Callback::from(move |_: Event| loaded.set(true))
    };
    // A run of its preview node (`preview` says which kind), or for a page
    // that draws samples of its own, those.
    let run = |preview: PreviewRun| {
        let message = match &props.ty {
            Some(ty) if !props.samples => {
                let input = ty
                    .inputs
                    .iter()
                    .find(|input| input.ty == protocol::cards::SocketType::Event)
                    .map_or_else(|| "trigger".to_string(), |input| input.name.clone());
                PluginMessage::Run {
                    node: ty.id.clone(),
                    input,
                    values: props.values.clone(),
                    changes: Map::new(),
                    settings: props.settings.clone(),
                    queue_id: None,
                    run_id: None,
                    preview: Some(preview),
                    origin: protocol::activity::ItemOrigin::Test,
                }
            }
            _ => PluginMessage::Samples,
        };
        serde_json::to_string(&message).expect("plugin messages serialise")
    };
    {
        // Once it has loaded (after its settings, posted above): its
        // samples, or its run drawn quietly, so it never stands empty.
        let (frame, opening) = (frame.clone(), run(PreviewRun::Show));
        use_effect_with(*loaded, move |loaded| {
            if *loaded {
                post(&frame, opening);
            }
        });
    }
    let play = {
        let (frame, play) = (frame.clone(), run(PreviewRun::Play));
        Callback::from(move |_: MouseEvent| post(&frame, play.clone()))
    };
    let show = {
        let node = props.ty.as_ref().filter(|_| !props.samples).map(|ty| ty.id.clone()).unwrap_or_default();
        let request = PanelRequest::PreviewPlugin {
            plugin: props.plugin.to_string(),
            node,
            values: props.values.clone(),
            settings: props.settings.clone(),
        };
        let panel = panel.clone();
        Callback::from(move |_: MouseEvent| {
            panel.send(request.clone());
        })
    };
    let base = format!("plugin-{}", props.plugin);
    // The page's bottom `shown` pixels: the rest is above the box.
    let above = (stage_height - props.shown) * *scale;
    let stage = format!(
        "width: {stage_width}px; height: {stage_height}px; top: -{above}px; transform: scale({})",
        *scale
    );
    let box_height = format!("height: {}px", (props.shown * *scale).round());
    // Never wider than the page itself (with its frame's border).
    let frame_width = format!("max-width: {}px", stage_width + 2.0);
    html! {
        <div class="preview-frame" style={frame_width}>
            <div class="plugin-preview" ref={host} style={box_height} aria-label="Preview">
                <iframe ref={frame} id={format!("{base}-preview")} class="plugin-stage" style={stage}
                        sandbox="allow-scripts" src={page_path(&props.plugin)} title="Preview" {onload}></iframe>
            </div>
            <div class="preview-bar">
                <button type="button" class="button flat small" id={format!("{base}-play")} onclick={play}>{ icon("play") }{ "Play" }</button>
                <button type="button" class="button flat small" id={format!("{base}-show")} onclick={show}
                        title="Shows it on the overlays too (in its turn in the queue, if it waits there)">
                    { icon("screen") }{ "Show on overlay" }
                </button>
            </div>
        </div>
    }
}

/// A plugin's own settings (or one of its presets', by id): a row each,
/// its label in a column of their own and its field beside it; the ones
/// without a group first, then each group (in the manifest's order) under
/// its heading.
pub(crate) fn settings_view(
    panel: &Panel,
    id: &str,
    settings: &[Setting],
    stored: &Map<String, Value>,
    preset: Option<&str>,
    help: Option<&str>,
) -> Html {
    let first_file = settings
        .iter()
        .find(|setting| matches!(setting.kind, SettingKind::File { .. }))
        .map(|setting| &setting.key);
    let mut groups: Vec<&str> = Vec::new();
    for group in settings.iter().filter_map(|setting| setting.group.as_deref()) {
        if !groups.contains(&group) {
            groups.push(group);
        }
    }
    let notes = panel
        .model
        .plugins
        .plugins()
        .iter()
        .find(|plugin| plugin.manifest.id == id)
        .map(|plugin| &plugin.notes);
    let rows = |group: Option<&str>| -> Html {
        settings
            .iter()
            // Only nodes have emotes and presets.
            .filter(|setting| !matches!(setting.kind, SettingKind::Emotes { .. } | SettingKind::Preset {}))
            .filter(|setting| setting.group.as_deref() == group)
            // A tick box beside another's field is drawn there.
            .filter(|setting| setting.beside.is_empty())
            .filter(|setting| shown_on_box(setting, settings, stored))
            .map(|setting| {
                let note = notes
                    .and_then(|notes| notes.get(&setting.key))
                    .map(|note| note_view(&format!("plugin-{id}-{}-note", setting.key), note));
                let help = help.filter(|_| first_file == Some(&setting.key));
                setting_row(panel, id, setting, settings, stored, preset, note, help)
            })
            .collect()
    };
    let loose = settings.iter().any(|setting| setting.group.is_none());
    html! {
        <>
            if loose {
                <div class="setting-rows">{ rows(None) }</div>
            }
            { for groups.iter().map(|group| html! {
                <section class="setting-group">
                    <h3>{ group.to_string() }</h3>
                    <div class="setting-rows">{ rows(Some(group)) }</div>
                </section>
            }) }
        </>
    }
}

/// Whether a plugin's own setting has a row: one shown only while another
/// is set (`"when": { "set": … }`) only then.
fn shown_on_box(setting: &Setting, settings: &[Setting], stored: &Map<String, Value>) -> bool {
    let Some(When::Set(other)) = &setting.when else { return true };
    settings
        .iter()
        .find(|each| each.key == *other)
        .is_none_or(|other| is_set(&other.read(stored.get(&other.key))))
}

/// Stores a plugin's own setting (in one of its presets, if given); one
/// undo step per field (`field`) while it's being changed.
fn setter(panel: &Panel, id: &str, key: &str, preset: Option<&str>, field: &str) -> Callback<Value> {
    let (panel, id, key) = (panel.clone(), id.to_string(), key.to_string());
    let preset = preset.map(str::to_string);
    let undo_key = format!("{field}-{}", preset.as_deref().unwrap_or_default());
    Callback::from(move |value: Value| {
        let (key, value, preset) = (key.clone(), value.clone(), preset.clone());
        edit_plugin(&panel, &id, Some(undo_key.clone()), move |entry| {
            own_settings_mut(entry, preset.as_deref()).insert(key.clone(), value.clone());
        });
    })
}

/// A tick box beside another setting's field (`beside`): a small button,
/// pressed while it's on, its face its `icon` (**B**, *I*) or its label.
fn toggle_button(
    panel: &Panel,
    id: &str,
    setting: &Setting,
    stored: &Map<String, Value>,
    preset: Option<&str>,
) -> Html {
    let field = format!("plugin-{id}-setting-{}", setting.key);
    let on = setting.read(stored.get(&setting.key)).as_bool().unwrap_or_default();
    let onclick =
        setter(panel, id, &setting.key, preset, &field).reform(move |_: MouseEvent| Value::Bool(!on));
    let label = if setting.short.is_empty() { setting.label.clone() } else { setting.short.clone() };
    let face = match setting.icon.as_str() {
        "bold" => html! { <b>{ "B" }</b> },
        "italic" => html! { <i>{ "I" }</i> },
        _ => html! { label },
    };
    html! {
        <button type="button" id={field} class={classes!("toggle", on.then_some("on"), setting.icon.is_empty().then_some("text"))}
                aria-pressed={on.to_string()} aria-label={setting.label.clone()} title={setting.label.clone()} {onclick}>
            { face }
        </button>
    }
}

/// A note the plugin's server code put under a setting (`host.set-note`):
/// a line each, orange for a problem.
fn note_view(id: &str, note: &PluginNote) -> Html {
    let warn = note.level == StatusLevel::Problem;
    html! {
        <div class={classes!("notice", "plugin-note", warn.then_some("warn"))} role="status" id={id.to_string()}>
            if warn {
                { icon("warn") }
            }
            <div class="note-lines">
                { for note.lines.iter().map(|line| html! { <p>{ line.clone() }</p> }) }
            </div>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct LookFieldProps {
    plugin: String,
    key_name: String,
    label: String,
    /// The extension the user's copies get (`.css`).
    extension: String,
    /// The plugin's help page, opened with ? beside the picker.
    #[prop_or_default]
    help: Option<String>,
    /// The setting's value: the picked look's name.
    value: String,
    set: Callback<Value>,
}

/// A file setting (a stylesheet): a pick of its looks, the plugin's own
/// (which never change) and the user's copies, with ✎ to show its text
/// and ? for the plugin's help. Typing in one of the plugin's own makes a
/// copy ("Classic copy"), picked once it's saved; a copy has its name and
/// a bin. What's typed reaches the preview at once and is saved with Save.
#[function_component]
fn LookField(props: &LookFieldProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let confirm = use_confirm();
    let editing = use_state(|| false);
    let helping = use_state(|| false);
    // A name typed for one of the user's looks, until it's renamed.
    let typed_name = use_state(|| None::<String>);
    let (plugin, key) = (props.plugin.clone(), props.key_name.clone());
    let base = format!("plugin-{plugin}-setting-{key}");
    let looks = panel.looks(&plugin, &key);
    let draft = panel.model.file_drafts.get(&(plugin.clone(), key.clone())).cloned();
    let picked = picked_look(&looks, &props.value).cloned();
    let picked_name = picked.as_ref().map(|look| look.name.clone()).unwrap_or_default();
    {
        // Once what was typed is what's saved, it's no longer a draft.
        let saved = draft.as_ref().is_some_and(|draft| {
            looks.iter().any(|look| look.own && look.name == draft.name && look.text == draft.text)
        });
        let (panel, plugin, key) = (panel.clone(), plugin.clone(), key.clone());
        use_effect_with(saved, move |saved| {
            if *saved {
                panel.file_draft(&plugin, &key, None);
            }
        });
    }
    {
        let typed_name = typed_name.clone();
        use_effect_with(picked_name.clone(), move |_| typed_name.set(None));
    }
    let set_draft = {
        let (panel, plugin, key) = (panel.clone(), plugin.clone(), key.clone());
        move |draft: Option<FileDraft>| panel.file_draft(&plugin, &key, draft)
    };
    let flip = |state: &UseStateHandle<bool>| {
        let state = state.clone();
        Callback::from(move |_: MouseEvent| state.set(!*state))
    };
    let pick = props.set.reform(|event: Event| Value::String(util::value_of(&event)));
    // Typing: into the draft; in one of the plugin's own, into a new copy.
    let oninput = {
        let (set_draft, draft, picked, looks) =
            (set_draft.clone(), draft.clone(), picked.clone(), looks.clone());
        Callback::from(move |event: InputEvent| {
            let text = util::value_of(&event);
            let draft = match (&draft, &picked) {
                (Some(draft), _) => FileDraft { text, ..draft.clone() },
                (None, Some(look)) if look.own => FileDraft { name: look.name.clone(), text, copy_of: None },
                (None, look) => {
                    let of = look.as_ref().map(|look| look.name.clone()).unwrap_or_default();
                    FileDraft { name: copy_name(&of, &looks), text, copy_of: Some(of) }
                }
            };
            set_draft(Some(draft));
        })
    };
    let copy = draft.as_ref().is_some_and(|draft| draft.copy_of.is_some());
    let own = copy || picked.as_ref().is_some_and(|look| look.own);
    let current = draft.as_ref().map(|draft| draft.name.clone()).unwrap_or_else(|| picked_name.clone());
    let shown_name = (*typed_name).clone().unwrap_or_else(|| current.clone());
    let name_problem = if copy {
        look_name_problem(&current, &looks, None)
    } else {
        look_name_problem(&shown_name, &looks, Some(&current))
    };
    // A new copy's name is as typed; one of the user's is renamed once it's
    // entered (Enter, or leaving the field).
    let on_name = {
        let (set_draft, draft, typed_name) = (set_draft.clone(), draft.clone(), typed_name.clone());
        Callback::from(move |event: InputEvent| {
            let name = util::value_of(&event);
            match &draft {
                Some(draft) if draft.copy_of.is_some() => {
                    set_draft(Some(FileDraft { name, ..draft.clone() }))
                }
                _ => typed_name.set(Some(name)),
            }
        })
    };
    let rename = {
        let (panel, plugin, key, set) = (panel.clone(), plugin.clone(), key.clone(), props.set.clone());
        let (looks, typed_name, draft, set_draft) =
            (looks.clone(), typed_name.clone(), draft.clone(), set_draft.clone());
        let current = current.clone();
        Callback::from(move |_: Event| {
            let Some(to) = (*typed_name).clone().filter(|to| *to != current) else { return };
            if copy || look_name_problem(&to, &looks, Some(&current)).is_some() {
                return;
            }
            panel.send(PanelRequest::RenamePluginLook {
                plugin: plugin.clone(),
                key: key.clone(),
                name: current.clone(),
                to: to.clone(),
            });
            if let Some(draft) = &draft {
                set_draft(Some(FileDraft { name: to.clone(), ..draft.clone() }));
            }
            set.emit(Value::String(to));
            typed_name.set(None);
        })
    };
    let name_keys = {
        let typed_name = typed_name.clone();
        Callback::from(move |event: KeyboardEvent| {
            if event.key() == "Escape" {
                typed_name.set(None);
            }
        })
    };
    let save = {
        let (panel, plugin, key, set) = (panel.clone(), plugin.clone(), key.clone(), props.set.clone());
        let (draft, value) = (draft.clone(), props.value.clone());
        Callback::from(move |_: MouseEvent| {
            let Some(draft) = &draft else { return };
            panel.send(PanelRequest::SavePluginLook {
                plugin: plugin.clone(),
                key: key.clone(),
                name: draft.name.clone(),
                text: Some(draft.text.clone()),
            });
            if draft.name != value {
                set.emit(Value::String(draft.name.clone()));
            }
        })
    };
    let discard = {
        let set_draft = set_draft.clone();
        Callback::from(move |_: MouseEvent| set_draft(None))
    };
    // A new copy is just thrown away; one of the user's is deleted after
    // a second click, and the default picked.
    let delete = {
        let (panel, plugin, key, set) = (panel.clone(), plugin.clone(), key.clone(), props.set.clone());
        let (confirm, current, set_draft) = (confirm.clone(), current.clone(), set_draft.clone());
        Callback::from(move |_: MouseEvent| {
            if !copy && !*confirm {
                confirm.set(true);
                return;
            }
            confirm.set(false);
            set_draft(None);
            if !copy {
                panel.send(PanelRequest::SavePluginLook {
                    plugin: plugin.clone(),
                    key: key.clone(),
                    name: current.clone(),
                    text: None,
                });
                set.emit(Value::String(String::new()));
            }
        })
    };
    let open = *editing || draft.is_some();
    let text = draft
        .as_ref()
        .map(|draft| draft.text.clone())
        .or_else(|| picked.as_ref().map(|look| look.text.clone()))
        .unwrap_or_default();
    let option = |look: &protocol::plugin::PluginLook| {
        html! { <option value={look.name.clone()} selected={look.name == current}>{ look.name.clone() }</option> }
    };
    let theirs: Vec<&protocol::plugin::PluginLook> = looks.iter().filter(|look| look.own).collect();
    html! {
        <>
            <div class="setting-row">
                <label class="setting-label" for={base.clone()}>{ props.label.clone() }</label>
                <div class="setting-control">
                    <div class="look-control">
                        <select id={base.clone()} onchange={pick} disabled={draft.is_some()}
                                title={draft.is_some().then_some("Save or discard what you typed first")}>
                            <optgroup label="Built in">
                                { for looks.iter().filter(|look| !look.own).map(option) }
                            </optgroup>
                            if !theirs.is_empty() || copy {
                                <optgroup label="Yours">
                                    { for theirs.into_iter().map(option) }
                                    if copy {
                                        <option value={current.clone()} selected=true>{ current.clone() }</option>
                                    }
                                </optgroup>
                            }
                        </select>
                        <button type="button" class="icon-button flat" id={format!("{base}-edit")} aria-label="Edit"
                                title="Edit" aria-expanded={open.to_string()} onclick={flip(&editing)}>
                            { icon("pencil") }
                        </button>
                        if props.help.is_some() {
                            <button type="button" class="icon-button flat" id={format!("{base}-help")} aria-label="Help"
                                    title="Help" aria-expanded={helping.to_string()} onclick={flip(&helping)}>
                                { icon("help") }
                            </button>
                        }
                    </div>
                </div>
            </div>
            if open {
                <div class="look-editor">
                    if own {
                        <div class="setting-row">
                            <label class="setting-label" for={format!("{base}-name")}>{ "Name" }</label>
                            <div class="setting-control">
                                <div class="look-control">
                                    <input type="text" id={format!("{base}-name")} spellcheck="false" value={shown_name}
                                           oninput={on_name} onchange={rename} onkeydown={name_keys} />
                                    if *confirm {
                                        <button type="button" class="button small danger" id={format!("{base}-delete")}
                                                onclick={delete}>{ "Click again to delete" }</button>
                                    } else {
                                        <button type="button" class="icon-button flat" id={format!("{base}-delete")}
                                                aria-label="Delete this look" title="Delete this look" onclick={delete}>
                                            { icon("trash") }
                                        </button>
                                    }
                                </div>
                                if let Some(problem) = name_problem {
                                    <div class="notice warn" role="alert">{ icon("warn") }<div>{ problem }</div></div>
                                }
                            </div>
                        </div>
                    }
                    <textarea id={format!("{base}-text")} class="css-editor mono" spellcheck="false"
                              aria-label={props.label.clone()} value={text} {oninput} />
                    <div class="button-row">
                        <button type="button" class="button small" id={format!("{base}-save")}
                                disabled={draft.is_none() || (copy && name_problem.is_some())} onclick={save}>{ "Save" }</button>
                        <button type="button" class="button ghost small" id={format!("{base}-discard")}
                                disabled={draft.is_none()} onclick={discard}>{ "Discard changes" }</button>
                    </div>
                    if own {
                        <p class="muted small">
                            if draft.is_some() { { "Not saved yet. " } }
                            { format!("Saved as plugin_data/{plugin}/{key}/{current}{} next to lsk.json; a save from any editor shows at once.", props.extension) }
                        </p>
                    }
                </div>
            }
            if let (Some(help), true) = (&props.help, *helping) {
                <div class="plugin-help">
                    <iframe sandbox="" src={help.clone()} title="Help" id={format!("{base}-help-page")}></iframe>
                </div>
            }
        </>
    }
}

#[derive(Properties, PartialEq)]
struct SecretFieldProps {
    plugin: String,
    key_name: String,
    placeholder: String,
}

/// A secret setting (a password): a password box, saved with Save (or
/// Enter) to `lsk_secrets.json` rather than the config, and Forget. The
/// panel never gets the value back, only whether one is saved; it isn't
/// undone with the config. Its row has its label.
#[function_component]
fn SecretField(props: &SecretFieldProps) -> Html {
    let panel = use_context::<Panel>().expect("panel context");
    let typed = use_state(String::new);
    let (plugin, key) = (props.plugin.clone(), props.key_name.clone());
    let base = format!("plugin-{plugin}-setting-{key}");
    let saved = panel.model.plugin_secrets.get(&plugin).is_some_and(|keys| keys.contains(&key));
    let send = {
        let (panel, plugin, key, typed) = (panel.clone(), plugin.clone(), key.clone(), typed.clone());
        move |value: Option<String>| {
            panel.send(PanelRequest::SavePluginSecret { plugin: plugin.clone(), key: key.clone(), value });
            typed.set(String::new());
        }
    };
    let oninput = {
        let typed = typed.clone();
        Callback::from(move |event: InputEvent| typed.set(util::value_of(&event)))
    };
    let save = {
        let (send, typed) = (send.clone(), typed.clone());
        move || {
            if !typed.is_empty() {
                send(Some((*typed).clone()));
            }
        }
    };
    let onkeydown = {
        let save = save.clone();
        Callback::from(move |event: KeyboardEvent| {
            if event.key() == "Enter" {
                event.prevent_default();
                save();
            }
        })
    };
    let onclick = Callback::from(move |_: MouseEvent| save());
    let forget = {
        let send = send.clone();
        Callback::from(move |_: MouseEvent| send(None))
    };
    let placeholder = if saved {
        "Saved; type to change it".to_string()
    } else if props.placeholder.is_empty() {
        "None saved".to_string()
    } else {
        props.placeholder.clone()
    };
    html! {
        <div class="button-row secret-field">
            <input type="password" id={base.clone()} autocomplete="off" spellcheck="false"
                   value={(*typed).clone()} {placeholder} {oninput} {onkeydown} />
            <button type="button" class="button small" id={format!("{base}-save")} disabled={typed.is_empty()}
                    {onclick}>{ "Save" }</button>
            if saved {
                <button type="button" class="button ghost small" id={format!("{base}-forget")}
                        onclick={forget}>{ "Forget" }</button>
            }
        </div>
    }
}

/// A text setting's choices: its own `suggestions`, then the list its
/// server code filled in that its `suggest_from` names (`value_of` gives
/// the other settings' values for `{key}`s in it), each value once.
pub(crate) fn suggested(
    panel: &Panel,
    plugin: &str,
    setting: &Setting,
    value_of: impl Fn(&str) -> Option<String>,
) -> Vec<Choice> {
    let SettingKind::Text { suggestions, suggest_from, .. } = &setting.kind else { return Vec::new() };
    let mut all: Vec<Choice> = suggestions.iter().cloned().map(Choice::plain).collect();
    let listed = options_list(suggest_from, value_of)
        .and_then(|name| panel.model.plugin_options.get(plugin)?.get(&name).cloned())
        .unwrap_or_default();
    for option in listed {
        if !all.iter().any(|each| each.value == option.value) {
            all.push(option);
        }
    }
    all
}

/// A `PickField`'s options and labels from choices.
pub(crate) fn pick_options(choices: &[Choice]) -> (Vec<String>, std::collections::BTreeMap<String, String>) {
    let options = choices.iter().map(|choice| choice.value.clone()).collect();
    let labels = choices
        .iter()
        .filter(|choice| choice.label != choice.value)
        .map(|choice| (choice.value.clone(), choice.label.clone()))
        .collect();
    (options, labels)
}

/// A text setting's field: a text box, or with suggestions a dropdown of
/// them that takes other values too (`PickField`; empty is the
/// placeholder, or "None"; with a `default` and no placeholder there's no
/// empty choice, and empty shows as the default).
pub(crate) fn text_field(
    id: &str,
    text: &str,
    placeholder: String,
    default: &str,
    suggestions: &[Choice],
    title: Option<&'static str>,
    set: Callback<Value>,
) -> Html {
    if suggestions.is_empty() {
        let oninput = set.reform(|event: InputEvent| Value::String(util::value_of(&event)));
        return html! {
            <input type="text" id={id.to_string()} {title} spellcheck="false" value={text.to_string()} {oninput}
                   {placeholder} />
        };
    }
    let (options, labels) = pick_options(suggestions);
    let (value, empty) = if placeholder.is_empty() && !default.is_empty() {
        (if text.trim().is_empty() { default } else { text }, None)
    } else {
        (text, Some(AttrValue::from(if placeholder.is_empty() { "None".to_string() } else { placeholder })))
    };
    html! {
        <PickField id={id.to_string()} value={value.to_string()} {options} {labels} {empty}
                   title={title.map(AttrValue::from)} onchange={set.reform(Value::String)} />
    }
}

/// A number as shown, with its unit: "8.0 s", "25%", "64 px".
pub(crate) fn show_number(unit: Unit, stored: f64) -> String {
    let shown = unit.show(stored);
    match unit {
        Unit::Seconds => format!("{shown:.1} s"),
        Unit::Percent => format!("{shown:.0}%"),
        Unit::Plain => util::show_number((shown * 100.0).round() / 100.0),
        _ => format!("{} {}", util::show_number((shown * 10.0).round() / 10.0), unit.suffix()),
    }
}

/// A choice with this many options or fewer, their labels this many
/// characters together, is a row of buttons rather than a dropdown (four
/// short ones fit the dock: the alerts' Above / Below / Left / Right).
const CHOICE_BUTTONS: usize = 4;
const CHOICE_BUTTON_CHARS: usize = 32;

/// A plugin-wide setting's row (in one of its presets, if given): its label
/// in the label column, its field beside it with the note its server code
/// put under it (`note`). A file setting's has the plugin's `help` page.
#[allow(clippy::too_many_arguments)]
fn setting_row(
    panel: &Panel,
    id: &str,
    setting: &Setting,
    settings_of: &[Setting],
    stored: &Map<String, Value>,
    preset: Option<&str>,
    note: Option<Html>,
    help: Option<&str>,
) -> Html {
    let key = setting.key.clone();
    let field = format!("plugin-{id}-setting-{}", setting.key);
    let value = setting.read(stored.get(&setting.key));
    let set = setter(panel, id, &setting.key, preset, &field);
    let text = value.as_str().unwrap_or_default().to_string();
    // Under its group's heading, the shorter label if it has one (the
    // whole one when pointed at).
    let mut label = if setting.short.is_empty() { setting.label.clone() } else { setting.short.clone() };
    let whole = (!setting.short.is_empty()).then(|| setting.label.clone());
    if let SettingKind::File { file, .. } = &setting.kind {
        let extension = file.rfind('.').map(|dot| file[dot..].to_string()).unwrap_or_default();
        return html! {
            <div class="look-setting" {key}>
                <LookField plugin={id.to_string()} key_name={setting.key.clone()} {label} {extension}
                           help={help.map(str::to_string)} value={text} {set} />
                { for note }
            </div>
        };
    }
    // Its label names the field (`<label for>`), or a group of buttons (a
    // grid of them, taller than a line; a list's lines).
    let (mut buttons, mut tall, mut list) = (false, false, false);
    let control = match &setting.kind {
        // Only nodes have these; the file has its row above.
        SettingKind::Emotes { .. } | SettingKind::Preset {} | SettingKind::File { .. } => return html! {},
        SettingKind::Secret { placeholder } => html! {
            <SecretField plugin={id.to_string()} key_name={setting.key.clone()} placeholder={placeholder.clone()} />
        },
        SettingKind::Bool { .. } => {
            let onchange = set.reform(|event: Event| Value::Bool(util::checked_of(&event)));
            html! {
                <div class="check-field">
                    <input type="checkbox" id={field.clone()} checked={value.as_bool().unwrap_or_default()}
                           title={whole.clone()} {onchange} />
                    if !setting.caption.is_empty() {
                        <label for={field.clone()}>{ setting.caption.clone() }</label>
                    }
                </div>
            }
        }
        SettingKind::Number { min: Some(min), max: Some(max), unit, step, slider: true, .. } => {
            let number = value.as_f64().unwrap_or_default();
            let (bounds, unit) = (setting.clone(), *unit);
            let oninput = set.reform(move |event: InputEvent| {
                let number = util::number_of(&event).unwrap_or_default();
                serde_json::json!(bounds.clamp(number))
            });
            let step = step.map_or_else(|| "any".to_string(), |step| step.to_string());
            html! {
                <div class="slider-row">
                    <input type="range" id={field.clone()} min={min.to_string()} max={max.to_string()} {step}
                           value={number.to_string()} {oninput} />
                    <span class="slider-value mono">{ show_number(unit, number) }</span>
                </div>
            }
        }
        SettingKind::Number { unit, .. } => {
            let (bounds, unit) = (setting.clone(), *unit);
            let onchange = set.reform(move |number: f64| serde_json::json!(bounds.clamp(unit.store(number))));
            let shown = unit.show(value.as_f64().unwrap_or_default());
            label = with_unit(&label, unit);
            // A short field, with its words after it ("messages at most").
            html! {
                <div class="number-row">
                    <NumberField id={field.clone()} value={shown} {onchange} />
                    if !setting.caption.is_empty() {
                        <span class="number-caption">{ setting.caption.clone() }</span>
                    }
                </div>
            }
        }
        SettingKind::Text { many: true, placeholder, .. } => {
            (buttons, tall, list) = (true, true, true);
            list_rows(&field, &label, &value, placeholder, set)
        }
        SettingKind::Text { placeholder, default, .. } => {
            let suggestions = suggested(panel, id, setting, |key| {
                let other = settings_of.iter().find(|other| other.key == key)?;
                other.read(stored.get(key)).as_str().map(str::to_string)
            });
            text_field(&field, &text, placeholder.clone(), default, &suggestions, None, set)
        }
        SettingKind::Position { random, .. } => {
            (buttons, tall) = (true, true);
            html! {
                <PositionPick id={field.clone()} label={label.clone()} value={value.clone()} random={*random}
                              onchange={set} />
            }
        }
        SettingKind::Colour { .. } => {
            let oninput = set.reform(|event: InputEvent| Value::String(util::value_of(&event)));
            html! { <input type="color" class="swatch" id={field.clone()} value={text} {oninput} /> }
        }
        SettingKind::Choice { options, .. }
            if options.len() <= CHOICE_BUTTONS
                && options.iter().map(|choice| choice.label.chars().count()).sum::<usize>()
                    <= CHOICE_BUTTON_CHARS =>
        {
            buttons = true;
            choice_buttons(&field, &label, options, &text, set)
        }
        SettingKind::Choice { options, .. } => {
            let onchange = set.reform(|event: Event| Value::String(util::value_of(&event)));
            html! {
                <select id={field.clone()} class="choice" {onchange}>
                    { for options.iter().map(|choice| html! {
                        <option value={choice.value.clone()} selected={choice.value == text}>{ choice.label.clone() }</option>
                    }) }
                </select>
            }
        }
        SettingKind::Picture { .. } | SettingKind::Media { .. } => {
            let videos = matches!(setting.kind, SettingKind::Media { .. });
            let onchange = set.reform(Value::String);
            let then = set.reform(Value::String);
            html! {
                <div class="picture-pick">
                    <PicturePick id={field.clone()} value={text} {videos} {onchange} />
                    <PictureUpload id={format!("{field}-upload")} {videos} {then} />
                </div>
            }
        }
        SettingKind::Sound { .. } => {
            let onchange = set.reform(Value::String);
            let then = set.reform(Value::String);
            html! {
                <div class="picture-pick">
                    <SoundPick id={field.clone()} value={text} {onchange} />
                    <SoundUpload id={format!("{field}-upload")} {then} compact=true />
                </div>
            }
        }
    };
    // Tick boxes beside its field, as small buttons (Bold beside Font).
    let toggles: Vec<Html> = settings_of
        .iter()
        .filter(|other| other.beside == setting.key && matches!(other.kind, SettingKind::Bool { .. }))
        .map(|other| toggle_button(panel, id, other, stored, preset))
        .collect();
    let control = if toggles.is_empty() {
        control
    } else {
        html! { <div class="with-toggles">{ control }{ for toggles }</div> }
    };
    html! {
        <div class={classes!("setting-row", tall.then_some("tall"), list.then_some("list"))} {key}>
            if buttons {
                <span class="setting-label" id={format!("{field}-label")} title={whole}>{ label }</span>
            } else {
                <label class="setting-label" for={field} title={whole}>{ label }</label>
            }
            <div class="setting-control">
                { control }
                { for note }
            </div>
        </div>
    }
}

/// Something a plugin wants that's only done once allowed: ⚠, what it
/// wants, and Allow on the right, on its first line (under it when the box
/// is too narrow).
fn allow_notice(id: String, text: String, button: String, onclick: Callback<MouseEvent>) -> Html {
    html! {
        <div class="notice warn with-action" role="status" {id}>
            { icon("warn") }
            <div class="notice-text">{ text }</div>
            <button type="button" class="button small" id={button} {onclick}>{ "Allow" }</button>
        </div>
    }
}

/// A list setting (`many`): a line each (`{id}-{index}`) with × to take
/// it out, and Add for another.
fn list_rows(id: &str, label: &str, value: &Value, placeholder: &str, set: Callback<Value>) -> Html {
    let items: Vec<String> = value
        .as_array()
        .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    // Each change: the whole list as the setting's value.
    let add = {
        let items = items.clone();
        set.reform(move |_: MouseEvent| {
            let mut items = items.clone();
            items.push(String::new());
            Value::from(items)
        })
    };
    html! {
        <div class="list-rows" role="group" aria-label={label.to_string()} id={id.to_string()}>
            { for items.iter().enumerate().map(|(index, item)| {
                let oninput = {
                    let items = items.clone();
                    set.reform(move |event: InputEvent| {
                        let mut items = items.clone();
                        items[index] = util::value_of(&event);
                        Value::from(items)
                    })
                };
                let onclick = {
                    let items = items.clone();
                    set.reform(move |_: MouseEvent| {
                        let mut items = items.clone();
                        items.remove(index);
                        Value::from(items)
                    })
                };
                html! {
                    <div class="list-row">
                        <input type="text" id={format!("{id}-{index}")} aria-label={format!("{label} {}", index + 1)}
                               spellcheck="false" value={item.clone()} placeholder={placeholder.to_string()} {oninput} />
                        <button type="button" class="tier-remove" id={format!("{id}-{index}-remove")}
                                title="Take it out" aria-label={format!("Take out {item}")} {onclick}>{ "×" }</button>
                    </div>
                }
            }) }
            <div>
                <button type="button" class="button ghost small" id={format!("{id}-add")} onclick={add}>
                    { icon("plus") }{ "Add" }
                </button>
            </div>
        </div>
    }
}

/// A choice with few options as a row of buttons (`{id}-{value}`), the
/// picked one pressed.
pub(crate) fn choice_buttons(
    id: &str,
    label: &str,
    options: &[Choice],
    value: &str,
    set: Callback<Value>,
) -> Html {
    html! {
        <div class="choice-buttons" role="radiogroup" aria-label={label.to_string()} id={id.to_string()}>
            { for options.iter().map(|choice| {
                let on = choice.value == value;
                let picked = choice.value.clone();
                let onclick = set.reform(move |_: MouseEvent| Value::String(picked.clone()));
                html! {
                    <button type="button" role="radio" aria-checked={on.to_string()}
                            id={format!("{id}-{}", choice.value)} class={classes!(on.then_some("on"))} {onclick}>
                        { choice.label.clone() }
                    </button>
                }
            }) }
        </div>
    }
}

/// A number setting's label with its unit: "Lifetime (s)".
pub(crate) fn with_unit(label: &str, unit: Unit) -> String {
    match unit.suffix() {
        "" => label.to_string(),
        suffix => format!("{label} ({suffix})"),
    }
}
