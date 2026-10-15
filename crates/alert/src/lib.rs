//! The built-in Alerts plugin's overlay page: a message on the overlay in
//! a style, with its picture and sound, then the text it reads aloud. The
//! overlay shell hosts it in a sandboxed iframe and passes it the plugin's
//! messages with `postMessage` (the types are `protocol::plugin`'s).
//!
//! Alerts wait their turn in the app's queue: a run comes with its
//! `queue_id` when it's on stage, and this page answers `done` once the
//! alert has left (after its message has been read). A new run replaces
//! whatever still shows: the queue has moved on (it was skipped, or another
//! overlay finished first). The markup, classes and CSS variables come from
//! `protocol::alert`, the look from `alert.css`.
//!
//! In the panel's preview (a run with `preview`), the alert stays up, and
//! the plugin's settings drawn as they're edited restyle it in place.

mod level;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use protocol::AlertStyle;
use protocol::plugin::{PageMessage, PluginMessage, PreviewRun, ShellMessage, TextPart, TextValue};
use protocol::{SpeechClip, sound};
use serde::Deserialize;
use serde_json::{Map, Value};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Document, HtmlAudioElement, HtmlElement, HtmlImageElement, HtmlMediaElement, MessageEvent};

/// The longest a spoken message waits for its alert's sound to end.
const SOUND_WAIT_MS: f64 = 15_000.0;
/// Between the alert's sound and its message.
const SPEECH_GAP_MS: f64 = 300.0;
/// The alert stays this long after its message ends.
const SPEECH_TAIL_MS: f64 = 700.0;
/// A message whose length isn't known (a plugin voice's MP3 or Ogg): until
/// it ends, a minute at most.
const UNKNOWN_SPEECH_MS: f64 = 60_000.0;

/// The values an Alert node's run brings (see its manifest).
#[derive(Debug, Clone, Default, Deserialize)]
struct AlertValues {
    #[serde(default)]
    message: Option<TextValue>,
    /// Seconds.
    #[serde(default)]
    duration: Option<f64>,
    /// The text read aloud, with its spoken message.
    #[serde(default)]
    read: Option<TextValue>,
}

/// One alert to show.
struct Item {
    queue_id: Option<u64>,
    values: AlertValues,
    style: AlertStyle,
    /// A run in the panel's preview: it stays up.
    preview: Option<PreviewRun>,
    /// Drawn again in a new style: no coming in, no sound.
    restyled: bool,
}

struct Showing {
    frame: HtmlElement,
    alert: HtmlElement,
    leave_at_ms: f64,
    remove_at_ms: f64,
    leave_ms: f64,
    leaving: bool,
    sound: Option<HtmlAudioElement>,
    /// Not started yet; the alert doesn't leave until it has.
    speech: Option<Speaking>,
    /// Started; stopped if the alert is skipped.
    spoken: Option<HtmlAudioElement>,
    /// The preview's: it never leaves by itself.
    held: bool,
}

struct Speaking {
    audio: HtmlAudioElement,
    duration_ms: f64,
    volume: f32,
    shown_at_ms: f64,
    /// Set once the alert's sound has ended.
    start_at_ms: Option<f64>,
}

struct Stage {
    queue_id: Option<u64>,
    showing: Showing,
    /// Ends without a `done`: skipped, so the queue has moved on.
    quiet: bool,
}

struct Page {
    layer: HtmlElement,
    /// The plugin's settings: the style for runs that bring none.
    shared: Map<String, Value>,
    stage: Option<Stage>,
    /// Arrived since the last frame; it starts on the next one.
    next: Option<Item>,
    /// Font stylesheets already added to the page.
    fonts: HashSet<String>,
    /// The preview's alert on stage, to draw again when the settings
    /// change.
    held: Option<AlertValues>,
    /// The settings changed while the preview's alert shows.
    restyle: bool,
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let layer: HtmlElement = document()?.get_element_by_id("alerts").ok_or("no #alerts")?.dyn_into()?;
    let page = Rc::new(RefCell::new(Page {
        layer,
        shared: Map::new(),
        stage: None,
        next: None,
        fonts: HashSet::new(),
        held: None,
        restyle: false,
    }));
    listen(Rc::clone(&page))?;
    start_animation_loop(page);
    Ok(())
}

