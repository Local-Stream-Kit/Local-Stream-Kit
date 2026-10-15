//! One node on the canvas: its header, its rows (outputs, settings, inputs
//! with their stand-in fields) and its problems. Row heights come from
//! [`layout`], so the sockets sit where the wires expect them.

use protocol::cards::{Card, MathOp, Node, NodeId, NodeKind, NumberTest, Socket, SocketType, TextTest, Tier};
use protocol::overrides::{FieldKind, field_of, value_of};
use protocol::panel::PanelRequest;
use protocol::plugin::{Attached, Choice, RANDOM_EMOTES, Setting as PluginSetting, SettingKind};
use serde_json::Value;
use web_sys::PointerEvent;
use yew::prelude::*;
use yew::virtual_dom::VList;

use super::fields::{ListField, NumberField, PickField};
use super::layout::{self, Row, RowKind, Setting};
use super::template::TemplateField;
use super::{edit_card, edit_node};
use crate::Panel;
use crate::util;
use crate::views::new_card::category_class;
use crate::views::position::PositionField;
use crate::views::sounds::{PicturePick, PictureUpload, SoundPick, SoundUpload};
use crate::views::{icon, node_icon};

/// What templates can hold, shown as the text fields' tooltip.
const TEMPLATE_HINT: &str = "{name} fills in a value from the event; {e:Kappa} shows that emote; [highlight]…[/] gives a part a text style (Plugins → Text styles)";

pub fn type_class(ty: SocketType) -> &'static str {
    match ty {
        SocketType::Event => "event",
        SocketType::Number => "number",
        SocketType::Text => "text",
        SocketType::Emotes => "emotes",
    }
}

/// What the canvas hands every node.
pub struct NodeContext<'a> {
    pub panel: &'a Panel,
    pub card: &'a Card,
    /// Pointer down on the header: select and start moving.
    pub on_head: Callback<(NodeId, PointerEvent)>,
    /// Pointer down elsewhere on the node: select.
    pub on_body: Callback<(NodeId, PointerEvent)>,
    /// Pointer down on a socket: `(node, socket, is_input, event)`.
    pub on_socket: Callback<(NodeId, String, bool, PointerEvent)>,
    /// Pointer down on a Text node's corner: start resizing it (or, the
    /// second of a double click, back to the size it opens at).
    pub on_resize: Callback<(NodeId, PointerEvent)>,
}

pub fn view(context: &NodeContext, node: &Node, selected: bool, problems: &[String]) -> Html {
    let id = node.id;
    let onpointerdown = {
        let on_body = context.on_body.clone();
        Callback::from(move |event: PointerEvent| on_body.emit((id, event)))
    };
    let on_head = {
        let on_head = context.on_head.clone();
        Callback::from(move |event: PointerEvent| on_head.emit((id, event)))
    };
    let style = format!("left: {}px; top: {}px; width: {}px", node.x, node.y, layout::width(&node.kind));
    let class = classes!("node", selected.then_some("selected"), (!problems.is_empty()).then_some("problem"));
    html! {
        <div {class} {style} data-node={id.to_string()} {onpointerdown} key={id.to_string()}>
            <div class={classes!("node-head", category_class(node.kind.category()))} onpointerdown={on_head}>
                <span class="node-name">
                    { node_icon(&context.panel.model.plugins, &node.kind) }
                    { title(&node.kind) }
                </span>
            </div>
            <div class="node-body">
                { for layout::rows(node, context.card).iter().map(|row| keyed(row_key(&row.kind), view_row(context, node, row))) }
            </div>
            // In a container of their own: Yew matches unkeyed siblings from
            // the end, so a problem coming or going next to the body would
            // rebuild the body (and take the focus out of a field).
            <div class="node-problems">
                { for problems.iter().map(|problem| html! { <div class="node-problem">{ problem.clone() }</div> }) }
            </div>
            if matches!(node.kind, NodeKind::Text { .. }) {
                <div class="node-resize" title="Drag to resize; double-click for the usual size"
                     onpointerdown={context.on_resize.reform(move |event: PointerEvent| (id, event))}></div>
            }
        </div>
    }
}

/// A missing plugin's node shows which plugin and node it was.
fn title(kind: &NodeKind) -> String {
    match kind {
        NodeKind::Plugin { plugin, node, ty: Attached(None), .. } => format!("{plugin}: {node}"),
        kind => kind.title().to_string(),
    }
}

/// Rows come and go (the emote codes when "These emotes" is picked), so
/// they're keyed: the others keep their elements, and their focus.
fn row_key(row: &RowKind) -> String {
    match row {
        RowKind::Output(socket) => format!("out-{}", socket.name),
        RowKind::Input { socket, .. } => format!("in-{}", socket.name),
        RowKind::Pair { input, output } => format!("pair-{}-{}", input.name, output.name),
        RowKind::Setting(setting) => format!("set-{setting:?}"),
    }
}

fn keyed(key: String, html: Html) -> Html {
    VList::with_children(vec![html], Some(key.into())).into()
}

fn socket(context: &NodeContext, node: NodeId, socket: &Socket, input: bool) -> Html {
    let onpointerdown = {
        let on_socket = context.on_socket.clone();
        let name = socket.name.clone();
        Callback::from(move |event: PointerEvent| on_socket.emit((node, name.clone(), input, event)))
    };
    html! {
        <span class={classes!("socket", if input { "in" } else { "out" }, type_class(socket.ty))}
              data-node={node.to_string()} data-socket={socket.name.clone()}
              data-input={input.to_string()} {onpointerdown}></span>
    }
}

fn view_row(context: &NodeContext, node: &Node, row: &Row) -> Html {
    let style = format!("height: {}px", row.height);
    match &row.kind {
        RowKind::Output(out) if matches!(node.kind, NodeKind::Rarity { .. }) => {
            view_tier(context, node, out, style)
        }
        RowKind::Output(out) => html! {
            <div class="node-row out" {style}>
                <span>{ out.label.clone() }</span>
                { socket(context, node.id, out, false) }
            </div>
        },
        RowKind::Input { socket: input, wired } => {
            let field = if *wired { None } else { input_field(context, node, input) };
            match field {
                None => html! {
                    <div class="node-row" {style}>
                        { socket(context, node.id, input, true) }
                        <span>{ input.label.clone() }</span>
                    </div>
                },
                Some((field, column)) => html! {
                    <div class={classes!("node-row", column.then_some("column"))} {style}>
                        { socket(context, node.id, input, true) }
                        <label for={field_id(node.id, &input.name)}>{ input.label.clone() }</label>
                        { field }
                    </div>
                },
            }
        }
        // An input with no field and an output, side by side.
        RowKind::Pair { input, output } => html! {
            <div class="node-row pair" {style}>
                { socket(context, node.id, input, true) }
                <span>{ input.label.clone() }</span>
                <span class="pair-out">{ output.label.clone() }</span>
                { socket(context, node.id, output, false) }
            </div>
        },
        RowKind::Setting(setting) => view_setting(context, node, *setting, style),
    }
}

