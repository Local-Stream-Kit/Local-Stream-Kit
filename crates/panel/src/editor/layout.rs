//! Node geometry. Every row of a node has a fixed height, so socket positions
//! (and so the wires) are computed here rather than measured in the page.

use protocol::cards::{Card, Category, Node, NodeKind, Socket, SocketType};
use protocol::plugin::{Setting as PluginSetting, SettingKind};

pub const HEADER: f64 = 30.0;
const PAD_TOP: f64 = 6.0;
const PAD_BOTTOM: f64 = 8.0;
const ROW: f64 = 26.0;
const FIELD_ROW: f64 = 32.0;
/// "+ Change a setting": a line of text, not a field.
const ADD_ROW: f64 = 24.0;
const TEXT_AREA_ROW: f64 = 62.0;
/// A text box's row is this much taller than the box.
const TEXT_BOX_PAD: f64 = 10.0;
/// A Text node's width and text box height, as it opens and at the least,
/// and at the most.
const TEXT_NODE_SIZE: (f64, f64) = (250.0, TEXT_AREA_ROW - TEXT_BOX_PAD);
const TEXT_NODE_MAX: (f64, f64) = (900.0, 900.0);

/// A node setting that isn't an input socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Words,
    BitsMin,
    SubNew,
    SubResub,
    SubGifted,
    SubInGiftBomb,
    SubPrimeToPaid,
    SubGiftContinued,
    Reward,
    CustomName,
    Command,
    /// One of a Viewer roles node's roles, by its field name.
    Role(&'static str),
    /// What a Hype train node fires for, by its field name.
    HypeTrain(&'static str),
    Percent,
    Seconds,
    PerViewer,
    NumberValue,
    MathOp,
    /// A Compare text node's test, its any / all, and Ignore case.
    TextTest,
    NeedsAll,
    IgnoreCase,
    /// One of a Compare text node's texts (under its To, while that isn't
    /// wired), by its index; and "+ Text".
    CompareTo(usize),
    AddCompareTo,
    /// A Compare number node's test.
    NumberTest,
    /// An Every Nth node's Every.
    Every,
    /// A Timer's minutes, and whether it runs from the start.
    TimerMinutes,
    FromStart,
    Template,
    RaidMin,
    CrowdViewers,
    CrowdSeconds,
    AddTier,
    GateSeconds,
    /// A plugin node's changed voice setting, by its key
    /// (`protocol::overrides::SPEECH_FIELDS`).
    Override(&'static str),
    AddOverride,
    /// A plugin node's setting with a row of its own, by its index in the
    /// node type's settings (for one with several values, its "Add…").
    Plugin(usize),
    /// One of the values of a plugin node's setting with several (`many`),
    /// under it: the setting's index, the value's.
    PluginPick(usize, usize),
    /// A plugin node's emotes setting's codes (with codes chosen), under
    /// its input, by the setting's index.
    PluginCodes(usize),
    /// One of its plugin's own settings a plugin node changes, by its index
    /// in the node type's `changeable`.
    PluginChange(usize),
    /// "+ Change a setting" for a plugin node.
    AddPluginChange,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    Output(Socket),
    /// `wired`: something is connected, so no field of its own.
    Input {
        socket: Socket,
        wired: bool,
    },
    /// An input with no field and an output, sharing a row (2026-10-15).
    Pair {
        input: Socket,
        output: Socket,
    },
    Setting(Setting),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub kind: RowKind,
    pub height: f64,
}

/// A Text node's width and text box height: as resized, within bounds.
pub fn text_size(size: Option<[f64; 2]>) -> (f64, f64) {
    let [width, height] = size.unwrap_or([TEXT_NODE_SIZE.0, TEXT_NODE_SIZE.1]);
    let fit = |value: f64, min: f64, max: f64| if value.is_finite() { value.clamp(min, max) } else { min };
    (fit(width, TEXT_NODE_SIZE.0, TEXT_NODE_MAX.0), fit(height, TEXT_NODE_SIZE.1, TEXT_NODE_MAX.1))
}

pub fn width(kind: &NodeKind) -> f64 {
    match kind {
        NodeKind::Text { size, .. } => text_size(*size).0,
        NodeKind::Rarity { .. } => 250.0,
        // A plugin's node: its settings can be of any kind.
        NodeKind::Plugin { .. } => plugin_width(kind),
        // Their tick box sits under their field: room for its words.
        NodeKind::Cooldown { .. } => with_tick(190.0, "Seconds", "For each viewer"),
        NodeKind::Timer { .. } => with_tick(190.0, "Minutes", "Running from the start"),
        _ => match kind.category() {
            Category::Events | Category::Effects => 230.0,
            Category::Flow | Category::Values => 190.0,
        },
    }
}

/// About how wide a label is in a node (12.5 px text).
fn label_width(label: &str) -> f64 {
    label.chars().count() as f64 * 6.8
}

/// A node `usual` wide, or wider so a tick box's words fit under its field
/// (`label`'s).
fn with_tick(usual: f64, label: &str, tick: &str) -> f64 {
    let needed = 28.0 + label_width(label) + 8.0 + label_width(tick) + 22.0;
    ((needed / 10.0).ceil() * 10.0).max(usual)
}

/// A plugin's node: 230 px, or wider when its longest label would leave its
/// fields less than about 140 px (or a tick box's words wouldn't fit under
/// them), since the labels share a column (2026-10-15).
fn plugin_width(kind: &NodeKind) -> f64 {
    let (Some(ty), NodeKind::Plugin { changes, .. }) = (kind.plugin_type(), kind) else { return 230.0 };
    let label = |setting: &PluginSetting| match &setting.kind {
        SettingKind::Number { unit, .. } => crate::views::plugins::with_unit(&setting.label, *unit),
        _ => setting.label.clone(),
    };
    let is_tick = |setting: &&PluginSetting| matches!(setting.kind, SettingKind::Bool { .. });
    // The settings it shows: its own, and those it changes for itself.
    let changed = ty.changeable.iter().filter(|setting| changes.contains_key(&setting.key));
    let settings = ty.settings.iter().chain(changed);
    let longest = settings
        .clone()
        .filter(|setting| !is_tick(setting))
        .map(|setting| label_width(&label(setting)))
        .chain(
            kind.inputs()
                .iter()
                .filter(|input| input.ty != SocketType::Event)
                .map(|input| label_width(&input.label)),
        )
        .fold(0.0, f64::max);
    let ticks =
        settings.filter(is_tick).map(|setting| label_width(&setting.label) + 22.0).fold(0.0, f64::max);
    // The rows' sides (14 px each), the gap, the fields.
    let needed = 28.0 + longest + 8.0 + ticks.max(140.0);
    ((needed / 10.0).ceil() * 10.0).max(230.0)
}

/// The node's rows top to bottom: outputs, then its event inputs (where the
/// event's wire comes in, 2026-10-15; a rarity's above its tiers),
/// settings, the other inputs (as in Blender, the rain's emote settings
/// under its Emotes input), then a plugin's "Wait in the queue" and its
/// style picker by the settings it changes. Outputs and the inputs with no
/// field right under them share rows while both fit.
pub fn rows(node: &Node, card: &Card) -> Vec<Row> {
    let kind = &node.kind;
    let wired = |name: &str| card.wire_into(node.id, name).is_some();
    // A rarity's outputs are its tiers, each with its name and chance.
    let rarity = matches!(kind, NodeKind::Rarity { .. });
    let output_height = if rarity { FIELD_ROW } else { ROW };
    let outputs: Vec<Row> = kind
        .outputs()
        .into_iter()
        .map(|socket| Row { kind: RowKind::Output(socket), height: output_height })
        .collect();
    let setting = |setting, height| Row { kind: RowKind::Setting(setting), height };
    let (events, inputs): (Vec<Socket>, Vec<Socket>) =
        kind.inputs().into_iter().partition(|socket| socket.ty == SocketType::Event);
    let events: Vec<Row> = events.into_iter().flat_map(|socket| input_rows(kind, socket, wired)).collect();
    // A rarity's event comes first, above its tiers (asked, 2026-10-15).
    let mut rows = if rarity { [events, outputs].concat() } else { [outputs, events].concat() };
    match kind {
        NodeKind::ChatWords { .. } => rows.push(setting(Setting::Words, FIELD_ROW)),
        NodeKind::ChatCommand { .. } => rows.push(setting(Setting::Command, FIELD_ROW)),
        NodeKind::ViewerRoles { .. } => {
            for role in ["broadcaster", "moderator", "vip", "subscriber"] {
                rows.push(setting(Setting::Role(role), 24.0));
            }
        }
        NodeKind::EmoteCombo { .. } | NodeKind::Chant { .. } => {
            if matches!(kind, NodeKind::Chant { .. }) {
                rows.push(setting(Setting::Words, FIELD_ROW));
            }
            rows.push(setting(Setting::CrowdViewers, FIELD_ROW));
            rows.push(setting(Setting::CrowdSeconds, FIELD_ROW));
        }
        NodeKind::Bits { .. } => rows.push(setting(Setting::BitsMin, FIELD_ROW)),
        NodeKind::Raid { .. } => rows.push(setting(Setting::RaidMin, FIELD_ROW)),
        NodeKind::Sub { .. } => {
            for sub in [
                Setting::SubNew,
                Setting::SubResub,
                Setting::SubGifted,
                Setting::SubInGiftBomb,
                Setting::SubPrimeToPaid,
                Setting::SubGiftContinued,
            ] {
                rows.push(setting(sub, 24.0));
            }
        }
        NodeKind::HypeTrain { .. } => {
            for change in ["started", "levelled_up", "progressed", "ended"] {
                rows.push(setting(Setting::HypeTrain(change), 24.0));
            }
        }
        NodeKind::Reward { .. } => rows.push(setting(Setting::Reward, FIELD_ROW)),
        NodeKind::Custom { .. } => rows.push(setting(Setting::CustomName, FIELD_ROW)),
        NodeKind::Chance { .. } => rows.push(setting(Setting::Percent, FIELD_ROW)),
        NodeKind::Rarity { .. } => rows.push(setting(Setting::AddTier, FIELD_ROW)),
        NodeKind::Gate { .. } => rows.push(setting(Setting::GateSeconds, FIELD_ROW)),
        NodeKind::Cooldown { .. } => {
            rows.push(setting(Setting::Seconds, FIELD_ROW));
            rows.push(setting(Setting::PerViewer, 24.0));
        }
        NodeKind::Number { .. } => rows.push(setting(Setting::NumberValue, FIELD_ROW)),
        NodeKind::Math { .. } => rows.push(setting(Setting::MathOp, FIELD_ROW)),
        NodeKind::CompareText { .. } => {
            rows.push(setting(Setting::TextTest, FIELD_ROW));
            if !wired("to") {
                rows.push(setting(Setting::NeedsAll, FIELD_ROW));
            }
            rows.push(setting(Setting::IgnoreCase, 24.0));
        }
        NodeKind::CompareNumber { .. } => rows.push(setting(Setting::NumberTest, FIELD_ROW)),
        NodeKind::EveryNth { .. } => rows.push(setting(Setting::Every, FIELD_ROW)),
        NodeKind::Timer { .. } => {
            rows.push(setting(Setting::TimerMinutes, FIELD_ROW));
            rows.push(setting(Setting::FromStart, 24.0));
        }
        NodeKind::Text { size, .. } => {
            rows.push(setting(Setting::Template, text_size(*size).1 + TEXT_BOX_PAD))
        }
        NodeKind::Plugin { .. } => {
            // Settings shown only while an input is or isn't wired go under
            // that input instead; "Wait in the queue" and the style picker
            // go last.
            for (index, each) in plugin_settings(kind, |each| each.when.is_none() && !goes_last(each), wired)
            {
                rows.push(setting(Setting::Plugin(index), plugin_setting_height(each)));
                for pick in 0..plugin_picks(kind, each) {
                    rows.push(setting(Setting::PluginPick(index, pick), FIELD_ROW));
                }
            }
        }
        _ => {}
    }
    for socket in inputs {
        rows.extend(input_rows(kind, socket, wired));
    }
    // A plugin's "Wait in the queue", under all its fields (asked,
    // 2026-10-15), then its style picker, with the settings it changes for
    // this node.
    for (index, each) in plugin_settings(kind, |each| each.when.is_none() && is_queue(each), wired) {
        rows.push(setting(Setting::Plugin(index), plugin_setting_height(each)));
    }
    for (index, _) in plugin_settings(kind, |each| each.when.is_none() && is_preset(each), wired) {
        rows.push(setting(Setting::Plugin(index), FIELD_ROW));
    }
    // Effects: the settings this node changes, last.
    if let (NodeKind::Plugin { changes, .. }, Some(ty)) = (kind, kind.plugin_type())
        && !ty.changeable.is_empty()
    {
        for (index, _) in ty.changeable.iter().enumerate().filter(|(_, each)| changes.contains_key(&each.key))
        {
            rows.push(setting(Setting::PluginChange(index), FIELD_ROW));
        }
        if ty.changeable.iter().any(|each| !changes.contains_key(&each.key)) {
            rows.push(setting(Setting::AddPluginChange, ADD_ROW));
        }
    }
    if let Some((overrides, fields)) = kind.overrides() {
        for field in fields.iter().filter(|field| overrides.contains_key(field.key)) {
            rows.push(setting(Setting::Override(field.key), FIELD_ROW));
        }
        rows.push(setting(Setting::AddOverride, ADD_ROW));
    }
    pair(rows, width(kind))
}

/// An input's row, and the rows that go under it: a Compare text node's
/// texts, a plugin's emote codes and the settings shown while it is (or
/// isn't) wired.
fn input_rows(kind: &NodeKind, socket: Socket, wired: impl Fn(&str) -> bool + Copy) -> Vec<Row> {
    let setting = |setting, height| Row { kind: RowKind::Setting(setting), height };
    let mut rows = Vec::new();
    let is_wired = wired(&socket.name);
    let height = match socket.ty {
        SocketType::Event => ROW,
        _ if is_wired => ROW,
        SocketType::Text if kind.text_setting(&socket.name).is_some() => TEXT_AREA_ROW,
        SocketType::Text => ROW,
        // A plugin's setting standing in for it, not shown now.
        _ if plugin_input_hidden(kind, &socket.name, wired) => ROW,
        // Inputs with no setting standing in for them (a plugin's) have
        // no field.
        SocketType::Number if kind.number_setting(&socket.name).is_none() => ROW,
        SocketType::Emotes if plugin_emotes(kind, &socket.name).is_none() => ROW,
        SocketType::Number | SocketType::Emotes => FIELD_ROW,
    };
    let name = socket.name.clone();
    rows.push(Row { kind: RowKind::Input { socket, wired: is_wired }, height });
    // A Compare text node's texts go under its To, a row each.
    if let NodeKind::CompareText { to, .. } = kind
        && name == "to"
        && !is_wired
    {
        for index in 0..to.len() {
            rows.push(setting(Setting::CompareTo(index), FIELD_ROW));
        }
        rows.push(setting(Setting::AddCompareTo, FIELD_ROW));
    }
    // A plugin's settings about this input go right under it: an
    // emotes setting's codes, and the settings shown while it is (or
    // isn't) wired.
    if !is_wired
        && let Some((index, codes)) = plugin_emotes(kind, &name)
        && codes.is_some()
    {
        rows.push(setting(Setting::PluginCodes(index), FIELD_ROW));
    }
    let about = |each: &PluginSetting| each.when.as_ref().is_some_and(|when| when.input() == name);
    for (index, each) in plugin_settings(kind, about, wired) {
        rows.push(setting(Setting::Plugin(index), plugin_setting_height(each)));
    }
    rows
}

/// A plugin setting picking one of its plugin's presets (an alert's style).
fn is_preset(setting: &PluginSetting) -> bool {
    matches!(setting.kind, SettingKind::Preset {})
}

/// A plugin effect's "Wait in the queue" tick box.
fn is_queue(setting: &PluginSetting) -> bool {
    setting.key == protocol::plugin::QUEUE_SETTING && matches!(setting.kind, SettingKind::Bool { .. })
}

/// The plugin settings shown after the node's inputs.
fn goes_last(setting: &PluginSetting) -> bool {
    is_preset(setting) || is_queue(setting)
}

/// Outputs and the inputs right under them that have no field share rows,
/// top down, while both are plain labels that fit side by side.
fn pair(rows: Vec<Row>, width: f64) -> Vec<Row> {
    let outputs = rows.iter().take_while(|row| matches!(row.kind, RowKind::Output(_))).count();
    // About a label's width at 12.5 px, the row's sides and the gap.
    let fits = |a: &Socket, b: &Socket| {
        (a.label.chars().count() + b.label.chars().count()) as f64 * 7.0 + 44.0 <= width
    };
    let mut pairs = 0;
    while pairs < outputs {
        let (Some(out), Some(into)) = (rows.get(pairs), rows.get(outputs + pairs)) else { break };
        let (RowKind::Output(output), RowKind::Input { socket: input, .. }) = (&out.kind, &into.kind) else {
            break;
        };
        if out.height != ROW || into.height != ROW || !fits(input, output) {
            break;
        }
        pairs += 1;
    }
    if pairs == 0 {
        return rows;
    }
    let mut shared: Vec<Row> = Vec::new();
    for n in 0..pairs {
        if let (RowKind::Output(output), RowKind::Input { socket: input, .. }) =
            (&rows[n].kind, &rows[outputs + n].kind)
        {
            shared.push(Row {
                kind: RowKind::Pair { input: input.clone(), output: output.clone() },
                height: ROW,
            });
        }
    }
    shared.extend(rows[pairs..outputs].iter().cloned());
    shared.extend(rows[outputs + pairs..].iter().cloned());
    shared
}

/// A plugin node's own settings (not standing in for an input) that pass
/// `pick` and are shown with these inputs wired, by index.
fn plugin_settings(
    kind: &NodeKind,
    pick: impl Fn(&PluginSetting) -> bool,
    wired: impl Fn(&str) -> bool + Copy,
) -> Vec<(usize, &PluginSetting)> {
    let Some(ty) = kind.plugin_type() else { return Vec::new() };
    ty.settings
        .iter()
        .enumerate()
        .filter(|(_, each)| ty.own_settings().any(|own| own.key == each.key))
        .filter(|(_, each)| pick(each) && ty.shown(each, wired))
        .collect()
}

/// How many values a plugin node's setting with several (`many`) has.
pub fn plugin_picks(kind: &NodeKind, setting: &PluginSetting) -> usize {
    let NodeKind::Plugin { settings, .. } = kind else { return 0 };
    if !matches!(setting.kind, SettingKind::Text { many: true, .. }) {
        return 0;
    }
    setting.read(settings.get(&setting.key)).as_array().map_or(0, Vec::len)
}

fn plugin_setting_height(setting: &PluginSetting) -> f64 {
    if matches!(setting.kind, SettingKind::Bool { .. }) { 24.0 } else { FIELD_ROW }
}

/// Whether a plugin node's setting standing in for this input isn't shown
/// now (so the input has no field).
pub fn plugin_input_hidden(kind: &NodeKind, input: &str, wired: impl Fn(&str) -> bool) -> bool {
    let Some(ty) = kind.plugin_type() else { return false };
    ty.setting(input).is_some_and(|setting| !ty.shown(setting, wired))
}

/// A plugin node's emotes setting standing in for this input: its index,
/// and its codes (`None` for random emotes).
pub fn plugin_emotes(kind: &NodeKind, input: &str) -> Option<(usize, Option<Vec<String>>)> {
    let NodeKind::Plugin { settings, .. } = kind else { return None };
    let ty = kind.plugin_type()?;
    let setting = ty.input_setting(input, SocketType::Emotes)?;
    let index = ty.settings.iter().position(|each| each.key == setting.key)?;
    let codes = setting
        .read(settings.get(input))
        .as_array()
        .map(|codes| codes.iter().filter_map(|code| code.as_str().map(str::to_string)).collect());
    Some((index, codes))
}

pub fn height(rows: &[Row]) -> f64 {
    HEADER + PAD_TOP + rows.iter().map(|row| row.height).sum::<f64>() + PAD_BOTTOM
}

/// Where a socket is on the canvas: `(x, y)` of its centre.
pub fn socket_position(node: &Node, card: &Card, socket: &str, input: bool) -> Option<(f64, f64)> {
    let mut y = HEADER + PAD_TOP;
    for row in rows(node, card) {
        let hit = match &row.kind {
            RowKind::Output(out) => !input && out.name == socket,
            RowKind::Input { socket: into, .. } => input && into.name == socket,
            RowKind::Pair { input: into, output } => {
                if input {
                    into.name == socket
                } else {
                    output.name == socket
                }
            }
            RowKind::Setting(_) => false,
        };
        if hit {
            let x = if input { node.x } else { node.x + width(&node.kind) };
            return Some((x, node.y + y + row.height / 2.0));
        }
        y += row.height;
    }
    None
}

/// The rectangle all nodes fit in: `(left, top, right, bottom)`.
pub fn bounds(card: &Card) -> Option<(f64, f64, f64, f64)> {
    card.nodes.iter().fold(None, |bounds, node| {
        let (w, h) = (width(&node.kind), height(&rows(node, card)));
        let (l, t, r, b) = (node.x, node.y, node.x + w, node.y + h);
        Some(match bounds {
            None => (l, t, r, b),
            Some((bl, bt, br, bb)) => (bl.min(l), bt.min(t), br.max(r), bb.max(b)),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockets_line_up_with_rows() {
        let card = crate::util::premade_cards().into_iter().find(|card| card.id == "hydrate").unwrap();
        let reward = card.node(1).unwrap();
        // Outputs come first: event is the first row.
        assert_eq!(
            socket_position(reward, &card, "event", false),
            Some((reward.x + 230.0, reward.y + 30.0 + 6.0 + 13.0))
        );
        let alert = card.node(3).unwrap();
        // Alert: Trigger and Started on one row, Ended (26 each), then
        // its message (an unwired textarea, 62); its style comes after.
        // 260 wide: "Duration (s)" and a 140 px field.
        assert_eq!(
            socket_position(alert, &card, "started", false),
            Some((alert.x + 260.0, alert.y + 36.0 + 13.0))
        );
        assert_eq!(socket_position(alert, &card, "trigger", true), Some((alert.x, alert.y + 36.0 + 13.0)));
        assert_eq!(
            socket_position(alert, &card, "message", true),
            Some((alert.x, alert.y + 36.0 + 52.0 + 31.0))
        );
        assert!(socket_position(alert, &card, "nope", true).is_none());
    }

    /// The node's rows, by their sockets' and settings' names.
    fn row_names(node: &Node, card: &Card) -> Vec<String> {
        rows(node, card)
            .iter()
            .map(|row| match &row.kind {
                RowKind::Output(socket) => format!("out {}", socket.name),
                RowKind::Input { socket, .. } => format!("in {}", socket.name),
                RowKind::Pair { input, output } => format!("{} | {}", input.name, output.name),
                RowKind::Setting(Setting::Plugin(index)) => {
                    format!("set {}", node.kind.plugin_type().unwrap().settings[*index].key)
                }
                RowKind::Setting(setting) => format!("{setting:?}"),
            })
            .collect()
    }

    /// A card with just `kind`, as node 1.
    fn alone(kind: NodeKind) -> (Node, Card) {
        let node = Node { id: 1, x: 0.0, y: 0.0, kind };
        let card = Card {
            id: "c".into(),
            name: "C".into(),
            enabled: true,
            nodes: vec![node.clone()],
            wires: Vec::new(),
        };
        (node, card)
    }

    #[test]
    fn the_trigger_comes_first_and_shares_a_row_with_an_output() {
        let card = crate::util::premade_cards().into_iter().find(|card| card.id == "hydrate").unwrap();
        let alert = card.node(3).unwrap();
        let names: Vec<String> = row_names(alert, &card).into_iter().take(5).collect();
        assert_eq!(
            names,
            ["trigger | started", "out ended", "in message", "in duration", "in read"],
            "an unwired message has a field: it doesn't share Ended's row"
        );
        assert!(names.iter().all(|name| name != "set style"), "the style picker comes after the inputs");
        // A Compare number node: Event next to Matches, Miss on its own.
        let compare = Node {
            id: 1,
            x: 0.0,
            y: 0.0,
            kind: NodeKind::CompareNumber { test: Default::default(), value: 0.0, number: 1.0, and: 2.0 },
        };
        let card = Card {
            id: "c".into(),
            name: "C".into(),
            enabled: true,
            nodes: vec![compare.clone()],
            wires: Vec::new(),
        };
        assert!(matches!(&rows(&compare, &card)[0].kind,
            RowKind::Pair { input, output } if input.name == "event" && output.name == "event"));
    }

    #[test]
    fn a_node_is_as_wide_as_its_labels_need() {
        let kinds = crate::util::built_in_plugins().node_kinds();
        let width_of = |title: &str| width(kinds.iter().find(|kind| kind.title() == title).unwrap());
        assert_eq!(width_of("Emote rain"), 230.0, "short labels: as before");
        // "Duration (s)" and a 140 px field; its changeable settings
        // count once changed.
        assert_eq!(width_of("Alert"), 260.0);
        // "Size (px)" and a 140 px field.
        assert_eq!(width_of("Particles"), 240.0);
        // Built-in nodes with a tick box under their field.
        assert_eq!(width(&NodeKind::Cooldown { seconds: 30.0, per_viewer: false }), 210.0);
        assert_eq!(width(&NodeKind::Timer { minutes: 15.0, from_start: true }), 260.0);
        // "Seconds (0 = auto)" leaves its fields about 140 px.
        assert_eq!(width_of("Show media"), 300.0);
    }

    #[test]
    fn a_raritys_event_comes_before_its_tiers() {
        let kind =
            NodeKind::catalog().into_iter().find(|kind| matches!(kind, NodeKind::Rarity { .. })).unwrap();
        let (rarity, card) = alone(kind);
        let names = row_names(&rarity, &card);
        assert_eq!(names[0], "in event");
        assert!(names[1].starts_with("out "), "{names:?}");
        assert_eq!(names.last().map(String::as_str), Some("AddTier"));
    }

    #[test]
    fn wait_in_the_queue_comes_under_the_fields() {
        let kinds = crate::util::built_in_plugins().node_kinds();
        for title in ["Sound", "Speak", "Show media"] {
            let kind = kinds.iter().find(|kind| kind.title() == title).unwrap().clone();
            let (node, card) = alone(kind);
            let names = row_names(&node, &card);
            let queue = names.iter().position(|name| name == "set queue").expect("a queue tick box");
            let last_input = names.iter().rposition(|name| name.starts_with("in ")).unwrap();
            assert!(queue > last_input, "{title}: {names:?}");
        }
    }

    #[test]
    fn a_gates_event_shares_its_outputs_row() {
        let (gate, card) = alone(NodeKind::Gate { seconds: 60.0 });
        assert_eq!(row_names(&gate, &card)[..2], ["event | event", "in open"]);
    }

    #[test]
    fn premade_cards_and_presets_have_no_nodes_on_top_of_each_other() {
        let mut overlapping = Vec::new();
        for card in crate::util::premade_cards().into_iter().chain(crate::util::preset_cards()) {
            let boxes: Vec<(u32, f64, f64, f64, f64)> = card
                .nodes
                .iter()
                .map(|node| {
                    let (w, h) = (width(&node.kind), height(&rows(node, &card)));
                    (node.id, node.x, node.y, node.x + w, node.y + h)
                })
                .collect();
            for (index, a) in boxes.iter().enumerate() {
                for b in &boxes[index + 1..] {
                    if !(a.3 <= b.1 || b.3 <= a.1 || a.4 <= b.2 || b.4 <= a.2) {
                        overlapping.push(format!("{}: {a:?} and {b:?}", card.name));
                    }
                }
            }
        }
        assert!(
            overlapping.is_empty(),
            "nodes on top of each other (id, left, top, right, bottom):
{}",
            overlapping.join(
                "
"
            )
        );
    }

    #[test]
    fn a_resized_text_node_is_wider_and_taller() {
        let node =
            |size| Node { id: 1, x: 0.0, y: 0.0, kind: NodeKind::Text { template: "{a}".into(), size } };
        let card = |node: &Node| Card {
            id: "c".into(),
            name: "C".into(),
            enabled: true,
            nodes: vec![node.clone()],
            wires: Vec::new(),
        };
        let small = node(None);
        assert_eq!(width(&small.kind), 250.0);
        let big = node(Some([400.0, 152.0]));
        assert_eq!(width(&big.kind), 400.0);
        let below = |node: &Node| socket_position(node, &card(node), "a", true).unwrap().1;
        assert_eq!(below(&big) - below(&small), 100.0, "the input moves down with the box");
        // Never smaller than it opens, nor huge.
        assert_eq!(text_size(Some([10.0, f64::NAN])), (250.0, 52.0));
        assert_eq!(text_size(Some([5000.0, 5000.0])), (900.0, 900.0));
    }

    #[test]
    fn the_rain_count_has_no_field_while_emotes_decide() {
        // Emote rain from chat: emotes wired, count not. The Rain plugin's
        // count setting is only shown without emotes wired in.
        let mut card = crate::util::premade_cards().into_iter().find(|card| card.id == "emote-rain").unwrap();
        let rain = card.node(2).unwrap().clone();
        let row = |card: &Card, name: &str| {
            rows(&rain, card).into_iter().find(|row| match &row.kind {
                RowKind::Input { socket, .. } => socket.name == name,
                RowKind::Setting(Setting::Plugin(index)) => {
                    rain.kind.plugin_type().is_some_and(|ty| ty.settings[*index].key == name)
                }
                _ => false,
            })
        };
        assert_eq!(row(&card, "count").unwrap().height, ROW);
        assert!(row(&card, "per_message").is_some(), "Rain: every emote, under Emotes");
        // Without emotes wired, the count is a field again.
        card.wires.retain(|wire| wire.to.socket != "emotes");
        assert_eq!(row(&card, "count").unwrap().height, FIELD_ROW);
        assert!(row(&card, "per_message").is_none());
    }
}
