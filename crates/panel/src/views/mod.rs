//! The dock pages: Cards, New card, Activity, Plugins (with the text
//! styles), Test, Status (with the Twitch login and the control API).

pub mod activity;
pub mod boxes;
pub mod cards;
pub mod control;
pub mod login;
pub mod new_card;
pub mod overlays;
pub mod plugins;
pub mod position;
pub mod sounds;
pub mod speech;
pub mod status;
pub mod tab;
pub mod test;
pub mod text_styles;
pub mod viewers;

use yew::prelude::*;

/// A platform's logo, small, in the text's colour: Twitch's glitch, OBS
/// Studio's (paths from Simple Icons, CC0).
pub fn platform_icon(platform: &str) -> Html {
    let (path, label) = match platform {
        "twitch" => (
            "M11.571 4.714h1.715v5.143H11.57zm4.715 0H18v5.143h-1.714zM6 0L1.714 4.286v15.428h5.143V24l4.286-4.286h3.428L22.286 12V0zm14.571 11.143l-3.428 3.428h-3.429l-3 3v-3H6.857V1.714h13.714Z",
            "Twitch",
        ),
        "obs" => (
            "M12,24C5.383,24,0,18.617,0,12S5.383,0,12,0s12,5.383,12,12S18.617,24,12,24z M12,1.109 C5.995,1.109,1.11,5.995,1.11,12C1.11,18.005,5.995,22.89,12,22.89S22.89,18.005,22.89,12C22.89,5.995,18.005,1.109,12,1.109z M6.182,5.99c0.352-1.698,1.503-3.229,3.05-3.996c-0.269,0.273-0.595,0.483-0.844,0.78c-1.02,1.1-1.48,2.692-1.199,4.156 c0.355,2.235,2.455,4.06,4.732,4.028c1.765,0.079,3.485-0.937,4.348-2.468c1.848,0.063,3.645,1.017,4.7,2.548 c0.54,0.799,0.962,1.736,0.991,2.711c-0.342-1.295-1.202-2.446-2.375-3.095c-1.135-0.639-2.529-0.802-3.772-0.425 c-1.56,0.448-2.849,1.723-3.293,3.293c-0.377,1.25-0.216,2.628,0.377,3.772c-0.825,1.429-2.315,2.449-3.932,2.756 c-1.244,0.261-2.551,0.059-3.709-0.464c1.036,0.302,2.161,0.355,3.191-0.011c1.381-0.457,2.522-1.567,3.024-2.935 c0.556-1.49,0.345-3.261-0.591-4.54c-0.7-1.007-1.803-1.717-3.002-1.969c-0.38-0.068-0.764-0.098-1.148-0.134 c-0.611-1.231-0.834-2.66-0.528-3.996L6.182,5.99z",
            "OBS",
        ),
        _ => return html! {},
    };
    html! {
        <svg class={classes!("platform-icon", platform.to_string())} width="13" height="13" viewBox="0 0 24 24"
             fill="currentColor" role="img" aria-label={label}>
            <title>{ label }</title>
            <path d={path}></path>
        </svg>
    }
}

/// A node's icon: its platform's logo, or its plugin's own (`icon` in its
/// manifest), served by the app and drawn as a mask in the text's colour,
/// so a plugin's SVG is never put in the page.
pub fn node_icon(plugins: &protocol::plugin::Catalog, kind: &protocol::cards::NodeKind) -> Html {
    if let Some(platform) = kind.platform() {
        return platform_icon(platform);
    }
    let protocol::cards::NodeKind::Plugin { plugin, .. } = kind else { return html! {} };
    let Some(info) = plugins.plugin(plugin).filter(|info| !info.manifest.icon.is_empty()) else {
        return html! {};
    };
    let url = protocol::plugin::icon_path(plugin);
    let style = format!("-webkit-mask-image: url(\"{url}\"); mask-image: url(\"{url}\")");
    let name = info.manifest.name.clone();
    html! { <span class="platform-icon plugin-icon" role="img" aria-label={name.clone()} title={name} {style}></span> }
}

/// A small stroke icon, by name.
pub fn icon(name: &str) -> Html {
    let path = match name {
        "plus" => "M8 3v10M3 8h10",
        "download" => "M8 2.5v8M4.5 7 8 10.5 11.5 7M3 13.5h10",
        "back" => "M10 3 5 8l5 5",
        "warn" => "M8 2 14.5 13.5h-13zM8 6.5v3M8 11.6v.4",
        "error" => "M8 2a6 6 0 1 0 0 12A6 6 0 0 0 8 2zM8 5v3.5M8 11v.3",
        "copy" => "M5.5 5.5h7v7h-7zM3.5 10.5v-7h7",
        "check" => "M3 8.5l3.2 3L13 4.5",
        "refresh" => "M13 3.5v3h-3M12.6 6.5A5 5 0 1 0 13 9",
        "folder" => "M2 4.5h4l1.5 1.5H14v6.5H2z",
        "external" => "M6 3h7v7M13 3 4 12",
        "undo" => "M5 3 2 6l3 3M2 6h7a4 4 0 0 1 0 8H6",
        "redo" => "m11 3 3 3-3 3M14 6H7a4 4 0 0 0 0 8h3",
        "trash" => "M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.7 9h5.6l.7-9",
        "duplicate" => "M5.5 5.5h7v7h-7zM3.5 10.5v-7h7",
        "menu" => "M3 4.5h10M3 8h10M3 11.5h10",
        "grip" => "M6 4h.01M10 4h.01M6 8h.01M10 8h.01M6 12h.01M10 12h.01",
        "chevron" => "m6 4 4 4-4 4",
        "down" => "M4.5 6.5 8 10l3.5-3.5",
        "search" => "M7 2.5a4.5 4.5 0 1 0 0 9 4.5 4.5 0 0 0 0-9zM10.5 10.5l3 3",
        "pause" => "M5.5 3.5v9M10.5 3.5v9",
        "play" => "M5 3.5v9l7-4.5z",
        "skip" => "M4 3.5v9l6-4.5zM12 3.5v9",
        "top" => "M3.5 3h9M8 13.5V6.5M4.8 9.7 8 6.5l3.2 3.2",
        "replay" => "M3.5 2.5v3.2h3.2M3.9 5.6A4.8 4.8 0 1 1 3.2 9",
        "volume" => "M2.5 6h2.5l3.5-3v10L5 10H2.5zM11 5.5a3.5 3.5 0 0 1 0 5M12.8 3.8a6 6 0 0 1 0 8.4",
        "mute" => "M2.5 6h2.5l3.5-3v10L5 10H2.5zM11 6l3.5 4M14.5 6 11 10",
        "pencil" => "M10.5 2.5l3 3L6 13H3v-3z",
        "help" => {
            "M8 1.8a6.2 6.2 0 1 0 0 12.4A6.2 6.2 0 0 0 8 1.8zM6.3 6.3a1.8 1.8 0 1 1 2.4 1.7c-.5.2-.7.6-.7 1.1v.3M8 11.3v.1"
        }
        "more" => "M3.5 8h.01M8 8h.01M12.5 8h.01",
        "screen" => "M2.5 2.5h11a1 1 0 0 1 1 1v7a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1zM5.5 14h5",

        _ => "",
    };
    html! {
        <svg class="icon" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor"
             stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d={path}></path>
        </svg>
    }
}
