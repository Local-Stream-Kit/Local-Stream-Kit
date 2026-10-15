//! The built-in Rain plugin's overlay page: emotes fall, bounce off each
//! other and the floor, pile up and fade out. The overlay shell hosts it in
//! a sandboxed iframe and passes it the plugin's messages with
//! `postMessage` (the types are `protocol::plugin`'s); it draws on its own
//! canvas.
//!
//! The app picks the emotes (from chat, or the node's emotes setting); this
//! page decides how many fall: with "Rain" (only sent with emotes wired in)
//! every emote, each different one, or one at random; with a count, that
//! many, taking them in turn.

mod emote_images;
mod rain;
mod settings;

use std::cell::RefCell;
use std::rc::Rc;

use protocol::plugin::{EmoteValue, PluginMessage};
use serde::Deserialize;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{HtmlElement, MessageEvent};

use crate::rain::{Rain, Sprite};
use crate::settings::RainSettings;

/// The most emotes one run rains, whatever its count says.
const MAX_RAIN_COUNT: usize = 500;

/// The values a run of the rain node brings (see its manifest).
#[derive(Debug, Default, Deserialize)]
struct RainValues {
    #[serde(default)]
    emotes: Vec<EmoteValue>,
    /// Only with emotes wired in.
    #[serde(default)]
    per_message: Option<PerMessage>,
    /// None: every chosen emote.
    #[serde(default)]
    count: Option<f64>,
}

/// How many of a message's emotes rain. A zero-width stack ("PogU RainTime")
/// counts as one emote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PerMessage {
    /// Every emote: "Kappa Kappa PogU" → 3.
    Each,
    /// Each different emote once: "Kappa Kappa PogU" → 2.
    Distinct,
    /// One random emote from the message.
    One,
}

/// When the overlay got the command (the shell adds it to every message).
#[derive(Deserialize)]
struct Stamp {
    #[serde(default)]
    issued_at_ms: Option<f64>,
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let document = web_sys::window().and_then(|window| window.document()).ok_or("no document")?;
    let layer: HtmlElement = document.get_element_by_id("rain").ok_or("no #rain")?.dyn_into()?;
    let rain = Rc::new(RefCell::new(Rain::new(layer)?));
    listen(Rc::clone(&rain))?;
    start_animation_loop(rain);
    Ok(())
}

/// Messages from the overlay shell (only from it: other pages could post
/// here too), first the ones that came before this started (`index.html`
/// kept them).
fn listen(rain: Rc<RefCell<Rain>>) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let parent = window.parent()?.map(JsValue::from);
    let handle = move |event: MessageEvent| {
        if event.source().map(JsValue::from) != parent {
            return;
        }
        let Some(json) = js_sys::JSON::stringify(&event.data()).ok().and_then(|json| json.as_string()) else {
            return;
        };
        // The shell's own messages (the master volume) don't parse: nothing
        // here makes a sound.
        let Ok(message) = serde_json::from_str::<PluginMessage>(&json) else { return };
        let issued_at_ms = serde_json::from_str::<Stamp>(&json).ok().and_then(|stamp| stamp.issued_at_ms);
        let mut rain = rain.borrow_mut();
        match message {
            PluginMessage::Settings { values } => rain.configure(RainSettings::from_values(values)),
            PluginMessage::Run { values, changes, .. } => {
                let values: RainValues =
                    serde_json::from_value(serde_json::Value::Object(values)).unwrap_or_default();
                let sprites = chosen(&values);
                let issued_at_ms = issued_at_ms.unwrap_or_else(js_sys::Date::now);
                rain.spawn(sprites, (!changes.is_empty()).then_some(changes), issued_at_ms);
            }
            _ => {}
        }
    };
    let early = js_sys::Reflect::get(&window, &"rainEarly".into())?;
    if let Some(early) = early.dyn_ref::<js_sys::Function>() {
        window.remove_event_listener_with_callback("message", early)?;
    }
    let waiting = js_sys::Reflect::get(&window, &"rainWaiting".into())?;
    let waiting: Vec<MessageEvent> = waiting
        .dyn_into::<js_sys::Array>()
        .map(|waiting| waiting.iter().filter_map(|event| event.dyn_into().ok()).collect())
        .unwrap_or_default();
    let handle = Rc::new(handle);
    let on_message = {
        let handle = Rc::clone(&handle);
        Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| handle(event))
    };
    window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref())?;
    // Listens for as long as the page is open.
    on_message.forget();
    for event in waiting {
        handle(event);
    }
    Ok(())
}