/// A rarity's output: the tier's name and chance (or the rest's name and
/// what's left), then its socket.
fn view_tier(context: &NodeContext, node: &Node, out: &Socket, style: String) -> Html {
    let NodeKind::Rarity { tiers, .. } = &node.kind else { return html! {} };
    let id = node.id;
    if out.name == "rest" {
        let left = 100.0 - tiers.iter().map(|tier| tier.percent).sum::<f64>();
        let left = format!("{:.2}", left.max(0.0));
        let left = left.trim_end_matches('0').trim_end_matches('.');
        let oninput = text_change(context, id, "rest", |kind, value| {
            if let NodeKind::Rarity { rest, .. } = kind {
                *rest = value;
            }
        });
        return html! {
            <div class="node-row out tier" {style}>
                <span class="tier-remove-space"></span>
                <input type="text" id={field_id(id, "rest")} aria-label="Name for the rest" spellcheck="false"
                       value={out.label.clone()} {oninput} />
                <span class="tier-left" title="The chance that's left">{ format!("{left} %") }</span>
                { socket(context, id, out, false) }
            </div>
        };
    }
    let Some(tier) = tiers.iter().find(|tier| tier.socket() == out.name) else { return html! {} };
    let tier_id = tier.id;
    let name_field = format!("{}-name", out.name);
    let percent_field = format!("{}-percent", out.name);
    let oninput = text_change(context, id, &name_field, move |kind, value| {
        if let Some(tier) = tier_mut(kind, tier_id) {
            tier.name = value;
        }
    });
    let onchange = change(context, id, &percent_field, move |kind, value: f64| {
        if let Some(tier) = tier_mut(kind, tier_id) {
            tier.percent = value;
        }
    });
    let remove = {
        let panel = context.panel.clone();
        let card = context.card.id.clone();
        let socket = out.name.clone();
        Callback::from(move |_| {
            let socket = socket.clone();
            edit_card(&panel, &card, None, move |card| {
                if let Some(NodeKind::Rarity { tiers, .. }) =
                    card.nodes.iter_mut().find(|each| each.id == id).map(|node| &mut node.kind)
                {
                    tiers.retain(|tier| tier.id != tier_id);
                }
                card.wires.retain(|wire| !(wire.from.node == id && wire.from.socket == socket));
            });
        })
    };
    html! {
        <div class="node-row out tier" {style}>
            <button type="button" class="tier-remove" title="Remove this tier"
                    aria-label={format!("Remove {}", tier.name)} onclick={remove}>{ "×" }</button>
            <input type="text" id={field_id(id, &name_field)} aria-label="Tier name" spellcheck="false"
                   value={tier.name.clone()} {oninput} />
            <NumberField id={field_id(id, &percent_field)} value={tier.percent} {onchange} />
            <span class="tier-unit">{ "%" }</span>
            { socket(context, id, out, false) }
        </div>
    }
}

/// The value a new voice change of `key` starts at: the speech settings'.
fn current_value(context: &NodeContext, kind: &NodeKind, key: &str) -> Option<Value> {
    let config = context.panel.model.config.as_ref()?;
    match kind {
        NodeKind::Plugin { .. } => value_of(&config.effects.speech.settings, key),
        _ => None,
    }
}

fn set_override(kind: &mut NodeKind, key: &str, value: Option<Value>) {
    if let Some(overrides) = kind.overrides_mut() {
        match value {
            Some(value) => overrides.insert(key.to_string(), value),
            None => overrides.remove(key),
        };
    }
}