/// Messages from the overlay shell (only from it: other pages could post
/// here too), first the ones that came before this started (`index.html`
/// kept them).
fn listen(page: Rc<RefCell<Page>>) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let parent = window.parent()?.map(JsValue::from);
    let handle = move |event: MessageEvent| {
        if event.source().map(JsValue::from) != parent {
            return;
        }
        let Some(json) = js_sys::JSON::stringify(&event.data()).ok().and_then(|json| json.as_string()) else {
            return;
        };
        if let Ok(ShellMessage::Volume { volume, muted }) = serde_json::from_str(&json) {
            level::set(volume, muted);
            return;
        }
        let Ok(message) = serde_json::from_str::<PluginMessage>(&json) else { return };
        page.borrow_mut().handle(message);
    };
    let early = js_sys::Reflect::get(&window, &"alertEarly".into())?;
    if let Some(early) = early.dyn_ref::<js_sys::Function>() {
        window.remove_event_listener_with_callback("message", early)?;
    }
    let waiting = js_sys::Reflect::get(&window, &"alertWaiting".into())?;
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

impl Page {
    fn handle(&mut self, message: PluginMessage) {
        match message {
            PluginMessage::Settings { values } => {
                self.shared = values;
                // The preview's alert follows the settings being edited.
                self.restyle = self.held.is_some();
            }
            PluginMessage::Run { values, settings, queue_id, preview, .. } => {
                // Sent again (the overlay reconnected): it carries on.
                let same = |id: Option<u64>| id.is_some() && id == queue_id;
                if self.stage.as_ref().is_some_and(|stage| same(stage.queue_id) && !stage.quiet)
                    || self.next.as_ref().is_some_and(|next| same(next.queue_id))
                {
                    return;
                }
                let settings = if settings.is_empty() { self.shared.clone() } else { settings };
                let style = style_of(settings);
                let values = serde_json::from_value(Value::Object(values)).unwrap_or_default();
                self.next = Some(Item { queue_id, values, style, preview, restyled: false });
            }
            PluginMessage::Skip { queue_id } => {
                if self.next.as_ref().is_some_and(|next| next.queue_id == Some(queue_id)) {
                    self.next = None;
                }
                if let Some(stage) = &mut self.stage
                    && stage.queue_id == Some(queue_id)
                    && !stage.quiet
                {
                    stage.showing.hide(js_sys::Date::now());
                    stage.quiet = true;
                }
            }
            _ => {}
        }
    }

    fn frame(&mut self, now_ms: f64) {
        if std::mem::take(&mut self.restyle)
            && self.next.is_none()
            && let Some(values) = self.held.clone()
        {
            let style = style_of(self.shared.clone());
            self.next =
                Some(Item { queue_id: None, values, style, preview: Some(PreviewRun::Show), restyled: true });
        }
        if let Some(item) = self.next.take() {
            // The queue has moved on from whatever still shows.
            if let Some(stage) = self.stage.take() {
                stage.showing.remove();
            }
            self.held = item.preview.is_some().then(|| item.values.clone());
            match self.show(&item, now_ms) {
                Ok(showing) => self.stage = Some(Stage { queue_id: item.queue_id, showing, quiet: false }),
                Err(error) => {
                    warn(&format!("couldn't show an alert: {error:?}"));
                    done(item.queue_id);
                }
            }
        }
        let Some(stage) = &mut self.stage else { return };
        if stage.showing.frame(now_ms) {
            if !stage.quiet {
                done(stage.queue_id);
            }
            self.stage = None;
            self.held = None;
        }
    }

