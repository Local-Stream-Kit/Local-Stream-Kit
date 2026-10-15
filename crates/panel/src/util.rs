//! Small helpers shared by the views.

use protocol::activity::NameColor;
use protocol::cards::{Card, Category, NodeKind};
use wasm_bindgen::JsCast;
use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

/// Whether a key press (or copy, paste) is going into a text field, so
/// shortcuts stay out of it. Switches, checkboxes, sliders and dropdowns
/// keep focus after a click but take no text, so they don't count.
pub fn typing_in_field(event: &web_sys::Event) -> bool {
    let Some(target) = event.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok()) else {
        return false;
    };
    if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
        return matches!(
            input.type_().as_str(),
            "text" | "search" | "number" | "email" | "url" | "password" | "tel" | ""
        );
    }
    target.tag_name().eq_ignore_ascii_case("textarea") || target.has_attribute("contenteditable")
}

/// The value of the input, select or textarea an event came from.
pub fn value_of(event: &Event) -> String {
    let Some(target) = event.target() else { return String::new() };
    if let Some(input) = target.dyn_ref::<HtmlInputElement>() {
        input.value()
    } else if let Some(select) = target.dyn_ref::<HtmlSelectElement>() {
        select.value()
    } else if let Some(area) = target.dyn_ref::<HtmlTextAreaElement>() {
        area.value()
    } else {
        String::new()
    }
}

pub fn checked_of(event: &Event) -> bool {
    event
        .target()
        .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        .is_some_and(|input| input.checked())
}

/// A number from a field; `None` while it isn't one yet (mid-typing).
pub fn number_of(event: &Event) -> Option<f64> {
    value_of(event).trim().replace(',', ".").parse::<f64>().ok().filter(|number| number.is_finite())
}