/// "+ Change a setting": adds an override, starting at the current value.
fn view_add_override(context: &NodeContext, node: &Node, kind: &NodeKind, style: String) -> Html {
    let Some((overrides, fields)) = kind.overrides() else { return html! {} };
    let starts: Vec<(&'static str, Value)> = fields
        .iter()
        .filter(|field| !overrides.contains_key(field.key))
        .filter_map(|field| Some((field.key, current_value(context, kind, field.key)?)))
        .collect();
    if starts.is_empty() {
        return html! {};
    }
    let options = starts.clone();
    let onchange = change(context, node.id, "add-override", move |kind, key: String| {
        if let Some((_, value)) = starts.iter().find(|(each, _)| *each == key) {
            set_override(kind, &key, Some(value.clone()));
        }
    })
    .reform(|event: Event| util::value_of(&event));
    let id = field_id(node.id, "add-override");
    // A plugin's node has its own "+ Change a setting…" for the plugin's
    // settings; these are the text to speech settings for what it reads.
    let (label, prompt) = match kind {
        NodeKind::Plugin { .. } => ("Change a voice setting for this node", "+ Change a voice setting"),
        _ => ("Change a setting for this node", "+ Change a setting"),
    };
    html! {
        <div class="node-row" {style}>
            <label for={id.clone()} class="visually-hidden">{ label }</label>
            <select {id} class="add-setting" {onchange}
                    title="Change one setting for this node only; the rest stay as set on the Plugins page">
                <option value="" selected=true>{ prompt }</option>
                { for options.iter().filter_map(|(key, _)| field_of(fields, key)).map(|field| html! {
                    <option value={field.key}>{ field.label }</option>
                }) }
            </select>
        </div>
    }
}

/// One changed setting: its field, and × to go back to the shared value.
fn view_override(
    context: &NodeContext,
    node: &Node,
    key: &'static str,
    kind: &NodeKind,
    style: String,
) -> Html {
    let Some((overrides, fields)) = kind.overrides() else { return html! {} };
    let (Some(field), Some(value)) = (field_of(fields, key), overrides.get(key)) else { return html! {} };
    let id = field_id(node.id, &format!("override-{key}"));
    let set = |context: &NodeContext| {
        change(context, node.id, &format!("override-{key}"), move |kind, value: Value| {
            set_override(kind, key, Some(value))
        })
    };
    let text = value.as_str().unwrap_or_default().to_string();
    let input = match field.kind {
        FieldKind::Number { unit, .. } => {
            let shown = unit.show(value.as_f64().unwrap_or_default());
            let onchange = set(context).reform(move |number: f64| serde_json::json!(unit.store(number)));
            html! { <NumberField id={id.clone()} value={shown} {onchange} /> }
        }
        FieldKind::Colour => {
            let oninput = set(context).reform(|event: InputEvent| Value::String(util::value_of(&event)));
            html! { <input type="color" id={id.clone()} class="override-colour" value={text} {oninput} /> }
        }
        FieldKind::Text => {
            let oninput = set(context).reform(|event: InputEvent| Value::String(util::value_of(&event)));
            html! { <input type="text" id={id.clone()} spellcheck="false" value={text} {oninput} /> }
        }
        FieldKind::Bool => {
            let onchange = set(context).reform(|event: Event| Value::Bool(util::checked_of(&event)));
            html! { <input type="checkbox" id={id.clone()} checked={value.as_bool().unwrap_or_default()} {onchange} /> }
        }
        FieldKind::Picture => picture_select(id.clone(), text, false, set(context)),
        FieldKind::Sound => {
            let onchange = set(context).reform(Value::String);
            html! { <SoundPick id={id.clone()} value={text} {onchange} /> }
        }
        FieldKind::Voice => {
            let onchange = set(context).reform(|event: Event| Value::String(util::value_of(&event)));
            html! { <select id={id.clone()} {onchange}>{ crate::views::speech::voice_options(context.panel, &text) }</select> }
        }
        FieldKind::Choice(choices) => {
            let onchange = set(context).reform(|event: Event| Value::String(util::value_of(&event)));
            html! {
                <select id={id.clone()} {onchange}>
                    { for choices().into_iter().map(|(value, label)| html! {
                        <option selected={value == text} value={value.clone()}>{ label }</option>
                    }) }
                </select>
            }
        }
    };
    let suffix = match field.kind {
        FieldKind::Number { unit, .. } if !unit.suffix().is_empty() => format!(" ({})", unit.suffix()),
        _ => String::new(),
    };
    let remove = {
        let panel = context.panel.clone();
        let card = context.card.id.clone();
        let node = node.id;
        Callback::from(move |_| {
            edit_node(&panel, &card, node, None, move |kind| set_override(kind, key, None))
        })
    };
    html! {
        <div class="node-row override" {style}>
            <label for={id} title={format!("{}{suffix}: for this node only", field.label)}>{ format!("{}{suffix}", field.label) }</label>
            { input }
            <button type="button" class="tier-remove" title="Back to the shared setting"
                    aria-label={format!("Stop changing {}", field.label)} onclick={remove}>{ "×" }</button>
        </div>
    }
}

/// A pick from the pictures folder, or none; `videos`: its videos too.
fn picture_select(id: String, text: String, videos: bool, set: Callback<Value>) -> Html {
    let onchange = set.reform(Value::String);
    html! { <PicturePick {id} value={text} {videos} {onchange} /> }
}

/// A plugin node's setting with a row of its own, edited by its type.
fn view_plugin_setting(context: &NodeContext, node: &Node, index: usize, style: String) -> Html {
    let NodeKind::Plugin { plugin, settings, ty: Attached(Some(ty)), .. } = &node.kind else {
        return html! {};
    };
    let Some(setting) = ty.settings.get(index) else { return html! {} };
    let stored = setting.read(settings.get(&setting.key));
    let id = field_id(node.id, &format!("plugin-{}", setting.key));
    let set = {
        let key = setting.key.clone();
        change(context, node.id, &format!("plugin-{key}"), move |kind, value: Value| {
            set_plugin(kind, &key, value)
        })
    };
    let label = setting.label.clone();
    let text = stored.as_str().unwrap_or_default().to_string();
    if let SettingKind::Text { many: true, placeholder, .. } = &setting.kind {
        return view_add_pick(context, node, setting, &stored, placeholder, &id, style);
    }
    let input = match &setting.kind {
        SettingKind::Bool { .. } => {
            let onchange = set.reform(|event: Event| Value::Bool(util::checked_of(&event)));
            // In the fields' column, under the fields.
            return html! {
                <div class="node-row check lined" {style}>
                    <span class="check-in-column">
                        <input type="checkbox" id={id.clone()} checked={stored.as_bool().unwrap_or_default()} {onchange} />
                        <label for={id}>{ label }</label>
                    </span>
                </div>
            };
        }
        SettingKind::Number { unit, .. } => {
            let (bounds, unit) = (setting.clone(), *unit);
            let onchange = set.reform(move |number: f64| serde_json::json!(bounds.clamp(unit.store(number))));
            let shown = unit.show(stored.as_f64().unwrap_or_default());
            html! { <NumberField id={id.clone()} value={shown} {onchange} /> }
        }
        SettingKind::Text { placeholder, .. } => {
            let suggestions = crate::views::plugins::suggested(context.panel, plugin, setting, |key| {
                let other = ty.settings.iter().find(|other| other.key == key)?;
                other.read(settings.get(key)).as_str().map(str::to_string)
            });
            if suggestions.is_empty() {
                let onchange = set.reform(Value::String);
                html! {
                    <TemplateField id={id.clone()} multiline={false} title={TEMPLATE_HINT} value={text.clone()}
                                   placeholder={placeholder.clone()} {onchange} />
                }
            } else {
                // A template: empty means empty, never the default.
                crate::views::plugins::text_field(
                    &id,
                    &text,
                    placeholder.clone(),
                    "",
                    &suggestions,
                    Some(TEMPLATE_HINT),
                    set,
                )
            }
        }
        SettingKind::Position { random, .. } => html! {
            <PositionField id={id.clone()} label={label.clone()} value={stored.clone()} random={*random} onchange={set} />
        },
        SettingKind::Colour { .. } => {
            let oninput = set.reform(|event: InputEvent| Value::String(util::value_of(&event)));
            html! { <input type="color" id={id.clone()} class="override-colour" value={text} {oninput} /> }
        }
        SettingKind::Choice { options, .. } => {
            let onchange = set.reform(|event: Event| Value::String(util::value_of(&event)));
            html! {
                <select id={id.clone()} {onchange}>
                    { for options.iter().map(|choice| html! {
                        <option value={choice.value.clone()} selected={choice.value == text}>{ choice.label.clone() }</option>
                    }) }
                </select>
            }
        }
        SettingKind::Picture { .. } | SettingKind::Media { .. } => {
            let videos = matches!(setting.kind, SettingKind::Media { .. });
            let then = set.reform(Value::String);
            html! {
                <div class="picture-pick">
                    { picture_select(id.clone(), text, videos, set) }
                    <PictureUpload id={format!("{id}-upload")} {videos} {then} />
                </div>
            }
        }
        // Always stands in for an input (its field is the input's).
        SettingKind::Emotes { .. } | SettingKind::File { .. } | SettingKind::Secret { .. } => return html! {},
        SettingKind::Preset {} => {
            let NodeKind::Plugin { plugin, .. } = &node.kind else { return html! {} };
            let config = context.panel.model.config.as_ref();
            let presets = config.map(|config| config.plugin(plugin).presets()).unwrap_or_default();
            let chosen = config.map(|config| config.plugin(plugin).preset(&text).id).unwrap_or_default();
            let onchange = set.reform(|event: Event| Value::String(util::value_of(&event)));
            html! {
                <select id={id.clone()} {onchange}>
                    { for presets.iter().map(|preset| html! {
                        <option value={preset.id.clone()} selected={preset.id == chosen}>{ preset.name.clone() }</option>
                    }) }
                </select>
            }
        }
        SettingKind::Sound { .. } => {
            // ▶ plays it at the node's volume setting (0–100), if it has one.
            let volume = ty
                .setting("volume")
                .filter(|volume| matches!(volume.kind, SettingKind::Number { .. }))
                .and_then(|volume| volume.read(settings.get("volume")).as_f64())
                .map_or(0.8, |percent| (percent / 100.0) as f32);
            let onchange = set.reform(Value::String);
            let then = set.reform(Value::String);
            html! {
                <div class="picture-pick">
                    <SoundPick id={id.clone()} value={text} {volume} {onchange} />
                    <SoundUpload id={format!("{id}-upload")} {then} compact=true />
                </div>
            }
        }
    };
    let label = match &setting.kind {
        SettingKind::Number { unit, .. } => crate::views::plugins::with_unit(&label, *unit),
        _ => label,
    };
    html! {
        <div class="node-row" {style}>
            <label for={id}>{ label }</label>
            { input }
        </div>
    }
}

/// The values of a plugin node's setting with several (`many`).
fn picks(stored: &Value) -> Vec<String> {
    stored
        .as_array()
        .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// What a plugin node's setting suggests (its list from server code).
fn plugin_suggestions(context: &NodeContext, node: &Node, setting: &PluginSetting) -> Vec<Choice> {
    let NodeKind::Plugin { plugin, settings, ty: Attached(Some(ty)), .. } = &node.kind else {
        return Vec::new();
    };
    crate::views::plugins::suggested(context.panel, plugin, setting, |key| {
        let other = ty.settings.iter().find(|other| other.key == key)?;
        other.read(settings.get(key)).as_str().map(str::to_string)
    })
}

/// A setting with several values (`many`): its label, and a dropdown that
/// adds one (those not picked yet, or "Other…" for one to type); the
/// values follow, a row each. With none: its placeholder ("Any scene").
fn view_add_pick(
    context: &NodeContext,
    node: &Node,
    setting: &PluginSetting,
    stored: &Value,
    placeholder: &str,
    id: &str,
    style: String,
) -> Html {
    let picked = picks(stored);
    let options: Vec<Choice> = plugin_suggestions(context, node, setting)
        .into_iter()
        .filter(|option| !picked.contains(&option.value))
        .collect();
    let key = setting.key.clone();
    let add = change(context, node.id, &format!("plugin-{key}"), move |kind, added: String| {
        let NodeKind::Plugin { settings, .. } = kind else { return };
        let mut values = picks(settings.get(&key).unwrap_or(&Value::Null));
        values.push(added);
        settings.insert(key.clone(), Value::from(values));
    });
    let onchange = Callback::from(move |event: Event| {
        let picked = util::value_of(&event);
        // Back to the prompt: the next pick adds another.
        if let Some(select) = event.target_dyn_into::<web_sys::HtmlSelectElement>() {
            select.set_value("");
        }
        match picked.as_str() {
            "" => {}
            OTHER_PICK => add.emit(String::new()),
            _ => add.emit(picked),
        }
    });
    let prompt = if !picked.is_empty() {
        "Add…".to_string()
    } else if placeholder.is_empty() {
        "Any".to_string()
    } else {
        placeholder.to_string()
    };
    html! {
        <div class="node-row" {style}>
            <label for={id.to_string()}>{ setting.label.clone() }</label>
            <select id={id.to_string()} class={classes!((!picked.is_empty()).then_some("add-override"))} {onchange}>
                <option value="" selected=true>{ prompt }</option>
                { for options.iter().map(|option| html! { <option value={option.value.clone()}>{ option.label.clone() }</option> }) }
                <option value={OTHER_PICK}>{ "Other…" }</option>
            </select>
        </div>
    }
}

/// The add dropdown's "Other…": a value to type.
const OTHER_PICK: &str = "\u{1}other";

/// One value of a setting with several: picked or typed, and × to take it
/// out.
fn view_plugin_pick(context: &NodeContext, node: &Node, index: usize, pick: usize, style: String) -> Html {
    let NodeKind::Plugin { settings, ty: Attached(Some(ty)), .. } = &node.kind else { return html! {} };
    let Some(setting) = ty.settings.get(index) else { return html! {} };
    let picked = picks(&setting.read(settings.get(&setting.key)));
    let Some(value) = picked.get(pick).cloned() else { return html! {} };
    let field = format!("plugin-{}-{pick}", setting.key);
    let id = field_id(node.id, &field);
    let edit = |remove: bool| {
        let key = setting.key.clone();
        move |kind: &mut NodeKind, value: String| {
            let NodeKind::Plugin { settings, .. } = kind else { return };
            let mut values = picks(settings.get(&key).unwrap_or(&Value::Null));
            if pick < values.len() {
                if remove {
                    values.remove(pick);
                } else {
                    values[pick] = value;
                }
            }
            settings.insert(key.clone(), Value::from(values));
        }
    };
    let onchange = change(context, node.id, &field, edit(false));
    let remove = {
        let panel = context.panel.clone();
        let card = context.card.id.clone();
        let (node, apply) = (node.id, edit(true));
        Callback::from(move |_: MouseEvent| {
            let apply = apply.clone();
            edit_node(&panel, &card, node, None, move |kind| apply(kind, String::new()))
        })
    };
    let (options, labels) = crate::views::plugins::pick_options(&plugin_suggestions(context, node, setting));
    html! {
        <div class="node-row override pick" {style}>
            <label for={id.clone()} class="visually-hidden">{ format!("{} {}", setting.label, pick + 1) }</label>
            <PickField id={id} {value} {options} {labels} {onchange} />
            <button type="button" class="tier-remove" title="Take it out"
                    aria-label={format!("Take out {} {}", setting.label, pick + 1)} onclick={remove}>{ "×" }</button>
        </div>
    }
}

/// One of the plugin's own settings this node changes: its field, and × to
/// go back to the plugin's.
fn view_plugin_change(context: &NodeContext, node: &Node, index: usize, style: String) -> Html {
    let NodeKind::Plugin { changes, ty: Attached(Some(ty)), .. } = &node.kind else { return html! {} };
    let Some(setting) = ty.changeable.get(index) else { return html! {} };
    let Some(stored) = changes.get(&setting.key) else { return html! {} };
    let stored = setting.read(Some(stored));
    let key = setting.key.clone();
    let field = format!("change-{key}");
    let id = field_id(node.id, &field);
    let set = {
        let key = key.clone();
        change(context, node.id, &field, move |kind, value: Value| set_change(kind, &key, Some(value)))
    };
    let text = stored.as_str().unwrap_or_default().to_string();
    let input = match &setting.kind {
        SettingKind::Number { unit, .. } => {
            let (bounds, unit) = (setting.clone(), *unit);
            let onchange = set.reform(move |number: f64| serde_json::json!(bounds.clamp(unit.store(number))));
            let shown = unit.show(stored.as_f64().unwrap_or_default());
            html! { <NumberField id={id.clone()} value={shown} {onchange} /> }
        }
        SettingKind::Bool { .. } => {
            let onchange = set.reform(|event: Event| Value::Bool(util::checked_of(&event)));
            html! { <input type="checkbox" id={id.clone()} checked={stored.as_bool().unwrap_or_default()} {onchange} /> }
        }
        SettingKind::Colour { .. } => {
            let oninput = set.reform(|event: InputEvent| Value::String(util::value_of(&event)));
            html! { <input type="color" id={id.clone()} class="override-colour" value={text} {oninput} /> }
        }
        SettingKind::Choice { options, .. } => {
            let onchange = set.reform(|event: Event| Value::String(util::value_of(&event)));
            html! {
                <select id={id.clone()} {onchange}>
                    { for options.iter().map(|choice| html! {
                        <option value={choice.value.clone()} selected={choice.value == text}>{ choice.label.clone() }</option>
                    }) }
                </select>
            }
        }
        SettingKind::Picture { .. } | SettingKind::Media { .. } => {
            let videos = matches!(setting.kind, SettingKind::Media { .. });
            picture_select(id.clone(), text, videos, set)
        }
        SettingKind::Sound { .. } => {
            let onchange = set.reform(Value::String);
            html! { <SoundPick id={id.clone()} value={text} {onchange} /> }
        }
        SettingKind::Position { random, .. } => html! {
            <PositionField id={id.clone()} label={setting.label.clone()} value={stored.clone()} random={*random}
                           onchange={set} />
        },
        SettingKind::Text { suggestions, placeholder, default, .. } => {
            let suggestions: Vec<Choice> = suggestions.iter().cloned().map(Choice::plain).collect();
            crate::views::plugins::text_field(
                &id,
                &text,
                placeholder.clone(),
                default,
                &suggestions,
                None,
                set,
            )
        }
        SettingKind::Emotes { .. } | SettingKind::Preset {} => {
            let oninput = set.reform(|event: InputEvent| Value::String(util::value_of(&event)));
            html! { <input type="text" id={id.clone()} spellcheck="false" value={text} {oninput} /> }
        }
        // The plugin's own only, never a node's.
        SettingKind::File { .. } | SettingKind::Secret { .. } => html! {},
    };
    let label = match &setting.kind {
        SettingKind::Number { unit, .. } => crate::views::plugins::with_unit(&setting.label, *unit),
        _ => setting.label.clone(),
    };
    let remove = {
        let panel = context.panel.clone();
        let card = context.card.id.clone();
        let node = node.id;
        Callback::from(move |_| {
            let key = key.clone();
            edit_node(&panel, &card, node, None, move |kind| set_change(kind, &key, None))
        })
    };
    html! {
        <div class="node-row override" {style}>
            <label for={id} title={format!("{label}: for this node only")}>{ label.clone() }</label>
            { input }
            <button type="button" class="tier-remove" title="Back to the plugin's setting"
                    aria-label={format!("Stop changing {}", setting.label)} onclick={remove}>{ "×" }</button>
        </div>
    }
}

/// "+ Change a setting": the plugin's own settings this node can change
/// and doesn't yet, each starting at the plugin's value (its preset's,
/// for a plugin with presets).
fn view_add_plugin_change(context: &NodeContext, node: &Node, style: String) -> Html {
    let NodeKind::Plugin { plugin, settings, changes, ty: Attached(Some(ty)), .. } = &node.kind else {
        return html! {};
    };
    let stored = context.panel.model.config.as_ref().map(|config| config.plugin(plugin)).unwrap_or_default();
    let own = match ty.preset_setting() {
        Some(setting) => {
            stored.preset(setting.read(settings.get(&setting.key)).as_str().unwrap_or_default()).settings
        }
        None => stored.settings,
    };
    let starts: Vec<(String, String, Value)> = ty
        .changeable
        .iter()
        .filter(|setting| !changes.contains_key(&setting.key))
        .map(|setting| (setting.key.clone(), setting.label.clone(), setting.read(own.get(&setting.key))))
        .collect();
    if starts.is_empty() {
        return html! {};
    }
    let options: Vec<(String, String)> =
        starts.iter().map(|(key, label, _)| (key.clone(), label.clone())).collect();
    let onchange = change(context, node.id, "add-change", move |kind, key: String| {
        if let Some((_, _, value)) = starts.iter().find(|(each, ..)| *each == key) {
            set_change(kind, &key, Some(value.clone()));
        }
    })
    .reform(|event: Event| util::value_of(&event));
    let id = field_id(node.id, "add-change");
    html! {
        <div class="node-row" {style}>
            <label for={id.clone()} class="visually-hidden">{ "Change a setting for this node" }</label>
            <select {id} class="add-setting" {onchange}
                    title="Change one setting for this node only; the rest stay as set for the whole plugin">
                <option value="" selected=true>{ "+ Change a setting" }</option>
                { for options.iter().map(|(key, label)| html! {
                    <option value={key.clone()}>{ label.clone() }</option>
                }) }
            </select>
        </div>
    }
}

fn set_change(kind: &mut NodeKind, key: &str, value: Option<Value>) {
    if let NodeKind::Plugin { changes, .. } = kind {
        match value {
            Some(value) => changes.insert(key.to_string(), value),
            None => changes.remove(key),
        };
    }
}

fn set_plugin(kind: &mut NodeKind, key: &str, value: Value) {
    if let NodeKind::Plugin { settings, .. } = kind {
        settings.insert(key.to_string(), value);
    }
}

/// A plugin's number setting, kept within its bounds.
fn plugin_number(setting: Option<&PluginSetting>, number: f64) -> Value {
    serde_json::json!(setting.map_or(number, |setting| setting.clamp(number)))
}

fn tier_mut(kind: &mut NodeKind, id: u32) -> Option<&mut Tier> {
    match kind {
        NodeKind::Rarity { tiers, .. } => tiers.iter_mut().find(|tier| tier.id == id),
        _ => None,
    }
}

fn field_id(node: NodeId, field: &str) -> String {
    format!("node-{node}-{field}")
}

/// Changes this node's settings; typing into one field is one undo step.
fn change<T: Clone + 'static>(
    context: &NodeContext,
    node: NodeId,
    field: &str,
    apply: impl Fn(&mut NodeKind, T) + 'static,
) -> Callback<T> {
    let panel = context.panel.clone();
    let card = context.card.id.clone();
    let key = format!("{card}/{node}/{field}");
    let apply = std::rc::Rc::new(apply);
    Callback::from(move |value: T| {
        let apply = apply.clone();
        edit_node(&panel, &card, node, Some(key.clone()), move |kind| apply(kind, value.clone()));
    })
}

fn text_change(
    context: &NodeContext,
    node: NodeId,
    field: &str,
    apply: impl Fn(&mut NodeKind, String) + 'static,
) -> Callback<InputEvent> {
    change(context, node, field, apply).reform(|event: InputEvent| util::value_of(&event))
}

/// The field standing in for an unwired input, and whether it goes under
/// its label (a text area) rather than beside it.
fn input_field(context: &NodeContext, node: &Node, input: &Socket) -> Option<(Html, bool)> {
    let id = field_id(node.id, &input.name);
    let wired = |name: &str| context.card.wire_into(node.id, name).is_some();
    if layout::plugin_input_hidden(&node.kind, &input.name, wired) {
        return None;
    }
    match input.ty {
        SocketType::Emotes if matches!(node.kind, NodeKind::Plugin { .. }) => {
            let (_, codes) = layout::plugin_emotes(&node.kind, &input.name)?;
            let key = input.name.clone();
            let onchange = change(context, node.id, &input.name, move |kind, codes: bool| {
                let value = if codes { Value::Array(Vec::new()) } else { Value::from(RANDOM_EMOTES) };
                set_plugin(kind, &key, value);
            })
            .reform(|event: Event| util::value_of(&event) == "codes");
            let codes = codes.is_some();
            Some((
                html! {
                    <select {id} {onchange}>
                        <option value="random" selected={!codes}>{ "Random" }</option>
                        <option value="codes" selected={codes}>{ "These emotes" }</option>
                    </select>
                },
                false,
            ))
        }
        SocketType::Number => {
            let value = node.kind.number_setting(&input.name)?;
            let name = input.name.clone();
            let onchange =
                change(context, node.id, &input.name, move |kind, value| set_number(kind, &name, value));
            Some((html! { <NumberField id={id} {value} {onchange} /> }, false))
        }
        SocketType::Text => {
            let value = node.kind.text_setting(&input.name)?.to_string();
            let name = input.name.clone();
            let onchange =
                change(context, node.id, &input.name, move |kind, value| set_text(kind, &name, value));
            Some((html! { <TemplateField id={id} title={TEMPLATE_HINT} value={value} {onchange} /> }, true))
        }
        SocketType::Emotes | SocketType::Event => None,
    }
}

fn view_setting(context: &NodeContext, node: &Node, setting: Setting, style: String) -> Html {
    let id = node.id;
    let number = |field: &str, label: &str, value: f64| {
        let name = field.to_string();
        let onchange = change(context, id, field, move |kind, value| set_number(kind, &name, value));
        html! {
            <div class="node-row" style={style.clone()}>
                <label for={field_id(id, field)}>{ label.to_string() }</label>
                <NumberField id={field_id(id, field)} {value} {onchange} />
            </div>
        }
    };
    let check = |field: &str, label: &str, on: bool, apply: fn(&mut NodeKind, bool)| {
        let onchange = change(context, id, field, apply).reform(|event: Event| util::checked_of(&event));
        html! {
            <div class="node-row check" style={style.clone()}>
                <input type="checkbox" id={field_id(id, field)} checked={on} {onchange} />
                <label for={field_id(id, field)}>{ label.to_string() }</label>
            </div>
        }
    };
    // A tick box in a node with fields: under them, as a plugin's are.
    let check_under_fields = |field: &str, label: &str, on: bool, apply: fn(&mut NodeKind, bool)| {
        let onchange = change(context, id, field, apply).reform(|event: Event| util::checked_of(&event));
        html! {
            <div class="node-row check lined" style={style.clone()}>
                <span class="check-in-column">
                    <input type="checkbox" id={field_id(id, field)} checked={on} {onchange} />
                    <label for={field_id(id, field)}>{ label.to_string() }</label>
                </span>
            </div>
        }
    };
    // `emotes`: complete emote codes (asking the server for the channel's
    // emotes when the field gets the focus).
    let list = |field: &str,
                label: &str,
                value: &[String],
                placeholder: &str,
                emotes: bool,
                apply: fn(&mut NodeKind, Vec<String>)| {
        let onchange = change(context, id, field, apply);
        let panel = context.panel.clone();
        let onfocus = Callback::from(move |()| {
            panel.send(PanelRequest::Emotes);
        });
        let completions = emotes.then(|| context.panel.model.emotes.clone());
        html! {
            <div class="node-row" style={style.clone()}>
                <label for={field_id(id, field)} class="visually-hidden">{ label.to_string() }</label>
                <ListField id={field_id(id, field)} value={value.to_vec()} {onchange}
                           placeholder={placeholder.to_string()} emotes={completions}
                           onfocus={if emotes { onfocus } else { Callback::noop() }} />
            </div>
        }
    };
    let text = |field: &str,
                label: &str,
                value: &str,
                placeholder: &str,
                choices: Option<&[String]>,
                apply: fn(&mut NodeKind, String)| {
        let field_html = match choices {
            Some(choices) => html! {
                <PickField id={field_id(id, field)} value={value.to_string()} options={choices.to_vec()}
                           empty={AttrValue::from(placeholder.to_string())}
                           onchange={change(context, id, field, apply)} />
            },
            None => html! {
                <input type="text" id={field_id(id, field)} value={value.to_string()} spellcheck="false"
                       placeholder={placeholder.to_string()} oninput={text_change(context, id, field, apply)} />
            },
        };
        html! {
            <div class="node-row" style={style.clone()}>
                <label for={field_id(id, field)}>{ label.to_string() }</label>
                { field_html }
            </div>
        }
    };

    match (setting, &node.kind) {
        (Setting::Words, NodeKind::ChatWords { words } | NodeKind::Chant { words, .. }) => {
            list("words", "Words", words, "hype, letsgo", false, |kind, value| {
                if let NodeKind::ChatWords { words } | NodeKind::Chant { words, .. } = kind {
                    *words = value;
                }
            })
        }
        (Setting::CrowdViewers, NodeKind::EmoteCombo { viewers, .. } | NodeKind::Chant { viewers, .. }) => {
            number("viewers", "Viewers", *viewers)
        }
        (Setting::CrowdSeconds, NodeKind::EmoteCombo { seconds, .. } | NodeKind::Chant { seconds, .. }) => {
            number("seconds", "Within (s)", *seconds)
        }
        (Setting::BitsMin, NodeKind::Bits { min }) => number("min", "At least", *min),
        (Setting::RaidMin, NodeKind::Raid { min_viewers }) => number("min_viewers", "At least", *min_viewers),
        (Setting::SubNew, NodeKind::Sub { new, .. }) => check("new", "New subs", *new, |kind, on| {
            if let NodeKind::Sub { new, .. } = kind {
                *new = on;
            }
        }),
        (Setting::SubResub, NodeKind::Sub { resub, .. }) => check("resub", "Resubs", *resub, |kind, on| {
            if let NodeKind::Sub { resub, .. } = kind {
                *resub = on;
            }
        }),
        (Setting::SubGifted, NodeKind::Sub { gifted, .. }) => {
            check("gifted", "Gifted subs", *gifted, |kind, on| {
                if let NodeKind::Sub { gifted, .. } = kind {
                    *gifted = on;
                }
            })
        }
        (Setting::SubInGiftBomb, NodeKind::Sub { in_gift_bomb, .. }) => {
            check("in_gift_bomb", "Gifts in a gift bomb", *in_gift_bomb, |kind, on| {
                if let NodeKind::Sub { in_gift_bomb, .. } = kind {
                    *in_gift_bomb = on;
                }
            })
        }
        (Setting::SubPrimeToPaid, NodeKind::Sub { prime_to_paid, .. }) => {
            check("prime_to_paid", "Switched from Prime to paid", *prime_to_paid, |kind, on| {
                if let NodeKind::Sub { prime_to_paid, .. } = kind {
                    *prime_to_paid = on;
                }
            })
        }
        (Setting::SubGiftContinued, NodeKind::Sub { gift_continued, .. }) => {
            check("gift_continued", "Continued a gifted sub", *gift_continued, |kind, on| {
                if let NodeKind::Sub { gift_continued, .. } = kind {
                    *gift_continued = on;
                }
            })
        }
        (Setting::Command, NodeKind::ChatCommand { command }) => {
            text("command", "Starts with", command, "!tts", None, |kind, value| {
                if let NodeKind::ChatCommand { command } = kind {
                    *command = value;
                }
            })
        }
        (Setting::Role(role), NodeKind::ViewerRoles { broadcaster, moderator, vip, subscriber }) => {
            type Set = fn(&mut NodeKind, bool);
            let (label, on, set): (&str, bool, Set) = match role {
                "broadcaster" => ("You (the broadcaster)", *broadcaster, |kind, on| {
                    if let NodeKind::ViewerRoles { broadcaster, .. } = kind {
                        *broadcaster = on;
                    }
                }),
                "moderator" => ("Moderators", *moderator, |kind, on| {
                    if let NodeKind::ViewerRoles { moderator, .. } = kind {
                        *moderator = on;
                    }
                }),
                "vip" => ("VIPs", *vip, |kind, on| {
                    if let NodeKind::ViewerRoles { vip, .. } = kind {
                        *vip = on;
                    }
                }),
                _ => ("Subscribers", *subscriber, |kind, on| {
                    if let NodeKind::ViewerRoles { subscriber, .. } = kind {
                        *subscriber = on;
                    }
                }),
            };
            check(role, label, on, set)
        }
        (Setting::HypeTrain(change), NodeKind::HypeTrain { started, levelled_up, progressed, ended }) => {
            type Set = fn(&mut NodeKind, bool);
            let (label, on, set): (&str, bool, Set) = match change {
                "started" => ("Started", *started, |kind, on| {
                    if let NodeKind::HypeTrain { started, .. } = kind {
                        *started = on;
                    }
                }),
                "levelled_up" => ("Levelled up", *levelled_up, |kind, on| {
                    if let NodeKind::HypeTrain { levelled_up, .. } = kind {
                        *levelled_up = on;
                    }
                }),
                "progressed" => ("Each contribution", *progressed, |kind, on| {
                    if let NodeKind::HypeTrain { progressed, .. } = kind {
                        *progressed = on;
                    }
                }),
                _ => ("Ended", *ended, |kind, on| {
                    if let NodeKind::HypeTrain { ended, .. } = kind {
                        *ended = on;
                    }
                }),
            };
            check(change, label, on, set)
        }
        (Setting::Reward, NodeKind::Reward { reward }) => {
            let rewards = &context.panel.model.status.rewards;
            text("reward", "Reward", reward, "Any reward", Some(rewards), |kind, value| {
                if let NodeKind::Reward { reward } = kind {
                    *reward = value;
                }
            })
        }
        (Setting::CustomName, NodeKind::Custom { name }) => {
            text("name", "Name", name, "Any", None, |kind, value| {
                if let NodeKind::Custom { name } = kind {
                    *name = value;
                }
            })
        }
        (Setting::Percent, NodeKind::Chance { percent }) => number("percent", "Percent", *percent),
        (Setting::Seconds, NodeKind::Cooldown { seconds, .. }) => number("seconds", "Seconds", *seconds),
        (Setting::PerViewer, NodeKind::Cooldown { per_viewer, .. }) => {
            check_under_fields("per_viewer", "For each viewer", *per_viewer, |kind, on| {
                if let NodeKind::Cooldown { per_viewer, .. } = kind {
                    *per_viewer = on;
                }
            })
        }
        (Setting::AddTier, NodeKind::Rarity { .. }) => {
            let panel = context.panel.clone();
            let card = context.card.id.clone();
            // A new tier gets 5 % (or what's left), under the others.
            let onclick = Callback::from(move |_| {
                edit_node(&panel, &card, id, None, |kind| {
                    if let NodeKind::Rarity { tiers, .. } = kind {
                        let next = tiers.iter().map(|tier| tier.id).max().unwrap_or(0) + 1;
                        let left = 100.0 - tiers.iter().map(|tier| tier.percent).sum::<f64>();
                        let percent = left.clamp(0.0, 5.0);
                        tiers.push(Tier { id: next, name: format!("Tier {}", tiers.len() + 1), percent });
                    }
                });
            });
            html! {
                <div class="node-row" {style}>
                    <button type="button" class="button ghost small add-tier" {onclick}>{ icon("plus") }{ "Tier" }</button>
                </div>
            }
        }
        (Setting::GateSeconds, NodeKind::Gate { seconds }) => number("seconds", "Open for (s)", *seconds),
        (Setting::Override(key), kind) => view_override(context, node, key, kind, style),
        (Setting::Plugin(index), _) => view_plugin_setting(context, node, index, style),
        (Setting::PluginPick(index, pick), _) => view_plugin_pick(context, node, index, pick, style),
        (Setting::PluginChange(index), _) => view_plugin_change(context, node, index, style),
        (Setting::AddPluginChange, _) => view_add_plugin_change(context, node, style),
        (Setting::AddOverride, kind) => view_add_override(context, node, kind, style),
        (Setting::NumberValue, NodeKind::Number { value }) => number("value", "Value", *value),
        (Setting::MathOp, NodeKind::Math { op, .. }) => {
            let onchange = change(context, id, "op", |kind, value: String| {
                if let NodeKind::Math { op, .. } = kind
                    && let Some(chosen) = MathOp::ALL.into_iter().find(|op| op.title() == value)
                {
                    *op = chosen;
                }
            })
            .reform(|event: Event| util::value_of(&event));
            html! {
                <div class="node-row" {style}>
                    <select aria-label="Operation" {onchange}>
                        { for MathOp::ALL.into_iter().map(|each| html! {
                            <option value={each.title()} selected={each == *op}>{ each.title() }</option>
                        }) }
                    </select>
                </div>
            }
        }
        (Setting::TextTest, NodeKind::CompareText { test, .. }) => {
            let onchange = change(context, id, "test", |kind, value: String| {
                if let NodeKind::CompareText { test, .. } = kind
                    && let Some(chosen) = TextTest::ALL.into_iter().find(|test| test.title() == value)
                {
                    *test = chosen;
                }
            })
            .reform(|event: Event| util::value_of(&event));
            html! {
                <div class="node-row" {style}>
                    <select aria-label="How to compare" {onchange}>
                        { for TextTest::ALL.into_iter().map(|each| html! {
                            <option value={each.title()} selected={each == *test}>{ each.title() }</option>
                        }) }
                    </select>
                </div>
            }
        }
        (Setting::NeedsAll, NodeKind::CompareText { all, .. }) => {
            let onchange = change(context, id, "all", |kind, value: String| {
                if let NodeKind::CompareText { all, .. } = kind {
                    *all = value == "all";
                }
            })
            .reform(|event: Event| util::value_of(&event));
            html! {
                <div class="node-row" {style}>
                    <select aria-label="Which of the texts" {onchange}>
                        <option value="any" selected={!*all}>{ "Any of them" }</option>
                        <option value="all" selected={*all}>{ "All of them" }</option>
                    </select>
                </div>
            }
        }
        (Setting::CompareTo(index), NodeKind::CompareText { to, .. }) => {
            let Some(value) = to.get(index).cloned() else { return html! {} };
            let field = format!("to-{index}");
            let oninput = text_change(context, id, &field, move |kind, value| {
                if let NodeKind::CompareText { to, .. } = kind
                    && let Some(text) = to.get_mut(index)
                {
                    *text = value;
                }
            });
            let remove = {
                let panel = context.panel.clone();
                let card = context.card.id.clone();
                Callback::from(move |_: MouseEvent| {
                    edit_node(&panel, &card, id, None, move |kind| {
                        if let NodeKind::CompareText { to, .. } = kind
                            && index < to.len()
                        {
                            to.remove(index);
                        }
                    })
                })
            };
            html! {
                <div class="node-row override pick" {style}>
                    <label for={field_id(id, &field)} class="visually-hidden">{ format!("To {}", index + 1) }</label>
                    <input type="text" id={field_id(id, &field)} spellcheck="false" placeholder="Gaming"
                           {value} {oninput} />
                    <button type="button" class="tier-remove" title="Take it out"
                            aria-label={format!("Take out To {}", index + 1)} onclick={remove}>{ "×" }</button>
                </div>
            }
        }
        (Setting::AddCompareTo, NodeKind::CompareText { .. }) => {
            let panel = context.panel.clone();
            let card = context.card.id.clone();
            let onclick = Callback::from(move |_| {
                edit_node(&panel, &card, id, None, |kind| {
                    if let NodeKind::CompareText { to, .. } = kind {
                        to.push(String::new());
                    }
                });
            });
            html! {
                <div class="node-row" {style}>
                    <button type="button" class="button ghost small add-text" {onclick}>{ icon("plus") }{ "Text" }</button>
                </div>
            }
        }
        (Setting::NumberTest, NodeKind::CompareNumber { test, .. }) => {
            let onchange = change(context, id, "test", |kind, value: String| {
                if let NodeKind::CompareNumber { test, .. } = kind
                    && let Some(chosen) = NumberTest::ALL.into_iter().find(|test| test.title() == value)
                {
                    *test = chosen;
                }
            })
            .reform(|event: Event| util::value_of(&event));
            html! {
                <div class="node-row" {style}>
                    <select aria-label="How to compare" {onchange}>
                        { for NumberTest::ALL.into_iter().map(|each| html! {
                            <option value={each.title()} selected={each == *test}>{ each.title() }</option>
                        }) }
                    </select>
                </div>
            }
        }
        (Setting::Every, NodeKind::EveryNth { every }) => number("every", "Every", *every),
        (Setting::TimerMinutes, NodeKind::Timer { minutes, .. }) => number("minutes", "Minutes", *minutes),
        (Setting::FromStart, NodeKind::Timer { from_start, .. }) => {
            check_under_fields("from_start", "Running from the start", *from_start, |kind, on| {
                if let NodeKind::Timer { from_start, .. } = kind {
                    *from_start = on;
                }
            })
        }
        (Setting::IgnoreCase, NodeKind::CompareText { ignore_case, .. }) => {
            check("ignore_case", "Ignore case", *ignore_case, |kind, on| {
                if let NodeKind::CompareText { ignore_case, .. } = kind {
                    *ignore_case = on;
                }
            })
        }
        (Setting::Template, NodeKind::Text { template, size }) => {
            let onchange = change(context, id, "template", |kind, value: String| {
                if let NodeKind::Text { template, .. } = kind {
                    *template = value;
                }
            });
            let box_style = format!("height: {}px", layout::text_size(*size).1);
            html! {
                <div class="node-row column" {style}>
                    <TemplateField class="tall" aria_label="Text, with {placeholders}" title={TEMPLATE_HINT}
                                   style={box_style} value={template.clone()} {onchange} />
                </div>
            }
        }
        (Setting::PluginCodes(index), NodeKind::Plugin { ty: Attached(Some(ty)), .. }) => {
            let Some(setting) = ty.settings.get(index) else { return html! {} };
            let Some((_, Some(codes))) = layout::plugin_emotes(&node.kind, &setting.key) else {
                return html! {};
            };
            let key = setting.key.clone();
            let field = format!("plugin-{key}-codes");
            let onchange = change(context, id, &field, move |kind, codes: Vec<String>| {
                set_plugin(kind, &key, Value::from(codes));
            });
            let panel = context.panel.clone();
            let onfocus = Callback::from(move |()| {
                panel.send(PanelRequest::Emotes);
            });
            html! {
                <div class="node-row" {style}>
                    <label for={field_id(id, &field)} class="visually-hidden">{ format!("{}: emote codes", setting.label) }</label>
                    <ListField id={field_id(id, &field)} value={codes} {onchange} placeholder="Kappa, PogU"
                               emotes={Some(context.panel.model.emotes.clone())} {onfocus} />
                </div>
            }
        }
        _ => html! {},
    }
}

/// Sets a number setting, or the setting standing in for a number input.
fn set_number(kind: &mut NodeKind, name: &str, number: f64) {
    match (kind, name) {
        (NodeKind::Bits { min }, "min") => *min = number,
        (NodeKind::Raid { min_viewers }, "min_viewers") => *min_viewers = number,
        (NodeKind::Chance { percent }, "percent") => *percent = number,
        (
            NodeKind::Cooldown { seconds, .. }
            | NodeKind::Gate { seconds }
            | NodeKind::Delay { seconds }
            | NodeKind::EmoteCombo { seconds, .. }
            | NodeKind::Chant { seconds, .. },
            "seconds",
        ) => *seconds = number,
        (NodeKind::EmoteCombo { viewers, .. } | NodeKind::Chant { viewers, .. }, "viewers") => {
            *viewers = number
        }
        (NodeKind::Number { value }, "value") => *value = number,
        (NodeKind::Math { a, .. }, "a") => *a = number,
        (NodeKind::Math { b, .. }, "b") => *b = number,
        (NodeKind::Clamp { value, .. } | NodeKind::CompareNumber { value, .. }, "value") => *value = number,
        (NodeKind::CompareNumber { number: than, .. }, "number") => *than = number,
        (NodeKind::CompareNumber { and, .. }, "and") => *and = number,
        (NodeKind::EveryNth { every }, "every") => *every = number,
        (NodeKind::Timer { minutes, .. }, "minutes") => *minutes = number,
        (NodeKind::Clamp { min, .. } | NodeKind::Random { min, .. }, "min") => *min = number,
        (NodeKind::Clamp { max, .. } | NodeKind::Random { max, .. }, "max") => *max = number,
        (NodeKind::Plugin { settings, ty, .. }, name) => {
            let setting = ty.0.as_ref().and_then(|ty| ty.setting(name));
            settings.insert(name.to_string(), plugin_number(setting, number));
        }
        _ => {}
    }
}

/// Sets the setting standing in for a text input.
fn set_text(kind: &mut NodeKind, name: &str, text: String) {
    match (kind, name) {
        (NodeKind::HasText { text: said } | NodeKind::CompareText { text: said, .. }, "text") => *said = text,
        (NodeKind::Plugin { settings, .. }, name) => {
            settings.insert(name.to_string(), Value::String(text));
        }
        _ => {}
    }
}