    fn show(&mut self, item: &Item, now_ms: f64) -> Result<Showing, JsValue> {
        let document = document()?;
        let style = &item.style;
        if let Some(sheet) = style.font_stylesheet()
            && self.fonts.insert(sheet.clone())
        {
            load_stylesheet(&document, &sheet)?;
        }
        let frame = element(&document, "alert-frame")?;
        frame.set_attribute("style", &style.frame_css())?;
        let room = element(&document, "alert-room")?;
        let body = element(&document, &style.classes())?;
        body.set_attribute("style", &style.css())?;
        if item.restyled {
            body.class_list().add_1("still")?;
        }
        // Drawn as the preview opens, or again in a new style: quietly.
        let quiet = item.preview == Some(PreviewRun::Show);
        if let Some(url) = style.picture_url() {
            let picture: HtmlImageElement = document.create_element("img")?.dyn_into()?;
            picture.set_class_name("alert-picture");
            picture.set_alt("");
            picture.set_src(&url);
            body.append_child(&picture)?;
        }
        let sound = sound_url(&style.sound).filter(|_| !quiet).and_then(|url| level::preload(&url));
        if let Some(sound) = &sound {
            level::start(sound, style.sound_volume);
        }
        let text_element = element(&document, "alert-text")?;
        let message = item.values.message.as_ref();
        match message {
            Some(message) if !message.parts.is_empty() => {
                for part in &message.parts {
                    let shown: HtmlElement = match part {
                        TextPart::Emote { emote, .. } if style.emotes => {
                            let images = std::iter::once(&emote.url).chain(&emote.overlays);
                            emote_element(&document, &emote.code, images)?
                        }
                        TextPart::Emote { emote, .. } => text_span(&document, &emote.code)?,
                        TextPart::Text { text, .. } => text_span(&document, text)?,
                    };
                    let Some(text_style) = part.style() else {
                        text_element.append_child(&shown)?;
                        continue;
                    };
                    // A part a text style marked: a span around it.
                    if let Some(sheet) = text_style.font_stylesheet()
                        && self.fonts.insert(sheet.clone())
                    {
                        load_stylesheet(&document, &sheet)?;
                    }
                    let styled: HtmlElement = document.create_element("span")?.dyn_into()?;
                    styled.set_class_name(&format!("text-style {}", text_style.class()));
                    styled.set_attribute("style", &text_style.css())?;
                    styled.append_child(&shown)?;
                    text_element.append_child(&styled)?;
                }
            }
            Some(message) => text_element.set_text_content(Some(&message.text)),
            None => {}
        }
        body.append_child(&text_element)?;
        room.append_child(&body)?;
        frame.append_child(&room)?;
        self.layer.append_child(&frame)?;
        let seconds = item.values.duration.filter(|seconds| seconds.is_finite()).unwrap_or(6.0);
        let duration = seconds.clamp(0.5, 600.0) * 1000.0;
        let leave = f64::from(style.animation.leave_ms());
        let speech =
            item.values.read.as_ref().and_then(|read| read.speech.as_ref()).filter(|_| !quiet).and_then(
                |clip| {
                    let SpeechClip { url, duration_ms, volume } = clip;
                    Some(Speaking {
                        audio: level::preload(url)?,
                        duration_ms: duration_ms.map_or(UNKNOWN_SPEECH_MS, f64::from),
                        volume: *volume,
                        shown_at_ms: now_ms,
                        start_at_ms: None,
                    })
                },
            );
        let held = item.preview.is_some();
        Ok(Showing {
            frame,
            alert: body,
            leave_at_ms: if held { f64::INFINITY } else { now_ms + (duration - leave).max(0.0) },
            remove_at_ms: if held { f64::INFINITY } else { now_ms + duration.max(leave) },
            leave_ms: leave,
            leaving: false,
            sound,
            speech,
            spoken: None,
            held,
        })
    }
}

impl Showing {
    /// Starts the spoken message once the alert's sound has ended, and keeps
    /// the alert up until the message has been read.
    fn speak(&mut self, now_ms: f64) {
        let Some(speech) = &mut self.speech else { return };
        if speech.start_at_ms.is_none() {
            let sound_over = self.sound.as_ref().is_none_or(|sound| {
                sound.ended()
                    || sound.paused()
                    || sound.network_state() == HtmlMediaElement::NETWORK_NO_SOURCE
            });
            if sound_over || now_ms - speech.shown_at_ms >= SOUND_WAIT_MS {
                speech.start_at_ms = Some(now_ms + SPEECH_GAP_MS);
            }
        }
        if speech.start_at_ms.is_some_and(|at| now_ms >= at) {
            level::start(&speech.audio, speech.volume);
            let end = now_ms + speech.duration_ms + SPEECH_TAIL_MS;
            if end > self.leave_at_ms {
                self.leave_at_ms = end;
                self.remove_at_ms = end + self.leave_ms;
            }
            self.spoken = self.speech.take().map(|speech| speech.audio);
        }
    }