/// A number as it should appear in a field: no trailing `.0`.
pub fn show_number(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        format!("{}", number as i64)
    } else {
        let text = format!("{number:.3}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// "Channel point reward (Hydrate) → Cooldown → Alert": what a card does,
/// in one line.
pub fn summary(card: &Card) -> String {
    let mut parts: Vec<String> = Vec::new();
    for category in [Category::Events, Category::Flow, Category::Effects] {
        let mut titles: Vec<String> = Vec::new();
        for node in card.nodes.iter().filter(|node| node.kind.category() == category) {
            let title = match &node.kind {
                NodeKind::Reward { reward } if !reward.trim().is_empty() => {
                    format!("Reward “{}”", reward.trim())
                }
                NodeKind::Bits { min } if *min > 0.0 => format!("Bits ≥ {}", show_number(*min)),
                NodeKind::Raid { min_viewers } if *min_viewers > 0.0 => {
                    format!("Raid ≥ {}", show_number(*min_viewers))
                }
                NodeKind::ChatWords { words } => {
                    let words: Vec<&str> =
                        words.iter().map(|word| word.trim()).filter(|word| !word.is_empty()).collect();
                    if words.is_empty() {
                        "Chat words".into()
                    } else {
                        format!("Chat words “{}”", words.join(", "))
                    }
                }
                NodeKind::ChatCommand { command } if !command.trim().is_empty() => {
                    format!("Chat command “{}”", command.trim())
                }
                NodeKind::Timer { minutes, .. } => format!("Timer (every {} min)", show_number(*minutes)),
                kind => kind.title().to_string(),
            };
            if !titles.contains(&title) {
                titles.push(title);
            }
        }
        if !titles.is_empty() {
            parts.push(titles.join(" / "));
        }
    }
    if parts.is_empty() { "Empty".into() } else { parts.join(" → ") }
}

/// A Twitch channel name as typed or pasted: `#name`, `Name` or a
/// `twitch.tv/name` link all give `name`.
pub fn channel_name(typed: &str) -> String {
    let typed = typed.trim();
    let name = match typed.find("twitch.tv/") {
        Some(at) => typed[at + "twitch.tv/".len()..].split(['/', '?', '#']).next().unwrap_or_default(),
        None => typed.trim_start_matches('#'),
    };
    name.trim().to_ascii_lowercase()
}

/// A new card id from its name, unique among `existing`.
pub fn new_card_id(name: &str, existing: &[Card]) -> String {
    let mut slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        slug = "card".into();
    }
    let mut id = slug.clone();
    let mut n = 2;
    while existing.iter().any(|card| card.id == id) {
        id = format!("{slug}-{n}");
        n += 1;
    }
    id
}

/// `text` with the viewers' names in their chat colours (where they
/// picked one), as on Twitch.
pub fn with_names(text: &str, names: &[NameColor]) -> Html {
    if names.is_empty() {
        return html! { { text.to_string() } };
    }
    html! {
        { for split_names(text, names).into_iter().map(|(part, name)| match name {
            Some(name) => html! {
                <span class="viewer-name" style={format!("color: {}", readable_color(&name.color))}>{ part }</span>
            },
            None => html! { { part } },
        }) }
    }
}

/// `text` with its links ([`protocol::links`]) as links, and the viewers'
/// names in their chat colours in the text between. A click on a link
/// calls `open` with its address instead of following it: the panel is
/// usually an OBS dock, where it would open inside OBS.
pub fn with_links(text: &str, names: &[NameColor], open: &Callback<String>) -> Html {
    let links = protocol::links::find(text);
    if links.is_empty() {
        return with_names(text, names);
    }
    let mut pieces = Vec::with_capacity(links.len() * 2 + 1);
    let mut at = 0;
    for link in links {
        pieces.push(with_names(&text[at..link.range.start], names));
        let onclick = {
            let (open, url) = (open.clone(), link.url.clone());
            Callback::from(move |event: MouseEvent| {
                // Not followed here, and the History entry it's in doesn't
                // open or close.
                event.prevent_default();
                event.stop_propagation();
                open.emit(url.clone());
            })
        };
        pieces.push(html! {
            <a class="viewer-link" href={link.url.clone()} target="_blank" rel="noopener noreferrer"
               title={format!("{} (opens in your browser)", link.url)} {onclick}>
                { text[link.range.clone()].to_string() }
            </a>
        });
        at = link.range.end;
    }
    pieces.push(with_names(&text[at..], names));
    html! { { for pieces } }
}

/// `text` in pieces, each a name (with its colour) or the text between.
/// Names count as whole words only, so "Al" isn't found in "Alpha".
fn split_names<'t, 'n>(text: &'t str, names: &'n [NameColor]) -> Vec<(&'t str, Option<&'n NameColor>)> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let found_at = |rest: &str, offset: usize, name: &NameColor| {
        let mut from = 0;
        while let Some(at) = rest[from..].find(&name.name).map(|at| at + from) {
            let end = at + name.name.len();
            let starts = !text[..offset + at].chars().next_back().is_some_and(word);
            let ends = !rest[end..].chars().next().is_some_and(word);
            if starts && ends {
                return Some(at);
            }
            from = at + rest[at..].chars().next().map_or(1, char::len_utf8);
        }
        None
    };
    let mut parts = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        let rest = &text[offset..];
        let next = names
            .iter()
            .filter(|name| !name.name.is_empty())
            .filter_map(|name| found_at(rest, offset, name).map(|at| (at, name)))
            .min_by_key(|(at, name)| (*at, std::cmp::Reverse(name.name.len())));
        let Some((at, name)) = next else {
            parts.push((rest, None));
            break;
        };
        if at > 0 {
            parts.push((&rest[..at], None));
        }
        parts.push((&rest[at..at + name.name.len()], Some(name)));
        offset += at + name.name.len();
    }
    parts
}