/// The emotes that fall: narrowed by "Rain", then a count's worth taken in
/// turn ("Kappa PogU", 5 → Kappa PogU Kappa PogU Kappa).
fn chosen(values: &RainValues) -> Vec<Sprite> {
    let emotes = &values.emotes;
    let picked: Vec<&EmoteValue> = match values.per_message {
        None | Some(PerMessage::Each) => emotes.iter().collect(),
        Some(PerMessage::Distinct) => {
            let mut distinct: Vec<&EmoteValue> = Vec::new();
            for emote in emotes {
                if !distinct.contains(&emote) {
                    distinct.push(emote);
                }
            }
            distinct
        }
        Some(PerMessage::One) if emotes.is_empty() => Vec::new(),
        Some(PerMessage::One) => {
            let index = (js_sys::Math::random() * emotes.len() as f64) as usize;
            vec![&emotes[index.min(emotes.len() - 1)]]
        }
    };
    let take = match values.count {
        Some(count) if count.is_finite() => (count.max(0.0) as usize).min(MAX_RAIN_COUNT),
        Some(_) => 0,
        None => picked.len().min(MAX_RAIN_COUNT),
    };
    if picked.is_empty() {
        return Vec::new();
    }
    picked
        .iter()
        .cycle()
        .take(take)
        .map(|emote| Sprite { image: emote.url.clone(), overlays: emote.overlays.clone() })
        .collect()
}

fn start_animation_loop(rain: Rc<RefCell<Rain>>) {
    type FrameCallback = Closure<dyn FnMut(f64)>;
    let callback: Rc<RefCell<Option<FrameCallback>>> = Rc::new(RefCell::new(None));
    let next = Rc::clone(&callback);
    *callback.borrow_mut() = Some(Closure::new(move |now_ms: f64| {
        rain.borrow_mut().frame(now_ms);
        request_frame(next.borrow().as_ref().expect("callback is set"));
    }));
    request_frame(callback.borrow().as_ref().expect("callback is set"));
}

fn request_frame(callback: &Closure<dyn FnMut(f64)>) {
    if let Some(window) = web_sys::window() {
        let _ = window.request_animation_frame(callback.as_ref().unchecked_ref());
    }
}

mod log {
    pub fn warn(message: &str) {
        web_sys::console::warn_1(&message.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emote(code: &str) -> EmoteValue {
        EmoteValue { code: code.into(), url: format!("/emote/7tv/{code}"), overlays: Vec::new() }
    }

    fn codes(sprites: Vec<Sprite>) -> Vec<String> {
        sprites.into_iter().map(|sprite| sprite.image.trim_start_matches("/emote/7tv/").to_string()).collect()
    }

    #[test]
    fn every_emote_each_different_one_and_counts() {
        let emotes = vec![emote("Kappa"), emote("Kappa"), emote("PogU")];
        let values = |per_message, count| RainValues { emotes: emotes.clone(), per_message, count };
        assert_eq!(codes(chosen(&values(Some(PerMessage::Each), None))), ["Kappa", "Kappa", "PogU"]);
        assert_eq!(codes(chosen(&values(Some(PerMessage::Distinct), None))), ["Kappa", "PogU"]);
        // A count takes them in turn.
        assert_eq!(
            codes(chosen(&values(Some(PerMessage::Distinct), Some(5.0)))),
            ["Kappa", "PogU", "Kappa", "PogU", "Kappa"]
        );
        assert_eq!(codes(chosen(&values(None, Some(1.0)))), ["Kappa"]);
        assert!(chosen(&values(None, Some(0.0))).is_empty());
        assert_eq!(chosen(&values(None, Some(10_000.0))).len(), MAX_RAIN_COUNT);
        assert!(chosen(&RainValues { emotes: Vec::new(), per_message: None, count: Some(5.0) }).is_empty());
    }

    #[test]
    fn values_as_the_app_sends_them() {
        let values: RainValues = serde_json::from_value(serde_json::json!({
            "emotes": [{ "code": "PogU", "url": "/emote/7tv/PogU", "overlays": ["/emote/7tv/RainTime"] }],
            "per_message": "one"
        }))
        .unwrap();
        assert_eq!(values.per_message, Some(PerMessage::One));
        assert_eq!(values.count, None);
        assert_eq!(values.emotes[0].overlays, ["/emote/7tv/RainTime"]);
    }
}