    /// Skipped: quiet now, and on its way out.
    fn hide(&mut self, now_ms: f64) {
        for audio in self.sound.iter().chain(&self.spoken) {
            let _ = audio.pause();
        }
        self.speech = None;
        if !self.leaving {
            self.leave_at_ms = now_ms;
            self.remove_at_ms = now_ms + self.leave_ms;
        }
    }

    /// Gone at once (another alert takes the stage).
    fn remove(mut self) {
        self.hide(0.0);
        self.frame.remove();
    }

    /// Whether it has left the page.
    fn frame(&mut self, now_ms: f64) -> bool {
        self.speak(now_ms);
        // A message that has ended (or failed) lets the alert go early,
        // but for the preview's.
        if !self.held
            && let Some(spoken) = &self.spoken
            && (spoken.ended() || spoken.error().is_some())
            && self.leave_at_ms > now_ms + SPEECH_TAIL_MS
        {
            self.leave_at_ms = now_ms + SPEECH_TAIL_MS;
            self.remove_at_ms = self.leave_at_ms + self.leave_ms;
        }
        let speaking = self.speech.is_some();
        if !speaking && !self.leaving && now_ms >= self.leave_at_ms {
            let _ = self.alert.class_list().add_1("leaving");
            self.leaving = true;
        }
        if !speaking && now_ms >= self.remove_at_ms {
            self.frame.remove();
            return true;
        }
        false
    }
}

/// The style the plugin's settings describe. No picture or sound comes as
/// `null`; anything missing or unreadable is the default's.
fn style_of(settings: Map<String, Value>) -> AlertStyle {
    let settings: Map<String, Value> = settings.into_iter().filter(|(_, value)| !value.is_null()).collect();
    serde_json::from_value(Value::Object(settings)).unwrap_or_else(|error| {
        warn(&format!("an alert's style couldn't be read: {error}"));
        AlertStyle::default()
    })
}

/// Where the style's sound loads from: a path on the app (as plugin
/// settings come) or an http(s) address as it is, else a sound's name.
fn sound_url(sound: &str) -> Option<String> {
    let sound = sound.trim();
    if sound.is_empty() {
        None
    } else if sound.starts_with('/') || sound.starts_with("http://") || sound.starts_with("https://") {
        Some(sound.to_string())
    } else {
        Some(sound::sound_path(sound))
    }
}

/// A queued alert has left: the queue moves on.
fn done(queue_id: Option<u64>) {
    let Some(queue_id) = queue_id else { return };
    let message = serde_json::to_string(&PageMessage::Done { queue_id }).expect("page messages serialise");
    let parent = web_sys::window().and_then(|window| window.parent().ok().flatten());
    if let (Some(parent), Ok(message)) = (parent, js_sys::JSON::parse(&message)) {
        let _ = parent.post_message(&message, "*");
    }
}

/// Text in the message.
fn text_span(document: &Document, text: &str) -> Result<HtmlElement, JsValue> {
    let span: HtmlElement = document.create_element("span")?.dyn_into()?;
    span.set_text_content(Some(text));
    Ok(span)
}

/// An emote in the text: its image, with zero-width emotes on top.
fn emote_element<'a>(
    document: &Document,
    code: &str,
    images: impl Iterator<Item = &'a String>,
) -> Result<HtmlElement, JsValue> {
    let holder: HtmlElement = document.create_element("span")?.dyn_into()?;
    holder.set_class_name("alert-emote");
    holder.set_title(code);
    for (index, image) in images.enumerate() {
        let img: HtmlImageElement = document.create_element("img")?.dyn_into()?;
        // The code stands in if the image doesn't load; overlays add nothing.
        img.set_alt(if index == 0 { code } else { "" });
        img.set_src(image);
        holder.append_child(&img)?;
    }
    Ok(holder)
}