/// A name colour that reads on the panel's dark background: too dark
/// ones (navy, black) are mixed with white until they do.
fn readable_color(color: &str) -> String {
    let Some((r, g, b)) = protocol::alert::parse_hex(color) else { return "inherit".into() };
    let luminance = |r: f64, g: f64, b: f64| {
        let linear = |c: f64| {
            let c = c / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    };
    let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
    let mut mixed = (r, g, b);
    for step in 0..=10 {
        let t = f64::from(step) / 10.0;
        mixed = (r + (255.0 - r) * t, g + (255.0 - g) * t, b + (255.0 - b) * t);
        if luminance(mixed.0, mixed.1, mixed.2) >= 0.18 {
            break;
        }
    }
    format!("#{:02x}{:02x}{:02x}", mixed.0.round() as u8, mixed.1.round() as u8, mixed.2.round() as u8)
}

/// The premade cards with the built-in plugins' node types attached, as
/// the panel has them (tests).
#[cfg(test)]
pub fn premade_cards() -> Vec<protocol::cards::Card> {
    let mut cards = protocol::cards::premade();
    for card in &mut cards {
        card.attach_plugins(&built_in_plugins());
    }
    cards
}

/// The cards the New card page starts from, likewise (tests).
#[cfg(test)]
pub fn preset_cards() -> Vec<protocol::cards::Card> {
    let mut cards: Vec<_> =
        protocol::cards::presets().into_iter().map(|preset| (preset.card)("preset")).collect();
    for card in &mut cards {
        card.attach_plugins(&built_in_plugins());
    }
    cards
}

#[cfg(test)]
pub(crate) fn built_in_plugins() -> protocol::plugin::Catalog {
    let plugin = |manifest: &str| protocol::plugin::PluginInfo {
        manifest: serde_json::from_str(manifest).unwrap(),
        overlay: true,
        bundled: true,
        ..Default::default()
    };
    protocol::plugin::Catalog::new(vec![
        plugin(include_str!("../../server/plugins/rain/plugin.json")),
        plugin(include_str!("../../server/plugins/alert/plugin.json")),
        plugin(include_str!("../../server/plugins/speak/plugin.json")),
        plugin(include_str!("../../server/plugins/sound/plugin.json")),
        plugin(include_str!("../../server/plugins/media/plugin.json")),
        plugin(include_str!("../../server/plugins/particles/plugin.json")),
        plugin(include_str!("../../server/plugins/chat/plugin.json")),
        plugin(include_str!("../../server/plugins/hype-train/plugin.json")),
    ])
}

/// "21:04" today, "9 Oct 21:04" before.
pub fn time(at_ms: u64) -> String {
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let at = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at_ms as f64));
    let now = js_sys::Date::new_0();
    let clock = format!("{:02}:{:02}", at.get_hours(), at.get_minutes());
    let today = at.get_date() == now.get_date()
        && at.get_month() == now.get_month()
        && at.get_full_year() == now.get_full_year();
    if today { clock } else { format!("{} {} {clock}", at.get_date(), MONTHS[at.get_month() as usize % 12]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_in_text() {
        let name = |name: &str| NameColor { name: name.into(), color: "#ff4500".into() };
        let names = [name("Al"), name("Bo")];
        let pieces = |text: &str| -> Vec<(String, bool)> {
            split_names(text, &names)
                .into_iter()
                .map(|(part, name)| (part.to_string(), name.is_some()))
                .collect()
        };
        assert_eq!(
            pieces("Al gifted a sub to Bo"),
            [("Al".into(), true), (" gifted a sub to ".into(), false), ("Bo".into(), true)]
        );
        assert_eq!(
            pieces("Alpha met Al_x and Al."),
            [("Alpha met Al_x and ".into(), false), ("Al".into(), true), (".".into(), false)]
        );
        assert_eq!(pieces("nobody"), [("nobody".into(), false)]);
    }

    #[test]
    fn dark_name_colours_are_brightened() {
        assert_eq!(readable_color("#ff4500"), "#ff4500", "bright enough already");
        assert_ne!(readable_color("#0000ff"), "#0000ff");
        assert_eq!(readable_color("not a colour"), "inherit");
    }

    #[test]
    fn summaries_and_ids() {
        let cards = premade_cards();
        let hydrate = cards.iter().find(|card| card.id == "hydrate").unwrap();
        assert_eq!(summary(hydrate), "Reward “Hydrate” → Cooldown → Alert");
        let rain = cards.iter().find(|card| card.id == "rain-on-bits").unwrap();
        assert_eq!(summary(rain), "Bits ≥ 100 → Emote rain");
        assert_eq!(new_card_id("Hydrate!", &cards), "hydrate-2");
        assert_eq!(new_card_id("  ", &cards), "card");
        assert_eq!(show_number(6.0), "6");
        assert_eq!(show_number(0.25), "0.25");
        assert_eq!(channel_name(" #Marrtin "), "marrtin");
        assert_eq!(channel_name("https://www.twitch.tv/Marrtin?sr=a"), "marrtin");
        assert_eq!(channel_name("twitch.tv/marrtin/videos"), "marrtin");
        assert_eq!(channel_name(""), "");
    }
}