fn document() -> Result<Document, JsValue> {
    web_sys::window().and_then(|w| w.document()).ok_or_else(|| "no document".into())
}

fn element(document: &Document, class: &str) -> Result<HtmlElement, JsValue> {
    let element: HtmlElement = document.create_element("div")?.dyn_into()?;
    element.set_class_name(class);
    Ok(element)
}

fn load_stylesheet(document: &Document, href: &str) -> Result<(), JsValue> {
    let link = document.create_element("link")?;
    link.set_attribute("rel", "stylesheet")?;
    link.set_attribute("href", href)?;
    document.head().ok_or("no head")?.append_child(&link)?;
    Ok(())
}

fn start_animation_loop(page: Rc<RefCell<Page>>) {
    type FrameCallback = Closure<dyn FnMut(f64)>;
    let callback: Rc<RefCell<Option<FrameCallback>>> = Rc::new(RefCell::new(None));
    let next = Rc::clone(&callback);
    *callback.borrow_mut() = Some(Closure::new(move |_: f64| {
        // Wall-clock ms: run times and lengths are compared with it.
        page.borrow_mut().frame(js_sys::Date::now());
        request_frame(next.borrow().as_ref().expect("callback is set"));
    }));
    request_frame(callback.borrow().as_ref().expect("callback is set"));
}

fn request_frame(callback: &Closure<dyn FnMut(f64)>) {
    if let Some(window) = web_sys::window() {
        let _ = window.request_animation_frame(callback.as_ref().unchecked_ref());
    }
}

fn warn(message: &str) {
    web_sys::console::warn_1(&message.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_from_the_plugins_settings() {
        // As the app sends them: pictures and sounds as paths, or null.
        let none = style_of(
            serde_json::json!({ "picture": null, "sound": null, "background": "#00ff00" })
                .as_object()
                .unwrap()
                .clone(),
        );
        assert_eq!((none.picture_url(), none.background.as_str()), (None, "#00ff00"));
        let style = style_of(
            serde_json::json!({
                "position": "bottom_left", "animation": "pop", "font": "", "font_size_px": 50.0,
                "picture": "/pictures/gold%2Egif", "sound": "/sounds/horn%2Emp3", "sound_volume": 0.5
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        assert_eq!(style.position, protocol::plugin::Position { x: 0.0, y: 100.0 });
        assert_eq!(style.frame_css(), "--x: 0; --y: 1");
        let below =
            style_of(serde_json::json!({ "picture_placement": "below" }).as_object().unwrap().clone());
        assert_eq!(below.classes(), "alert anim-slide-down picture-below");
        // Anywhere between, as x and y in %.
        let custom =
            style_of(serde_json::json!({ "position": { "x": 12, "y": 85 } }).as_object().unwrap().clone());
        assert_eq!(custom.frame_css(), "--x: 0.12; --y: 0.85");
        assert_eq!(style.picture_url().as_deref(), Some("/pictures/gold%2Egif"));
        assert_eq!(sound_url(&style.sound).as_deref(), Some("/sounds/horn%2Emp3"));
        assert_eq!(sound_url("horn.mp3").as_deref(), Some("/sounds/horn%2Emp3"));
        assert_eq!(sound_url(" "), None);
    }

    #[test]
    fn values_as_the_app_sends_them() {
        let values: AlertValues = serde_json::from_value(serde_json::json!({
            "message": { "text": "hi Kappa", "parts": [
                { "type": "text", "text": "hi " },
                { "type": "emote", "code": "Kappa", "url": "/emote/twitch/25", "overlays": [] }
            ] },
            "duration": 5,
            "read": { "text": "thanks", "parts": [], "speech": { "url": "/speech/a.wav", "volume": 0.8 } },
            "style": "loud"
        }))
        .unwrap();
        assert_eq!(values.message.unwrap().parts.len(), 2);
        assert_eq!(values.duration, Some(5.0));
        let speech = values.read.unwrap().speech.unwrap();
        assert_eq!((speech.url.as_str(), speech.duration_ms), ("/speech/a.wav", None));
    }
}
